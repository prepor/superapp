//! The course as it was kept before this app: a folder of JSON notebooks,
//! read into the rows above.
//!
//! The original is a pipeline of files — a deck, a schedule, a grammar
//! reference, a log of sittings, a session the compiler authored for
//! tomorrow — written over a year by several hands, so the same thing is
//! spelled two ways in two files: an item is `item_id` here and `id`
//! there, its type `item_type` or `type`. Reading is therefore forgiving
//! of the spelling and strict about the shape: whatever of the files is
//! there is read, a missing one is simply nothing, and what comes out is
//! one [`Course`] value with no rows in it yet.
//!
//! Writing is one undoable action. Every key is its own — an item's slug,
//! a card's item, a topic's id, a lesson's uid, a grade's instant — so a
//! second import over the same folder adds nothing, and undo removes
//! exactly the rows this import put down and puts back the learner it
//! replaced.

use std::collections::{HashMap, HashSet};

use kernel::caps::{real_path, Disk};
use kernel::effect::{Ctx, Effect};
use kernel::history::Intent;
use kernel::session::{Action, Session};
use rusqlite::{params, Connection};
use serde_json::Value;

use super::model;
use super::sm2;

/// The largest notebook the reader will take. The original's schedule is
/// the big one, and it is under 200 KiB after a year.
pub const MAX_FILE: usize = 8 << 20;

/// The hour of the day a grade from a `review_history` entry is filed at.
/// The original keeps the day alone; noon is the middle of it, and putting
/// every replayed grade there keeps the order of two days apart whatever
/// zone the dates were written in.
const NOON: f64 = 12.0 * 3600.0;

// -- what a folder says ---------------------------------------------------

/// The learner's own row.
#[derive(Debug, Clone, PartialEq)]
pub struct Learner {
    pub name: String,
    pub native: String,
    pub target: String,
    pub level: String,
    pub goal: String,
    pub daily_minutes: i64,
    pub streak: i64,
    pub last_active: Option<f64>,
    pub started: f64,
}

/// One skill's standing, from the two files that each keep half of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub mastery: i64,
    pub accuracy: f64,
    pub lessons: i64,
    pub practiced: Option<f64>,
}

/// Something on the schedule, with the state the original cached for it.
/// The state is kept because an item whose history the file never wrote
/// has nowhere else to come from; where there are grades, replaying them
/// overwrites it.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub created: f64,
    pub ease: f64,
    pub interval: i64,
    pub reps: i64,
    pub due: f64,
    pub reviewed: Option<f64>,
    pub mastery: i64,
}

/// One grade, wherever it was found: an item's own history or a device's
/// append-only log.
#[derive(Debug, Clone, PartialEq)]
pub struct Review {
    pub item: String,
    pub at: f64,
    pub device: String,
    pub quality: i64,
}

/// The flashcard behind a vocabulary item.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub item: String,
    pub front: String,
    pub back: String,
    pub example: String,
    pub audio: String,
    pub notes: String,
}

/// One rule of the grammar reference. `introduced` and `practiced` are
/// lesson uids here; the write resolves each to the local id of the lesson
/// that wears it.
#[derive(Debug, Clone, PartialEq)]
pub struct Topic {
    pub id: String,
    pub title: String,
    pub category: String,
    pub level: String,
    pub summary: String,
    pub mastery: Option<i64>,
    pub items: String,
    pub sections: String,
    pub related: String,
    pub introduced: Option<String>,
    pub practiced: Option<String>,
    pub updated: f64,
}

/// A stumble the tutor wrote up under a topic.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub uid: String,
    pub topic: String,
    pub note: String,
    pub lesson_uid: String,
}

/// An error pattern. Its mastery is not carried: the item of the same name
/// has it, and the schedule is what says how well a pattern is known.
#[derive(Debug, Clone, PartialEq)]
pub struct Mistake {
    pub id: String,
    pub category: String,
    pub subcategory: String,
    pub frequency: i64,
    pub last: Option<f64>,
    pub notes: String,
    pub wrong: String,
    pub right: String,
    pub context: String,
}

/// A sitting: one played and logged, or the one the compiler authored for
/// the next day.
#[derive(Debug, Clone, PartialEq)]
pub struct Lesson {
    pub uid: String,
    pub title: String,
    pub for_date: f64,
    pub focus: String,
    pub status: String,
    pub generated: f64,
    pub accuracy: Option<f64>,
    pub minutes: Option<f64>,
    pub notes: String,
}

/// One exercise of the authored lesson.
#[derive(Debug, Clone, PartialEq)]
pub struct Exercise {
    pub lesson_uid: String,
    pub seq: i64,
    pub section: String,
    pub kind: String,
    pub grading: String,
    pub prompt: String,
    pub passage: String,
    pub audio: String,
    pub choices: String,
    pub accepted: String,
    pub model: String,
    pub hints: String,
    pub explanation: String,
    pub items: String,
    pub difficulty: i64,
}

