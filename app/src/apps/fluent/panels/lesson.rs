//! The player: one exercise at a time, graded on the spot, and the summary
//! when the last one is answered. Every answer is a row the moment it is
//! given, so closing the panel loses nothing and reopening it resumes.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, Closed, Exercise, LessonRow, Patch};
use super::super::tutor;
use super::{grade_of, grade_verbs, speak, speak_unseen, History, Lookup};

/// The most seconds one exercise is credited with.
const LONGEST_SITTING: f64 = 30.0 * 60.0;

/// Where the player stands on the current exercise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Answering,
    Feedback(Closed),
    SelfGrade,
    Done,
}

pub struct Lesson {
    id: PanelId,
    slot: SlotId,
    lesson: i64,
    store: Rc<Store>,
    index: usize,
    phase: Phase,
    /// What is in the field, mirrored from the widget.
    typed: String,
    /// The highlighted choice.
    choice: Option<usize>,
    /// What is selected in one of the stage's own runs, mirrored from the
    /// widget on every event. A word here is what **lookup** looks up.
    selection: Option<String>,
    hints_shown: i64,
    /// When the current exercise was put up, on the session's clock.
    started_at: f64,
    now: f64,
}

impl Lesson {
    pub const TAG: Tag = Tag("lesson");

    #[must_use]
    pub fn id(lesson: i64) -> PanelId {
        PanelId::new(Self::TAG, [lesson.to_string()])
    }

    #[must_use]
    pub fn row(&self) -> Option<LessonRow> {
        model::lesson(&self.store, self.lesson)
    }

    #[must_use]
    pub fn exercises(&self) -> Rc<Vec<Exercise>> {
        model::exercises(&self.store, self.lesson)
    }

    #[must_use]
    pub fn current(&self) -> Option<Exercise> {
        if self.phase == Phase::Done {
            return None;
        }
        self.exercises().get(self.index).cloned()
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    #[must_use]
    pub fn choice(&self) -> Option<usize> {
        self.choice
    }

    #[must_use]
    pub fn hints_shown(&self) -> i64 {
        self.hints_shown
    }

    pub fn tick(&mut self, now: f64) {
        self.now = now;
        self.reconcile();
    }

    /// Puts the player where the rows say it stands. Every answer is a row
    /// and every row can be taken back — from the bar, from a chat, from
    /// another device — while this instance stands: an answer undone under
    /// its feedback is an exercise to answer again, a self-grade undone is
    /// its grade pad again, a lesson reopened after its last answer is its
    /// summary with **finish** on the bar. Read on every draw; a phase that
    /// agrees with the rows is left exactly as it is, choice and all.
    fn reconcile(&mut self) {
        let exs = self.exercises();
        let len = exs.len();
        let closed = self.row().is_some_and(|l| l.status == "done");
        let first = exs.iter().position(|e| !e.done()).unwrap_or(len);
        let (index, phase) = if closed {
            (len, Phase::Done)
        } else if self.index < first {
            // The exercise here is done. Under its own feedback that is
            // where we are; under anything else — an answer redone beneath
            // the field it was taken out of — it is its feedback.
            match self.phase {
                Phase::Feedback(_) => (self.index, self.phase),
                _ => (self.index, Phase::Feedback(closed_of(&exs[self.index]))),
            }
        } else {
            match exs.get(first) {
                Some(ex) if ex.answer.is_some() => (first, Phase::SelfGrade),
                Some(_) => (first, Phase::Answering),
                None => (len, Phase::Done),
            }
        };
        if (index, phase) == (self.index, self.phase) {
            return;
        }
        self.index = index;
        self.phase = phase;
        self.choice = None;
        self.selection = None;
        self.hints_shown = exs.get(index).map_or(0, |e| e.hints_shown);
        self.started_at = self.now;
    }

    pub fn set_typed(&mut self, text: String) {
        self.typed = text;
    }

    /// What is selected in the prompt, the passage, the transcript, the
    /// model answer or any other of the stage's runs — or `None` where
    /// nothing is, which is also what closing a selection says. Answers
    /// whether it changed, so the widget knows when to ask for the frame
    /// that redraws the bar.
    pub fn set_selection(&mut self, text: Option<String>) -> bool {
        if self.selection == text {
            return false;
        }
        self.selection = text;
        true
    }

    #[must_use]
    pub fn selection(&self) -> Option<&str> {
        self.selection.as_deref()
    }

    /// The selection, where it is a word worth looking up: at most three
    /// words, at most forty characters, and at least one letter in it. A
    /// cloze's row of underscores is not a word and a paragraph is not a
    /// term, so neither puts **lookup** on the bar.
    #[must_use]
    pub fn lookup_term(&self) -> Option<String> {
        let term = self.selection.as_deref()?.trim();
        let words = term.split_whitespace().count();
        if words == 0 || words > 3 || term.chars().count() > 40 {
            return None;
        }
        term.chars()
            .any(char::is_alphabetic)
            .then(|| term.to_string())
    }

    /// `(done, total)` — how far the lesson has come.
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        let exs = self.exercises();
        let done = exs.iter().filter(|e| e.done()).count();
        (done, exs.len())
    }

