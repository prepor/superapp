//! The course's tables. One rung so far.

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
    kind TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    ease REAL NOT NULL DEFAULT 2.5,
    interval INTEGER NOT NULL DEFAULT 1,
    reps INTEGER NOT NULL DEFAULT 0,
    due REAL NOT NULL DEFAULT 0,
    created REAL NOT NULL DEFAULT 0,
    reviewed REAL,
    mastery INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX fluent_item_due ON fluent_item(kind, due);
CREATE TABLE fluent_card (
    item TEXT PRIMARY KEY REFERENCES fluent_item(id),
    front TEXT NOT NULL,
    back TEXT NOT NULL,
    example TEXT NOT NULL DEFAULT '',
    audio TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT ''
);
CREATE TABLE fluent_review (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item TEXT NOT NULL,
    at REAL NOT NULL,
    quality INTEGER NOT NULL DEFAULT 0,
    device TEXT NOT NULL DEFAULT '',
    lesson INTEGER,
    exercise INTEGER,
    UNIQUE(item, at, device)
);
CREATE INDEX fluent_review_item ON fluent_review(item, at);
CREATE TABLE fluent_lesson (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL DEFAULT '',
    for_date REAL NOT NULL,
    focus TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'ready',
    generated REAL NOT NULL DEFAULT 0,
    started REAL,
    ended REAL,
    accuracy REAL,
    minutes REAL,
    notes TEXT NOT NULL DEFAULT ''
);
CREATE INDEX fluent_lesson_shelf ON fluent_lesson(status, for_date);
CREATE TABLE fluent_exercise (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lesson INTEGER NOT NULL REFERENCES fluent_lesson(id),
    seq INTEGER NOT NULL,
    section TEXT NOT NULL,
    kind TEXT NOT NULL,
    grading TEXT NOT NULL,
    prompt TEXT NOT NULL,
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
    UNIQUE(lesson, seq)
);
CREATE TABLE fluent_topic (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    category TEXT NOT NULL,
    level TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    mastery INTEGER,
    items TEXT NOT NULL DEFAULT '[]',
    introduced INTEGER,
    practiced INTEGER,
    sections TEXT NOT NULL DEFAULT '[]',
    related TEXT NOT NULL DEFAULT '[]',
    updated REAL NOT NULL DEFAULT 0
);
CREATE TABLE fluent_topic_note (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    topic TEXT NOT NULL REFERENCES fluent_topic(id),
    lesson INTEGER,
    note TEXT NOT NULL,
    at REAL NOT NULL
);
CREATE TABLE fluent_mistake (
    id TEXT PRIMARY KEY,
    category TEXT NOT NULL,
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
";
