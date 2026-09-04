//! What an app is, and what the kernel asks of the list of them.
//!
//! The binary is the only place that knows which apps exist. The kernel
//! builds one [`Apps`] registry from that list at boot and asks the list for
//! everything; it never asks an app by name.

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use crate::caps::{ClockSource, MemSecrets, SecretsFactory};
use crate::effect::{Job, Registry, World};
use crate::panel::{PanelId, PanelKind, Tag};
use crate::search;
use crate::store::{Db, Store};

pub use crate::problems::{Announced, Problem, ProblemSource};

/// One app: what it adds to the shell. The binary lists the apps once; the
/// kernel asks the list for everything and never asks an app by name.
pub trait App: Any + Sync + Send + 'static {
    /// One word, stable. Prefixes the app's schema key (`schema:mail`) and
    /// names its e2e directory. What another app asks the registry for.
    fn id(&self) -> &'static str;

    /// Every panel kind the app owns. Read once at boot into the registry;
    /// two apps claiming one tag stop the process there, naming both.
    fn kinds(&self) -> &'static [&'static dyn PanelKind];

    /// The app's own migration ladder, applied at every store open after
    /// the kernel's, in app-list order. `None` for an app that stores
    /// nothing.
    fn schema(&self) -> Option<&'static Schema> {
        None
    }

    /// Demo rows for a new store. Called once, on the first open of an
    /// empty store. Must be idempotent: a crash between the seed and its
    /// record repeats it.
    ///
    /// # Errors
    ///
    /// If the store refuses the write.
    fn seed(&self, _store: &Store) -> rusqlite::Result<()> {
        Ok(())
    }

    /// Registers the app's deferred effects (`reg.register::<Move>()`).
    /// Called for every world built, on any thread, so the queue can be
    /// read back and run wherever it is opened.
    fn effects(&self, _reg: &mut Registry) {}

    /// Supplies the app's capabilities for one mode: `Real` gets the
    /// network and the OS, `Fake` the in-memory versions, `Deny` nothing.
    /// Called per world, so a worker's sessions live in that worker's world
    /// and nowhere else. `env` carries the store directory, whether this is
    /// a scripted run, the secrets backend, and the clock.
    fn outside(&self, _mode: Mode, _env: &Env, _caps: &mut Capabilities) {}

    /// The launcher's sources beyond open panels and roots. Each gets its
    /// own thread and store reader in production; under virtual time they
    /// answer inline, so a scripted `type` is followed by its rows in the
    /// same tick.
    fn search_providers(&self) -> Vec<Box<dyn search::Provider>> {
        Vec::new()
    }

    /// Standing conditions this app can be in. Listed on every poll; a
    /// problem is announced once per key and cleared when the source stops
    /// listing it.
    fn problems(&self) -> &'static [&'static dyn ProblemSource] {
        &[]
    }

    /// The background passes the app wants running now, derived from the
    /// store. Asked at boot and again after every action, at the moment
    /// the workers are kicked, so a pass that a new row calls for starts
    /// without a restart. The kernel diffs the answer by [`Worker::name`]:
    /// new names are spawned, missing names retire. Must be cheap: one
    /// cached query.
    fn workers(&self, _store: &Store) -> Vec<Box<dyn Worker>> {
        Vec::new()
    }

    /// The panels the launcher offers whether or not they are open, in
    /// the order the app wants them, each with the label and the words a
    /// query may match. Apps follow the app list.
    fn roots(&self) -> Vec<Root> {
        Vec::new()
    }

    /// For [`Apps::get_as`].
    fn as_any(&self) -> &dyn Any;
}

/// One launcher root.
#[derive(Debug, Clone)]
pub struct Root {
    pub id: PanelId,
    pub label: String,
    /// Extra words a query may match ("log queue" for the effects list).
    pub words: String,
}

impl Root {
    #[must_use]
    pub fn new(id: PanelId, label: impl Into<String>, words: impl Into<String>) -> Root {
        Root {
            id,
            label: label.into(),
            words: words.into(),
        }
    }
}

// -- the registry --------------------------------------------------------------

/// Every app in this build, and the tags they own.
pub struct Apps {
    list: &'static [&'static dyn App],
    kinds: HashMap<Tag, &'static dyn PanelKind>,
}

