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

/// What of the course travels between devices: every grade, wherever it
/// was given. The schedule a device shows is derived from them.
pub static REPLICATED: &[Replicated] = &[Replicated {
    table: "fluent_review",
    key: &["item", "at", "device"],
    columns: &["quality", "lesson", "exercise"],
}];

const DESCRIBE: &str = "fluent_learner: one row — the learner's name, native and target language, \
CEFR level and goal, daily minutes, the streak and when it was last fed. \
fluent_item: everything on a schedule, keyed by a slug (vocab_die_gebuehr, article_gender, \
eszett_usage): kind is vocab, grammar or error; ease, interval, reps and due are the SM-2 \
state, due a day at 00:00 UTC in unix seconds; mastery is a 0–5 stamp. \
fluent_card: the flashcard behind a vocab item — front (the word, with its article), back \
(the meaning in the learner's language), example, audio (what to speak), notes. \
fluent_review: one row per grade ever given, quality 0–5, when, on which device, and the \
lesson and exercise it came from where it did; never edit these, add one through \
fluent.grade or by playing. \
fluent_lesson: an authored sitting — title, for_date, focus (JSON tags), status building | \
ready | done, started/ended, and once done its accuracy, minutes and the tutor's notes. \
fluent_exercise: one exercise of a lesson in seq order: section (warmup, review, new, \
set_piece, cooldown), kind (mcq, cloze, translate, free_write, listen_mcq, read_mcq), \
grading (closed = matched against accepted; self_check = the learner grades against model), \
prompt, passage, audio, choices/accepted/hints/items as JSON arrays, model, explanation, \
difficulty 1–5; then what happened: answer, result (correct | almost | wrong), self_grade, \
tutor_grade, tutor_note, tutor_fix, hints_shown, elapsed, answered. \
fluent_topic: the grammar reference the tutor keeps — id slug, title, category, level, \
summary, mastery stamp, items it links to, the lessons it was introduced and last practiced \
in, sections as JSON (text, tip, table, examples), related topic ids. \
fluent_topic_note: the learner's own stumbles on a topic, one line each, newest first. \
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
