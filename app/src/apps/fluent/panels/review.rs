//! The flashcards: the cards due today, one at a time, front then back,
//! graded 0–5. Each grade is a review row and the card's next date, and
//! one undo takes it back.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, CardRow};
use super::{grade_of, grade_verbs, speak, Cards};

pub struct Review {
    id: PanelId,
    slot: SlotId,
    store: Rc<Store>,
    /// The queue as it stood when the panel opened: a grade moves a card's
    /// date, and the queue must not move under the learner with it.
    queue: Vec<CardRow>,
    pos: usize,
    flipped: bool,
    right: usize,
    started: f64,
    now: f64,
}

impl Review {
    pub const TAG: Tag = Tag("review");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    #[must_use]
    pub fn current(&self) -> Option<&CardRow> {
        self.queue.get(self.pos)
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[must_use]
    pub fn flipped(&self) -> bool {
        self.flipped
    }

    #[must_use]
    pub fn finished(&self) -> bool {
        !self.queue.is_empty() && self.pos >= self.queue.len()
    }

    /// `(graded, right, minutes)` for the finished screen.
    #[must_use]
    pub fn outcome(&self) -> (usize, usize, i64) {
        (self.pos, self.right, ((self.now - self.started) / 60.0).round().max(1.0) as i64)
    }

    pub fn tick(&mut self, now: f64) {
        self.now = now;
    }

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }

    /// How many new cards and how many reviews the queue holds.
    #[must_use]
    pub fn split(&self) -> (usize, usize) {
        let new = self.queue.iter().filter(|c| c.reps == 0).count();
        (new, self.queue.len() - new)
    }

    pub fn flip(&mut self, s: &mut Session) {
        if self.current().is_some() && !self.flipped {
            self.flipped = true;
            s.redraw();
        }
    }

    pub fn grade(&mut self, quality: i64, s: &mut Session) {
        let Some(card) = self.current().cloned() else { return };
        if !self.flipped {
            return;
        }
        let word = super::super::sm2::WORDS[quality.clamp(0, 5) as usize];
        if model::review_card(s, &card.item, quality, format!("grade „{}“ {word}", card.front)) {
            if quality >= 3 {
                self.right += 1;
            }
            self.pos += 1;
            self.flipped = false;
        }
        s.redraw();
    }

    pub fn play(&self, s: &mut Session) {
        let audio = self.current().map(|c| c.audio.clone()).unwrap_or_default();
        speak(s, &audio);
    }
}

impl Panel for Review {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "review".into()
    }
    fn about(&self) -> String {
        "The flashcards due today, one at a time: the front is the word, show turns it to \
         the meaning, the example and the note, and a grade 0–5 files a review and moves the \
         card's next date by SM-2. New cards come with the due ones. The queue is the one the \
         panel opened with; a grade is one undoable action."
            .into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let mut v = Vec::new();
        if let Some(_card) = self.current() {
            if self.flipped {
                v.extend(grade_verbs());
            } else {
                v.push(Verb::run("fluent.show", "show", Some('s')));
            }
            v.push(Verb::run("fluent.play", "play", Some('y')));
        } else {
            v.push(Verb::go(
                "fluent.cards",
                "cards",
                Some('c'),
                Nav::Open { from: self.slot, id: Cards::id(), fresh: false },
            ));
        }
        v
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "fluent.show" => self.flip(s),
            "fluent.play" => self.play(s),
            other => {
                if let Some(q) = grade_of(other) {
                    self.grade(q, s);
                }
            }
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct ReviewKind;
impl PanelKind for ReviewKind {
    fn tag(&self) -> Tag {
        Review::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let now = cx.session().now();
        let queue = model::due_cards(&store, now).to_vec();
        Box::new(Review {
            id: id.clone(),
            slot: 0,
            store,
            queue,
            pos: 0,
            flipped: false,
            right: 0,
            started: now,
            now,
        })
    }
}
