use kernel::app::{Schema, Step};
use kernel::sync::Replicated;
use rusqlite::{params, Connection};

use crate::reader::html;

pub static SCHEMA: Schema = Schema {
    app: "rss",
    steps: &[
        Step::Sql(V1),
        Step::Run(add_source),
        Step::Derived {
            key: "rss:html",
            version: html::VERSION as i64,
            rebuild: rebuild_html,
        },
        Step::Run(feed_defaults),
        Step::Sql(SEEN),
        Step::Run(fill_seen),
    ],
};

/// What of a reader's decisions travels between their devices: what they
/// subscribe to, and what they have read.
///
/// A read mark is a fact about `(feed url, guid)` rather than about an
/// article row, because it can exist before the article does — the other
/// device read something this one has not fetched yet. Everything else on a
/// feed is this device's own fetch bookkeeping.
pub static REPLICATED: &[Replicated] = &[
    Replicated {
        table: "rss_feed",
        key: &["url"],
        columns: &["subscribed"],
    },
    Replicated {
        table: "rss_seen",
        key: &["feed_url", "guid"],
        columns: &["seen"],
    },
];

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

/// Keep the selected publisher content before narrowing, with the MIME type
/// and effective URL needed to reproduce its reading without another fetch.
/// NULL raw identifies entries cached by builds that discarded the source.
fn add_source(c: &Connection) -> rusqlite::Result<()> {
    for (name, sql) in [
        (
            "content_type",
            "ALTER TABLE rss_article ADD COLUMN content_type TEXT NOT NULL DEFAULT 'text/html'",
        ),
        (
            "base_url",
            "ALTER TABLE rss_article ADD COLUMN base_url TEXT NOT NULL DEFAULT ''",
        ),
        ("raw", "ALTER TABLE rss_article ADD COLUMN raw TEXT"),
    ] {
        // A migration interrupted between columns can resume on the next open.
        if !c
            .prepare("SELECT 1 FROM pragma_table_info('rss_article') WHERE name=?")?
            .exists([name])?
        {
            c.execute_batch(sql)?;
        }
    }
    // A conditional 304 cannot recover missing source. Request one full pass
    // for legacy feeds, including removed feeds if they are subscribed again.
    c.execute(
        "UPDATE rss_feed SET etag='',modified='',requested=requested+1
        WHERE EXISTS(SELECT 1 FROM rss_article WHERE feed=rss_feed.id AND raw IS NULL)",
        [],
    )?;
    Ok(())
}

/// Every cached article participates, including removed subscriptions and
/// entries no longer in a publisher's feed window. Legacy entries have only
/// the old HTML to narrow; do not pretend it is the original publisher body.
fn rebuild_html(c: &Connection) -> rusqlite::Result<()> {
    // Finish the table scan before updating it, keeping only IDs in memory.
    let ids = c
        .prepare("SELECT id FROM rss_article")?
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut query =
        c.prepare("SELECT raw,content_type,base_url,html FROM rss_article WHERE id=?")?;
    for id in ids {
        let narrowed = query.query_row([id], |row| {
            let raw: Option<String> = row.get(0)?;
            Ok(match raw {
                Some(raw) => super::parse::reading(
                    &raw,
                    &row.get::<_, String>(1)?,
                    &row.get::<_, String>(2)?,
                ),
                None => html::sanitize(&row.get::<_, String>(3)?),
            })
        })?;
        c.execute(
            "UPDATE rss_article SET html=?1 WHERE id=?2",
            params![narrowed, id],
        )?;
    }
    Ok(())
}

/// Read state, as a fact about a feed's url and an entry's guid.
/// `rss_article.seen` stays as the projection every list reads: writing a
/// mark carries it to the article, and an article that arrives afterwards
/// picks its mark up.
const SEEN: &str = "
CREATE TABLE rss_seen (
    feed_url TEXT NOT NULL,
    guid     TEXT NOT NULL,
    seen     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(feed_url, guid)
);
CREATE TRIGGER rss_seen_marks AFTER INSERT ON rss_seen BEGIN
    UPDATE rss_article SET seen = new.seen
     WHERE guid = new.guid AND feed = (SELECT id FROM rss_feed WHERE url = new.feed_url);
