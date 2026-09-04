//! What a directory is to this app: the listing, its filter, and the
//! spellings a path travels in.
//!
//! The disk is reached through the world's [`Disk`](kernel::caps::Disk)
//! capability, in the display spelling the panels use (`~/Downloads`), and
//! the kernel translates. Nothing here writes: the four verbs that do are
//! in [`ops`](super::ops).
//!
//! Watching is the other half of reading, and [`Watch`] is where a panel
//! asks for it: somebody else's write is a listing that has gone stale,
//! and a panel that is not open is a directory nobody has to be told
//! about.

use std::rc::Rc;

use kernel::caps::{Disk, Watcher};
use kernel::effect::World;
use kernel::filter::{Ast, Op};
use kernel::richtable::{self, Datasource, Suggestion, TagDef, TagType, Values};
use kernel::store::Store;
use kernel::theme;

// What a file *is* — its media type, whether a picture is worth decoding
// and how big it is, how much of it to read, and how big a thing another
// app may carry out — is the kernel's, beside `FileKind`: mail asks the
// same questions of a part of a letter that this app asks of a path.
pub use kernel::caps::{
    basename, display_path, fmt_size, image_size, is_root, join, parent, preview_of, real_path,
    Entry, FileId, FileKind, Preview, HOME, ROOT,
};

/// Rows per page. A listing is in memory, so the size only bounds a draw.
pub const PAGE: usize = 50;

// -- paths ---------------------------------------------------------------------

/// The crumb line above a listing: `(label, path)` per ancestor, the
/// directory itself last — `~ / Downloads / 2026`, or `/ tmp`.
#[must_use]
pub fn crumbs(dir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut acc = String::new();
    if dir.starts_with('/') {
        acc = ROOT.to_string();
        out.push((ROOT.to_string(), ROOT.to_string()));
    }
    for seg in dir.split('/').filter(|s| !s.is_empty()) {
        acc = join(&acc, seg);
        out.push((seg.to_string(), acc.clone()));
    }
    out
}

