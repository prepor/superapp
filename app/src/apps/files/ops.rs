//! The disk, written: the verbs a bar runs, what a `… here` plans, and
//! the intents that give each one back.
//!
//! Each verb is an effect ([`kernel::effect::Effect`]) — the store cannot
//! reproduce what happens on a disk. One path at a time, and none of them
//! on the thread that draws: a verb hands its paths to
//! [`run`](super::run), which performs exactly these functions off the UI
//! thread and hands back the [`Done`] records below. No path anyone ever
//! *had* is removed: what a delete takes goes to the trash, and undo moves
//! it back; even the reversal of a copy trashes what the copy made.
//!
//! The reversals *are* run where they are asked, on the UI thread: an undo
//! walks a history node, which is one gesture over what one run left, and
//! nothing may be half-walked.

use std::cell::RefCell;
use std::collections::BTreeSet;

use kernel::caps::{Clip, CopyPath, MakeDir, MovePath, OpenPath, Trash};
use kernel::effect::World;
use kernel::history::Intent;

use super::model::{
    basename, display_path, id_in, is_root, join, list_in, parent, plural, real_path, stat_in,
    FileId,
};
use super::{Clipboard, Op, FILES};

// -- the verbs -----------------------------------------------------------------

/// `open`: the path handed to the OS — whatever opens that kind of file.
/// Nothing is executed by us, and nothing of ours changes, so no listing
/// goes stale behind it.
///
/// # Errors
///
/// Whatever the disk said.
pub fn open_in(world: &World, path: &str) -> Result<(), String> {
    world.run(&OpenPath {
        path: &real_path(path),
    })
}

/// `new dir`: one directory, where nothing is yet.
///
/// # Errors
///
/// Whatever the disk said.
pub fn make_dir_in(world: &World, path: &str) -> Result<(), String> {
    let r = world.run(&MakeDir {
        path: &real_path(path),
    });
    FILES.touched();
    r
}

/// A file, or a directory with everything under it, copied.
///
/// # Errors
///
/// Whatever the disk said.
pub fn copy_in(world: &World, from: &str, to: &str) -> Result<(), String> {
    let r = world.run(&CopyPath {
        from: &real_path(from),
        to: &real_path(to),
    });
    FILES.touched();
    r
}

/// A path moved — and the same verb undo puts one back with.
///
/// # Errors
///
/// Whatever the disk said.
pub fn move_in(world: &World, from: &str, to: &str) -> Result<(), String> {
    let r = world.run(&MovePath {
        from: &real_path(from),
        to: &real_path(to),
    });
    FILES.touched();
    r
}

/// To the trash, and where it landed — in the panels' spelling, so an
/// intent can carry it until the undo that needs it.
///
/// # Errors
///
/// Whatever the disk said.
pub fn trash_in(world: &World, path: &str) -> Result<String, String> {
    let r = world
        .run(&Trash {
            path: &real_path(path),
        })
        .map(|p| display_path(&p));
    FILES.touched();
    r
}

/// `copy path`: what the paths are called on *this* machine, one to a
/// line, onto the system clipboard — the real spelling and not the
/// panels', because what is pasted lands somewhere that never heard of
/// `~`.
///
/// Nothing of ours changes and no disk is touched, so no listing goes
/// stale behind it and there is nothing to give back.
///
/// # Errors
///
/// Whatever the clipboard said.
pub fn clip_paths(world: &World, paths: &[String]) -> Result<(), String> {
    let text = paths
        .iter()
        .map(|p| real_path(p).to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    world.run(&Clip {
        text: &text,
        what: if paths.len() == 1 {
            "the path"
        } else {
            "the paths"
        },
    })
}

// -- what a name may be ------------------------------------------------------

/// What a name has to be before any disk is asked: one segment, and a
/// segment that names something. `rename` renames a thing where it already
/// is — a path typed into the field would carry it off somewhere else under
/// the word *rename*, and `.` and `..` name the directory rather than
/// anything in it.
///
/// # Errors
///
/// The sentence the status line carries.
pub fn check_name(name: &str) -> Result<(), String> {
    match name {
        n if n.contains('/') => Err("a name is not a path".to_string()),
        "." | ".." => Err(format!("“{name}” is not a name")),
        _ => Ok(()),
    }
}

// -- what a `… here` plans -----------------------------------------------------

/// The one clash a copy is allowed to make is into the file's own
/// directory, where the duplicate is the point: `notes.txt` lands beside
/// `notes.txt` as `notes copy.txt`, and beside that as `notes copy 2.txt`.
/// The extension stays where it belongs, and a dot-file — which is all
/// extension — takes the suffix at the end.
#[must_use]
pub fn copy_name(name: &str, n: usize) -> String {
    let suffix = if n <= 1 {
        " copy".to_string()
    } else {
        format!(" copy {n}")
    };
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}{suffix}.{ext}"),
        _ => format!("{name}{suffix}"),
    }
}

