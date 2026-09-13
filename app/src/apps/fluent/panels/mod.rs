//! The course's panels: what each shows, what its bar wears, and what a
//! verb does to the store. The widgets that draw them are under
//! [`widgets`](super::widgets).

use kernel::panel::PanelKind;
use kernel::session::Session;

mod card;
mod desk;
mod lesson;
mod lists;
mod progress;
mod review;
mod topic;

pub use card::Card;
pub use desk::{Desk, ShelfState};
pub use lesson::{Lesson, Phase};
pub use lists::{Cards, Grammar, History};
pub use progress::Progress;
pub use review::Review;
pub use topic::Topic;

pub static KINDS: &[&dyn PanelKind] = &[
    &desk::DeskKind,
    &lesson::LessonKind,
    &review::ReviewKind,
    &lists::CardsKind,
    &lists::GrammarKind,
    &lists::HistoryKind,
    &card::CardKind,
    &topic::TopicKind,
    &progress::ProgressKind,
];

/// The grade pad's six verbs, one id apiece so a bar test can tell them
/// apart. The label carries the key — a plain digit is the panel's own,
/// never a chord — and the word the original course uses for it.
pub const GRADE_IDS: [&str; 6] = [
    "fluent.grade0",
    "fluent.grade1",
    "fluent.grade2",
    "fluent.grade3",
    "fluent.grade4",
    "fluent.grade5",
];

#[must_use]
pub fn grade_of(verb: &str) -> Option<i64> {
    GRADE_IDS.iter().position(|id| *id == verb).map(|i| i as i64)
}

#[must_use]
pub fn grade_verbs() -> Vec<kernel::panel::Verb> {
    GRADE_IDS
        .iter()
        .zip(super::sm2::WORDS)
        .enumerate()
        .map(|(q, (id, word))| kernel::panel::Verb::run(id, format!("{q} {word}"), None))
        .collect()
}

/// Speaking is the platform's, in a later phase; this round a card says
/// what it would say.
pub fn speak(s: &mut Session, text: &str) {
    if text.trim().is_empty() {
        s.notify("nothing to speak here", true);
    } else {
        s.notify(format!("speaking: „{text}“"), false);
    }
}
