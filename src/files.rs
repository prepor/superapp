//! The file browser's domain: what a directory lists, what a
//! file card shows, the path field's completion, and the one held item
//! `copy`/`move` carry to a `… here`.
//!
//! The disk is **outside** (see [`crate::effect::Outside`]): a listing is
//! read through it during draw, and `open` hands a path to the OS
//! through it. The [`demo`] tree is what the fake outside serves — the
//! panels library's worlds, the tests — and what a real world never
//! sees.
//!
//! Paths cross the boundary in two spellings: the **display** form the
//! panels show and persist (`~/Downloads/2026`, `/tmp`) and the real
//! [`Path`] the outside reads; [`real_path`] and [`display_path`] map
//! between them.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::effect::World;
use crate::filter::{Ast, Op};
use crate::richtable::{
    self, Completion, Datasource, Suggestion, TagDef, TagType, Values, MAX_SUGGESTIONS,
};
use crate::store::Store;

/// The root the launcher's `files` opens on.
pub const HOME: &str = "~";
/// The other root: the whole disk, for `go to /tmp`.
pub const ROOT: &str = "/";

/// How much of a text file the card reads.
pub const TEXT_PREVIEW_MAX: usize = 64 * 1024;
/// How much of an image the card decodes.
pub const IMAGE_PREVIEW_MAX: usize = 20 * 1024 * 1024;
/// How big a file compose will carry out as a part (CR-010). Past this the
/// attach refuses on the panel's status line rather than building a mail no
/// server will take.
pub const ATTACH_MAX: u64 = 25 * 1024 * 1024;

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

    /// An entry off the disk's own account of a file.
    #[must_use]
    pub fn from_metadata(name: &str, meta: &std::fs::Metadata) -> Entry {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64());
        Entry {
            name: name.to_string(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified,
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

/// The media type a name claims. What a part compose carries out is
/// labelled with, and what a card shows when the kind word says little —
/// a short table over the kinds this app actually meets, and
/// `application/octet-stream` for everything else, which is the honest
/// answer rather than a guess.
#[must_use]
pub fn mime_of(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("html") => "text/html",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("yaml" | "yml") => "application/yaml",
        Some("rs" | "toml" | "tla" | "cfg" | "sh") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Which decoder a picture *probably* wants, off its name — enough to
/// decide whether to read the file at all. What actually decodes it is
/// [`sniff`], on the bytes: a name lies often enough (a PNG saved as
/// `.jpg`) that trusting it would leave a picture unshown.
#[must_use]
pub fn image_format(name: &str) -> Option<ImageFormat> {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("png") => Some(ImageFormat::Png),
        Some("jpg" | "jpeg") => Some(ImageFormat::Jpeg),
        _ => None,
    }
}

/// The two picture formats the card decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

/// What these bytes actually are, by their magic: PNG's signature, a
/// JPEG's `FF D8 FF`. The card decodes by this, never by the name.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else {
        None
    }
}

/// A picture's `(width, height)` in pixels off its header alone — the
/// PNG's IHDR, a JPEG's first frame marker — so the card can wish its
/// rows before anything is decoded. `None` for what is not one.
#[must_use]
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    // PNG: an 8-byte signature, then the IHDR chunk: length, "IHDR",
    // width, height — big-endian.
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return (w > 0 && h > 0).then_some((w, h));
    }
    // JPEG: segments of `FF xx len…`; the size sits in the first SOF
    // segment (C0–CF, not the tables at C4, C8, CC).
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2;
        while i + 4 <= bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
                i += 2;
                continue;
            }
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            let sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
            if sof {
                if i + 9 > bytes.len() {
                    return None;
                }
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]);
                return (w > 0 && h > 0).then_some((u32::from(w), u32::from(h)));
            }
            i += 2 + len.max(2);
        }
    }
    None
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

/// Where home is on this machine, for the two spellings to meet.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The display spelling as the path the outside reads: `~` is home.
#[must_use]
pub fn real_path(display: &str) -> PathBuf {
    match display.strip_prefix('~') {
        Some(rest) => {
            let mut p = home_dir();
            for seg in rest.split('/').filter(|s| !s.is_empty()) {
                p.push(seg);
            }
            p
        }
        None => PathBuf::from(display),
    }
}