/// The first [`copy_name`] `dir` has room for: not on the disk, and not
/// already claimed by an earlier step of the same plan.
#[must_use]
pub fn free_name(world: &World, dir: &str, name: &str, taken: &BTreeSet<String>) -> String {
    let mut n = 1;
    loop {
        let candidate = copy_name(name, n);
        if !taken.contains(&candidate) && stat_in(world, &join(dir, &candidate)).is_none() {
            return candidate;
        }
        n += 1;
    }
}

/// One path a `… here` will perform: where it comes from and where it
/// lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub from: String,
    pub to: String,
}

/// What a `… here` can do and what it refuses, path by path — a batch
/// refuses exactly as one does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// One sentence per path that will not be performed, as the status
    /// line says it.
    pub refused: Vec<String>,
}

/// Resolves the clipboard against the directory it is dropped into, before
/// anything is written: the destination each path takes, and the refusal
/// for each one that has none.
#[must_use]
pub fn plan_here(world: &World, clip: &Clipboard, dir: &str) -> Plan {
    let mut plan = Plan::default();
    // The names this plan has already spoken for: two `report.pdf`s held
    // from two directories clash with each other, not only with the disk.
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for path in &clip.paths {
        let name = basename(path).to_string();
        let same_dir = parent(path) == Some(dir);
        // The source may have gone while the clipboard waited: a watch
        // says that a directory changed, not that this path is still
        // there, so this is the first look at the path itself.
        if is_root(path) {
            plan.refused.push(format!("“{path}” is a root"));
        } else if stat_in(world, path).is_none() {
            plan.refused.push(format!("“{name}” is no longer there"));
        } else if *path == dir || dir.starts_with(&format!("{path}/")) {
            plan.refused
                .push(format!("cannot {} “{name}” into itself", clip.verb.verb()));
        } else if same_dir && clip.verb == Op::Copy {
            // The one clash allowed, under a name that is free.
            let fresh = free_name(world, dir, &name, &taken);
            taken.insert(fresh.clone());
            plan.steps.push(Step {
                from: path.clone(),
                to: join(dir, &fresh),
            });
        } else if same_dir || taken.contains(&name) || stat_in(world, &join(dir, &name)).is_some() {
            // A move that goes nowhere, or a name the destination already
            // has: one sentence covers both, so it is written once.
            plan.refused.push(format!("“{name}” is already here"));
        } else {
            taken.insert(name.clone());
            plan.steps.push(Step {
                from: path.clone(),
                to: join(dir, &name),
            });
        }
    }
    plan
}

// -- the intents ---------------------------------------------------------------
//
// What a verb claimed of the disk, and how to give it back. In memory, on a
// history node, never serialized — what survives a restart is the disk
// itself. Each one asks the disk before it reverses rather than trusting
// what it remembers: watched or not, the world may well have moved on, and
// a reversal that has to be refused says so
// ([`Intent::blocked`]) instead of writing over whatever is there now.

/// One path a verb performed, and what it left where it put it. A [`Step`]
/// is the plan — what is about to happen; this is the record of what did.
/// The entry is how a reversal tells its own work from a stranger's: a
/// path that is *there* is not the same question as a path that is *ours*,
/// and undo takes things away.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    pub from: String,
    pub to: String,
    /// The object the disk had at `to` the moment after the write. `None`
    /// where the disk would not say — and then the reversal is refused
    /// rather than guessed at, because undo takes things away.
    pub landed: Option<FileId>,
}

impl Done {
    /// The record of a step just performed: the object the disk now has at
    /// its destination.
    #[must_use]
    pub fn of(world: &World, from: &str, to: &str) -> Done {
        Done {
            from: from.to_string(),
            to: to.to_string(),
            landed: id_in(world, to),
        }
    }
}