END;
CREATE TRIGGER rss_seen_remarks AFTER UPDATE OF seen ON rss_seen BEGIN
    UPDATE rss_article SET seen = new.seen
     WHERE guid = new.guid AND feed = (SELECT id FROM rss_feed WHERE url = new.feed_url);
END;
CREATE TRIGGER rss_article_marked AFTER INSERT ON rss_article
WHEN EXISTS (SELECT 1 FROM rss_seen s JOIN rss_feed f ON f.url = s.feed_url
              WHERE f.id = new.feed AND s.guid = new.guid)
BEGIN
    UPDATE rss_article
       SET seen = (SELECT s.seen FROM rss_seen s JOIN rss_feed f ON f.url = s.feed_url
                    WHERE f.id = new.feed AND s.guid = new.guid)
     WHERE id = new.id;
END;
";

/// What this device has already read, as marks of its own. Runs once; a
/// mark from another device arrives as an ordinary row.
fn fill_seen(c: &Connection) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO rss_seen(feed_url, guid, seen)
         SELECT f.url, a.guid, 1 FROM rss_article a JOIN rss_feed f ON f.id = a.feed
          WHERE a.seen = 1
         ON CONFLICT(feed_url, guid) DO NOTHING",
        [],
    )?;
    Ok(())
}

