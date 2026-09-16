//! The editor: a page's document — the frontmatter block and the body —
//! in the shell's `SourceInput`. Every change goes to the page's draft
//! row; **save** files the page and its revision in one write.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::markdown::{self, Front};
use super::super::model;

/// The argument of an editor on a page nobody has written yet.
pub const NEW: &str = "new";

pub struct Edit {
    id: PanelId,
    slot: SlotId,
    store: Rc<Store>,
    /// The page's uid; `None` until the first save of a new one.
    uid: Option<String>,
    slug: String,
    pub text: String,
    /// The document as the page has it: what *saved* means.
    saved: String,
    /// Bumped when the text comes from outside the widget.
    pub revision: u64,
    pub error: String,
    draft_failed: bool,
}

impl Edit {
    pub const TAG: Tag = Tag("kb-edit");

    #[must_use]
    pub fn id(slug: &str) -> PanelId {
        PanelId::new(Self::TAG, [slug])
    }

    #[must_use]
    pub fn new_id() -> PanelId {
        PanelId::new(Self::TAG, [NEW])
    }

    #[must_use]
    pub fn is_new(&self) -> bool {
        self.uid.is_none()
    }

    #[must_use]
    pub fn dirty(&self) -> bool {
        self.text != self.saved
    }

    /// The line under the field.
    #[must_use]
    pub fn status(&self) -> String {
        if self.draft_failed {
            "not saved — retry by editing".into()
        } else if self.is_new() && self.text.trim() == Self::blank().trim() {
            "a new page: the first save takes its slug from the title".into()
        } else if self.dirty() {
            "draft saved · save to write the page".into()
        } else {
            "saved".into()
        }
    }

    /// What a new page starts as: the frontmatter block and nothing else.
    #[must_use]
    pub fn blank() -> String {
        "---\ntype: concept\ntitle: \nsummary: \ntags: []\n---\n\n".into()
    }

    /// The widget's text, changed: the draft row, coalesced by the store.
    pub fn edited(&mut self, text: String, now: f64) {
        if text == self.text {
            return;
        }
        self.text = text;
        let key = self.uid.clone().unwrap_or_else(|| NEW.to_string());
        let body = self.dirty().then(|| self.text.clone());
        self.draft_failed = model::save_draft(&self.store, &key, body, now).is_err();
    }

    /// **save**: the document as the page's next revision. A new page's
    /// first save takes its slug from the title and points this slot at it.
    pub fn save(&mut self, s: &mut Session) {
        if !self.dirty() {
            return;
        }
        let (front, _) = markdown::parse_document(&self.text);
        if front.title.trim().is_empty() {
            self.error = "a page needs a title".into();
            s.redraw();
            return;
        }
        let uid = self.uid.clone();
        let message = if uid.is_some() { "edited" } else { "new page" };
        match model::save(s, uid.clone(), self.text.clone(), message.into()) {
            Some(page) => {
                self.error.clear();
                self.saved.clone_from(&self.text);
                if uid.is_none() {
                    let _ = model::save_draft(&self.store, NEW, None, s.now());
                    self.uid = Some(page.clone());
                    if let Some(p) = model::page_by_uid(s.store(), &page) {
                        self.slug.clone_from(&p.slug);
                        s.nav_within(Nav::Replace { slot: self.slot, id: Self::id(&p.slug) });
                    }
                }
                s.notify("page saved", false);
            }
            None => self.error = "the store refused the write".into(),
        }
        s.redraw();
    }
}

impl Panel for Edit {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        let (front, _) = markdown::parse_document(&self.text);
        let name = if front.title.trim().is_empty() {
            if self.is_new() { "new page".to_string() } else { self.slug.clone() }
        } else {
            front.title
        };
        format!("{name}{}", if self.dirty() { " *" } else { "" })
    }
    fn about(&self) -> String {
        "A page of the knowledge base as its source: the frontmatter block — type, title, \
         summary, aliases, tags — over the Markdown body, exactly what kb.write takes. Every \
         change is kept as a draft; save writes the page and files a revision. Text undo is the \
         editor's own."
            .into()
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["body"]
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (6, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("kb.save", "save", Some('s'))]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "kb.save" {
            self.save(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct EditKind;
impl PanelKind for EditKind {
    fn tag(&self) -> Tag {
        Edit::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let slug = id.arg(0).unwrap_or(NEW).to_string();
        let page = (slug != NEW).then(|| model::page(&store, &slug)).flatten();
        let (uid, saved) = match &page {
            Some(p) => {
                let front = Front {
                    kind: p.kind.clone(),
                    title: p.title.clone(),
                    summary: p.summary.clone(),
                    slug: String::new(),
                    aliases: p.aliases.clone(),
                    tags: p.tags.clone(),
                    extra: p.extra.clone(),
                };
                (Some(p.uid.clone()), markdown::document(&front, &p.body))
            }
            None => (None, Edit::blank()),
        };
        let key = uid.clone().unwrap_or_else(|| NEW.to_string());
        let text = model::draft(&store, &key).unwrap_or_else(|| saved.clone());
        Box::new(Edit {
            id: id.clone(),
            slot: 0,
            store,
            uid,
            slug,
            text,
            saved,
            revision: 0,
            error: String::new(),
            draft_failed: false,
        })
    }
}