/// Why a path cannot be put back: something else took the name.
fn occupied(world: &World, path: &str) -> Option<String> {
    stat_in(world, path)
        .is_some()
        .then(|| format!("something else is at “{}” now", basename(path)))
}

/// Why what is at `path` is not this action's to take away: nothing is
/// there, or the thing there is a different object wearing the same name —
/// a file deleted and replaced, a directory made again from scratch. The
/// question is asked of the **object**, not the path, because a name is
/// cheap to reuse and a reversal removes what it finds.
///
/// Fail closed: a record that never learned what it made, or a disk that
/// will not say now, refuses. Undo may decline; it may not guess.
fn ours(world: &World, path: &str, landed: Option<FileId>) -> Option<String> {
    let name = basename(path);
    match (id_in(world, path), landed) {
        (None, _) => Some(format!("“{name}” is no longer there")),
        (_, None) => Some(format!(
            "“{name}” cannot be told apart from what is there now"
        )),
        (Some(now), Some(was)) if now != was => Some(format!("“{name}” is not what was put there")),
        _ => None,
    }
}

/// Every path attempted, and then one error naming the ones that would
/// not go: a reversal that stopped at the first failure would leave the
/// rest of a batch untouched and say nothing about which half moved.
fn all(rs: impl IntoIterator<Item = Result<(), String>>) -> Result<(), String> {
    let bad: Vec<String> = rs.into_iter().filter_map(Result::err).collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad.join(", "))
    }
}

/// `copy here`: undo puts the copies in the trash — never an `rm`, even
/// for what we made ourselves.
pub struct Copied {
    /// A redo copies again and lands a new file, with its own times, so
    /// what each one *is* is rewritten rather than assumed.
    pub done: RefCell<Vec<Done>>,
}

impl Copied {
    #[must_use]
    pub fn new(done: Vec<Done>) -> Copied {
        Copied {
            done: RefCell::new(done),
        }
    }
}

impl Intent for Copied {
    fn describe(&self) -> String {
        format!("copied {}", plural(self.done.borrow().len()))
    }

    fn blocked(&self, w: &World) -> Option<String> {
        self.done
            .borrow()
            .iter()
            .find_map(|d| ours(w, &d.to, d.landed))
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        all(self
            .done
            .borrow()
            .iter()
            .map(|d| trash_in(w, &d.to).map(|_| ())))
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let mut done = self.done.borrow_mut();
        all(done.iter_mut().map(|d| {
            copy_in(w, &d.from, &d.to)?;
            d.landed = id_in(w, &d.to);
            Ok(())
        }))
    }
}

/// `move here`: undo moves each one back where it was.
pub struct Moved {
    pub done: RefCell<Vec<Done>>,
}

impl Moved {
    #[must_use]
    pub fn new(done: Vec<Done>) -> Moved {
        Moved {
            done: RefCell::new(done),
        }
    }
}

impl Intent for Moved {
    fn describe(&self) -> String {
        format!("moved {}", plural(self.done.borrow().len()))
    }

    fn blocked(&self, w: &World) -> Option<String> {
        self.done
            .borrow()
            .iter()
            .find_map(|d| ours(w, &d.to, d.landed).or_else(|| occupied(w, &d.from)))
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        all(self
            .done
            .borrow()
            .iter()
            .map(|d| move_in(w, &d.to, &d.from)))
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let mut done = self.done.borrow_mut();
        all(done.iter_mut().map(|d| {
            move_in(w, &d.from, &d.to)?;
            d.landed = id_in(w, &d.to);
            Ok(())
        }))
    }
}

/// `rename`: undo puts the old name back.
///
/// The disk verb is a move — what makes it a rename is that only the last
/// segment changes — so the record and the reversal are a move's, and only
/// the words are the rename's. One [`Done`], never a set: a new name is a
/// name, and two things cannot both wear it.
pub struct Renamed {
    pub done: RefCell<Done>,
}

impl Renamed {
    #[must_use]
    pub fn new(done: Done) -> Renamed {
        Renamed {
            done: RefCell::new(done),
        }
    }
}

