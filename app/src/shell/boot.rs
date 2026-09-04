//! Argv, the world, and the session a stage comes up on.
//!
//! Everything a run is configured by arrives here: the store's path, the
//! script to replay, the forced grid, whether anything is rasterized at
//! all. An app's own knobs are environment variables it reads itself; argv
//! belongs to the shell.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use kernel::app::{Apps, Env, Kicks, Mode, Workers};
use kernel::caps::{Clipboard, ClockSource, Disk, MemSecrets, Screen, SecretsFactory};
use kernel::e2e;
use kernel::layout::Grid;
use kernel::repl::r2;
use kernel::session::{ReplMount, Session};
use kernel::store::Store;
use kernel::time::virtual_epoch;
use makepad_widgets::*;

use crate::platform::disk::RealDisk;
use crate::platform::secret::Keychain;

/// On virtual time — which is every headless build — one draw cycle is one
/// frame of exactly this long, for both the springs and the e2e runner.
/// Nothing reads the wall clock, so a run is reproducible.
pub const FRAME_MS: f64 = 1000.0 / 60.0;

/// Command-line configuration, read once.
#[derive(Debug, Default)]
pub struct Config {
    pub e2e: Option<String>,
    pub out: String,
    pub db: Option<String>,
    pub grid: Option<Grid>,
    pub window: Option<(f64, f64)>,
    /// `--no-draw` runs the widget pass without rasterizing. This tells
    /// `shot` there is nothing to save.
    pub no_draw: bool,
    /// `--demo-disk`: the file capability reads the kernel's demo tree
    /// rather than this machine's own. A files suite says it, and it is the
    /// only way a scripted run may write to a disk at all.
    pub demo_disk: bool,
    /// `--bucket URL`: where device sync's lease and log live.
    pub bucket: Option<String>,
    /// `--front`: a scripted run may take the screen. Off by default, so a
    /// suite stays behind whatever window the person is working in.
    pub front: bool,
    /// Open the panels library instead of the workspace (`--library
    /// [NAME...]`): the catalogue's scenes whose names contain one of
    /// these, or every scene when none is given.
    pub library: Option<Vec<String>>,
}

/// The scene names `--library` asked for (none: every scene), when it did.
#[must_use]
pub fn library_filter() -> Option<&'static [String]> {
    config().library.as_deref()
}

/// `SUPERAPP_FRAME_LOG=1`: every frame's draw cost, and every event that took
/// over a millisecond, on stderr — for finding out where a window spends its
/// time. Read once: it is on the paint path.
#[must_use]
pub fn frame_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SUPERAPP_FRAME_LOG").is_some())
}

/// The script to replay, if any, and where its pictures go. The canvas
/// reads it itself when the window opened on the library.
#[must_use]
pub fn e2e_script() -> (Option<&'static str>, &'static str) {
    let c = config();
    (c.e2e.as_deref(), &c.out)
}

/// What `--help` prints. Argv is the shell's; an app's own knobs are
/// environment variables it reads itself, so they are not here.
const USAGE: &str = "\
superapp — specialized panels on one scrolling workspace.

  cargo run -p superapp [-- FLAG...]

  --db PATH           the store to open (default: the app support directory)
  --bucket URL        point this device at a device-sync bucket
  --r2-login          read a bucket's secret key from stdin, file it, exit
  --library [NAME...] open the panels library instead of a workspace,
                      narrowed to the scenes whose names match
  --grid WxH          the unit grid a workspace is cut into
  --window WxH        the window size

Replaying a script:

  --e2e FILE          the suite to replay
  --e2e-out DIR       where its pictures go (default: e2e/out)
  --no-draw           do the widget pass and rasterize nothing
  --demo-disk         read the demo tree rather than this machine's files
  --front             let the run take the screen
  --draws N           stop after N frames (a headless build only)
";

