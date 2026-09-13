//! The desk: what today holds, and the way to everything else.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use crate::apps::agent::Chat;

use super::super::model::{self, Shelf};
use super::super::sm2;
use super::{Cards, Grammar, History, Lesson, Progress, Review, Setup};

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
    /// No learner row: the course has not been set up, so there is nothing
    /// for the tutor to build and nobody to build it for.
    Unset,
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
        if !Setup::learner_set(&self.store) {
            return ShelfState::Unset;
        }
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
         last lessons' accuracy. Start plays the shelf's lesson; build asks the tutor for one \
         and tutor goes to the chat it is being built in; review is the flashcards due today; \
         cards, grammar, progress and history are the rest of the course. A store with no \
         learner row offers set up and nothing else."
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
            // While the tutor is at work its chat is the one thing to go
            // to, because a run whose chat is shown nowhere waits for it.
            ShelfState::Building(s) => {
                if let Some(chat) = s.chat {
                    v.push(Verb::go("fluent.tutor", "tutor", Some('o'), open(Chat::id(chat))));
                }
            }
            ShelfState::Empty => v.push(Verb::run("fluent.build", "build", Some('b'))),
            // Nothing of the course is offered before it has a learner:
            // there is nobody to build a lesson for.
            ShelfState::Unset => {
                v.push(Verb::go("fluent.setup", "set up", Some('e'), open(Setup::id())));
                return v;
            }
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
    /// *build* and *build fresh* are the same question put to the tutor:
    /// author a lesson for today.
    ///
    /// A stale lesson is left exactly where it stands. Nothing marks it
    /// done — it was never played, and a row that says it was would lie to
    /// the history and to the accuracy. The placeholder for today simply
    /// takes the shelf from it, being for a later day, and the stale
    /// lesson stays in the history where it can still be opened and
    /// played, until the tutor's own lesson replaces the placeholder.
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "fluent.build" {
            let first = model::lessons_played(&self.store) == 0;
            // After the event: this runs as `&mut self` off the bar, and
            // the chip the tutor carries is read off this very panel.
            let slot = self.slot;
            s.after_event(move |s| super::super::tutor::author(s, slot, first));
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
