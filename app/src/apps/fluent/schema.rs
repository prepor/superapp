//! The course's tables. One rung so far.
//!
//! Every decision the learner makes travels between their devices, so each
//! table is keyed by something that names its row on all of them — a slug,
//! a uid, the instant a grade was given — and every column but that key has
//! a default, because a row another device made arrives one cell at a time.
//! What a device works out for itself — the SM-2 cache on an item, the
//! local id a lesson wears here — stays local and is kept by triggers or
//! replayed from the grades.

use kernel::app::{Schema, Step};

pub static SCHEMA: Schema = Schema {
    app: "fluent",
    steps: &[Step::Sql(V1)],
};

/// Dates are days: unix seconds at 00:00 UTC, so `due <= today` is one
/// comparison and `sqlite3` reads them with `datetime(due, 'unixepoch')`.
const V1: &str = "
CREATE TABLE fluent_learner (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    name TEXT NOT NULL DEFAULT '',
    native TEXT NOT NULL DEFAULT '',
    target TEXT NOT NULL DEFAULT '',
    level TEXT NOT NULL DEFAULT 'A1',
    goal TEXT NOT NULL DEFAULT 'B1',
    daily_minutes INTEGER NOT NULL DEFAULT 30,
    streak INTEGER NOT NULL DEFAULT 0,
    last_active REAL,
    started REAL NOT NULL DEFAULT 0
);
CREATE TABLE fluent_item (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL DEFAULT 'vocab',
    content TEXT NOT NULL DEFAULT '',
    created REAL NOT NULL DEFAULT 0,
    -- The schedule: replayed from this device's grades, never written by
    -- hand and never replicated, so two devices grading one card in one
    -- day agree by adding their grades up rather than by overwriting.
    ease REAL NOT NULL DEFAULT 2.5,
    interval INTEGER NOT NULL DEFAULT 1,
    reps INTEGER NOT NULL DEFAULT 0,
    due REAL NOT NULL DEFAULT 0,
    reviewed REAL,
    mastery INTEGER NOT NULL DEFAULT 0,
    -- Where the replay starts: JSON of the schedule the item was made with
    -- — a migrated item's cached state where its grades were never written
    -- down, a fresh one's due day otherwise. Replicated, so every device
    -- replays from the same place. '' is a fresh item due on its created day.
    base TEXT NOT NULL DEFAULT ''
);
CREATE INDEX fluent_item_due ON fluent_item(kind, due);
CREATE TABLE fluent_card (
    item TEXT PRIMARY KEY,
    front TEXT NOT NULL DEFAULT '',
    back TEXT NOT NULL DEFAULT '',
    example TEXT NOT NULL DEFAULT '',
    audio TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT ''
);
-- What the tutor once said a word means, kept so it is asked once. A cache
-- and nothing else: it travels nowhere, nothing is scheduled by it, and a
-- store that lost it is a store that asks again. The key is the dictionary
-- form the tutor answered with — a noun with its article — which is how a
-- selection finds it: the same article-blind match the deck is searched by.
CREATE TABLE fluent_lookup (
    term TEXT PRIMARY KEY,
    translation TEXT NOT NULL DEFAULT '',
    pos TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    at REAL NOT NULL DEFAULT 0
);
CREATE TABLE fluent_review (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item TEXT NOT NULL,
    at REAL NOT NULL,
    device TEXT NOT NULL DEFAULT '',
    quality INTEGER NOT NULL DEFAULT 0,
    lesson_uid TEXT NOT NULL DEFAULT '',
    seq INTEGER,
    lesson INTEGER,  -- local: the lesson_uid as this device numbers it
    UNIQUE(item, at, device)
);
CREATE INDEX fluent_review_item ON fluent_review(item, at);
CREATE INDEX fluent_review_from ON fluent_review(lesson_uid, seq);
CREATE TABLE fluent_lesson (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uid TEXT NOT NULL UNIQUE DEFAULT (lower(hex(randomblob(16)))),
    title TEXT NOT NULL DEFAULT '',
    for_date REAL NOT NULL DEFAULT 0,
    focus TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'ready',
    generated REAL NOT NULL DEFAULT 0,
    started REAL,
    ended REAL,
    accuracy REAL,
    minutes REAL,
    notes TEXT NOT NULL DEFAULT '',
    chat INTEGER  -- local: the tutor's chat about this lesson
);
CREATE INDEX fluent_lesson_shelf ON fluent_lesson(status, for_date);
CREATE TABLE fluent_exercise (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lesson_uid TEXT NOT NULL DEFAULT '',
    seq INTEGER NOT NULL,
    lesson INTEGER NOT NULL DEFAULT 0,  -- local: the lesson_uid, numbered here
    section TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT '',
    grading TEXT NOT NULL DEFAULT 'closed',
    prompt TEXT NOT NULL DEFAULT '',
    passage TEXT NOT NULL DEFAULT '',
    audio TEXT NOT NULL DEFAULT '',
    choices TEXT NOT NULL DEFAULT '[]',
    accepted TEXT NOT NULL DEFAULT '[]',
    model TEXT NOT NULL DEFAULT '',
    hints TEXT NOT NULL DEFAULT '[]',
    explanation TEXT NOT NULL DEFAULT '',
    items TEXT NOT NULL DEFAULT '[]',
    difficulty INTEGER NOT NULL DEFAULT 1,
    answer TEXT,
    result TEXT,
    self_grade INTEGER,
    tutor_grade INTEGER,
    tutor_note TEXT NOT NULL DEFAULT '',
    tutor_fix TEXT NOT NULL DEFAULT '',
    hints_shown INTEGER NOT NULL DEFAULT 0,
    elapsed REAL NOT NULL DEFAULT 0,
    answered REAL,
    UNIQUE(lesson_uid, seq)
);
CREATE INDEX fluent_exercise_order ON fluent_exercise(lesson, seq);
CREATE TABLE fluent_topic (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    category TEXT NOT NULL DEFAULT 'other',
    level TEXT NOT NULL DEFAULT 'A1',
    summary TEXT NOT NULL DEFAULT '',
    mastery INTEGER,
    items TEXT NOT NULL DEFAULT '[]',
    -- The lessons it was introduced and last practiced in, by the uid every
    -- device knows them under; '' is none. The local id is a lookup away.
    introduced TEXT NOT NULL DEFAULT '',
    practiced TEXT NOT NULL DEFAULT '',
    sections TEXT NOT NULL DEFAULT '[]',
    related TEXT NOT NULL DEFAULT '[]',
    updated REAL NOT NULL DEFAULT 0,
    rank INTEGER NOT NULL DEFAULT 99  -- local: the category's place in the list
);
CREATE TABLE fluent_topic_note (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uid TEXT NOT NULL UNIQUE DEFAULT (lower(hex(randomblob(16)))),
    topic TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    at REAL NOT NULL DEFAULT 0,
    lesson_uid TEXT NOT NULL DEFAULT '',
    lesson INTEGER  -- local: the lesson_uid, numbered here
);
CREATE TABLE fluent_mistake (
    id TEXT PRIMARY KEY,
    category TEXT NOT NULL DEFAULT '',
    subcategory TEXT NOT NULL DEFAULT '',
    frequency INTEGER NOT NULL DEFAULT 0,
    last REAL,
    notes TEXT NOT NULL DEFAULT '',
    wrong TEXT NOT NULL DEFAULT '',
    right TEXT NOT NULL DEFAULT '',
    context TEXT NOT NULL DEFAULT ''
);
CREATE TABLE fluent_skill (
    name TEXT PRIMARY KEY,
    mastery INTEGER NOT NULL DEFAULT 0,
    accuracy REAL NOT NULL DEFAULT 0,
    lessons INTEGER NOT NULL DEFAULT 0,
    practiced REAL
);

