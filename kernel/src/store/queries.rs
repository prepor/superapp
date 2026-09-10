//! UI query snapshots. Preparing and executing SQL happens in the blocking
//! pool; the UI only checks generations, requests a snapshot, and publishes it.

use super::*;
use tokio::sync::mpsc;

struct Snapshot {
    key: QueryKey,
    deps: Vec<String>,
    gens: Vec<u64>,
    rows: Result<Box<dyn Any + Send>, String>,
}

struct Failure {
    deps: Vec<String>,
    gens: Vec<u64>,
    attempts: u32,
    retry_at: std::time::Instant,
}

enum Event {
    Rows(Snapshot),
    External,
    DisplayReady,
    Retry,
}

pub(super) struct Background {
    pending: HashSet<QueryKey>,
    failures: HashMap<QueryKey, Failure>,
    send: mpsc::UnboundedSender<Event>,
    receive: mpsc::UnboundedReceiver<Event>,
    notify: Arc<dyn Fn() + Send + Sync>,
    display_pending: Arc<AtomicBool>,
}

struct SnapshotScope<'a>(&'a Store);

impl Drop for SnapshotScope<'_> {
    fn drop(&mut self) {
        self.0.snapshot_scopes.set(self.0.snapshot_scopes.get() - 1);
    }
}

