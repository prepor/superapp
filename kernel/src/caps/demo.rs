//! The fixture: a machine-independent `~` a suite can address a row of by
//! name. Written as well as read, so the verbs act on it exactly as they
//! act on a filesystem and a suite proves them rather than a draft of them.

use std::collections::BTreeMap;

use super::{basename, is_root, join, parent, sort, Entry, FileId, FileKind, HOME, ROOT};
use crate::time::ts;

struct Fx {
    path: &'static str,
    dir: bool,
    size: u64,
    /// `(year, month, day, hour, minute)`.
    at: (i64, u32, u32, u32, u32),
}

const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;

const TREE: &[Fx] = &[
    Fx {
        path: "~/Desktop",
        dir: true,
        size: 0,
        at: (2026, 8, 31, 18, 40),
    },
    Fx {
        path: "~/Documents",
        dir: true,
        size: 0,
        at: (2026, 8, 29, 11, 5),
    },
    Fx {
        path: "~/Downloads",
        dir: true,
        size: 0,
        at: (2026, 9, 1, 9, 12),
    },
    Fx {
        path: "~/Pictures",
        dir: true,
        size: 0,
        at: (2026, 8, 24, 20, 3),
    },
    Fx {
        path: "~/superapp",
        dir: true,
        size: 0,
        at: (2026, 9, 2, 7, 30),
    },
    Fx {
        path: "~/.config",
        dir: true,
        size: 0,
        at: (2026, 7, 14, 10, 0),
    },
    Fx {
        path: "~/notes.md",
        dir: false,
        size: 2 * KB + 130,
        at: (2026, 8, 30, 22, 47),
    },
    Fx {
        path: "~/.zshrc",
        dir: false,
        size: KB + 90,
        at: (2026, 6, 2, 9, 0),
    },
    Fx {
        path: "~/Desktop/todo.txt",
        dir: false,
        size: 300,
        at: (2026, 8, 31, 18, 40),
    },
    Fx {
        path: "~/Documents/panel-model.md",
        dir: false,
        size: 9 * KB,
        at: (2026, 8, 29, 11, 5),
    },
    Fx {
        path: "~/Documents/interaction-grammar.md",
        dir: false,
        size: 14 * KB,
        at: (2026, 9, 2, 6, 55),
    },
    Fx {
        path: "~/Documents/Lease.tla",
        dir: false,
        size: 5 * KB,
        at: (2026, 8, 28, 15, 20),
    },
    Fx {
        path: "~/Downloads/2026",
        dir: true,
        size: 0,
        at: (2026, 8, 17, 12, 0),
    },
    Fx {
        path: "~/Downloads/report-q3.pdf",
        dir: false,
        size: MB + 200 * KB,
        at: (2026, 8, 31, 9, 14),
    },
    Fx {
        path: "~/Downloads/budget-2026.xlsx",
        dir: false,
        size: 84 * KB,
        at: (2026, 8, 31, 9, 14),
    },
    Fx {
        path: "~/Downloads/screenshot-2026-08-30.png",
        dir: false,
        size: 412 * KB,
        at: (2026, 8, 30, 14, 2),
    },
    Fx {
        path: "~/Downloads/superapp-0.1.0.dmg",
        dir: false,
        size: 38 * MB,
        at: (2026, 9, 1, 9, 12),
    },
    Fx {
        path: "~/Downloads/logs.tar.gz",
        dir: false,
        size: 3 * MB + 400 * KB,
        at: (2026, 8, 30, 7, 30),
    },
    Fx {
        path: "~/Downloads/README.txt",
        dir: false,
        size: 640,
        at: (2026, 8, 12, 16, 45),
    },
    Fx {
        path: "~/Downloads/.DS_Store",
        dir: false,
        size: 6 * KB,
        at: (2026, 9, 1, 9, 12),
    },
    Fx {
        path: "~/Downloads/2026/invoice-0817.pdf",
        dir: false,
        size: 96 * KB,
        at: (2026, 8, 17, 12, 0),
    },
    Fx {
        path: "~/Downloads/2026/photo-lisbon.jpg",
        dir: false,
        size: 2 * MB + 800 * KB,
        at: (2026, 8, 3, 19, 21),
    },
    Fx {
        path: "~/Downloads/2026/notes.txt",
        dir: false,
        size: KB + 100,
        at: (2026, 8, 17, 12, 0),
    },
    Fx {
        path: "~/Pictures/lisbon",
        dir: true,
        size: 0,
        at: (2026, 8, 3, 19, 21),
    },
    Fx {
        path: "~/Pictures/fold-cover.png",
        dir: false,
        size: MB + 100 * KB,
        at: (2026, 8, 24, 20, 3),
    },
    Fx {
        path: "~/Pictures/lisbon/IMG_0417.jpg",
        dir: false,
        size: 3 * MB + 200 * KB,
        at: (2026, 8, 3, 19, 21),
    },
    Fx {
        path: "~/Pictures/lisbon/IMG_0418.jpg",
        dir: false,
        size: 3 * MB,
        at: (2026, 8, 3, 19, 24),
    },
    Fx {
        path: "~/superapp/files",
        dir: true,
        size: 0,
        at: (2026, 9, 2, 7, 30),
    },
    Fx {
        path: "~/superapp/superapp.db",
        dir: false,
        size: 24 * MB,
        at: (2026, 9, 2, 7, 30),
    },
    // Beyond home: what a typed path reaches.
    Fx {
        path: "/Applications",
        dir: true,
        size: 0,
        at: (2026, 8, 20, 10, 0),
    },
    Fx {
        path: "/Users",
        dir: true,
        size: 0,
        at: (2026, 6, 1, 9, 0),
    },
    Fx {
        path: "/Users/andrey",
        dir: true,
        size: 0,
        at: (2026, 9, 2, 7, 30),
    },
    Fx {
        path: "/etc",
        dir: true,
        size: 0,
        at: (2026, 7, 14, 10, 0),
    },
    Fx {
        path: "/etc/hosts",
        dir: false,
        size: 213,
        at: (2026, 7, 14, 10, 0),
    },
    Fx {
        path: "/tmp",
        dir: true,
        size: 0,
        at: (2026, 9, 2, 12, 40),
    },
    Fx {
        path: "/tmp/superapp-e2e",
        dir: true,
        size: 0,
        at: (2026, 9, 2, 12, 40),
    },
    Fx {
        path: "/tmp/superapp-e2e/frames",
        dir: true,
        size: 0,
        at: (2026, 9, 2, 12, 41),
    },
    Fx {
        path: "/tmp/superapp-e2e/superapp.db",
        dir: false,
        size: 2 * MB,
        at: (2026, 9, 2, 12, 40),
    },
    Fx {
        path: "/tmp/notes.txt",
        dir: false,
        size: 380,
        at: (2026, 9, 1, 18, 5),
    },
    Fx {
        path: "/tmp/.keep",
        dir: false,
        size: 0,
        at: (2026, 9, 1, 18, 5),
    },
];