    /// Whether there is something to check: a choice made, or text typed.
    #[must_use]
    pub fn can_submit(&self) -> bool {
        match self.current() {
            Some(ex) if ex.typed() => !self.typed.trim().is_empty(),
            Some(_) => self.choice.is_some(),
            None => false,
        }
    }

    /// The next hint, if there is one left to show.
    pub fn hint(&mut self, s: &mut Session) {
        let Some(ex) = self.current() else { return };
        if self.phase != Phase::Answering {
            return;
        }
        if (self.hints_shown as usize) < ex.hints.len() {
            self.hints_shown += 1;
            s.redraw();
        }
    }

    pub fn move_choice(&mut self, d: isize, s: &mut Session) {
        let Some(ex) = self.current() else { return };
        if self.phase != Phase::Answering || ex.choices.is_empty() {
            return;
        }
        let n = ex.choices.len() as isize;
        let next = match self.choice {
            None => {
                if d > 0 {
                    0
                } else {
                    n - 1
                }
            }
            Some(c) => (c as isize + d).clamp(0, n - 1),
        };
        self.choice = Some(next as usize);
        s.redraw();
    }

    /// A choice, picked: it is the answer, and it is checked at once.
    pub fn choose(&mut self, i: usize, s: &mut Session) {
        let Some(ex) = self.current() else { return };
        if self.phase != Phase::Answering || i >= ex.choices.len() {
            return;
        }
        self.choice = Some(i);
        self.typed = ex.choices[i].clone();
        self.submit(s);
    }

    /// Checks what was answered. A closed exercise is graded here; a
    /// self-check one shows its model answer and waits for the grade —
    /// unless the answer *is* the model answer, which needs no judgement.
    pub fn submit(&mut self, s: &mut Session) {
        let Some(ex) = self.current() else { return };
        if self.phase != Phase::Answering || !self.can_submit() {
            return;
        }
        let answer = self.typed.trim().to_string();
        // The seconds this exercise took, which is what the lesson's
        // minutes add up. A panel left standing has no other clock than
        // this one, so a stretch nobody was here for is cut at half an hour.
        let elapsed = (self.now - self.started_at).clamp(0.0, LONGEST_SITTING);
        let q = ex.seq;
        if ex.self_check() && !model::matches_model(&answer, &ex.model, &ex.accepted) {
            let patch =
                Patch { answer: Some(answer.clone()), hints_shown: self.hints_shown, elapsed, ..Patch::default() };
            if model::record(s, &ex, patch, format!("answer Q{q}")) {
                self.phase = Phase::SelfGrade;
                // The tutor is asked the moment the answer is written, and
                // nobody waits for it: the model answer is already up and
                // the grade pad is already on the bar.
                tutor::grade(s, &ex, &answer);
            }
        } else {
            let g = if ex.self_check() { Closed::Correct } else { model::grade_closed(&answer, &ex.accepted) };
            let patch = Patch {
                answer: Some(answer),
                result: Some(g),
                self_grade: ex.self_check().then_some(5),
                hints_shown: self.hints_shown,
                elapsed,
                quality: Some(g.quality()),
            };
            if model::record(s, &ex, patch, format!("answer Q{q}: {}", g.word())) {
                self.phase = Phase::Feedback(g);
            }
        }
        s.redraw();
    }