/// Everything one folder holds, as rows waiting to be written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Course {
    /// The folder it was read from, as the panel spells it.
    pub root: String,
    /// When it was written: the day a card the deck alone knew is due.
    pub imported_at: f64,
    pub learner: Option<Learner>,
    pub skills: Vec<Skill>,
    pub items: Vec<Item>,
    pub cards: Vec<Card>,
    pub reviews: Vec<Review>,
    pub topics: Vec<Topic>,
    pub notes: Vec<Note>,
    pub mistakes: Vec<Mistake>,
    pub lessons: Vec<Lesson>,
    pub exercises: Vec<Exercise>,
}

// -- reading --------------------------------------------------------------

/// The notebooks under `<root>/data`, read into a [`Course`].
///
/// Every file is optional: a folder with only a deck in it imports the
/// deck. A file that is there but is not JSON is an error, because that is
/// a folder saying something went wrong rather than a folder without it.
///
/// # Errors
///
/// If the path is empty, if a notebook is unreadable JSON, or if none of
/// the notebooks is there at all.
pub fn read(disk: &mut dyn Disk, root: &str) -> Result<Course, String> {
    let root = root.trim().trim_end_matches('/');
    if root.is_empty() {
        return Err("enter the path to the course folder".into());
    }
    let dir = format!("{root}/data");
    let mut course = Course { root: root.to_string(), ..Course::default() };
    let mut found = 0;
    let mut file = |disk: &mut dyn Disk, name: &str| -> Result<Option<Value>, String> {
        match slurp(disk, &dir, name)? {
            Some(v) => {
                found += 1;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    };

    let profile = file(disk, "learner-profile.json")?;
    let progress = file(disk, "progress-db.json")?;
    let mastery = file(disk, "mastery-db.json")?;
    let schedule = file(disk, "spaced-repetition.json")?;
    let deck = file(disk, "vocab-deck.json")?;
    let grammar = file(disk, "grammar-kb.json")?;
    let mistakes = file(disk, "mistakes-db.json")?;
    let log = file(disk, "session-log.json")?;
    let next = file(disk, "next-session.json")?;
    if found == 0 {
        return Err(format!("no notebooks under {dir}"));
    }

    if let Some(v) = &profile {
        course.learner = Some(learner_of(v));
    }
    course.skills = skills_of(mastery.as_ref(), progress.as_ref());
    if let Some(v) = &schedule {
        items_of(v, &mut course);
    }
    if let Some(v) = &deck {
        cards_of(v, &mut course);
    }
    if let Some(v) = &mistakes {
        mistakes_of(v, &mut course);
    }
    // The lessons come before the grammar, because a topic names the
    // sitting it was introduced in and the note under it is dated by that
    // sitting's day.
    if let Some(v) = &log {
        lessons_of(v, &mut course);
    }
    if let Some(v) = &next {
        next_of(v, &mut course);
    }
    if let Some(v) = &grammar {
        topics_of(v, &mut course);
    }
    logs_of(disk, &dir, &mut course);
    Ok(course)
}

/// One file's JSON, or `None` where there is no such file.
fn slurp(disk: &mut dyn Disk, dir: &str, name: &str) -> Result<Option<Value>, String> {
    let Ok(bytes) = disk.read_file(&real_path(&format!("{dir}/{name}")), MAX_FILE) else {
        return Ok(None);
    };
    let v = serde_json::from_slice(&bytes).map_err(|e| format!("{name}: {e}"))?;
    Ok(Some(v))
}

fn learner_of(v: &Value) -> Learner {
    let l = v.get("learner").unwrap_or(&Value::Null);
    Learner {
        name: text(l, "name"),
        native: text(l, "native_language"),
        target: text(l, "target_language"),
        level: non_empty(&text(l, "current_level"), "A1"),
        goal: non_empty(&text(l, "target_level"), "B1"),
        daily_minutes: num(l, "daily_goal_minutes").unwrap_or(30.0) as i64,
        streak: num(v, "current_streak_days").unwrap_or(0.0) as i64,
        last_active: day_of(v, "last_updated"),
        started: day_of(v, "profile_created").unwrap_or(0.0),
    }
}

/// The skills of both files, by name: the mastery stamp is the mastery
/// file's, the accuracy and the count of sittings the progress file's.
fn skills_of(mastery: Option<&Value>, progress: Option<&Value>) -> Vec<Skill> {
    let by_name = |v: Option<&Value>, key: &str| -> Vec<(String, Value)> {
        v.and_then(|v| v.get(key))
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    };
    let stamped = by_name(mastery, "skills");
    let counted = by_name(progress, "skill_progress");
    let mut names: Vec<String> = stamped.iter().chain(&counted).map(|(k, _)| k.clone()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| {
            let of = |rows: &[(String, Value)]| rows.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone());
            let (m, p) = (of(&stamped), of(&counted));
            let read = |v: &Option<Value>, k: &str| v.as_ref().and_then(|v| num(v, k));
            Skill {
                mastery: read(&m, "mastery_level").unwrap_or(0.0) as i64,
                accuracy: read(&p, "accuracy").or_else(|| read(&m, "avg_accuracy")).unwrap_or(0.0),
                lessons: read(&p, "sessions").or_else(|| read(&m, "practice_count")).unwrap_or(0.0) as i64,
                practiced: p
                    .as_ref()
                    .and_then(|v| day_of(v, "last_practiced"))
                    .or_else(|| m.as_ref().and_then(|v| day_of(v, "last_practiced"))),
                name,
            }
        })
        .collect()
}

/// The schedule: every item, and the grades its own history remembers.
fn items_of(v: &Value, course: &mut Course) {
    let Some(items) = v.get("items").and_then(Value::as_object) else {
        return;
    };
    for (key, it) in items {
        let id = non_empty(&either(it, &["item_id", "id"]), key);
        let history = it.get("review_history").and_then(Value::as_array);
        let first = history
            .and_then(|h| h.first())
            .and_then(|h| h.get("date"))
            .and_then(Value::as_str)
            .and_then(parse_day);
        course.items.push(Item {
            kind: item_kind(&either(it, &["item_type", "type"]), &id),
            content: text(it, "content"),
            created: day_of(it, "created_date").or(first).unwrap_or(0.0),
            ease: num(it, "easiness_factor").unwrap_or(2.5),
            interval: num(it, "interval_days").unwrap_or(1.0) as i64,
            reps: num(it, "repetitions").unwrap_or(0.0) as i64,
            due: day_of(it, "due_date").unwrap_or(0.0),
            reviewed: day_of(it, "last_reviewed"),
            mastery: num(it, "mastery_level").unwrap_or(0.0) as i64,
            id: id.clone(),
        });
        // Two grades on one day would be one key; the second and the third
        // are pushed a second apart so all of them survive.
        let mut nth: HashMap<i64, i64> = HashMap::new();
        for h in history.into_iter().flatten() {
            let Some(day) = h.get("date").and_then(Value::as_str).and_then(parse_day) else {
                continue;
            };
            let n = nth.entry(day as i64).or_default();
            course.reviews.push(Review {
                item: id.clone(),
                at: day + NOON + *n as f64,
                device: "import".into(),
                quality: num(h, "quality").unwrap_or(0.0) as i64,
            });
            *n += 1;
        }
    }
}

fn cards_of(v: &Value, course: &mut Course) {
    let Some(cards) = v.get("cards").and_then(Value::as_object) else {
        return;
    };
    for (key, c) in cards {
        course.cards.push(Card {
            item: non_empty(&either(c, &["item_id", "id"]), key),
            front: text(c, "front"),
            back: text(c, "back"),
            example: text(c, "example"),
            audio: either(c, &["audio_text", "audio"]),
            notes: text(c, "notes"),
        });
    }
}

fn mistakes_of(v: &Value, course: &mut Course) {
    let Some(patterns) = v.get("error_patterns").and_then(Value::as_object) else {
        return;
    };
    for (key, m) in patterns {
        // The example was written two ways over the year, and the older
        // spelling is the one the schema knows.
        let first = m.get("examples").and_then(Value::as_array).and_then(|a| a.first());
        let blank = Value::Null;
        let e = first.unwrap_or(&blank);
        course.mistakes.push(Mistake {
            id: key.clone(),
            category: text(m, "category"),
            subcategory: text(m, "subcategory"),
            frequency: num(m, "frequency").unwrap_or(0.0) as i64,
            last: day_of(m, "last_occurred").or_else(|| day_of(m, "last_seen")),
            notes: text(m, "notes"),
            wrong: either(e, &["your_answer", "incorrect"]),
            right: either(e, &["correct_answer", "correct"]),
            context: text(e, "context"),
        });
    }
}

/// The sittings that were played. The original numbers them two ways —
/// `001` early, `session-031` later — and the uid takes whichever the file
/// wrote, because that is the name the grammar reference points at.
fn lessons_of(v: &Value, course: &mut Course) {
    let Some(sessions) = v.get("sessions").and_then(Value::as_array) else {
        return;
    };
    for s in sessions {
        let id = either(s, &["session_id", "id"]);
        if id.is_empty() {
            continue;
        }
        let skills = strings(s, "skills_practiced");
        let skills = if skills.is_empty() { strings(s, "skill_practiced") } else { skills };
        let notes = text(s, "notes");
        let title = notes
            .lines()
            .next()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map_or_else(|| skills.join(" · "), str::to_string);
        let focus = if s.get("focus_areas").is_some() { "focus_areas" } else { "topics_covered" };
        course.lessons.push(Lesson {
            uid: format!("import-{id}"),
            title,
            for_date: day_of(s, "date").unwrap_or(0.0),
            focus: list(s, focus),
            status: "done".into(),
            generated: 0.0,
            accuracy: num(s, "accuracy"),
            minutes: num(s, "duration_minutes"),
            notes,
        });
    }
}

/// The lesson the compiler authored for the next day, with its exercises
/// in the order its sections name them.
fn next_of(v: &Value, course: &mut Course) {
    let id = either(v, &["session_id", "id"]);
    if id.is_empty() {
        return;
    }
    let uid = format!("import-{id}");
    course.lessons.push(Lesson {
        uid: uid.clone(),
        title: text(v, "title"),
        for_date: day_of(v, "for_date").unwrap_or(0.0),
        focus: list(v, "focus"),
        status: "ready".into(),
        generated: v.get("generated_at").and_then(Value::as_str).and_then(parse_ts).unwrap_or(0.0),
        accuracy: None,
        minutes: None,
        notes: String::new(),
    });
    let blank = serde_json::Map::new();
    let defs = v.get("exercises").and_then(Value::as_object).unwrap_or(&blank);
    // The sections are the order; a file without them keeps the map's own.
    let mut order: Vec<(String, String)> = Vec::new();
    match v.get("sections").and_then(Value::as_array) {
        Some(sections) => {
            for sec in sections {
                let kind = text(sec, "kind");
                for e in sec.get("exercises").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(id) = e.as_str() {
                        order.push((kind.clone(), id.to_string()));
                    }
                }
            }
        }
        None => order.extend(defs.keys().map(|k| (String::new(), k.clone()))),
    }
    // A set piece is one text and several questions about it: the first
    // question is in the anchor's own prompt, under a bare `Frage:`, and
    // the rest name the anchor. The text goes on every one of them, so
    // each question can be played on its own.
    let anchors: HashSet<String> = order
        .iter()
        .filter_map(|(_, id)| defs.get(id).map(|d| text(d, "passage_ref")))
        .filter(|r| !r.is_empty())
        .collect();
    let mut passages: HashMap<String, (String, String)> = HashMap::new();
    for id in &anchors {
        if let Some(d) = defs.get(id) {
            if let Some(split) = set_piece(&text(d, "prompt")) {
                passages.insert(id.clone(), split);
            }
        }
    }
    for (seq, (section, id)) in order.iter().enumerate() {
        let Some(d) = defs.get(id) else { continue };
        let split = passages.get(id);
        let referred = text(d, "passage_ref");
        let passage = match (split, passages.get(&referred)) {
            (Some((p, _)), _) | (None, Some((p, _))) => p.clone(),
            (None, None) => String::new(),
        };
        course.exercises.push(Exercise {
            lesson_uid: uid.clone(),
            seq: seq as i64 + 1,
            section: section.clone(),
            kind: either(d, &["type", "kind"]),
            grading: non_empty(&text(d, "grading"), "closed"),
            prompt: split.map_or_else(|| text(d, "prompt"), |(_, q)| q.clone()),
            passage,
            audio: either(d, &["audio_text", "audio"]),
            choices: list(d, "choices"),
            accepted: list(d, "accepted_answers"),
            model: either(d, &["model_answer", "model"]),
            hints: list(d, "hints"),
            explanation: text(d, "explanation"),
            items: list(d, "srs_item_ids"),
            difficulty: num(d, "difficulty").unwrap_or(2.0) as i64,
        });
    }
}