/// The fixture materialised into one map, so `new dir`, a copy, a move
/// and the trash act on it exactly as they act on a real disk.
#[derive(Debug, Clone)]
pub struct Tree {
    /// Display path → what is there. The two roots are in here as
    /// directories, so nothing special-cases them.
    nodes: BTreeMap<String, Node>,
    /// The next object number. A fixture needs identity for the same
    /// reason a disk does — a reversal asks whether the thing at a path
    /// is the thing it put there — so a node gets one when it is
    /// **made**: a move carries it (a rename keeps the inode) and a copy
    /// takes a fresh one (a copy is another object).
    next: u64,
}

#[derive(Debug, Clone)]
struct Node {
    entry: Entry,
    bytes: Vec<u8>,
    id: u64,
}

impl Default for Tree {
    fn default() -> Tree {
        Tree::new()
    }
}

impl Tree {
    /// The fixture, as it stands before anything has written to it.
    #[must_use]
    pub fn new() -> Tree {
        let mut d = Tree {
            nodes: BTreeMap::new(),
            next: 1,
        };
        for root in [HOME, ROOT] {
            if let Some(e) = fixture_entry(root) {
                d.put(root, e, Vec::new());
            }
        }
        for fx in TREE {
            d.put(fx.path, entry_of(fx), bytes_of(fx.path).unwrap_or_default());
        }
        d
    }

