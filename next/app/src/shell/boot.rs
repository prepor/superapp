//! Argv, the world, and the session a stage comes up on.
//!
//! Everything a run is configured by arrives here: the store's path, the
//! script to replay, the forced grid, whether anything is rasterized at
//! all. An app's own knobs are environment variables it reads itself; argv
//! belongs to the shell.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use kernel::app::{Apps, Env, Mode, Workers};
use kernel::caps::{Clipboard, ClockSource, MemSecrets, Screen};
use kernel::e2e;
use kernel::layout::Grid;
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::virtual_epoch;
use makepad_widgets::*;

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
    /// `--demo-disk`: the file capability reads the demo tree. In the
    /// prototype it always does; the flag is kept so a suite written for
    /// the shipping tree still parses.
    pub demo_disk: bool,
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

/// The script to replay, if any, and where its pictures go. The canvas
/// reads it itself when the window opened on the library.
#[must_use]
pub fn e2e_script() -> (Option<&'static str>, &'static str) {
    let c = config();
    (c.e2e.as_deref(), &c.out)
}

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
                // The headless backend's own flags: read (and skipped)
                // here so they are not reported as unknown.
                "--no-draw" => c.no_draw = true,
                "--demo-disk" => c.demo_disk = true,
                "--draws" => {
                    args.next();
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
                other => eprintln!("superapp-next: ignoring unknown argument {other:?}"),
            }
        }
        c
    })
}

fn parse_wxh(s: &str) -> Option<(f64, f64)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
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
        Boot {
            db: db_path(),
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
    /// background passes it runs. The shell replaces two of the kernel's
    /// fakes with the real thing: only it knows what a frame is, so only it
    /// can photograph one, and a clipboard is the platform's.
    #[must_use]
    pub fn session(&self) -> (Session, ClockSource) {
        let clock = if self.virtual_time {
            ClockSource::virtual_from(virtual_epoch())
        } else {
            ClockSource::System
        };
        let env = Env {
            db_dir: self
                .db
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            scripted: self.steps.is_some(),
            secrets: MemSecrets::new(),
            clock: clock.clone(),
            demo_disk: true,
        };
        // A library mount: its own store, in memory, with the demo rows and
        // the outside its scene asked for. Nothing it does can reach the
        // window's world, and nothing it files outlives the frame.
        if self.mode != Mode::Real {
            return (Session::fake_mode(super::apps(), self.mode, &env), clock);
        }
        let apps = Apps::new(super::apps());
        let store = Store::open(self.db.as_deref(), &apps.schemas())
            .unwrap_or_else(|e| panic!("store: opening {:?} failed: {e}", self.db));
        // Demo rows go in once, on the first open of an empty store: a
        // store that has booted keeps whatever it was left as, empty or not.
        if matches!(store.load_wm(), Ok(None)) {
            if let Err(e) = apps.seed(&store) {
                eprintln!("store: seeding the demo world failed: {e}");
            }
        }
        let scripted = env.scripted;
        let world = Rc::new(apps.world(store, Mode::Real, &env));
        world.caps(|c| {
            c.insert::<dyn Screen>(Box::new(RealScreen));
            // A scripted run may not touch a human's clipboard; the
            // kernel's fake is already in place for it.
            if !scripted {
                c.insert::<dyn Clipboard>(Box::new(RealClipboard));
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
        (Session::new(apps, world, workers), clock)
    }
}

/// Where the store lives: `--db` wins; an e2e run gets a fresh temp file
/// (deleted first, so every run seeds the same demo world); otherwise the
/// platform data dir. `None` (no resolvable home) falls back to in memory.
fn db_path() -> Option<PathBuf> {
    let c = config();
    if let Some(p) = &c.db {
        return Some(PathBuf::from(p));
    }
    if c.e2e.is_some() {
        let p = std::env::temp_dir().join(format!("superapp-next-e2e-{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let name = format!("{}{suffix}", p.file_name()?.to_string_lossy());
            let _ = std::fs::remove_file(p.with_file_name(name));
        }
        return Some(p);
    }
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("Library/Application Support/superapp-next");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("superapp-next.db"))
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
pub struct RealScreen;

impl Screen for RealScreen {
    fn shot(&mut self, path: &Path) -> Result<(), String> {
        #[cfg(headless)]
        {
            headless_shot(path)
        }
        #[cfg(not(headless))]
        {
            let _ = path;
            Err("the prototype photographs a headless build only".into())
        }
    }
}

/// The newest frame the headless rasterizer wrote, copied to `path`.
#[cfg(headless)]
fn headless_shot(path: &Path) -> Result<(), String> {
    let dir = std::env::var("MAKEPAD_HEADLESS_OUT_DIR")
        .map_err(|_| "MAKEPAD_HEADLESS_OUT_DIR is not set".to_string())?;
    let newest = std::fs::read_dir(&dir)
        .map_err(|e| format!("{dir}: {e}"))?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("window_"))
        .max_by_key(std::fs::DirEntry::file_name)
        .ok_or_else(|| format!("no rendered frame in {dir}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::copy(newest.path(), path)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", path.display()))
}
