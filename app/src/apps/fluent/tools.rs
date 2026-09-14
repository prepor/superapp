//! What the tutor may do by name: read what is due and what was played,
//! grade an answer, and author the next lesson whole.
//!
//! Each is the panel's own code path over ids — a grade is the write the
//! summary shows, a lesson is rows the desk finds on the shelf — so a tool
//! and a button cannot disagree, and `cmd+z` takes either back.

use kernel::history::Intent;
use kernel::session::{Action, Session};
use kernel::tool::Tool;
use rusqlite::{params, OptionalExtension};
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
            "Put a whole lesson on the shelf, with everything the day leaves behind: the \
             title, the day it is for, the focus tags, and the exercises in order — an arc \
             of warmup, review, new, set_piece, cooldown. \
             Closed exercises need accepted answers; self_check ones need a model answer; \
             mcq, listen_mcq and read_mcq need choices; listen_mcq needs audio; a set piece \
             puts its text in passage on every sub-question. Every exercise names the items it \
             grades into. Beside the lesson: cards for the words it introduces, topics for the \
             rules it touches (an existing topic is extended, never rewritten), topic_notes for \
             what the learner got wrong, and mistakes for the patterns behind them. \
             Replaces the shelf's building placeholder. One undo removes the lesson and \
             everything filed with it, and puts back what it replaced.",
            author_schema(),
            true,
            author,
        ),
    ]
}

/// An array of objects of one shape, with the keys it must have.
fn array_of(item: Value, required: &[&str], describe: &str) -> Value {
    json!({
        "type": "array",
        "description": describe,
        "items": {
            "type": "object",
            "properties": item,
            "required": required,
            "additionalProperties": false
        }
    })
}

/// `fluent.author`'s input, built in pieces: one whole `json!` of it is
/// more nesting than the macro will expand.
fn author_schema() -> Value {
    let strings = json!({"type": "array", "items": {"type": "string"}});
    let exercise = json!({
        "section": {"type": "string", "enum": ["warmup", "review", "new", "set_piece", "cooldown"]},
        "kind": {"type": "string", "enum": ["mcq", "cloze", "translate", "free_write", "listen_mcq", "read_mcq"]},
        "grading": {"type": "string", "enum": ["closed", "self_check"]},
        "prompt": {"type": "string"},
        "passage": {"type": "string", "description": "the set piece's text, on every one of its sub-questions"},
        "audio": {"type": "string", "description": "what a listen_mcq speaks"},
        "choices": strings,
        "accepted": strings,
        "model": {"type": "string", "description": "the model answer a self_check is graded against"},
        "hints": {"type": "array", "items": {"type": "string"}, "description": "one at a time, from vague to precise"},
        "explanation": {"type": "string"},
        "items": {"type": "array", "items": {"type": "string"}, "description": "the item ids this exercise grades into"},
        "difficulty": {"type": "integer", "description": "1 to 5"}
    });
    let card = json!({
        "item": {"type": "string", "description": "the item id, vocab_<word>"},
        "front": {"type": "string", "description": "the word, with its article"},
        "back": {"type": "string", "description": "the meaning in the learner's own language"},
        "example": {"type": "string"},
        "audio": {"type": "string", "description": "what to speak"},
        "notes": {"type": "string"}
    });
    let example = json!({
        "type": "array",
        "description": "an examples block",
        "items": {
            "type": "object",
            "properties": {"text": {"type": "string"}, "note": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false
        }
    });
    let section = json!({
        "kind": {"type": "string", "enum": ["text", "table", "examples", "tip"]},
        "body": {"type": "string", "description": "a text or a tip"},
        "caption": {"type": "string", "description": "a table's caption"},
        "columns": strings,
        "rows": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}},
        "items": example
    });
    let topic = json!({
        "id": {"type": "string", "description": "a kebab-case slug"},
        "title": {"type": "string"},
        "category": {"type": "string", "enum": ["cases", "verbs", "sentence_structure", "prepositions", "adjectives", "pronouns", "nouns", "other"]},
        "level": {"type": "string", "description": "CEFR: A1 to C2"},
        "summary": {"type": "string"},
        "mastery": {"type": "integer", "description": "0 to 5"},
        "items": strings,
        "sections": array_of(section, &["kind"], "the rule itself: a text, a table wherever it is table-shaped, examples, a tip"),
        "related": strings
    });
    let mistake = json!({
        "id": {"type": "string", "description": "a stable slug: article_gender_fem, dativ_after_mit"},
        "category": {"type": "string"},
        "subcategory": {"type": "string"},
        "wrong": {"type": "string"},
        "right": {"type": "string"},
        "context": {"type": "string"},
        "notes": {"type": "string"}
    });
    let note = json!({
        "topic": {"type": "string", "description": "fluent_topic.id"},
        "note": {"type": "string"}
    });
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "for_date": {"type": "string", "description": "YYYY-MM-DD; tomorrow when omitted"},
            "focus": strings,
            "notes": {"type": "string", "description": "the tutor's notes on the lesson this one follows"},
            "finished": {"type": "integer", "description": "fluent_lesson.id of the lesson just played, which the notes and the practiced stamps belong to; the newest done one when omitted"},
            "exercises": array_of(exercise, &["section", "kind", "grading", "prompt", "items"], "the lesson, in order"),
            "cards": array_of(card, &["item", "front", "back"], "a flashcard for every new word the lesson introduces, under the same item id the exercise names"),
            "topics": array_of(topic, &["id", "title", "category", "summary"], "the grammar reference: one per rule the lesson touches. An id that exists is extended — new sections appended, items and related unioned — not rewritten."),
            "topic_notes": array_of(note, &["topic", "note"], "one short line per notable error, quoting the mistake and its correction"),
            "mistakes": array_of(mistake, &["id", "category", "wrong", "right"], "the error patterns behind those errors; an id seen before has its count raised")
        },
        "required": ["title", "exercises"],
        "additionalProperties": false
    })
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
        "id": row.id, "uid": row.uid, "title": row.title, "for_date": kernel::time::fmt_date(row.for_date),
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
#[derive(Clone)]
struct Authored {
    lesson: i64,
    /// What names this lesson on every device. Made here rather than left
    /// to the column's default, so that undoing the lesson and redoing it
    /// puts the same one back rather than a second one under a new name.
    uid: String,
    title: String,
    for_date: f64,
    focus: String,
    /// The tutor's notes: on the lesson just played, where there is one.
    notes: String,
    /// That lesson's id and the notes it had, where the notes go onto it.
    notes_before: Option<(i64, String)>,
    generated: f64,
    exercises: Vec<Value>,
    /// The building placeholder this lesson replaced, if there was one:
    /// its local id, its uid and the day it was for.
    replaced: Option<(i64, String, f64)>,
    /// What the same call filed beside the lesson: the cards, the topics,
    /// the notes and the mistakes, and the rows they stood on.
    kept: Housekeeping,
}

