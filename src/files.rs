//! The file browser's domain (CR-008, **draft**): what a directory lists,
//! what a file card shows, and the one held item `copy`/`move` carry to a
//! `… here`.
//!
//! Over a **demo tree** for now — the panels library draws every state of
//! the feature from it while the design settles. The disk (a listing read
//! through the outside during draw, the watcher, the verbs as effects)
//! arrives with the implementation; nothing here touches a filesystem.
//!
//! Paths are the display form, `~/Downloads/2026`.

use std::rc::Rc;

use crate::filter::{Ast, Op};
use crate::richtable::{
    self, Completion, Datasource, Suggestion, TagDef, TagType, Values, MAX_SUGGESTIONS,
};
use crate::store::Store;

/// The root the launcher's `files` opens on.
pub const HOME: &str = "~";
/// The other root: the whole disk, for `go to /tmp`.
pub const ROOT: &str = "/";

/// One entry of a directory, as the files panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The name alone, no slash — the row adds one to a directory's.
    pub name: String,
    pub is_dir: bool,
    /// Bytes; a directory has none.
    pub size: u64,
    /// Unix seconds.
    pub modified: f64,
}

impl Entry {
    /// A dot-file: out of a listing unless `@hidden` asks.
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    #[must_use]
    pub fn kind(&self) -> FileKind {
        if self.is_dir {
            FileKind::Dir
        } else {
            FileKind::of_name(&self.name)
        }
    }

    /// What the row shows: a directory wears a trailing `/`.
    #[must_use]
    pub fn label(&self) -> String {
        if self.is_dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// What a file is, off its extension — the card's word and `@kind:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    Image,
    Text,
    Pdf,
    Archive,
    Other,
}

impl FileKind {
    #[must_use]
    pub fn of_name(name: &str) -> FileKind {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "svg") => FileKind::Image,
            Some(
                "txt" | "md" | "rs" | "toml" | "json" | "tla" | "cfg" | "log" | "csv" | "html"
                | "xml" | "yaml" | "yml" | "sh",
            ) => FileKind::Text,
            Some("pdf") => FileKind::Pdf,
            Some("zip" | "gz" | "tgz" | "tar" | "dmg" | "7z" | "rar" | "xz") => FileKind::Archive,
            _ => FileKind::Other,
        }
    }

    /// The card's word for it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            FileKind::Dir => "directory",
            FileKind::Image => "image",
            FileKind::Text => "text",
            FileKind::Pdf => "pdf",
            FileKind::Archive => "archive",
            FileKind::Other => "file",
        }
    }

    /// The `@kind:` value that finds it.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            FileKind::Dir => "dir",
            FileKind::Image => "image",
            FileKind::Text => "text",
            FileKind::Pdf => "pdf",
            FileKind::Archive => "archive",
            FileKind::Other => "other",
        }
    }
}

// -- paths -------------------------------------------------------------------

/// `~/Downloads` + `2026` → `~/Downloads/2026`; `/` + `tmp` → `/tmp`.
#[must_use]
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir == ROOT {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The directory a path sits in; `None` at a root (`~`, `/`).
#[must_use]
pub fn parent(path: &str) -> Option<&str> {
    if path == ROOT {
        return None;
    }
    path.rsplit_once('/')
        .map(|(p, _)| if p.is_empty() { ROOT } else { p })
}

/// The last segment: the panel's title. A root is its own name.
#[must_use]
pub fn basename(path: &str) -> &str {
    if path == ROOT {
        return ROOT;
    }
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

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

/// A typed path as the tree spells it: `~/`-relative or absolute, no
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

/// `1.2 MB`, `84 KB`, `640 B`.
#[must_use]
pub fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    let unit = |v: f64, u: &str| {
        if v < 10.0 {
            format!("{v:.1} {u}")
        } else {
            format!("{v:.0} {u}")
        }
    };
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        unit(b / 1024.0, "KB")
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        unit(b / (1024.0 * 1024.0), "MB")
    } else {
        unit(b / (1024.0 * 1024.0 * 1024.0), "GB")
    }
}

