//! Subscriptions and cached articles. Refresh never overwrites seen state.

use kernel::effect::World;
use kernel::filter::Op;
use kernel::history::Intent;
use kernel::richtable::{Dir, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, Values};
use kernel::session::{Edit, Session};
use kernel::store::{Store, Val};
use rusqlite::{params, Connection, OptionalExtension};

use super::{opml, parse};

#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub unseen: i64,
    pub checked: Option<f64>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Article {
    pub id: i64,
    pub feed: i64,
    pub feed_title: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub published: f64,
    pub seen: bool,
}

pub static FEEDS: SqlSource<Feed, i64> = SqlSource {
    spec: &SqlSpec {
        id: "rss feeds", describe: "subscribed feeds with their unseen counts and refresh status",
        select: "f.id, f.url, f.title, (SELECT COUNT(*) FROM rss_article a WHERE a.feed=f.id AND a.seen=0), f.checked, f.error",
        from: "rss_feed f", base: "f.subscribed=1", text: &["f.title", "f.url"], index: None,
        tags: &[("failed", TagSql::Where("f.error <> ''"))],
        order: &[("f.title", Dir::Asc), ("f.id", Dir::Asc)], group: None, key: "f.id", deps: &[],
    },
    tags: &[TagDef { name: "failed", kind: TagType::Bool, ops: &[], describe: "last refresh failed", values: Values::None }],
    map: |r| Ok(Feed { id:r.get(0)?, url:r.get(1)?, title:r.get(2)?, unseen:r.get(3)?, checked:r.get(4)?, error:r.get(5)? }),
    key: |r| r.id, rank: |r| vec![Val::S(r.title.clone()), Val::I(r.id)], suggest: |_,_,_| Vec::new(),
};

pub static ARTICLES: SqlSource<Article, i64> = SqlSource {
    spec: &SqlSpec {
        id: "rss articles",
        describe: "articles from subscribed feeds, oldest first",
        select: "a.id, a.feed, f.title, a.title, a.url, a.author, a.published, a.seen",
        from: "rss_article a JOIN rss_feed f ON f.id=a.feed",
        base: "f.subscribed=1",
        text: &["a.title", "a.author", "f.title"],
        index: None,
        tags: &[
            ("unseen", TagSql::Where("a.seen=0")),
            ("seen", TagSql::Where("a.seen=1")),
            ("feed", TagSql::Col("f.title")),
            ("feed_id", TagSql::Col("a.feed")),
            ("author", TagSql::Col("a.author")),
            ("date", TagSql::Col("a.published")),
        ],
        order: &[("a.published", Dir::Asc), ("a.id", Dir::Asc)],
        group: None,
        key: "a.id",
        deps: &[],
    },
    tags: &[
        TagDef {
            name: "unseen",
            kind: TagType::Bool,
            ops: &[],
            describe: "not yet opened",
            values: Values::None,
        },
        TagDef {
            name: "seen",
            kind: TagType::Bool,
            ops: &[],
            describe: "already read",
            values: Values::None,
        },
        TagDef {
            name: "feed",
            kind: TagType::Text,
            ops: &[Op::Eq],
            describe: "feed title",
            values: Values::Dynamic,
        },
        TagDef {
            name: "feed_id",
            kind: TagType::Number,
            ops: &[Op::Eq],
            describe: "one subscription by id",
            values: Values::None,
        },
        TagDef {
            name: "author",
            kind: TagType::Text,
            ops: &[Op::Eq],
            describe: "article author",
            values: Values::Dynamic,
        },
        TagDef {
            name: "date",
            kind: TagType::Date,
            ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte],
            describe: "publication date",
            values: Values::None,
        },
    ],
    map: |r| {
        Ok(Article {
            id: r.get(0)?,
            feed: r.get(1)?,
            feed_title: r.get(2)?,
            title: r.get(3)?,
            url: r.get(4)?,
            author: r.get(5)?,
            published: r.get(6)?,
            seen: r.get(7)?,
        })
    },
    key: |r| r.id,
    rank: |r| vec![Val::F(r.published), Val::I(r.id)],
    suggest,
};

fn suggest(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    let sql = match tag {
        "feed" => "SELECT DISTINCT title FROM rss_feed WHERE subscribed=1 ORDER BY title",
        "author" => "SELECT DISTINCT a.author FROM rss_article a JOIN rss_feed f ON f.id=a.feed WHERE f.subscribed=1 AND a.author<>'' ORDER BY a.author",
        _ => return Vec::new(),
    };
    store
        .snapshot_rows_sql(
            "rss suggestions",
            "feed titles and authors",
            sql,
            &[],
            |r| r.get::<_, String>(0),
        )
        .iter()
        .filter(|s| s.to_lowercase().contains(&typed.to_lowercase()))
        .map(|s| Suggestion::value(s.clone()))
        .collect()
}

#[cfg(test)]
pub fn article(store: &Store, id: i64) -> Option<Article> {
    use kernel::richtable::Datasource;
    ARTICLES.by_key(store, &id)
}

