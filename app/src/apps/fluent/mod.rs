//! Fluent: a language course that lives in the store.
//!
//! The tutor is an agent; the learner plays. A **lesson** is authored whole
//! by the tutor between two sittings — the arc, the exercises, the model
//! answers — and played without waiting on a model; a **card** is a word
//! on a flashcard, reviewed on its own schedule; every grade is a
//! **review** row, which is what the schedule and the tutor both read.
//! Nothing here is a file: the six JSON notebooks of the original are
//! tables, so the agent reads them with `sql.query` and writes them through
//! this app's own tools.

use std::any::Any;

use kernel::app::{App, Mode, Root, Schema};
use kernel::panel::PanelKind;
use kernel::store::Store;
use kernel::sync::Replicated;
use kernel::tool::Tool;

pub mod model;
mod panels;
mod scenes;
mod schema;
mod seed;
pub mod sm2;
#[cfg(test)]
mod tests;
mod tools;
mod ui;
mod widgets;

pub use panels::{Cards, Desk, Grammar, Progress, Review};
pub use ui::UI;

pub struct Fluent;
pub static FLUENT: Fluent = Fluent;

/// Where `fluent_review` stood when this store's schedule was last
/// replayed from it — one per store, the way any app's transient state is.
/// Empty until the first poll, which is why a run replays once at its
/// first quiet frame: a store can have been handed grades while nothing
/// was watching.
#[derive(Default)]
struct Replayed(std::sync::Mutex<Vec<u64>>);

/// What of the course travels between devices: everything the learner and
/// the tutor decide — the course itself, every grade, and where each stands.
///
/// A row is named by what names it on every device: a slug, a uid, or the
/// instant a grade was given. What a device works out for itself does not
/// travel — the SM-2 cache on an item, which is replayed from the grades
/// here, and the integer id a lesson wears on this device, which the
/// triggers keep beside the uid.
pub static REPLICATED: &[Replicated] = &[
    Replicated {
        table: "fluent_learner",
        key: &["id"],
        columns: &[
            "name",
            "native",
            "target",
            "level",
            "goal",
            "daily_minutes",
            "streak",
            "last_active",
            "started",
        ],
    },
    Replicated {
        table: "fluent_item",
        key: &["id"],
        columns: &["kind", "content", "created"],
    },
    Replicated {
        table: "fluent_card",
        key: &["item"],
        columns: &["front", "back", "example", "audio", "notes"],
    },
    Replicated {
        table: "fluent_review",
        key: &["item", "at", "device"],
        columns: &["quality", "lesson_uid", "seq"],
    },
    Replicated {
        table: "fluent_lesson",
        key: &["uid"],
        columns: &[
            "title",
            "for_date",
            "focus",
            "status",
            "generated",
            "started",
            "ended",
            "accuracy",
            "minutes",
            "notes",
        ],
    },
    Replicated {
        table: "fluent_exercise",
        key: &["lesson_uid", "seq"],
        columns: &[
            "section",
            "kind",
            "grading",
            "prompt",
            "passage",
            "audio",
            "choices",
            "accepted",
            "model",
            "hints",
            "explanation",
            "items",
            "difficulty",
            "answer",
            "result",
            "self_grade",
            "tutor_grade",
            "tutor_note",
            "tutor_fix",
            "hints_shown",
            "elapsed",
            "answered",
        ],
    },
    Replicated {
        table: "fluent_topic",
        key: &["id"],
        columns: &[
            "title",
            "category",
            "level",
            "summary",
            "mastery",
            "items",
            "introduced",
            "practiced",
            "sections",
            "related",
            "updated",
        ],
    },
    Replicated {
        table: "fluent_topic_note",
        key: &["uid"],
        columns: &["topic", "note", "at", "lesson_uid"],
    },
    Replicated {
        table: "fluent_mistake",
        key: &["id"],
        columns: &[
            "category",
            "subcategory",
            "frequency",
            "last",
            "notes",
            "wrong",
            "right",
            "context",
        ],
    },
    Replicated {
        table: "fluent_skill",
        key: &["name"],
        columns: &["mastery", "accuracy", "lessons", "practiced"],
    },
];

