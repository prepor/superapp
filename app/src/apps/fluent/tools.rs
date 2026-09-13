//! What the tutor may do by name: read what is due and what was played,
//! grade an answer, and author the next lesson whole.
//!
//! Each is the panel's own code path over ids — a grade is the write the
//! summary shows, a lesson is rows the desk finds on the shelf — so a tool
//! and a button cannot disagree, and `cmd+z` takes either back.

use kernel::history::Intent;
use kernel::session::{Action, Session};
use kernel::tool::Tool;
use rusqlite::params;
use serde_json::{json, Value};

use super::model;
use super::sm2::{day_start, DAY};

#[must_use]
pub fn all() -> Vec<Tool> {
    vec![
        Tool::new(
            "fluent.due",
            "What is due today on the learner's schedule: every item whose due day has come, \
             with the card behind it where it is a word. Call this before authoring a lesson, \
             so the review section covers what SM-2 says is due rather than a guess.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            false,
            due,
        ),
        Tool::new(
            "fluent.lesson",
            "One lesson whole: its row and every exercise in order with what the learner \
             answered, how it graded, the self-grade and any tutor grade. Read the finished \
             lesson before grading its free answers or authoring the next one.",
            json!({
                "type": "object",
                "properties": {"lesson": {"type": "integer", "description": "fluent_lesson.id; the newest finished one when omitted"}},
                "additionalProperties": false
            }),
            false,
            lesson,
        ),
        Tool::new(
            "fluent.grade",
            "The tutor's grade on a self_check answer: a quality 0–5, one sentence of feedback \
             in the learner's language, and the corrected text. It lands on the exercise for \
             the summary and the calibration line; one undo takes it back.",
            json!({
                "type": "object",
                "properties": {
                    "exercise": {"type": "integer", "description": "fluent_exercise.id"},
                    "quality": {"type": "integer", "description": "0 blank … 5 perfect"},
                    "note": {"type": "string", "description": "one line of feedback"},
                    "fix": {"type": "string", "description": "the corrected text, when the answer needed one"}
                },
                "required": ["exercise", "quality", "note"],
                "additionalProperties": false
            }),
            true,
            grade,
        ),
        Tool::new(
            "fluent.author",
            "Put a whole lesson on the shelf: the title, the day it is for, the focus tags, and \
             the exercises in order — an arc of warmup, review, new, set_piece, cooldown. \
             Closed exercises need accepted answers; self_check ones need a model answer; \
             mcq, listen_mcq and read_mcq need choices; listen_mcq needs audio; a set piece \
             puts its text in passage on every sub-question. Every exercise names the items it \
             grades into. Replaces the shelf's building placeholder. One undo removes it.",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "for_date": {"type": "string", "description": "YYYY-MM-DD; tomorrow when omitted"},
                    "focus": {"type": "array", "items": {"type": "string"}},
                    "notes": {"type": "string", "description": "the tutor's notes on the lesson this one follows"},
                    "exercises": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "section": {"type": "string", "enum": ["warmup", "review", "new", "set_piece", "cooldown"]},
                                "kind": {"type": "string", "enum": ["mcq", "cloze", "translate", "free_write", "listen_mcq", "read_mcq"]},
                                "grading": {"type": "string", "enum": ["closed", "self_check"]},
                                "prompt": {"type": "string"},
                                "passage": {"type": "string"},
                                "audio": {"type": "string"},
                                "choices": {"type": "array", "items": {"type": "string"}},
                                "accepted": {"type": "array", "items": {"type": "string"}},
                                "model": {"type": "string"},
                                "hints": {"type": "array", "items": {"type": "string"}},
                                "explanation": {"type": "string"},
                                "items": {"type": "array", "items": {"type": "string"}},
                                "difficulty": {"type": "integer"}
                            },
                            "required": ["section", "kind", "grading", "prompt", "items"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["title", "exercises"],
                "additionalProperties": false
            }),
            true,
            author,
        ),
    ]
}

