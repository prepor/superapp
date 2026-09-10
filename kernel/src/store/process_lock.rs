//! One process may own a file-backed store, including its migrations.
//!
//! SQLite transaction locks serialize individual writes; they do not stop two
//! processes from using the same replicated device identity and native account
//! sessions. This lock spans the writer connection's entire lifetime.
use std::io;
use std::path::{Path, PathBuf};

pub(super) struct ProcessLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

impl ProcessLock {
    /// The sidecar is never removed. Unlinking a locked file would let a new
    /// process lock a different inode under the same path while we still own
    /// the old one. Closing the descriptor releases the kernel lock instead.
    #[cfg(unix)]
    pub(super) fn acquire(path: &Path) -> io::Result<(PathBuf, Self)> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        let canonical = canonical_database_path(path)?;
        if std::fs::metadata(&canonical).is_ok_and(|meta| meta.nlink() > 1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "database hard links are unsupported; use its original path or a symbolic link",
            ));
        }
        let mut sidecar = canonical.as_os_str().to_os_string();
        sidecar.push(".writer.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(Path::new(&sidecar))?;
        // SAFETY: file owns this live descriptor for the complete lock
        // lifetime. Nonblocking flock never waits for another app to close.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Err(io::Error::new(io::ErrorKind::WouldBlock,
                    "this database is already open in another Superapp instance; close it before reopening"));
            }
            return Err(error);
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        Ok((canonical, Self { _file: file }))
    }

    #[cfg(not(unix))]
    pub(super) fn acquire(_path: &Path) -> io::Result<(PathBuf, Self)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file-backed stores require a process lock that is not implemented on this platform",
        ))
    }
}

/// Resolve aliases before creating the sidecar. The database itself may be
/// new, so canonicalizing only the complete path is insufficient. A dangling
/// final symlink is also resolved before creating its target database.
#[cfg(unix)]
fn canonical_database_path(path: &Path) -> io::Result<PathBuf> {
    let mut candidate = path.to_path_buf();
    for _ in 0..40 {
        match candidate.canonicalize() {
            Ok(path) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&candidate)?;
                candidate = if target.is_absolute() {
                    target
                } else {
                    candidate
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(target)
                };
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "database path cannot be resolved",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = candidate
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .canonicalize()?;
                let name = candidate.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "database path has no filename")
                })?;
                return Ok(parent.join(name));
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "too many database symbolic links",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::{Schema, Step};
    use crate::store::{Db, Store};
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "superapp-process-lock-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn another_open_refuses_before_migration_but_shared_readers_and_restart_work() {
        static EXTRA: Schema = Schema {
            app: "lock-probe",
            steps: &[Step::Sql(
                "CREATE TABLE must_not_migrate(id INTEGER PRIMARY KEY)",
            )],
        };
        let directory = Directory::new();
        let path = directory.0.join("store.db");
        let a = Store::open(Some(&path), &[]).unwrap();
        let device = a.device();
        a.write(|tx| {
            tx.execute("INSERT INTO meta VALUES('process-lock','saved')", [])
                .map(|_| ())
        })
        .unwrap();
        let error = match Store::open(Some(&path), &[&EXTRA]) {
            Ok(_) => panic!("a second independent writer opened the same database"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already open"), "{error}");
        assert_eq!(
            a.conn()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name='must_not_migrate'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let reader = Store::with_db(a.db()).unwrap();
        drop(a);
        assert!(
            Db::open(Some(&path), &[]).is_err(),
            "shared readers still own the same writer"
        );
        drop(reader);
        let reopened = Store::open(Some(&path), &[]).unwrap();
        assert_eq!(reopened.device(), device);
        assert_eq!(
            reopened
                .conn()
                .query_row("SELECT value FROM meta WHERE key='process-lock'", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "saved"
        );
        let lock_path = directory.0.join("store.db.writer.lock");
        assert_eq!(
            std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(reopened);
        assert!(
            lock_path.exists(),
            "unlock must never unlink the shared lock inode"
        );
    }

    #[test]
    fn aliases_and_new_database_symlinks_share_the_same_process_lock() {
        let directory = Directory::new();
        let path = directory.0.join("future.db");
        let alias = directory.0.join("alias.db");
        symlink("future.db", &alias).unwrap();
        let a = Store::open(Some(&alias), &[]).unwrap();
        assert!(path.exists());
        assert!(
            Store::open(Some(&path), &[]).is_err(),
            "a dangling alias must lock its future target"
        );
        let parent_alias = directory.0.join("alias-parent");
        symlink(&directory.0, &parent_alias).unwrap();
        assert!(Store::open(Some(&parent_alias.join("future.db")), &[]).is_err());
        drop(a);
        let b = Store::open(Some(&path), &[]).unwrap();
        assert!(
            Store::open(Some(&alias), &[]).is_err(),
            "an existing alias must lock its canonical target"
        );
        drop(b);
        let alias_again = Store::open(Some(&alias), &[]).unwrap();
        drop(alias_again);
    }

    #[test]
    fn dropping_the_last_handle_joins_accepted_writes_before_unlocking() {
        let directory = Directory::new();
        let path = directory.0.join("store.db");
        let db = Db::open(Some(&path), &[]).unwrap();
        let store = Store::with_db(db.clone()).unwrap();
        let (started, wait_started) = std::sync::mpsc::channel();
        let (finish, wait_finish) = std::sync::mpsc::channel();
        let pending = store
            .submit_write(move |tx| {
                started.send(()).unwrap();
                wait_finish.recv().unwrap();
                tx.execute(
                    "INSERT INTO meta VALUES('accepted-before-close','saved')",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        wait_started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        drop(store);
        let closing = std::thread::spawn(move || drop(db));
        assert!(
            Db::open(Some(&path), &[]).is_err(),
            "the draining writer still owns the file lock"
        );
        finish.send(()).unwrap();
        closing.join().unwrap();
        drop(pending);
        let reopened = Store::open(Some(&path), &[]).unwrap();
        assert_eq!(
            reopened
                .conn()
                .query_row(
                    "SELECT value FROM meta WHERE key='accepted-before-close'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "saved"
        );
    }

    const CHILD_DB: &str = "SUPERAPP_PROCESS_LOCK_TEST_DATABASE";
    const CHILD_EXPECT_OPEN: &str = "SUPERAPP_PROCESS_LOCK_TEST_EXPECT_OPEN";

    #[test]
    fn a_separate_process_cannot_open_until_the_owner_closes() {
        let directory = Directory::new();
        let path = directory.0.join("store.db");
        let owner = Db::open(Some(&path), &[]).unwrap();
        let child = |expect_open: bool| {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "store::process_lock::tests::subprocess_open_probe",
                    "--nocapture",
                ])
                .env(CHILD_DB, &path)
                .env(CHILD_EXPECT_OPEN, if expect_open { "yes" } else { "no" })
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        };
        child(false);
        drop(owner);
        child(true);
    }

    #[test]
    fn subprocess_open_probe() {
        let Some(path) = std::env::var_os(CHILD_DB) else {
            return;
        };
        let opened = Db::open(Some(Path::new(&path)), &[]);
        if std::env::var(CHILD_EXPECT_OPEN).as_deref() == Ok("yes") {
            assert!(opened.is_ok());
        } else {
            let error = match opened {
                Ok(_) => panic!("another process obtained writer ownership"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("already open"), "{error}");
        }
    }
}