/// One row as `SELECT *` found it, under the key that names it: what undo
/// puts back where an upsert wrote over something, and `None` where there
/// was nothing and the reversal is a delete.
#[derive(Clone, Debug)]
struct Kept {
    table: &'static str,
    key_col: &'static str,
    key: String,
    row: Option<Vec<(String, rusqlite::types::Value)>>,
}

impl Kept {
    /// The row as it stands, before anything writes over it.
    fn of(
        c: &rusqlite::Connection, table: &'static str, key_col: &'static str, key: &str,
    ) -> rusqlite::Result<Kept> {
        let mut stmt = c.prepare(&format!("SELECT * FROM {table} WHERE {key_col} = ?1"))?;
        let names: Vec<String> = stmt.column_names().iter().map(|n| (*n).to_string()).collect();
        let mut rows = stmt.query([key])?;
        let row = match rows.next()? {
            None => None,
            Some(r) => {
                let mut cells = Vec::with_capacity(names.len());
                for (i, name) in names.iter().enumerate() {
                    cells.push((name.clone(), r.get::<_, rusqlite::types::Value>(i)?));
                }
                Some(cells)
            }
        };
        Ok(Kept { table, key_col, key: key.to_string(), row })
    }

    /// Whether there was a row here at all, which is what an upsert's
    /// *insert or extend* turns on.
    fn was(&self) -> bool {
        self.row.is_some()
    }

