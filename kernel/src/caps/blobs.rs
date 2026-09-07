//! A device-local blob cache: bytes an app owns, kept off the store.
//!
//! Media is the one thing the store must not carry — a photo, a video, a
//! voice note is large, and the store replicates. So a file lives here
//! instead: keyed by an opaque string its app owns (`tg:<remote id>`, later
//! `mail:<attachment>`), bounded by one byte budget, evicted least-recently
//! used when a write would cross it. The cache knows no app; it is a kernel
//! capability every world holds, reachable from a worker's thread, exactly
//! like [`Secrets`](super::Secrets).
//!
//! The index — key, file, size, recency — is the cache's *own* small SQLite
//! database in the cache directory, not the app's store: the cache is
//! device-local and un-synced, so its budget survives a restart without
//! riding the replicating store.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

/// A gibibyte: the default budget, a scroll's worth of media and no more of
/// a device than that.
pub const BLOB_BUDGET_DEFAULT: u64 = 1024 * 1024 * 1024;

/// A bounded, least-recently-used file store any app draws from.
///
/// Two verbs put bytes in — [`put`](Blobs::put) writes them,
/// [`ingest`](Blobs::ingest) adopts a finished download — and both overwrite
/// a key; [`get`](Blobs::get) takes one back out and marks it freshest, so
/// the files a view keeps asking for are the last to be evicted. A miss is
/// the caller's to refill: nothing here reaches the wire.
pub trait Blobs {
    /// The cached file for `key`, if present, marked most-recently-used. A
    /// row whose file has vanished under the cache reads as a miss and is
    /// dropped.
    fn get(&mut self, key: &str) -> Option<PathBuf>;

    /// Writes `bytes` under `key`, evicting the least-recently-used entries
    /// first if this would cross the budget. Overwrites an existing key.
    ///
    /// # Errors
    ///
    /// If the bytes could not be written, or the index could not be updated.
    fn put(&mut self, key: &str, bytes: &[u8]) -> Result<PathBuf, String>;

    /// Takes an existing file — a finished download — *into* the cache under
    /// `key`: a rename when it is on the same filesystem, else a copy then a
    /// remove. Evicts as [`put`](Blobs::put) does.
    ///
    /// # Errors
    ///
    /// If the source is gone, could not be moved in, or the index could not
    /// be updated.
    fn ingest(&mut self, key: &str, src: &Path) -> Result<PathBuf, String>;

    /// Whether `key` is cached and its file is really there. Does not touch
    /// recency — only the verbs that hand bytes across do.
    fn contains(&mut self, key: &str) -> bool;

    /// Forgets `key`: its row and its file, whichever is there.
    fn remove(&mut self, key: &str);

    /// The byte budget this cache was built with.
    fn budget(&self) -> u64;

    /// What the cache holds right now, in bytes — for a test to weigh
    /// against the budget.
    fn total(&self) -> u64;
}

/// The real cache: plain files under a directory, an index beside them.
///
/// Shared, not merely on disk — each worker builds its own world, so the
/// handle the window holds and the one a download worker holds must be the
/// same cache over the same budget. One [`Mutex`] serialises every
/// operation, so the index and the directory never disagree across threads.
/// Cloning shares the one backend, the way [`MemSecrets`](super::MemSecrets)
/// does.
#[derive(Clone)]
pub struct BlobCache(Arc<Mutex<Inner>>);

impl BlobCache {
    /// A cache over `dir` with a `budget` of bytes. Opens nothing yet: the
    /// directory and its index are made on first use, so a world that never
    /// asks for a blob leaves no trace on disk.
    #[must_use]
    pub fn at(dir: PathBuf, budget: u64) -> BlobCache {
        BlobCache(Arc::new(Mutex::new(Inner {
            dir,
            budget,
            conn: None,
        })))
    }
}

impl std::fmt::Debug for BlobCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BlobCache")
    }
}

impl Blobs for BlobCache {
    fn get(&mut self, key: &str) -> Option<PathBuf> {
        self.0.lock().ok()?.get(key)
    }
    fn put(&mut self, key: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        self.0.lock().map_err(poisoned)?.put(key, bytes)
    }
    fn ingest(&mut self, key: &str, src: &Path) -> Result<PathBuf, String> {
        self.0.lock().map_err(poisoned)?.ingest(key, src)
    }
    fn contains(&mut self, key: &str) -> bool {
        self.0.lock().map(|mut g| g.contains(key)).unwrap_or(false)
    }
    fn remove(&mut self, key: &str) {
        if let Ok(mut g) = self.0.lock() {
            g.remove(key);
        }
    }
    fn budget(&self) -> u64 {
        self.0.lock().map(|g| g.budget).unwrap_or(0)
    }
    fn total(&self) -> u64 {
        self.0.lock().map(|mut g| g.total()).unwrap_or(0)
    }
}

