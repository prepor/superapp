//! What the panels read, and the few things they write.
//!
//! Every read is a registered query, so a panel follows a commit under it
//! and an agent's chip carries the SQL that drew it. Every write is one
//! undoable action with the intent that reverses it: an answer, a grade,
//! a finished lesson.

use std::rc::Rc;
use std::sync::Mutex;

use kernel::effect::World;
use kernel::filter::Op;
use kernel::history::Intent;
use kernel::richtable::{Dir, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, Values};
use kernel::session::{Action, Session};
use kernel::store::{Store, Val};
use kernel::time::{civil_from_days, fmt_date};
use rusqlite::{params, Connection, OptionalExtension};

use super::sm2::{self, State, DAY};

// ---------------------------------------------------------------------------
// The learner and the desk
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Learner {
    pub name: String,
    pub native: String,
    pub target: String,
    pub level: String,
    pub goal: String,
    pub daily_minutes: i64,
    pub streak: i64,
    pub last_active: Option<f64>,
}

impl Learner {
    /// Whether the streak still counts: fed today or yesterday.
    #[must_use]
    pub fn streak_active(&self, now: f64) -> bool {
        self.last_active
            .is_some_and(|t| sm2::days_between(t, now) <= 1)
    }
}

pub fn learner(store: &Store) -> Option<Learner> {
    store
        .rows_sql(
            "fluent learner",
            "the learner: name, languages, level, streak",
            "SELECT name, native, target, level, goal, daily_minutes, streak, last_active FROM fluent_learner WHERE id = 1",
            &[],
            |r| {
                Ok(Learner {
                    name: r.get(0)?,
                    native: r.get(1)?,
                    target: r.get(2)?,
                    level: r.get(3)?,
                    goal: r.get(4)?,
                    daily_minutes: r.get(5)?,
                    streak: r.get(6)?,
                    last_active: r.get(7)?,
                })
            },
        )
        .first()
        .cloned()
}

/// What is on the shelf: the newest lesson that is not done.
#[derive(Clone, Debug, PartialEq)]
pub struct Shelf {
    pub id: i64,
    pub title: String,
    pub for_date: f64,
    pub focus: Vec<String>,
    pub status: String,
    pub exercises: i64,
    pub reviews: i64,
    pub started: bool,
}

pub fn shelf(store: &Store) -> Option<Shelf> {
    store
        .rows_sql(
            "fluent shelf",
            "the lesson on the shelf: the newest that is building or ready",
            "SELECT l.id, l.title, l.for_date, l.focus, l.status,
                    (SELECT COUNT(*) FROM fluent_exercise e WHERE e.lesson = l.id),
                    (SELECT COUNT(*) FROM fluent_exercise e WHERE e.lesson = l.id AND e.section = 'review'),
                    l.started IS NOT NULL
               FROM fluent_lesson l WHERE l.status IN ('ready', 'building')
              ORDER BY l.for_date DESC, l.id DESC LIMIT 1",
            &[],
            |r| {
                Ok(Shelf {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    for_date: r.get(2)?,
                    focus: json_strings(&r.get::<_, String>(3)?),
                    status: r.get(4)?,
                    exercises: r.get(5)?,
                    reviews: r.get(6)?,
                    started: r.get(7)?,
                })
            },
        )
        .first()
        .cloned()
}

/// How many items are due by `now`: everything, and the cards alone.
pub fn due_counts(store: &Store, now: f64) -> (i64, i64) {
    let today = sm2::day_start(now);
    store
        .rows_sql(
            "fluent due",
            "how many items are due today, and how many of them are cards",
            "SELECT COUNT(*), SUM(kind = 'vocab') FROM fluent_item WHERE due <= ?1",
            &[Val::F(today)],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .first()
        .copied()
        .unwrap_or((0, 0))
}

/// The accuracy of the last `n` finished lessons, oldest first, with the
/// day each was for.
pub fn recent_accuracy(store: &Store, n: i64) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = store
        .rows_sql(
            "fluent accuracy",
            "the accuracy of the newest finished lessons",
            "SELECT for_date, accuracy FROM fluent_lesson WHERE status = 'done' AND accuracy IS NOT NULL
              ORDER BY for_date DESC, id DESC LIMIT ?1",
            &[Val::I(n)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .to_vec();
    v.reverse();
    v
}

/// Words learned — cards reviewed at least once — and how many of the
/// deck's cards arrived in the last seven days.
pub fn words(store: &Store, now: f64) -> (i64, i64) {
    let week_ago = sm2::day_start(now) - 7.0 * DAY;
    store
        .rows_sql(
            "fluent words",
            "cards learned, and the cards that arrived this week",
            "SELECT SUM(i.reps > 0), SUM(i.created >= ?1) FROM fluent_item i WHERE i.kind = 'vocab'",
            &[Val::F(week_ago)],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                ))
            },
        )
        .first()
        .copied()
        .unwrap_or((0, 0))
}

// ---------------------------------------------------------------------------
// Items and cards
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub ease: f64,
    pub interval: i64,
    pub reps: i64,
    pub due: f64,
    pub reviewed: Option<f64>,
    pub mastery: i64,
}

impl Item {
    #[must_use]
    pub fn state(&self) -> State {
        State { ease: self.ease, interval: self.interval, reps: self.reps }
    }
}

fn item_tx(c: &Connection, id: &str) -> rusqlite::Result<Option<Item>> {
    c.query_row(
        "SELECT id, kind, content, ease, interval, reps, due, reviewed, mastery FROM fluent_item WHERE id = ?1",
        [id],
        |r| {
            Ok(Item {
                id: r.get(0)?,
                kind: r.get(1)?,
                content: r.get(2)?,
                ease: r.get(3)?,
                interval: r.get(4)?,
                reps: r.get(5)?,
                due: r.get(6)?,
                reviewed: r.get(7)?,
                mastery: r.get(8)?,
            })
        },
    )
    .optional()
}

