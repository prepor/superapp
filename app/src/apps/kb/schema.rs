//! The KB's tables, as CR-022's store section spells them. One rung.
//!
//! A page is a row keyed by a random `uid` with its slug unique beside it;
//! a file is a path with a hash; a revision is one write of a page; the
//! draft, the links and the aliases are this device's own. Every column but
//! a key has a default, because a row another device made arrives one cell
//! at a time.
//!
//! `kb_cache` is the prototype's own and no part of the plan: it stands in
//! for the blob cache and the outbox, saying where a file's bytes are so a
//! file card can draw each of its four states before phase 3 builds the
//! real ones.

use kernel::app::{Schema, Step};

pub static SCHEMA: Schema = Schema {
    app: "kb",
    steps: &[Step::Sql(V1)],
};

const V1: &str = "
CREATE TABLE kb_page (
    uid TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL DEFAULT 'concept',
    title TEXT NOT NULL DEFAULT '',
    summary TEXT NOT NULL DEFAULT '',
    aliases TEXT NOT NULL DEFAULT '[]',
    tags TEXT NOT NULL DEFAULT '[]',
    extra TEXT NOT NULL DEFAULT '{}',
    body TEXT NOT NULL DEFAULT '',
    path TEXT NOT NULL DEFAULT '',
    created REAL NOT NULL DEFAULT 0,
    updated REAL NOT NULL DEFAULT 0,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE kb_file (
    path TEXT PRIMARY KEY NOT NULL,
    hash TEXT NOT NULL DEFAULT '',
    mime TEXT NOT NULL DEFAULT '',
    size INTEGER NOT NULL DEFAULT 0,
    text TEXT NOT NULL DEFAULT '',
    created REAL NOT NULL DEFAULT 0,
    updated REAL NOT NULL DEFAULT 0,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE kb_revision (
    uid TEXT PRIMARY KEY NOT NULL,
    page TEXT NOT NULL DEFAULT '',
    at REAL NOT NULL DEFAULT 0,
    device TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT 'editor',
    chat_title TEXT NOT NULL DEFAULT '',
    message TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT ''
);
CREATE INDEX kb_revision_page ON kb_revision(page, at DESC);
CREATE TABLE kb_draft (
    page TEXT PRIMARY KEY NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    updated REAL NOT NULL DEFAULT 0
);
CREATE TABLE kb_link (
    page TEXT NOT NULL,
    target TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'wiki',
    stamp REAL NOT NULL DEFAULT 0
);
CREATE INDEX kb_link_page ON kb_link(page);
CREATE INDEX kb_link_target ON kb_link(target);
CREATE TABLE kb_alias (
    alias TEXT PRIMARY KEY NOT NULL,
    uid TEXT NOT NULL
);
CREATE TABLE kb_cache (
    hash TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL DEFAULT 'missing'
);
";
