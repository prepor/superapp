//! What a page, a file and a revision are, read off the store — and the
//! few writes the prototype's panels make: a save with its revision, a
//! rename, a soft delete, a draft.

use std::rc::Rc;

use kernel::history::Intent;
use kernel::filter::Op;
use kernel::richtable::{Dir, SqlSource, SqlSpec, TagDef, TagSql, TagType, Values};
use kernel::session::{Action, Session};
use kernel::store::{Store, Val, Q};
use rusqlite::{params, Connection, OptionalExtension};

use super::markdown;

/// The caption a kind's rows sit under.
#[must_use]
pub fn caption(kind: &str) -> &'static str {
    match kind {
        "project" => "PROJECTS",
        "concept" => "CONCEPTS",
        "entity" => "ENTITIES",
        "source" => "SOURCES",
        "skill" => "SKILLS",
        "memory" => "MEMORY",
        "inbox" => "INBOX",
        _ => "OTHER",
    }
}

/// A kind's place in the catalogue, as SQL, so the page and the rank
/// queries order by the same expression.
macro_rules! kind_rank {
    () => {
        "CASE p.kind WHEN 'project' THEN 0 WHEN 'concept' THEN 1 WHEN 'entity' THEN 2 \
         WHEN 'source' THEN 3 WHEN 'skill' THEN 4 WHEN 'memory' THEN 5 WHEN 'inbox' THEN 6 ELSE 7 END"
    };
}

// -- the catalogue ---------------------------------------------------------------

/// One row of the catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRow {
    pub uid: String,
    pub slug: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub updated: f64,
    pub rank: i64,
}

/// A link whose target no page, alias or file answers to.
const DANGLING: &str = "EXISTS (SELECT 1 FROM kb_link l WHERE l.page = p.uid \
    AND l.target NOT IN (SELECT slug FROM kb_page WHERE deleted = 0) \
    AND l.target NOT IN (SELECT alias FROM kb_alias) \
    AND l.target NOT IN (SELECT path FROM kb_file WHERE deleted = 0))";

/// A page nothing links to, by slug or by alias.
const ORPHAN: &str = "NOT EXISTS (SELECT 1 FROM kb_link l JOIN kb_page q ON q.uid = l.page \
    WHERE q.deleted = 0 AND q.uid <> p.uid \
    AND (l.target = p.slug OR l.target IN (SELECT alias FROM kb_alias WHERE uid = p.uid)))";

pub static PAGES: SqlSource<PageRow, String> = SqlSource {
    spec: &SqlSpec {
        id: "kb pages",
        describe: "every page of the knowledge base, by kind",
        select: concat!("p.uid, p.slug, p.kind, p.title, p.summary, p.updated, ", kind_rank!(), " AS kind_rank"),
        from: "kb_page p",
        base: "p.deleted = 0",
        text: &["p.title", "p.summary", "p.slug", "p.body"],
        index: None,
        tags: &[
            ("kind", TagSql::Col("p.kind")),
            ("tag", TagSql::Col("p.tags")),
            ("orphan", TagSql::Where(ORPHAN)),
            ("dangling", TagSql::Where(DANGLING)),
            ("stale", TagSql::Where("p.updated < strftime('%s', 'now') - 7776000")),
            ("date", TagSql::Col("p.updated")),
        ],
        order: &[(kind_rank!(), Dir::Asc), ("p.title", Dir::Asc), ("p.slug", Dir::Asc)],
        group: None,
        key: "p.slug",
        deps: &[],
    },
    tags: &[
        TagDef {
            name: "kind",
            kind: TagType::Text,
            ops: &[Op::Eq],
            describe: "project, concept, entity, source, skill, memory, inbox",
            values: Values::Static(&[
                ("project", "project"),
                ("concept", "concept"),
                ("entity", "entity"),
                ("source", "source"),
                ("skill", "skill"),
                ("memory", "memory"),
                ("inbox", "inbox"),
            ]),
        },
        TagDef { name: "tag", kind: TagType::Text, ops: &[Op::Eq], describe: "a tag off the frontmatter", values: Values::None },
        TagDef { name: "orphan", kind: TagType::Bool, ops: &[], describe: "nothing links here", values: Values::None },
        TagDef { name: "dangling", kind: TagType::Bool, ops: &[], describe: "links to a page that is not there", values: Values::None },
        TagDef { name: "stale", kind: TagType::Bool, ops: &[], describe: "not written in ninety days", values: Values::None },
        TagDef { name: "date", kind: TagType::Date, ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte], describe: "when it was last written", values: Values::None },
    ],
    map: |r| {
        Ok(PageRow {
            uid: r.get(0)?,
            slug: r.get(1)?,
            kind: r.get(2)?,
            title: r.get(3)?,
            summary: r.get(4)?,
            updated: r.get(5)?,
            rank: r.get(6)?,
        })
    },
    key: |r| r.slug.clone(),
    rank: |r| vec![Val::I(r.rank), Val::S(r.title.clone()), Val::S(r.slug.clone())],
    suggest: |_, _, _| Vec::new(),
};