pub fn article_snapshot(store: &Store, id: i64) -> Option<Article> {
    let query = ARTICLES.spec.by_key();
    store.snapshot_rows_sql(ARTICLES.spec.id, ARTICLES.spec.describe, &query.sql,
        &[Val::I(id)], ARTICLES.map).first().cloned()
}

pub fn body_snapshot(store: &Store, id: i64) -> std::rc::Rc<Vec<String>> {
    store.snapshot_rows_sql("rss body", "the article's stored HTML reading",
        "SELECT html FROM rss_article WHERE id=?", &[Val::I(id)], |r| r.get(0))
}

#[cfg(test)]
pub fn body(store: &Store, id: i64) -> String {
    store
        .rows_sql(
            "rss body",
            "the article's stored HTML reading",
            "SELECT html FROM rss_article WHERE id=?",
            &[Val::I(id)],
            |r| r.get::<_, String>(0),
        )
        .first()
        .cloned()
        .unwrap_or_default()
}

/// A subscription is retained when removed so undo and re-adding keep its
/// cached articles and read state. Removed feeds are absent from all lists.
pub fn add_async(s: &mut Session, raw: &str,
    complete: impl FnOnce(&mut Session, Result<i64, String>) + 'static) {
    let url = match parse::feed_url(raw) {
        Ok(url) => url,
        Err(error) => { complete(s, Err(error)); return; }
    };
    let title = initial_title(&url);
    s.act_async_result(Edit::writing("rss.add", format!("subscribe to {title}"), move |tx| {
        let active: Option<bool> = tx.query_row("SELECT subscribed FROM rss_feed WHERE url=?", [&url], |row| row.get(0)).optional()?;
        if active == Some(true) {
            return Err(rusqlite::Error::InvalidParameterName("already subscribed to this feed".into()));
        }
        tx.execute("INSERT INTO rss_feed(url,title) VALUES(?1,?2) ON CONFLICT(url) DO UPDATE SET subscribed=1,requested=requested+1", params![url,title])?;
        tx.query_row("SELECT id FROM rss_feed WHERE url=?", [url], |row| row.get::<_, i64>(0))
    }).claiming_with(|id| vec![Box::new(Flags { kind: Flag::Subscribed, before: vec![(*id, false)], after: true })]),
        move |session, result| complete(session, result.map_err(|error| error.to_string())));
}

#[cfg(test)]
pub fn add(s: &mut Session, raw: &str) -> Result<i64, String> {
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = result.clone();
    add_async(s, raw, move |_, value| *capture.borrow_mut() = Some(value));
    let value = result.borrow_mut().take().expect("inline fixture");
    value
}

pub(super) fn initial_title(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.into())
}

#[derive(Debug, PartialEq)]
pub struct Imported {
    pub added: usize,
    pub existing: usize,
    pub skipped: usize,
}

impl Imported {
    pub fn summary(&self) -> String {
        format!(
            "{} feeds imported · {} already subscribed · {} skipped",
            self.added, self.existing, self.skipped
        )
    }
}

/// One transaction checks existing subscriptions and imports the entire list.
/// Its returned IDs are exactly the rows the history entry may undo.
fn import_rows(
    doc: opml::Document,
) -> impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<(Imported, Vec<i64>)> + Send {
    move |c| {
        let mut result = Imported {
            added: 0,
            existing: 0,
            skipped: doc.skipped,
        };
        let mut ids = Vec::new();
        let mut insert = c.prepare_cached(
            "INSERT INTO rss_feed(url,title) VALUES(?1,?2) ON CONFLICT(url) DO UPDATE SET subscribed=1,requested=requested+1 WHERE subscribed=0 RETURNING id"
        )?;
        for feed in doc.feeds {
            let id = insert
                .query_row(params![feed.url, feed.title], |r| r.get::<_, i64>(0))
                .optional()?;
            if let Some(id) = id {
                ids.push(id);
            } else {
                result.existing += 1;
            }
        }
        result.added = ids.len();
        Ok((result, ids))
    }
}

fn imported(s: &mut Session, result: Option<(Imported, Vec<i64>)>) -> Result<Imported, String> {
    let (result, ids) = result.ok_or("could not import feeds")?;
    if !ids.is_empty() {
        s.claim(Box::new(Flags {
            kind: Flag::Subscribed,
            before: ids.into_iter().map(|id| (id, false)).collect(),
            after: true,
        }));
    }
    Ok(result)
}

pub(super) fn import(s: &mut Session, doc: opml::Document) -> Result<Imported, String> {
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let completed = result.clone();
    import_async(s, doc, move |_, value| {
        *completed.borrow_mut() = Some(value);
    });
    result
        .take()
        .expect("inline import completes before returning")
}

pub(super) fn import_async(
    s: &mut Session,
    doc: opml::Document,
    done: impl FnOnce(&mut Session, Result<Imported, String>) + 'static,
) {
    s.act_async(
        kernel::session::Edit::writing(
            "rss.import_opml",
            format!("import {} feeds", doc.feeds.len()),
            import_rows(doc),
        )
        .record_if(|result| result.0.added > 0),
        move |s, result| {
            let result = imported(s, result);
            done(s, result);
        },
    );
}

