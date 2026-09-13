//! The desk: what today holds, and the way to everything else.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, Shelf};
use super::super::sm2;
use super::{Cards, Grammar, History, Lesson, Progress, Review};

/// A lesson built more than this many days ago is stale: the reviews it
/// wove in are not the ones due any more.
pub const STALE_DAYS: i64 = 3;

pub struct Desk {
    id: PanelId,
    slot: SlotId,
    store: Rc<Store>,
    /// The session's clock at the last draw: what the greeting, the
    /// streak and the shelf's staleness are judged against.
    now: f64,
}

/// What the shelf holds, as the desk reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum ShelfState {
    Ready(Shelf),
    Stale(Shelf, i64),
    Building(Shelf),
    Empty,
}

impl Desk {
    pub const TAG: Tag = Tag("fluent");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    pub fn tick(&mut self, now: f64) {
        self.now = now;
    }

    #[must_use]
    pub fn shelf(&self) -> ShelfState {
        match model::shelf(&self.store) {
            None => ShelfState::Empty,
            Some(s) if s.status == "building" => ShelfState::Building(s),
            Some(s) => {
                let away = sm2::days_between(s.for_date, self.now);
                if away > STALE_DAYS && !s.started {
                    ShelfState::Stale(s, away)
                } else {
                    ShelfState::Ready(s)
                }
            }
        }
    }

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }
}

impl Panel for Desk {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "fluent".into()
    }
    fn about(&self) -> String {
        "The course's desk: the learner and the streak, the lesson on the shelf for today \
         (ready, still building, or stale after days away), what is due on the deck, and the \
         last lessons' accuracy. Start plays the shelf's lesson; review is the flashcards due \
         today; cards, grammar, progress and history are the rest of the course."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 5)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let open = |id: PanelId| Nav::Open { from: self.slot, id, fresh: false };
        let mut v = Vec::new();
        match self.shelf() {
            ShelfState::Ready(s) => v.push(Verb::go(
                "fluent.start",
                if s.started { "continue" } else { "start" },
                Some('s'),
                open(Lesson::id(s.id)),
            )),
            ShelfState::Stale(s, _) => {
                v.push(Verb::run("fluent.build", "build fresh", Some('b')));
                v.push(Verb::go("fluent.play_anyway", "play anyway", Some('y'), open(Lesson::id(s.id))));
            }
            ShelfState::Building(_) => {}
            ShelfState::Empty => v.push(Verb::run("fluent.build", "build", Some('b'))),
        }
        let (_, cards) = model::due_counts(&self.store, self.now);
        let review = if cards > 0 { format!("review {cards}") } else { "review".to_string() };
        v.push(Verb::go("fluent.review", review, Some('r'), open(Review::id())));
        v.push(Verb::go("fluent.cards", "cards", Some('c'), open(Cards::id())));
        v.push(Verb::go("fluent.grammar", "grammar", Some('g'), open(Grammar::id())));
        v.push(Verb::go("fluent.progress", "progress", Some('p'), open(Progress::id())));
        v.push(Verb::go("fluent.history", "history", Some('h'), open(History::id())));
        v
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "fluent.build" {
            s.notify(
                "draft: the tutor authors the shelf's lesson — an agent run over fluent.due and fluent.author",
                false,
            );
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct DeskKind;
impl PanelKind for DeskKind {
    fn tag(&self) -> Tag {
        Desk::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Desk {
            id: id.clone(),
            slot: 0,
            store: cx.session().store().clone(),
            now: cx.session().now(),
        })
    }
}