fn put_item_tx(c: &Connection, it: &Item) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE fluent_item SET ease = ?2, interval = ?3, reps = ?4, due = ?5, reviewed = ?6, mastery = ?7 WHERE id = ?1",
        params![it.id, it.ease, it.interval, it.reps, it.due, it.reviewed, it.mastery],
    )?;
    Ok(())
}

/// One card with its schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct CardRow {
    pub item: String,
    pub front: String,
    pub back: String,
    pub example: String,
    pub audio: String,
    pub notes: String,
    pub due: f64,
    pub reps: i64,
    pub ease: f64,
    pub mastery: i64,
    pub reviewed: Option<f64>,
}

const CARD_SELECT: &str =
    "c.item, c.front, c.back, c.example, c.audio, c.notes, i.due, i.reps, i.ease, i.mastery, i.reviewed";

fn card_of(r: &rusqlite::Row) -> rusqlite::Result<CardRow> {
    Ok(CardRow {
        item: r.get(0)?,
        front: r.get(1)?,
        back: r.get(2)?,
        example: r.get(3)?,
        audio: r.get(4)?,
        notes: r.get(5)?,
        due: r.get(6)?,
        reps: r.get(7)?,
        ease: r.get(8)?,
        mastery: r.get(9)?,
        reviewed: r.get(10)?,
    })
}

pub static CARDS: SqlSource<CardRow, String> = SqlSource {
    spec: &SqlSpec {
        id: "fluent cards",
        describe: "the deck: every card with its schedule, soonest due first",
        select: CARD_SELECT,
        from: "fluent_card c JOIN fluent_item i ON i.id = c.item",
        base: "1",
        text: &["c.front", "c.back", "c.example"],
        index: None,
        tags: &[
            ("new", TagSql::Where("i.reps = 0")),
            ("weak", TagSql::Where("(i.ease < 2.0 OR i.mastery <= 2) AND i.reps > 0")),
            ("learned", TagSql::Where("i.reps > 0")),
            ("mastery", TagSql::Col("i.mastery")),
        ],
        order: &[("i.due", Dir::Asc), ("c.front", Dir::Asc)],
        group: None,
        key: "c.item",
        deps: &[],
    },
    tags: &[
        TagDef { name: "new", kind: TagType::Bool, ops: &[], describe: "never reviewed", values: Values::None },
        TagDef { name: "weak", kind: TagType::Bool, ops: &[], describe: "a low ease or a low mastery", values: Values::None },
        TagDef { name: "learned", kind: TagType::Bool, ops: &[], describe: "reviewed at least once", values: Values::None },
        TagDef { name: "mastery", kind: TagType::Number, ops: &[Op::Eq, Op::Lt, Op::Lte, Op::Gt, Op::Gte], describe: "the 0–5 stamp", values: Values::None },
    ],
    map: card_of,
    key: |r| r.item.clone(),
    rank: |r| vec![Val::F(r.due), Val::S(r.front.clone())],
    suggest: |_, _, _| Vec::new(),
};

pub fn card(store: &Store, item: &str) -> Option<CardRow> {
    store
        .rows_sql(
            "fluent card",
            "one card, with its schedule",
            "SELECT c.item, c.front, c.back, c.example, c.audio, c.notes, i.due, i.reps, i.ease, i.mastery, i.reviewed
               FROM fluent_card c JOIN fluent_item i ON i.id = c.item WHERE c.item = ?1",
            &[Val::S(item.to_string())],
            card_of,
        )
        .first()
        .cloned()
}

/// The cards due by `now`, soonest first — the review's queue.
pub fn due_cards(store: &Store, now: f64) -> Rc<Vec<CardRow>> {
    store.rows_sql(
        "fluent due cards",
        "the cards due today, soonest first",
        "SELECT c.item, c.front, c.back, c.example, c.audio, c.notes, i.due, i.reps, i.ease, i.mastery, i.reviewed
           FROM fluent_card c JOIN fluent_item i ON i.id = c.item WHERE i.due <= ?1 ORDER BY i.due, c.front",
        &[Val::F(sm2::day_start(now))],
        card_of,
    )
}

/// The day the next card comes due after `now`, if any is scheduled.
pub fn next_due(store: &Store, now: f64) -> Option<f64> {
    store
        .rows_sql(
            "fluent next due",
            "when the next card comes due",
            "SELECT MIN(i.due) FROM fluent_item i WHERE i.kind = 'vocab' AND i.due > ?1",
            &[Val::F(sm2::day_start(now))],
            |r| r.get::<_, Option<f64>>(0),
        )
        .first()
        .copied()
        .flatten()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewRow {
    pub at: f64,
    pub quality: i64,
    pub lesson: Option<i64>,
    pub device: String,
}

pub fn reviews_of(store: &Store, item: &str) -> Rc<Vec<ReviewRow>> {
    store.rows_sql(
        "fluent reviews",
        "every grade one item was ever given, newest first",
        "SELECT at, quality, lesson, device FROM fluent_review WHERE item = ?1 ORDER BY at DESC, id DESC",
        &[Val::S(item.to_string())],
        |r| Ok(ReviewRow { at: r.get(0)?, quality: r.get(1)?, lesson: r.get(2)?, device: r.get(3)? }),
    )
}

// ---------------------------------------------------------------------------
// Lessons and exercises
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct LessonRow {
    pub id: i64,
    pub title: String,
    pub for_date: f64,
    pub focus: Vec<String>,
    pub status: String,
    pub started: Option<f64>,
    pub ended: Option<f64>,
    pub accuracy: Option<f64>,
    pub minutes: Option<f64>,
    pub notes: String,
}

fn lesson_of(r: &rusqlite::Row) -> rusqlite::Result<LessonRow> {
    Ok(LessonRow {
        id: r.get(0)?,
        title: r.get(1)?,
        for_date: r.get(2)?,
        focus: json_strings(&r.get::<_, String>(3)?),
        status: r.get(4)?,
        started: r.get(5)?,
        ended: r.get(6)?,
        accuracy: r.get(7)?,
        minutes: r.get(8)?,
        notes: r.get(9)?,
    })
}

const LESSON_SELECT: &str =
    "l.id, l.title, l.for_date, l.focus, l.status, l.started, l.ended, l.accuracy, l.minutes, l.notes";

pub static LESSONS: SqlSource<LessonRow, i64> = SqlSource {
    spec: &SqlSpec {
        id: "fluent lessons",
        describe: "the lessons played, newest first",
        select: LESSON_SELECT,
        from: "fluent_lesson l",
        base: "l.status = 'done'",
        text: &["l.title", "l.focus", "l.notes"],
        index: None,
        tags: &[
            ("date", TagSql::Col("l.for_date")),
            ("accuracy", TagSql::Col("l.accuracy")),
        ],
        order: &[("l.for_date", Dir::Desc), ("l.id", Dir::Desc)],
        group: None,
        key: "l.id",
        deps: &[],
    },
    tags: &[
        TagDef { name: "date", kind: TagType::Date, ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte], describe: "the day the lesson was for", values: Values::None },
        TagDef { name: "accuracy", kind: TagType::Number, ops: &[Op::Lt, Op::Lte, Op::Gt, Op::Gte], describe: "a fraction, 0 to 1", values: Values::None },
    ],
    map: lesson_of,
    key: |r| r.id,
    rank: |r| vec![Val::F(r.for_date), Val::I(r.id)],
    suggest: |_, _, _| Vec::new(),
};

