//! What a page, a file and a revision are, read off the store — and the
//! few writes the prototype's panels make: a save with its revision, a
//! rename, a soft delete, a draft.
//!
//! One resolver says what a link names. It reads the live pages, their
//! aliases and the files, and it is the same function whether the reader
//! is drawing a body, a write is deriving the `kb_link` rows, or the
//! catalogue is filtering — the rows carry what it answered in their
//! `resolved` column, so the filters and the backlinks are one `WHERE`
//! over the same answer the reading drew.
//!
//! Every write of a page appends a revision and never removes one: an
//! undo is a compensating write, so the history keeps growing as the CR's
//! append-only log says it must.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

use kernel::filter::Op;
use kernel::history::Intent;
use kernel::richtable::{Dir, SqlSource, SqlSpec, TagDef, TagSql, TagType, Values};
use kernel::session::{Action, Session};
use kernel::store::{Store, Val, Q};
use rusqlite::{params, Connection, OptionalExtension};

use super::markdown::{self, Front, LinkKind};

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

/// A page with a link the resolver could not answer.
const DANGLING: &str = "EXISTS (SELECT 1 FROM kb_link l WHERE l.page = p.uid AND l.resolved = '')";

/// A page no other live page's link resolves to.
const ORPHAN: &str = "NOT EXISTS (SELECT 1 FROM kb_link l JOIN kb_page q ON q.uid = l.page \
    WHERE q.deleted = 0 AND q.uid <> p.uid AND l.resolved = 'page:' || p.uid)";

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

impl Page {
    /// The frontmatter this row spells.
    #[must_use]
    pub fn front(&self) -> Front {
        Front {
            kind: self.kind.clone(),
            title: self.title.clone(),
            summary: self.summary.clone(),
            slug: String::new(),
            aliases: self.aliases.clone(),
            tags: self.tags.clone(),
            extra: self.extra.clone(),
        }
    }

    /// The whole document: the block and the body.
    #[must_use]
    pub fn document(&self) -> String {
        markdown::document(&self.front(), &self.body)
    }
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
    describe: "one page of the knowledge base, whole, by its slug or an alias",
    sql: "SELECT uid, slug, kind, title, summary, aliases, tags, extra, body, path, created, updated
            FROM kb_page WHERE deleted = 0 AND (lower(slug) = lower(?1) OR uid IN (SELECT uid FROM kb_alias WHERE alias = lower(?1)))",
};

static PAGE_BY_UID: Q = Q {
    id: "kb page by uid",
    describe: "one page of the knowledge base, whole, by its uid",
    sql: "SELECT uid, slug, kind, title, summary, aliases, tags, extra, body, path, created, updated
            FROM kb_page WHERE deleted = 0 AND uid = ?1",
};

/// A page by its slug, or by an alias of it, case-folded. Cached by the
/// store.
#[must_use]
pub fn page(store: &Store, slug: &str) -> Option<Page> {
    store.rows(&PAGE_BY_SLUG, &[Val::S(slug.to_string())], page_of).first().cloned()
}

#[must_use]
pub fn page_by_uid(store: &Store, uid: &str) -> Option<Page> {
    store.rows(&PAGE_BY_UID, &[Val::S(uid.to_string())], page_of).first().cloned()
}

// -- the resolver ----------------------------------------------------------------

/// Where a link written in a body points, once resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Page { uid: String, slug: String, title: String },
    File { path: String, hash: String },
    Dangling(String),
}