// -- one page --------------------------------------------------------------------

/// A page whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub uid: String,
    pub slug: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub extra: String,
    pub body: String,
    pub path: String,
    pub created: f64,
    pub updated: f64,
}

fn page_of(r: &rusqlite::Row) -> rusqlite::Result<Page> {
    let list = |s: String| serde_json::from_str::<Vec<String>>(&s).unwrap_or_default();
    Ok(Page {
        uid: r.get(0)?,
        slug: r.get(1)?,
        kind: r.get(2)?,
        title: r.get(3)?,
        summary: r.get(4)?,
        aliases: list(r.get(5)?),
        tags: list(r.get(6)?),
        extra: r.get(7)?,
        body: r.get(8)?,
        path: r.get(9)?,
        created: r.get(10)?,
        updated: r.get(11)?,
    })
}

static PAGE_BY_SLUG: Q = Q {
    id: "kb page",
    describe: "one page of the knowledge base, whole, by its slug",
    sql: "SELECT uid, slug, kind, title, summary, aliases, tags, extra, body, path, created, updated
            FROM kb_page WHERE deleted = 0 AND (slug = ?1 OR uid IN (SELECT uid FROM kb_alias WHERE alias = ?1))",
};

static PAGE_BY_UID: Q = Q {
    id: "kb page by uid",
    describe: "one page of the knowledge base, whole, by its uid",
    sql: "SELECT uid, slug, kind, title, summary, aliases, tags, extra, body, path, created, updated
            FROM kb_page WHERE deleted = 0 AND uid = ?1",
};

/// A page by its slug, or by an alias of it. Cached by the store.
#[must_use]
pub fn page(store: &Store, slug: &str) -> Option<Page> {
    store.rows(&PAGE_BY_SLUG, &[Val::S(slug.to_string())], page_of).first().cloned()
}

#[must_use]
pub fn page_by_uid(store: &Store, uid: &str) -> Option<Page> {
    store.rows(&PAGE_BY_UID, &[Val::S(uid.to_string())], page_of).first().cloned()
}

// -- links -----------------------------------------------------------------------

/// Where a link written in a body points, once resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Page { slug: String, title: String },
    File { path: String, hash: String },
    Dangling(String),
}

/// One link a page names, as written and as resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub kind: String,
    pub resolved: Target,
}

static SLUGS: Q = Q {
    id: "kb slugs",
    describe: "every page's slug and title",
    sql: "SELECT slug, title FROM kb_page WHERE deleted = 0",
};
static ALIASES: Q = Q {
    id: "kb aliases",
    describe: "every alias and the slug it names",
    sql: "SELECT a.alias, p.slug, p.title FROM kb_alias a JOIN kb_page p ON p.uid = a.uid WHERE p.deleted = 0",
};
static FILES: Q = Q {
    id: "kb files",
    describe: "every file's path and hash",
    sql: "SELECT path, hash FROM kb_file WHERE deleted = 0",
};
static LINKS_OF: Q = Q {
    id: "kb links",
    describe: "what this page names",
    sql: "SELECT target, kind FROM kb_link WHERE page = ?1 ORDER BY rowid",
};

/// Resolves what a page's bodies name, the way the reading and the
/// catalogue's filters both do: a slug, an alias (case-folded), a file's
/// path — else dangling.
pub struct Resolver {
    slugs: Rc<Vec<(String, String)>>,
    aliases: Rc<Vec<(String, String, String)>>,
    files: Rc<Vec<(String, String)>>,
}