pub fn lesson(store: &Store, id: i64) -> Option<LessonRow> {
    store
        .rows_sql(
            "fluent lesson",
            "one lesson: its title, day, focus, status and outcome",
            "SELECT l.id, l.title, l.for_date, l.focus, l.status, l.started, l.ended, l.accuracy, l.minutes, l.notes
               FROM fluent_lesson l WHERE l.id = ?1",
            &[Val::I(id)],
            lesson_of,
        )
        .first()
        .cloned()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Exercise {
    pub id: i64,
    pub lesson: i64,
    pub seq: i64,
    pub section: String,
    pub kind: String,
    pub grading: String,
    pub prompt: String,
    pub passage: String,
    pub audio: String,
    pub choices: Vec<String>,
    pub accepted: Vec<String>,
    pub model: String,
    pub hints: Vec<String>,
    pub explanation: String,
    pub items: Vec<String>,
    pub difficulty: i64,
    pub answer: Option<String>,
    pub result: Option<String>,
    pub self_grade: Option<i64>,
    pub tutor_grade: Option<i64>,
    pub tutor_note: String,
    pub tutor_fix: String,
    pub hints_shown: i64,
    pub elapsed: f64,
    pub answered: Option<f64>,
}

impl Exercise {
    /// Whether the learner answers by typing (a field) or by choosing.
    #[must_use]
    pub fn typed(&self) -> bool {
        matches!(self.kind.as_str(), "cloze" | "translate" | "free_write")
    }

    #[must_use]
    pub fn self_check(&self) -> bool {
        self.grading == "self_check"
    }

    /// Whether the record says this one is finished with: a closed answer
    /// graded, or a self-check one self-graded.
    #[must_use]
    pub fn done(&self) -> bool {
        self.answer.is_some() && (self.result.is_some() || self.self_grade.is_some())
    }

    /// Whether it counts as right for the accuracy line.
    #[must_use]
    pub fn counts_correct(&self) -> bool {
        match self.result.as_deref() {
            Some("correct" | "almost") => true,
            Some(_) => false,
            None => self.tutor_grade.or(self.self_grade).is_some_and(|q| q >= 3),
        }
    }

    /// The answer to show beside a wrong one: the model, or the first
    /// accepted.
    #[must_use]
    pub fn key_answer(&self) -> String {
        if !self.model.is_empty() {
            self.model.clone()
        } else {
            self.accepted.first().cloned().unwrap_or_default()
        }
    }

    /// The caption over the exercise: `REVIEW · CLOZE`.
    #[must_use]
    pub fn caption(&self) -> String {
        format!("{} · {}", section_word(&self.section), kind_word(&self.kind)).to_uppercase()
    }
}

#[must_use]
pub fn section_word(s: &str) -> &str {
    match s {
        "warmup" => "warm-up",
        "set_piece" => "set piece",
        other => other,
    }
}

#[must_use]
pub fn kind_word(k: &str) -> &str {
    match k {
        "free_write" => "free write",
        "listen_mcq" => "listen",
        "read_mcq" => "read",
        "mcq" => "choose",
        other => other,
    }
}

const EXERCISE_SELECT: &str = "e.id, e.lesson, e.seq, e.section, e.kind, e.grading, e.prompt, e.passage, e.audio, e.choices, e.accepted, e.model, e.hints, e.explanation, e.items, e.difficulty, e.answer, e.result, e.self_grade, e.tutor_grade, e.tutor_note, e.tutor_fix, e.hints_shown, e.elapsed, e.answered";

fn exercise_of(r: &rusqlite::Row) -> rusqlite::Result<Exercise> {
    Ok(Exercise {
        id: r.get(0)?,
        lesson: r.get(1)?,
        seq: r.get(2)?,
        section: r.get(3)?,
        kind: r.get(4)?,
        grading: r.get(5)?,
        prompt: r.get(6)?,
        passage: r.get(7)?,
        audio: r.get(8)?,
        choices: json_strings(&r.get::<_, String>(9)?),
        accepted: json_strings(&r.get::<_, String>(10)?),
        model: r.get(11)?,
        hints: json_strings(&r.get::<_, String>(12)?),
        explanation: r.get(13)?,
        items: json_strings(&r.get::<_, String>(14)?),
        difficulty: r.get(15)?,
        answer: r.get(16)?,
        result: r.get(17)?,
        self_grade: r.get(18)?,
        tutor_grade: r.get(19)?,
        tutor_note: r.get(20)?,
        tutor_fix: r.get(21)?,
        hints_shown: r.get(22)?,
        elapsed: r.get(23)?,
        answered: r.get(24)?,
    })
}

pub fn exercises(store: &Store, lesson: i64) -> Rc<Vec<Exercise>> {
    store.rows_sql(
        "fluent exercises",
        "a lesson's exercises in order, with what was answered",
        "SELECT e.id, e.lesson, e.seq, e.section, e.kind, e.grading, e.prompt, e.passage, e.audio, e.choices, e.accepted, e.model, e.hints, e.explanation, e.items, e.difficulty, e.answer, e.result, e.self_grade, e.tutor_grade, e.tutor_note, e.tutor_fix, e.hints_shown, e.elapsed, e.answered
           FROM fluent_exercise e WHERE e.lesson = ?1 ORDER BY e.seq",
        &[Val::I(lesson)],
        exercise_of,
    )
}

fn exercise_tx(c: &Connection, id: i64) -> rusqlite::Result<Option<Exercise>> {
    c.query_row(
        &format!("SELECT {EXERCISE_SELECT} FROM fluent_exercise e WHERE e.id = ?1"),
        [id],
        exercise_of,
    )
    .optional()
}

// ---------------------------------------------------------------------------
// Grading
// ---------------------------------------------------------------------------

/// What a closed exercise says about an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    Correct,
    Almost,
    Wrong,
}