impl Target {
    /// What the `kb_link.resolved` column holds for this answer.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Target::Page { uid, .. } => format!("page:{uid}"),
            Target::File { path, .. } => format!("file:{path}"),
            Target::Dangling(_) => String::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct PageRef {
    uid: String,
    slug: String,
    title: String,
    path: String,
}

static SLUGS: Q = Q {
    id: "kb slugs",
    describe: "every live page's uid, slug, title and imported path",
    sql: "SELECT uid, slug, title, path FROM kb_page WHERE deleted = 0",
};
static ALIASES: Q = Q {
    id: "kb aliases",
    describe: "every alias of a live page, case-folded, and the page's uid",
    sql: "SELECT a.alias, a.uid FROM kb_alias a JOIN kb_page p ON p.uid = a.uid WHERE p.deleted = 0",
};
static FILES: Q = Q {
    id: "kb files",
    describe: "every live file's path and hash",
    sql: "SELECT path, hash FROM kb_file WHERE deleted = 0",
};

/// What a link names — the one function the reader, the link rows, the
/// backlinks and the catalogue's filters all read.
///
/// A target is tried as written first: a slug, an alias (case-folded), a
/// page's imported path, a file's path. Then with `%20` read as a space —
/// never `%2F`, which is a literal in the folder's filenames — and last a
/// wikilink's word against a file's stem. Deleted pages, and the aliases
/// of deleted pages, answer nothing.
pub struct Resolver {
    pages: Vec<PageRef>,
    aliases: Vec<(String, String)>,
    files: Vec<(String, String)>,
}

impl Resolver {
    /// Off the store's cached queries: what a panel builds on a draw.
    #[must_use]
    pub fn new(store: &Store) -> Resolver {
        Resolver {
            pages: store
                .rows(&SLUGS, &[], |r| Ok(PageRef { uid: r.get(0)?, slug: r.get(1)?, title: r.get(2)?, path: r.get(3)? }))
                .iter()
                .cloned()
                .collect(),
            aliases: store.rows(&ALIASES, &[], |r| Ok((r.get(0)?, r.get(1)?))).iter().cloned().collect(),
            files: store.rows(&FILES, &[], |r| Ok((r.get(0)?, r.get(1)?))).iter().cloned().collect(),
        }
    }

