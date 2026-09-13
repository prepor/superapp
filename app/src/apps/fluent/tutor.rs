//! Starting the tutor: the two runs the course puts to the agent.
//!
//! There is no tutor panel of this app's own. The tutor is a chat — the
//! agent app's, joined to whatever asked for it, carrying that panel as a
//! chip — and [`prompt::brief`] is its first turn. What starts one is a
//! lesson finishing ([`compile`]) or the desk's *build* ([`author`]).
//!
//! Both refuse in a word rather than half-working: a build without the
//! agent app has no tutor, and a course without a learner row has nobody
//! to teach. And both leave one local note behind — the chat's id on the
//! shelf's building placeholder — so the desk can put a link to the run's
//! own panel on its bar, which is where the run's hands are.

use kernel::layout::SlotId;
use kernel::session::Session;

use crate::apps::agent::{chip::Chip, Agent};

use super::model;
use super::prompt::{self, Task};
use super::sm2::{day_start, DAY};

/// After a lesson: grade the writing it left open, then author tomorrow's.
///
/// `from` is the summary's own slot, so the chat opens beside the report
/// the learner is reading and the lesson goes into it as a chip — the
/// prompts, the answers, the model answers and the notes in full.
pub fn compile(s: &mut Session, from: SlotId, lesson: i64) {
    let day = day_start(s.now()) + DAY;
    run(s, from, Task::Compile { lesson, day }, None);
}

/// From the desk: author a lesson for today out of the schedule alone.
///
/// `first` is a course with nothing played yet, which the brief answers
/// with a gentle first lesson rather than a review of nothing.
pub fn author(s: &mut Session, from: SlotId, first: bool) {
    let day = day_start(s.now());
    run(s, from, Task::Author { first, day }, Some(day));
}

/// The one road: the brief as the first turn, the asking panel as the
/// chip, and the chat's id written onto the placeholder once the send has
/// committed.
///
/// `place` is the day a placeholder is to be made for, where the caller
/// wants one — *build* does, because the shelf has nothing to say the
/// tutor is at work. A compile makes none: the finish has already left one
/// there, or deliberately left the lesson standing that was.
///
/// Both refusals come before either, so a build that cannot ask changes
/// nothing at all.
fn run(s: &mut Session, from: SlotId, task: Task, place: Option<f64>) {
    let Some(agent) = s.apps().get_as::<Agent>() else {
        s.notify("no agent app in this build, so there is no tutor to ask", true);
        return;
    };
    let Some(learner) = model::learner(s.store()) else {
        s.notify("set the course up first — the tutor is told who it is teaching", true);
        return;
    };
    let placeholder = match place {
        Some(day) => model::ensure_building(s.store(), day),
        None => model::building_lesson(s.store()),
    };
    let brief = prompt::brief(&learner, task);
    let chips = Chip::panel(s, from).into_iter().collect();
    let store = s.store().clone();
    agent.start(s, from, &brief, chips, move |s, chat| match chat {
        Some(chat) => {
            if let Some(lesson) = placeholder {
                model::set_lesson_chat(&store, lesson, chat);
            }
            s.redraw();
        }
        None => s.notify("the tutor could not be started", true),
    });
}