    /// A node with a fresh object number — what making something does.
    fn put(&mut self, path: &str, entry: Entry, bytes: Vec<u8>) {
        let id = self.next;
        self.next += 1;
        self.nodes
            .insert(path.to_string(), Node { entry, bytes, id });
    }

    fn dir_at(&self, path: &str) -> bool {
        self.nodes.get(path).is_some_and(|n| n.entry.is_dir)
    }

    /// The entry at a path, `None` for what the tree does not have.
    #[must_use]
    pub fn entry(&self, path: &str) -> Option<Entry> {
        self.nodes.get(path).map(|n| n.entry.clone())
    }

    /// The object at a path, as this tree numbers them — the fixture's
    /// answer to `lstat`'s device and inode.
    #[must_use]
    pub fn id(&self, path: &str) -> Option<FileId> {
        self.nodes.get(path).map(|n| FileId { dev: 1, ino: n.id })
    }

    /// One directory's listing, in the browser's order.
    ///
    /// # Errors
    ///
    /// If there is no such directory.
    pub fn list(&self, dir: &str) -> Result<Vec<Entry>, String> {
        if !self.dir_at(dir) {
            return Err(format!("{dir}: no such directory"));
        }
        let mut v: Vec<Entry> = self
            .nodes
            .iter()
            .filter(|(p, _)| parent(p) == Some(dir))
            .map(|(_, n)| n.entry.clone())
            .collect();
        sort(&mut v);
        Ok(v)
    }

    /// The first `max` bytes of a file.
    ///
    /// # Errors
    ///
    /// If there is no such file.
    pub fn read(&self, path: &str, max: usize) -> Result<Vec<u8>, String> {
        let node = self
            .nodes
            .get(path)
            .ok_or_else(|| format!("{path}: no such file"))?;
        let mut out = node.bytes.clone();
        out.truncate(max);
        Ok(out)
    }

    /// A path and everything under it — what a copy, a move and the
    /// trash carry as one.
    fn subtree(&self, path: &str) -> Vec<String> {
        let under = format!("{path}/");
        self.nodes
            .keys()
            .filter(|p| p.as_str() == path || p.starts_with(&under))
            .cloned()
            .collect()
    }

    /// What every verb that writes asks of a destination first: it is
    /// not a root, nothing is there, and its directory is.
    fn free(&self, path: &str) -> Result<(), String> {
        if is_root(path) {
            return Err(format!("{path} is a root"));
        }
        if self.nodes.contains_key(path) {
            return Err(format!("{path} is already there"));
        }
        match parent(path) {
            Some(d) if self.dir_at(d) => Ok(()),
            Some(d) => Err(format!("{d}: no such directory")),
            None => Err(format!("{path} is a root")),
        }
    }

    /// One directory, where nothing is yet.
    ///
    /// # Errors
    ///
    /// If something is already there, or its parent is not a directory.
    pub fn make_dir(&mut self, path: &str, now: f64) -> Result<(), String> {
        self.free(path)?;
        let e = Entry {
            name: basename(path).to_string(),
            is_dir: true,
            size: 0,
            modified: now,
        };
        self.put(path, e, Vec::new());
        Ok(())
    }

    /// A file, or a directory with everything under it. The times come
    /// along: a copy of a file is that file.
    ///
    /// # Errors
    ///
    /// If the source is gone, the destination is taken, or a directory
    /// is asked to copy into itself.
    pub fn copy(&mut self, from: &str, to: &str) -> Result<(), String> {
        if is_root(from) {
            return Err(format!("{from} is a root"));
        }
        if !self.nodes.contains_key(from) {
            return Err(format!("{from}: no such path"));
        }
        self.free(to)?;
        if to.starts_with(&format!("{from}/")) {
            return Err(format!("{from} is inside itself"));
        }
        for p in self.subtree(from) {
            let node = self.nodes[&p].clone();
            let dest = format!("{to}{}", &p[from.len()..]);
            let mut e = node.entry;
            e.name = basename(&dest).to_string();
            // A fresh number each: a copy is another object, however
            // alike its bytes.
            self.put(&dest, e, node.bytes);
        }
        Ok(())
    }