/// The configuration this process was started with.
pub fn config() -> &'static Config {
    static CONFIG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    CONFIG.get_or_init(|| {
        let mut c = Config {
            out: "e2e/out".into(),
            ..Default::default()
        };
        let mut args = std::env::args().skip(1).peekable();
        while let Some(a) = args.next() {
            match a.as_str() {
                "--library" => {
                    let mut names = Vec::new();
                    while let Some(n) = args.next_if(|n| !n.starts_with("--")) {
                        names.push(n);
                    }
                    c.library = Some(names);
                }
                "--no-draw" => c.no_draw = true,
                "--demo-disk" => c.demo_disk = true,
                "--front" => c.front = true,
                "--bucket" => c.bucket = args.next(),
                // Handled before the window exists; named here so it is not
                // reported as unknown.
                "--r2-login" => {}
                // The headless backend's own: makepad reads it, and it is
                // named here so it is not reported as unknown.
                "--draws" => {
                    args.next();
                }
                "--help" | "-h" => {
                    print!("{USAGE}");
                    std::process::exit(0);
                }
                "--e2e" => c.e2e = args.next(),
                "--e2e-out" => {
                    if let Some(o) = args.next() {
                        c.out = o;
                    }
                }
                "--db" => c.db = args.next(),
                "--grid" => {
                    c.grid = args.next().and_then(|s| {
                        parse_wxh(&s).map(|(w, h)| Grid {
                            w: w as u32,
                            h: h as u32,
                        })
                    });
                }
                "--window" => c.window = args.next().and_then(|s| parse_wxh(&s)),
                other => eprintln!("superapp: ignoring unknown argument {other:?} — see --help"),
            }
        }
        c
    })
}

fn parse_wxh(s: &str) -> Option<(f64, f64)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Whether a scripted run stays behind every normal window. It must not take
/// the screen from whoever is using the Mac — unless `--front` asks. Only a
/// windowed build has a window to put anywhere.
#[cfg(all(target_os = "macos", not(headless)))]
#[must_use]
pub fn background_run() -> bool {
    let c = config();
    c.e2e.is_some() && !c.front
}