impl Intent for Renamed {
    fn describe(&self) -> String {
        let d = self.done.borrow();
        format!("renamed “{}” to “{}”", basename(&d.from), basename(&d.to))
    }

    /// [`occupied`] asks whether anything is at the old name, which is the
    /// right question for a move and the wrong one here: where the disk
    /// does not tell two cases apart, what stats at the old name after a
    /// case-only rename is this very file. So the question is asked of the
    /// **object** — is what is there somebody else? — as every other
    /// reversal asks it, and a disk that will not say refuses.
    fn blocked(&self, w: &World) -> Option<String> {
        let d = self.done.borrow();
        ours(w, &d.to, d.landed).or_else(|| {
            let stranger = id_in(w, &d.from).is_some_and(|there| Some(there) != d.landed);
            stranger.then(|| format!("something else is at “{}” now", basename(&d.from)))
        })
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let d = self.done.borrow();
        move_in(w, &d.to, &d.from)
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let mut d = self.done.borrow_mut();
        move_in(w, &d.from, &d.to)?;
        d.landed = id_in(w, &d.to);
        Ok(())
    }
}

/// `delete`: what the trash took, and where it took it from. A redo
/// trashes again and lands somewhere new — the trash picks the name — so
/// where each one *is* is remembered rather than assumed.
pub struct Deleted {
    /// `from` is where it was, `to` where the trash put it.
    pub done: RefCell<Vec<Done>>,
}

impl Deleted {
    #[must_use]
    pub fn new(done: Vec<Done>) -> Deleted {
        Deleted {
            done: RefCell::new(done),
        }
    }
}

impl Intent for Deleted {
    fn describe(&self) -> String {
        format!("trashed {}", plural(self.done.borrow().len()))
    }

    /// The three ways a restore expires: the trash was emptied, what is in
    /// there is not what went in, or something took the name back.
    fn blocked(&self, w: &World) -> Option<String> {
        self.done.borrow().iter().find_map(|d| {
            let name = basename(&d.from);
            match (id_in(w, &d.to), d.landed) {
                (None, _) => Some(format!("“{name}” is not in the trash any more")),
                (_, None) => Some(format!(
                    "“{name}” cannot be told apart from what is in the trash"
                )),
                (Some(now), Some(was)) if now != was => {
                    Some(format!("“{name}” in the trash is not what went there"))
                }
                _ => occupied(w, &d.from),
            }
        })
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        all(self
            .done
            .borrow()
            .iter()
            .map(|d| move_in(w, &d.to, &d.from)))
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let mut done = self.done.borrow_mut();
        all(done.iter_mut().map(|d| {
            d.to = trash_in(w, &d.from)?;
            d.landed = id_in(w, &d.to);
            Ok(())
        }))
    }
}

/// `new dir`: undo trashes it, while it is still the empty directory it
/// was made as. Anything inside and the reversal has expired — which is
/// what it means for one to expire honestly, and it is also why an empty
/// directory needs no identity of its own: one is exactly like another.
pub struct MadeDir {
    pub path: String,
    /// The directory this made, as the disk numbers it. One empty
    /// directory looks exactly like another, so without this an undo would
    /// happily trash somebody else's — a redo mints a new one, so it is
    /// rewritten rather than assumed.
    pub landed: RefCell<Option<FileId>>,
}

impl MadeDir {
    /// The record of a directory just made, off what the run read the
    /// moment after the write — which is the only moment the answer is
    /// certainly about the directory this made.
    #[must_use]
    pub fn made(done: &Done) -> MadeDir {
        MadeDir {
            path: done.to.clone(),
            landed: RefCell::new(done.landed),
        }
    }
}

impl Intent for MadeDir {
    fn describe(&self) -> String {
        format!("made “{}/”", basename(&self.path))
    }

    fn blocked(&self, w: &World) -> Option<String> {
        if let Some(why) = ours(w, &self.path, *self.landed.borrow()) {
            return Some(why);
        }
        match list_in(w, &self.path) {
            Ok(v) if v.is_empty() => None,
            Ok(_) => Some(format!("“{}/” is not empty any more", basename(&self.path))),
            Err(e) => Some(e),
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        trash_in(w, &self.path).map(|_| ())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        make_dir_in(w, &self.path)?;
        *self.landed.borrow_mut() = id_in(w, &self.path);
        Ok(())
    }
}