    /// The learner's own grade on a self-check answer, then on.
    ///
    /// Where the tutor's word has already landed — it was asked the moment
    /// the answer was written, and it may be quicker than the learner —
    /// the tutor's quality is what the items are graded at, and the
    /// learner's stays beside it as the self-grade the calibration line
    /// reads. The other order rewrites the filed grades when the tutor
    /// arrives; this order files them right the first time.
    pub fn grade(&mut self, quality: i64, s: &mut Session) {
        let Some(ex) = self.current() else { return };
        if self.phase != Phase::SelfGrade {
            return;
        }
        let quality = quality.clamp(0, 5);
        let filed = ex.tutor_grade.unwrap_or(quality);
        let patch = Patch { self_grade: Some(quality), quality: Some(filed), ..Patch::default() };
        if model::record(s, &ex, patch, format!("self-grade Q{} {quality}/5", ex.seq)) {
            self.advance(s);
        }
    }

    /// The next exercise, or the summary after the last.
    pub fn advance(&mut self, s: &mut Session) {
        if self.phase == Phase::Done {
            return;
        }
        let total = self.exercises().len();
        self.index += 1;
        self.typed.clear();
        self.choice = None;
        self.selection = None;
        self.hints_shown = 0;
        self.started_at = self.now;
        if self.index >= total {
            self.finish(s);
        } else {
            self.phase = Phase::Answering;
        }
        s.redraw();
    }

    /// Closes the lesson where it stands — the report is what was answered.
    ///
    /// The tutor is called from here, with this slot as its own, so the
    /// chat opens beside the summary: what it is asked to do is grade the
    /// writing in this lesson and author the next one, and the lesson goes
    /// into the chat as the chip. A build without the agent app says so and
    /// the summary is the whole of it.
    pub fn finish(&mut self, s: &mut Session) {
        let open = self.row().is_some_and(|l| l.status != "done");
        // The summary of a lesson whose row is still open — reopened after
        // its last answer, or its finish undone — is the one summary that
        // can still finish.
        if self.phase == Phase::Done && !open {
            return;
        }
        let mut closed = false;
        if open {
            closed = model::finish(s, self.lesson);
        }
        self.phase = Phase::Done;
        self.index = self.exercises().len();
        s.redraw();
        if closed {
            // After the event, because this runs as `&mut self` off the bar
            // and the chip the tutor carries is read off this very panel.
            let (slot, lesson) = (self.slot, self.lesson);
            s.after_event(move |s| super::super::tutor::compile(s, slot, lesson));
        }
    }

    /// Reads the exercise's audio out. A listening exercise still being
    /// answered keeps its words off the screen: the toast says it is
    /// speaking and no more, and the transcript shows once the answer is
    /// in. A world with no voice shows the words anyway — there is no other
    /// way to answer it.
    pub fn play(&self, s: &mut Session) {
        let (audio, unseen) = self
            .current()
            .map(|e| (e.audio, e.kind == "listen_mcq" && self.phase == Phase::Answering))
            .unwrap_or_default();
        if unseen {
            speak_unseen(s, &audio);
        } else {
            speak(s, &audio);
        }
    }

    /// `(right, total, minutes)` for the summary; a lesson played on
    /// another day answers with what its row was stamped with.
    #[must_use]
    pub fn outcome(&self) -> (usize, usize, f64) {
        let exs = self.exercises();
        let row = self.row();
        if exs.is_empty() {
            if let Some(r) = &row {
                let total = 10usize;
                let right = (r.accuracy.unwrap_or(0.0) * total as f64).round() as usize;
                return (right, total, r.minutes.unwrap_or(0.0));
            }
        }
        let (right, total, mut minutes) = model::outcome(&exs);
        if let Some(m) = row.as_ref().and_then(|r| r.minutes) {
            minutes = m;
        }
        (right, total, minutes)
    }
}