/// The passage and the first question of a set piece's anchor, where its
/// prompt holds both. `None` when the prompt is a question and nothing
/// more.
#[must_use]
pub fn set_piece(prompt: &str) -> Option<(String, String)> {
    let paragraphs: Vec<&str> = prompt.split("\n\n").collect();
    for (i, p) in paragraphs.iter().enumerate() {
        let Some(question) = p.trim_start().strip_prefix("Frage:") else {
            continue;
        };
        if i == 0 {
            return None;
        }
        let rest = std::iter::once(question.trim().to_string())
            .chain(paragraphs[i + 1..].iter().map(|p| (*p).to_string()))
            .collect::<Vec<_>>()
            .join("\n\n");
        return Some((paragraphs[..i].join("\n\n").trim_end().to_string(), rest.trim().to_string()));
    }
    None
}

/// The grammar reference, and the learner's own stumbles under each rule.
fn topics_of(v: &Value, course: &mut Course) {
    let updated = v.get("updated_at").and_then(Value::as_str).and_then(parse_ts).unwrap_or(0.0);
    let Some(topics) = v.get("topics").and_then(Value::as_object) else {
        return;
    };
    for (key, t) in topics {
        let id = non_empty(&either(t, &["topic_id", "id"]), key);
        let session = |k: &str| {
            t.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| format!("import-{s}"))
        };
        course.topics.push(Topic {
            title: text(t, "title"),
            category: non_empty(&text(t, "category"), "other"),
            level: non_empty(&text(t, "level"), "A1"),
            summary: text(t, "summary"),
            mastery: t.get("mastery").and_then(Value::as_i64),
            items: list(t, "srs_item_ids"),
            sections: list(t, "sections"),
            related: list(t, "related"),
            introduced: session("introduced_in_session"),
            practiced: session("last_practiced_session"),
            updated,
            id: id.clone(),
        });
        let notes = t
            .get("session_notes")
            .and_then(Value::as_array)
            .or_else(|| t.get("notes").and_then(Value::as_array));
        for (n, note) in notes.into_iter().flatten().enumerate() {
            let line = either(note, &["note", "text"]);
            if line.is_empty() {
                continue;
            }
            let session = either(note, &["session_id", "session"]);
            course.notes.push(Note {
                // Named by the rule it is under and its place in the list,
                // so importing the same folder twice writes one note.
                uid: format!("import-{id}-{n}"),
                topic: id.clone(),
                note: line,
                lesson_uid: if session.is_empty() { String::new() } else { format!("import-{session}") },
            });
        }
    }
}