impl Resolver {
    #[must_use]
    pub fn new(store: &Store) -> Resolver {
        Resolver {
            slugs: store.rows(&SLUGS, &[], |r| Ok((r.get(0)?, r.get(1)?))),
            aliases: store.rows(&ALIASES, &[], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))),
            files: store.rows(&FILES, &[], |r| Ok((r.get(0)?, r.get(1)?))),
        }
    }

    #[must_use]
    pub fn resolve(&self, target: &str) -> Target {
        let t = target.trim_end_matches(".md");
        if let Some((slug, title)) = self.slugs.iter().find(|(s, _)| s == t) {
            return Target::Page { slug: slug.clone(), title: title.clone() };
        }
        let folded = t.to_lowercase();
        if let Some((_, slug, title)) = self.aliases.iter().find(|(a, _, _)| a.to_lowercase() == folded) {
            return Target::Page { slug: slug.clone(), title: title.clone() };
        }
        let decoded = target.replace("%20", " ");
        if let Some((path, hash)) = self.files.iter().find(|(p, _)| *p == target || *p == decoded) {
            return Target::File { path: path.clone(), hash: hash.clone() };
        }
        Target::Dangling(target.to_string())
    }
}

/// What a page names, resolved.
#[must_use]
pub fn links(store: &Store, uid: &str) -> Vec<Link> {
    let resolver = Resolver::new(store);
    store
        .rows(&LINKS_OF, &[Val::S(uid.to_string())], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .iter()
        .map(|(target, kind)| Link { resolved: resolver.resolve(target), target: target.clone(), kind: kind.clone() })
        .collect()
}

static BACKLINKS: Q = Q {
    id: "kb backlinks",
    describe: "the pages that link here",
    sql: "SELECT p.slug, p.title FROM kb_link l JOIN kb_page p ON p.uid = l.page
           WHERE p.deleted = 0 AND p.uid <> ?2
             AND (l.target = ?1 OR l.target = ?1 || '.md'
                  OR l.target IN (SELECT alias FROM kb_alias WHERE uid = ?2))
           GROUP BY p.slug ORDER BY p.title",
};

/// The pages that name this one, by slug or alias.
#[must_use]
pub fn backlinks(store: &Store, page: &Page) -> Rc<Vec<(String, String)>> {
    store.rows(
        &BACKLINKS,
        &[Val::S(page.slug.clone()), Val::S(page.uid.clone())],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Rewrites a page's link rows from its body, in the transaction that
/// wrote the body.
pub fn relink(c: &Connection, uid: &str, body: &str, now: f64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM kb_link WHERE page = ?1", [uid])?;
    for (target, kind) in markdown::links(body) {
        c.execute(
            "INSERT INTO kb_link(page, target, kind, stamp) VALUES(?1, ?2, ?3, ?4)",
            params![uid, target, kind, now],
        )?;
    }
    Ok(())
}

/// Rewrites a page's alias rows from its frontmatter.
pub fn realias(c: &Connection, uid: &str, aliases: &[String]) -> rusqlite::Result<()> {
    c.execute("DELETE FROM kb_alias WHERE uid = ?1", [uid])?;
    for a in aliases {
        c.execute(
            "INSERT OR REPLACE INTO kb_alias(alias, uid) VALUES(?1, ?2)",
            params![a.to_lowercase(), uid],
        )?;
    }
    Ok(())
}

// -- files -----------------------------------------------------------------------

/// One file's row.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub path: String,
    pub hash: String,
    pub mime: String,
    pub size: u64,
    pub text: String,
    pub updated: f64,
}

/// Where a file's bytes are, as the prototype's `kb_cache` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Cached,
    Fetching,
    Outbox,
    Missing,
}

impl Where {
    #[must_use]
    pub fn of(word: &str) -> Where {
        match word {
            "cached" => Where::Cached,
            "fetching" => Where::Fetching,
            "outbox" => Where::Outbox,
            _ => Where::Missing,
        }
    }

    /// The card's muted line.
    #[must_use]
    pub fn line(self) -> &'static str {
        match self {
            Where::Cached => "in the bucket · cached",
            Where::Fetching => "fetching…",
            Where::Outbox => "in the outbox, not backed up yet",
            Where::Missing => "not here: fetch",
        }
    }

    /// Whether the bytes are on this device.
    #[must_use]
    pub fn here(self) -> bool {
        matches!(self, Where::Cached | Where::Outbox)
    }
}