// -- the held item -----------------------------------------------------------

/// What `copy` and `move` hold, until a `… here` performs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldOp {
    Copy,
    Move,
}

impl HoldOp {
    /// The verb, for a toast.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            HoldOp::Copy => "copy",
            HoldOp::Move => "move",
        }
    }

    /// The button every files panel shows while this is held.
    #[must_use]
    pub fn here_label(self) -> &'static str {
        match self {
            HoldOp::Copy => "copy here",
            HoldOp::Move => "move here",
        }
    }

    /// What a `… here` did, past tense.
    #[must_use]
    pub fn done(self) -> &'static str {
        match self {
            HoldOp::Copy => "copied",
            HoldOp::Move => "moved",
        }
    }
}

/// The one held item, process-wide: context, not history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub op: HoldOp,
    pub path: String,
}

// -- the demo tree -----------------------------------------------------------

struct Fx {
    path: &'static str,
    dir: bool,
    size: u64,
    /// `(year, month, day, hour, minute)`.
    at: (i64, u32, u32, u32, u32),
}

const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;

/// A home directory a design review can walk: two levels under Downloads,
/// pictures, documents, dot-files, one of every kind the card previews.
const TREE: &[Fx] = &[
    Fx { path: "~/Desktop", dir: true, size: 0, at: (2026, 8, 31, 18, 40) },
    Fx { path: "~/Documents", dir: true, size: 0, at: (2026, 8, 29, 11, 5) },
    Fx { path: "~/Downloads", dir: true, size: 0, at: (2026, 9, 1, 9, 12) },
    Fx { path: "~/Pictures", dir: true, size: 0, at: (2026, 8, 24, 20, 3) },
    Fx { path: "~/superapp", dir: true, size: 0, at: (2026, 9, 2, 7, 30) },
    Fx { path: "~/.config", dir: true, size: 0, at: (2026, 7, 14, 10, 0) },
    Fx { path: "~/notes.md", dir: false, size: 2 * KB + 130, at: (2026, 8, 30, 22, 47) },
    Fx { path: "~/.zshrc", dir: false, size: 1 * KB + 90, at: (2026, 6, 2, 9, 0) },
    Fx { path: "~/Desktop/todo.txt", dir: false, size: 300, at: (2026, 8, 31, 18, 40) },
    Fx { path: "~/Documents/panel-model.md", dir: false, size: 9 * KB, at: (2026, 8, 29, 11, 5) },
    Fx { path: "~/Documents/cr-008-files.md", dir: false, size: 14 * KB, at: (2026, 9, 2, 6, 55) },
    Fx { path: "~/Documents/Lease.tla", dir: false, size: 5 * KB, at: (2026, 8, 28, 15, 20) },
    Fx { path: "~/Downloads/2026", dir: true, size: 0, at: (2026, 8, 17, 12, 0) },
    Fx { path: "~/Downloads/report-q3.pdf", dir: false, size: MB + 200 * KB, at: (2026, 8, 31, 9, 14) },
    Fx { path: "~/Downloads/budget-2026.xlsx", dir: false, size: 84 * KB, at: (2026, 8, 31, 9, 14) },
    Fx { path: "~/Downloads/screenshot-2026-08-30.png", dir: false, size: 412 * KB, at: (2026, 8, 30, 14, 2) },
    Fx { path: "~/Downloads/superapp-0.1.0.dmg", dir: false, size: 38 * MB, at: (2026, 9, 1, 9, 12) },
    Fx { path: "~/Downloads/logs.tar.gz", dir: false, size: 3 * MB + 400 * KB, at: (2026, 8, 30, 7, 30) },
    Fx { path: "~/Downloads/README.txt", dir: false, size: 640, at: (2026, 8, 12, 16, 45) },
    Fx { path: "~/Downloads/.DS_Store", dir: false, size: 6 * KB, at: (2026, 9, 1, 9, 12) },
    Fx { path: "~/Downloads/2026/invoice-0817.pdf", dir: false, size: 96 * KB, at: (2026, 8, 17, 12, 0) },
    Fx { path: "~/Downloads/2026/photo-lisbon.jpg", dir: false, size: 2 * MB + 800 * KB, at: (2026, 8, 3, 19, 21) },
    Fx { path: "~/Downloads/2026/notes.txt", dir: false, size: KB + 100, at: (2026, 8, 17, 12, 0) },
    Fx { path: "~/Pictures/lisbon", dir: true, size: 0, at: (2026, 8, 3, 19, 21) },
    Fx { path: "~/Pictures/fold-cover.png", dir: false, size: MB + 100 * KB, at: (2026, 8, 24, 20, 3) },
    Fx { path: "~/Pictures/lisbon/IMG_0417.jpg", dir: false, size: 3 * MB + 200 * KB, at: (2026, 8, 3, 19, 21) },
    Fx { path: "~/Pictures/lisbon/IMG_0418.jpg", dir: false, size: 3 * MB, at: (2026, 8, 3, 19, 24) },
    Fx { path: "~/superapp/files", dir: true, size: 0, at: (2026, 9, 2, 7, 30) },
    Fx { path: "~/superapp/superapp.db", dir: false, size: 24 * MB, at: (2026, 9, 2, 7, 30) },
    Fx { path: "~/superapp/panel-context.md", dir: false, size: 3 * KB, at: (2026, 9, 1, 23, 8) },
    // Beyond home: what `go to` reaches.
    Fx { path: "/Applications", dir: true, size: 0, at: (2026, 8, 20, 10, 0) },
    Fx { path: "/Users", dir: true, size: 0, at: (2026, 6, 1, 9, 0) },
    Fx { path: "/Users/andrey", dir: true, size: 0, at: (2026, 9, 2, 7, 30) },
    Fx { path: "/etc", dir: true, size: 0, at: (2026, 7, 14, 10, 0) },
    Fx { path: "/etc/hosts", dir: false, size: 213, at: (2026, 7, 14, 10, 0) },
    Fx { path: "/tmp", dir: true, size: 0, at: (2026, 9, 2, 12, 40) },
    Fx { path: "/tmp/superapp-e2e", dir: true, size: 0, at: (2026, 9, 2, 12, 40) },
    Fx { path: "/tmp/superapp-e2e/frames", dir: true, size: 0, at: (2026, 9, 2, 12, 41) },
    Fx { path: "/tmp/superapp-e2e/superapp.db", dir: false, size: 2 * MB, at: (2026, 9, 2, 12, 40) },
    Fx { path: "/tmp/notes.txt", dir: false, size: 380, at: (2026, 9, 1, 18, 5) },
    Fx { path: "/tmp/.keep", dir: false, size: 0, at: (2026, 9, 1, 18, 5) },
];