/// A subscription another device made arrives with its url and nothing
/// else, so every other column of `rss_feed` has to have a default —
/// including `title`, which had none. SQLite cannot alter a column's
/// default, so the table is rebuilt, which is safe to repeat: a store that
/// already has the default is left alone.
fn feed_defaults(c: &Connection) -> rusqlite::Result<()> {
    let done: i64 = c.query_row(
        "SELECT count(*) FROM pragma_table_info('rss_feed')
          WHERE name = 'title' AND dflt_value IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if done > 0 {
        return Ok(());
    }
    // `rss_article` names this table in a foreign key, which is why the
    // rebuild happens with the keys off: dropping the old table would
    // otherwise be refused, and the clause names `rss_feed` again the
    // moment the new one takes that name.
    c.execute_batch("PRAGMA foreign_keys=off")?;
    let rebuilt = c.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE rss_feed_rebuilt (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             url TEXT NOT NULL UNIQUE,
             title TEXT NOT NULL DEFAULT '',
             subscribed INTEGER NOT NULL DEFAULT 1,
             checked REAL,
             error TEXT NOT NULL DEFAULT '',
             etag TEXT NOT NULL DEFAULT '',
             modified TEXT NOT NULL DEFAULT '',
             requested INTEGER NOT NULL DEFAULT 0,
             completed INTEGER NOT NULL DEFAULT -1
         );
         INSERT INTO rss_feed_rebuilt(id, url, title, subscribed, checked, error, etag,
                                      modified, requested, completed)
              SELECT id, url, title, subscribed, checked, error, etag,
                     modified, requested, completed FROM rss_feed;
         DROP TABLE rss_feed;
         ALTER TABLE rss_feed_rebuilt RENAME TO rss_feed;
         COMMIT;",
    );
    c.execute_batch("PRAGMA foreign_keys=on")?;
    rebuilt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::rss::{model, parse};
    use kernel::store::Store;
    use kernel::sync::Device;

    static OLD: Schema = Schema {
        app: "rss",
        steps: &[Step::Sql(V1)],
    };

    /// The ladder is climbed at open, on a connection of its own — a rung
    /// that rebuilds a table needs the pragmas and the transaction an open
    /// has — so this walks the upgrade the way a device does: a store
    /// written by the old build, opened by this one.
    #[test]
    fn legacy_caches_are_narrowed_and_request_source_without_losing_state() {
        for interrupted in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "superapp-rss-legacy-{}-{interrupted}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("rss.db");
            {
                let store = Store::open(Some(&path), &[&OLD], Device::fake()).unwrap();
                store.write(move |c| {
                    c.execute_batch("INSERT INTO rss_feed(id,url,title,subscribed,etag,modified,requested,completed)
                        VALUES(1,'https://example.com/feed','Active',1,'old etag','old date',3,3),
                              (2,'https://example.com/removed','Removed',0,'old etag','old date',3,3);
                        INSERT INTO rss_article(id,feed,guid,title,url,author,published,html,seen)
                        VALUES(1,1,'one','One','','',100,'<p>kept</p><script>discard()</script>',1),
                              (2,2,'two','Two','','',200,'<p>kept</p><script>discard()</script>',0);")?;
                    // A migration interrupted between columns resumes on the
                    // next open.
                    if interrupted {
                        c.execute_batch("ALTER TABLE rss_article ADD COLUMN content_type TEXT NOT NULL DEFAULT 'text/html'")?;
                    }
                    Ok(())
                }).unwrap();
            }
            let store = Store::open(Some(&path), &[&SCHEMA], Device::fake()).unwrap();
            store.write(|c| {
                for (id, guid, published, seen) in [(1, "one", 100.0, true), (2, "two", 200.0, false)] {
                    let cached = c.query_row("SELECT guid,published,seen,html,raw,content_type,base_url FROM rss_article WHERE id=?", [id], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, bool>(2)?, r.get::<_, String>(3)?,
                            r.get::<_, Option<String>>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
                    })?;
                    assert_eq!(cached, (guid.into(), published, seen, "<p>kept</p>".into(), None, "text/html".into(), String::new()));
                    let feed = c.query_row("SELECT subscribed,etag,modified,requested,completed FROM rss_feed WHERE id=?", [id], |r| {
                        Ok((r.get::<_, bool>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
                    })?;
                    assert_eq!(feed, (id == 1, String::new(), String::new(), 4, 3));
                }
                let version: i64 = c.query_row("SELECT value FROM meta WHERE key='rss:html'", [], |r| r.get(0))?;
                assert_eq!(version, html::VERSION as i64);
                // What this device had read is a mark of its own now, under
                // the feed's url and the entry's guid.
                let marks: Vec<(String, String, bool)> = c
                    .prepare("SELECT feed_url,guid,seen FROM rss_seen ORDER BY guid")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                assert_eq!(marks, vec![("https://example.com/feed".to_string(), "one".to_string(), true)]);
                c.execute("UPDATE rss_feed SET etag='fresh etag' WHERE id=1", [])?;
                Ok(())
            }).unwrap();
            drop(store);

            // A normal open must not request the same recovery again.
            let store = Store::open(Some(&path), &[&SCHEMA], Device::fake()).unwrap();
            store.write(|c| {
                let state = c.query_row("SELECT etag,requested FROM rss_feed WHERE id=1", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
                assert_eq!(state, ("fresh etag".into(), 4));

                // A returning entry recovers its source, keeping its ID and read state.
                let feed = parse::parse(br#"<rss version="2.0"><channel><title>Active</title>
                    <item><guid>one</guid><description><![CDATA[<p>fresh</p>]]></description></item>
                    </channel></rss>"#, "https://example.com/feed").unwrap();
                model::ingest(c, 1, &feed, 999.0)?;
                let recovered = c.query_row("SELECT raw,base_url,published,seen FROM rss_article WHERE id=1", [], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, bool>(3)?))
                })?;
                assert_eq!(recovered, ("<p>fresh</p>".into(), "https://example.com/feed".into(), 100.0, true));
                Ok(())
            }).unwrap();
            drop(store);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A read mark can arrive before the article it is about: the row is
    /// written whatever this device has fetched, and the article picks it
    /// up when it lands.
    #[test]
    fn a_mark_that_arrives_first_is_picked_up_by_the_article() {
        let store = Store::open(None, &[&SCHEMA], Device::fake()).unwrap();
        store
            .write(|c| {
                c.execute_batch(
                    "INSERT INTO rss_feed(id,url,title) VALUES(1,'https://example.com/feed','Active');
                     INSERT INTO rss_seen(feed_url,guid,seen) VALUES('https://example.com/feed','later',1);",
                )?;
                // Nothing to mark yet, and nothing refused.
                c.execute(
                    "INSERT INTO rss_article(feed,guid,title,url,author,published,html)
                     VALUES(1,'later','Later','','',1,'')",
                    [],
                )?;
                let seen: bool = c.query_row("SELECT seen FROM rss_article WHERE guid='later'", [], |r| r.get(0))?;
                assert!(seen, "the article takes the mark that was waiting for it");
                Ok(())
            })
            .unwrap();
    }
}