fn due(s: &mut Session, _: &Value) -> Result<Value, String> {
    let today = day_start(s.now());
    let rows = s.store().rows_sql(
        "fluent due items",
        "every item due today, with its card where it has one",
        "SELECT i.id, i.kind, i.content, i.due, i.reps, i.ease, i.mastery, c.front, c.back
           FROM fluent_item i LEFT JOIN fluent_card c ON c.item = i.id
          WHERE i.due <= ?1 ORDER BY i.due, i.id",
        &[kernel::store::Val::F(today)],
        |r| {
            Ok(json!({
                "item": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "content": r.get::<_, String>(2)?,
                "due": kernel::time::fmt_date(r.get::<_, f64>(3)?),
                "reps": r.get::<_, i64>(4)?,
                "ease": r.get::<_, f64>(5)?,
                "mastery": r.get::<_, i64>(6)?,
                "front": r.get::<_, Option<String>>(7)?,
                "back": r.get::<_, Option<String>>(8)?,
            }))
        },
    );
    Ok(json!({"today": kernel::time::fmt_date(today), "due": rows.to_vec()}))
}

fn lesson(s: &mut Session, input: &Value) -> Result<Value, String> {
    let id = match input.get("lesson").and_then(Value::as_i64) {
        Some(id) => id,
        None => s
            .store()
            .rows_sql(
                "fluent newest done",
                "the newest finished lesson",
                "SELECT id FROM fluent_lesson WHERE status = 'done' ORDER BY for_date DESC, id DESC LIMIT 1",
                &[],
                |r| r.get::<_, i64>(0),
            )
            .first()
            .copied()
            .ok_or("no finished lesson yet")?,
    };
    let row = model::lesson(s.store(), id).ok_or_else(|| format!("no lesson {id}"))?;
    let exs = model::exercises(s.store(), id);
    Ok(json!({
        "id": row.id, "title": row.title, "for_date": kernel::time::fmt_date(row.for_date),
        "focus": row.focus, "status": row.status, "accuracy": row.accuracy, "minutes": row.minutes,
        "notes": row.notes,
        "exercises": exs.iter().map(|e| json!({
            "id": e.id, "seq": e.seq, "section": e.section, "kind": e.kind, "grading": e.grading,
            "prompt": e.prompt, "passage": e.passage, "audio": e.audio, "choices": e.choices,
            "accepted": e.accepted, "model": e.model, "hints": e.hints, "explanation": e.explanation,
            "items": e.items, "difficulty": e.difficulty, "answer": e.answer, "result": e.result,
            "self_grade": e.self_grade, "tutor_grade": e.tutor_grade, "tutor_note": e.tutor_note,
            "tutor_fix": e.tutor_fix, "hints_shown": e.hints_shown, "elapsed": e.elapsed,
        })).collect::<Vec<_>>(),
    }))
}

fn grade(s: &mut Session, input: &Value) -> Result<Value, String> {
    let exercise = input.get("exercise").and_then(Value::as_i64).ok_or("missing exercise")?;
    let quality = input.get("quality").and_then(Value::as_i64).ok_or("missing quality")?;
    if !(0..=5).contains(&quality) {
        return Err("quality is 0 to 5".into());
    }
    let note = input.get("note").and_then(Value::as_str).unwrap_or("");
    let fix = input.get("fix").and_then(Value::as_str).unwrap_or("");
    if !model::tutor_grade(s, exercise, quality, note, fix) {
        return Err(format!("no exercise {exercise}"));
    }
    Ok(json!({"exercise": exercise, "tutor_grade": quality}))
}

/// The rows an authored lesson is, kept whole so undo can put them back.
struct Authored {
    lesson: i64,
    title: String,
    for_date: f64,
    focus: String,
    notes: String,
    generated: f64,
    exercises: Vec<Value>,
    /// The building placeholder this lesson replaced, if there was one.
    replaced: Option<(i64, f64)>,
}

impl Authored {
    fn insert(&self, c: &rusqlite::Connection) -> rusqlite::Result<()> {
        if let Some((id, _)) = self.replaced {
            c.execute("DELETE FROM fluent_lesson WHERE id = ?1 AND status = 'building'", [id])?;
        }
        c.execute(
            "INSERT INTO fluent_lesson(id, title, for_date, focus, status, generated, notes) VALUES(?1, ?2, ?3, ?4, 'ready', ?5, ?6)",
            params![self.lesson, self.title, self.for_date, self.focus, self.generated, self.notes],
        )?;
        for (n, e) in self.exercises.iter().enumerate() {
            let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let arr = |k: &str| e.get(k).cloned().unwrap_or_else(|| json!([])).to_string();
            c.execute(
                "INSERT INTO fluent_exercise(lesson, seq, section, kind, grading, prompt, passage, audio, choices, accepted, model, hints, explanation, items, difficulty)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    self.lesson, n as i64 + 1, s("section"), s("kind"), s("grading"), s("prompt"),
                    s("passage"), s("audio"), arr("choices"), arr("accepted"), s("model"), arr("hints"),
                    s("explanation"), arr("items"), e.get("difficulty").and_then(Value::as_i64).unwrap_or(2)
                ],
            )?;
        }
        Ok(())
    }
}