impl Apps {
    /// The registry, built once at boot.
    ///
    /// # Panics
    ///
    /// If two apps claim one tag — the process stops there, naming both.
    #[must_use]
    pub fn new(list: &'static [&'static dyn App]) -> Apps {
        let mut kinds: HashMap<Tag, &'static dyn PanelKind> = HashMap::new();
        let mut owner: HashMap<Tag, &'static str> = HashMap::new();
        for app in list {
            for kind in app.kinds() {
                let tag = kind.tag();
                if let Some(first) = owner.get(&tag) {
                    panic!("two apps claim the tag {tag}: {first} and {}", app.id());
                }
                owner.insert(tag, app.id());
                kinds.insert(tag, *kind);
            }
        }
        Apps { list, kinds }
    }

    /// An app by id, or `None` when it is not in this build. This is how
    /// apps reach each other: mail asks for `"files"` to read its
    /// clipboard, and offers no *attach* when there is no files app.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&'static dyn App> {
        self.list.iter().copied().find(|a| a.id() == id)
    }

    /// The same, downcast to the app's own type, for its public API.
    #[must_use]
    pub fn get_as<T: App>(&self) -> Option<&'static T> {
        self.list
            .iter()
            .find_map(|a| a.as_any().downcast_ref::<T>())
    }

    /// The kind that opens this tag, or `None` for a tag no app in this
    /// build owns.
    #[must_use]
    pub fn kind(&self, tag: Tag) -> Option<&'static dyn PanelKind> {
        self.kinds.get(&tag).copied()
    }

    /// Every app, in the order the binary listed them.
    #[must_use]
    pub fn list(&self) -> &'static [&'static dyn App] {
        self.list
    }

    /// Every tag any app owns, sorted — a boot check reads this.
    #[must_use]
    pub fn tags(&self) -> Vec<Tag> {
        let mut v: Vec<Tag> = self.kinds.keys().copied().collect();
        v.sort_by_key(|t| t.as_str());
        v
    }

    /// The launcher's roots, apps in list order.
    #[must_use]
    pub fn roots(&self) -> Vec<Root> {
        self.list.iter().flat_map(|a| a.roots()).collect()
    }

    /// The search providers, apps in list order.
    #[must_use]
    pub fn providers(&self) -> Vec<Box<dyn search::Provider>> {
        self.list
            .iter()
            .flat_map(|a| a.search_providers())
            .collect()
    }

    /// Every problem source: the kernel's own first — device sync is not an
    /// app, and an unreachable bucket is a condition every build can be in —
    /// then the apps', in list order.
    #[must_use]
    pub fn problem_sources(&self) -> Vec<&'static dyn ProblemSource> {
        std::iter::once(&crate::repl::BUCKET_PROBLEM as &'static dyn ProblemSource)
            .chain(self.list.iter().flat_map(|a| a.problems().iter().copied()))
            .collect()
    }

    /// What stands right now, every source asked.
    #[must_use]
    pub fn problems(&self, store: &Store) -> Vec<Problem> {
        self.problem_sources()
            .into_iter()
            .flat_map(|s| s.list(store))
            .collect()
    }

    /// The ladders, apps in list order — what [`Store::open`] climbs after
    /// the kernel's.
    #[must_use]
    pub fn schemas(&self) -> Vec<&'static Schema> {
        self.list.iter().filter_map(|a| a.schema()).collect()
    }

    /// Every app's demo rows, once, on the first open of an empty store.
    ///
    /// # Errors
    ///
    /// If any app's seed refuses.
    pub fn seed(&self, store: &Store) -> rusqlite::Result<()> {
        for a in self.list {
            a.seed(store)?;
        }
        Ok(())
    }

    /// The effect registry: every app's deferred kinds. Built per world, on
    /// whatever thread wants one.
    #[must_use]
    pub fn registry(&self) -> Registry {
        registry_for(self.list)
    }

    /// One world's capabilities: the kernel's, then every app's.
    #[must_use]
    pub fn capabilities(&self, mode: Mode, env: &Env) -> Capabilities {
        capabilities_for(self.list, mode, env)
    }

    /// A world over `store`, with this build's effects and capabilities.
    #[must_use]
    pub fn world(&self, store: Store, mode: Mode, env: &Env) -> World {
        world_for(self.list, store, mode, env)
    }
}

