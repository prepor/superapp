//! The catalogue: every page, grouped by kind, as a rich table.

use std::any::Any;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;

use super::super::model::{self, PageRow};
use super::{ask, start_chat};

/// The one argument that is not a filter: the **ask kb** root.
pub const ASK: &str = "ask";

pub struct Catalogue {
    id: PanelId,
    slot: SlotId,
    pub list: ListState<&'static SqlSource<PageRow, String>>,
    pub filter: String,
    /// Opened as the **ask kb** root: the first draw offers the panel as
    /// context, so a chat with the catalogue as its chip stands beside it.
    ask_pending: bool,
}

impl Catalogue {
    pub const TAG: Tag = Tag("kb");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    #[must_use]
    pub fn filtered(filter: &str) -> PanelId {
        PanelId::new(Self::TAG, [filter])
    }

    /// The **ask kb** root: the catalogue, and a chat about it.
    #[must_use]
    pub fn ask() -> PanelId {
        PanelId::new(Self::TAG, [ASK])
    }

    #[must_use]
    pub fn slot(&self) -> SlotId {
        self.slot
    }

    /// Whether this open still owes the chat the root promised. Answers
    /// once.
    pub fn take_ask(&mut self) -> bool {
        std::mem::take(&mut self.ask_pending)
    }
}

impl Panel for Catalogue {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "kb".into()
    }
    fn about(&self) -> String {
        "The knowledge base's catalogue: every page with its kind, its title and its one-line \
         summary, grouped by kind — projects, concepts, entities, sources, then skills, memory \
         and the inbox. Free text searches titles, summaries and bodies; @kind:, @tag:, @orphan, \
         @dangling, @stale and @date narrow it. The cursor previews a page beside the list. ask \
         opens a chat with this catalogue as its chip; lint opens one that runs kb.lint."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        let f = self.list.table().filter();
        if f.is_empty() {
            Self::id()
        } else {
            Self::filtered(f)
        }
    }
    fn root(&self) -> PanelId {
        Self::id()
    }
    fn verbs(&self) -> Vec<Verb> {
        let go = |id, label, accel, target| {
            Verb::go(id, label, accel, Nav::Open { from: self.slot, id: target, fresh: false })
        };
        let mut verbs = vec![
            Verb::run("kb.ask", "ask", Some('a')),
            go("kb.new", "new page", Some('n'), super::Edit::new_id()),
            // `l` is the shell's; the letter is the one in *link check*.
            Verb::run("kb.lint", "lint", Some('k')),
            go("kb.import", "import", Some('m'), super::Import::id()),
        ];
        let marked = self.list.marks().len();
        if marked > 0 {
            verbs.push(Verb::run("kb.delete", format!("delete {marked}"), Some('d')));
        } else if self.list.cursor_key().is_some() {
            verbs.push(Verb::run("kb.delete", "delete", Some('d')));
        }
        verbs
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "kb.ask" => ask(s, self.slot),
            "kb.lint" => start_chat(s, self.slot, "run kb.lint and propose fixes"),
            "kb.delete" => {
                let marked = self.list.marks().keys();
                let slugs = if marked.is_empty() {
                    self.list.cursor_key().cloned().into_iter().collect()
                } else {
                    marked
                };
                if model::delete(s, slugs.clone()) {
                    for slug in &slugs {
                        for slot in s.showing(&super::Page::id(slug)) {
                            s.nav_within(Nav::Close { slot, label: None });
                        }
                    }
                    self.list.clear_marks();
                }
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct CatalogueKind;
impl PanelKind for CatalogueKind {
    fn tag(&self) -> Tag {
        Catalogue::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        let arg = id.arg(0).unwrap_or("");
        let asking = arg == ASK;
        let filter = if asking { String::new() } else { arg.to_string() };
        let mut list = ListState::new(&model::PAGES, 50);
        list.set_filter(&filter);
        Box::new(Catalogue {
            id: id.clone(),
            slot: 0,
            list,
            filter,
            ask_pending: asking,
        })
    }
}
