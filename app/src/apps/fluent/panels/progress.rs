//! Progress: the streak, eight weeks of activity, mastery per skill, the
//! accuracy of the last lessons, the errors that keep coming back, and
//! the words.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::store::Store;

use super::History;

pub struct Progress {
    id: PanelId,
    slot: SlotId,
    store: Rc<Store>,
    now: f64,
}

impl Progress {
    pub const TAG: Tag = Tag("progress");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }

    pub fn tick(&mut self, now: f64) {
        self.now = now;
    }
}

impl Panel for Progress {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "progress".into()
    }
    fn about(&self) -> String {
        "The learner's progress: the streak and whether today keeps it, minutes studied on \
         each of the last 56 days, mastery 0–5 and accuracy per skill, the accuracy of the \
         last ten lessons, the error patterns seen most often with their last sighting, and \
         the words learned. Everything here is read off the rows; nothing is edited."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::go(
            "fluent.history",
            "history",
            Some('h'),
            Nav::Open { from: self.slot, id: History::id(), fresh: false },
        )]
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct ProgressKind;
impl PanelKind for ProgressKind {
    fn tag(&self) -> Tag {
        Progress::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Progress {
            id: id.clone(),
            slot: 0,
            store: cx.session().store().clone(),
            now: cx.session().now(),
        })
    }
}