impl Closed {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Closed::Correct => "correct",
            Closed::Almost => "almost",
            Closed::Wrong => "wrong",
        }
    }

    /// The headline the feedback wears.
    #[must_use]
    pub fn headline(self) -> &'static str {
        match self {
            Closed::Correct => "Richtig!",
            Closed::Almost => "Fast!",
            Closed::Wrong => "Nicht ganz.",
        }
    }

    /// The SM-2 quality a closed grade stands for.
    #[must_use]
    pub fn quality(self) -> i64 {
        match self {
            Closed::Correct => 5,
            Closed::Almost => 4,
            Closed::Wrong => 2,
        }
    }
}

fn loose(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace('ß', "ss")
        .replace('ä', "ae")
        .replace('ö', "oe")
        .replace('ü', "ue")
        .trim_end_matches(['.', '!', '?'])
        .to_string()
}

/// A closed grade: exact after trimming is right; the same letters with a
/// case or umlaut slip is almost; anything else is wrong.
#[must_use]
pub fn grade_closed(answer: &str, accepted: &[String]) -> Closed {
    let a = answer.trim();
    if accepted.iter().any(|x| x.trim() == a) {
        return Closed::Correct;
    }
    let l = loose(a);
    if !l.is_empty() && accepted.iter().any(|x| loose(x) == l) {
        return Closed::Almost;
    }
    Closed::Wrong
}

/// A self-check answer that is the model answer, or one of the accepted
/// ones, needs no judgement.
#[must_use]
pub fn matches_model(answer: &str, model: &str, accepted: &[String]) -> bool {
    let l = loose(answer);
    !l.is_empty() && (loose(model) == l || accepted.iter().any(|x| loose(x) == l))
}

/// What a record writes onto an exercise.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    pub answer: Option<String>,
    pub result: Option<Closed>,
    pub self_grade: Option<i64>,
    pub hints_shown: i64,
    pub elapsed: f64,
    /// When `Some`, a review of this quality is filed for each of the
    /// exercise's items and their schedules move.
    pub quality: Option<i64>,
}

#[derive(Debug)]
struct Recorded {
    exercise: i64,
    before: Exercise,
    items_before: Vec<Item>,
    reviews: Mutex<Vec<i64>>,
    patch: Patch,
    at: f64,
    device: String,
}

fn apply_patch_tx(
    c: &Connection,
    ex: &Exercise,
    patch: &Patch,
    at: f64,
    device: &str,
) -> rusqlite::Result<(Vec<Item>, Vec<i64>)> {
    c.execute(
        "UPDATE fluent_exercise SET answer = COALESCE(?2, answer), result = COALESCE(?3, result),
                self_grade = COALESCE(?4, self_grade), hints_shown = MAX(hints_shown, ?5),
                elapsed = elapsed + ?6, answered = COALESCE(answered, ?7)
          WHERE id = ?1",
        params![
            ex.id,
            patch.answer,
            patch.result.map(Closed::word),
            patch.self_grade,
            patch.hints_shown,
            patch.elapsed,
            at
        ],
    )?;
    c.execute(
        "UPDATE fluent_lesson SET started = COALESCE(started, ?2) WHERE id = ?1",
        params![ex.lesson, at],
    )?;
    let mut before = Vec::new();
    let mut reviews = Vec::new();
    if let Some(q) = patch.quality {
        for (n, item) in ex.items.iter().enumerate() {
            let Some(it) = item_tx(c, item)? else { continue };
            before.push(it.clone());
            // A review's row key is the item, the instant and the device: two
            // grades of one item inside one second are stamped a millisecond
            // apart, so the key stays unique whatever the clock says.
            let stamp = free_stamp(c, item, at + n as f64 * 0.001, device)?;
            c.execute(
                "INSERT INTO fluent_review(item, at, quality, device, lesson, exercise) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![item, stamp, q, device, ex.lesson, ex.id],
            )?;
            reviews.push(c.last_insert_rowid());
            let s = sm2::step(it.state(), q);
            put_item_tx(
                c,
                &Item {
                    ease: s.ease,
                    interval: s.interval,
                    reps: s.reps,
                    due: sm2::due_after(at, s),
                    reviewed: Some(at),
                    mastery: sm2::mastery_after(it.mastery, q),
                    ..it
                },
            )?;
        }
    }
    Ok((before, reviews))
}

