//! A page's history — every write, newest first — and one revision as a
//! reading, with **restore** on its bar.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::{fmt_date, fmt_date_long};

use super::super::markdown;
use super::super::model::{self, Resolver};

pub struct History {
    id: PanelId,
    slot: SlotId,
    slug: String,
    store: Rc<Store>,
}

impl History {
    pub const TAG: Tag = Tag("kb-history");

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

    /// The revisions, newest first.
    #[must_use]
    pub fn revisions(&self) -> Rc<Vec<model::Revision>> {
        match self.page() {
            Some(p) => model::revisions(&self.store, &p.uid),
            None => Rc::new(Vec::new()),
        }
    }

    /// One row's line: the date, the device, who, and the message.
    #[must_use]
    pub fn line(r: &model::Revision) -> String {
        format!("{} · {} · {}", fmt_date(r.at), r.device, r.by())
    }
}

impl Panel for History {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        format!("history of {}", self.page().map_or_else(|| self.slug.clone(), |p| p.title))
    }
    fn about(&self) -> String {
        format!(
            "Every write of the page {}, newest first: when, on which device, by whom — the \
             editor, the import, or an agent's chat by its title — and the message that came \
             with it. Each opens the document as it was left, with restore on its bar.",
            self.slug
        )
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 4)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct HistoryKind;
impl PanelKind for HistoryKind {
    fn tag(&self) -> Tag {
        History::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(History {
            id: id.clone(),
            slot: 0,
            slug: id.arg(0).unwrap_or("").to_string(),
            store: cx.session().store().clone(),
        })
    }
}

pub struct Revision {
    id: PanelId,
    slot: SlotId,
    uid: String,
    store: Rc<Store>,
}

impl Revision {
    pub const TAG: Tag = Tag("kb-revision");

    #[must_use]
    pub fn id(uid: &str) -> PanelId {
        PanelId::new(Self::TAG, [uid])
    }

    #[must_use]
    pub fn revision(&self) -> Option<(model::Revision, String)> {
        model::revision(&self.store, &self.uid)
    }

    /// The muted line: who, when, and the message.
    #[must_use]
    pub fn meta(r: &model::Revision) -> String {
        let mut parts = vec![r.by(), format!("on {}", r.device), fmt_date_long(r.at)];
        if !r.message.is_empty() {
            parts.push(r.message.clone());
        }
        parts.join(" · ")
    }

    /// The document's body as HTML, the frontmatter left off.
    #[must_use]
    pub fn html(&self, document: &str) -> String {
        let (_, body) = markdown::parse_document(document);
        let resolver = Resolver::new(&self.store);
        markdown::html(&body, &|t| resolver.resolve(t))
    }

    /// The page the revision is of, for the title.
    #[must_use]
    pub fn page(&self) -> Option<model::Page> {
        let (r, _) = self.revision()?;
        model::page_by_uid(&self.store, &r.page)
    }

    /// A link followed in the reading.
    pub fn follow(&self, s: &mut Session, href: &str) -> bool {
        let Some((kind, what)) = markdown::route(href) else {
            return false;
        };
        let id = match kind {
            "page" => super::Page::id(&what),
            _ => super::File::id(&what),
        };
        s.nav(Nav::Open { from: self.slot, id, fresh: false });
        true
    }
}

impl Panel for Revision {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        match self.revision() {
            Some((r, _)) => format!(
                "{} · {}",
                self.page().map_or_else(|| r.page.clone(), |p| p.title),
                fmt_date(r.at)
            ),
            None => "revision".into(),
        }
    }
    fn about(&self) -> String {
        "One revision of a page: the document as that write left it, who made it and when. \
         restore writes this body as the page's next revision."
            .into()
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
        vec![Verb::run("kb.restore", "restore", Some('r'))]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb != "kb.restore" {
            return;
        }
        let Some((r, document)) = self.revision() else {
            return;
        };
        let message = format!("restored from {}", fmt_date_long(r.at));
        match model::save(s, Some(r.page.clone()), document, message) {
            Some(_) => s.notify("restored", false),
            None => s.notify("the store refused the restore", true),
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct RevisionKind;
impl PanelKind for RevisionKind {
    fn tag(&self) -> Tag {
        Revision::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Revision {
            id: id.clone(),
            slot: 0,
            uid: id.arg(0).unwrap_or("").to_string(),
            store: cx.session().store().clone(),
        })
    }
}