const DESCRIBE: &str = "fluent_learner: one row — the learner's name, native and target language, \
CEFR level and goal, daily minutes, the streak and when it was last fed; finishing a lesson \
feeds both, so never write streak or last_active by hand. \
fluent_item: everything on a schedule, keyed by a slug (vocab_die_gebuehr, article_gender, \
eszett_usage): kind is vocab, grammar or error, content is the one line it shows. ease, \
interval, reps, due, reviewed and mastery are derived — this device replays the item's \
grades in fluent_review into them after every write and after device sync brings grades in — \
so they are read, never written: to move an item's schedule, file a grade. due is a day at \
00:00 UTC in unix seconds and mastery a 0–5 stamp. \
fluent_card: the flashcard behind a vocab item — front (the word, with its article), back \
(the meaning in the learner's language), example, audio (what to speak), notes. \
fluent_review: one row per grade ever given, keyed by item, instant and device: quality 0–5, \
lesson_uid and seq where it came from a lesson's exercise, and lesson, the local id of that \
lesson_uid, which a trigger keeps. Never edit these, add one through fluent.grade or by \
playing; the schedule is what they add up to. \
fluent_lesson: an authored sitting — title, for_date, focus (JSON tags), status building | \
ready | done, started/ended, and once done its accuracy, minutes and the tutor's notes. uid \
names the lesson on every one of this person's devices and is never written by hand; the \
integer id is this device's own and is what panels, joins and these tools take. \
fluent_exercise: one exercise of a lesson, keyed by its lesson's uid and its seq in that \
lesson (lesson is the local id, kept by a trigger): section (warmup, review, new, set_piece, \
cooldown), kind (mcq, cloze, translate, free_write, listen_mcq, read_mcq), grading (closed = \
matched against accepted; self_check = the learner grades against model), prompt, passage, \
audio, choices/accepted/hints/items as JSON arrays, model, explanation, difficulty 1–5; then \
what happened: answer, result (correct | almost | wrong), self_grade, tutor_grade, \
tutor_note, tutor_fix, hints_shown, elapsed, answered. A tutor grade overrules the learner's: \
fluent.grade rewrites the grades that answer filed and the items follow. \
fluent_topic: the grammar reference the tutor keeps — id slug, title, category, level, \
summary, mastery stamp, items it links to, the lessons it was introduced and last practiced \
in, sections as JSON (text, tip, table, examples), related topic ids. rank is derived from \
category and orders the list; leave it alone. \
fluent_topic_note: the learner's own stumbles on a topic, one line each, newest first, with \
the lesson_uid each came from. \
fluent_mistake: an error pattern — category, frequency, last seen, a wrong/right example. \
fluent_skill: mastery and accuracy per skill (writing, reading, listening, speaking, vocabulary). \
Author a lesson with fluent.author (a whole lesson in one call, tomorrow's shelf), grade a \
self_check answer with fluent.grade, read what is due with fluent.due and a lesson with \
fluent.lesson. Prefer these to sql.write: they keep the schedule and the streak honest.";

impl App for Fluent {
    fn id(&self) -> &'static str {
        "fluent"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        panels::KINDS
    }
    fn schema(&self) -> Option<&'static Schema> {
        Some(&schema::SCHEMA)
    }
    fn replicated(&self) -> &'static [Replicated] {
        REPLICATED
    }
    fn seed(&self, store: &Store, mode: Mode) -> rusqlite::Result<()> {
        seed::seed(store, mode)
    }
    /// The schedule follows the grades, wherever they were given. A grade
    /// this device filed moved its item inside that write; one another
    /// device filed arrives while the app runs, as ops on `fluent_review`,
    /// and nothing here pressed anything — so every look at the table's
    /// revision that finds it moved replays the items that have grades.
    ///
    /// One query for the items and one write for all of them, and the
    /// replay is the same answer every time, so a poll that finds nothing
    /// new costs a generation count.
    fn poll(&self, s: &mut kernel::session::Session) {
        let replayed = s.store().local::<Replayed>();
        let now = s.store().revision(&["fluent_review"]);
        let mut seen = replayed.0.lock().unwrap_or_else(|e| e.into_inner());
        if *seen == now {
            return;
        }
        *seen = now;
        drop(seen);
        let _ = s.store().write(|c| model::recompute_all_tx(c));
    }
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(Desk::id(), "fluent", "german learn language lesson tutor"),
            Root::new(Review::id(), "review cards", "flashcards due vocab"),
            Root::new(Cards::id(), "cards", "vocab deck words"),
            Root::new(Grammar::id(), "grammar", "grammatik rules topics"),
            Root::new(Progress::id(), "progress", "streak mastery accuracy"),
        ]
    }
    fn describe(&self) -> Option<&'static str> {
        Some(DESCRIBE)
    }
    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