/// Where `--r2-login`'s file fallback writes: the directory beside the
/// store, when this run names one. The keychain needs none.
#[must_use]
pub fn login_dir() -> Option<PathBuf> {
    db_path()
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Where device sync's bucket is, from the three sources that let each
/// platform configure it: the `--bucket` flag (desktop), the
/// `SUPERAPP_BUCKET` environment variable, and a `bucket` file beside the
/// store — how a device with no shell and no cable is pointed at one. The
/// file's first line is the URL; for a real endpoint the next two are the
/// keys, read by [`r2`].
#[must_use]
pub fn resolve_bucket(db: Option<&Path>) -> Option<String> {
    if let Some(u) = config().bucket.clone() {
        return Some(u);
    }
    if let Ok(u) = std::env::var("SUPERAPP_BUCKET") {
        let u = u.trim().to_string();
        if !u.is_empty() {
            return Some(u);
        }
    }
    r2::url_from_file(db.and_then(Path::parent))
}

/// Everything a stage needs to come up.
pub struct Boot {
    /// The store's path; `None` is in memory.
    pub db: Option<PathBuf>,
    /// A forced unit grid (`--grid`).
    pub grid: Option<Grid>,
    /// Run on the fixed frame clock — exactly under a headless build.
    pub virtual_time: bool,
    /// A script to replay, and where its screenshots go. The window's own
    /// stage runs it as a suite; a mount replays it up to its last step and
    /// stays there.
    pub steps: Option<Vec<e2e::Step>>,
    pub out: PathBuf,
    pub no_draw: bool,
    /// Which outside this world gets. The window's own stage takes
    /// [`Mode::Real`]; a library mount takes `Deny` or `Fake` and comes up
    /// on a store of its own, in memory, with the demo rows.
    pub mode: Mode,
    /// The window's own stage: it owns the keyboard, the store poll and the
    /// window. A mount owns nothing outside its pass.
    pub primary: bool,
    /// A prefix for the script's messages — a mount's scene and node.
    pub tag: String,
    /// Come up on this one panel, fresh, in place of the session.
    /// Otherwise the workspace is the restored session, or the first root.
    pub open: Option<Opener>,
    /// With `open`: draw that panel alone at the whole viewport, chrome
    /// included — a panel node of the library. Without: the panel is the
    /// workspace's first column and the stage draws the whole strip, so
    /// what it opens beside itself shows.
    pub solo: bool,
    /// Device sync's bucket, when one is configured. Only the window's own
    /// stage replicates: a library mount's world is its own.
    pub bucket: Option<String>,
}

/// What a solo stage opens on: the identity, resolved against the seeded
/// store (a mail by its subject, a job by its status).
pub type Opener = Box<dyn FnOnce(&Store) -> kernel::panel::PanelId>;

impl Boot {
    /// The stage's boot, from argv. A script that fails to parse ends the
    /// process here, before a window exists to be confused by it.
    #[must_use]
    pub fn from_argv() -> Boot {
        let c = config();
        // Opened on the library, the script is the canvas's, not this
        // stage's.
        let script = c.e2e.as_ref().filter(|_| c.library.is_none());
        let steps = script.map(|path| {
            match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|s| e2e::parse(&s))
            {
                Ok(steps) => {
                    eprintln!("e2e: {} step(s) from {path}", steps.len());
                    steps
                }
                Err(e) => {
                    eprintln!("e2e: {path}: {e}");
                    std::process::exit(2);
                }
            }
        });
        let db = db_path();
        Boot {
            bucket: resolve_bucket(db.as_deref()),
            db,
            grid: c.grid,
            virtual_time: cfg!(headless),
            steps,
            out: PathBuf::from(&c.out),
            no_draw: c.no_draw,
            mode: Mode::Real,
            primary: true,
            tag: String::new(),
            open: None,
            solo: false,
        }
    }

    /// The world and the session this boot describes.
    ///
    /// The apps supply the schema ladders the store climbs, the demo rows a
    /// fresh store is seeded with, the capabilities the world gets, and the
    /// background passes it runs. The shell replaces four of the kernel's
    /// fakes with the real thing: only it knows what a frame is, so only it
    /// can photograph one; a clipboard is the platform's; and the disk and
    /// the secret store are `platform/`'s, unless a script asked otherwise.
    #[must_use]
    pub fn session(&self) -> (Session, ClockSource) {
        let clock = if self.virtual_time {
            ClockSource::virtual_from(virtual_epoch())
        } else {
            ClockSource::System
        };
        let c = config();
        let db_dir = self
            .db
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let scripted = self.steps.is_some();
        // The machine's own secret store, for a real run that nobody is
        // scripting. It goes on the env rather than on this world alone,
        // because a worker builds a world of its own on its own thread: the
        // password a settings form writes has to be the one a sync pass
        // reads, and a private map per thread would never be.
        let keychain = (self.mode == Mode::Real && !scripted).then(|| {
            let dir = db_dir.clone();
            SecretsFactory::new(move || Box::new(Keychain::new(dir.clone())))
        });
        let env = Env {
            db_dir: db_dir.clone(),
            scripted,
            secrets: MemSecrets::new(),
            secrets_backend: keychain,
            clock: clock.clone(),
            demo_disk: c.demo_disk,
            // Filled in by the mount that runs the passes: only a threaded
            // one has channels to wake anybody through.
            kicks: Kicks::default(),
        };
        // A library mount: its own store, in memory, with the demo rows and
        // the outside its scene asked for. Nothing it does can reach the
        // window's world, and nothing it files outlives the frame.
        if self.mode != Mode::Real {
            return (Session::fake_mode(super::apps(), self.mode, &env), clock);
        }
        let apps = Apps::new(super::apps());
        // A store another build wrote is not a crash. There is no
        // migration, so say which file, which two schemas, and the two ways
        // past it, and leave — before a window exists to put a backtrace in.
        let store = Store::open(self.db.as_deref(), &apps.schemas()).unwrap_or_else(|e| {
            if let Some(was) = kernel::store::refused_schema(&e) {
                eprintln!("{}", foreign_store(self.db.as_deref(), was));
                std::process::exit(2);
            }
            panic!("store: opening {:?} failed: {e}", self.db)
        });
        // Which outside the demo rows are written for. A scripted run's
        // worlds reach the fakes however real the window around them is, so
        // its store is seeded the fake world's way and a suite has a demo
        // server to sync against; a run a person is looking at is seeded for
        // the outside, where no demo server exists.
        let seed_mode = if scripted || self.virtual_time {
            Mode::Fake
        } else {
            Mode::Real
        };
        // Demo rows go in once, on the first open of an empty store: a
        // store that has booted keeps whatever it was left as, empty or not.
        //
        // Under replication nothing is written until the first pass has
        // resolved this device's role: a would-be follower must not seed a
        // world it is about to replace with the holder's snapshot. The
        // session seeds when it first holds instead.
        if self.bucket.is_none() && matches!(store.load_wm(), Ok(None)) {
            if let Err(e) = apps.seed(&store, seed_mode) {
                eprintln!("store: seeding the demo world failed: {e}");
            }
        }
        let world = Rc::new(apps.world(store, Mode::Real, &env));
        world.caps(|caps| {
            caps.insert::<dyn Screen>(Box::new(RealScreen));
            // A scripted run may not touch a human's clipboard; the kernel's
            // fake is already in place for it. The keychain arrived with the
            // env, which is where every world of this run reads it from.
            if !scripted {
                caps.insert::<dyn Clipboard>(Box::new(RealClipboard));
            }
            // The disk is the machine's own unless the run asked for the
            // demo tree. A script against a real disk may read it and not
            // write to it: a suite must no more delete a human's files than
            // write to their keychain, and the refusal lands on the panel's
            // line where a forgotten `--demo-disk` is a failing step.
            if !c.demo_disk {
                caps.insert::<dyn Disk>(Box::new(if scripted {
                    RealDisk::read_only()
                } else {
                    RealDisk::new()
                }));
            }
        });
        let workers = if self.virtual_time {
            // Under virtual time the passes run inline from the frame loop,
            // so a scripted `wait` advances them the way it advances the
            // springs — the last thing between a run and reproducibility.
            Workers::inline(super::apps(), world.clone())
        } else {
            Workers::threads(
                super::apps(),
                world.store().clone(),
                Mode::Real,
                env,
                || {
                    SignalToUI::set_ui_signal();
                },
            )
        };
        let mut session = Session::new(apps, world, workers, seed_mode);
        // Device sync, when a bucket is configured. Only the window's own
        // stage replicates: a mount's world is its own, and two drivers over
        // one store is exactly what the lease forbids between machines.
        if self.primary {
            let mount = if self.virtual_time {
                ReplMount::Inline
            } else {
                ReplMount::Threads
            };
            session.mount_repl(mount, SignalToUI::set_ui_signal);
            if let Some(url) = &self.bucket {
                session.start_repl(url);
            }
        }
        (session, clock)
    }
}