/// The devices' own logs: one grade per line, appended as it was given.
///
/// A folder's log directory is listed where the disk can list it; a disk
/// that cannot — the demo tree a library mount reads — is asked for the
/// pipeline's cursor file, which names the logs it has folded. Both are
/// consulted and a name found twice is read once.
fn logs_of(disk: &mut dyn Disk, dir: &str, course: &mut Course) {
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = disk.list_dir(&real_path(&format!("{dir}/logs"))) {
        names.extend(entries.into_iter().map(|e| e.name).filter(|n| is_log(n)));
    }
    if let Ok(Some(cursors)) = slurp(disk, dir, "logs/.cursors.json") {
        if let Some(files) = cursors.get("files").and_then(Value::as_object) {
            names.extend(files.keys().filter(|n| is_log(n)).cloned());
        }
    }
    names.sort();
    names.dedup();
    let mut seen: HashSet<(String, i64, String)> = course
        .reviews
        .iter()
        .map(|r| (r.item.clone(), r.at as i64, r.device.clone()))
        .collect();
    for name in names {
        let Ok(bytes) = disk.read_file(&real_path(&format!("{dir}/logs/{name}")), MAX_FILE) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else { continue };
        // The device is the file's name, which is what the folder is keyed
        // by; the line repeats it, and a line that disagrees is still that
        // file's.
        let device = name.trim_start_matches("reviews-").trim_end_matches(".jsonl").to_string();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let item = either(&v, &["item_id", "id"]);
            let Some(at) = v.get("reviewed_at").and_then(Value::as_str).and_then(parse_ts) else {
                continue;
            };
            if item.is_empty() || !seen.insert((item.clone(), at as i64, device.clone())) {
                continue;
            }
            course.reviews.push(Review {
                item,
                at,
                device: device.clone(),
                quality: num(&v, "quality").unwrap_or(0.0) as i64,
            });
        }
    }
}