/// A real path as the panels spell it: home and below as `~/…`.
#[must_use]
pub fn display_path(path: &Path) -> String {
    let home = home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => HOME.to_string(),
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
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

/// Directories first, then names, case folded — the one order a listing
/// has, whichever outside produced it.
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

// -- the disk, through the outside --------------------------------------------

/// A directory's listing, or why there is none — through the world's
/// outside, in the display spelling the panels use.
pub fn list_in(world: &World, dir: &str) -> Result<Vec<Entry>, String> {
    world.outside(|o| o.list_dir(&real_path(dir)))
}

/// One path's entry, if the disk has it.
pub fn stat_in(world: &World, path: &str) -> Option<Entry> {
    world.outside(|o| o.stat(&real_path(path))).ok().flatten()
}

/// Whether the disk has this path as a directory.
pub fn is_dir_in(world: &World, path: &str) -> bool {
    stat_in(world, path).is_some_and(|e| e.is_dir)
}

/// The first `max` bytes of a file, through the outside.
pub fn read_in(world: &World, path: &str, max: usize) -> Result<Vec<u8>, String> {
    world.outside(|o| o.read_file(&real_path(path), max))
}

// -- the card ------------------------------------------------------------------

/// What a card draws, whichever side it came from: a file on the disk
/// (CR-008) or a part of a letter (CR-010). The panel reads this and
/// nothing else, so one widget serves both — which is the whole reason
/// attachments were cheap to build once the browser existed.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// The big line: the file's name, the part's filename.
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    /// The muted line: when a file last changed, who a part came with.
    pub when: String,
    /// The selectable line under it: the path, or the part's media type.
    pub detail: String,
}

impl Card {
    /// The line beside the name: what it is and how big — `pdf · 96 KB`.
    #[must_use]
    pub fn kind_line(&self) -> String {
        format!("{} · {}", self.kind.word(), fmt_size(self.size))
    }
}

/// The card a path makes; `None` when the disk no longer has it.
#[must_use]
pub fn disk_card(world: &World, path: &str) -> Option<Card> {
    let e = stat_in(world, path)?;
    Some(Card {
        name: e.name.clone(),
        kind: e.kind(),
        size: e.size,
        when: format!("modified {}", crate::mail::fmt_date(e.modified)),
        detail: path.to_string(),
    })
}

/// What a card shows under the rule: a text file's reading, a picture's
/// bytes, or nothing at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Preview {
    Text(String),
    Image(Vec<u8>),
    None,
}

/// The preview a card of this kind wants, read through `read` — which is
/// handed the cap and answers `None` when the bytes cannot be had (yet).
/// The kind decides *whether* to read at all: a 38 MB disk image is never
/// pulled into a panel to be told it is not a picture.
pub fn preview_of(
    kind: FileKind,
    name: &str,
    read: impl FnOnce(usize) -> Option<Vec<u8>>,
) -> Preview {
    match kind {
        FileKind::Text => match read(TEXT_PREVIEW_MAX) {
            Some(b) => Preview::Text(String::from_utf8_lossy(&b).into_owned()),
            None => Preview::None,
        },
        // The name says whether to read it; the bytes say how to decode it.
        FileKind::Image if image_format(name).is_some() => match read(IMAGE_PREVIEW_MAX) {
            Some(b) => Preview::Image(b),
            None => Preview::None,
        },
        _ => Preview::None,
    }
}

/// Where a letter's part lands when it is opened (CR-010): the app's own
/// scratch directory, **a folder per part**, so nothing can be overwritten
/// by anything — not another letter's part, and not this letter's second
/// `image.png`, which is a shape mail actually arrives in. The folder
/// carries the disambiguation so the file keeps the name the sender gave
/// it, which is the name the viewer will put in its title bar. An ordinary
/// directory either way, so a files panel can walk to it afterwards.
#[must_use]
pub fn scratch(mail: i64, at: u32, name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("superapp-parts")
        .join(format!("mail-{mail}"))
        .join(format!("part-{at}"))
        // A part's filename comes off the wire: the last segment of it is
        // all that may reach the disk, and never `..`.
        .join(safe_name(name))
}