/// Whether the tree has this path as a directory.
#[must_use]
pub fn is_dir(path: &str) -> bool {
    path == HOME || path == ROOT || TREE.iter().any(|f| f.path == path && f.dir)
}

fn entry_of(fx: &Fx) -> Entry {
    let (y, mo, d, h, min) = fx.at;
    Entry {
        name: basename(fx.path).to_string(),
        is_dir: fx.dir,
        size: fx.size,
        modified: crate::mail::ts(y, mo, d, h, min),
    }
}

/// Whether the tree has this path — a directory or a file.
#[must_use]
pub fn exists(path: &str) -> bool {
    path == HOME || path == ROOT || TREE.iter().any(|f| f.path == path)
}

/// The entry at a path, if the tree has it.
#[must_use]
pub fn entry(path: &str) -> Option<Entry> {
    TREE.iter().find(|f| f.path == path).map(entry_of)
}

/// A directory's listing, unfiltered: directories first, then files, by
/// name, case folded. Dot-files included — the filter decides.
#[must_use]
pub fn list(dir: &str) -> Vec<Entry> {
    let mut v: Vec<Entry> = TREE
        .iter()
        .filter(|f| parent(f.path) == Some(dir))
        .map(entry_of)
        .collect();
    v.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    v
}

/// A text file's reading, for the card's preview.
#[must_use]
pub fn text_of(path: &str) -> Option<String> {
    let e = entry(path)?;
    if e.kind() != FileKind::Text {
        return None;
    }
    Some(match e.name.as_str() {
        "hosts" => "127.0.0.1\tlocalhost\n255.255.255.255\tbroadcasthost\n::1\tlocalhost".into(),
        "README.txt" => "superapp 0.1.0\n\nA personal user-space OS: one workspace, specialized panels, no windows.\n\nDrag the .app to Applications. First launch asks for nothing; add a mail account in settings.".into(),
        "todo.txt" => "- files: the card previews\n- files: move here / copy here\n- attachments (follow-up CR)\n- rename?".into(),
        "notes.txt" => "Lisbon, August.\n\nInvoice 0817 is for the flat; the photos are from the last evening.".into(),
        "notes.md" => "# notes\n\n- a directory is a list panel\n- a file is a card\n- enter goes, the cursor previews\n\nThe join is the only relation.".into(),
        "panel-context.md" => "# panel: files ~/Downloads\n\nfilter: @kind:image\nentries: 8 (1 shown)\nlisted: 0.4 s ago".into(),
        _ => format!("{}\n\n(the first 64 KB of the file, in the app's one face)", e.name),
    })
}