fn is_log(name: &str) -> bool {
    name.starts_with("reviews-") && name.ends_with(".jsonl")
}

// -- the shapes the files disagree about ----------------------------------

/// The first of `keys` this object has as a string. The original spells
/// one field two ways in two files, and both are read.
fn either(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn day_of(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_str).and_then(parse_day)
}

/// A JSON array as it stands, for a column that keeps one. Anything else
/// — a missing key, a string where a list was meant — is the empty list.
fn list(v: &Value, key: &str) -> String {
    v.get(key).filter(|v| v.is_array()).map_or_else(|| "[]".to_string(), Value::to_string)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn non_empty(text: &str, fallback: &str) -> String {
    if text.is_empty() { fallback.to_string() } else { text.to_string() }
}

/// The kind an item is on this schedule. The type is the file's word where
/// it wrote one; an item with a type nobody here knows — the original has
/// a `skill` — is told apart by its slug, which is how the deck names a
/// word.
fn item_kind(kind: &str, id: &str) -> String {
    match kind {
        "vocabulary" => "vocab",
        "grammar_rule" => "grammar",
        "error_pattern" => "error",
        _ if id.starts_with("vocab") => "vocab",
        _ => "grammar",
    }
    .to_string()
}

/// `2026-08-05` as the day it names: midnight UTC, which is what a `due`
/// column holds.
#[must_use]
pub fn parse_day(text: &str) -> Option<f64> {
    let mut it = text.trim().split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(kernel::time::ts(y, m, d, 0, 0))
}

/// `2026-08-05T15:10:58Z` as the instant it names. A zone that is not `Z`
/// is taken off, so what comes out is always UTC; a fractional second is
/// dropped, because a grade is filed to the second.
#[must_use]
pub fn parse_ts(text: &str) -> Option<f64> {
    let text = text.trim();
    let (date, rest) = text.split_once('T').or_else(|| text.split_once(' '))?;
    let day = parse_day(date)?;
    let mut split = rest.len();
    for (i, ch) in rest.char_indices() {
        if matches!(ch, 'Z' | 'z' | '+') || (ch == '-' && i > 0) {
            split = i;
            break;
        }
    }
    let (clock, zone) = rest.split_at(split);
    let mut parts = clock.split(':');
    let h: f64 = parts.next()?.trim().parse().ok()?;
    let m: f64 = parts.next().unwrap_or("0").parse().ok()?;
    let s: f64 = parts.next().unwrap_or("0").split('.').next()?.parse().ok()?;
    if !(0.0..24.0).contains(&h) || !(0.0..60.0).contains(&m) || !(0.0..61.0).contains(&s) {
        return None;
    }
    let offset = match zone.chars().next() {
        None | Some('Z' | 'z') => 0.0,
        Some(sign) => {
            let (oh, om) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
            let magnitude = oh.parse::<f64>().ok()? * 3600.0 + om.parse::<f64>().unwrap_or(0.0) * 60.0;
            if sign == '-' { magnitude } else { -magnitude }
        }
    };
    Some(day + h * 3600.0 + m * 60.0 + s + offset)
}

// -- writing --------------------------------------------------------------

/// What one table took and what it already had.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count {
    pub added: usize,
    pub skipped: usize,
}