impl Store {
    /// Publish completed queries, then keep those display snapshots stable
    /// for this scope. New reads still run in the background; their results
    /// become visible after the draw, on the next poll. Scopes may nest.
    #[must_use = "keep the scope alive until the display read is complete"]
    pub fn snapshot_scope(&self) -> impl Drop + '_ {
        self.poll_background();
        self.snapshot_scopes.set(self.snapshot_scopes.get() + 1);
        SnapshotScope(self)
    }

    /// A selection lookup may remove a key only after this query has an
    /// answer for the current dependencies. Display snapshots can keep old
    /// rows while refreshing; pending lookups must not claim those rows are
    /// still present, or that an old empty answer means a row was deleted.
    pub fn poll_snapshot_rows_sql_deps<T: Send + 'static>(
        &self, id: &'static str, describe: &'static str, sql: &str,
        params: &[Val], deps: &[&str],
        map: fn(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> std::task::Poll<Rc<Vec<T>>> {
        let rows = self.snapshot_rows_sql_deps(id, describe, sql, params, deps, map);
        if !self.ui_attached() {
            return std::task::Poll::Ready(rows);
        }
        let key = (sql.to_owned(), fmt_params(params), TypeId::of::<T>());
        if self.cache.borrow().get(&key).is_some_and(|cached| {
            cached.deps.iter().zip(&cached.gens).all(|(table, generation)| self.gen_of(table) == *generation)
        }) {
            std::task::Poll::Ready(rows)
        } else {
            std::task::Poll::Pending
        }
    }

    pub fn snapshot_rows<T: Send + 'static>(&self, query: &'static Q, params: &[Val],
        map: fn(&rusqlite::Row) -> rusqlite::Result<T>) -> Rc<Vec<T>> {
        self.snapshot_rows_sql(query.id, query.describe, query.sql, params, map)
    }

    pub fn snapshot_rows_sql<T: Send + 'static>(&self, id: &'static str, describe: &'static str,
        sql: &str, params: &[Val], map: fn(&rusqlite::Row) -> rusqlite::Result<T>) -> Rc<Vec<T>> {
        self.snapshot_rows_sql_deps(id, describe, sql, params, &[], map)
    }

    /// Changes when background snapshots arrive, independently of commits.
    pub fn query_revision(&self) -> u64 { self.query_revision.get() }

    /// An in-memory stamp for controls that assemble offers from several
    /// queries. Includes commits so an unchanged caret still requests fresh
    /// snapshots, and completed reads so the offer appears without typing.
    pub fn display_revision(&self) -> (u64, u64, u64) {
        (self.db.commits.lock().expect("commit clock").serial,
            self.query_revision.get(), self.mem_version.get())
    }

    /// Whether this reader is waiting for a requested snapshot.
    pub fn queries_pending(&self) -> bool {
        self.background.borrow().as_ref().is_some_and(|b| !b.pending.is_empty())
    }

    pub fn ui_attached(&self) -> bool {
        self.background.borrow().is_some()
    }

    /// A background UI completion must both wake the platform and record a
    /// display change. A signal alone can leave the renderer idle when no SQL
    /// committed: the shell redraws after `poll_external` reports a change.
    /// This advances display readiness, without invalidating table generations.
    pub fn ui_waker(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.background.borrow().as_ref().map(|background| {
            let (send,notify,pending) = (background.send.clone(),background.notify.clone(),background.display_pending.clone());
            Arc::new(move || {
                if !pending.swap(true,Ordering::AcqRel) && send.send(Event::DisplayReady).is_ok() {notify();}
            }) as Arc<dyn Fn() + Send + Sync>
        })
    }

    /// Attaches this reader to the UI event loop. Display snapshots load in
    /// the background and mutations report completion on a subsequent event.
    /// Service worlds and deterministic fixtures retain ordinary readers.
    pub fn attach_ui(&self, notify: impl Fn() + Send + Sync + 'static) {
        if self.background.borrow().is_some() { return; }
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        self.db.listeners.lock().expect("commit listeners").push(Arc::downgrade(&notify));
        let (send, receive) = mpsc::unbounded_channel();
        *self.background.borrow_mut() = Some(Background {
            pending: HashSet::new(), failures: HashMap::new(), send: send.clone(), receive, notify: notify.clone(),
            display_pending: Arc::new(AtomicBool::new(false)),
        });
        // Only the writer's data_version distinguishes external commits:
        // commits on that connection never advance its own data_version.
        let db = self.db.clone();
        crate::runtime::spawn(async move {
            let mut last = None;
            loop {
                let version = db.raw_async(|conn| conn.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))).await;
                if let Ok(version) = version {
                    // Invalidating the initial sample also covers a foreign
                    // commit between the first snapshot and this baseline.
                    if last.replace(version) != Some(version) {
                        if send.send(Event::External).is_err() { return; }
                        notify();
                    }
                }
                tokio::select! {
                    () = send.closed() => return,
                    () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {},
                }
            }
        });
    }

    pub(super) fn poll_background(&self) -> bool {
        if self.snapshot_scopes.get() > 0 { return false; }
        let mut background = self.background.borrow_mut();
        let Some(background) = background.as_mut() else { return false; };
        let mut changed = false;
        while let Ok(event) = background.receive.try_recv() {
            changed = true;
            match event {
                Event::External => {
                    self.external_generation.set(self.external_generation.get() + 1);
                }
                Event::DisplayReady => { background.display_pending.store(false,Ordering::Release); },
                Event::Retry => {},
                Event::Rows(snapshot) => {
                    background.pending.remove(&snapshot.key);
                    // Preserve the pre-read stamp: a commit during the query
                    // makes this result stale and schedules its replacement.
                    match snapshot.rows {
                        Ok(rows) => {
                            background.failures.remove(&snapshot.key);
                            let rows: Box<dyn Any> = rows;
                            self.cache.borrow_mut().insert(snapshot.key, Cached {
                                deps: Rc::new(snapshot.deps), gens: snapshot.gens, rows: Rc::from(rows),
                            });
                        }
                        Err(error) => {
                            eprintln!("store: background query failed: {error}");
                            let attempts = background.failures.get(&snapshot.key).map_or(1, |f| f.attempts.saturating_add(1));
                            let delay = std::time::Duration::from_secs(1_u64 << attempts.saturating_sub(1).min(5));
                            background.failures.insert(snapshot.key, Failure {
                                deps: snapshot.deps, gens: snapshot.gens, attempts,
                                retry_at: std::time::Instant::now() + delay,
                            });
                            let (send, notify) = (background.send.clone(), background.notify.clone());
                            crate::runtime::spawn(async move {
                                tokio::select! {
                                    () = send.closed() => {},
                                    () = tokio::time::sleep(delay) => {
                                        if send.send(Event::Retry).is_ok() { notify(); }
                                    }
                                }
                            });
                        }
                    }
                }
            }
        }
        if changed {
            self.query_revision.set(self.query_revision.get() + 1);
            self.redraw.set(true);
        }
        changed
    }

    pub(super) fn background_rows<T: Send + 'static>(
        &self, id: &'static str, sql: &str, params: &[Val], also: &[&str],
        map: fn(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> Rc<Vec<T>> {
        self.poll_background();
        let key = (sql.to_owned(), fmt_params(params), TypeId::of::<T>());
        let cache = self.cache.borrow();
        let cached = cache.get(&key);
        let rows = cached.and_then(|c| c.rows.clone().downcast::<Vec<T>>().ok())
            .unwrap_or_else(|| Rc::new(Vec::new()));
        if cached.is_some_and(|c| c.deps.iter().zip(&c.gens).all(|(t, g)| self.gen_of(t) == *g)) {
            return rows;
        }
        drop(cache);
        let mut background = self.background.borrow_mut();
        let background = background.as_mut().expect("background query reader");
        if background.failures.get(&key).is_some_and(|failed| {
            std::time::Instant::now() < failed.retry_at
                && failed.deps.iter().zip(&failed.gens).all(|(table, generation)| self.gen_of(table) == *generation)
        }) { return rows; }
        if !background.pending.insert(key.clone()) { return rows; }
        let (send, notify) = (background.send.clone(), background.notify.clone());
        let db = self.db.clone();
        let commits = db.commits.lock().expect("commit clock").clone();
        let local = self.generations.borrow().clone();
        let external = self.external_generation.get();
        let sql = sql.to_owned();
        let params = params.to_vec();
        let also: Vec<String> = also.iter().map(|s| (*s).to_owned()).collect();
        crate::runtime::spawn(async move {
            let queried = db.read_async(move |conn| {
                let seen: Arc<Mutex<BTreeSet<String>>> = Arc::default();
                let reads = seen.clone();
                conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                    if let AuthAction::Read { table_name, .. } = ctx.action {
                        reads.lock().expect("read dependencies").insert(table_name.to_owned());
                    }
                    Authorization::Allow
                }))?;
                let result = (|| {
                    let mut statement = conn.prepare(&sql)?;
                    let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), map)?.collect::<rusqlite::Result<Vec<T>>>();
                    rows
                })();
                conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
                let mut deps: Vec<_> = seen.lock().expect("read dependencies").iter().cloned().collect();
                deps.extend(also);
                Ok((deps, result))
            }).await;
            let (deps, result) = queried.unwrap_or_else(|error| (Vec::new(), Err(error)));
            let rows = result.map(|rows| Box::new(rows) as Box<dyn Any + Send>)
                .map_err(|error| format!("{id}: {error}"));
            let gens = deps.iter().map(|table| commits.reset + commits.tables.get(table).copied().unwrap_or(0)
                + external + local.get(table).copied().unwrap_or(0)).collect();
            if send.send(Event::Rows(Snapshot { key, deps, gens, rows })).is_ok() { notify(); }
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    thread_local! { static UI: Cell<bool> = const { Cell::new(false) }; }

    fn off_ui(row: &rusqlite::Row) -> rusqlite::Result<i64> {
        assert!(!UI.get(), "SQL row construction ran on the UI thread");
        row.get(0)
    }

    fn receive<T>(receive: &mut tokio::sync::mpsc::UnboundedReceiver<T>) -> T {
        crate::runtime::block_on(async {
            tokio::time::timeout(Duration::from_secs(5), receive.recv())
                .await.expect("background work completed").expect("sender remains alive")
        })
    }

    fn until<T>(woke: &mut mpsc::UnboundedReceiver<()>, mut ready: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = ready() { return value; }
            assert!(Instant::now() < deadline, "background result arrived");
            receive(woke);
        }
    }

    #[test]
    fn readonly_ui_completion_requests_a_frame_without_invalidating_data() {
        let store = Store::open(None,&[]).unwrap();
        let (notify,woke) = std::sync::mpsc::channel();
        store.attach_ui(move || {let _ = notify.send(());});
        // Consume the initial external-connection sample before measuring
        // this completion, so it cannot mask a missing redraw notification.
        woke.recv_timeout(Duration::from_secs(5)).unwrap();
        store.poll_external();
        store.take_redraw();
        let data = store.revision(&["meta"]);
        let display = store.display_revision();
        let wake = store.ui_waker().unwrap();
        let another = store.ui_waker().unwrap();
        let later = wake.clone();
        std::thread::spawn(move || {for _ in 0..32 {wake();another();}}).join().unwrap();
        woke.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(woke.try_recv().is_err(),"future wakeups coalesce across handles before the next frame");
        assert!(store.poll_external(),"a readonly completion must reach the shell's redraw gate");
        assert!(store.take_redraw(),"the completed snapshot becomes visible without another input event");
        assert_eq!(display.1+1,store.display_revision().1,"one presentation generation per batch");
        assert_eq!(data,store.revision(&["meta"]),"display readiness does not expire the underlying snapshot");
        assert!(!store.poll_external(),"the completion is consumed once");
        assert!(!store.take_redraw());
        later();
        woke.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(store.poll_external(),"a later completion can request its own frame");
        assert_eq!(data,store.revision(&["meta"]));
        assert!(!store.poll_external());
    }

    #[test]
    fn display_queries_execute_off_ui_and_refresh_after_a_commit() {
        let store = Store::open(None, &[]).unwrap();
        store.write(|tx| tx.execute("INSERT INTO meta(key,value) VALUES('snapshot-test',1)", [])).unwrap();
        let (wake, mut woke) = mpsc::unbounded_channel();
        store.attach_ui(move || { let _ = wake.send(()); });
        UI.set(true);
        let query = || store.snapshot_rows_sql_deps("snapshot-test", "snapshot",
            "SELECT value FROM meta WHERE key='snapshot-test'", &[], &[], off_ui);
        assert!(query().is_empty(), "a cold display starts with an unloaded page");
        assert_eq!(until(&mut woke, || query().first().copied()), 1);
        let before = store.query_revision();
        store.write(|tx| tx.execute("UPDATE meta SET value=2 WHERE key='snapshot-test'", [])).unwrap();
        assert_eq!(&*query(), &[1], "the old snapshot stays visible while refreshing");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "updated snapshot arrived");
            receive(&mut woke);
            if query().first() == Some(&2) { break; }
        }
        assert!(store.query_revision() > before);
        // A domain lookup never treats an in-flight display request as absence.
        assert_eq!(&*store.rows_sql("domain", "domain lookup", "SELECT 7", &[], |r| r.get::<_, i64>(0)), &[7]);
        UI.set(false);
    }

    #[test]
    fn a_draw_keeps_its_snapshot_when_a_refresh_finishes_between_rows() {
        let store = Store::open(None, &[]).unwrap();
        store.write(|tx| tx.execute("INSERT INTO meta(key,value) VALUES('draw-snapshot',1)", [])).unwrap();
        let (wake, mut woke) = mpsc::unbounded_channel();
        store.attach_ui(move || { let _ = wake.send(()); });
        until(&mut woke, || {
            store.poll_background();
            (store.external_generation.get() > 0).then_some(())
        });
        let query = || store.snapshot_rows_sql("draw-snapshot", "draw snapshot",
            "SELECT value FROM meta WHERE key='draw-snapshot'", &[], off_ui);
        assert_eq!(until(&mut woke, || query().first().copied()), 1);
        let scope = store.snapshot_scope();
        let revision = store.query_revision();
        store.write(|tx| tx.execute("UPDATE meta SET value=2 WHERE key='draw-snapshot'", [])).unwrap();
        assert_eq!(&*query(), &[1]);
        // Wait until the worker has delivered the refreshed rows, without
        // publishing them into the frame that is still being assembled.
        until(&mut woke, || store.background.borrow().as_ref()
            .is_some_and(|b| !b.receive.is_empty()).then_some(()));
        let nested = store.snapshot_scope();
        assert_eq!(&*query(), &[1], "later rows in this draw use the same page");
        drop(nested);
        assert_eq!(&*query(), &[1], "a nested draw cannot release the outer scope");
        assert_eq!(store.query_revision(), revision);
        drop(scope);
        assert_eq!(&*query(), &[2], "the next draw publishes the completed refresh");
        assert!(store.query_revision() > revision);
    }

    #[test]
    fn one_sql_query_keeps_each_result_type_and_failure_independent() {
        const SQL: &str = "SELECT 7";
        let store = Store::open(None, &[]).unwrap();
        let (wake, mut woke) = mpsc::unbounded_channel();
        store.attach_ui(move || { let _ = wake.send(()); });
        until(&mut woke, || {
            store.poll_background();
            (store.external_generation.get() > 0).then_some(())
        });
        let numbers = store.rows_sql("typed-number", "number", SQL, &[], |row| row.get::<_, i64>(0));
        assert_eq!(&*numbers, &[7]);
        let text = || store.snapshot_rows_sql("typed-text", "text", SQL, &[], |row| {
            row.get::<_, i64>(0).map(|value| value.to_string())
        });
        assert!(text().is_empty(), "a differently typed cache entry is a miss");
        assert_eq!(until(&mut woke, || text().first().cloned()), "7");
        assert!(Rc::ptr_eq(&numbers, &store.snapshot_rows_sql("typed-number", "number", SQL,
            &[], |row| row.get::<_, i64>(0))), "loading text must not replace the number snapshot");

        store.snapshot_rows_sql("typed-failure", "failure", SQL, &[], |_| {
            Err::<usize, _>(rusqlite::Error::InvalidQuery)
        });
        until(&mut woke, || {
            store.poll_background();
            store.background.borrow().as_ref().is_some_and(|b| !b.failures.is_empty()).then_some(())
        });
        let unsigned = || store.snapshot_rows_sql("typed-unsigned", "unsigned", SQL, &[], |row| row.get::<_, u32>(0));
        assert!(unsigned().is_empty());
        assert!(store.queries_pending(), "another result type must not inherit a failed mapper's backoff");
        assert_eq!(until(&mut woke, || unsigned().first().copied()), 7);
        assert_eq!(&*text(), &["7"]);
        assert!(Rc::ptr_eq(&numbers, &store.rows_sql("typed-number", "number", SQL,
            &[], |row| row.get::<_, i64>(0))));
    }

    #[test]
    fn external_updates_survive_a_local_commit_in_the_same_poll_interval() {
        let dir = std::env::temp_dir().join(format!("store-external-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.sqlite");
        let store = Store::open(Some(&path), &[]).unwrap();
        crate::runtime::block_on(store.db.raw_async(|conn| conn.execute_batch(
            "CREATE TABLE observed_a(value INTEGER); INSERT INTO observed_a VALUES(1);
             CREATE TABLE observed_b(value INTEGER); INSERT INTO observed_b VALUES(1);"))).unwrap();
        let external = Store::open(Some(&path), &[]).unwrap();
        let (wake, mut woke) = mpsc::unbounded_channel();
        store.attach_ui(move || { let _ = wake.send(()); });
        until(&mut woke, || {
            store.poll_background();
            (store.external_generation.get() > 0).then_some(())
        });
        let query = || store.snapshot_rows_sql_deps("external-test", "external snapshot",
            "SELECT value FROM observed_b", &[], &[], off_ui);
        assert_eq!(until(&mut woke, || query().first().copied()), 1);
        // These writers have separate commit clocks, as another process would.
        store.write(|tx| tx.execute("UPDATE observed_a SET value=2", [])).unwrap();
        external.write(|tx| tx.execute("UPDATE observed_b SET value=2", [])).unwrap();
        until(&mut woke, || (query().first() == Some(&2)).then_some(()));
        drop(store);
        drop(external);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_refresh_keeps_the_old_snapshot_and_retries_on_a_new_revision() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FAIL: AtomicBool = AtomicBool::new(false);
        fn mapped(row: &rusqlite::Row) -> rusqlite::Result<i64> {
            if FAIL.swap(false, Ordering::SeqCst) { Err(rusqlite::Error::InvalidQuery) }
            else { row.get(0) }
        }
        let store = Store::open(None, &[]).unwrap();
        store.write(|tx| tx.execute("INSERT INTO meta(key,value) VALUES('failed-refresh',1)", [])).unwrap();
        let (wake, mut woke) = mpsc::unbounded_channel();
        store.attach_ui(move || { let _ = wake.send(()); });
        until(&mut woke, || {
            store.poll_background();
            (store.external_generation.get() > 0).then_some(())
        });
        let query = || store.snapshot_rows_sql_deps("failed-refresh", "refresh",
            "SELECT value FROM meta WHERE key='failed-refresh'", &[], &[], mapped);
        assert_eq!(until(&mut woke, || query().first().copied()), 1);
        store.write(|tx| tx.execute("UPDATE meta SET value=2 WHERE key='failed-refresh'", [])).unwrap();
        FAIL.store(true, Ordering::SeqCst);
        assert_eq!(&*query(), &[1]);
        until(&mut woke, || {
            store.poll_background();
            store.background.borrow().as_ref().is_some_and(|b| !b.failures.is_empty()).then_some(())
        });
        assert_eq!(&*query(), &[1], "a failed refresh must not erase its last successful rows");
        assert!(!store.queries_pending(), "a failure backs off instead of retrying on every draw");
        store.write(|tx| tx.execute("UPDATE meta SET value=3 WHERE key='failed-refresh'", [])).unwrap();
        until(&mut woke, || (query().first() == Some(&3)).then_some(()));
    }

    #[test]
    fn a_failed_cold_query_without_table_dependencies_retries_after_backoff() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FAIL: AtomicBool = AtomicBool::new(true);
        fn mapped(row: &rusqlite::Row) -> rusqlite::Result<i64> {
            if FAIL.swap(false, Ordering::SeqCst) { Err(rusqlite::Error::InvalidQuery) }
            else { row.get(0) }
        }
        let store = Store::open(None, &[]).unwrap();
        let (wake, mut woke) = mpsc::unbounded_channel();
        store.attach_ui(move || { let _ = wake.send(()); });
        let query = || store.snapshot_rows_sql_deps("cold-retry", "cold retry", "SELECT 7", &[], &[], mapped);
        assert_eq!(until(&mut woke, || query().first().copied()), 7);
    }

    #[test]
    fn submitting_a_write_does_not_wait_and_dropping_its_reply_does_not_cancel_it() {
        let store = Store::open(None, &[]).unwrap();
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let _first = store.submit_write(move |_tx| {
            entered.send(()).unwrap();
            held.recv().unwrap();
            Ok(())
        }).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        // The writer is provably blocked until after this call returns.
        drop(store.submit_write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('accepted-write',1)", [])
        }).unwrap());
        release.send(()).unwrap();
        store.write(|_| Ok(())).unwrap();
        let value: i64 = store.conn().query_row("SELECT value FROM meta WHERE key='accepted-write'", [], |r| r.get(0)).unwrap();
        assert_eq!(value, 1);
    }

    #[test]
    fn an_async_write_yields_while_the_writer_is_busy_and_rolls_back_on_error() {
        let store = Store::open(None, &[]).unwrap();
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let _first = store.submit_write(move |_| {
            entered.send(()).unwrap();
            held.recv().unwrap();
            Ok(())
        }).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        crate::runtime::block_on(async {
            let write = store.write_async(|tx| {
                tx.execute("INSERT INTO meta(key,value) VALUES('rolled-back',1)", [])?;
                Err::<(), _>(rusqlite::Error::InvalidQuery)
            });
            tokio::pin!(write);
            tokio::select! {
                biased;
                _ = &mut write => panic!("writer should still be held"),
                () = tokio::task::yield_now() => {},
            }
            release.send(()).unwrap();
            assert!(write.await.is_err());
        });
        let count: i64 = store.conn().query_row("SELECT COUNT(*) FROM meta WHERE key='rolled-back'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    #[ignore = "manual comparison of synchronous and background UI query latency"]
    fn expensive_query_ui_latency() {
        const SQL: &str = "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000000) SELECT sum(i) FROM n";
        let store = Store::open(None, &[]).unwrap();
        let start = Instant::now();
        let expected: i64 = store.conn().query_row(SQL, [], |r| r.get(0)).unwrap();
        let sync = start.elapsed();
        let (wake, mut woke) = mpsc::unbounded_channel();
        store.attach_ui(move || { let _ = wake.send(()); });
        let start = Instant::now();
        assert!(store.snapshot_rows_sql_deps("slow", "slow query", SQL, &[], &[], off_ui).is_empty());
        let ui = start.elapsed();
        let value = until(&mut woke, || store.snapshot_rows_sql_deps("slow", "slow query", SQL, &[], &[], off_ui).first().copied());
        assert_eq!(value, expected);
        eprintln!("identical SQLite query: sync UI wait {sync:?}; async UI submission {ui:?}; background completion {:?}", start.elapsed());
    }
}