    /// The row put back exactly as it stood: gone where it was not there.
    fn restore(&self, c: &rusqlite::Connection) -> rusqlite::Result<()> {
        let (table, key_col) = (self.table, self.key_col);
        c.execute(&format!("DELETE FROM {table} WHERE {key_col} = ?1"), [&self.key])?;
        let Some(row) = &self.row else { return Ok(()) };
        let cols = row.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ");
        let marks = (1..=row.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
        let vals: Vec<&dyn rusqlite::ToSql> =
            row.iter().map(|(_, v)| v as &dyn rusqlite::ToSql).collect();
        c.execute(&format!("INSERT INTO {table}({cols}) VALUES({marks})"), vals.as_slice())?;
        Ok(())
    }
}

/// What a lesson leaves behind: the words it introduces as cards, the rules
/// it touches as topics, the learner's own stumbles as notes, and the
/// patterns behind them as mistakes.
///
/// All of it lands inside the one `fluent.author` action, so one `cmd+z`
/// takes the lesson and everything filed with it — and puts back, cell for
/// cell, whatever an upsert wrote over.
#[derive(Clone, Default)]
struct Housekeeping {
    now: f64,
    /// The lesson these stamps belong to: the local id a topic's
    /// `practiced` takes, and the uid a note is named by.
    finished: Option<(i64, String)>,
    cards: Vec<Value>,
    /// The items the exercises and the topics name that no card and no
    /// mistake in the call makes: `(id, content)`, put on the schedule as
    /// grammar where the schedule has no such item.
    rules: Vec<(String, String)>,
    topics: Vec<Value>,
    notes: Vec<Value>,
    mistakes: Vec<Value>,
    /// A uid per note, made here rather than left to the column's default,
    /// so that undo and redo file the same row rather than a second one.
    note_uids: Vec<String>,
    /// Every row the writes are about to touch, as it stood.
    before: Vec<Kept>,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

impl Housekeeping {
    /// Reads what is about to be written over, and names the notes. Runs
    /// once, in the transaction, before [`Housekeeping::apply`] — a
    /// reapply files what this captured rather than looking again.
    fn capture(&mut self, c: &rusqlite::Connection) -> rusqlite::Result<()> {
        for card in &self.cards {
            let item = text(card, "item");
            self.before.push(Kept::of(c, "fluent_item", "id", &item)?);
            self.before.push(Kept::of(c, "fluent_card", "item", &item)?);
        }
        for (id, _) in &self.rules {
            self.before.push(Kept::of(c, "fluent_item", "id", id)?);
        }
        for topic in &self.topics {
            self.before.push(Kept::of(c, "fluent_topic", "id", &text(topic, "id"))?);
        }
        for _ in 0..self.notes.len() {
            let uid: String = c.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
            self.before.push(Kept {
                table: "fluent_topic_note",
                key_col: "uid",
                key: uid.clone(),
                row: None,
            });
            self.note_uids.push(uid);
        }
        for mistake in &self.mistakes {
            let id = text(mistake, "id");
            self.before.push(Kept::of(c, "fluent_item", "id", &id)?);
            self.before.push(Kept::of(c, "fluent_mistake", "id", &id)?);
        }
        Ok(())
    }

    /// What each `before` says of the row it was read from: whether there
    /// was one, and what it held.
    fn stood(&self, table: &str, key: &str) -> Option<&Kept> {
        self.before.iter().find(|k| k.table == table && k.key == key)
    }