    /// Inside a write's transaction: what the link rows are derived with.
    pub fn conn(c: &Connection) -> rusqlite::Result<Resolver> {
        let pages = c
            .prepare(SLUGS.sql)?
            .query_map([], |r| Ok(PageRef { uid: r.get(0)?, slug: r.get(1)?, title: r.get(2)?, path: r.get(3)? }))?
            .collect::<rusqlite::Result<_>>()?;
        let aliases = c
            .prepare(ALIASES.sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let files = c
            .prepare(FILES.sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Resolver { pages, aliases, files })
    }

    fn page(&self, p: &PageRef) -> Target {
        Target::Page { uid: p.uid.clone(), slug: p.slug.clone(), title: p.title.clone() }
    }

    fn by_word(&self, word: &str) -> Option<Target> {
        // A slug is its lower-case form: `[[Berlin]]` names `berlin`, as
        // the page lookup and the name check read it.
        let folded = word.to_lowercase();
        if let Some(p) = self.pages.iter().find(|p| p.slug.to_lowercase() == folded) {
            return Some(self.page(p));
        }
        if let Some((_, uid)) = self.aliases.iter().find(|(a, _)| *a == folded) {
            if let Some(p) = self.pages.iter().find(|p| p.uid == *uid) {
                return Some(self.page(p));
            }
        }
        None
    }

    fn by_path(&self, path: &str) -> Option<Target> {
        if let Some(p) = self.pages.iter().find(|p| !p.path.is_empty() && (p.path == path || p.path == format!("{path}.md"))) {
            return Some(self.page(p));
        }
        if let Some((p, h)) = self.files.iter().find(|(p, _)| p == path) {
            return Some(Target::File { path: p.clone(), hash: h.clone() });
        }
        None
    }

    #[must_use]
    pub fn resolve(&self, target: &str, kind: LinkKind) -> Target {
        let t = target.trim();
        if t.is_empty() {
            return Target::Dangling(target.to_string());
        }
        let word = t.strip_suffix(".md").unwrap_or(t);
        let tries: [&str; 2] = [t, &t.replace("%20", " ")];
        for (i, candidate) in tries.iter().enumerate() {
            if i == 1 && tries[0] == tries[1] {
                break;
            }
            let word = if i == 0 { word.to_string() } else { candidate.strip_suffix(".md").unwrap_or(candidate).to_string() };
            if let Some(hit) = self.by_word(&word) {
                return hit;
            }
            if let Some(hit) = self.by_path(candidate) {
                return hit;
            }
        }
        if kind == LinkKind::Wiki {
            let stem = |path: &str| {
                let name = path.rsplit('/').next().unwrap_or(path);
                name.rsplit_once('.').map_or(name, |(s, _)| s).to_string()
            };
            if let Some((p, h)) = self.files.iter().find(|(p, _)| stem(p) == t) {
                return Target::File { path: p.clone(), hash: h.clone() };
            }
        }
        Target::Dangling(target.to_string())
    }
}

// -- links -----------------------------------------------------------------------

/// One link a page names, as written and as resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub kind: LinkKind,
    pub resolved: Target,
}

static LINKS_OF: Q = Q {
    id: "kb links",
    describe: "what this page names, and what each name resolved to",
    sql: "SELECT target, kind FROM kb_link WHERE page = ?1 ORDER BY rowid",
};

/// What a page names, resolved as its rows were.
#[must_use]
pub fn links(store: &Store, uid: &str) -> Vec<Link> {
    let resolver = Resolver::new(store);
    store
        .rows(&LINKS_OF, &[Val::S(uid.to_string())], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .iter()
        .map(|(target, kind)| {
            let kind = LinkKind::of(kind);
            Link { resolved: resolver.resolve(target, kind), target: target.clone(), kind }
        })
        .collect()
}

static BACKLINKS: Q = Q {
    id: "kb backlinks",
    describe: "the pages whose links resolve to this one",
    sql: "SELECT p.slug, p.title FROM kb_link l JOIN kb_page p ON p.uid = l.page
           WHERE p.deleted = 0 AND p.uid <> ?1 AND l.resolved = 'page:' || ?1
           GROUP BY p.slug ORDER BY p.title",
};

/// The pages that name this one, by the rows' own resolution.
#[must_use]
pub fn backlinks(store: &Store, page: &Page) -> Rc<Vec<(String, String)>> {
    store.rows(&BACKLINKS, &[Val::S(page.uid.clone())], |r| Ok((r.get(0)?, r.get(1)?)))
}

/// Rewrites one page's link rows from its body, resolved by `resolver`.
fn relink(c: &Connection, resolver: &Resolver, uid: &str, body: &str, now: f64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM kb_link WHERE page = ?1", [uid])?;
    for (target, kind) in markdown::links(body) {
        let resolved = resolver.resolve(&target, kind).key();
        c.execute(
            "INSERT INTO kb_link(page, target, kind, stamp, resolved) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![uid, target, kind.word(), now, resolved],
        )?;
    }
    Ok(())
}

/// Re-derives the alias rows and every live page's link rows: what a
/// write does once the slugs, aliases, files or bodies it changed have
/// landed, so the rows say what the reader would draw now. Both tables
/// are a function of the live rows and nothing else: a deleted page's
/// aliases are gone from the table, and back with it.
pub fn rederive_links(c: &Connection, now: f64) -> rusqlite::Result<()> {
    let pages: Vec<(String, String, String)> = c
        .prepare("SELECT uid, body, aliases FROM kb_page WHERE deleted = 0 ORDER BY created, uid")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    c.execute("DELETE FROM kb_alias", [])?;
    for (uid, _, aliases) in &pages {
        let aliases: Vec<String> = serde_json::from_str(aliases).unwrap_or_default();
        for a in aliases {
            // Two live pages never share a word: `name_conflict` refuses
            // the write. The oldest keeps it should a row arrive otherwise.
            c.execute("INSERT OR IGNORE INTO kb_alias(alias, uid) VALUES(?1, ?2)", params![a.to_lowercase(), uid])?;
        }
    }
    let resolver = Resolver::conn(c)?;
    c.execute("DELETE FROM kb_link WHERE page NOT IN (SELECT uid FROM kb_page WHERE deleted = 0)", [])?;
    for (uid, body, _) in &pages {
        relink(c, &resolver, uid, body, now)?;
    }
    Ok(())
}

/// Why a slug or an alias cannot be taken: another live page already
/// answers to it, by slug or by alias. A page's own words are not a
/// conflict with itself.
pub fn name_conflict(c: &Connection, uid: Option<&str>, slug: &str, aliases: &[String]) -> rusqlite::Result<Option<String>> {
    let me = uid.unwrap_or("");
    let mut words: Vec<(String, &str)> = vec![(slug.to_string(), "slug")];
    words.extend(aliases.iter().map(|a| (a.clone(), "alias")));
    for (word, what) in words {
        let folded = word.to_lowercase();
        let other: Option<String> = c
            .query_row(
                "SELECT slug FROM kb_page WHERE deleted = 0 AND uid <> ?1
                   AND (lower(slug) = ?2 OR uid IN (SELECT uid FROM kb_alias WHERE alias = ?2)) LIMIT 1",
                params![me, folded],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = other {
            return Ok(Some(format!("the {what} {word} is already {other}'s")));
        }
    }
    Ok(None)
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

/// Where a file's bytes are. In the plan this is the blob cache and the
/// outbox; the prototype keeps it in memory, per store, so a real install
/// carries nothing of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Cached,
    Fetching,
    Outbox,
    Missing,
}

impl Where {
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

/// The prototype's stand-in for the blob cache and the outbox: where each
/// hash's bytes are, kept beside the store in memory and never in it.
#[derive(Default)]
pub struct Cache(Mutex<HashMap<String, Where>>);

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

#[must_use]
pub fn where_is(store: &Store, hash: &str) -> Where {
    store
        .local::<Cache>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(hash)
        .copied()
        .unwrap_or(Where::Missing)
}

pub fn set_where(store: &Store, hash: &str, state: Where) {
    store
        .local::<Cache>()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(hash.to_string(), state);
}

static NAMED_BY: Q = Q {
    id: "kb file pages",
    describe: "the pages whose links resolve to this file",
    sql: "SELECT p.slug, p.title FROM kb_link l JOIN kb_page p ON p.uid = l.page
           WHERE p.deleted = 0 AND l.resolved = 'file:' || ?1
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
    sql: "SELECT uid, page, at, device, author, chat_title, message FROM kb_revision WHERE page = ?1 ORDER BY at DESC, rowid DESC",
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

/// Appends one revision: the only way a row gets into `kb_revision`, and
/// nothing takes one out.
fn append_revision(
    c: &Connection,
    page: &str,
    at: f64,
    author: &str,
    chat_title: &str,
    message: &str,
    body: &str,
) -> rusqlite::Result<String> {
    let uid = new_uid(c);
    c.execute(
        "INSERT INTO kb_revision(uid, page, at, device, author, chat_title, message, body) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![uid, page, at, DEVICE, author, chat_title, message, body],
    )?;
    Ok(uid)
}

/// The device a write here names: the prototype has one.
const DEVICE: &str = "this device";

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

/// A page's row as it stood, whole, for an undo to put back byte for byte.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    slug: String,
    kind: String,
    title: String,
    summary: String,
    aliases: String,
    tags: String,
    extra: String,
    body: String,
    path: String,
    created: f64,
    updated: f64,
    deleted: bool,
}

impl Snapshot {
    /// The document this row spells, for the revision an undo appends.
    fn document(&self) -> String {
        let list = |s: &str| serde_json::from_str::<Vec<String>>(s).unwrap_or_default();
        let front = Front {
            kind: self.kind.clone(),
            title: self.title.clone(),
            summary: self.summary.clone(),
            slug: String::new(),
            aliases: list(&self.aliases),
            tags: list(&self.tags),
            extra: self.extra.clone(),
        };
        markdown::document(&front, &self.body)
    }
}

fn snapshot(c: &Connection, uid: &str) -> rusqlite::Result<Option<Snapshot>> {
    c.query_row(
        "SELECT slug, kind, title, summary, aliases, tags, extra, body, path, created, updated, deleted FROM kb_page WHERE uid = ?1",
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
                path: r.get(8)?,
                created: r.get(9)?,
                updated: r.get(10)?,
                deleted: r.get(11)?,
            })
        },
    )
    .optional()
}

/// Puts a row back exactly. The aliases and the links are derived
/// afterwards, over every page, by the caller.
fn put_snapshot(c: &Connection, uid: &str, s: &Snapshot) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO kb_page(uid, slug, kind, title, summary, aliases, tags, extra, body, path, created, updated, deleted)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(uid) DO UPDATE SET slug = excluded.slug, kind = excluded.kind, title = excluded.title,
           summary = excluded.summary, aliases = excluded.aliases, tags = excluded.tags, extra = excluded.extra,
           body = excluded.body, path = excluded.path, created = excluded.created, updated = excluded.updated,
           deleted = excluded.deleted",
        params![
            uid, s.slug, s.kind, s.title, s.summary, s.aliases, s.tags, s.extra, s.body, s.path, s.created, s.updated,
            s.deleted
        ],
    )?;
    Ok(())
}

