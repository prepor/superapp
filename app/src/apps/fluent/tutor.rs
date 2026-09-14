//! Asking the tutor: the two runs the course puts to the agent, and the
//! two questions it puts to the model with no chat at all.
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
//!
//! The other two are one answer graded as it is written ([`grade`]) and
//! one selected word looked up ([`look_up`]). Neither wants a chat: there
//! is nothing to say back to, nothing to call, and nothing a person would
//! open a panel to read. Each is a system line and a question through
//! [`Agent::ask_once`], asked on a worker so nobody waits, and each is
//! answered here on the UI thread — a grade as an undoable write and a
//! quiet line, a word as an entry the panel that asked is holding open.

use std::future::Future;
use std::pin::Pin;

use kernel::layout::SlotId;
use kernel::session::Session;

use crate::apps::agent::{self, chip::Chip, Agent};

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

// ---------------------------------------------------------------------------
// The two questions with no chat in them
// ---------------------------------------------------------------------------

/// One question to the model on a worker, answered on the UI thread.
///
/// [`Agent::ask_once`] and nothing else: no chat row, no run, no panel, no
/// tools. The work goes to a worker's own world through
/// [`Session::prepare_work`](kernel::session::Session::prepare_work), so
/// nobody waits for it, and the completion lands back here with the
/// session in hand.
///
/// Answers whether it went at all. A build with no agent app has no tutor
/// to ask: the caller that a person is watching says so, and the one nobody
/// asked for stays quiet.
fn ask(
    s: &mut Session,
    system: String,
    user: String,
    done: impl FnOnce(&mut Session, Result<String, String>) + 'static,
) -> bool {
    if s.apps().get("agent").is_none() {
        return false;
    }
    s.prepare_work(move |world| asking(world, system, user), done);
    true
}

/// The question as a future over one world — spelled out, because what
/// `prepare_work` takes is a function of a world's own lifetime.
fn asking<'a>(
    world: &'a kernel::effect::World,
    system: String,
    user: String,
) -> Pin<Box<dyn Future<Output = Result<String, String>> + 'a>> {
    Box::pin(async move { Agent::ask_once(world, agent::MODEL, &system, &user).await })
}

/// The tutor's word on one self-check answer, asked for the moment the
/// answer is written.
///
/// The learner never waits on it: the model answer is already on the
/// screen and the grade pad already works. What comes back either lands on
/// an exercise nobody has graded since — the reviews that answer filed are
/// rewritten, one undo apiece — or is dropped, because the tutor's compile
/// at the lesson's end grades whatever this did not reach.
pub fn grade(s: &mut Session, ex: &model::Exercise, answer: &str) {
    if ex.tutor_grade.is_some() {
        return;
    }
    let Some(learner) = model::learner(s.store()) else {
        return;
    };
    let (system, user) = (
        prompt::grader(&learner),
        prompt::grade_request(ex, answer, &learner),
    );
    let (exercise, seq, asked) = (ex.id, ex.seq, answer.to_string());
    ask(s, system, user, move |s, reply| landed(s, exercise, seq, &asked, reply));
}

/// What the tutor said about one answer, back on the UI thread.
///
/// Quiet whatever happens. A grade that landed says so in a line; a reply
/// that never came, or came as something other than a verdict, says the
/// lesson's end will check it — never an error, because nothing the
/// learner did went wrong.
fn landed(s: &mut Session, exercise: i64, seq: i64, asked: &str, reply: Result<String, String>) {
    let verdict = reply
        .ok()
        .and_then(|text| agent::json_object(&text))
        .and_then(|v| read_verdict(&v));
    let Some((quality, feedback, fix)) = verdict else {
        s.notify(format!("the tutor could not check Q{seq} — the lesson's end will"), false);
        return;
    };
    // The row as it stands now, not as it stood when the question went
    // out: the lesson may have been undone away, the answer taken back or
    // given again as something else, and the tutor's own compile may have
    // graded it in the meantime. A verdict on words that are no longer the
    // answer is a verdict on nothing.
    let Some(ex) = model::exercise(s.store(), exercise) else {
        return;
    };
    if ex.tutor_grade.is_some() || ex.answer.as_deref() != Some(asked) {
        return;
    }
    if !model::tutor_grade(s, exercise, quality, &feedback, &fix) {
        return;
    }
    s.notify(
        match ex.self_grade {
            Some(q) if q == quality => format!("tutor checked Q{seq} — agrees ({quality}/5)"),
            Some(q) => format!("tutor suggests {quality}/5 for Q{seq} — you said {q}"),
            None => format!("tutor checked Q{seq} — {quality}/5"),
        },
        false,
    );
}

/// A verdict out of the object the grader answered with: the quality, the
/// feedback, the corrected text. Anything without a quality in it is not a
/// verdict.
fn read_verdict(v: &serde_json::Value) -> Option<(i64, String, String)> {
    let quality = v.get("quality")?.as_i64()?.clamp(0, 5);
    let text = |key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Some((quality, text("feedback"), text("corrected_text")))
}

/// One selected word to the tutor, as a dictionary. The answer lands
/// wherever `done` puts it — the lookup panel, which is showing *asking
/// the tutor…* while it is out.
///
/// # Errors
///
/// If this build has no agent app, or the course has no learner yet to say
/// which two languages the question is between.
pub fn look_up(
    s: &mut Session,
    term: &str,
    done: impl FnOnce(&mut Session, Result<model::Entry, String>) + 'static,
) -> Result<(), String> {
    let Some(learner) = model::learner(s.store()) else {
        return Err("set the course up first — a dictionary is between two languages".into());
    };
    let (system, user) = (prompt::dictionary(&learner), prompt::lookup_request(term));
    let asked = term.trim().to_string();
    let went = ask(s, system, user, move |s, reply| {
        done(s, reply.and_then(|text| read_entry(&text, &asked)));
    });
    if went {
        Ok(())
    } else {
        Err("no agent app in this build, so there is no tutor to ask".into())
    }
}

/// The dictionary's object, read as an entry. A model that answered with
/// no term at all is answering about the word it was given.
fn read_entry(text: &str, asked: &str) -> Result<model::Entry, String> {
    let Some(v) = agent::json_object(text) else {
        return Err("the tutor did not answer with a word".into());
    };
    let field = |key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let term = field("term");
    Ok(model::Entry {
        term: if term.is_empty() { asked.to_string() } else { term },
        translation: field("translation"),
        pos: field("pos"),
        note: field("note"),
    })
}
