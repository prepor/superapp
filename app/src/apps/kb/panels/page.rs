//! One page as a reading: the title, a muted line, the body, and under
//! rules what it names, what names it, and the files it reaches.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::fmt_date_long;

use super::super::markdown::{self, LinkKind};
use super::super::model::{self, Link, Resolver, Target};
use super::{ask, start_chat};

pub struct Page {
    id: PanelId,
    slot: SlotId,
    slug: String,
    store: Rc<Store>,
    /// The **rename** field's text while it stands where the title is.
    renaming: Option<String>,
    /// What a verb refused, until the next one.
    status: Option<String>,
}

impl Page {
    pub const TAG: Tag = Tag("page");

    #[must_use]
    pub fn id(slug: &str) -> PanelId {
        PanelId::new(Self::TAG, [slug])
    }

    #[must_use]
    pub fn slot(&self) -> SlotId {
        self.slot
    }

    #[must_use]
    pub fn page(&self) -> Option<model::Page> {
        model::page(&self.store, &self.slug)
    }

    /// The muted line under the title: *entity · city, harbour · written
    /// 28 Aug 2026 · 4 revisions*.
    #[must_use]
    pub fn meta(&self, p: &model::Page) -> String {
        let mut parts = vec![p.kind.clone()];
        if !p.tags.is_empty() {
            parts.push(p.tags.join(", "));
        }
        parts.push(format!("written {}", fmt_date_long(p.updated)));
        let n = model::revisions(&self.store, &p.uid).len();
        parts.push(format!("{n} revision{}", if n == 1 { "" } else { "s" }));
        parts.join(" · ")
    }

    /// The body as the `Html` widget draws it.
    #[must_use]
    pub fn html(&self, p: &model::Page) -> String {
        let resolver = Resolver::new(&self.store);
        markdown::html(&p.body, &|t, k| resolver.resolve(t, k))
    }

    #[must_use]
    pub fn links(&self, p: &model::Page) -> Vec<Link> {
        model::links(&self.store, &p.uid)
    }

    #[must_use]
    pub fn backlinks(&self, p: &model::Page) -> Rc<Vec<(String, String)>> {
        model::backlinks(&self.store, p)
    }

    /// The pictures the body names, by the key the reader files them
    /// under, with the bytes the cache would hand over.
    #[must_use]
    pub fn pictures(&self, p: &model::Page) -> Vec<(String, Vec<u8>)> {
        self.links(p)
            .into_iter()
            .filter(|l| l.kind == LinkKind::Image)
            .filter_map(|l| match l.resolved {
                Target::File { hash, .. } => super::super::seed::bytes_of(&hash).map(|b| (format!("cid:kb/{hash}"), b)),
                _ => None,
            })
            .collect()
    }

    #[must_use]
    pub fn renaming(&self) -> Option<&str> {
        self.renaming.as_deref()
    }

    pub fn set_renaming(&mut self, text: Option<String>) {
        self.renaming = text;
    }

    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Enter in the field: the rename, and the panel pointed at the new
    /// slug in the same breath.
    pub fn rename(&mut self, s: &mut Session, title: &str) {
        let Some(p) = self.page() else {
            return;
        };
        match model::rename(s, &p.uid, title) {
            Ok(slug) => {
                self.renaming = None;
                self.status = None;
                if slug != self.slug {
                    s.nav_within(Nav::Replace { slot: self.slot, id: Self::id(&slug) });
                }
            }
            Err(e) => self.status = Some(e),
        }
        s.redraw();
    }

    /// A link in the reading, followed: a page or a file opens joined.
    pub fn follow(&self, s: &mut Session, href: &str) -> bool {
        let Some((kind, what)) = markdown::route(href) else {
            return false;
        };
        let id = match kind {
            "page" => Self::id(&what),
            _ => super::File::id(&what),
        };
        s.nav(Nav::Open { from: self.slot, id, fresh: false });
        true
    }
}

impl Panel for Page {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.page().map_or_else(|| self.slug.clone(), |p| p.title)
    }
    fn about(&self) -> String {
        format!(
            "One page of the knowledge base, {}: its kind, tags and summary, the body as written \
             — a Markdown document whose [[wikilinks]] name other pages by slug — and under it \
             what the page links to, what links to it, and the files it names. The slug is the \
             word a wikilink targets and is unique across the KB.",
            self.slug
        )
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["body"]
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let go = |id, label, accel, target| {
            Verb::go(id, label, accel, Nav::Open { from: self.slot, id: target, fresh: false })
        };
        let mut verbs = Vec::new();
        let Some(p) = self.page() else {
            return verbs;
        };
        if p.kind == "skill" {
            verbs.push(Verb::run("kb.use", "use", Some('s')));
        }
        verbs.extend([
            Verb::run("kb.ask", "ask", Some('a')),
            go("kb.edit", "edit", Some('e'), super::Edit::id(&p.slug)),
            go("kb.history", "history", Some('h'), super::History::id(&p.slug)),
            Verb::run("kb.rename", "rename", Some('r')),
            Verb::run("kb.delete", "delete", Some('d')),
        ]);
        verbs
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "kb.ask" => ask(s, self.slot),
            "kb.use" => start_chat(s, self.slot, "follow this skill"),
            "kb.rename" => {
                if self.renaming.is_none() {
                    self.renaming = self.page().map(|p| p.title);
                    s.redraw();
                }
            }
            "kb.delete" => {
                let Some(p) = self.page() else {
                    return;
                };
                if model::delete(s, vec![p.slug]) {
                    s.nav_within(Nav::Close { slot: self.slot, label: Some(p.title) });
                }
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct PageKind;
impl PanelKind for PageKind {
    fn tag(&self) -> Tag {
        Page::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Page {
            id: id.clone(),
            slot: 0,
            slug: id.arg(0).unwrap_or("").to_string(),
            store: cx.session().store().clone(),
            renaming: None,
            status: None,
        })
    }
}
