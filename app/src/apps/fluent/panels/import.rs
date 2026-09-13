//! The migration form: where the original course's folder is, and the one
//! press that reads it in.
//!
//! The reading is a worker's — a year of grades is a megabyte of JSON and
//! parsing it on the UI thread would drop frames — and the writing is this
//! thread's, because it is one undoable action on the store the panels are
//! drawn from.

use std::any::Any;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;

use super::super::import::{self, Course, Imported};

/// Where the original keeps its folder: iCloud Drive, under the name the
/// course goes by. Spelled the way the panels spell a path, with `~` for
/// home; the disk is asked for the real one.
pub const ICLOUD: &str = "~/Library/Mobile Documents/com~apple~CloudDocs/fluent";

enum State {
    Idle,
    Reading(tokio::sync::oneshot::Receiver<Result<Course, String>>),
}

pub struct Import {
    id: PanelId,
    slot: SlotId,
    pub path: String,
    pub error: String,
    pub status: String,
    state: State,
}

impl Import {
    pub const TAG: Tag = Tag("fluent-import");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// Reads the folder, then writes it. A world with workers reads on one
    /// of them; a world without — a test, a library mount — reads here, so
    /// the same call answers either way.
    pub fn submit(&mut self, s: &mut Session) {
        if !matches!(self.state, State::Idle) {
            return;
        }
        self.error.clear();
        self.status.clear();
        if let Some(factory) = s.world().factory() {
            let path = self.path.clone();
            let (done, pending) = tokio::sync::oneshot::channel();
            self.state = State::Reading(pending);
            self.status = "reading…".into();
            kernel::runtime::spawn_blocking(move || {
                let result = factory
                    .build()
                    .map_err(|e| e.to_string())
                    .and_then(|world| world.run(&import::Read(path)));
                let _ = done.send(result);
                makepad_widgets::SignalToUI::set_ui_signal();
            });
        } else {
            let result = s
                .world()
                .run(&import::Read(self.path.clone()))
                .and_then(|course| import::import(s, course));
            self.show(result);
        }
        s.redraw();
    }

    fn show(&mut self, result: Result<Imported, String>) {
        self.state = State::Idle;
        self.status.clear();
        self.error.clear();
        match result {
            Ok(done) => self.status = done.summary(),
            Err(why) => self.error = why,
        }
    }

    /// What the worker left, once it has left it. The write is here rather
    /// than on the worker: an import is an undoable action, and the history
    /// is this thread's.
    pub fn poll(&mut self, s: &mut Session) {
        let State::Reading(pending) = &mut self.state else {
            return;
        };
        let result = match pending.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(_) => Err("the course reader stopped".into()),
        };
        self.state = State::Idle;
        let result = result.and_then(|course| import::import(s, course));
        self.show(result);
        s.redraw();
    }
}

impl Panel for Import {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "import course".into()
    }
    fn about(&self) -> String {
        "The course as it was kept before this app: the original's folder of JSON notebooks — \
         the learner's profile, the schedule, the deck, the grammar reference, the error \
         patterns, the log of sittings and the session authored for the next day. Paste the \
         folder's path, then import: whatever of the files is there is read, every row is \
         keyed by its own name, and what the course already has is left as it stands, so \
         importing the same folder twice adds nothing. The schedule is replayed from the \
         grades the files carry. One undo removes the import whole."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 3)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        // `i` is the workspace's own; the letter is the one in *migrate*.
        vec![Verb::run("fluent.import", "import", Some('m'))]
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "fluent.import" {
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
            path: ICLOUD.to_string(),
            error: String::new(),
            status: String::new(),
            state: State::Idle,
        })
    }
}