/// The nearest instant at or after `at` no review of this item on this
/// device has been stamped with.
fn free_stamp(c: &Connection, item: &str, at: f64, device: &str) -> rusqlite::Result<f64> {
    let mut stamp = at;
    loop {
        let taken: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM fluent_review WHERE item = ?1 AND at = ?2 AND device = ?3)",
            params![item, stamp, device],
            |r| r.get(0),
        )?;
        if !taken {
            return Ok(stamp);
        }
        stamp += 0.001;
    }
}

fn restore_exercise_tx(c: &Connection, ex: &Exercise) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE fluent_exercise SET answer = ?2, result = ?3, self_grade = ?4, hints_shown = ?5, elapsed = ?6, answered = ?7 WHERE id = ?1",
        params![ex.id, ex.answer, ex.result, ex.self_grade, ex.hints_shown, ex.elapsed, ex.answered],
    )?;
    Ok(())
}

impl Intent for Recorded {
    fn describe(&self) -> String {
        format!("answer to exercise {}", self.exercise)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let before = self.before.clone();
        let items = self.items_before.clone();
        let reviews = self.reviews.lock().map_or_else(|e| e.into_inner().clone(), |r| r.clone());
        w.store()
            .write(move |c| {
                restore_exercise_tx(c, &before)?;
                for id in reviews {
                    c.execute("DELETE FROM fluent_review WHERE id = ?1", [id])?;
                }
                for it in &items {
                    put_item_tx(c, it)?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let before = self.before.clone();
        let patch = self.patch.clone();
        let (at, device) = (self.at, self.device.clone());
        let ids = w
            .store()
            .write(move |c| apply_patch_tx(c, &before, &patch, at, &device).map(|(_, ids)| ids))
            .map_err(|e| e.to_string())?;
        if let Ok(mut r) = self.reviews.lock() {
            *r = ids;
        }
        Ok(())
    }
}

/// Records what happened on one exercise, as one undoable action.
pub fn record(s: &mut Session, ex: &Exercise, patch: Patch, label: impl Into<String>) -> bool {
    let at = s.now();
    let device = s.store().device();
    let id = ex.id;
    let write_patch = patch.clone();
    let write_device = device.clone();
    let Some(Some((before, items_before, reviews))) = s.act(Action::writing(
        "fluent.answer",
        label,
        move |c| {
            let Some(before) = exercise_tx(c, id)? else { return Ok(None) };
            let (items, reviews) = apply_patch_tx(c, &before, &write_patch, at, &write_device)?;
            Ok(Some((before, items, reviews)))
        },
    )) else {
        return false;
    };
    s.claim(Box::new(Recorded {
        exercise: id,
        before,
        items_before,
        reviews: Mutex::new(reviews),
        patch,
        at,
        device,
    }));
    true
}

/// The tutor's word on a self-check answer: a grade, a note, a corrected
/// text. One undoable write; the schedule is not moved here.
pub fn tutor_grade(s: &mut Session, exercise: i64, quality: i64, note: &str, fix: &str) -> bool {
    struct Graded {
        exercise: i64,
        before: (Option<i64>, String, String),
        after: (Option<i64>, String, String),
    }
    fn set(w: &World, id: i64, v: &(Option<i64>, String, String)) -> Result<(), String> {
        let v = v.clone();
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE fluent_exercise SET tutor_grade = ?2, tutor_note = ?3, tutor_fix = ?4 WHERE id = ?1",
                    params![id, v.0, v.1, v.2],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    impl Intent for Graded {
        fn describe(&self) -> String {
            format!("tutor grade on exercise {}", self.exercise)
        }
        fn reverse(&self, w: &World) -> Result<(), String> {
            set(w, self.exercise, &self.before)
        }
        fn reapply(&self, w: &World) -> Result<(), String> {
            set(w, self.exercise, &self.after)
        }
    }
    let after = (Some(quality.clamp(0, 5)), note.to_string(), fix.to_string());
    let write_after = after.clone();
    let Some(Some(before)) = s.act(Action::writing(
        "fluent.tutor",
        format!("tutor grades exercise {exercise}: {}/5", quality.clamp(0, 5)),
        move |c| {
            let before = c
                .query_row(
                    "SELECT tutor_grade, tutor_note, tutor_fix FROM fluent_exercise WHERE id = ?1",
                    [exercise],
                    |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
                )
                .optional()?;
            let Some(before) = before else { return Ok(None) };
            c.execute(
                "UPDATE fluent_exercise SET tutor_grade = ?2, tutor_note = ?3, tutor_fix = ?4 WHERE id = ?1",
                params![exercise, write_after.0, write_after.1, write_after.2],
            )?;
            Ok(Some(before))
        },
    )) else {
        return false;
    };
    s.claim(Box::new(Graded { exercise, before, after }));
    true
}

/// A lesson's outcome, as its row will carry it: how many right of how
/// many, and the minutes.
#[must_use]
pub fn outcome(exs: &[Exercise], started: Option<f64>, now: f64) -> (usize, usize, f64) {
    let right = exs.iter().filter(|e| e.done() && e.counts_correct()).count();
    let minutes = started.map_or(0.0, |t| ((now - t) / 60.0).max(1.0).round());
    (right, exs.len(), minutes)
}

/// Closes a lesson: done, with its accuracy and minutes stamped, and a
/// building row put on the shelf for tomorrow — the tutor's to fill.
pub fn finish(s: &mut Session, lesson: i64) -> bool {
    struct Finished {
        lesson: i64,
        before: (String, Option<f64>, Option<f64>, Option<f64>),
        after: (String, Option<f64>, Option<f64>, Option<f64>),
        building: Mutex<Option<i64>>,
        tomorrow: f64,
    }
    impl Intent for Finished {
        fn describe(&self) -> String {
            format!("lesson {} finished", self.lesson)
        }
        fn reverse(&self, w: &World) -> Result<(), String> {
            let (id, v) = (self.lesson, self.before.clone());
            let building = self.building.lock().map_or(None, |b| *b);
            w.store()
                .write(move |c| {
                    c.execute(
                        "UPDATE fluent_lesson SET status = ?2, ended = ?3, accuracy = ?4, minutes = ?5 WHERE id = ?1",
                        params![id, v.0, v.1, v.2, v.3],
                    )?;
                    if let Some(b) = building {
                        c.execute("DELETE FROM fluent_lesson WHERE id = ?1 AND status = 'building'", [b])?;
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())
        }
        fn reapply(&self, w: &World) -> Result<(), String> {
            let (id, v, tomorrow) = (self.lesson, self.after.clone(), self.tomorrow);
            let b = w
                .store()
                .write(move |c| {
                    c.execute(
                        "UPDATE fluent_lesson SET status = ?2, ended = ?3, accuracy = ?4, minutes = ?5 WHERE id = ?1",
                        params![id, v.0, v.1, v.2, v.3],
                    )?;
                    building_tx(c, tomorrow)
                })
                .map_err(|e| e.to_string())?;
            if let Ok(mut slot) = self.building.lock() {
                *slot = b;
            }
            Ok(())
        }
    }
    let now = s.now();
    let tomorrow = sm2::day_start(now) + DAY;
    let Some(Some((before, after, building))) = s.act(Action::writing(
        "fluent.finish",
        "finish the lesson",
        move |c| {
            let before = c
                .query_row(
                    "SELECT status, ended, accuracy, minutes, started FROM fluent_lesson WHERE id = ?1",
                    [lesson],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<f64>>(1)?,
                            r.get::<_, Option<f64>>(2)?,
                            r.get::<_, Option<f64>>(3)?,
                            r.get::<_, Option<f64>>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((status, ended, accuracy, minutes, started)) = before else { return Ok(None) };
            let mut stmt = c.prepare(&format!("SELECT {EXERCISE_SELECT} FROM fluent_exercise e WHERE e.lesson = ?1 ORDER BY e.seq"))?;
            let exs: Vec<Exercise> = stmt.query_map([lesson], exercise_of)?.collect::<rusqlite::Result<_>>()?;
            let (right, total, mins) = outcome(&exs, started, now);
            let acc = if total == 0 { 0.0 } else { right as f64 / total as f64 };
            let after = ("done".to_string(), Some(now), Some(acc), Some(mins));
            c.execute(
                "UPDATE fluent_lesson SET status = 'done', ended = ?2, accuracy = ?3, minutes = ?4 WHERE id = ?1",
                params![lesson, now, acc, mins],
            )?;
            let building = building_tx(c, tomorrow)?;
            Ok(Some(((status, ended, accuracy, minutes), after, building)))
        },
    )) else {
        return false;
    };
    s.claim(Box::new(Finished { lesson, before, after, building: Mutex::new(building), tomorrow }));
    true
}

/// The shelf's placeholder for the lesson the tutor has yet to author:
/// one, never two.
fn building_tx(c: &Connection, tomorrow: f64) -> rusqlite::Result<Option<i64>> {
    let open: i64 = c.query_row(
        "SELECT COUNT(*) FROM fluent_lesson WHERE status IN ('ready', 'building')",
        [],
        |r| r.get(0),
    )?;
    if open > 0 {
        return Ok(None);
    }
    c.execute(
        "INSERT INTO fluent_lesson(title, for_date, status, generated) VALUES('', ?1, 'building', ?1)",
        [tomorrow],
    )?;
    Ok(Some(c.last_insert_rowid()))
}

// ---------------------------------------------------------------------------
// Reviewing a card
// ---------------------------------------------------------------------------

struct Reviewed {
    item: String,
    before: Item,
    after: Item,
    at: f64,
    quality: i64,
    device: String,
    review: Mutex<Option<i64>>,
}

impl Intent for Reviewed {
    fn describe(&self) -> String {
        format!("review of {}", self.item)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let before = self.before.clone();
        let id = self.review.lock().map_or(None, |r| *r);
        w.store()
            .write(move |c| {
                put_item_tx(c, &before)?;
                if let Some(id) = id {
                    c.execute("DELETE FROM fluent_review WHERE id = ?1", [id])?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (after, at, q, device, item) =
            (self.after.clone(), self.at, self.quality, self.device.clone(), self.item.clone());
        let id = w
            .store()
            .write(move |c| {
                put_item_tx(c, &after)?;
                let stamp = free_stamp(c, &item, at, &device)?;
                c.execute(
                    "INSERT INTO fluent_review(item, at, quality, device) VALUES(?1, ?2, ?3, ?4)",
                    params![item, stamp, q, device],
                )?;
                Ok(c.last_insert_rowid())
            })
            .map_err(|e| e.to_string())?;
        if let Ok(mut r) = self.review.lock() {
            *r = Some(id);
        }
        Ok(())
    }
}

/// One grade on one card: a review row, and the card's schedule moved.
pub fn review_card(s: &mut Session, item: &str, quality: i64, label: impl Into<String>) -> bool {
    let at = s.now();
    let device = s.store().device();
    let q = quality.clamp(0, 5);
    let (write_item, write_device) = (item.to_string(), device.clone());
    let Some(Some((before, after, id))) = s.act(Action::writing("fluent.review", label, move |c| {
        let Some(before) = item_tx(c, &write_item)? else { return Ok(None) };
        let st = sm2::step(before.state(), q);
        let after = Item {
            ease: st.ease,
            interval: st.interval,
            reps: st.reps,
            due: sm2::due_after(at, st),
            reviewed: Some(at),
            mastery: sm2::mastery_after(before.mastery, q),
            ..before.clone()
        };
        put_item_tx(c, &after)?;
        let stamp = free_stamp(c, &write_item, at, &write_device)?;
        c.execute(
            "INSERT INTO fluent_review(item, at, quality, device) VALUES(?1, ?2, ?3, ?4)",
            params![write_item, stamp, q, write_device],
        )?;
        Ok(Some((before, after, c.last_insert_rowid())))
    })) else {
        return false;
    };
    s.claim(Box::new(Reviewed {
        item: item.to_string(),
        before,
        after,
        at,
        quality: q,
        device,
        review: Mutex::new(Some(id)),
    }));
    true
}

// ---------------------------------------------------------------------------
// Grammar
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct TopicRow {
    pub id: String,
    pub title: String,
    pub category: String,
    pub level: String,
    pub summary: String,
    pub mastery: Option<i64>,
    pub introduced: Option<i64>,
    pub practiced: Option<i64>,
    pub sections: String,
    pub related: Vec<String>,
}

const TOPIC_SELECT: &str =
    "t.id, t.title, t.category, t.level, t.summary, t.mastery, t.introduced, t.practiced, t.sections, t.related";

fn topic_of(r: &rusqlite::Row) -> rusqlite::Result<TopicRow> {
    Ok(TopicRow {
        id: r.get(0)?,
        title: r.get(1)?,
        category: r.get(2)?,
        level: r.get(3)?,
        summary: r.get(4)?,
        mastery: r.get(5)?,
        introduced: r.get(6)?,
        practiced: r.get(7)?,
        sections: r.get(8)?,
        related: json_strings(&r.get::<_, String>(9)?),
    })
}

pub static TOPICS: SqlSource<TopicRow, String> = SqlSource {
    spec: &SqlSpec {
        id: "fluent topics",
        describe: "the grammar reference the tutor keeps, by category",
        select: TOPIC_SELECT,
        from: "fluent_topic t",
        base: "1",
        text: &["t.title", "t.summary"],
        index: None,
        tags: &[
            ("level", TagSql::Col("t.level")),
            ("category", TagSql::Col("t.category")),
            ("mastery", TagSql::Col("t.mastery")),
        ],
        order: &[("t.category", Dir::Asc), ("t.level", Dir::Asc), ("t.title", Dir::Asc)],
        group: None,
        key: "t.id",
        deps: &[],
    },
    tags: &[
        TagDef { name: "level", kind: TagType::Text, ops: &[Op::Eq], describe: "CEFR level of the rule", values: Values::Dynamic },
        TagDef { name: "category", kind: TagType::Text, ops: &[Op::Eq], describe: "cases, verbs, prepositions, …", values: Values::Dynamic },
        TagDef { name: "mastery", kind: TagType::Number, ops: &[Op::Eq, Op::Lt, Op::Lte, Op::Gt, Op::Gte], describe: "the 0–5 stamp", values: Values::None },
    ],
    map: topic_of,
    key: |r| r.id.clone(),
    rank: |r| vec![Val::S(r.category.clone()), Val::S(r.level.clone()), Val::S(r.title.clone())],
    suggest: suggest_topics,
};

fn suggest_topics(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    let sql = match tag {
        "level" => "SELECT DISTINCT level FROM fluent_topic ORDER BY level",
        "category" => "SELECT DISTINCT category FROM fluent_topic ORDER BY category",
        _ => return Vec::new(),
    };
    store
        .rows_sql("fluent topic tags", "the levels and categories the topics use", sql, &[], |r| {
            r.get::<_, String>(0)
        })
        .iter()
        .filter(|v| v.to_lowercase().starts_with(&typed.to_lowercase()))
        .map(|v| Suggestion::value(v.clone()))
        .collect()
}

pub fn topic(store: &Store, id: &str) -> Option<TopicRow> {
    store
        .rows_sql(
            "fluent topic",
            "one grammar topic, whole",
            "SELECT t.id, t.title, t.category, t.level, t.summary, t.mastery, t.introduced, t.practiced, t.sections, t.related
               FROM fluent_topic t WHERE t.id = ?1",
            &[Val::S(id.to_string())],
            topic_of,
        )
        .first()
        .cloned()
}

/// The category's name in the panel, in the course's language.
#[must_use]
pub fn category_word(c: &str) -> &str {
    match c {
        "cases" => "Fälle",
        "verbs" => "Verben",
        "sentence_structure" => "Satzbau",
        "prepositions" => "Präpositionen",
        "adjectives" => "Adjektive",
        "pronouns" => "Pronomen",
        "nouns" => "Nomen",
        _ => "Sonstiges",
    }
}

/// One block of a topic.
#[derive(Clone, Debug, PartialEq)]
pub enum Section {
    Text(String),
    Tip(String),
    Table { caption: String, columns: Vec<String>, rows: Vec<Vec<String>> },
    Examples(Vec<(String, String)>),
}

/// The blocks a topic's `sections` JSON holds; anything unreadable is left
/// out rather than shown as a blank.
#[must_use]
pub fn sections(json: &str) -> Vec<Section> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let strs = |v: Option<&serde_json::Value>| -> Vec<String> {
        v.and_then(|v| v.as_array())
            .map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default()
    };
    items
        .iter()
        .filter_map(|it| {
            let text = |k: &str| it.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            match it.get("kind").and_then(|k| k.as_str())? {
                "text" => Some(Section::Text(text("body"))),
                "tip" => Some(Section::Tip(text("body"))),
                "table" => Some(Section::Table {
                    caption: text("caption"),
                    columns: strs(it.get("columns")),
                    rows: it
                        .get("rows")
                        .and_then(|r| r.as_array())
                        .map(|rows| rows.iter().map(|row| strs(Some(row))).collect())
                        .unwrap_or_default(),
                }),
                "examples" => Some(Section::Examples(
                    it.get("items")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|e| {
                                    (
                                        e.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                        e.get("note").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                )),
                _ => None,
            }
        })
        .collect()
}

/// A table laid out in the mono face: every column as wide as its widest
/// cell, two spaces between, the header row over a rule of dashes.
#[must_use]
pub fn table_text(columns: &[String], rows: &[Vec<String>]) -> String {
    let n = columns.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut widths = vec![0usize; n];
    for row in std::iter::once(columns).chain(rows.iter().map(Vec::as_slice)) {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |row: &[String]| -> String {
        (0..n)
            .map(|i| {
                let cell = row.get(i).map_or("", String::as_str);
                let pad = widths[i] - cell.chars().count();
                format!("{cell}{}", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![line(columns)];
    out.push(widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("  "));
    out.extend(rows.iter().map(|r| line(r)));
    out.join("\n")
}

#[derive(Clone, Debug, PartialEq)]
pub struct TopicNote {
    pub lesson: Option<i64>,
    pub note: String,
    pub at: f64,
}

pub fn topic_notes(store: &Store, id: &str) -> Rc<Vec<TopicNote>> {
    store.rows_sql(
        "fluent topic notes",
        "the learner's own stumbles on one topic, newest first",
        "SELECT lesson, note, at FROM fluent_topic_note WHERE topic = ?1 ORDER BY at DESC, id DESC",
        &[Val::S(id.to_string())],
        |r| Ok(TopicNote { lesson: r.get(0)?, note: r.get(1)?, at: r.get(2)? }),
    )
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Mistake {
    pub id: String,
    pub category: String,
    pub frequency: i64,
    pub last: Option<f64>,
    pub wrong: String,
    pub right: String,
    pub mastery: i64,
}

pub fn top_mistakes(store: &Store, n: i64) -> Rc<Vec<Mistake>> {
    store.rows_sql(
        "fluent mistakes",
        "the error patterns seen most often",
        "SELECT m.id, m.category, m.frequency, m.last, m.wrong, m.right, COALESCE(i.mastery, 0)
           FROM fluent_mistake m LEFT JOIN fluent_item i ON i.id = m.id
          ORDER BY m.frequency DESC, m.last DESC LIMIT ?1",
        &[Val::I(n)],
        |r| {
            Ok(Mistake {
                id: r.get(0)?,
                category: r.get(1)?,
                frequency: r.get(2)?,
                last: r.get(3)?,
                wrong: r.get(4)?,
                right: r.get(5)?,
                mastery: r.get(6)?,
            })
        },
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub mastery: i64,
    pub accuracy: f64,
    pub lessons: i64,
}

pub fn skills(store: &Store) -> Rc<Vec<Skill>> {
    store.rows_sql(
        "fluent skills",
        "mastery and accuracy per skill",
        "SELECT name, mastery, accuracy, lessons FROM fluent_skill ORDER BY mastery DESC, name",
        &[],
        |r| Ok(Skill { name: r.get(0)?, mastery: r.get(1)?, accuracy: r.get(2)?, lessons: r.get(3)? }),
    )
}

/// Minutes studied on each of the last 56 days, oldest first: what the
/// activity strip is drawn from.
pub fn activity(store: &Store, now: f64) -> Vec<f64> {
    let today = sm2::day_start(now);
    let first = today - 55.0 * DAY;
    let rows = store.rows_sql(
        "fluent activity",
        "minutes per day over the last eight weeks",
        "SELECT for_date, COALESCE(minutes, 0) FROM fluent_lesson WHERE status = 'done' AND for_date >= ?1",
        &[Val::F(first)],
        |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)),
    );
    let mut days = vec![0.0; 56];
    for (day, minutes) in rows.iter() {
        let i = ((sm2::day_start(*day) - first) / DAY).round() as i64;
        if (0..56).contains(&i) {
            days[i as usize] += minutes;
        }
    }
    days
}

// ---------------------------------------------------------------------------
// Words for dates and numbers
// ---------------------------------------------------------------------------

/// `today`, `tomorrow`, `in 3 days`, `2 days ago`, or the date.
#[must_use]
pub fn relative_day(day: f64, now: f64) -> String {
    match sm2::days_between(now, day) {
        0 => "today".into(),
        1 => "tomorrow".into(),
        -1 => "yesterday".into(),
        d if d > 1 && d < 14 => format!("in {d} days"),
        d if d < 0 && d > -14 => format!("{} days ago", -d),
        _ => fmt_day(day),
    }
}

/// `sep 01`, without the hour the list style carries.
#[must_use]
pub fn fmt_day(ts: f64) -> String {
    fmt_date(ts).split(' ').take(2).collect::<Vec<_>>().join(" ")
}

/// The weekday, `Montag` to `Sonntag` — the course's language for a card
/// that says when the next one comes.
#[must_use]
pub fn weekday(ts: f64) -> &'static str {
    const DAYS: [&str; 7] = ["Donnerstag", "Freitag", "Samstag", "Sonntag", "Montag", "Dienstag", "Mittwoch"];
    let days = (ts / DAY).floor() as i64;
    DAYS[days.rem_euclid(7) as usize]
}

/// `1 Sep` — the day with its month, for a shelf.
#[must_use]
pub fn fmt_day_month(ts: f64) -> String {
    let (_, m, d) = civil_from_days((ts / DAY).floor() as i64);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {}", MONTHS[(m - 1) as usize])
}

#[must_use]
pub fn json_strings(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

#[must_use]
pub fn percent(fraction: f64) -> String {
    format!("{}%", (fraction * 100.0).round() as i64)
}

/// The German greeting for the hour.
#[must_use]
pub fn greeting(now: f64) -> &'static str {
    let hour = ((now.rem_euclid(DAY)) / 3600.0) as u32;
    match hour {
        5..=11 => "Guten Morgen",
        12..=17 => "Guten Tag",
        _ => "Guten Abend",
    }
}

/// One item's schedule, as a test reads it.
#[cfg(test)]
pub fn item_state(store: &Store, id: &str) -> Item {
    store
        .rows_sql(
            "fluent item",
            "one item's schedule",
            "SELECT id, kind, content, ease, interval, reps, due, reviewed, mastery FROM fluent_item WHERE id = ?1",
            &[Val::S(id.to_string())],
            |r| {
                Ok(Item {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    content: r.get(2)?,
                    ease: r.get(3)?,
                    interval: r.get(4)?,
                    reps: r.get(5)?,
                    due: r.get(6)?,
                    reviewed: r.get(7)?,
                    mastery: r.get(8)?,
                })
            },
        )
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("no item {id}"))
}