/// An image file's bytes, for the card's preview. The demo tree has no
/// pictures of its own, so every image is the app icon.
#[must_use]
pub fn image_of(path: &str) -> Option<&'static [u8]> {
    let e = entry(path)?;
    (e.kind() == FileKind::Image).then_some(include_bytes!("../resources/icon_256.png"))
}

// -- the path field's completion ----------------------------------------------

/// The `go to` field as a completion: the segment under the caret,
/// matched as a prefix against the entries of the directory the segments
/// before it name — a shell's tab, in the rich table's box. A picked
/// directory lands with its slash, so the next offer opens at once; a
/// root is offered when nothing is typed yet.
pub struct PathCompletion;

/// What the caret is in the middle of typing in a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCtx {
    /// Where the segment starts: after the last `/` before the caret.
    pub start: usize,
    /// The directory the segments before it name; `None` before the
    /// first slash, where a root is what completes.
    pub dir: Option<String>,
    /// The segment as typed up to the caret.
    pub prefix: String,
}

impl Completion for PathCompletion {
    type Ctx = PathCtx;

    fn context(&self, text: &str, cursor: usize) -> Option<PathCtx> {
        let mut cursor = cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let before = &text[..cursor];
        match before.rfind('/') {
            Some(i) => {
                let dir = normalize(&before[..=i])?;
                Some(PathCtx {
                    start: i + 1,
                    dir: Some(dir),
                    prefix: before[i + 1..].to_string(),
                })
            }
            None => Some(PathCtx {
                start: 0,
                dir: None,
                prefix: before.to_string(),
            }),
        }
    }