impl Count {
    fn took(&mut self, rows: usize) -> bool {
        if rows > 0 {
            self.added += 1;
        } else {
            self.skipped += 1;
        }
        rows > 0
    }
}

/// What an import wrote, table by table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Imported {
    /// Whether the learner's own row was written over.
    pub learner: bool,
    pub skills: Count,
    pub items: Count,
    pub cards: Count,
    pub reviews: Count,
    pub topics: Count,
    pub notes: Count,
    pub mistakes: Count,
    pub lessons: Count,
    pub exercises: Count,
}

impl Imported {
    /// Everything the course already had under the same key.
    #[must_use]
    pub fn skipped(&self) -> usize {
        [
            self.skills, self.items, self.cards, self.reviews, self.topics, self.notes,
            self.mistakes, self.lessons, self.exercises,
        ]
        .iter()
        .map(|c| c.skipped)
        .sum()
    }

    /// The one line the panel says.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} items · {} cards · {} grades · {} lessons · {} topics · {} mistakes imported · {} already in the course",
            self.items.added,
            self.cards.added,
            self.reviews.added,
            self.lessons.added,
            self.topics.added,
            self.mistakes.added,
            self.skipped()
        )
    }
}

/// Where an item's schedule stood. Kept for an item this import did not
/// insert but did file a grade on: taking the grade back leaves the item
/// with none, and replaying no grades says nothing about where it stands,
/// so undo puts the state back by hand.
#[derive(Debug, Clone)]
struct Schedule {
    item: String,
    ease: f64,
    interval: i64,
    reps: i64,
    due: f64,
    reviewed: Option<f64>,
    mastery: i64,
}

/// Exactly what this import put down, so undo can take it back and no more.
#[derive(Debug, Clone, Default)]
struct Planted {
    /// The learner row as it stood, where there was one to write over.
    learner: Option<Learner>,
    learner_written: bool,
    skills: Vec<String>,
    items: Vec<String>,
    cards: Vec<String>,
    reviews: Vec<(String, f64, String)>,
    topics: Vec<String>,
    notes: Vec<String>,
    mistakes: Vec<String>,
    lessons: Vec<String>,
    exercises: Vec<(String, i64)>,
    moved: Vec<Schedule>,
}

impl Planted {
    fn any(&self) -> bool {
        self.learner_written
            || !self.skills.is_empty()
            || !self.items.is_empty()
            || !self.cards.is_empty()
            || !self.reviews.is_empty()
            || !self.topics.is_empty()
            || !self.notes.is_empty()
            || !self.mistakes.is_empty()
            || !self.lessons.is_empty()
            || !self.exercises.is_empty()
            || !self.moved.is_empty()
    }
}