/// What the shell says when the store it was pointed at belongs to another
/// build: the file, the schema it is at, the schema this build reads, and
/// the two ways past — another file, or this one moved aside. Nothing here
/// touches the file: other checkouts still open it.
#[must_use]
fn foreign_store(db: Option<&Path>, was: i64) -> String {
    let file = db.map_or_else(|| "the store".to_string(), |p| p.display().to_string());
    format!(
        "superapp: {file} is schema {was}, and this build reads {} — there is no migration.\n\
         superapp: open another file with --db PATH, or move this one aside by hand.",
        kernel::store::KERNEL_VERSION
    )
}

/// Where the store lives: `--db` wins; an e2e run gets a fresh directory of
/// its own; otherwise the platform data dir. `None` (no resolvable home)
/// falls back to in memory.
///
/// A scripted run gets a **directory**, not a bare file, because what sits
/// *beside* a store is part of it: the `bucket` file the device-sync form
/// writes is read by the next launch, and one suite must not configure the
/// next. Named by the pid and swept before it is used, so parallel suites
/// share nothing and every run seeds the same demo world.
/// Resolved once: it makes (and, for a scripted run, sweeps) a directory,
/// and asking twice must not sweep the store the first answer opened.
fn db_path() -> Option<PathBuf> {
    static DB: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DB.get_or_init(resolve_db).clone()
}