static FILE_BY_PATH: Q = Q {
    id: "kb file",
    describe: "one file of the knowledge base: its path, hash, type and size",
    sql: "SELECT path, hash, mime, size, text, updated FROM kb_file WHERE deleted = 0 AND path = ?1",
};

#[must_use]
pub fn file(store: &Store, path: &str) -> Option<File> {
    store
        .rows(&FILE_BY_PATH, &[Val::S(path.to_string())], |r| {
            Ok(File {
                path: r.get(0)?,
                hash: r.get(1)?,
                mime: r.get(2)?,
                size: r.get::<_, i64>(3)?.max(0) as u64,
                text: r.get(4)?,
                updated: r.get(5)?,
            })
        })
        .first()
        .cloned()
}

static WHERE_IS: Q = Q {
    id: "kb cache",
    describe: "where a file's bytes are: cached, fetching, in the outbox, or not here",
    sql: "SELECT state FROM kb_cache WHERE hash = ?1",
};

#[must_use]
pub fn where_is(store: &Store, hash: &str) -> Where {
    store
        .rows(&WHERE_IS, &[Val::S(hash.to_string())], |r| r.get::<_, String>(0))
        .first()
        .map_or(Where::Missing, |w| Where::of(w))
}

pub fn set_where(store: &Store, hash: &str, state: Where) {
    let hash = hash.to_string();
    let word = match state {
        Where::Cached => "cached",
        Where::Fetching => "fetching",
        Where::Outbox => "outbox",
        Where::Missing => "missing",
    };
    let _ = store.write(move |c| {
        c.execute(
            "INSERT INTO kb_cache(hash, state) VALUES(?1, ?2) ON CONFLICT(hash) DO UPDATE SET state = excluded.state",
            params![hash, word],
        )?;
        Ok(())
    });
}

static NAMED_BY: Q = Q {
    id: "kb file pages",
    describe: "the pages that name this file",
    sql: "SELECT p.slug, p.title FROM kb_link l JOIN kb_page p ON p.uid = l.page
           WHERE p.deleted = 0 AND (l.target = ?1 OR replace(l.target, '%20', ' ') = ?1)
           GROUP BY p.slug ORDER BY p.title",
};

#[must_use]
pub fn named_by(store: &Store, path: &str) -> Rc<Vec<(String, String)>> {
    store.rows(&NAMED_BY, &[Val::S(path.to_string())], |r| Ok((r.get(0)?, r.get(1)?)))
}

// -- revisions -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Revision {
    pub uid: String,
    pub page: String,
    pub at: f64,
    pub device: String,
    pub author: String,
    pub chat_title: String,
    pub message: String,
}

impl Revision {
    /// The chat's id where the author is a chat.
    #[must_use]
    pub fn chat(&self) -> Option<i64> {
        self.author.strip_prefix("chat:")?.parse().ok()
    }

    /// The history's word for who wrote it.
    #[must_use]
    pub fn by(&self) -> String {
        match self.author.as_str() {
            "editor" => "editor".to_string(),
            "import" => "import".to_string(),
            a if a.starts_with("chat:") => format!("by the agent in \u{201c}{}\u{201d}", self.chat_title),
            other => other.to_string(),
        }
    }
}

fn revision_of(r: &rusqlite::Row) -> rusqlite::Result<Revision> {
    Ok(Revision {
        uid: r.get(0)?,
        page: r.get(1)?,
        at: r.get(2)?,
        device: r.get(3)?,
        author: r.get(4)?,
        chat_title: r.get(5)?,
        message: r.get(6)?,
    })
}

static REVISIONS: Q = Q {
    id: "kb revisions",
    describe: "every write of this page, newest first: when, on which device, by whom, and its message",
    sql: "SELECT uid, page, at, device, author, chat_title, message FROM kb_revision WHERE page = ?1 ORDER BY at DESC, uid",
};