/// The whole course, written in one transaction. Every row is offered
/// under its own key and the one already there wins, so this is the same
/// write however often it runs; the schedule is replayed at the end,
/// because grades have arrived.
fn insert_all(c: &Connection, course: &Course) -> rusqlite::Result<(Imported, Planted)> {
    let mut done = Imported::default();
    let mut planted = Planted::default();

    if let Some(l) = &course.learner {
        planted.learner = c
            .query_row(
                "SELECT name, native, target, level, goal, daily_minutes, streak, last_active, started
                   FROM fluent_learner WHERE id = 1",
                [],
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
                        started: r.get(8)?,
                    })
                },
            )
            .ok();
        c.execute(
            "INSERT INTO fluent_learner(id, name, native, target, level, goal, daily_minutes, streak, last_active, started)
             VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET name = ?1, native = ?2, target = ?3, level = ?4, goal = ?5,
                    daily_minutes = ?6, streak = ?7, last_active = ?8, started = ?9",
            params![l.name, l.native, l.target, l.level, l.goal, l.daily_minutes, l.streak, l.last_active, l.started],
        )?;
        planted.learner_written = true;
        done.learner = true;
    }

    for s in &course.skills {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_skill(name, mastery, accuracy, lessons, practiced) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![s.name, s.mastery, s.accuracy, s.lessons, s.practiced],
        )?;
        if done.skills.took(rows) {
            planted.skills.push(s.name.clone());
        }
    }

    // The lessons come first: a topic names the one it was introduced in,
    // and an exercise and a grade are numbered by the trigger that finds it.
    for l in &course.lessons {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_lesson(uid, title, for_date, focus, status, generated, accuracy, minutes, notes)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![l.uid, l.title, l.for_date, l.focus, l.status, l.generated, l.accuracy, l.minutes, l.notes],
        )?;
        if done.lessons.took(rows) {
            planted.lessons.push(l.uid.clone());
        }
    }
    for e in &course.exercises {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_exercise(lesson_uid, seq, section, kind, grading, prompt, passage, audio,
                    choices, accepted, model, hints, explanation, items, difficulty)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                e.lesson_uid, e.seq, e.section, e.kind, e.grading, e.prompt, e.passage, e.audio,
                e.choices, e.accepted, e.model, e.hints, e.explanation, e.items, e.difficulty
            ],
        )?;
        if done.exercises.took(rows) {
            planted.exercises.push((e.lesson_uid.clone(), e.seq));
        }
    }

    // An item whose grades the folder remembers is replayed from them; one
    // it kept only a schedule for starts its replay there, on every device,
    // so the first grade given here carries the progress on rather than
    // starting the word over.
    let graded: HashSet<&String> = course.reviews.iter().map(|r| &r.item).collect();
    for it in &course.items {
        let base = if graded.contains(&it.id) {
            String::new()
        } else {
            model::base_json(&sm2::Replay {
                state: sm2::State { ease: it.ease, interval: it.interval, reps: it.reps },
                due: it.due,
                reviewed: it.reviewed,
                mastery: it.mastery,
            })
        };
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_item(id, kind, content, created, ease, interval, reps, due, reviewed, mastery, base)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                it.id, it.kind, it.content, it.created, it.ease, it.interval, it.reps, it.due,
                it.reviewed, it.mastery, base
            ],
        )?;
        if done.items.took(rows) {
            planted.items.push(it.id.clone());
        }
    }
    // A card is a word on the schedule too. A folder with a deck and no
    // schedule — or a deck with words the schedule never listed — puts
    // each such word down as a fresh item, due today, so the card is in
    // the deck rather than behind a join nothing satisfies.
    let today = sm2::day_start(course.imported_at);
    for card in &course.cards {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_item(id, kind, content, created, due, base) VALUES(?1, 'vocab', ?2, ?3, ?4, ?5)",
            params![card.item, card.front, course.imported_at, today, model::fresh_base(today)],
        )?;
        // Counted only where it was written: a card whose word the
        // schedule listed is not an item skipped.
        if rows > 0 {
            done.items.added += 1;
            planted.items.push(card.item.clone());
        }
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_card(item, front, back, example, audio, notes) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![card.item, card.front, card.back, card.example, card.audio, card.notes],
        )?;
        if done.cards.took(rows) {
            planted.cards.push(card.item.clone());
        }
    }

    for t in &course.topics {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_topic(id, title, category, level, summary, mastery, items,
                    introduced, practiced, sections, related, updated)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                t.id,
                t.title,
                t.category,
                t.level,
                t.summary,
                t.mastery,
                t.items,
                t.introduced.clone().unwrap_or_default(),
                t.practiced.clone().unwrap_or_default(),
                t.sections,
                t.related,
                t.updated
            ],
        )?;
        if done.topics.took(rows) {
            planted.topics.push(t.id.clone());
        }
    }
    for n in &course.notes {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_topic_note(uid, topic, note, at, lesson_uid)
             VALUES(?1, ?2, ?3, COALESCE((SELECT for_date FROM fluent_lesson WHERE uid = ?4), 0), ?4)",
            params![n.uid, n.topic, n.note, n.lesson_uid],
        )?;
        if done.notes.took(rows) {
            planted.notes.push(n.uid.clone());
        }
    }
    for m in &course.mistakes {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_mistake(id, category, subcategory, frequency, last, notes, wrong, right, context)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![m.id, m.category, m.subcategory, m.frequency, m.last, m.notes, m.wrong, m.right, m.context],
        )?;
        if done.mistakes.took(rows) {
            planted.mistakes.push(m.id.clone());
        }
    }

    // An item this import did not write but is about to grade stands
    // somewhere already; that is what undo has to put back.
    let ours: HashSet<&String> = planted.items.iter().collect();
    let mut touched: Vec<&String> = course.reviews.iter().map(|r| &r.item).collect();
    touched.sort();
    touched.dedup();
    for item in touched {
        if ours.contains(item) {
            continue;
        }
        let was = c
            .query_row(
                "SELECT ease, interval, reps, due, reviewed, mastery FROM fluent_item WHERE id = ?1",
                [item],
                |r| {
                    Ok(Schedule {
                        item: item.clone(),
                        ease: r.get(0)?,
                        interval: r.get(1)?,
                        reps: r.get(2)?,
                        due: r.get(3)?,
                        reviewed: r.get(4)?,
                        mastery: r.get(5)?,
                    })
                },
            )
            .ok();
        planted.moved.extend(was);
    }

    for r in &course.reviews {
        let rows = c.execute(
            "INSERT OR IGNORE INTO fluent_review(item, at, device, quality) VALUES(?1, ?2, ?3, ?4)",
            params![r.item, r.at, r.device, r.quality],
        )?;
        if done.reviews.took(rows) {
            planted.reviews.push((r.item.clone(), r.at, r.device.clone()));
        }
    }

    // Grades have arrived, so every item that has any now stands where
    // they leave it. An item the file carried a cached state for and no
    // history keeps that state: replaying nothing writes nothing.
    model::recompute_all_tx(c)?;
    Ok((done, planted))
}