    fn offer(&self, _store: &Store, ctx: &PathCtx) -> Vec<Suggestion> {
        let Some(dir) = &ctx.dir else {
            // Before a slash: the two roots, as far as they match.
            return [HOME, ROOT]
                .iter()
                .filter(|r| r.starts_with(ctx.prefix.as_str()) || ctx.prefix.is_empty())
                .map(|r| Suggestion::labeled(format!("{r}/").replace("//", "/"), if *r == ROOT { ROOT.to_string() } else { format!("{r}/") }))
                .collect();
        };
        let prefix = ctx.prefix.to_lowercase();
        let hidden = prefix.starts_with('.');
        let mut out: Vec<Suggestion> = list(dir)
            .into_iter()
            .filter(|e| hidden || !e.hidden())
            .filter(|e| e.name.to_lowercase().starts_with(&prefix))
            .map(|e| {
                let label = e.label();
                let describe = if e.is_dir { String::new() } else { fmt_size(e.size) };
                Suggestion {
                    value: label.clone(),
                    label,
                    describe,
                }
            })
            .collect();
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    fn splice(&self, text: &str, cursor: usize, ctx: &PathCtx, pick: &Suggestion) -> (String, usize) {
        let cursor = cursor.min(text.len()).max(ctx.start);
        let out = format!("{}{}{}", &text[..ctx.start], pick.value, &text[cursor..]);
        (out, ctx.start + pick.value.len())
    }
}

// -- the datasource ----------------------------------------------------------

/// Rows per page. A listing is in memory, so the size only bounds a draw.
pub const PAGE: usize = 50;

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

/// One directory as a rich-table datasource: the listing in memory, the
/// filter evaluated over it. The draft re-lists per call; the
/// implementation stamps the listing with the watcher's generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirSource {
    pub dir: String,
}

impl DirSource {
    #[must_use]
    pub fn new(dir: &str) -> Self {
        DirSource {
            dir: dir.to_string(),
        }
    }

    fn filtered(&self, ast: Option<&Ast>) -> Vec<Entry> {
        let hidden = ast.is_some_and(|a| a.tag_names().contains(&"hidden"));
        list(&self.dir)
            .into_iter()
            .filter(|e| hidden || !e.hidden())
            .filter(|e| ast.map_or(true, |a| matches(e, a)))
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
    type Row = Entry;

    fn tags(&self) -> &'static [TagDef] {
        TAGS
    }

    fn count(&self, _store: &Store, ast: Option<&Ast>) -> Option<usize> {
        Some(self.filtered(ast).len())
    }

    fn page(&self, _store: &Store, ast: Option<&Ast>, offset: usize, limit: usize) -> Rc<Vec<Entry>> {
        Rc::new(self.filtered(ast).into_iter().skip(offset).take(limit).collect())
    }

    fn index_of(&self, _store: &Store, ast: Option<&Ast>, row: &Entry) -> Option<usize> {
        self.filtered(ast).iter().position(|e| e.name == row.name)
    }

    fn suggest(&self, _store: &Store, _tag: &str, _prefix: &str) -> Vec<Suggestion> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter;

    fn names(v: &[Entry]) -> Vec<String> {
        v.iter().map(Entry::label).collect()
    }

    #[test]
    fn a_listing_puts_directories_first_then_names() {
        let l = list("~/Downloads");
        assert_eq!(
            names(&l),
            [
                "2026/",
                ".DS_Store",
                "budget-2026.xlsx",
                "logs.tar.gz",
                "README.txt",
                "report-q3.pdf",
                "screenshot-2026-08-30.png",
                "superapp-0.1.0.dmg",
            ]
        );
        assert!(list("~/nowhere").is_empty());
    }

    #[test]
    fn the_filter_hides_dot_files_unless_asked() {
        let store = Store::open(None).unwrap();
        let src = DirSource::new("~/Downloads");
        let all = src.count(&store, None).unwrap();
        assert_eq!(all, 7, "the .DS_Store is out");
        let ast = filter::parse("@hidden").ast;
        assert_eq!(src.count(&store, ast.as_ref()), Some(8));
        let ast = filter::parse("@kind:image").ast;
        let page = src.page(&store, ast.as_ref(), 0, PAGE);
        assert_eq!(names(&page), ["screenshot-2026-08-30.png"]);
        let ast = filter::parse("@size>1000000 @not:dir").ast;
        let page = src.page(&store, ast.as_ref(), 0, PAGE);
        assert_eq!(names(&page), ["logs.tar.gz", "report-q3.pdf", "superapp-0.1.0.dmg"]);
        let ast = filter::parse("@dir").ast;
        assert_eq!(names(&src.page(&store, ast.as_ref(), 0, PAGE)), ["2026/"]);
        let ast = filter::parse("@modified>30.08.2026").ast;
        let page = src.page(&store, ast.as_ref(), 0, PAGE);
        assert_eq!(
            names(&page),
            ["budget-2026.xlsx", "report-q3.pdf", "superapp-0.1.0.dmg"]
        );
        let ast = filter::parse("q3").ast;
        let page = src.page(&store, ast.as_ref(), 0, PAGE);
        assert_eq!(names(&page), ["report-q3.pdf"]);
        let row = page[0].clone();
        assert_eq!(src.index_of(&store, None, &row), Some(4));
    }