    fn apply(&self, c: &rusqlite::Connection) -> rusqlite::Result<()> {
        let today = day_start(self.now);
        for card in &self.cards {
            let item = text(card, "item");
            let front = text(card, "front");
            // A word is an item on the schedule and a card in the deck. The
            // item comes first and only where there is none: its columns
            // are the replay of its grades, and a word met before keeps the
            // history it has.
            if !self.stood("fluent_item", &item).is_some_and(Kept::was) {
                c.execute(
                    "INSERT INTO fluent_item(id, kind, content, created, due, base) VALUES(?1, 'vocab', ?2, ?3, ?4, ?5)",
                    params![item, front, self.now, today, model::fresh_base(today)],
                )?;
            }
            c.execute(
                "INSERT INTO fluent_card(item, front, back, example, audio, notes) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(item) DO UPDATE SET front = excluded.front, back = excluded.back,
                     example = excluded.example, audio = excluded.audio, notes = excluded.notes",
                params![
                    item,
                    front,
                    text(card, "back"),
                    text(card, "example"),
                    text(card, "audio"),
                    text(card, "notes")
                ],
            )?;
        }
        for (id, content) in &self.rules {
            if self.stood("fluent_item", id).is_some_and(Kept::was) {
                continue;
            }
            let kind = if id.starts_with("vocab_") { "vocab" } else { "grammar" };
            c.execute(
                "INSERT INTO fluent_item(id, kind, content, created, due, base) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, kind, content, self.now, today, model::fresh_base(today)],
            )?;
        }
        for topic in &self.topics {
            self.put_topic(c, topic)?;
        }
        for (note, uid) in self.notes.iter().zip(&self.note_uids) {
            c.execute(
                "INSERT INTO fluent_topic_note(uid, topic, note, at, lesson_uid) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    uid,
                    text(note, "topic"),
                    text(note, "note"),
                    self.now,
                    self.finished.as_ref().map_or(String::new(), |(_, uid)| uid.clone())
                ],
            )?;
        }
        for mistake in &self.mistakes {
            let id = text(mistake, "id");
            let (wrong, right) = (text(mistake, "wrong"), text(mistake, "right"));
            if !self.stood("fluent_item", &id).is_some_and(Kept::was) {
                c.execute(
                    "INSERT INTO fluent_item(id, kind, content, created, due, base) VALUES(?1, 'error', ?2, ?3, ?4, ?5)",
                    params![id, format!("{wrong} → {right}"), self.now, today, model::fresh_base(today)],
                )?;
            }
            // Seen before is one row with its count raised, never a second.
            c.execute(
                "INSERT INTO fluent_mistake(id, category, subcategory, frequency, last, notes, wrong, right, context)
                 VALUES(?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET category = excluded.category, subcategory = excluded.subcategory,
                     frequency = fluent_mistake.frequency + 1, last = excluded.last, notes = excluded.notes,
                     wrong = excluded.wrong, right = excluded.right, context = excluded.context",
                params![
                    id,
                    text(mistake, "category"),
                    text(mistake, "subcategory"),
                    self.now,
                    text(mistake, "notes"),
                    wrong,
                    right,
                    text(mistake, "context")
                ],
            )?;
        }
        Ok(())
    }