-- A lesson is named across devices by its uid; the integer id is this
-- device's own, and panels, tools and joins are written in it. So a row
-- that names a lesson carries the uid and the triggers keep the local id
-- beside it, whichever of the two rows this device sees first.
CREATE TRIGGER fluent_review_numbered AFTER INSERT ON fluent_review
WHEN EXISTS (SELECT 1 FROM fluent_lesson WHERE uid = new.lesson_uid) BEGIN
    UPDATE fluent_review SET lesson = (SELECT id FROM fluent_lesson WHERE uid = new.lesson_uid)
     WHERE id = new.id;
END;
CREATE TRIGGER fluent_review_renumbered AFTER UPDATE OF lesson_uid ON fluent_review
WHEN EXISTS (SELECT 1 FROM fluent_lesson WHERE uid = new.lesson_uid) BEGIN
    UPDATE fluent_review SET lesson = (SELECT id FROM fluent_lesson WHERE uid = new.lesson_uid)
     WHERE id = new.id;
END;
CREATE TRIGGER fluent_exercise_numbered AFTER INSERT ON fluent_exercise
WHEN EXISTS (SELECT 1 FROM fluent_lesson WHERE uid = new.lesson_uid) BEGIN
    UPDATE fluent_exercise SET lesson = (SELECT id FROM fluent_lesson WHERE uid = new.lesson_uid)
     WHERE id = new.id;