    /// What does not bind is dropped, as the SQL builder drops it — under
    /// `@not:` and inside a group too — never answered as true or false.
    #[test]
    fn clauses_that_do_not_bind_are_dropped_not_answered() {
        let store = Store::open(None).unwrap();
        let src = DirSource::new("~/Downloads");
        let count = |q: &str| src.count(&store, filter::parse(q).ast.as_ref()).unwrap();
        let all = count("");
        assert_eq!(count("@bogus"), all, "an unknown tag shows everything");
        assert_eq!(count("@not:bogus"), all, "…and so does its negation");
        assert_eq!(count("@size>abc"), all, "an unreadable value is dropped");
        assert_eq!(count("(@bogus @or @nope)"), all, "a group of nothing is nothing");
        assert_eq!(count("(@dir @or @bogus)"), count("@dir"), "a dropped member leaves the rest");
        assert_eq!(count("@not:dir"), all - count("@dir"));
        assert_eq!(count("@bogus @not:dir"), all - count("@dir"));
        assert_eq!(count("(@not:dir @or @bogus)"), all - count("@dir"));
    }

    #[test]
    fn paths_and_crumbs() {
        assert_eq!(join("~", "Downloads"), "~/Downloads");
        assert_eq!(join("/", "tmp"), "/tmp");
        assert_eq!(parent("~/Downloads/2026"), Some("~/Downloads"));
        assert_eq!(parent("~"), None);
        assert_eq!(parent("/tmp"), Some("/"));
        assert_eq!(parent("/"), None);
        assert_eq!(basename("~/Downloads/2026/notes.txt"), "notes.txt");
        assert_eq!(basename("~"), "~");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename("/tmp"), "tmp");
        assert_eq!(
            crumbs("~/Downloads/2026"),
            [
                ("~".to_string(), "~".to_string()),
                ("Downloads".to_string(), "~/Downloads".to_string()),
                ("2026".to_string(), "~/Downloads/2026".to_string()),
            ]
        );
        assert_eq!(
            crumbs("/tmp/superapp-e2e"),
            [
                ("/".to_string(), "/".to_string()),
                ("tmp".to_string(), "/tmp".to_string()),
                ("superapp-e2e".to_string(), "/tmp/superapp-e2e".to_string()),
            ]
        );
        assert!(exists("~/Downloads/2026"));
        assert!(!exists("~/Downloads/2027"));
        assert!(exists("/") && exists("/tmp") && is_dir("/tmp") && !is_dir("/tmp/notes.txt"));
        assert_eq!(names(&list("/")), ["Applications/", "etc/", "tmp/", "Users/"]);
        assert_eq!(names(&list("/tmp")), ["superapp-e2e/", ".keep", "notes.txt"]);
    }

    #[test]
    fn a_typed_path_is_read_the_way_the_tree_spells_it() {
        assert_eq!(normalize("~").as_deref(), Some("~"));
        assert_eq!(normalize("~/").as_deref(), Some("~"));
        assert_eq!(normalize("~/Downloads/").as_deref(), Some("~/Downloads"));
        assert_eq!(normalize("/tmp/").as_deref(), Some("/tmp"));
        assert_eq!(normalize("/").as_deref(), Some("/"));
        assert_eq!(normalize("/tmp/../etc/./hosts").as_deref(), Some("/etc/hosts"));
        assert_eq!(normalize("~/../.."), Some("~".into()), "a root does not climb out");
        assert_eq!(normalize("Downloads"), None, "relative spellings are not read");
        assert_eq!(normalize("  "), None);
        // A second root restarts the path (find-file's rule), so a typed
        // absolute path wins over the seeded one.
        assert_eq!(normalize("~/Downloads//tmp/").as_deref(), Some("/tmp"));
        assert_eq!(normalize("~/Downloads//").as_deref(), Some("/"));
        assert_eq!(normalize("/tmp/~/Downloads").as_deref(), Some("~/Downloads"));
        assert_eq!(normalize("/tmp/~").as_deref(), Some("~"));
        assert_eq!(normalize("~/a//b/~/c").as_deref(), Some("~/c"));
    }

    /// The path field completes like a shell's tab: the segment under the
    /// caret against the directory before it, directories with their
    /// slash so the next offer opens at once, the roots before a slash.
    #[test]
    fn the_path_field_completes_segment_by_segment() {
        let store = Store::open(None).unwrap();
        let c = PathCompletion;
        let labels = |text: &str| -> Vec<String> {
            let ctx = c.context(text, text.len()).unwrap();
            c.offer(&store, &ctx).into_iter().map(|s| s.label).collect()
        };
        assert_eq!(labels(""), ["~/", "/"]);
        assert_eq!(labels("~"), ["~/"]);
        assert_eq!(labels("/t"), ["tmp/"]);
        assert_eq!(labels("/tmp/"), ["superapp-e2e/", "notes.txt"], "dot-files wait for a dot");
        assert_eq!(labels("/tmp/."), [".keep"]);
        assert_eq!(labels("~/Dow"), ["Downloads/"]);
        // After the seed, a second root restarts: the offer follows.
        assert_eq!(labels("~/Downloads//t"), ["tmp/"]);
        assert_eq!(labels("~/Downloads/~/Pic"), ["Pictures/"]);
        // A prefix folds case, as the filter does everywhere else.
        assert_eq!(labels("~/Downloads/re"), ["README.txt", "report-q3.pdf"]);
        assert_eq!(labels("~/Downloads/rep"), ["report-q3.pdf"]);
        assert!(labels("~/nowhere/x").is_empty());
        // A pick replaces the segment and lands the caret after it.
        let ctx = c.context("~/Dow", 5).unwrap();
        let pick = Suggestion::value("Downloads/");
        assert_eq!(c.splice("~/Dow", 5, &ctx, &pick), ("~/Downloads/".into(), 12));
        let ctx = c.context("/t", 2).unwrap();
        assert_eq!(c.splice("/t", 2, &ctx, &Suggestion::value("tmp/")), ("/tmp/".into(), 5));
        // The root offer lands as a root.
        let ctx = c.context("", 0).unwrap();
        let roots = c.offer(&store, &ctx);
        assert_eq!(roots[1].value, "/");
        assert_eq!(c.splice("", 0, &ctx, &roots[0]), ("~/".into(), 2));
    }

    #[test]
    fn sizes_and_kinds() {
        assert_eq!(fmt_size(640), "640 B");
        assert_eq!(fmt_size(KB + 100), "1.1 KB");
        assert_eq!(fmt_size(84 * KB), "84 KB");
        assert_eq!(fmt_size(MB + 200 * KB), "1.2 MB");
        assert_eq!(fmt_size(38 * MB), "38 MB");
        assert_eq!(FileKind::of_name("photo.JPG"), FileKind::Image);
        assert_eq!(FileKind::of_name("logs.tar.gz"), FileKind::Archive);
        assert_eq!(FileKind::of_name(".DS_Store"), FileKind::Other);
        assert_eq!(FileKind::of_name("Lease.tla"), FileKind::Text);
        assert!(text_of("~/Downloads/README.txt").is_some());
        assert!(text_of("~/Downloads/report-q3.pdf").is_none());
        assert!(image_of("~/Downloads/2026/photo-lisbon.jpg").is_some());
        assert!(image_of("~/Downloads/README.txt").is_none());
    }
}