fn poisoned<T>(_: T) -> String {
    "the blob cache is poisoned".to_string()
}

/// The one backend behind every clone. Its connection opens lazily and is
/// reconciled with the directory the first time it does, so a crash that
/// left a file without a row (or a row without a file) heals on the next
/// open rather than lying to a reader.
struct Inner {
    dir: PathBuf,
    budget: u64,
    conn: Option<Connection>,
}

impl Inner {
    fn ensure_open(&mut self) -> Result<(), String> {
        if self.conn.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let conn = Connection::open(self.dir.join("index.db")).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS blob(\
                 key TEXT PRIMARY KEY, \
                 file TEXT NOT NULL, \
                 size INTEGER NOT NULL, \
                 seq INTEGER NOT NULL)",
        )
        .map_err(|e| e.to_string())?;
        reconcile(&conn, &self.dir)?;
        self.conn = Some(conn);
        Ok(())
    }

    fn get(&mut self, key: &str) -> Option<PathBuf> {
        self.ensure_open().ok()?;
        let conn = self.conn.as_ref()?;
        let file: Option<String> = conn
            .query_row("SELECT file FROM blob WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .ok()?;
        let file = file?;
        let path = self.dir.join(&file);
        if !path.exists() {
            // The file went out from under the row: a miss, and the row goes.
            let _ = conn.execute("DELETE FROM blob WHERE key = ?1", params![key]);
            return None;
        }
        if let Ok(next) = next_seq(conn) {
            let _ = conn.execute(
                "UPDATE blob SET seq = ?1 WHERE key = ?2",
                params![next, key],
            );
        }
        Some(path)
    }

    fn put(&mut self, key: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        self.ensure_open()?;
        let dir = self.dir.clone();
        let budget = self.budget;
        let file = file_name(key);
        let final_path = dir.join(&file);
        // Write to a temp beside the target, then rename into place: a reader
        // never sees a half-written blob, and the name is the key's, so an
        // overwrite lands on the same file.
        let tmp = dir.join(format!("{file}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &final_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })?;
        self.record(key, &file, bytes.len() as u64, &dir, budget)?;
        Ok(final_path)
    }

    fn ingest(&mut self, key: &str, src: &Path) -> Result<PathBuf, String> {
        self.ensure_open()?;
        let dir = self.dir.clone();
        let budget = self.budget;
        let size = std::fs::metadata(src).map_err(|e| e.to_string())?.len();
        let file = file_name(key);
        let final_path = dir.join(&file);
        if std::fs::rename(src, &final_path).is_err() {
            // Another filesystem: copy through a temp in the cache dir, swap
            // it in, then drop the source, so a reader never sees half a file.
            let tmp = dir.join(format!("{file}.{}.tmp", std::process::id()));
            std::fs::copy(src, &tmp).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                e.to_string()
            })?;
            let _ = std::fs::remove_file(src);
        }
        self.record(key, &file, size, &dir, budget)?;
        Ok(final_path)
    }

    /// Record a freshly written key and bring the cache back inside its
    /// budget. The row is upserted with the next recency stamp, then the
    /// coldest others are evicted until the total fits — never this key,
    /// which is both the freshest and named apart. A single item larger than
    /// the whole budget empties the cache and then stays: refusing it would
    /// mean it could never be cached and would re-download forever, so the
    /// budget is a target, not a ceiling for one oversized blob.
    fn record(
        &mut self,
        key: &str,
        file: &str,
        size: u64,
        dir: &Path,
        budget: u64,
    ) -> Result<(), String> {
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| "the blob cache is closed".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let seq = next_seq(&tx).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO blob(key, file, size, seq) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(key) DO UPDATE SET file = ?2, size = ?3, seq = ?4",
            // SQLite integers are signed; a blob's size fits i64 many times
            // over.
            params![key, file, size as i64, seq],
        )
        .map_err(|e| e.to_string())?;
        let mut evicted: Vec<String> = Vec::new();
        loop {
            let total: i64 = tx
                .query_row("SELECT COALESCE(SUM(size), 0) FROM blob", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if total as u64 <= budget {
                break;
            }
            let victim: Option<(String, String)> = tx
                .query_row(
                    "SELECT key, file FROM blob WHERE key != ?1 ORDER BY seq ASC LIMIT 1",
                    params![key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            match victim {
                Some((vkey, vfile)) => {
                    tx.execute("DELETE FROM blob WHERE key = ?1", params![vkey])
                        .map_err(|e| e.to_string())?;
                    evicted.push(vfile);
                }
                // Only the key we are inserting is left: an oversized blob
                // stays rather than being refused.
                None => break,
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        // The files go only once the index is durable, so an interrupted
        // commit leaves the directory ahead of the index — which reconcile
        // heals — never behind it.
        for vfile in evicted {
            let _ = std::fs::remove_file(dir.join(vfile));
        }
        Ok(())
    }

    fn contains(&mut self, key: &str) -> bool {
        if self.ensure_open().is_err() {
            return false;
        }
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        let file: Option<String> = conn
            .query_row("SELECT file FROM blob WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten();
        match file {
            Some(f) if self.dir.join(&f).exists() => true,
            Some(_) => {
                let _ = conn.execute("DELETE FROM blob WHERE key = ?1", params![key]);
                false
            }
            None => false,
        }
    }

    fn remove(&mut self, key: &str) {
        if self.ensure_open().is_err() {
            return;
        }
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let file: Option<String> = conn
            .query_row("SELECT file FROM blob WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten();
        let _ = conn.execute("DELETE FROM blob WHERE key = ?1", params![key]);
        if let Some(f) = file {
            let _ = std::fs::remove_file(self.dir.join(f));
        }
    }

    fn total(&mut self) -> u64 {
        // Opens like the other verbs, because an unopened handle may still
        // sit over a persisted index from an earlier run whose bytes it must
        // count. A truly fresh cache opens to an empty table and reads 0.
        if self.ensure_open().is_err() {
            return 0;
        }
        let Some(conn) = self.conn.as_ref() else {
            return 0;
        };
        conn.query_row("SELECT COALESCE(SUM(size), 0) FROM blob", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n.max(0) as u64)
        .unwrap_or(0)
    }
}

/// The next recency stamp: one past the highest in the table. Monotone
/// within a run and across reopens — the max persists — and never reused,
/// since eviction takes the lowest. So LRU order is a pure function of the
/// calls, not the wall clock, and a test can rely on it.
fn next_seq(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM blob", [], |r| {
        r.get(0)
    })
}

/// A path-safe, collision-resistant file name for an arbitrary key: the
/// SHA-256 of its bytes, in hex. Key-addressed, no content dedup — two keys
/// with the same bytes are two files, which is what a key-owned cache wants.
///
/// Public so a reader that knows the cache directory can locate a blob by
/// path — `<dir>/<file_name(key)>` — without holding the [`Blobs`] cap, the
/// way a widget resolves a downloaded photo on its draw thread. It only
/// names the file; it neither opens nor touches the index or its recency.
#[must_use]
pub fn file_name(key: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest.as_ref() {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Bring the index and the directory back into agreement, once, on open: a
/// row whose file is gone is dropped, and a file no row claims is deleted.
/// So a crash mid-write can leave neither a reader a path to nothing nor the
/// budget counting bytes that are no longer there.
fn reconcile(conn: &Connection, dir: &Path) -> Result<(), String> {
    let mut keep: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn
            .prepare("SELECT key, file FROM blob")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        for (key, file) in rows {
            if dir.join(&file).exists() {
                keep.insert(file);
            } else {
                let _ = conn.execute("DELETE FROM blob WHERE key = ?1", params![key]);
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // The index itself (and any journal it carries) is not a blob.
            if name.starts_with("index.db") || keep.contains(&name) {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty cache directory of this test's own, swept first so a
    /// previous run cannot bleed in. Named by the pid and the case.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("superapp-blobs-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn put_then_get_roundtrip() {
        let dir = scratch("roundtrip");
        let mut c = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);
        assert_eq!(c.total(), 0, "an untouched cache holds nothing");
        assert!(!c.contains("tg:1"));

        let path = c.put("tg:1", b"hello media").unwrap();
        assert!(c.contains("tg:1"));
        assert_eq!(c.total(), 11);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello media");
        assert_eq!(c.get("tg:1"), Some(path.clone()), "get returns the path");

        // Overwriting a key replaces the bytes and the size, not adds.
        c.put("tg:1", b"more").unwrap();
        assert_eq!(c.total(), 4);
        assert_eq!(std::fs::read(c.get("tg:1").unwrap()).unwrap(), b"more");

        c.remove("tg:1");
        assert!(!c.contains("tg:1"));
        assert_eq!(c.get("tg:1"), None);
        assert_eq!(c.total(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ingest_moves_a_file_in() {
        let dir = scratch("ingest");
        let mut c = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);

        // A finished download sitting elsewhere in the same temp root.
        let src = std::env::temp_dir().join(format!("superapp-blobs-src-{}", std::process::id()));
        std::fs::write(&src, b"downloaded bytes").unwrap();

        let path = c.ingest("tg:file", &src).unwrap();
        assert!(
            !src.exists(),
            "the source is taken into the cache, not left"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"downloaded bytes");
        assert_eq!(c.get("tg:file"), Some(path));
        assert_eq!(c.total(), 16);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eviction_is_least_recently_used() {
        let dir = scratch("lru");
        // Budget for three 100-byte blobs, no more.
        let mut c = BlobCache::at(dir.clone(), 300);
        let hundred = vec![b'x'; 100];
        c.put("a", &hundred).unwrap();
        c.put("b", &hundred).unwrap();
        c.put("c", &hundred).unwrap();
        assert_eq!(c.total(), 300);
        assert!(c.contains("a") && c.contains("b") && c.contains("c"));

        // A fourth crosses the budget: the coldest, `a`, is evicted.
        c.put("d", &hundred).unwrap();
        assert_eq!(c.total(), 300);
        assert!(!c.contains("a"), "the least-recently-used went");
        assert!(c.contains("b") && c.contains("c") && c.contains("d"));
        // Its file went with its row.
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            4,
            "index.db plus three blobs"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_bumps_recency_so_it_survives() {
        let dir = scratch("bump");
        let mut c = BlobCache::at(dir.clone(), 300);
        let hundred = vec![b'x'; 100];
        c.put("a", &hundred).unwrap();
        c.put("b", &hundred).unwrap();
        c.put("c", &hundred).unwrap();

        // Touch `a`: now `b` is the coldest.
        assert!(c.get("a").is_some());
        c.put("d", &hundred).unwrap();
        assert!(c.contains("a"), "a was got, so it is not the one evicted");
        assert!(!c.contains("b"), "b was the coldest once a was touched");
        assert!(c.contains("c") && c.contains("d"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_item_over_budget_is_still_stored() {
        let dir = scratch("oversize");
        let mut c = BlobCache::at(dir.clone(), 50);
        let big = vec![b'y'; 200];
        let path = c.put("whale", &big).unwrap();
        assert!(
            c.contains("whale"),
            "an oversized blob is kept, not refused"
        );
        assert_eq!(c.total(), 200);
        assert_eq!(std::fs::read(&path).unwrap().len(), 200);

        // And it is evictable like anything else once a smaller one arrives.
        c.put("minnow", &[b'z'; 10]).unwrap();
        assert!(
            !c.contains("whale"),
            "the oversized blob yields to the new one"
        );
        assert!(c.contains("minnow"));
        assert_eq!(c.total(), 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_index_survives_a_reopen() {
        let dir = scratch("reopen");
        {
            let mut c = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);
            c.put("keep", b"persist me").unwrap();
            c.put("also", b"and me").unwrap();
        } // dropped: the connection closes, the index stays on disk.

        let mut c = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);
        assert_eq!(c.total(), 16, "the index came back with its sizes");
        assert_eq!(
            std::fs::read(c.get("keep").unwrap()).unwrap(),
            b"persist me"
        );
        assert!(c.contains("also"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reopen_reconciles_the_directory() {
        let dir = scratch("reconcile");
        let gone_path;
        {
            let mut c = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);
            gone_path = c.put("gone", b"doomed").unwrap();
            c.put("stays", b"safe").unwrap();
        }
        // A hand deletes one blob's file, and something drops an orphan with
        // no row beside the index.
        std::fs::remove_file(&gone_path).unwrap();
        let orphan = dir.join("orphan.bin");
        std::fs::write(&orphan, b"junk").unwrap();

        let mut c = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);
        assert!(
            !c.contains("gone"),
            "the row for a vanished file was dropped"
        );
        assert_eq!(c.get("gone"), None);
        assert!(c.contains("stays"));
        assert_eq!(c.total(), 4, "only the surviving blob is counted");
        assert!(!orphan.exists(), "a file no row claims was swept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clones_share_one_backend() {
        // The thread-sharing model: a worker's world and the window's hold
        // clones of one handle, so what one writes the other reads.
        let dir = scratch("shared");
        let cache = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);
        let mut writer = cache.clone();
        let mut reader = cache.clone();
        writer.put("k", b"shared").unwrap();
        assert_eq!(std::fs::read(reader.get("k").unwrap()).unwrap(), b"shared");
        assert_eq!(reader.total(), 6);
        assert_eq!(reader.budget(), BLOB_BUDGET_DEFAULT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_name_names_where_a_put_lands() {
        // The name is a pure function of the key and it is the one `put` uses,
        // so a reader that holds only the directory can build the path a blob
        // sits at — how a widget finds a cached photo without the cap.
        let dir = scratch("file_name");
        let mut c = BlobCache::at(dir.clone(), BLOB_BUDGET_DEFAULT);
        let put = c.put("tg:unique", b"photo bytes").unwrap();
        let built = dir.join(file_name("tg:unique"));
        assert_eq!(built, put, "the built path is where put wrote");
        assert_eq!(std::fs::read(&built).unwrap(), b"photo bytes");
        // It is hex of the digest — stable and directory-safe — and a
        // different key names a different file.
        assert_eq!(file_name("tg:unique").len(), 64);
        assert!(file_name("tg:unique").bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(file_name("tg:unique"), file_name("tg:other"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