#[must_use]
pub fn revisions(store: &Store, page_uid: &str) -> Rc<Vec<Revision>> {
    store.rows(&REVISIONS, &[Val::S(page_uid.to_string())], revision_of)
}

static REVISION: Q = Q {
    id: "kb revision",
    describe: "one revision of a page, with the document it left",
    sql: "SELECT uid, page, at, device, author, chat_title, message, body FROM kb_revision WHERE uid = ?1",
};

#[must_use]
pub fn revision(store: &Store, uid: &str) -> Option<(Revision, String)> {
    store
        .rows(&REVISION, &[Val::S(uid.to_string())], |r| Ok((revision_of(r)?, r.get::<_, String>(7)?)))
        .first()
        .cloned()
}

// -- drafts ----------------------------------------------------------------------

static DRAFT: Q = Q {
    id: "kb draft",
    describe: "the editor's unsaved text for this page",
    sql: "SELECT body FROM kb_draft WHERE page = ?1",
};

#[must_use]
pub fn draft(store: &Store, page: &str) -> Option<String> {
    store.rows(&DRAFT, &[Val::S(page.to_string())], |r| r.get::<_, String>(0)).first().cloned()
}

pub fn save_draft(store: &Store, page: &str, body: Option<String>, now: f64) -> Result<(), String> {
    let page = page.to_string();
    store
        .write(move |c| {
            match body {
                Some(body) => c.execute(
                    "INSERT INTO kb_draft(page, body, updated) VALUES(?1, ?2, ?3)
                     ON CONFLICT(page) DO UPDATE SET body = excluded.body, updated = excluded.updated",
                    params![page, body, now],
                )?,
                None => c.execute("DELETE FROM kb_draft WHERE page = ?1", [page])?,
            };
            Ok(())
        })
        .map_err(|e| e.to_string())
}

// -- writes ----------------------------------------------------------------------

/// Sixteen random bytes in hex: what names a page and a revision on every
/// device.
#[must_use]
pub fn new_uid(c: &Connection) -> String {
    c.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))
        .unwrap_or_else(|_| "0000".repeat(8))
}

/// The row as it stood, for an undo.
#[derive(Clone, Debug)]
struct Snapshot {
    slug: String,
    kind: String,
    title: String,
    summary: String,
    aliases: String,
    tags: String,
    extra: String,
    body: String,
    updated: f64,
    deleted: bool,
}

fn snapshot(c: &Connection, uid: &str) -> rusqlite::Result<Option<Snapshot>> {
    c.query_row(
        "SELECT slug, kind, title, summary, aliases, tags, extra, body, updated, deleted FROM kb_page WHERE uid = ?1",
        [uid],
        |r| {
            Ok(Snapshot {
                slug: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                summary: r.get(3)?,
                aliases: r.get(4)?,
                tags: r.get(5)?,
                extra: r.get(6)?,
                body: r.get(7)?,
                updated: r.get(8)?,
                deleted: r.get(9)?,
            })
        },
    )
    .optional()
}

fn put_snapshot(c: &Connection, uid: &str, s: &Snapshot) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO kb_page(uid, slug, kind, title, summary, aliases, tags, extra, body, created, updated, deleted)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)
         ON CONFLICT(uid) DO UPDATE SET slug = excluded.slug, kind = excluded.kind, title = excluded.title,
           summary = excluded.summary, aliases = excluded.aliases, tags = excluded.tags, extra = excluded.extra,
           body = excluded.body, updated = excluded.updated, deleted = excluded.deleted",
        params![uid, s.slug, s.kind, s.title, s.summary, s.aliases, s.tags, s.extra, s.body, s.updated, s.deleted],
    )?;
    let aliases: Vec<String> = serde_json::from_str(&s.aliases).unwrap_or_default();
    realias(c, uid, &aliases)?;
    relink(c, uid, &s.body, s.updated)
}

/// One write of a page, as history holds it: the row before and after, and
/// the revision it filed.
struct Wrote {
    uid: String,
    before: Option<Snapshot>,
    after: Snapshot,
    revision: String,
    what: String,
}