#[derive(Clone, Copy)]
pub enum Flag {
    Subscribed,
    Seen,
}

impl Flag {
    fn table(self) -> (&'static str, &'static str) {
        match self {
            Self::Subscribed => ("rss_feed", "subscribed"),
            Self::Seen => ("rss_article", "seen"),
        }
    }
}

pub struct Flags {
    pub kind: Flag,
    pub before: Vec<(i64, bool)>,
    pub after: bool,
}

impl Flags {
    pub(super) fn try_of_conn(conn: &Connection, kind: Flag, ids: &[i64], after: bool) -> rusqlite::Result<Self> {
        let (table, col) = kind.table();
        let mut before = Vec::new();
        let mut query = conn.prepare(&format!("SELECT {col} FROM {table} WHERE id=?"))?;
        for id in ids {
            if let Some(value) = query.query_row([id], |row| row.get::<_, bool>(0)).optional()? {
                if value != after { before.push((*id, value)); }
            }
        }
        Ok(Self { kind, before, after })
    }
    pub fn write(&self) -> kernel::session::Write {
        let (kind, rows, after) = (self.kind, self.before.clone(), self.after);
        Box::new(move |c| {
            for (id, _) in rows {
                flag_tx(c, kind, id, after)?;
            }
            Ok(())
        })
    }
}

fn flag_tx(c: &Connection, kind: Flag, id: i64, value: bool) -> rusqlite::Result<()> {
    let (table, col) = kind.table();
    c.execute(
        &format!("UPDATE {table} SET {col}=?1 WHERE id=?2"),
        params![value, id],
    )?;
    Ok(())
}

impl Intent for Flags {
    fn describe(&self) -> String {
        format!("{} RSS rows changed", self.before.len())
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (kind, rows) = (self.kind, self.before.clone());
        w.store()
            .write(move |c| {
                for (id, value) in rows {
                    flag_tx(c, kind, id, value)?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (kind, rows, after) = (self.kind, self.before.clone(), self.after);
        w.store()
            .write(move |c| {
                for (id, _) in rows {
                    flag_tx(c, kind, id, after)?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

pub fn change_async(s: &mut Session, kind: Flag, ids: &[i64], after: bool,
    complete: impl FnOnce(&mut Session, bool) + 'static) {
    let ids = ids.to_vec();
    let word = match kind {
        Flag::Subscribed => "remove feeds",
        Flag::Seen if after => "mark seen",
        Flag::Seen => "mark unseen",
    };
    s.act_async(Edit::writing("rss.change", word, move |tx| {
        let flags = Flags::try_of_conn(tx, kind, &ids, after)?;
        (flags.write())(tx)?;
        Ok((!flags.before.is_empty(), Some(flags)))
    }).record_if(|result| result.0).claiming_with(|result| vec![Box::new(result.1.take().expect("one claim"))]),
        move |session, result| complete(session, result.is_some_and(|result| result.0)));
}

#[cfg(test)]
pub fn change(s: &mut Session, kind: Flag, ids: &[i64], after: bool) -> bool {
    let result = std::rc::Rc::new(std::cell::Cell::new(false));
    let capture = result.clone();
    change_async(s, kind, ids, after, move |_, changed| capture.set(changed));
    result.get()
}

/// Keep entries missing from later feed windows. Repeated GUIDs update the
/// reading in place while retaining the original order and seen flag.
pub fn ingest(c: &Connection, id: i64, feed: &parse::Feed, now: f64) -> rusqlite::Result<()> {
    let active = c
        .query_row("SELECT subscribed FROM rss_feed WHERE id=?", [id], |r| {
            r.get::<_, bool>(0)
        })
        .optional()?
        .unwrap_or(false);
    if !active {
        return Ok(());
    }
    c.execute(
        "UPDATE rss_feed SET title=?1 WHERE id=?2",
        params![feed.title, id],
    )?;
    for a in &feed.articles {
        c.execute("INSERT INTO rss_article(feed,guid,title,url,author,published,html,raw,content_type,base_url) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
            ON CONFLICT(feed,guid) DO UPDATE SET title=excluded.title,url=excluded.url,author=excluded.author,html=excluded.html,raw=excluded.raw,content_type=excluded.content_type,base_url=excluded.base_url",
            params![id,a.guid,a.title,a.url,a.author,a.published.unwrap_or(now),a.html,a.raw,a.content_type,a.base_url])?;
    }
    Ok(())
}

pub fn refresh(s: &mut Session) {
    if !s.writable() {
        s.notify("this device is read-only", true);
        return;
    }
    s.act_async(Edit::writing("rss.refresh", "refresh feeds", |tx| {
        tx.execute("UPDATE rss_feed SET requested=requested+1 WHERE subscribed=1", [])
    }).record_if(|_| false).wake_if(|changed| *changed > 0), |session, result| {
        if result.is_some() { session.notify("refreshing feeds", false); }
    });
}
