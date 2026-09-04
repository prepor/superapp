//! This machine's filesystem as the kernel's [`Disk`].
//!
//! Everything here refuses before it overwrites. A copy and a move claim
//! their destination exclusively — `create_new` on macOS, `renamex_np` for a
//! rename — because `std::fs::rename` and `std::fs::copy` both replace
//! silently and this app never does. A delete goes to the trash, never to
//! `remove`, so undo can move it back out.
//!
//! Two ways in are shut for a scripted run: with no `--demo-disk` the verbs
//! that write refuse in one sentence, because a suite must no more delete a
//! human's files than write to their keychain. The kernel's demo tree is
//! what a suite gets instead.

use std::path::{Path, PathBuf};

use kernel::caps::{sort, Disk, Entry, FileId};

/// `EXDEV` — "cross-device link", what a `rename` between two filesystems
/// answers. The same number on macOS and on linux, and not worth a libc
/// dependency to name.
const EXDEV: i32 = 18;

/// The real filesystem.
pub struct RealDisk {
    /// Whether the verbs that write may write at all. Off for a script
    /// replayed against a real disk; the refusal lands on the panel's line,
    /// where a forgotten `--demo-disk` is a failing step rather than a trip
    /// to the trash.
    writes: bool,
}

impl RealDisk {
    /// One that writes.
    #[must_use]
    pub fn new() -> RealDisk {
        RealDisk { writes: true }
    }

    /// One whose writing verbs refuse.
    #[must_use]
    pub fn read_only() -> RealDisk {
        RealDisk { writes: false }
    }

    /// The one sentence every refused write gives.
    fn sealed<T>() -> Result<T, String> {
        Err("this run may not write to the disk — a script wants --demo-disk".into())
    }
}

impl Default for RealDisk {
    fn default() -> RealDisk {
        RealDisk::new()
    }
}

/// One entry as a listing draws it.
fn entry_of(name: &str, m: &std::fs::Metadata) -> Entry {
    Entry {
        name: name.to_string(),
        is_dir: m.is_dir(),
        size: m.len(),
        modified: m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64()),
    }
}

