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

/// Reads a card's or an exercise's words out loud, in the language the
/// learner is learning, through the machine's own voice
/// ([`speak`](super::speak)).
///
/// The quiet line stays: a world with no voice — a scripted run, a library
/// mount, a build on a platform with no synthesizer — is not a failure, and
/// *speaking: „…“* is the only proof a machine that cannot hear has that
/// the card played. Where there is no voice the words are offered in
/// writing instead, which is what the learner wanted them for.
pub fn speak(s: &mut Session, text: &str) {
    if text.trim().is_empty() {
        s.notify("nothing to speak here", true);
        return;
    }
    let said = s.world().run(&super::speak::Speak {
        text: text.to_string(),
        lang: super::speak::lang(s.store()).to_string(),
    });
    match said {
        Ok(()) => s.notify(format!("speaking: „{text}“"), false),
        Err(_) => s.notify(format!("no speech in this world — it would say: „{text}“"), false),
    }
}