/// The rows an import is, kept whole so one press takes them back.
struct Migrated {
    course: Course,
    planted: Planted,
}

impl Intent for Migrated {
    fn describe(&self) -> String {
        format!("the course from {}", self.course.root)
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        let p = self.planted.clone();
        w.store()
            .write(move |c| {
                for (item, at, device) in &p.reviews {
                    c.execute(
                        "DELETE FROM fluent_review WHERE item = ?1 AND at = ?2 AND device = ?3",
                        params![item, at, device],
                    )?;
                }
                for (uid, seq) in &p.exercises {
                    c.execute("DELETE FROM fluent_exercise WHERE lesson_uid = ?1 AND seq = ?2", params![uid, seq])?;
                }
                for uid in &p.lessons {
                    c.execute("DELETE FROM fluent_lesson WHERE uid = ?1", [uid])?;
                }
                for uid in &p.notes {
                    c.execute("DELETE FROM fluent_topic_note WHERE uid = ?1", [uid])?;
                }
                for id in &p.topics {
                    c.execute("DELETE FROM fluent_topic WHERE id = ?1", [id])?;
                }
                for item in &p.cards {
                    c.execute("DELETE FROM fluent_card WHERE item = ?1", [item])?;
                }
                for id in &p.items {
                    c.execute("DELETE FROM fluent_item WHERE id = ?1", [id])?;
                }
                for id in &p.mistakes {
                    c.execute("DELETE FROM fluent_mistake WHERE id = ?1", [id])?;
                }
                for name in &p.skills {
                    c.execute("DELETE FROM fluent_skill WHERE name = ?1", [name])?;
                }
                if p.learner_written {
                    match &p.learner {
                        Some(l) => {
                            c.execute(
                                "UPDATE fluent_learner SET name = ?1, native = ?2, target = ?3, level = ?4,
                                        goal = ?5, daily_minutes = ?6, streak = ?7, last_active = ?8, started = ?9
                                  WHERE id = 1",
                                params![l.name, l.native, l.target, l.level, l.goal, l.daily_minutes, l.streak, l.last_active, l.started],
                            )?;
                        }
                        None => {
                            c.execute("DELETE FROM fluent_learner WHERE id = 1", [])?;
                        }
                    }
                }
                // An item that keeps grades is replayed from them; one
                // whose only grades were this import's has none left to
                // replay, so its schedule is written back as it stood.
                for was in &p.moved {
                    c.execute(
                        "UPDATE fluent_item SET ease = ?2, interval = ?3, reps = ?4, due = ?5, reviewed = ?6, mastery = ?7
                          WHERE id = ?1",
                        params![was.item, was.ease, was.interval, was.reps, was.due, was.reviewed, was.mastery],
                    )?;
                }
                model::recompute_all_tx(c)?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        let course = self.course.clone();
        w.store()
            .write(move |c| insert_all(c, &course).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}

/// The course written into the store: one action, one undo.
///
/// # Errors
///
/// If the store refused the write.
pub fn import(s: &mut Session, course: Course) -> Result<Imported, String> {
    let course = Course { imported_at: s.now(), ..course };
    let label = format!("import the course from {}", course.root);
    let rows = course.clone();
    let written = s.act(Action::writing("fluent.import", label, move |c| insert_all(c, &rows)));
    let Some((done, planted)) = written else {
        return Err("the store refused the course".into());
    };
    if planted.any() {
        s.claim(Box::new(Migrated { course, planted }));
    }
    Ok(done)
}

/// Reading the folder, as an effect: a worker builds a world of its own to
/// run it, so a course of a thousand grades is parsed off the UI thread.
pub struct Read(pub String);

impl Effect for Read {
    const KIND: &'static str = "fluent.read_course";
    type Reply = Course;
    fn describe(&self) -> String {
        format!("read the course at {}", self.0)
    }
    fn writes(&self) -> bool {
        false
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Course, String> {
        let disk = cx.cap::<dyn Disk>()?;
        read(disk, &self.0)
    }
}