/// One write of a page, as history holds it: the row before and after.
/// Undo and redo put a row back and append a revision saying so; the
/// revision the write itself filed stays.
struct Wrote {
    uid: String,
    before: Option<Snapshot>,
    after: Snapshot,
    message: String,
}

impl Wrote {
    fn put(&self, w: &kernel::effect::World, back: bool) -> Result<(), String> {
        let (uid, before, after, message) = (self.uid.clone(), self.before.clone(), self.after.clone(), self.message.clone());
        let now = w.now();
        w.store()
            .write(move |c| {
                let (row, said) = if back {
                    // A page that did not exist is put away, not removed:
                    // its revisions name it, and a redo brings it back.
                    let row = before.clone().unwrap_or_else(|| Snapshot { deleted: true, ..after.clone() });
                    (row, format!("undone: {message}"))
                } else {
                    (after.clone(), format!("redone: {message}"))
                };
                put_snapshot(c, &uid, &row)?;
                rederive_links(c, now)?;
                let body = if row.deleted { String::new() } else { row.document() };
                append_revision(c, &uid, now, "editor", "", &said, &body)?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

impl Intent for Wrote {
    fn describe(&self) -> String {
        self.message.clone()
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.put(w, true)
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.put(w, false)
    }
}

/// What a document would leave on the page: the row before and the row
/// after, as one write of it would make them — a slug the block names, or
/// the row's own where it has one (a restore does not rename), or the
/// title's word for a page that has none yet. Read-only, so a save asks it
/// first and refuses what it refuses; the write asks it again inside its
/// transaction and answers with the same words.
type Planned = (String, Option<Snapshot>, Snapshot);

fn plan(c: &Connection, uid: Option<&str>, document: &str, now: f64) -> rusqlite::Result<Result<Planned, String>> {
    let (front, body) = markdown::parse_document(document);
    if front.title.trim().is_empty() {
        return Ok(Err("a page needs a title".into()));
    }
    let uid = uid.map_or_else(|| new_uid(c), str::to_string);
    let before = snapshot(c, &uid)?;
    let slug = if front.slug.is_empty() {
        before.as_ref().map_or_else(|| markdown::slug_of(&front.title), |s| s.slug.clone())
    } else {
        front.slug.clone()
    };
    if slug.is_empty() {
        return Ok(Err("a page needs a title with a word in it".into()));
    }
    // The name check reads the row as it will be — the slug it keeps or
    // takes, the aliases the block names — not the title's word alone.
    if let Some(why) = name_conflict(c, Some(&uid), &slug, &front.aliases)? {
        return Ok(Err(why));
    }
    let after = Snapshot {
        slug,
        kind: if front.kind.is_empty() { "concept".into() } else { front.kind.clone() },
        title: front.title.clone(),
        summary: front.summary.clone(),
        aliases: serde_json::to_string(&front.aliases).unwrap_or_else(|_| "[]".into()),
        tags: serde_json::to_string(&front.tags).unwrap_or_else(|_| "[]".into()),
        extra: front.extra.clone(),
        body,
        path: before.as_ref().map_or_else(String::new, |s| s.path.clone()),
        created: before.as_ref().map_or(now, |s| s.created),
        updated: now,
        deleted: false,
    };
    Ok(Ok((uid, before, after)))
}

/// A write the plan refused, carried out of the transaction as the store's
/// own error kind so the action fails rather than lands.
fn refused(why: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(why)))
}

/// Files a document as the page's next revision, in one transaction: the
/// row, the aliases, every page's links re-derived, the revision, the
/// draft gone. Answers the page's uid and the rows before and after.
fn write_document(
    c: &Connection,
    uid: Option<&str>,
    document: &str,
    message: &str,
    now: f64,
) -> rusqlite::Result<Planned> {
    let (uid, before, after) = plan(c, uid, document, now)?.map_err(refused)?;
    put_snapshot(c, &uid, &after)?;
    rederive_links(c, now)?;
    append_revision(c, &uid, now, "editor", "", message, document)?;
    c.execute("DELETE FROM kb_draft WHERE page = ?1", [&uid])?;
    Ok((uid, before, after))
}

/// The editor's save, and the revision's restore: one undoable action.
/// Answers the page's uid, so a new page's editor can point at it, or why
/// the document was refused — no title, or a name another page has, read
/// off the row the write would leave.
pub fn save(s: &mut Session, uid: Option<String>, document: String, message: String) -> Result<String, String> {
    let now = s.now();
    plan(s.store().conn(), uid.as_deref(), &document, now).map_err(|e| e.to_string())??;
    let label = message.clone();
    let (uid_for, doc, msg) = (uid.clone(), document, message.clone());
    let Some((page, before, after)) = s.act(Action::writing("kb.write", label, move |c| {
        write_document(c, uid_for.as_deref(), &doc, &msg, now)
    })) else {
        return Err("the store refused the write".into());
    };
    s.claim(Box::new(Wrote { uid: page.clone(), before, after, message }));
    Ok(page)
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
        let now = w.now();
        w.store()
            .write(move |c| {
                for uid in uids {
                    c.execute("UPDATE kb_page SET deleted = ?1 WHERE uid = ?2", params![value, uid])?;
                }
                rederive_links(c, now)
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

/// Soft-deletes pages by slug, undoably. Their links stop resolving and
/// stop counting, and come back with them.
pub fn delete(s: &mut Session, slugs: Vec<String>) -> bool {
    if slugs.is_empty() {
        return false;
    }
    let n = slugs.len();
    let label = if n == 1 { format!("delete {}", slugs[0]) } else { format!("delete {n} pages") };
    let now = s.now();
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
        rederive_links(c, now)?;
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

/// One page as a rename left it or found it: the row whole.
#[derive(Clone, Debug)]
struct Renamed {
    /// Every row the rename touched, before and after, the renamed page
    /// first.
    rows: Vec<(String, Snapshot, Snapshot)>,
    from: String,
    to: String,
}

impl Renamed {
    fn put(&self, w: &kernel::effect::World, back: bool) -> Result<(), String> {
        let rows = self.rows.clone();
        let (from, to) = (self.from.clone(), self.to.clone());
        let now = w.now();
        w.store()
            .write(move |c| {
                for (uid, before, after) in &rows {
                    put_snapshot(c, uid, if back { before } else { after })?;
                }
                rederive_links(c, now)?;
                let said = if back {
                    format!("undone: renamed {from} to {to}")
                } else {
                    format!("redone: renamed {from} to {to}")
                };
                for (uid, before, after) in &rows {
                    let row = if back { before } else { after };
                    append_revision(c, uid, now, "editor", "", &said, &row.document())?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

impl Intent for Renamed {
    fn describe(&self) -> String {
        format!("rename {} to {}", self.from, self.to)
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.put(w, true)
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        self.put(w, false)
    }
}

/// Renames a page: a new slug — the title's kebab form — and every link
/// that resolves to the page rewritten in the bodies that carry it, in
/// one undoable write. Each page whose body moved gets a revision; the
/// undo puts every row back exactly and appends its own.
pub fn rename(s: &mut Session, uid: &str, title: &str) -> Result<String, String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("a page needs a title".into());
    }
    let slug = markdown::slug_of(&title);
    if slug.is_empty() {
        return Err("a page needs a title with a word in it".into());
    }
    let Some(page) = page_by_uid(s.store(), uid) else {
        return Err("this page is gone".into());
    };
    if let Some(why) = name_conflict(s.store().conn(), Some(uid), &slug, &[]).map_err(|e| e.to_string())? {
        return Err(why);
    }
    let (from, to) = (page.slug.clone(), slug.clone());
    let now = s.now();
    let (u, new_slug, new_title) = (uid.to_string(), slug.clone(), title.clone());
    let (f, t) = (from.clone(), to.clone());
    let rows = s.act(Action::writing("kb.rename", format!("rename {from} to {to}"), move |c| {
        rename_tx(c, &u, &new_slug, &new_title, &f, &t, now)
    }));
    let Some(rows) = rows else {
        return Err("the store refused the rename".into());
    };
    s.claim(Box::new(Renamed { rows, from, to }));
    Ok(slug)
}

fn rename_tx(
    c: &Connection,
    uid: &str,
    slug: &str,
    title: &str,
    from: &str,
    to: &str,
    now: f64,
) -> rusqlite::Result<Vec<(String, Snapshot, Snapshot)>> {
    let resolver = Resolver::conn(c)?;
    let names = |t: &str, k: LinkKind| matches!(resolver.resolve(t, k), Target::Page { uid: u, .. } if u == uid);
    // Every page whose rows resolve to this one, by the rows' own answer.
    let mut affected: Vec<String> = c
        .prepare("SELECT DISTINCT page FROM kb_link WHERE resolved = 'page:' || ?1 AND page <> ?1")?
        .query_map([uid], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    affected.sort();
    let mut rows = Vec::new();
    // The page itself: the slug and the title move, and its own links to
    // itself, if any, with them.
    let Some(before) = snapshot(c, uid)? else {
        return Ok(rows);
    };
    let mut after = before.clone();
    after.slug = slug.to_string();
    after.title = title.to_string();
    after.body = markdown::rename_links(&before.body, &names, to);
    after.updated = now;
    put_snapshot(c, uid, &after)?;
    append_revision(c, uid, now, "editor", "", &format!("renamed {from} to {to}"), &after.document())?;
    rows.push((uid.to_string(), before, after));
    for other in affected {
        let Some(before) = snapshot(c, &other)? else {
            continue;
        };
        let body = markdown::rename_links(&before.body, &names, to);
        if body == before.body {
            continue;
        }
        let mut after = before.clone();
        after.body = body;
        after.updated = now;
        put_snapshot(c, &other, &after)?;
        append_revision(c, &other, now, "editor", "", &format!("links to {from} renamed to {to}"), &after.document())?;
        rows.push((other, before, after));
    }
    rederive_links(c, now)?;
    Ok(rows)
}

/// A page written by someone other than the editor — a seed's chat —
/// straight into a store: the row, the links, and one revision naming
/// that author. Answers the page's uid.
pub fn seed_document(
    c: &Connection,
    document: &str,
    author: &str,
    chat_title: &str,
    message: &str,
    now: f64,
) -> rusqlite::Result<String> {
    let (uid, _, _) = write_document(c, None, document, message, now)?;
    c.execute(
        "UPDATE kb_revision SET author = ?2, chat_title = ?3 WHERE page = ?1",
        params![uid, author, chat_title],
    )?;
    Ok(uid)
}
