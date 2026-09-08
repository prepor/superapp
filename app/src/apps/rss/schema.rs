use kernel::app::{Schema, Step};
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
    ],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::rss::{model, parse};
    use kernel::store::Store;

    static OLD: Schema = Schema {
        app: "rss",
        steps: &[Step::Sql(V1)],
    };

    #[test]
    fn legacy_caches_are_narrowed_and_request_source_without_losing_state() {
        for interrupted in [false, true] {
            let store = Store::open(None, &[&OLD]).unwrap();
            store.write(move |c| {
                c.execute_batch("INSERT INTO rss_feed(id,url,title,subscribed,etag,modified,requested,completed)
                    VALUES(1,'https://example.com/feed','Active',1,'old etag','old date',3,3),
                          (2,'https://example.com/removed','Removed',0,'old etag','old date',3,3);
                    INSERT INTO rss_article(id,feed,guid,title,url,author,published,html,seen)
                    VALUES(1,1,'one','One','','',100,'<p>kept</p><script>discard()</script>',1),
                          (2,2,'two','Two','','',200,'<p>kept</p><script>discard()</script>',0);")?;
                if interrupted {
                    c.execute_batch("ALTER TABLE rss_article ADD COLUMN content_type TEXT NOT NULL DEFAULT 'text/html'")?;
                }
                SCHEMA.apply(c)?;
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

                // A normal open must not request the same recovery again.
                c.execute("UPDATE rss_feed SET etag='fresh etag' WHERE id=1", [])?;
                SCHEMA.apply(c)?;
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
        }
    }
}