/// The effect registry for an app list. A free function because a worker
/// thread builds its own world from the list alone.
#[must_use]
pub fn registry_for(list: &'static [&'static dyn App]) -> Registry {
    let mut reg = Registry::new();
    for a in list {
        a.effects(&mut reg);
    }
    reg
}

/// The capabilities for one world: the kernel's first, so an app — or the
/// shell — may replace one.
#[must_use]
pub fn capabilities_for(list: &'static [&'static dyn App], mode: Mode, env: &Env) -> Capabilities {
    let mut caps = Capabilities::default();
    crate::caps::install(mode, env, &mut caps);
    for a in list {
        a.outside(mode, env, &mut caps);
    }
    caps
}

/// A whole world for one thread.
#[must_use]
pub fn world_for(list: &'static [&'static dyn App], store: Store, mode: Mode, env: &Env) -> World {
    World::new(
        Rc::new(store),
        capabilities_for(list, mode, env),
        registry_for(list),
    )
}

// -- capabilities --------------------------------------------------------------

/// Which outside a world gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Real,
    Fake,
    /// Nothing but the clock: an effect that asks for anything else fails
    /// with *this world has no …*. The default for a library mount.
    Deny,
}

/// What [`App::outside`] may need to build a real or fake backend.
#[derive(Debug, Clone)]
pub struct Env {
    pub db_dir: Option<PathBuf>,
    /// A scripted run: nothing may touch a human's keychain or disk.
    pub scripted: bool,
    /// The shared in-memory store, which is what a scripted run and every
    /// test keep their passwords in.
    pub secrets: MemSecrets,
    /// The machine's own store, when the shell installed one. Every world
    /// built from this env takes it — the window's and each worker's alike —
    /// so a password the settings form wrote is the one a sync pass reads.
    pub secrets_backend: Option<SecretsFactory>,
    pub clock: ClockSource,
    pub demo_disk: bool,
}

impl Default for Env {
    /// What a test gets: nothing on disk, a clock that only moves when it
    /// is moved, and the demo tree.
    fn default() -> Env {
        Env {
            db_dir: None,
            scripted: true,
            secrets: MemSecrets::new(),
            secrets_backend: None,
            clock: ClockSource::default(),
            demo_disk: true,
        }
    }
}

/// One world's backends, each under the trait it implements.
#[derive(Default)]
pub struct Capabilities {
    map: HashMap<TypeId, Box<dyn Any>>,
}

impl Capabilities {
    #[must_use]
    pub fn new() -> Capabilities {
        Capabilities::default()
    }

    /// A backend under the trait it implements. A fake registers itself
    /// under both its traits and its concrete type, so a test can reach
    /// `get::<mail::FakeServers>()` to plant a mail.
    pub fn insert<C: ?Sized + 'static>(&mut self, imp: Box<C>) {
        self.map.insert(TypeId::of::<C>(), Box::new(imp));
    }

    pub fn get<C: ?Sized + 'static>(&mut self) -> Option<&mut C> {
        self.map
            .get_mut(&TypeId::of::<C>())?
            .downcast_mut::<Box<C>>()
            .map(|b| &mut **b)
    }

    /// Takes one out — what a test does to prove an effect fails without
    /// it.
    pub fn remove<C: ?Sized + 'static>(&mut self) -> bool {
        self.map.remove(&TypeId::of::<C>()).is_some()
    }

    #[must_use]
    pub fn has<C: ?Sized + 'static>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<C>())
    }
}

impl std::fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capabilities")
            .field("n", &self.map.len())
            .finish()
    }
}

// -- schema --------------------------------------------------------------------

/// One app's migration ladder. `meta['schema:<app>']` records how many
/// steps have run. An app never alters another app's tables; new apps
/// prefix their table names with their id.
pub struct Schema {
    pub app: &'static str,
    pub steps: &'static [Step],
}

pub enum Step {
    /// Applied once, in order.
    Sql(&'static str),
    /// Applied once, in order.
    Run(fn(&Connection) -> rusqlite::Result<()>),
    /// Data rebuilt from other rows (a search index, a narrowing, derived
    /// rows): runs whenever `meta[key]` is not `version`, then sets it.
    Derived {
        key: &'static str,
        version: i64,
        rebuild: fn(&Connection) -> rusqlite::Result<()>,
    },
}

impl Schema {
    /// Where this ladder's progress is written down.
    #[must_use]
    pub fn key(&self) -> String {
        format!("schema:{}", self.app)
    }