impl Panel for Lesson {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.row().map_or_else(|| "lesson".into(), |l| l.title)
    }
    fn about(&self) -> String {
        format!(
            "Lesson {} of the course, played one exercise at a time: the caption says the \
             section and the kind, the prompt is under it, then a field or the choices. A \
             closed exercise is graded on the spot against its accepted answers; a self_check \
             one shows the model answer and takes the learner's own 0–5 grade, which the tutor \
             may later overrule with fluent.grade. Every answer is a row as it is given, so the \
             lesson resumes where it was left. A self_check answer is also sent to the tutor \
             the moment it is written, and its grade lands on the exercise a few seconds later \
             unless the learner or the lesson's end got there first. Every word of the course's \
             own language on the stage is selectable: a selection of up to three words puts \
             lookup on the bar, which opens that word's dictionary entry joined to the lesson. \
             After the last exercise the same panel is the summary: right of total, the \
             minutes, every correction, and how the self-grades matched the tutor's.",
            self.lesson
        )
    }
    fn context_text_columns(&self) -> &'static [&'static str] {
        &["prompt", "passage", "answer", "model", "tutor_note"]
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let slot = self.slot;
        let ask = || {
            Verb::call("fluent.ask", "ask", Some('a'), move |s| {
                let Some(agent) = s.apps().get("agent") else {
                    s.notify("no agent app in this build to ask", true);
                    return;
                };
                if !agent.ask(s, slot) {
                    s.notify("the tutor could not take this panel", true);
                }
            })
        };
        // A word selected in one of the stage's own runs, which is the
        // one verb here that comes from the pointer and not from the phase.
        let lookup = || {
            self.lookup_term().map(|term| {
                Verb::go(
                    "fluent.lookup",
                    "lookup",
                    Some('k'),
                    Nav::Open { from: self.slot, id: Lookup::id(&term), fresh: false },
                )
            })
        };
        let mut v = Vec::new();
        let Some(ex) = self.current() else {
            // Every answer given and the row still open: the summary that
            // finishes — feeds the streak and calls the tutor — on a press.
            if !self.exercises().is_empty() && self.row().is_some_and(|l| l.status != "done") {
                v.push(Verb::run("fluent.end", "finish", Some('e')));
            }
            v.push(ask());
            v.push(Verb::go(
                "fluent.history",
                "history",
                Some('h'),
                Nav::Open { from: self.slot, id: History::id(), fresh: false },
            ));
            v.extend(lookup());
            return v;
        };
        match self.phase {
            Phase::Answering => {
                if ex.typed() {
                    v.push(Verb::run("fluent.check", "check", Some('c')));
                }
                if (self.hints_shown as usize) < ex.hints.len() {
                    v.push(Verb::run("fluent.hint", "hint", Some('h')));
                }
                if !ex.audio.is_empty() {
                    v.push(Verb::run("fluent.play", "play", Some('y')));
                }
                v.push(Verb::run("fluent.end", "end", Some('e')));
            }
            Phase::Feedback(_) => {
                v.push(Verb::run("fluent.next", "next", Some('n')));
                if !ex.audio.is_empty() {
                    v.push(Verb::run("fluent.play", "play", Some('y')));
                }
                v.push(ask());
            }
            Phase::SelfGrade => {
                v.extend(grade_verbs());
                v.push(ask());
            }
            Phase::Done => {}
        }
        v.extend(lookup());
        v
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "fluent.check" => self.submit(s),
            "fluent.hint" => self.hint(s),
            "fluent.play" => self.play(s),
            "fluent.end" => self.finish(s),
            "fluent.next" => self.advance(s),
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

pub struct LessonKind;
impl PanelKind for LessonKind {
    fn tag(&self) -> Tag {
        Lesson::TAG
    }
    fn coalesce_navigation(&self) -> bool {
        false
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let lesson = id.arg(0).and_then(|a| a.parse().ok()).unwrap_or(0);
        let store = cx.session().store().clone();
        let now = cx.session().now();
        let exs = model::exercises(&store, lesson);
        // Where the rows say the lesson stands: the first exercise not
        // done — at its grade pad where it was answered and never graded —
        // or the summary after the last.
        let index = exs.iter().position(|e| !e.done()).unwrap_or(exs.len());
        let mut player = Lesson {
            id: id.clone(),
            slot: 0,
            lesson,
            store,
            index,
            phase: Phase::Answering,
            typed: String::new(),
            choice: None,
            selection: None,
            hints_shown: 0,
            started_at: now,
            now,
        };
        player.reconcile();
        Box::new(player)
    }
}

/// The verdict a done exercise showed: the word its row keeps, or for a
/// self-check one graded by hand, whether the grade counted it right.
fn closed_of(ex: &Exercise) -> Closed {
    ex.result
        .as_deref()
        .and_then(Closed::from_word)
        .unwrap_or(if ex.counts_correct() { Closed::Correct } else { Closed::Wrong })
}
