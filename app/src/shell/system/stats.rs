//! A storage snapshot and the activity of this session. Disk and uncached
//! SQL reads happen at open or refresh, on a reader thread in a real run.

use std::any::Any;
use std::path::Path;
use std::sync::{mpsc, Arc};

use kernel::caps::{fmt_size, BlobCache, BlobStats};
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::{Db, Store};
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

#[derive(Debug)]
struct Database {
    bytes: u64,
    reusable: u64,
    /// None for an in-memory store. On-disk bytes include the WAL and SHM.
    disk: Option<(u64, u64)>,
}

impl Database {
    fn read(store: &Store) -> Result<Self, String> {
        let (bytes, reusable) = store
            .conn()
            .query_row(
                "SELECT page_count * page_size, freelist_count * page_size
             FROM pragma_page_count(), pragma_page_size(), pragma_freelist_count()",
                [],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )
            .map_err(|e| e.to_string())?;
        let disk = store
            .conn()
            .path()
            .filter(|p| !p.is_empty())
            .map(|path| {
                let main = std::fs::metadata(path)
                    .map_err(|e| format!("{path}: {e}"))?
                    .len();
                let wal = sidecar_size(Path::new(&format!("{path}-wal")))?;
                let shm = sidecar_size(Path::new(&format!("{path}-shm")))?;
                Ok::<_, String>((main + wal + shm, wal))
            })
            .transpose()?;
        Ok(Self {
            bytes,
            reusable,
            disk,
        })
    }
}

/// Missing SQLite sidecars are normal, including with an in-memory journal.
fn sidecar_size(path: &Path) -> Result<u64, String> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Jobs {
    pending: u64,
    running: u64,
    failed: u64,
}

impl Jobs {
    fn read(store: &Store) -> Result<Self, String> {
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FILTER (WHERE status = 'pending'),
                    COUNT(*) FILTER (WHERE status = 'processing'),
                    COUNT(*) FILTER (WHERE status = 'failed') FROM effect",
                [],
                |r| {
                    Ok(Self {
                        pending: r.get::<_, i64>(0)? as u64,
                        running: r.get::<_, i64>(1)? as u64,
                        failed: r.get::<_, i64>(2)? as u64,
                    })
                },
            )
            .map_err(|e| e.to_string())
    }
}

struct Snapshot {
    database: Result<Database, String>,
    cache: Result<BlobStats, String>,
    jobs: Result<Jobs, String>,
}

impl Snapshot {
    fn read(db: Arc<Db>, cache: Result<BlobCache, String>) -> Self {
        let store = Store::with_db(db).map_err(|e| e.to_string());
        Self {
            database: store
                .as_ref()
                .map_err(Clone::clone)
                .and_then(Database::read),
            jobs: store.as_ref().map_err(Clone::clone).and_then(Jobs::read),
            cache: cache.and_then(|c| c.stats()),
        }
    }

    fn errors(&self) -> String {
        [
            self.database
                .as_ref()
                .err()
                .map(|e| format!("Database: {e}")),
            self.cache
                .as_ref()
                .err()
                .map(|e| format!("File cache: {e}")),
            self.jobs.as_ref().err().map(|e| format!("Jobs: {e}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
    }
}

/// The instance keeps the last result while a refresh is in flight. Repeated
/// refreshes share that read; a closed panel drops its receiver.
pub struct Stats {
    id: PanelId,
    snapshot: Option<Snapshot>,
    pending: Option<mpsc::Receiver<Snapshot>>,
    error: Option<String>,
}

#[derive(Debug)]
struct StatsReady;

impl Stats {
    pub const TAG: Tag = Tag("stats");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    fn refresh(&mut self, s: &Session) {
        self.poll();
        if self.pending.is_some() {
            return;
        }
        self.error = None;
        let db = s.store().db();
        let cache = s
            .world()
            .with_cap::<BlobCache, _>(|c| c.clone())
            .map_err(|_| "not available in this session".to_string());
        if s.workers().is_inline() {
            // Tests and library mounts complete within their virtual clock.
            // This runs on open or a verb, never in draw_walk.
            self.snapshot = Some(Snapshot::read(db, cache));
            return;
        }
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("system-stats".into())
            .spawn(move || {
                if tx.send(Snapshot::read(db, cache)).is_ok() {
                    Cx::post_action(StatsReady);
                }
            }) {
            Ok(_) => self.pending = Some(rx),
            Err(e) => self.error = Some(format!("Could not refresh stats: {e}")),
        }
    }

    /// Only receives prepared values; no SQL, filesystem access, or waiting.
    fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        match rx.try_recv() {
            Ok(snapshot) => self.snapshot = Some(snapshot),
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("The stats reader stopped. Refresh to try again.".into());
            }
        }
        self.pending = None;
        true
    }
}

impl Panel for Stats {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "superapp stats".into()
    }