    /// The copy, and then the source is gone.
    ///
    /// # Errors
    ///
    /// As [`Tree::copy`].
    pub fn mv(&mut self, from: &str, to: &str) -> Result<(), String> {
        self.copy(from, to)?;
        // The objects go with the names: a rename keeps the inode, so
        // the moved nodes carry the numbers they had rather than the
        // fresh ones the copy just minted.
        for p in self.subtree(from) {
            let Some(old) = self.nodes.remove(&p) else {
                continue;
            };
            let dest = format!("{to}{}", &p[from.len()..]);
            if let Some(n) = self.nodes.get_mut(&dest) {
                n.id = old.id;
            }
        }
        Ok(())
    }

    /// The trash: where a delete puts a path, answering where it landed
    /// so undo can move it back. `~/.Trash`, made if it is not there,
    /// and a name that does not clash — the real one's shape.
    ///
    /// # Errors
    ///
    /// If the path is a root, or the trash cannot be made.
    pub fn trash(&mut self, path: &str, now: f64) -> Result<String, String> {
        let dir = join(HOME, ".Trash");
        if !self.dir_at(&dir) {
            self.make_dir(&dir, now)?;
        }
        let name = basename(path);
        let mut to = join(&dir, name);
        let mut n = 1;
        while self.nodes.contains_key(&to) {
            n += 1;
            to = join(&dir, &format!("{name} {n}"));
        }
        self.mv(path, &to)?;
        Ok(to)
    }
}

fn entry_of(fx: &Fx) -> Entry {
    let (y, mo, d, h, min) = fx.at;
    Entry {
        name: basename(fx.path).to_string(),
        is_dir: fx.dir,
        size: fx.size,
        modified: ts(y, mo, d, h, min),
    }
}

/// Whether the fixture has this path — a directory or a file.
#[must_use]
pub fn exists(path: &str) -> bool {
    is_root(path) || TREE.iter().any(|f| f.path == path)
}

/// Whether the fixture has this path as a directory.
#[must_use]
pub fn is_dir(path: &str) -> bool {
    is_root(path) || TREE.iter().any(|f| f.path == path && f.dir)
}

/// The entry at a path, if the fixture has it; a root is a directory.
#[must_use]
pub fn fixture_entry(path: &str) -> Option<Entry> {
    if is_root(path) {
        return Some(Entry {
            name: path.to_string(),
            is_dir: true,
            size: 0,
            modified: ts(2026, 9, 2, 7, 30),
        });
    }
    TREE.iter().find(|f| f.path == path).map(entry_of)
}

/// A text file's reading, for a card's preview.
#[must_use]
pub fn text_of(path: &str) -> Option<String> {
    let e = fixture_entry(path)?;
    if e.kind() != FileKind::Text {
        return None;
    }
    Some(match e.name.as_str() {
        "hosts" => {
            "127.0.0.1\tlocalhost\n255.255.255.255\tbroadcasthost\n::1\tlocalhost".into()
        }
        "README.txt" => "superapp 0.1.0\n\nA personal user-space OS: one workspace, specialized panels, no windows.\n\nDrag the .app to Applications. First launch asks for nothing; add a mail account in settings.".into(),
        "todo.txt" => "- files: the card previews\n- files: move here / copy here\n- attachments: save a part where I choose\n- rename?".into(),
        "notes.txt" => "Lisbon, August.\n\nInvoice 0817 is for the flat; the photos are from the last evening.".into(),
        "notes.md" => "# notes\n\n- a directory is a list panel\n- a file is a card\n- enter goes, the cursor previews\n\nThe join is the only relation.".into(),
        _ => format!("{}\n\n(the first 64 KB of the file, in the app's one face)", e.name),
    })
}

/// The one picture the fixture has: the app's own 32-pixel icon, which
/// every image in the tree reads as. A card decodes what it is handed
/// rather than what the name claims, so one real PNG proves the whole
/// path — and a fixture that shipped megabytes to say the same would be
/// a fixture about nothing.
const ICON_PNG: &[u8] = include_bytes!("../../resources/icon_32.png");

/// A file's bytes. Text files carry their reading, pictures the icon,
/// everything else nothing at all.
#[must_use]
pub fn bytes_of(path: &str) -> Option<Vec<u8>> {
    let e = fixture_entry(path)?;
    match e.kind() {
        FileKind::Text => text_of(path).map(String::into_bytes),
        FileKind::Image => Some(ICON_PNG.to_vec()),
        _ => Some(Vec::new()),
    }
}