fn resolve_db() -> Option<PathBuf> {
    let c = config();
    if let Some(p) = &c.db {
        let p = PathBuf::from(p);
        if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        return Some(p);
    }
    if c.e2e.is_some() {
        let dir = std::env::temp_dir().join(format!("superapp-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        return Some(dir.join("store.db"));
    }
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("Library/Application Support/superapp");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("superapp.db"))
}

/// The shell's own [`Clipboard`]: the platform's, not the kernel's fake.
pub struct RealClipboard;

impl Clipboard for RealClipboard {
    fn put(&mut self, text: &str) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            use std::io::Write;
            let mut child = std::process::Command::new("/usr/bin/pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("pbcopy: {e}"))?;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin
                    .write_all(text.as_bytes())
                    .map_err(|e| format!("pbcopy: {e}"))?;
            }
            let _ = child.wait();
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = text;
            Err("no clipboard on this platform".into())
        }
    }
}

/// The shell's own [`Screen`]: only it knows what a frame is.
///
/// Under a headless build there is no window to photograph — makepad
/// renders the frames itself, so a "screenshot" is picking the right one.
/// A windowed macOS run has a window, and `platform/` photographs its own
/// layer.
pub struct RealScreen;

impl Screen for RealScreen {
    fn shot(&mut self, path: &Path) -> Result<(), String> {
        #[cfg(headless)]
        {
            headless_shot(path)
        }
        #[cfg(all(not(headless), target_os = "macos"))]
        {
            crate::platform::mac::screenshot(path)
        }
        #[cfg(all(not(headless), not(target_os = "macos")))]
        {
            let _ = path;
            Err("no window capture on this platform".into())
        }
    }
}

/// The newest frame the headless rasterizer wrote, copied to `path`.
///
/// Which frame that is, is the harness's business: a `shot` asks for a draw
/// and waits until the rasterizer has written it ([`frame_mark`],
/// [`frame_after`]), so by the time this runs the newest file *is* the
/// frame the step drew.
#[cfg(headless)]
fn headless_shot(path: &Path) -> Result<(), String> {
    let dir = frame_dir().ok_or_else(|| "MAKEPAD_HEADLESS_OUT_DIR is not set".to_string())?;
    let (_, newest) =
        newest_frame(&dir).ok_or_else(|| format!("no rendered frame in {}", dir.display()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::copy(newest, path)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", path.display()))
}

// -- the rasterizer's own frame counter ----------------------------------------
//
// makepad's headless backend writes `window_<id>_frame_<n>.png` into
// `MAKEPAD_HEADLESS_OUT_DIR` at the end of every draw cycle, `n` counting up
// per window. The harness reads the highest `n` before it asks for a draw and
// waits for a higher one: *that* file is the frame its own step drew.
//
// It matters because the loop is `next-frame event, then draw`. A `shot` runs
// from the frame event, so the newest frame at that instant is the one before
// its step — which is why two shots with a click between them used to be the
// same picture.

/// Where the rasterizer writes, when this build rasterizes at all.
#[cfg(headless)]
fn frame_dir() -> Option<PathBuf> {
    std::env::var_os("MAKEPAD_HEADLESS_OUT_DIR").map(PathBuf::from)
}

/// The highest frame counter in `dir`, and the file that carries it.
#[cfg(headless)]
fn newest_frame(dir: &Path) -> Option<(u64, PathBuf)> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let n = name.strip_prefix("window_")?.rsplit_once("_frame_")?.1;
            let n = n.strip_suffix(".png")?.parse::<u64>().ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
}

/// Where the rasterizer is now: the frame it has most recently written.
/// `None` when there is no counter to read — a build with no headless
/// backend, or one with nowhere to write — and then a shot waits for
/// nothing.
#[must_use]
pub fn frame_mark() -> Option<u64> {
    #[cfg(headless)]
    {
        let dir = frame_dir()?;
        Some(newest_frame(&dir).map_or(0, |(n, _)| n))
    }
    #[cfg(not(headless))]
    {
        None
    }
}

/// What the rasterizer has done since `mark` — what a pending `shot` asks
/// every frame until it may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// There is no counter to wait on: shoot now.
    Uncounted,
    /// The frame this shot asked for has not been written yet.
    Pending,
    /// Written, and nothing on it: the software rasterizer draws with
    /// shaders it compiles by shelling out to `rustc`, and until they are
    /// loaded every pass paints one flat colour. Worth another frame.
    Blank(u64),
    /// Written, with a picture on it.
    Ready(u64),
}