impl Intent for Wrote {
    fn describe(&self) -> String {
        self.what.clone()
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        let (uid, before, revision) = (self.uid.clone(), self.before.clone(), self.revision.clone());
        w.store()
            .write(move |c| {
                match &before {
                    Some(s) => put_snapshot(c, &uid, s)?,
                    None => {
                        c.execute("DELETE FROM kb_page WHERE uid = ?1", [&uid])?;
                        c.execute("DELETE FROM kb_link WHERE page = ?1", [&uid])?;
                        c.execute("DELETE FROM kb_alias WHERE uid = ?1", [&uid])?;
                    }
                }
                c.execute("DELETE FROM kb_revision WHERE uid = ?1", [&revision])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        let (uid, after) = (self.uid.clone(), self.after.clone());
        w.store().write(move |c| put_snapshot(c, &uid, &after)).map_err(|e| e.to_string())
    }
}

/// Who wrote a revision, as `kb_revision.author` spells it. The editor is
/// the one author the prototype's panels are; the import and the chats
/// come with their phases.
#[derive(Clone, Debug)]
pub enum Author {
    Editor,
}

impl Author {
    fn word(&self) -> String {
        match self {
            Author::Editor => "editor".into(),
        }
    }
    fn chat_title(&self) -> String {
        String::new()
    }
}

/// Files a document as the page's next revision, in one transaction: the
/// row, the revision, the links and aliases re-read, the draft gone.
/// Answers the page's uid.
fn write_document(
    c: &Connection,
    uid: Option<&str>,
    document: &str,
    author: &Author,
    message: &str,
    device: &str,
    now: f64,
) -> rusqlite::Result<(String, Option<Snapshot>, Snapshot, String)> {
    let (front, body) = markdown::parse_document(document);
    let uid = uid.map_or_else(|| new_uid(c), str::to_string);
    let before = snapshot(c, &uid)?;
    let slug = if front.slug.is_empty() {
        before.as_ref().map_or_else(|| markdown::slug_of(&front.title), |s| s.slug.clone())
    } else {
        front.slug.clone()
    };
    let after = Snapshot {
        slug,
        kind: if front.kind.is_empty() { "concept".into() } else { front.kind.clone() },
        title: front.title.clone(),
        summary: front.summary.clone(),
        aliases: serde_json::to_string(&front.aliases).unwrap_or_else(|_| "[]".into()),
        tags: serde_json::to_string(&front.tags).unwrap_or_else(|_| "[]".into()),
        extra: front.extra.clone(),
        body: body.clone(),
        updated: now,
        deleted: false,
    };
    put_snapshot(c, &uid, &after)?;
    let revision = new_uid(c);
    c.execute(
        "INSERT INTO kb_revision(uid, page, at, device, author, chat_title, message, body) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![revision, uid, now, device, author.word(), author.chat_title(), message, document],
    )?;
    c.execute("DELETE FROM kb_draft WHERE page = ?1", [&uid])?;
    Ok((uid, before, after, revision))
}

/// The editor's save, and the revision's restore: one undoable action.
/// Answers the page's uid, so a new page's editor can point at it.
pub fn save(s: &mut Session, uid: Option<String>, document: String, message: String) -> Option<String> {
    let now = s.now();
    let what = match &uid {
        Some(_) => "write a page",
        None => "new page",
    };
    let label = message.clone();
    let (page, before, after, revision) = s.act(Action::writing("kb.write", label, move |c| {
        write_document(c, uid.as_deref(), &document, &Author::Editor, &message, "this device", now)
    }))?;
    s.claim(Box::new(Wrote {
        uid: page.clone(),
        before,
        after,
        revision,
        what: what.into(),
    }));
    Some(page)
}

/// The rows' `deleted` bit, as it stood and as it stands.
struct Deleted {
    uids: Vec<String>,
    before: bool,
    after: bool,
}

impl Deleted {
    fn set(&self, w: &kernel::effect::World, value: bool) -> Result<(), String> {
        let uids = self.uids.clone();
        w.store()
            .write(move |c| {
                for uid in uids {
                    c.execute("UPDATE kb_page SET deleted = ?1 WHERE uid = ?2", params![value, uid])?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

impl Intent for Deleted {
    fn describe(&self) -> String {
        "page visibility".into()
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.set(w, self.before)
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.set(w, self.after)
    }
}

/// Soft-deletes pages by slug, undoably.
pub fn delete(s: &mut Session, slugs: Vec<String>) -> bool {
    if slugs.is_empty() {
        return false;
    }
    let n = slugs.len();
    let label = if n == 1 { format!("delete {}", slugs[0]) } else { format!("delete {n} pages") };
    let uids = s.act(Action::writing("kb.delete", label, move |c| {
        let mut uids = Vec::new();
        for slug in slugs {
            let uid: Option<String> = c
                .query_row("SELECT uid FROM kb_page WHERE slug = ?1 AND deleted = 0", [&slug], |r| r.get(0))
                .optional()?;
            if let Some(uid) = uid {
                c.execute("UPDATE kb_page SET deleted = 1 WHERE uid = ?1", [&uid])?;
                uids.push(uid);
            }
        }
        Ok(uids)
    }));
    match uids {
        Some(uids) if !uids.is_empty() => {
            s.claim(Box::new(Deleted { uids, before: false, after: true }));
            true
        }
        _ => false,
    }
}

/// A rename: the slug and the title, and every inbound link rewritten.
struct Renamed {
    uid: String,
    from: (String, String),
    to: (String, String),
}

impl Renamed {
    fn apply(&self, w: &kernel::effect::World, from: &(String, String), to: &(String, String)) -> Result<(), String> {
        let (uid, from, to) = (self.uid.clone(), from.clone(), to.clone());
        w.store().write(move |c| rename_tx(c, &uid, &from.0, &to.0, &to.1)).map_err(|e| e.to_string())
    }
}

impl Intent for Renamed {
    fn describe(&self) -> String {
        format!("rename {} to {}", self.from.0, self.to.0)
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.apply(w, &self.to, &self.from)
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.apply(w, &self.from, &self.to)
    }
}

fn rename_tx(c: &Connection, uid: &str, old: &str, slug: &str, title: &str) -> rusqlite::Result<()> {
    c.execute("UPDATE kb_page SET slug = ?2, title = ?3 WHERE uid = ?1", params![uid, slug, title])?;
    c.execute("UPDATE kb_link SET target = ?2 WHERE target = ?1", params![old, slug])?;
    c.execute(
        "UPDATE kb_link SET target = ?2 || '.md' WHERE target = ?1 || '.md'",
        params![old, slug],
    )?;
    // The bodies that spelled the old slug say the new one.
    let mut stmt = c.prepare("SELECT page FROM kb_link WHERE target = ?1 OR target = ?1 || '.md'")?;
    let pages: Vec<String> = stmt.query_map([slug], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for p in pages {
        let body: String = c.query_row("SELECT body FROM kb_page WHERE uid = ?1", [&p], |r| r.get(0))?;
        let renamed = markdown::rename_links(&body, old, slug);
        if renamed != body {
            c.execute("UPDATE kb_page SET body = ?2 WHERE uid = ?1", params![p, renamed])?;
        }
    }
    Ok(())
}

/// Renames a page: a new slug — the title's kebab form — and every inbound
/// link rewritten in the same undoable write.
pub fn rename(s: &mut Session, uid: &str, title: &str) -> Result<String, String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("a page needs a title".into());
    }
    let slug = markdown::slug_of(&title);
    let Some(page) = page_by_uid(s.store(), uid) else {
        return Err("this page is gone".into());
    };
    if slug != page.slug && page_exists(s.store(), &slug) {
        return Err(format!("a page is already called {slug}"));
    }
    let from = (page.slug.clone(), page.title.clone());
    let to = (slug.clone(), title.clone());
    let (u, f, t) = (uid.to_string(), from.clone(), to.clone());
    let done = s.act(Action::writing("kb.rename", format!("rename {} to {slug}", page.slug), move |c| {
        rename_tx(c, &u, &f.0, &t.0, &t.1)
    }));
    if done.is_none() {
        return Err("the store refused the rename".into());
    }
    s.claim(Box::new(Renamed { uid: uid.to_string(), from, to }));
    Ok(slug)
}

fn page_exists(store: &Store, slug: &str) -> bool {
    store
        .conn()
        .query_row("SELECT 1 FROM kb_page WHERE slug = ?1", [slug], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}