    /// How many steps this store has run.
    ///
    /// # Errors
    ///
    /// If the read fails for a reason other than there being no row.
    pub fn progress(&self, conn: &Connection) -> rusqlite::Result<i64> {
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [self.key()], |r| {
                r.get(0)
            })
            .unwrap_or(0))
    }

    /// Climbs the ladder: every step not yet run, in order, plus every
    /// [`Step::Derived`] whose version has moved. Runs at every store open,
    /// after the kernel's own.
    ///
    /// # Errors
    ///
    /// If a step fails; the ones before it stay applied and recorded, so
    /// the next open resumes where this one stopped.
    pub fn apply(&self, conn: &Connection) -> rusqlite::Result<()> {
        let done = self.progress(conn)?;
        for (i, step) in self.steps.iter().enumerate() {
            let n = i as i64 + 1;
            match step {
                Step::Sql(sql) => {
                    if n > done {
                        conn.execute_batch(sql)?;
                    }
                }
                Step::Run(f) => {
                    if n > done {
                        f(conn)?;
                    }
                }
                Step::Derived {
                    key,
                    version,
                    rebuild,
                } => {
                    // Derived data is versioned by the walk that made it,
                    // not by the ladder's counter: a better walk rebuilds
                    // every store on its next open, however old.
                    let at: i64 = conn
                        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                        .unwrap_or(0);
                    if at != *version {
                        rebuild(conn)?;
                        conn.execute(
                            "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, ?2)",
                            rusqlite::params![key, version],
                        )?;
                    }
                }
            }
            if n > done {
                conn.execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, ?2)",
                    rusqlite::params![self.key(), n],
                )?;
            }
        }
        Ok(())
    }
}

// -- workers -------------------------------------------------------------------

/// One background pass with its own thread and its own world (its own store
/// reader, its own real capabilities).
pub trait Worker: Send + 'static {
    /// Unique among running workers (`sync-2`, `sender`); how the kernel
    /// diffs the set after each action.
    fn name(&self) -> String;

    /// The kick address, in the `action.entity` vocabulary: `kick(entity)`
    /// wakes only this one, `kick_all` wakes everyone.
    fn entity(&self) -> Option<String> {
        None
    }

    /// Which queued jobs this thread may run. A job may need something only
    /// one thread holds, such as a live session; the worker holding it
    /// claims the job and no other worker does, so it never burns an
    /// attempt on the wrong thread.
    fn claims(&self, job: &Job) -> bool;

    /// One pass. After it the kernel runs the queued jobs this worker
    /// `claims`, notifies the UI, and sleeps as [`Wake`] says or until
    /// kicked. Under virtual time the kernel runs every pass inline from
    /// the frame loop instead, then drains the queue until it stops moving.
    fn pass(&mut self, w: &World) -> Wake;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    After(Duration),
    OnKick,
}

/// One running thread's handle. Dropping the sender closes the channel,
/// which is how a worker that is no longer wanted retires.
struct Live {
    entity: Option<String>,
    kick: mpsc::Sender<()>,
}

/// How the passes run.
enum Mount {
    /// Production: a thread each, its own world, its own store reader.
    Threads {
        mode: Mode,
        env: Env,
        notify: Arc<dyn Fn() + Send + Sync>,
        live: HashMap<String, Live>,
    },
    /// Under virtual time: every pass runs from the caller's thread,
    /// against the session's own world, so a scripted tick is followed by
    /// its consequences in the same tick.
    Inline {
        world: Rc<World>,
        live: Vec<Box<dyn Worker>>,
    },
    /// Nothing runs: a library mount, or a test that only wants the layout.
    None,
}

/// How many rounds of draining an inline tick will do before giving up. A
/// job that files another due job would otherwise spin the frame loop; a
/// bound turns that into a visible backlog instead of a hang.
const INLINE_ROUNDS: usize = 64;

/// The set of background passes this build wants running, kept in step with
/// the store.
///
/// [`Workers::kick_all`] re-asks the apps and diffs the answer by
/// [`Worker::name`]: new names are spawned, missing names retire. The
/// session does this after every action, so a pass that a new row calls for
/// starts without a restart.
pub struct Workers {
    apps: &'static [&'static dyn App],
    /// The reader the apps are asked on — the session's own.
    store: Rc<Store>,
    mount: std::cell::RefCell<Mount>,
}

