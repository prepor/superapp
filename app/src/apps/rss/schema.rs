use kernel::app::{Schema, Step};

pub static SCHEMA: Schema = Schema {
    app: "rss",
    steps: &[Step::Sql(V1)],
};

const V1: &str = "
CREATE TABLE rss_feed (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    url TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    subscribed INTEGER NOT NULL DEFAULT 1,
    checked REAL,
    error TEXT NOT NULL DEFAULT '',
    etag TEXT NOT NULL DEFAULT '',
    modified TEXT NOT NULL DEFAULT '',
    requested INTEGER NOT NULL DEFAULT 0,
    completed INTEGER NOT NULL DEFAULT -1
);
CREATE TABLE rss_article (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    feed INTEGER NOT NULL REFERENCES rss_feed(id),
    guid TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    author TEXT NOT NULL,
    published REAL NOT NULL,
    html TEXT NOT NULL,
    seen INTEGER NOT NULL DEFAULT 0,
    UNIQUE(feed, guid)
);
CREATE INDEX rss_article_order ON rss_article(published, id);
CREATE INDEX rss_article_unseen ON rss_article(seen, published, id);
CREATE INDEX rss_article_feed ON rss_article(feed, seen);
";