/// A typed path as the panels spell it: `~/`-relative or absolute, no
/// trailing slash except on a root, `~` alone for home. `None` for a
/// spelling the browser does not read (relative, empty).
///
/// A second root inside the text **restarts** the path — Emacs'
/// find-file rule: `~/Downloads//tmp` is `/tmp`, `~/Downloads/~/x` is
/// `~/x`. The field is seeded with where the panel stands, so this is
/// how a typed absolute path wins over the seed without clearing it.
#[must_use]
pub fn normalize(typed: &str) -> Option<String> {
    let mut t = typed.trim();
    let restart = [t.rfind("//"), t.rfind("/~")].into_iter().flatten().max();
    if let Some(i) = restart {
        t = &t[i + 1..];
    }
    if t.is_empty() || !(t.starts_with('~') || t.starts_with('/')) {
        return None;
    }
    let mut segs: Vec<&str> = Vec::new();
    let (root, rest) = if let Some(r) = t.strip_prefix('~') {
        (HOME, r)
    } else {
        (ROOT, t)
    };
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        match seg {
            "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    Some(segs.iter().fold(root.to_string(), |acc, s| join(&acc, s)))
}

/// `1 file`, `3 files` — what a set of paths is called.
#[must_use]
pub fn plural(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// How many lines a text preview takes at `cols` characters a line,
/// wrapped the way the card draws it: every line at least one.
#[must_use]
pub fn text_lines(text: &str, cols: usize) -> usize {
    let cols = cols.max(1);
    text.lines()
        .map(|l| l.chars().count().div_ceil(cols).max(1))
        .sum::<usize>()
        .max(1)
}

/// How many lines a picture of `w × h` takes, drawn at the full width of a
/// card `cols` characters wide.
///
/// The card draws a picture at the text's width, so its height in points is
/// that width times the aspect; in lines it is that over one line's height.
/// Both are multiples of the type size, which cancels — what is left is the
/// column in characters times `MONO_ADV / LINE_H`.
#[must_use]
pub fn image_lines(cols: usize, w: u32, h: u32) -> f64 {
    let text_w = cols.max(1) as f64 * theme::MONO_ADV;
    text_w * f64::from(h) / f64::from(w.max(1)) / theme::LINE_H
}

// -- the disk, read ------------------------------------------------------------

/// A directory's listing, or why there is none — through the world's
/// [`Disk`], in the display spelling the panels use.
///
/// # Errors
///
/// Whatever the disk said, and *this world has no Disk* where there is
/// none.
pub fn list_in(world: &World, dir: &str) -> Result<Vec<Entry>, String> {
    world
        .with_cap::<dyn Disk, _>(|d| d.list_dir(&real_path(dir)))
        .and_then(|r| r)
}

/// One path's entry, if the disk has it.
#[must_use]
pub fn stat_in(world: &World, path: &str) -> Option<Entry> {
    world
        .with_cap::<dyn Disk, _>(|d| d.stat(&real_path(path)))
        .ok()
        .and_then(Result::ok)
        .flatten()
}

/// Whether the disk has this path as a directory.
#[must_use]
pub fn is_dir_in(world: &World, path: &str) -> bool {
    stat_in(world, path).is_some_and(|e| e.is_dir)
}

/// The first `max` bytes of a file.
///
/// # Errors
///
/// Whatever the disk said.
pub fn read_in(world: &World, path: &str, max: usize) -> Result<Vec<u8>, String> {
    world
        .with_cap::<dyn Disk, _>(|d| d.read_file(&real_path(path), max))
        .and_then(|r| r)
}

/// What the disk calls the object at this path — `None` for nothing there
/// **and** for a disk that would not say, which a reversal treats the same
/// way: it refuses.
#[must_use]
pub fn id_in(world: &World, path: &str) -> Option<FileId> {
    world
        .with_cap::<dyn Disk, _>(|d| d.file_id(&real_path(path)))
        .ok()
        .and_then(Result::ok)
        .flatten()
}

// -- the disk, watched ---------------------------------------------------------

/// A directory watched for as long as this is held.
///
/// What a panel wants to be told about is the one directory it shows, and
/// only while it is showing it — so the instance keeps one of these, and
/// closing the panel drops it and lets the watcher go. Two panels on one
/// directory are two holds, and it is watched until both have let go.
///
/// A world with no machine behind its watcher — every test, every scripted
/// run — takes the hold and never reports anything, which is the app as it
/// was before anything watched: a panel refreshes on the writes it knows
/// about.
pub struct Watch {
    world: Rc<World>,
    dir: String,
}

impl Watch {
    /// Starts watching `dir`, in the display spelling the panels use.
    #[must_use]
    pub fn on(world: &Rc<World>, dir: &str) -> Watch {
        let _ = world.with_cap::<dyn Watcher, _>(|w| w.watch(&real_path(dir)));
        Watch {
            world: world.clone(),
            dir: dir.to_string(),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self
            .world
            .with_cap::<dyn Watcher, _>(|w| w.unwatch(&real_path(&self.dir)));
    }
}

/// How many rounds of somebody else's writing this directory has seen. The
/// number says nothing by itself; a panel keeps the one its listing was
/// read at and looks again when they differ.
#[must_use]
pub fn watched_at(world: &World, dir: &str) -> u64 {
    world
        .with_cap::<dyn Watcher, _>(|w| w.revision(&real_path(dir)))
        .unwrap_or(0)
}

// -- the datasource ------------------------------------------------------------

/// The tags a files panel's filter accepts.
pub static TAGS: &[TagDef] = &[
    TagDef {
        name: "dir",
        kind: TagType::Bool,
        ops: &[],
        describe: "directories only",
        values: Values::None,
    },
    TagDef {
        name: "hidden",
        kind: TagType::Bool,
        ops: &[],
        describe: "dot-files too",
        values: Values::None,
    },
    TagDef {
        name: "kind",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "image · text · pdf · archive · other",
        values: Values::Static(&[
            ("image", "image"),
            ("text", "text"),
            ("pdf", "pdf"),
            ("archive", "archive"),
            ("other", "other"),
        ]),
    },
    TagDef {
        name: "size",
        kind: TagType::Number,
        ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte],
        describe: "bytes",
        values: Values::None,
    },
    TagDef {
        name: "modified",
        kind: TagType::Date,
        ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte],
        describe: "when it last changed, dd.mm.yyyy",
        values: Values::None,
    },
];

/// One row of a listing: the entry, and the directory it is in.
///
/// A name is not a panel. What a row opens is that name under the directory
/// the source lists, and the rich table asks a row what it opens while the
/// instance that owns the listing is already borrowed — so a row carries
/// where it is rather than asking the panel back.
#[derive(Debug, Clone, PartialEq)]
pub struct DirRow {
    pub dir: String,
    pub entry: Entry,
}

/// One directory as a rich-table datasource: the listing in memory — read
/// through the disk when the panel opened on the directory — and the
/// filter evaluated over it. A panel re-lists when a verb says the disk
/// changed, or the watcher says another program did.
#[derive(Debug, Clone, PartialEq)]
pub struct DirSource {
    pub dir: String,
    pub entries: Rc<Vec<Entry>>,
}

impl DirSource {
    #[must_use]
    pub fn new(dir: &str, entries: Vec<Entry>) -> DirSource {
        DirSource {
            dir: dir.to_string(),
            entries: Rc::new(entries),
        }
    }

    /// One entry as a row of this listing.
    fn row(&self, entry: Entry) -> DirRow {
        DirRow {
            dir: self.dir.clone(),
            entry,
        }
    }

    fn filtered(&self, ast: Option<&Ast>) -> Vec<Entry> {
        let hidden = ast.is_some_and(|a| a.tag_names().contains(&"hidden"));
        self.entries
            .iter()
            .filter(|e| hidden || !e.hidden())
            .filter(|e| ast.is_none_or(|a| matches(e, a)))
            .cloned()
            .collect()
    }
}

fn cmp(op: Op, have: f64, want: f64) -> bool {
    match op {
        Op::Eq => (have - want).abs() < f64::EPSILON,
        Op::Gt => have > want,
        Op::Gte => have >= want,
        Op::Lt => have < want,
        Op::Lte => have <= want,
    }
}

/// The filter grammar over one entry, with the SQL builder's semantics
/// for what does not bind: a tag the source does not know, a value a
/// typed tag cannot read, and `@hidden` (a switch, not a predicate) are
/// **dropped** rather than answered — `None` — so `@not:bogus` does not
/// hide everything and `(@dir @or @bogus)` is `@dir`. A filter that is
/// nothing but dropped clauses shows everything, and the error line says
/// why.
fn holds(e: &Entry, ast: &Ast) -> Option<bool> {
    match ast {
        Ast::Text(t) => Some(e.name.to_lowercase().contains(&t.to_lowercase())),
        Ast::Tag(t) => match t.as_str() {
            "dir" => Some(e.is_dir),
            _ => None,
        },
        Ast::Op { tag, op, value } => match tag.as_str() {
            "kind" => {
                let v = value.trim().to_lowercase();
                Some(e.kind().tag() == v || e.kind().word() == v)
            }
            "size" => value
                .trim()
                .parse::<f64>()
                .ok()
                .map(|v| cmp(*op, e.size as f64, v)),
            "modified" => richtable::date_span(value).map(|(lo, hi)| match op {
                Op::Eq => e.modified >= lo && e.modified < hi,
                Op::Gt => e.modified >= hi,
                Op::Gte => e.modified >= lo,
                Op::Lt => e.modified < lo,
                Op::Lte => e.modified < hi,
            }),
            _ => None,
        },
        Ast::Not(inner) => holds(e, inner).map(|b| !b),
        Ast::And(v) | Ast::Or(v) => {
            let parts: Vec<bool> = v.iter().filter_map(|a| holds(e, a)).collect();
            if parts.is_empty() {
                None
            } else if matches!(ast, Ast::And(_)) {
                Some(parts.iter().all(|b| *b))
            } else {
                Some(parts.iter().any(|b| *b))
            }
        }
    }
}

/// Whether an entry passes the filter; a filter nothing in which binds
/// passes everything.
#[must_use]
pub fn matches(e: &Entry, ast: &Ast) -> bool {
    holds(e, ast).unwrap_or(true)
}

impl Datasource for DirSource {
    type Row = DirRow;
    /// A row is its name: unique within the one directory a source lists,
    /// and what a mark holds.
    type Key = String;

    fn tags(&self) -> &'static [TagDef] {
        TAGS
    }

    fn key(&self, row: &DirRow) -> String {
        row.entry.name.clone()
    }

    /// A name travels as itself: it is already the spelling a panel
    /// argument, a hit, or an intent would write down.
    fn key_text(&self, key: &String) -> String {
        key.clone()
    }

    fn key_parse(&self, text: &str) -> Option<String> {
        (!text.is_empty()).then(|| text.to_string())
    }

    /// Every name the filter shows, in the table's order — what `all`
    /// marks. The listing is in memory, so this is the order itself, read
    /// once.
    fn keys(&self, _store: &Store, ast: Option<&Ast>) -> Option<Vec<String>> {
        Some(self.filtered(ast).into_iter().map(|e| e.name).collect())
    }

    /// Which of these names the filter still shows; the rest are the marks
    /// it hides. The caller's order is kept.
    fn present(&self, _store: &Store, ast: Option<&Ast>, keys: &[String]) -> Vec<String> {
        let shown: std::collections::BTreeSet<String> =
            self.filtered(ast).into_iter().map(|e| e.name).collect();
        keys.iter()
            .filter(|k| shown.contains(*k))
            .cloned()
            .collect()
    }

    /// The entry by name, filter or no filter: a directory's own listing
    /// is its base condition, and a dot-file the filter hid is hidden,
    /// not gone.
    fn by_key(&self, _store: &Store, key: &String) -> Option<DirRow> {
        self.entries
            .iter()
            .find(|e| &e.name == key)
            .cloned()
            .map(|e| self.row(e))
    }

    fn count(&self, _store: &Store, ast: Option<&Ast>) -> Option<usize> {
        Some(self.filtered(ast).len())
    }

    fn page(
        &self,
        _store: &Store,
        ast: Option<&Ast>,
        offset: usize,
        limit: usize,
    ) -> Rc<Vec<DirRow>> {
        Rc::new(
            self.filtered(ast)
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|e| self.row(e))
                .collect(),
        )
    }

    fn index_of(&self, _store: &Store, ast: Option<&Ast>, row: &DirRow) -> Option<usize> {
        self.filtered(ast)
            .iter()
            .position(|e| e.name == row.entry.name)
    }

    fn suggest(&self, _store: &Store, _tag: &str, _prefix: &str) -> Vec<Suggestion> {
        Vec::new()
    }
}