impl Workers {
    /// Production: a thread per worker, each with its own world over its
    /// own reader of the one database. `notify` wakes the UI thread once a
    /// pass has changed something.
    #[must_use]
    pub fn threads(
        apps: &'static [&'static dyn App],
        store: Rc<Store>,
        mode: Mode,
        env: Env,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Workers {
        Workers {
            apps,
            store,
            mount: std::cell::RefCell::new(Mount::Threads {
                mode,
                env,
                notify: Arc::new(notify),
                live: HashMap::new(),
            }),
        }
    }

    /// Under virtual time: the same passes, run from the caller's thread
    /// against the world it already has.
    #[must_use]
    pub fn inline(apps: &'static [&'static dyn App], world: Rc<World>) -> Workers {
        let store = world.store().clone();
        Workers {
            apps,
            store,
            mount: std::cell::RefCell::new(Mount::Inline {
                world,
                live: Vec::new(),
            }),
        }
    }

    /// A set that runs nothing — a library mount, or a test that only
    /// wants the layout.
    #[must_use]
    pub fn none(store: Rc<Store>) -> Workers {
        Workers {
            apps: &[],
            store,
            mount: std::cell::RefCell::new(Mount::None),
        }
    }

    /// Whether time only moves when it is moved, in which case a tick is
    /// the caller's to drive.
    #[must_use]
    pub fn is_inline(&self) -> bool {
        matches!(&*self.mount.borrow(), Mount::Inline { .. })
    }

    /// Whether anything is running at all.
    #[must_use]
    pub fn any(&self) -> bool {
        match &*self.mount.borrow() {
            Mount::Threads { live, .. } => !live.is_empty(),
            Mount::Inline { live, .. } => !live.is_empty(),
            Mount::None => false,
        }
    }

    /// Every running worker's name, sorted — what a test asserts the diff
    /// on.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = match &*self.mount.borrow() {
            Mount::Threads { live, .. } => live.keys().cloned().collect(),
            Mount::Inline { live, .. } => live.iter().map(|w| w.name()).collect(),
            Mount::None => Vec::new(),
        };
        v.sort();
        v
    }

    /// Wakes every worker and re-asks the apps for the set. The session
    /// does this itself after every action.
    pub fn kick_all(&self) {
        let want: Vec<Box<dyn Worker>> = self
            .apps
            .iter()
            .flat_map(|a| a.workers(&self.store))
            .collect();
        let names: HashSet<String> = want.iter().map(|w| w.name()).collect();
        let mut mount = self.mount.borrow_mut();
        match &mut *mount {
            Mount::Threads {
                mode,
                env,
                notify,
                live,
            } => {
                // A missing name retires: dropping its sender closes the
                // channel, and the thread returns on its next wake.
                live.retain(|name, _| names.contains(name));
                for w in want {
                    let name = w.name();
                    if live.contains_key(&name) {
                        continue;
                    }
                    let entity = w.entity();
                    let (kick, rx) = mpsc::channel::<()>();
                    let (apps, mode, env, notify) = (self.apps, *mode, env.clone(), notify.clone());
                    let db = self.store.db();
                    match std::thread::Builder::new()
                        .name(format!("worker-{name}"))
                        .spawn(move || worker_loop(apps, db, mode, &env, w, &rx, &*notify))
                    {
                        Ok(_) => {
                            live.insert(name, Live { entity, kick });
                        }
                        // A pass that could not be spawned is simply one
                        // that is not running; the rest still are.
                        Err(e) => eprintln!("workers: {name} did not start: {e}"),
                    }
                }
                for l in live.values() {
                    let _ = l.kick.send(());
                }
            }
            Mount::Inline { live, .. } => {
                live.retain(|w| names.contains(&w.name()));
                let have: HashSet<String> = live.iter().map(|w| w.name()).collect();
                for w in want {
                    if !have.contains(&w.name()) {
                        live.push(w);
                    }
                }
                drop(mount);
                self.tick();
            }
            Mount::None => {}
        }
    }

    /// Wakes one worker, by the address it answers to.
    pub fn kick(&self, entity: &str) {
        let mount = self.mount.borrow();
        match &*mount {
            Mount::Threads { live, .. } => {
                for l in live.values() {
                    if l.entity.as_deref() == Some(entity) {
                        let _ = l.kick.send(());
                    }
                }
            }
            Mount::Inline { .. } => {
                drop(mount);
                self.tick();
            }
            Mount::None => {}
        }
    }

    /// One inline round: every pass, then the queue drained until it stops
    /// moving. A no-op with threads, where each thread drives itself.
    /// Answers whether anything happened.
    pub fn tick(&self) -> bool {
        let mut mount = self.mount.borrow_mut();
        let Mount::Inline { world, live } = &mut *mount else {
            return false;
        };
        let mut moved = false;
        for w in live.iter_mut() {
            w.pass(world);
            moved |= world.run_effects_where(|j| w.claims(j)) > 0;
        }
        // Inline, one thread is every thread: whatever the passes filed is
        // drained here, and a job that filed another is drained with it.
        for _ in 0..INLINE_ROUNDS {
            if world.run_effects() == 0 {
                break;
            }
            moved = true;
        }
        moved
    }
}