/// A filename from outside as a single, harmless segment: no separators, no
/// climbing, never empty.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if last.is_empty() || last == "." || last == ".." {
        "part".to_string()
    } else {
        last.to_string()
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

/// What is held, process-wide: context, not history. A **set** of paths
/// (CR-009): a panel's own `copy`/`move` holds the one thing it shows, a
/// marked list holds every marked row, and a `… here` performs the set —
/// refusing per path exactly as it does for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub op: HoldOp,
    pub paths: Vec<String>,
}

impl Hold {
    /// One object held: what a panel's own verb holds.
    #[must_use]
    pub fn one(op: HoldOp, path: impl Into<String>) -> Hold {
        Hold {
            op,
            paths: vec![path.into()],
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// What a toast calls it: the name where one thing is held, the count
    /// where a set is.
    #[must_use]
    pub fn what(&self) -> String {
        match self.paths.as_slice() {
            [one] => format!("“{}”", basename(one)),
            many => plural(many.len()),
        }
    }
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

// -- the demo tree -----------------------------------------------------------

/// A home directory a design review can walk, and a little beyond it:
/// what the fake outside serves — the panels library's worlds, the tests.
/// Display spellings throughout.
pub mod demo {
    use super::{basename, parent, sort, Entry, FileKind, HOME, ROOT};

    struct Fx {
        path: &'static str,
        dir: bool,
        size: u64,
        /// `(year, month, day, hour, minute)`.
        at: (i64, u32, u32, u32, u32),
    }

    pub(super) const KB: u64 = 1024;
    pub(super) const MB: u64 = 1024 * 1024;

    const TREE: &[Fx] = &[
        Fx { path: "~/Desktop", dir: true, size: 0, at: (2026, 8, 31, 18, 40) },
        Fx { path: "~/Documents", dir: true, size: 0, at: (2026, 8, 29, 11, 5) },
        Fx { path: "~/Downloads", dir: true, size: 0, at: (2026, 9, 1, 9, 12) },
        Fx { path: "~/Pictures", dir: true, size: 0, at: (2026, 8, 24, 20, 3) },
        Fx { path: "~/superapp", dir: true, size: 0, at: (2026, 9, 2, 7, 30) },
        Fx { path: "~/.config", dir: true, size: 0, at: (2026, 7, 14, 10, 0) },
        Fx { path: "~/notes.md", dir: false, size: 2 * KB + 130, at: (2026, 8, 30, 22, 47) },
        Fx { path: "~/.zshrc", dir: false, size: KB + 90, at: (2026, 6, 2, 9, 0) },
        Fx { path: "~/Desktop/todo.txt", dir: false, size: 300, at: (2026, 8, 31, 18, 40) },
        Fx { path: "~/Documents/panel-model.md", dir: false, size: 9 * KB, at: (2026, 8, 29, 11, 5) },
        Fx { path: "~/Documents/interaction-grammar.md", dir: false, size: 14 * KB, at: (2026, 9, 2, 6, 55) },
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

    /// Whether the tree has this path as a directory.
    #[must_use]
    pub fn is_dir(path: &str) -> bool {
        path == HOME || path == ROOT || TREE.iter().any(|f| f.path == path && f.dir)
    }

    /// The entry at a path, if the tree has it; a root is a directory.
    #[must_use]
    pub fn entry(path: &str) -> Option<Entry> {
        if path == HOME || path == ROOT {
            return Some(Entry {
                name: path.to_string(),
                is_dir: true,
                size: 0,
                modified: crate::mail::ts(2026, 9, 2, 7, 30),
            });
        }
        TREE.iter().find(|f| f.path == path).map(entry_of)
    }

    /// A directory's listing, unfiltered: directories first, then files,
    /// by name. Dot-files included — the filter decides. `None` for a
    /// directory the tree does not have.
    #[must_use]
    pub fn list(dir: &str) -> Option<Vec<Entry>> {
        if !is_dir(dir) {
            return None;
        }
        let mut v: Vec<Entry> = TREE
            .iter()
            .filter(|f| parent(f.path) == Some(dir))
            .map(entry_of)
            .collect();
        sort(&mut v);
        Some(v)
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
            "todo.txt" => "- files: the card previews\n- files: move here / copy here\n- attachments: save a part where I choose\n- rename?".into(),
            "notes.txt" => "Lisbon, August.\n\nInvoice 0817 is for the flat; the photos are from the last evening.".into(),
            "notes.md" => "# notes\n\n- a directory is a list panel\n- a file is a card\n- enter goes, the cursor previews\n\nThe join is the only relation.".into(),
            "panel-context.md" => "# panel: files ~/Downloads\n\nfilter: @kind:image\nentries: 8 (1 shown)\nlisted: 0.4 s ago".into(),
            _ => format!("{}\n\n(the first 64 KB of the file, in the app's one face)", e.name),
        })
    }

    /// A file's bytes, for the card's preview: a text file's reading, or
    /// — the demo tree has no pictures of its own — the app icon as PNG
    /// for every image. `None` for what the tree does not have.
    #[must_use]
    pub fn bytes_of(path: &str) -> Option<Vec<u8>> {
        let e = entry(path)?;
        match e.kind() {
            FileKind::Text => text_of(path).map(String::into_bytes),
            FileKind::Image => Some(include_bytes!("../resources/icon_256.png").to_vec()),
            _ => Some(Vec::new()),
        }
    }
}

// -- the path field's completion ----------------------------------------------

/// The `go to` field as a completion: the segment under the caret,
/// matched as a prefix against the entries of the directory the segments
/// before it name — a shell's tab, in the rich table's box. A picked
/// directory lands with its slash, so the next offer opens at once; a
/// root is offered when nothing is typed yet. The listing comes through
/// the world's outside, like the panel's own.
pub struct PathCompletion {
    pub world: Rc<World>,
}

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
            return [(HOME, "~/"), (ROOT, ROOT)]
                .iter()
                .filter(|(r, _)| ctx.prefix.is_empty() || r.starts_with(ctx.prefix.as_str()))
                .map(|(_, v)| Suggestion::value(*v))
                .collect();
        };
        let prefix = ctx.prefix.to_lowercase();
        let hidden = prefix.starts_with('.');
        let mut out: Vec<Suggestion> = list_in(&self.world, dir)
            .unwrap_or_default()
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

/// One directory as a rich-table datasource: the listing in memory — read
/// through the outside when the panel opened on the directory — and the
/// filter evaluated over it. The watcher that keeps it true is the next
/// step; until then a panel re-lists when its directory changes.
#[derive(Debug, Clone, PartialEq)]
pub struct DirSource {
    pub dir: String,
    pub entries: Rc<Vec<Entry>>,
}

impl DirSource {
    #[must_use]
    pub fn new(dir: &str, entries: Vec<Entry>) -> Self {
        DirSource {
            dir: dir.to_string(),
            entries: Rc::new(entries),
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
    type Row = Entry;
    /// A row is its name: unique within the one directory a source lists,
    /// and what a mark would hold (CR-009).
    type Key = String;

    fn tags(&self) -> &'static [TagDef] {
        TAGS
    }

    fn key(&self, row: &Entry) -> String {
        row.name.clone()
    }

    /// Every name the filter shows, in the table's order — what `all`
    /// marks (CR-009). The listing is in memory, so this is the order
    /// itself, read once.
    fn keys(&self, _store: &Store, ast: Option<&Ast>) -> Option<Vec<String>> {
        Some(self.filtered(ast).into_iter().map(|e| e.name).collect())
    }

    /// Which of these names the filter still shows; the rest are the marks
    /// it hides. The caller's order is kept.
    fn present(&self, _store: &Store, ast: Option<&Ast>, keys: &[String]) -> Vec<String> {
        let shown: std::collections::BTreeSet<String> =
            self.filtered(ast).into_iter().map(|e| e.name).collect();
        keys.iter().filter(|k| shown.contains(*k)).cloned().collect()
    }

    /// The entry by name, filter or no filter: a directory's own listing
    /// is its base condition, and a dot-file the filter hid is hidden,
    /// not gone.
    fn by_key(&self, _store: &Store, key: &String) -> Option<Entry> {
        self.entries.iter().find(|e| &e.name == key).cloned()
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

    fn downloads() -> DirSource {
        DirSource::new("~/Downloads", demo::list("~/Downloads").unwrap())
    }

    #[test]
    fn a_listing_puts_directories_first_then_names() {
        let l = demo::list("~/Downloads").unwrap();
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
        assert!(demo::list("~/nowhere").is_none());
        assert_eq!(names(&demo::list("/").unwrap()), ["Applications/", "etc/", "tmp/", "Users/"]);
        assert_eq!(names(&demo::list("/tmp").unwrap()), ["superapp-e2e/", ".keep", "notes.txt"]);
    }

    #[test]
    fn the_filter_hides_dot_files_unless_asked() {
        let store = Store::open(None).unwrap();
        let src = downloads();
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

    /// The three questions a mark asks (CR-009), answered off the listing
    /// the source already holds: every name under the filter, which of
    /// these it still shows, and the entry by name whatever the filter —
    /// a directory's own listing is its base `WHERE`, so a dot-file the
    /// filter hides is hidden, not gone.
    #[test]
    fn a_listing_answers_the_marks_three_questions() {
        let store = Store::open(None).unwrap();
        let src = downloads();
        let keys = |q: &str| src.keys(&store, filter::parse(q).ast.as_ref()).unwrap();
        assert_eq!(
            keys(""),
            [
                "2026",
                "budget-2026.xlsx",
                "logs.tar.gz",
                "README.txt",
                "report-q3.pdf",
                "screenshot-2026-08-30.png",
                "superapp-0.1.0.dmg",
            ],
            "the filtered names, in the table's order"
        );
        assert_eq!(keys("@hidden")[1], ".DS_Store");
        assert_eq!(keys("@dir"), ["2026"]);
        // A set marked under `@hidden`, then read back without it: the
        // dot-file is the one the filter hides.
        let marked = vec!["2026".to_string(), ".DS_Store".to_string()];
        let hidden = filter::parse("@hidden").ast;
        assert_eq!(src.present(&store, hidden.as_ref(), &marked), marked);
        assert_eq!(src.present(&store, None, &marked), ["2026"]);
        let none: [String; 0] = [];
        assert_eq!(src.present(&store, filter::parse("q3").ast.as_ref(), &marked), none);
        // …and it is still an entry: the row a hidden mark draws.
        let dot = src.by_key(&store, &".DS_Store".to_string());
        assert_eq!(dot.map(|e| e.size), Some(6 * demo::KB));
        assert_eq!(src.by_key(&store, &"2026".to_string()).map(|e| e.is_dir), Some(true));
        assert_eq!(src.by_key(&store, &"gone.txt".to_string()), None, "a mark whose entry left");
    }

    /// What does not bind is dropped, as the SQL builder drops it — under
    /// `@not:` and inside a group too — never answered as true or false.
    #[test]
    fn clauses_that_do_not_bind_are_dropped_not_answered() {
        let store = Store::open(None).unwrap();
        let src = downloads();
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
        assert!(demo::exists("~/Downloads/2026"));
        assert!(!demo::exists("~/Downloads/2027"));
        assert!(demo::exists("/") && demo::exists("/tmp") && demo::is_dir("/tmp"));
        assert!(!demo::is_dir("/tmp/notes.txt"));
    }

    /// The two spellings meet at home: `~` is `$HOME` on the way out and
    /// `$HOME` is `~` on the way back; the rest of the disk is itself.
    #[test]
    fn display_and_real_paths_round_trip() {
        let home = home_dir();
        assert_eq!(real_path("~"), home);
        assert_eq!(real_path("~/Downloads/2026"), home.join("Downloads").join("2026"));
        assert_eq!(real_path("/tmp"), PathBuf::from("/tmp"));
        assert_eq!(real_path("/"), PathBuf::from("/"));
        assert_eq!(display_path(&home), "~");
        assert_eq!(display_path(&home.join("Downloads")), "~/Downloads");
        assert_eq!(display_path(Path::new("/tmp/x")), "/tmp/x");
        for d in ["~", "~/Downloads/2026", "/tmp", "/", "/etc/hosts"] {
            assert_eq!(display_path(&real_path(d)), d);
        }
    }

    #[test]
    fn a_typed_path_is_read_the_way_the_panels_spell_it() {
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

    #[test]
    fn sizes_and_kinds() {
        assert_eq!(fmt_size(640), "640 B");
        assert_eq!(fmt_size(demo::KB + 100), "1.1 KB");
        assert_eq!(fmt_size(84 * demo::KB), "84 KB");
        assert_eq!(fmt_size(demo::MB + 200 * demo::KB), "1.2 MB");
        assert_eq!(fmt_size(38 * demo::MB), "38 MB");
        assert_eq!(FileKind::of_name("photo.JPG"), FileKind::Image);
        assert_eq!(FileKind::of_name("logs.tar.gz"), FileKind::Archive);
        assert_eq!(FileKind::of_name(".DS_Store"), FileKind::Other);
        assert_eq!(FileKind::of_name("Lease.tla"), FileKind::Text);
        assert_eq!(image_format("a.jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format("a.gif"), None);
        // What decodes is the bytes' own account of themselves: the demo
        // tree's `.jpg` is a PNG, and it draws.
        let icon = include_bytes!("../resources/icon_256.png");
        assert_eq!(sniff(icon), Some(ImageFormat::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageFormat::Jpeg));
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(&demo::bytes_of("~/Downloads/2026/photo-lisbon.jpg").unwrap()), Some(ImageFormat::Png));
        // The card's wish reads a picture's size off its header alone.
        let icon = include_bytes!("../resources/icon_256.png");
        assert_eq!(image_size(icon), Some((256, 256)));
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x01, 0x90, 0x02, 0x80, 0x01, 0x01, 0x11, 0x00]);
        assert_eq!(image_size(&jpeg), Some((640, 400)));
        assert_eq!(image_size(b"not a picture"), None);
        assert_eq!(text_lines("", 40), 1);
        assert_eq!(text_lines("a\nb\nc", 40), 3);
        assert_eq!(text_lines(&"x".repeat(100), 40), 3, "a long line wraps");
        assert!(demo::text_of("~/Downloads/README.txt").is_some());
        assert!(demo::text_of("~/Downloads/report-q3.pdf").is_none());
        assert!(demo::bytes_of("~/Downloads/2026/photo-lisbon.jpg").is_some_and(|b| !b.is_empty()));
        assert!(demo::bytes_of("~/Downloads/nope.txt").is_none());
    }

    /// The card and its preview are the two sides' one vocabulary
    /// (CR-010): the kind decides whether the bytes are worth reading at
    /// all, and a source that cannot answer yet is a card with no preview
    /// rather than a card that says there is none.
    #[test]
    fn one_card_serves_the_disk_and_a_letter() {
        let w = World::fake(crate::effect::Registry::new());
        let card = disk_card(&w, "~/Downloads/README.txt").unwrap();
        assert_eq!(card.name, "README.txt");
        assert_eq!(card.kind_line(), "text · 640 B");
        assert_eq!(card.when, "modified aug 12 16:45");
        assert_eq!(card.detail, "~/Downloads/README.txt");
        assert_eq!(disk_card(&w, "~/Downloads/gone"), None);

        // Text is read; a picture is read by its name and decoded by its
        // bytes; everything else is never read at all — which is the point,
        // since the alternative is pulling 38 MB into a panel to be told it
        // is a disk image.
        let asked = std::cell::Cell::new(0);
        let read = |max: usize| {
            asked.set(asked.get() + 1);
            read_in(&w, "~/Downloads/README.txt", max).ok()
        };
        assert!(matches!(
            preview_of(FileKind::Text, "README.txt", read),
            Preview::Text(t) if t.starts_with("superapp 0.1.0")
        ));
        assert_eq!(asked.get(), 1);
        let never = |_: usize| -> Option<Vec<u8>> { panic!("read for a kind with no preview") };
        assert_eq!(preview_of(FileKind::Pdf, "a.pdf", never), Preview::None);
        assert_eq!(preview_of(FileKind::Archive, "a.zip", never), Preview::None);
        assert_eq!(preview_of(FileKind::Dir, "d", never), Preview::None);
        // A `.gif` is an image the card cannot decode: not read either.
        assert_eq!(preview_of(FileKind::Image, "a.gif", never), Preview::None);
        // Bytes still on their way: no preview, and nothing decided.
        assert_eq!(preview_of(FileKind::Text, "a.txt", |_| None), Preview::None);
        let icon = include_bytes!("../resources/icon_256.png").to_vec();
        assert_eq!(
            preview_of(FileKind::Image, "a.png", |_| Some(icon.clone())),
            Preview::Image(icon)
        );
    }

    /// A name off the wire reaches the disk as one harmless segment, under
    /// the app's own scratch directory and never anywhere else (CR-010).
    #[test]
    fn a_parts_name_cannot_climb_out_of_the_scratch_directory() {
        let root = std::env::temp_dir().join("superapp-parts").join("mail-7");
        let p2 = root.join("part-2");
        assert_eq!(scratch(7, 2, "invoice.pdf"), p2.join("invoice.pdf"));
        assert_eq!(scratch(7, 2, "../../etc/passwd"), p2.join("passwd"));
        assert_eq!(scratch(7, 2, "a/b/c.txt"), p2.join("c.txt"));
        assert_eq!(scratch(7, 2, ".."), p2.join("part"));
        assert_eq!(scratch(7, 2, "  "), p2.join("part"));
        assert_eq!(safe_name("C:\\Windows\\x.dll"), "x.dll");
        // Two parts of one letter under one name land apart, which is the
        // whole reason the part is in the path: mail really does carry two
        // `image.png`s.
        assert_ne!(scratch(7, 2, "image.png"), scratch(7, 3, "image.png"));
        assert_ne!(scratch(7, 2, "image.png"), scratch(8, 2, "image.png"));
    }

    /// What an outgoing part is labelled with, and what a card falls back
    /// to: a short table, and the honest answer past it.
    #[test]
    fn a_name_claims_a_media_type() {
        assert_eq!(mime_of("q3.CSV"), "text/csv");
        assert_eq!(mime_of("report-q3.pdf"), "application/pdf");
        assert_eq!(mime_of("photo.jpeg"), "image/jpeg");
        assert_eq!(mime_of("logs.tar.gz"), "application/gzip");
        assert_eq!(mime_of("superapp.db"), "application/octet-stream");
        assert_eq!(mime_of("noextension"), "application/octet-stream");
    }

    /// The fake outside serves the demo tree through the same verbs the
    /// real one reads the disk with, in the panels' spelling.
    #[test]
    fn the_fake_outside_serves_the_demo_tree() {
        let w = World::fake(crate::effect::Registry::new());
        assert_eq!(
            names(&list_in(&w, "~/Downloads").unwrap()),
            names(&demo::list("~/Downloads").unwrap())
        );
        assert!(list_in(&w, "~/nowhere").is_err());
        assert!(is_dir_in(&w, "/tmp"));
        assert!(!is_dir_in(&w, "/tmp/notes.txt"));
        assert_eq!(stat_in(&w, "~/Downloads/README.txt").map(|e| e.size), Some(640));
        assert_eq!(stat_in(&w, "~/Downloads/none"), None);
        let bytes = read_in(&w, "~/Downloads/README.txt", 16).unwrap();
        assert_eq!(bytes.len(), 16, "a read stops at the cap");
        assert!(read_in(&w, "~/Downloads/none", 16).is_err());
        // `open` is an effect: it hands the real path to the outside and
        // the fake records it.
        let real = real_path("~/Downloads/report-q3.pdf");
        w.run(&crate::effect::OpenPath { path: &real }).unwrap();
        assert_eq!(w.with_fake(|f| f.opened.clone()), vec![real]);
        // A world with no outside refuses, loudly, rather than pretending.
        let deny = World::new(
            Rc::new(Store::open(None).unwrap()),
            Box::new(crate::effect::Deny::with_clock(crate::effect::Clock::Virtual(
                std::sync::Arc::new(std::sync::Mutex::new(0.0)),
            ))),
            crate::effect::Registry::new(),
        );
        assert!(list_in(&deny, "~").unwrap_err().contains("no outside"));
    }

    /// The path field completes like a shell's tab: the segment under the
    /// caret against the directory before it, directories with their
    /// slash so the next offer opens at once, the roots before a slash.
    #[test]
    fn the_path_field_completes_segment_by_segment() {
        let store = Store::open(None).unwrap();
        let c = PathCompletion {
            world: Rc::new(World::fake(crate::effect::Registry::new())),
        };
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
}
