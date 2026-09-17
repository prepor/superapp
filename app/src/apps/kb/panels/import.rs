//! The import form: the folder, and the one press that reads it in.
//!
//! In phase 0 the press reads nothing: the status line walks the three
//! states the real import will show — idle, *reading…*, then the counts —
//! on the world's clock, so the library can draw each. Phase 4 puts the
//! tree reader behind the same verb.

use std::any::Any;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb, Want};
use kernel::session::Session;

/// Where the folder is on the Mac that has it.
pub const FOLDER: &str = "~/cloud/KB";

/// The files browser's tag and pick mode, named rather than imported: a
/// build without the files app gets the shell's missing card.
const FILES: Tag = Tag("files");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Reading,
    Done,
}

pub struct Import {
    id: PanelId,
    slot: SlotId,
    pub path: String,
    stage: Stage,
    since: f64,
}

impl Import {
    pub const TAG: Tag = Tag("kb-import");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// The line under the field.
    #[must_use]
    pub fn status(&self) -> String {
        match self.stage {
            Stage::Idle => String::new(),
            Stage::Reading => "reading…".into(),
            Stage::Done => "101 pages, 96 files · 12 of 96 uploaded".into(),
        }
    }

    /// **import**: the read begins. Nothing is read in phase 0; the line
    /// says what the reader would.
    pub fn submit(&mut self, s: &mut Session) {
        if self.stage == Stage::Reading {
            return;
        }
        self.stage = Stage::Reading;
        self.since = s.now();
        s.redraw();
    }

    /// The read, done, two seconds of the world's clock after it began.
    pub fn poll(&mut self, s: &mut Session) {
        if self.stage == Stage::Reading && s.now() - self.since >= 2.0 {
            self.stage = Stage::Done;
            s.redraw();
        }
    }
}

impl Panel for Import {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "kb import".into()
    }
    fn about(&self) -> String {
        "The folder the knowledge base was kept in before this app — Markdown pages over raw \
         files, cross-linked by [[wikilinks]] — read in once: every page a row, every file a \
         hash in the bucket, one revision apiece. Importing the same folder twice adds nothing. \
         In this prototype the press reads nothing yet."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 3)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn wants(&self) -> Option<Want> {
        Some(Want::dirs("import this folder", Some('m'), "Choose the folder the knowledge base is kept in."))
    }
    fn took(&mut self, paths: Vec<String>, s: &mut Session) {
        if let Some(path) = paths.into_iter().next() {
            self.path = path;
            self.stage = Stage::Idle;
            s.redraw();
        }
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::go(
                "kb.browse",
                "browse",
                Some('b'),
                Nav::Open { from: self.slot, id: PanelId::new(FILES, ["~", "pick"]), fresh: false },
            ),
            Verb::run("kb.import", "import", Some('m')),
        ]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "kb.import" {
            self.submit(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct ImportKind;
impl PanelKind for ImportKind {
    fn tag(&self) -> Tag {
        Import::TAG
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Import {
            id: id.clone(),
            slot: 0,
            path: FOLDER.to_string(),
            stage: Stage::Idle,
            since: 0.0,
        })
    }
}