impl Intent for Authored {
    fn describe(&self) -> String {
        format!("lesson “{}” on the shelf", self.title)
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        let (id, replaced, generated) = (self.lesson, self.replaced, self.generated);
        w.store()
            .write(move |c| {
                c.execute("DELETE FROM fluent_exercise WHERE lesson = ?1", [id])?;
                c.execute("DELETE FROM fluent_lesson WHERE id = ?1", [id])?;
                if let Some((b, day)) = replaced {
                    c.execute(
                        "INSERT INTO fluent_lesson(id, title, for_date, status, generated) VALUES(?1, '', ?2, 'building', ?3)",
                        params![b, day, generated],
                    )?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        let me = Authored {
            lesson: self.lesson,
            title: self.title.clone(),
            for_date: self.for_date,
            focus: self.focus.clone(),
            notes: self.notes.clone(),
            generated: self.generated,
            exercises: self.exercises.clone(),
            replaced: self.replaced,
        };
        w.store().write(move |c| me.insert(c)).map_err(|e| e.to_string())
    }
}

fn author(s: &mut Session, input: &Value) -> Result<Value, String> {
    let title = input.get("title").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if title.is_empty() {
        return Err("a lesson needs a title".into());
    }
    let exercises: Vec<Value> = input
        .get("exercises")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if exercises.is_empty() {
        return Err("a lesson needs at least one exercise".into());
    }
    for (n, e) in exercises.iter().enumerate() {
        let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
        let grading = e.get("grading").and_then(Value::as_str).unwrap_or("");
        let has = |k: &str| e.get(k).and_then(Value::as_array).is_some_and(|a| !a.is_empty());
        let text = |k: &str| e.get(k).and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty());
        let q = n + 1;
        if matches!(kind, "mcq" | "listen_mcq" | "read_mcq") && !has("choices") {
            return Err(format!("exercise {q}: a {kind} needs choices"));
        }
        if grading == "closed" && !has("accepted") {
            return Err(format!("exercise {q}: a closed exercise needs accepted answers"));
        }
        if grading == "self_check" && !text("model") {
            return Err(format!("exercise {q}: a self_check exercise needs a model answer"));
        }
        if kind == "listen_mcq" && !text("audio") {
            return Err(format!("exercise {q}: a listen_mcq needs audio"));
        }
        if !has("items") {
            return Err(format!("exercise {q}: name the items it grades into"));
        }
    }
    let now = s.now();
    let for_date = match input.get("for_date").and_then(Value::as_str) {
        Some(d) => parse_day(d).ok_or_else(|| format!("for_date {d:?} is not YYYY-MM-DD"))?,
        None => day_start(now) + DAY,
    };
    let focus = input.get("focus").cloned().unwrap_or_else(|| json!([])).to_string();
    let notes = input.get("notes").and_then(Value::as_str).unwrap_or("").to_string();
    let label = format!("author “{title}”");
    let (t, ex, f, n) = (title.clone(), exercises.clone(), focus.clone(), notes.clone());
    let authored = s.act(Action::writing("fluent.author", label, move |c| {
        let replaced = c
            .query_row(
                "SELECT id, for_date FROM fluent_lesson WHERE status = 'building' ORDER BY for_date DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
            )
            .ok();
        let next: i64 = c.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM fluent_lesson", [], |r| r.get(0))?;
        let a = Authored {
            lesson: next,
            title: t,
            for_date,
            focus: f,
            notes: n,
            generated: now,
            exercises: ex,
            replaced,
        };
        a.insert(c)?;
        Ok(a)
    }));
    let Some(a) = authored else { return Err("the store refused the lesson".into()) };
    let id = a.lesson;
    let count = a.exercises.len();
    s.claim(Box::new(a));
    Ok(json!({"lesson": id, "title": title, "exercises": count, "for_date": kernel::time::fmt_date(for_date)}))
}

/// `YYYY-MM-DD` as a day.
fn parse_day(s: &str) -> Option<f64> {
    let mut it = s.trim().split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(kernel::time::ts(y, m, d, 0, 0))
}