/// Whether the rasterizer has written a frame past `mark`, and whether that
/// frame has anything on it.
#[must_use]
pub fn frame_after(mark: Option<u64>) -> Frame {
    let Some(mark) = mark else {
        return Frame::Uncounted;
    };
    #[cfg(headless)]
    {
        let Some(dir) = frame_dir() else {
            return Frame::Uncounted;
        };
        match newest_frame(&dir) {
            Some((n, path)) if n > mark => {
                if is_blank(&path) {
                    Frame::Blank(n)
                } else {
                    Frame::Ready(n)
                }
            }
            _ => Frame::Pending,
        }
    }
    #[cfg(not(headless))]
    {
        let _ = mark;
        Frame::Uncounted
    }
}

/// Whether a written frame has anything on it, read off the file rather than
/// out of the pixels.
///
/// A frame drawn before its shaders are loaded is one flat colour, and PNG
/// says so in its size: a uniform image deflates to almost nothing, while a
/// frame with a word of text on it does not. Measured on this tree's own
/// frames at 2880×1800, a busy one runs about 23 pixels to the byte and the
/// sparsest picture in any suite — an empty workspace, one line of grey in
/// the middle of it — about 150. The bound below is nearly an order of
/// magnitude past that, so it answers the question it is asked — *is there
/// anything here at all* — and never the one it is not.
///
/// Being wrong the safe way costs a few frames: the world stands still while
/// a shot waits, so a frame wrongly called blank is redrawn as the same
/// picture, and [`SHOT_PATIENCE`](super::e2e) bounds how many of those there
/// can be.
#[cfg(headless)]
fn is_blank(path: &Path) -> bool {
    /// The most pixels one byte of a drawn frame may account for.
    const PIXELS_PER_BYTE: u64 = 1000;
    let Ok(bytes) = std::fs::metadata(path).map(|m| m.len()) else {
        return true; // gone between the listing and the look: not an answer
    };
    match png_size(path) {
        // No IHDR to read: a half-written file, which the next frame
        // replaces.
        None => true,
        Some((w, h)) => bytes.saturating_mul(PIXELS_PER_BYTE) < u64::from(w) * u64::from(h),
    }
}

/// A PNG's dimensions, off its first chunk: the eight-byte signature, then
/// IHDR's length and tag, then width and height, big-endian.
#[cfg(headless)]
fn png_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut head = [0u8; 24];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([head[16], head[17], head[18], head[19]]);
    let h = u32::from_be_bytes([head[20], head[21], head[22], head[23]]);
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store a previous design left behind: the kernel refuses it, boot
    /// reads its schema back out of the refusal, and what the person gets is
    /// two lines naming the file, both numbers and both ways past — not a
    /// backtrace. The exit itself is `std::process::exit(2)` at the call
    /// site; this is the message it goes out on.
    #[test]
    fn a_store_of_another_schema_is_spoken_not_panicked() {
        let dir = std::env::temp_dir().join(format!("superapp-boot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        let _ = std::fs::remove_file(&path);
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", 12).unwrap();
        }
        let e = Store::open(Some(&path), &[]).err().expect("refused");
        let was =
            kernel::store::refused_schema(&e).expect("boot knows this failure from any other");
        assert_eq!(was, 12);
        let said = foreign_store(Some(&path), was);
        assert_eq!(said.lines().count(), 2, "{said}");
        assert!(said.contains(&path.display().to_string()), "{said}");
        assert!(said.contains("schema 12"), "{said}");
        assert!(
            said.contains(&format!("reads {}", kernel::store::KERNEL_VERSION)),
            "{said}"
        );
        assert!(said.contains("--db PATH"), "{said}");
        assert!(said.contains("aside"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