impl Disk for RealDisk {
    fn list_dir(&mut self, dir: &Path) -> Result<Vec<Entry>, String> {
        let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut out = Vec::new();
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            // A link is listed as what it points at while that exists, as
            // itself otherwise; an entry that cannot be read at all is left
            // out rather than failing the listing.
            let Ok(meta) = std::fs::metadata(ent.path()).or_else(|_| ent.metadata()) else {
                continue;
            };
            out.push(entry_of(&name, &meta));
        }
        sort(&mut out);
        Ok(out)
    }

    fn stat(&mut self, path: &Path) -> Result<Option<Entry>, String> {
        // A link is answered as what it points at while that exists, and as
        // itself otherwise — the rule `list_dir` lists by. A dangling link
        // is a row the panel is showing, so a verb that called it absent
        // would refuse a source that is right there.
        let found = std::fs::metadata(path).or_else(|e| match e.kind() {
            std::io::ErrorKind::NotFound => std::fs::symlink_metadata(path),
            _ => Err(e),
        });
        match found {
            Ok(m) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into());
                Ok(Some(entry_of(&name, &m)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut buf = Vec::new();
        f.take(max as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(buf)
    }

    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// macOS: `/usr/bin/open`, the same door the Finder uses — the OS picks
    /// the viewer, and nothing runs under our name.
    ///
    /// Not from a script, though. Handing a path to the OS starts a program
    /// on somebody's machine and puts a window in front of whoever is using
    /// it; a suite has no more business doing that than deleting their files,
    /// so a read-only disk refuses this too.
    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        if !self.writes {
            return Err(
                "this run may not hand a path to the OS — a script wants --demo-disk".into(),
            );
        }
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("/usr/bin/open")
                .arg(path)
                .status()
                .map_err(|e| format!("open: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("open refused {} ({status})", path.display()))
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err("no opener on this platform".into())
        }
    }

    fn make_dir(&mut self, path: &Path) -> Result<(), String> {
        if !self.writes {
            return Self::sealed();
        }
        // `create_dir`, never `create_dir_all`: `new dir` makes the one
        // directory it named, and a typo is a refusal rather than a tree.
        std::fs::create_dir(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        if !self.writes {
            return Self::sealed();
        }
        if inside(from, to) {
            return Err(format!("{} cannot go inside itself", from.display()));
        }
        free(to)?;
        let Err(f) = copy_tree(from, to) else {
            return Ok(());
        };
        let err = format!("{}: {}", from.display(), f.err);
        // Only what this call made is this call's to clean up. A destination
        // that was already there when we reached for it — a racer got the
        // name between `free` and the claim — is somebody else's object, and
        // sweeping it would be the overwrite this path exists to prevent.
        if !f.made_root {
            return Err(err);
        }
        // A half-made copy is nobody's: not the copy that was asked for, and
        // not anything a panel is showing. It is removed rather than trashed
        // — a tree we made and no one has seen is not a deletion. Its source
        // is untouched throughout.
        match sweep(to) {
            Ok(()) => Err(err),
            Err(e) => Err(format!(
                "{err} — and a part of it was left at {}: {e}",
                to.display()
            )),
        }
    }

    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        if !self.writes {
            return Self::sealed();
        }
        if inside(from, to) {
            return Err(format!("{} cannot go inside itself", from.display()));
        }
        // The check is for the sentence; the exclusion is the kernel's.
        free(to)?;
        match rename_excl(from, to) {
            Ok(()) => Ok(()),
            // Somebody took the name between the check and the rename.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(format!("{} is already there", to.display()))
            }
            // EXDEV: the two paths are on different filesystems, where a
            // rename cannot reach. Copy, then trash the source — so even the
            // halfway state of a cross-volume move is recoverable.
            Err(e) if e.raw_os_error() == Some(EXDEV) => {
                self.copy_path(from, to)?;
                match self.trash(from) {
                    Ok(_) => Ok(()),
                    // The copy stands but the source did not go, which is
                    // not a move — and the caller is about to be told this
                    // failed, so it will record no node for the copy that
                    // would be left behind. Take it back off the disk: the
                    // move either happened or it did not.
                    Err(why) => Err(match sweep(to) {
                        Ok(()) => why,
                        Err(e) => format!("{why} — and the copy was left at {}: {e}", to.display()),
                    }),
                }
            }
            Err(e) => Err(format!("{}: {e}", from.display())),
        }
    }

    /// macOS: `NSFileManager`'s own `trashItemAtURL:`, the door the Finder
    /// uses — the right trash for the volume the file is on, a name that
    /// does not clash, and Put Back where the Finder shows it. Never a
    /// `remove_file`: undo has to be able to move it back.
    fn trash(&mut self, path: &Path) -> Result<PathBuf, String> {
        if !self.writes {
            return Self::sealed();
        }
        #[cfg(target_os = "macos")]
        {
            super::mac::trash(path)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err("no trash on this platform".into())
        }
    }

    fn file_id(&mut self, path: &Path) -> Result<Option<FileId>, String> {
        // `symlink_metadata`, never `metadata`: a link replaced by a link to
        // the same target is a different object, and following the link
        // would report the target's identity for both.
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(path) {
            Ok(m) => Ok(Some(FileId {
                dev: m.dev(),
                ino: m.ino(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

/// What a real copy and a real move ask of a destination before they write
/// anything: nothing is there. `std::fs::rename` and `std::fs::copy` both
/// replace silently, and this app never does — a clash is refused on the
/// panel's status line, and undo is free to move a path back without
/// wondering what it lands on.
fn free(to: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(to) {
        Ok(_) => Err(format!("{} is already there", to.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", to.display())),
    }
}

/// A copy that did not finish, and the one thing the caller cannot work out
/// for itself afterwards: whether the destination root standing there is
/// **ours** — created by this call — or somebody else's, which we ran into.
/// Only the first may be swept away.
struct CopyFail {
    err: std::io::Error,
    made_root: bool,
}

impl CopyFail {
    /// Failed before the destination was claimed: whatever is at that name,
    /// if anything, belongs to somebody else.
    fn before(err: std::io::Error) -> CopyFail {
        CopyFail {
            err,
            made_root: false,
        }
    }

    /// Failed after: the destination is ours, half-made.
    fn after(err: std::io::Error) -> CopyFail {
        CopyFail {
            err,
            made_root: true,
        }
    }
}

/// A file, a symlink as itself, or a directory with everything under it.
/// The recursion is the tree's depth, which is the disk's own bound.
///
/// Nothing else: a FIFO, a socket or a device node is refused by name.
/// `fs::copy` opens what it is given, and opening a FIFO blocks until
/// somebody writes to the other end — the browser performs its verbs on the
/// frame of the click, so that would be the window stopped for good.
fn copy_tree(from: &Path, to: &Path) -> Result<(), CopyFail> {
    let meta = std::fs::symlink_metadata(from).map_err(CopyFail::before)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        // Copied as a link, the way `cp -R` does it: following it would
        // duplicate what it points at, which is not what was asked for.
        let target = std::fs::read_link(from).map_err(CopyFail::before)?;
        return std::os::unix::fs::symlink(target, to).map_err(CopyFail::before);
    }
    if ft.is_file() {
        return copy_file(from, to);
    }
    if !ft.is_dir() {
        return Err(CopyFail::before(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a file or a directory", from.display()),
        )));
    }
    // `create_dir` is the atomic claim on the name, and the line either side
    // of which the destination becomes ours to clean up.
    std::fs::create_dir(to).map_err(CopyFail::before)?;
    let walk = || -> std::io::Result<()> {
        for ent in std::fs::read_dir(from)? {
            let ent = ent?;
            copy_tree(&ent.path(), &to.join(ent.file_name())).map_err(|f| f.err)?;
        }
        // The mode last, and only once the children are in: a directory the
        // source kept to itself must not land 0755 in a shared parent, and
        // setting 0700 before the walk would lock the walk out of its own
        // destination.
        std::fs::set_permissions(to, meta.permissions())
    };
    walk().map_err(CopyFail::after)
}

/// One file's bytes, written **through the descriptor that claimed the
/// name**. `create_new` is `O_EXCL` — an atomic claim — but reopening `to`
/// by name afterwards (which is what `fs::copy` does) gives a racer room to
/// unlink our empty file and leave a file or a symlink of their own for the
/// second open to truncate. The descriptor cannot be swapped under us, so
/// the bytes and the mode go through it.
fn copy_file(from: &Path, to: &Path) -> Result<(), CopyFail> {
    let mut src = std::fs::File::open(from).map_err(CopyFail::before)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(CopyFail::before)?;
    std::io::copy(&mut src, &mut dst).map_err(CopyFail::after)?;
    let mode = src.metadata().map_err(CopyFail::after)?.permissions();
    dst.set_permissions(mode).map_err(CopyFail::after)?;
    Ok(())
}

/// Whether `to` is `from` itself or somewhere under it — asked of the
/// **resolved** paths, which is the question a comparison of two spellings
/// cannot answer. An alias in the middle (`~/link` pointing at
/// `~/Downloads`) makes a copy feed itself its own output otherwise: the
/// walk creates entries in the directory it is still reading.
///
/// Only a real directory can contain anything, so a symlink source answers
/// *no*: it is copied as a link and recurses into nothing. A destination
/// that does not exist yet — the ordinary case — is resolved through its
/// parent.
fn inside(from: &Path, to: &Path) -> bool {
    if std::fs::symlink_metadata(from).is_ok_and(|m| !m.is_dir()) {
        return false;
    }
    let Ok(src) = from.canonicalize() else {
        return false;
    };
    let dst = match to.canonicalize() {
        Ok(p) => p,
        Err(_) => match (to.parent().map(Path::canonicalize), to.file_name()) {
            (Some(Ok(parent)), Some(name)) => parent.join(name),
            _ => return false,
        },
    };
    dst.starts_with(&src)
}

/// Takes back something this process made and nobody has seen: the half-tree
/// of a copy that failed, or the far side of a cross-volume move that could
/// not finish. **Not** the trash: a path the user never had is not a
/// deletion, and filling the trash with failures would be the ruder of the
/// two. Never called on anything but a destination this call created.
fn sweep(path: &Path) -> std::io::Result<()> {
    let ft = std::fs::symlink_metadata(path)?.file_type();
    if ft.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// `RENAME_EXCL` from macOS' `sys/stdio.h`: fail if the destination exists.
#[cfg(target_os = "macos")]
const RENAME_EXCL: std::ffi::c_uint = 0x0000_0004;

/// A rename that refuses an existing destination, where the platform has
/// one. Plain `rename(2)` replaces silently, and checking first is a window
/// another program can write into — so on macOS this is `renamex_np`, whose
/// exclusion is the kernel's.
#[cfg(target_os = "macos")]
fn rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::{c_char, c_int, c_uint, CString};
    use std::os::unix::ffi::OsStrExt;

    extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }

    let f = CString::new(from.as_os_str().as_bytes())?;
    let t = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: two NUL-terminated paths that outlive the call, and the one
    // flag the man page defines for it.
    if unsafe { renamex_np(f.as_ptr(), t.as_ptr(), RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Other platforms cannot make this rename exclusive. [`free`] checks the
/// destination first, but another write could still win the race.
#[cfg(not(target_os = "macos"))]
fn rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree of this process's own, removed on the way out.
    fn scratch(what: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("superapp-disk-{what}-{stamp}"));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// The whole refusal grammar on a real tree: nothing is ever
    /// overwritten, a copy into its own subtree is refused, a directory
    /// comes over with everything under it, and a name that is taken is a
    /// sentence rather than a loss.
    #[test]
    fn the_real_disk_refuses_before_it_overwrites() {
        let dir = scratch("refuse");
        let mut d = RealDisk::new();

        let a = dir.join("a");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(a.join("one.txt"), b"one").unwrap();
        std::fs::write(dir.join("flat.txt"), b"flat").unwrap();

        // A directory, with what is under it.
        let b = dir.join("b");
        d.copy_path(&a, &b).unwrap();
        assert_eq!(std::fs::read(b.join("one.txt")).unwrap(), b"one");
        // …and the same copy again is refused, not merged.
        assert!(d.copy_path(&a, &b).unwrap_err().contains("already there"));

        // A copy into its own subtree would feed itself.
        assert!(d
            .copy_path(&a, &a.join("inner"))
            .unwrap_err()
            .contains("cannot go inside itself"));

        // A move claims its name exclusively.
        let moved = dir.join("moved.txt");
        d.move_path(&dir.join("flat.txt"), &moved).unwrap();
        assert!(!dir.join("flat.txt").exists());
        assert_eq!(std::fs::read(&moved).unwrap(), b"flat");
        std::fs::write(dir.join("flat.txt"), b"again").unwrap();
        assert!(d
            .move_path(&dir.join("flat.txt"), &moved)
            .unwrap_err()
            .contains("already there"));

        // `new dir` makes the one directory it named, and no tree.
        d.make_dir(&dir.join("fresh")).unwrap();
        assert!(dir.join("fresh").is_dir());
        assert!(d.make_dir(&dir.join("fresh")).is_err(), "a taken name");
        assert!(d.make_dir(&dir.join("no/such/parent")).is_err());

        // A listing, a stat, and the id of what is actually there.
        let names: Vec<String> = d
            .list_dir(&dir)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.iter().any(|n| n == "b"));
        assert!(d.stat(&moved).unwrap().is_some());
        assert!(d.stat(&dir.join("nothing")).unwrap().is_none());
        assert!(d.file_id(&moved).unwrap().is_some());
        assert_eq!(d.file_id(&dir.join("nothing")).unwrap(), None);
        assert_eq!(d.read_file(&moved, 2).unwrap(), b"fl");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A scripted run against a real disk writes nothing and starts nothing,
    /// and says so in the one sentence a suite can read back.
    #[test]
    fn a_read_only_disk_refuses_every_write() {
        let dir = scratch("sealed");
        let mut d = RealDisk::read_only();
        let f = dir.join("keep.txt");
        std::fs::write(&f, b"keep").unwrap();

        for said in [
            d.make_dir(&dir.join("no")).unwrap_err(),
            d.copy_path(&f, &dir.join("copy")).unwrap_err(),
            d.move_path(&f, &dir.join("gone")).unwrap_err(),
            d.trash(&f).unwrap_err(),
            // Not a write, but the same rule: a suite may no more put a
            // window in front of whoever is at the machine.
            d.open_path(&f).unwrap_err(),
        ] {
            assert!(said.contains("--demo-disk"), "{said}");
        }
        assert!(f.exists(), "and nothing happened");
        // Reading is not writing.
        assert_eq!(d.read_file(&f, 99).unwrap(), b"keep");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
