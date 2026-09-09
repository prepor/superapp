use kernel::app::{Schema, Step};
pub static SCHEMA: Schema = Schema {
    app: "calendar",
    steps: &[
        Step::Sql(
            r#"
CREATE TABLE calendar_source(
 id INTEGER PRIMARY KEY AUTOINCREMENT, account INTEGER NOT NULL REFERENCES account(id),
 remote TEXT NOT NULL, title TEXT NOT NULL, zone TEXT NOT NULL DEFAULT 'UTC',
 role TEXT NOT NULL, meet INTEGER NOT NULL DEFAULT 0, active INTEGER NOT NULL DEFAULT 1,
 checked REAL, error TEXT NOT NULL DEFAULT '', UNIQUE(account,remote)
);
CREATE TABLE calendar_event(
 id INTEGER PRIMARY KEY AUTOINCREMENT, source INTEGER NOT NULL REFERENCES calendar_source(id),
 remote TEXT NOT NULL, title TEXT NOT NULL, start REAL NOT NULL, end REAL NOT NULL,
 day TEXT NOT NULL, all_day INTEGER NOT NULL, location TEXT NOT NULL DEFAULT '',
 guests TEXT NOT NULL DEFAULT '', response TEXT NOT NULL DEFAULT '',
 meet TEXT NOT NULL DEFAULT '', series TEXT NOT NULL DEFAULT '',
 active INTEGER NOT NULL DEFAULT 1, etag TEXT NOT NULL DEFAULT '', raw TEXT NOT NULL,
 UNIQUE(source,remote)
);
CREATE INDEX calendar_event_time ON calendar_event(start,id);
CREATE INDEX calendar_event_source ON calendar_event(source,start);
CREATE TABLE calendar_draft(
 id INTEGER PRIMARY KEY AUTOINCREMENT, event INTEGER, source INTEGER NOT NULL,
 revision INTEGER NOT NULL DEFAULT 1, form TEXT NOT NULL, base TEXT NOT NULL DEFAULT '{}',
 updated REAL NOT NULL, state TEXT NOT NULL DEFAULT 'draft', error TEXT NOT NULL DEFAULT ''
);
CREATE TABLE calendar_change(
 id INTEGER PRIMARY KEY AUTOINCREMENT, draft INTEGER, source INTEGER NOT NULL,
 event INTEGER, kind TEXT NOT NULL, body TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending',
 error TEXT NOT NULL DEFAULT '', result TEXT NOT NULL DEFAULT '{}', updated REAL NOT NULL
);
CREATE TABLE calendar_sync(
 id INTEGER PRIMARY KEY CHECK(id=1), start REAL NOT NULL, end REAL NOT NULL,
 requested INTEGER NOT NULL DEFAULT 1, completed INTEGER NOT NULL DEFAULT 0,
 checked REAL, error TEXT NOT NULL DEFAULT ''
);
CREATE TABLE calendar_availability(
 id INTEGER PRIMARY KEY AUTOINCREMENT, account INTEGER NOT NULL, request TEXT NOT NULL,
 response TEXT, error TEXT NOT NULL DEFAULT '', checked REAL, draft INTEGER
);
"#,
        ),
        Step::Sql("INSERT OR IGNORE INTO calendar_sync(id,start,end) VALUES(1,0,0)"),
    ],
};