    /// One topic, written or extended. An existing topic keeps everything
    /// it has: its sections are appended to, its items and its related ids
    /// are unioned, and only the stamps at its head are refreshed — the
    /// reference is the tutor's memory, not this lesson's output.
    fn put_topic(&self, c: &rusqlite::Connection, topic: &Value) -> rusqlite::Result<()> {
        let id = text(topic, "id");
        let stood = self.stood("fluent_topic", &id).and_then(|k| k.row.as_ref());
        let cell = |name: &str| {
            stood.and_then(|row| row.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()))
        };
        let json_of = |name: &str| match cell(name) {
            Some(rusqlite::types::Value::Text(t)) => {
                serde_json::from_str::<Vec<Value>>(&t).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        // A section already there, whole — the same rows, the same
        // examples — is not written twice; anything that differs in any
        // part is an extension and goes on the end.
        let mut sections = json_of("sections");
        for section in topic.get("sections").and_then(Value::as_array).into_iter().flatten() {
            if !sections.iter().any(|had| had == section) {
                sections.push(section.clone());
            }
        }
        let union = |name: &str| {
            let mut all: Vec<String> = json_of(name)
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            for v in topic.get(name).and_then(Value::as_array).into_iter().flatten() {
                if let Some(s) = v.as_str() {
                    if !all.iter().any(|had| had == s) {
                        all.push(s.to_string());
                    }
                }
            }
            serde_json::to_string(&all).unwrap_or_else(|_| "[]".into())
        };
        let level = match topic.get("level").and_then(Value::as_str) {
            Some(l) if !l.trim().is_empty() => l.trim().to_string(),
            _ => match cell("level") {
                Some(rusqlite::types::Value::Text(t)) => t,
                _ => "A1".to_string(),
            },
        };
        let mastery = match topic.get("mastery").and_then(Value::as_i64) {
            Some(m) => Some(m.clamp(0, 5)),
            None => match cell("mastery") {
                Some(rusqlite::types::Value::Integer(m)) => Some(m),
                _ => None,
            },
        };
        // The lessons it names are uids, which every device reads alike.
        let lesson = self.finished.as_ref().map(|(_, uid)| uid.clone());
        let had = |name: &str| match cell(name) {
            Some(rusqlite::types::Value::Text(t)) if !t.is_empty() => Some(t),
            _ => None,
        };
        let introduced = had("introduced").or_else(|| lesson.clone()).unwrap_or_default();
        let practiced = lesson.or_else(|| had("practiced")).unwrap_or_default();
        c.execute(
            "INSERT INTO fluent_topic(id, title, category, level, summary, mastery, items, introduced, practiced, sections, related, updated)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, category = excluded.category,
                 level = excluded.level, summary = excluded.summary, mastery = excluded.mastery,
                 items = excluded.items, introduced = excluded.introduced,
                 practiced = excluded.practiced, sections = excluded.sections,
                 related = excluded.related, updated = excluded.updated",
            params![
                id,
                text(topic, "title"),
                text(topic, "category"),
                level,
                text(topic, "summary"),
                mastery,
                union("items"),
                introduced,
                practiced,
                serde_json::to_string(&sections).unwrap_or_else(|_| "[]".into()),
                union("related"),
                self.now
            ],
        )?;
        Ok(())
    }

    /// Every row this filing touched, back where it stood.
    fn restore(&self, c: &rusqlite::Connection) -> rusqlite::Result<()> {
        for kept in self.before.iter().rev() {
            kept.restore(c)?;
        }
        Ok(())
    }
}

impl Authored {
    fn insert(&self, c: &rusqlite::Connection) -> rusqlite::Result<()> {
        if let Some((id, _, _)) = &self.replaced {
            c.execute("DELETE FROM fluent_lesson WHERE id = ?1 AND status = 'building'", [id])?;
        }
        let own = if self.notes_before.is_some() { "" } else { self.notes.as_str() };
        c.execute(
            "INSERT INTO fluent_lesson(id, uid, title, for_date, focus, status, generated, notes) VALUES(?1, ?2, ?3, ?4, ?5, 'ready', ?6, ?7)",
            params![self.lesson, self.uid, self.title, self.for_date, self.focus, self.generated, own],
        )?;
        if let Some((played, _)) = &self.notes_before {
            c.execute("UPDATE fluent_lesson SET notes = ?2 WHERE id = ?1", params![played, self.notes])?;
        }
        for (n, e) in self.exercises.iter().enumerate() {
            let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let arr = |k: &str| e.get(k).cloned().unwrap_or_else(|| json!([])).to_string();
            // The exercise names its lesson by uid; the local id beside it
            // is the schema's own trigger's business.
            c.execute(
                "INSERT INTO fluent_exercise(lesson_uid, seq, section, kind, grading, prompt, passage, audio, choices, accepted, model, hints, explanation, items, difficulty)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    self.uid, n as i64 + 1, s("section"), s("kind"), s("grading"), s("prompt"),
                    s("passage"), s("audio"), arr("choices"), arr("accepted"), s("model"), arr("hints"),
                    s("explanation"), arr("items"), e.get("difficulty").and_then(Value::as_i64).unwrap_or(2)
                ],
            )?;
        }
        self.kept.apply(c)
    }
}

impl Intent for Authored {
    fn describe(&self) -> String {
        format!("lesson “{}” on the shelf", self.title)
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        let (id, uid, generated) = (self.lesson, self.uid.clone(), self.generated);
        let replaced = self.replaced.clone();
        let kept = self.kept.clone();
        let notes_before = self.notes_before.clone();
        w.store()
            .write(move |c| {
                kept.restore(c)?;
                if let Some((played, notes)) = notes_before {
                    c.execute("UPDATE fluent_lesson SET notes = ?2 WHERE id = ?1", params![played, notes])?;
                }
                c.execute("DELETE FROM fluent_exercise WHERE lesson_uid = ?1", [uid])?;
                c.execute("DELETE FROM fluent_lesson WHERE id = ?1", [id])?;
                if let Some((b, uid, day)) = replaced {
                    c.execute(
                        "INSERT INTO fluent_lesson(id, uid, title, for_date, status, generated) VALUES(?1, ?2, '', ?3, 'building', ?4)",
                        params![b, uid, day, generated],
                    )?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        let me = self.clone();
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
        if e.get("choices").and_then(Value::as_array).is_some_and(|a| a.len() > model::MAX_CHOICES) {
            return Err(format!(
                "exercise {q}: at most {} choices — that is what the player shows",
                model::MAX_CHOICES
            ));
        }
    }
    let now = s.now();
    let for_date = match input.get("for_date").and_then(Value::as_str) {
        Some(d) => parse_day(d).ok_or_else(|| format!("for_date {d:?} is not YYYY-MM-DD"))?,
        None => day_start(now) + DAY,
    };
    let focus = input.get("focus").cloned().unwrap_or_else(|| json!([])).to_string();
    let notes = input.get("notes").and_then(Value::as_str).unwrap_or("").to_string();
    let list = |key: &str| -> Vec<Value> {
        input.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
    };
    let (cards, topics, note_lines, mistakes) =
        (list("cards"), list("topics"), list("topic_notes"), list("mistakes"));
    // Every item an exercise grades into or a topic links to that nothing
    // else in this call makes — no card, no mistake — is a rule to put on
    // the schedule: made here, named after the topic that lists it or after
    // its own slug, so the first answer into it files a grade rather than
    // falling through to nothing.
    let made: Vec<String> =
        cards.iter().map(|c| text(c, "item")).chain(mistakes.iter().map(|m| text(m, "id"))).collect();
    let mut rules: Vec<(String, String)> = Vec::new();
    let named = exercises
        .iter()
        .chain(topics.iter())
        .flat_map(|v| v.get("items").and_then(Value::as_array).into_iter().flatten())
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty());
    for id in named {
        if made.iter().any(|m| m == id) || rules.iter().any(|(r, _)| r == id) {
            continue;
        }
        let lists = |t: &&Value| {
            t.get("items")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id)))
        };
        let content = topics
            .iter()
            .find(lists)
            .map(|t| text(t, "title"))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| id.replace('_', " "));
        rules.push((id.to_string(), content));
    }
    for card in &cards {
        if text(card, "item").is_empty() || text(card, "front").is_empty() {
            return Err("every card needs an item id and a front".into());
        }
    }
    for topic in &topics {
        if text(topic, "id").is_empty() {
            return Err("every topic needs an id".into());
        }
    }
    for note in &note_lines {
        if text(note, "topic").is_empty() || text(note, "note").is_empty() {
            return Err("every topic note names its topic and says one line".into());
        }
    }
    for mistake in &mistakes {
        if text(mistake, "id").is_empty() {
            return Err("every mistake needs an id".into());
        }
    }
    let finished = input.get("finished").and_then(Value::as_i64);
    let label = format!("author “{title}”");
    let (t, ex, f, n) = (title.clone(), exercises.clone(), focus.clone(), notes.clone());
    let authored = s.act(Action::writing("fluent.author", label, move |c| {
        let replaced = c
            .query_row(
                "SELECT id, uid, for_date FROM fluent_lesson WHERE status = 'building' ORDER BY for_date DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?)),
            )
            .ok();
        // Which lesson the notes and the practiced stamps belong to: the
        // one the call names, or the newest the learner has played.
        let played = match finished {
            Some(id) => c
                .query_row("SELECT id, uid FROM fluent_lesson WHERE id = ?1", [id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
                .optional()?,
            None => c
                .query_row(
                    "SELECT id, uid FROM fluent_lesson WHERE status = 'done' ORDER BY for_date DESC, id DESC LIMIT 1",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?,
        };
        // The notes are on the lesson they are about — the one just played
        // — and what stood there is kept for undo. With no lesson played
        // yet they stay on the new one, which is the only lesson there is.
        let notes_before = match &played {
            Some((id, _)) if !n.is_empty() => Some((
                *id,
                c.query_row("SELECT notes FROM fluent_lesson WHERE id = ?1", [id], |r| r.get::<_, String>(0))?,
            )),
            _ => None,
        };
        let mut kept = Housekeeping {
            now,
            finished: played,
            cards: cards.clone(),
            rules: rules.clone(),
            topics: topics.clone(),
            notes: note_lines.clone(),
            mistakes: mistakes.clone(),
            ..Housekeeping::default()
        };
        kept.capture(c)?;
        let next: i64 = c.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM fluent_lesson", [], |r| r.get(0))?;
        let uid: String = c.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
        let a = Authored {
            lesson: next,
            uid,
            title: t,
            for_date,
            focus: f,
            notes: n,
            notes_before,
            generated: now,
            exercises: ex,
            replaced,
            kept,
        };
        a.insert(c)?;
        Ok(a)
    }));
    let Some(a) = authored else { return Err("the store refused the lesson".into()) };
    let (id, uid) = (a.lesson, a.uid.clone());
    let count = a.exercises.len();
    let filed = json!({
        "cards": a.kept.cards.len(),
        "topics": a.kept.topics.len(),
        "topic_notes": a.kept.notes.len(),
        "mistakes": a.kept.mistakes.len(),
    });
    let played = a.kept.finished.as_ref().map(|(id, _)| *id);
    s.claim(Box::new(a));
    Ok(json!({
        "lesson": id, "uid": uid, "title": title, "exercises": count,
        "for_date": kernel::time::fmt_date(for_date), "finished": played, "filed": filed
    }))
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