    fn about(&self) -> String {
        "Superapp's local storage and activity: SQLite size, its on-disk files \
         including the write-ahead log, reusable database pages, completed \
         cached files and their byte budget, open panels, occupied workspaces, \
         background workers, and queued, running and failed jobs. It takes no \
         arguments. Opening or refreshing reads a new storage and job snapshot; \
         session counts follow the current workspace layout. Cache measurements \
         exclude its index and unfinished downloads and do not change recency \
         or evict anything. Missing capabilities and read failures are shown \
         as unavailable values with an explanation."
            .into()
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("system.stats.refresh", "refresh", Some('r'))]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "system.stats.refresh" {
            self.refresh(s);
            s.redraw();
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct StatsKind;

impl PanelKind for StatsKind {
    fn tag(&self) -> Tag {
        Stats::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let mut stats = Stats {
            id: id.clone(),
            snapshot: None,
            pending: None,
            error: None,
        };
        stats.refresh(cx.session());
        Box::new(stats)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct StatsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for StatsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Some(props) = scope.props.get::<PanelProps>() {
            if let Some(stats) = props.panel.borrow_mut().as_any().downcast_mut::<Stats>() {
                if stats.poll() {
                    self.view.redraw(cx);
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(stats) = props.panel.borrow_mut().as_any().downcast_mut::<Stats>() {
            stats.poll();
            self.fill(cx, stats);
        }
        if let Some(s) = scope.data.get_mut::<Session>() {
            let workspaces = &s.ws().wss;
            self.value(
                cx,
                ids!(panels),
                workspaces
                    .iter()
                    .map(|w| w.slots.len())
                    .sum::<usize>()
                    .to_string(),
            );
            self.value(
                cx,
                ids!(workspaces),
                workspaces
                    .iter()
                    .filter(|w| !w.is_empty())
                    .count()
                    .to_string(),
            );
            self.value(cx, ids!(workers), s.workers().names().len().to_string());
        }
        let step = self.view.draw_walk(cx, scope, walk);
        for (label, path) in [
            ("stats database", ids!(database.value)),
            ("stats file cache", ids!(cache.value)),
            ("stats cached files", ids!(files.value)),
            ("stats open panels", ids!(panels.value)),
            ("stats queued jobs", ids!(pending.value)),
            ("stats failed jobs", ids!(failed.value)),
        ] {
            let w = self.view.widget(cx, path);
            let rect = w.area().rect(cx);
            if rect.size.x > 0.0 {
                props.hits.add(label, rect, MouseCursor::Text, props.slot);
            }
        }
        step
    }
}

impl StatsPanel {
    fn value(&self, cx: &mut Cx, row: &[LiveId], value: String) {
        self.view
            .widget(cx, row)
            .text_input(cx, ids!(value))
            .set_text(cx, &value);
    }

    fn fill(&self, cx: &mut Cx, stats: &Stats) {
        let snapshot = stats.snapshot.as_ref();
        let database = snapshot.and_then(|s| s.database.as_ref().ok());
        let cache = snapshot.and_then(|s| s.cache.as_ref().ok());
        let jobs = snapshot.and_then(|s| s.jobs.as_ref().ok());
        let absent = if snapshot.is_none() && stats.pending.is_some() {
            "…"
        } else {
            "unavailable"
        };
        let bytes = |n: Option<u64>| n.map(fmt_size).unwrap_or_else(|| absent.into());
        let count = |n: Option<u64>| n.map(|n| n.to_string()).unwrap_or_else(|| absent.into());
        for (row, text) in [
            (ids!(database), bytes(database.map(|d| d.bytes))),
            (
                ids!(disk),
                database
                    .map(|d| {
                        d.disk
                            .map(|(n, _)| fmt_size(n))
                            .unwrap_or_else(|| "in memory".into())
                    })
                    .unwrap_or_else(|| absent.into()),
            ),
            (
                ids!(wal),
                database
                    .map(|d| {
                        d.disk
                            .map(|(_, n)| fmt_size(n))
                            .unwrap_or_else(|| "—".into())
                    })
                    .unwrap_or_else(|| absent.into()),
            ),
            (ids!(reusable), bytes(database.map(|d| d.reusable))),
            (ids!(cache), bytes(cache.map(|c| c.bytes))),
            (ids!(budget), bytes(cache.map(|c| c.budget))),
            (ids!(files), count(cache.map(|c| c.files))),
            (ids!(pending), count(jobs.map(|j| j.pending))),
            (ids!(running), count(jobs.map(|j| j.running))),
            (ids!(failed), count(jobs.map(|j| j.failed))),
        ] {
            self.value(cx, row, text);
        }
        self.view.label(cx, ids!(status)).set_text(
            cx,
            if stats.pending.is_some() {
                "refreshing storage and jobs…"
            } else {
                "storage and jobs update on refresh"
            },
        );
        let errors = [
            stats.error.clone().unwrap_or_default(),
            snapshot.map(Snapshot::errors).unwrap_or_default(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        self.view
            .widget(cx, ids!(errors))
            .set_visible(cx, !errors.is_empty());
        self.view
            .text_input(cx, ids!(errors.text))
            .set_text(cx, &errors);
    }
}

#[cfg(test)]
mod tests {
    use kernel::app::{App, Env, Mode};
    use kernel::caps::Blobs;

    use super::*;

    static APPS: &[&dyn App] = &[&super::super::SYSTEM];

    fn grow(store: &Store) {
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO meta(key, value) VALUES('stats-fixture', zeroblob(131072))",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn database_stats_follow_growth_and_reusable_pages_in_memory() {
        let store = Store::open(None, &[]).unwrap();
        let before = Database::read(&store).unwrap();
        assert!(before.bytes > 0);
        assert!(before.disk.is_none());
        // This table is created after the replication table list is fixed,
        // so deleting its payload does not retain it in a changeset.
        store
            .write(|tx| {
                tx.execute_batch(
                    "CREATE TABLE stats_fixture(id INTEGER PRIMARY KEY, bytes BLOB);
                              INSERT INTO stats_fixture VALUES(1, zeroblob(131072))",
                )
            })
            .unwrap();
        let grown = Database::read(&store).unwrap();
        assert!(grown.bytes > before.bytes);
        store
            .write(|tx| {
                tx.execute("DELETE FROM stats_fixture", [])?;
                Ok(())
            })
            .unwrap();
        let after = Database::read(&store).unwrap();
        assert_eq!(after.bytes, grown.bytes, "deleting leaves reusable pages");
        assert!(after.reusable > grown.reusable);
    }

    #[test]
    fn database_disk_usage_includes_wal_without_checkpointing() {
        let dir = std::env::temp_dir().join(format!("superapp-stats-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("custom store.sqlite");
        let store = Store::open(Some(&path), &[]).unwrap();
        grow(&store);
        let main = std::fs::metadata(&path).unwrap().len();
        let wal = sidecar_size(&dir.join("custom store.sqlite-wal")).unwrap();
        let shm = sidecar_size(&dir.join("custom store.sqlite-shm")).unwrap();
        assert!(wal > 0, "fixture has uncheckpointed data");
        assert_eq!(
            Database::read(&store).unwrap().disk,
            Some((main + wal + shm, wal))
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), main);
        assert_eq!(
            sidecar_size(&dir.join("custom store.sqlite-wal")).unwrap(),
            wal
        );
        assert_eq!(sidecar_size(&dir.join("absent-wal")).unwrap(), 0);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refresh_reads_the_same_cache_and_current_job_states_without_writing() {
        let mut s = Session::fake(APPS);
        let slot = super::super::tests::open(&mut s, Stats::id());
        let panel = s.panel(slot).unwrap();
        let mut panel = panel.borrow_mut();
        let stats = panel.as_any().downcast_mut::<Stats>().unwrap();
        assert_eq!(
            stats
                .snapshot
                .as_ref()
                .unwrap()
                .cache
                .as_ref()
                .unwrap()
                .files,
            0
        );
        s.world()
            .with_cap::<dyn Blobs, _>(|c| c.put("stats-test", &[1; 123]))
            .unwrap()
            .unwrap();
        s.store()
            .write(|tx| {
                for status in [
                    "pending",
                    "pending",
                    "processing",
                    "failed",
                    "done",
                    "obsolete",
                ] {
                    tx.execute(
                        "INSERT INTO effect(kind, payload, status, created, updated)
                     VALUES('stats.fixture', '{}', ?1, 1, 1)",
                        [status],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let revision = s.store().revision(&["effect", "panel", "meta"]);
        stats.run("system.stats.refresh", &mut s);
        let snapshot = stats.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.cache.as_ref().unwrap().bytes, 123);
        assert_eq!(
            snapshot.jobs.as_ref().unwrap(),
            &Jobs {
                pending: 2,
                running: 1,
                failed: 1
            }
        );
        assert_eq!(s.store().revision(&["effect", "panel", "meta"]), revision);
        s.world()
            .with_cap::<dyn Blobs, _>(|c| c.remove("stats-test"))
            .unwrap();
    }

    #[test]
    fn unavailable_cache_does_not_hide_other_measurements() {
        let mut s = Session::fake_mode(APPS, Mode::Deny, &Env::default());
        let slot = super::super::tests::open(&mut s, Stats::id());
        let panel = s.panel(slot).unwrap();
        let mut panel = panel.borrow_mut();
        let stats = panel.as_any().downcast_mut::<Stats>().unwrap();
        let snapshot = stats.snapshot.as_ref().unwrap();
        assert!(snapshot.database.is_ok());
        assert!(snapshot.jobs.is_ok());
        assert!(snapshot.cache.is_err());
        assert!(snapshot.errors().contains("File cache:"));
    }
}
