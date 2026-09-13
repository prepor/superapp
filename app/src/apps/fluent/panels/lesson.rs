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
use super::{grade_of, grade_verbs, speak, History};

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
    }

    pub fn set_typed(&mut self, text: String) {
        self.typed = text;
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
        let elapsed = (self.now - self.started_at).max(0.0);
        let q = ex.seq;
        if ex.self_check() && !model::matches_model(&answer, &ex.model, &ex.accepted) {
            let patch = Patch { answer: Some(answer), hints_shown: self.hints_shown, elapsed, ..Patch::default() };
            if model::record(s, &ex, patch, format!("answer Q{q}")) {
                self.phase = Phase::SelfGrade;
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
    pub fn grade(&mut self, quality: i64, s: &mut Session) {
        let Some(ex) = self.current() else { return };
        if self.phase != Phase::SelfGrade {
            return;
        }
        let patch = Patch { self_grade: Some(quality), quality: Some(quality), ..Patch::default() };
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
    pub fn finish(&mut self, s: &mut Session) {
        if self.phase == Phase::Done {
            return;
        }
        if self.row().is_some_and(|l| l.status != "done") {
            model::finish(s, self.lesson);
        }
        self.phase = Phase::Done;
        self.index = self.exercises().len();
        s.redraw();
    }

    pub fn play(&self, s: &mut Session) {
        let audio = self.current().map(|e| e.audio).unwrap_or_default();
        speak(s, &audio);
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
        let (right, total, mut minutes) = model::outcome(&exs, row.as_ref().and_then(|r| r.started), self.now);
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
             lesson resumes where it was left. After the last exercise the same panel is the \
             summary: right of total, the minutes, every correction, and how the self-grades \
             matched the tutor's.",
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
        let mut v = Vec::new();
        let Some(ex) = self.current() else {
            v.push(ask());
            v.push(Verb::go(
                "fluent.history",
                "history",
                Some('h'),
                Nav::Open { from: self.slot, id: History::id(), fresh: false },
            ));
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
        let row = model::lesson(&store, lesson);
        let index = exs.iter().position(|e| !e.done()).unwrap_or(exs.len());
        let finished = row.as_ref().is_some_and(|l| l.status == "done") || index >= exs.len();
        Box::new(Lesson {
            id: id.clone(),
            slot: 0,
            lesson,
            store,
            index,
            phase: if finished { Phase::Done } else { Phase::Answering },
            typed: String::new(),
            choice: None,
            hints_shown: 0,
            started_at: now,
            now,
        })
    }
}