END;
CREATE TRIGGER fluent_exercise_renumbered AFTER UPDATE OF lesson_uid ON fluent_exercise
WHEN EXISTS (SELECT 1 FROM fluent_lesson WHERE uid = new.lesson_uid) BEGIN
    UPDATE fluent_exercise SET lesson = (SELECT id FROM fluent_lesson WHERE uid = new.lesson_uid)
     WHERE id = new.id;
END;
CREATE TRIGGER fluent_topic_note_numbered AFTER INSERT ON fluent_topic_note
WHEN EXISTS (SELECT 1 FROM fluent_lesson WHERE uid = new.lesson_uid) BEGIN
    UPDATE fluent_topic_note SET lesson = (SELECT id FROM fluent_lesson WHERE uid = new.lesson_uid)
     WHERE id = new.id;
END;
CREATE TRIGGER fluent_topic_note_renumbered AFTER UPDATE OF lesson_uid ON fluent_topic_note
WHEN EXISTS (SELECT 1 FROM fluent_lesson WHERE uid = new.lesson_uid) BEGIN
    UPDATE fluent_topic_note SET lesson = (SELECT id FROM fluent_lesson WHERE uid = new.lesson_uid)
     WHERE id = new.id;
END;
-- The other order: the grades and the exercises of a lesson can reach this
-- device before the lesson row does.
CREATE TRIGGER fluent_lesson_numbers AFTER INSERT ON fluent_lesson BEGIN
    UPDATE fluent_review SET lesson = new.id WHERE lesson_uid = new.uid AND lesson IS NULL;
    UPDATE fluent_exercise SET lesson = new.id WHERE lesson_uid = new.uid AND lesson = 0;
    UPDATE fluent_topic_note SET lesson = new.id WHERE lesson_uid = new.uid AND lesson IS NULL;
END;

-- The grammar list's sections are the original's order, not the category
-- slug's, so the category carries a rank the order names.
CREATE TRIGGER fluent_topic_ranked AFTER INSERT ON fluent_topic BEGIN
    UPDATE fluent_topic SET rank = CASE new.category
        WHEN 'cases' THEN 0 WHEN 'prepositions' THEN 1 WHEN 'adjectives' THEN 2
        WHEN 'verbs' THEN 3 WHEN 'sentence_structure' THEN 4 WHEN 'pronouns' THEN 5
        WHEN 'nouns' THEN 6 ELSE 7 END
     WHERE id = new.id;
END;
CREATE TRIGGER fluent_topic_reranked AFTER UPDATE OF category ON fluent_topic BEGIN
    UPDATE fluent_topic SET rank = CASE new.category
        WHEN 'cases' THEN 0 WHEN 'prepositions' THEN 1 WHEN 'adjectives' THEN 2
        WHEN 'verbs' THEN 3 WHEN 'sentence_structure' THEN 4 WHEN 'pronouns' THEN 5
        WHEN 'nouns' THEN 6 ELSE 7 END
     WHERE id = new.id;
END;
";