impl std::fmt::Debug for Workers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workers")
            .field("inline", &self.is_inline())
            .field("names", &self.names())
            .finish()
    }
}

/// One worker's thread: its own reader, its own world, its own pass. It
/// exits when its handle drops — the kick channel closes — which is how a
/// retired worker stops without a shutdown protocol.
fn worker_loop(
    apps: &'static [&'static dyn App],
    db: Arc<Db>,
    mode: Mode,
    env: &Env,
    mut worker: Box<dyn Worker>,
    kicks: &mpsc::Receiver<()>,
    notify: &(dyn Fn() + Send + Sync),
) {
    let Ok(store) = Store::with_db(db) else {
        return;
    };
    let world = world_for(apps, store, mode, env);
    loop {
        let wake = worker.pass(&world);
        world.run_effects_where(|j| worker.claims(j));
        notify();
        let closed = match wake {
            Wake::After(d) => {
                matches!(
                    kicks.recv_timeout(d),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                )
            }
            Wake::OnKick => kicks.recv().is_err(),
        };
        if closed {
            return;
        }
        // A burst of kicks is one pass.
        while kicks.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{Ctx, Deferred, Effect};
    use crate::panel::{Opening, Panel, PanelId, Tag};
    use serde::{Deserialize, Serialize};

    // -- a tiny app, and a second one that clashes with it -------------------

    struct Beep;
    impl PanelKind for Beep {
        fn tag(&self) -> Tag {
            Tag("beep")
        }
        fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
            crate::panel::missing(id)
        }
    }
    static BEEP: Beep = Beep;
    static BEEP_KINDS: &[&dyn PanelKind] = &[&BEEP];

    struct One;
    impl App for One {
        fn id(&self) -> &'static str {
            "one"
        }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] {
            BEEP_KINDS
        }
        fn roots(&self) -> Vec<Root> {
            vec![Root::new(PanelId::bare(Tag("beep")), "beep", "noise")]
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    static ONE: One = One;

    struct Two;
    impl App for Two {
        fn id(&self) -> &'static str {
            "two"
        }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] {
            BEEP_KINDS
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    static TWO: Two = Two;

    static ONE_APP: &[&dyn App] = &[&ONE];
    static BOTH: &[&dyn App] = &[&ONE, &TWO];

    #[test]
    fn the_registry_answers_by_id_and_by_tag() {
        let apps = Apps::new(ONE_APP);
        assert!(apps.get("one").is_some());
        assert!(apps.get("files").is_none(), "not in this build");
        assert!(apps.get_as::<One>().is_some());
        assert!(apps.get_as::<Two>().is_none());
        assert!(apps.kind(Tag("beep")).is_some());
        assert!(apps.kind(Tag("nothing")).is_none());
        assert_eq!(apps.tags(), vec![Tag("beep")]);
        assert_eq!(apps.roots().len(), 1);
        assert_eq!(apps.roots()[0].label, "beep");
        assert!(apps.schemas().is_empty());
        // An app with no sources of its own still gets the kernel's one.
        assert_eq!(apps.problem_sources().len(), 1);
    }

    #[test]
    #[should_panic(expected = "two apps claim the tag beep: one and two")]
    fn two_apps_claiming_one_tag_stop_the_boot() {
        let _ = Apps::new(BOTH);
    }

    // -- the ladder ----------------------------------------------------------

    fn rebuilt(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('rebuilds', 1)
             ON CONFLICT(key) DO UPDATE SET value = value + 1",
            [],
        )
        .map(|_| ())
    }

    static LADDER: Schema = Schema {
        app: "one",
        steps: &[
            Step::Sql("CREATE TABLE one_thing(id INTEGER PRIMARY KEY, name TEXT)"),
            Step::Run(|c| {
                c.execute("INSERT INTO one_thing(name) VALUES('planted')", [])
                    .map(|_| ())
            }),
            Step::Derived {
                key: "one:derived",
                version: 1,
                rebuild: rebuilt,
            },
        ],
    };

    static LADDER_V2: Schema = Schema {
        app: "one",
        steps: &[
            Step::Sql("CREATE TABLE one_thing(id INTEGER PRIMARY KEY, name TEXT)"),
            Step::Run(|c| {
                c.execute("INSERT INTO one_thing(name) VALUES('planted')", [])
                    .map(|_| ())
            }),
            Step::Derived {
                key: "one:derived",
                version: 2,
                rebuild: rebuilt,
            },
        ],
    };

    fn meta(store: &Store, key: &str) -> i64 {
        store
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .unwrap_or(0)
    }

    /// The ladder runs once, records how far it got, and does not repeat
    /// itself on the next open.
    #[test]
    fn an_apps_ladder_records_its_progress() {
        let store = Store::open(None, &[&LADDER]).expect("store");
        assert_eq!(meta(&store, "schema:one"), 3);
        assert_eq!(meta(&store, "one:derived"), 1);
        assert_eq!(meta(&store, "rebuilds"), 1);
        let planted: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM one_thing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(planted, 1);

        // A second climb of the same ladder does nothing: the `Sql` step
        // would fail on a table that is already there, and the `Run` step
        // would plant a second row.
        store.write(|c| LADDER.apply(c)).expect("a second open");
        let planted: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM one_thing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(planted, 1, "the steps ran once");
        assert_eq!(meta(&store, "rebuilds"), 1, "and so did the rebuild");
    }

    /// A `Derived` step is versioned by the walk that made it, not by the
    /// counter: bumping the version rebuilds an already-climbed ladder.
    #[test]
    fn a_derived_step_reruns_when_its_version_moves() {
        let store = Store::open(None, &[&LADDER]).expect("store");
        assert_eq!(meta(&store, "rebuilds"), 1);
        assert_eq!(meta(&store, "one:derived"), 1);

        store.write(|c| LADDER_V2.apply(c)).expect("the new walk");
        assert_eq!(meta(&store, "rebuilds"), 2, "the derived data was redone");
        assert_eq!(meta(&store, "one:derived"), 2);
        assert_eq!(meta(&store, "schema:one"), 3, "and the counter stood still");

        // …and once redone, it stays redone.
        store.write(|c| LADDER_V2.apply(c)).unwrap();
        assert_eq!(meta(&store, "rebuilds"), 2);
    }

    // -- workers -------------------------------------------------------------

    /// A deferred effect a pass files, so a test can watch the drain.
    #[derive(Serialize, Deserialize)]
    struct Tick(i64);

    impl Effect for Tick {
        const KIND: &'static str = "tick";
        type Reply = ();
        fn describe(&self) -> String {
            format!("tick {}", self.0)
        }
        fn writes(&self) -> bool {
            true
        }
        fn perform(&self, _cx: &mut Ctx<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    impl Deferred for Tick {
        fn idempotent(&self) -> bool {
            true
        }
        fn settle(&self, tx: &rusqlite::Transaction, _r: &()) -> rusqlite::Result<()> {
            tx.execute(
                "INSERT INTO meta(key, value) VALUES('ticks', 1)
                 ON CONFLICT(key) DO UPDATE SET value = value + 1",
                [],
            )
            .map(|_| ())
        }
    }

    /// A pass that files one job the first time it runs.
    struct Once {
        name: &'static str,
        filed: bool,
    }

    impl Worker for Once {
        fn name(&self) -> String {
            self.name.to_string()
        }
        fn entity(&self) -> Option<String> {
            Some(format!("worker:{}", self.name))
        }
        fn claims(&self, job: &Job) -> bool {
            job.kind == "tick"
        }
        fn pass(&mut self, w: &World) -> Wake {
            if !self.filed {
                self.filed = true;
                let _ = w.enqueue(&Tick(1));
            }
            Wake::OnKick
        }
    }

    struct Passes;
    impl App for Passes {
        fn id(&self) -> &'static str {
            "passes"
        }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] {
            &[]
        }
        fn effects(&self, reg: &mut Registry) {
            reg.register::<Tick>();
        }
        fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
            // Derived from the store: the row says how many passes to run.
            let n: i64 = store
                .conn()
                .query_row("SELECT value FROM meta WHERE key = 'passes'", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0);
            let mut v: Vec<Box<dyn Worker>> = Vec::new();
            if n >= 1 {
                v.push(Box::new(Once {
                    name: "one",
                    filed: false,
                }));
            }
            if n >= 2 {
                v.push(Box::new(Once {
                    name: "two",
                    filed: false,
                }));
            }
            v
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    static PASSES: Passes = Passes;
    static PASSES_APPS: &[&dyn App] = &[&PASSES];

    fn set_passes(store: &Store, n: i64) {
        store
            .write(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES('passes', ?1)",
                    [n],
                )
                .map(|_| ())
            })
            .unwrap();
    }

    /// The inline mount runs a pass from the caller's thread and drains
    /// what it filed, in the same call.
    #[test]
    fn inline_workers_run_a_pass_and_drain_the_queue() {
        let apps = Apps::new(PASSES_APPS);
        let world = Rc::new(World::fake(apps.registry()));
        let store = world.store().clone();
        let workers = Workers::inline(PASSES_APPS, world.clone());
        assert!(workers.is_inline());
        assert!(!workers.any(), "nothing before the apps are asked");

        set_passes(&store, 1);
        workers.kick_all();
        assert_eq!(workers.names(), vec!["one".to_string()]);
        assert!(workers.any());
        assert_eq!(meta(&store, "ticks"), 1, "the pass filed and it drained");
        assert_eq!(world.jobs()[0].status, "done");

        // The set follows the store: a second row asks for a second pass.
        set_passes(&store, 2);
        workers.kick_all();
        assert_eq!(workers.names(), vec!["one".to_string(), "two".to_string()]);
        assert_eq!(meta(&store, "ticks"), 2, "the new pass filed too");

        // …and one that is no longer wanted retires.
        set_passes(&store, 1);
        workers.kick_all();
        assert_eq!(workers.names(), vec!["one".to_string()]);

        // A kick by address is a tick as well, and files nothing new: the
        // pass has already done its one job.
        workers.kick("worker:one");
        assert_eq!(meta(&store, "ticks"), 2);
    }

    /// A worker claims only its own jobs, so an inline pass never runs
    /// another's — though the drain that follows will, one thread being
    /// every thread.
    #[test]
    fn a_worker_claims_only_what_it_asks_for() {
        let w = Once {
            name: "one",
            filed: false,
        };
        let job = |kind: &str| Job {
            id: 1,
            kind: kind.into(),
            entity: None,
            status: "pending".into(),
            reply: None,
            error: None,
            attempts: 0,
            payload: "{}".into(),
            idempotent: true,
            created: 0.0,
            updated: 0.0,
            not_before: 0.0,
            what: None,
            writes: true,
        };
        assert!(w.claims(&job("tick")));
        assert!(!w.claims(&job("something else")));
        assert_eq!(w.entity().as_deref(), Some("worker:one"));
    }

    /// The bag holds a backend under the trait it implements, and hands it
    /// back.
    #[test]
    fn capabilities_are_kept_by_trait() {
        let mut c = Capabilities::new();
        assert!(!c.has::<dyn crate::caps::Clock>());
        c.insert::<dyn crate::caps::Clock>(Box::new(crate::caps::FakeClock::at(7.0)));
        assert!(c.has::<dyn crate::caps::Clock>());
        assert_eq!(
            c.get::<dyn crate::caps::Clock>().map(|k| k.now()),
            Some(7.0)
        );
        // …and under its concrete type too, for a test that wants the fake.
        c.insert::<crate::caps::FakeClipboard>(Box::new(crate::caps::FakeClipboard::new()));
        assert!(c.get::<crate::caps::FakeClipboard>().is_some());
        assert!(c.remove::<dyn crate::caps::Clock>());
        assert!(!c.remove::<dyn crate::caps::Clock>());
        assert!(c.get::<dyn crate::caps::Clock>().is_none());
        assert!(format!("{c:?}").contains("Capabilities"));
    }
}
