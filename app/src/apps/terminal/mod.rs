//! A local shell in a PTY, with Ghostty's VT state drawn by Makepad.

use kernel::app::{App, Capabilities, Env, Mode, Root};
use kernel::panel::{Opening, Panel, PanelId, PanelKind, PanelWidth, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;
use std::{
    any::Any,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
};

mod engine;
#[cfg(all(test, headless))]
mod input_tests;
mod process;
#[cfg(test)]
mod tests;
mod ui;
mod widget;

pub use engine::SessionHandle;
pub use ui::UI;
pub use widget::TerminalViewWidgetRefExt;
pub struct TerminalApp;
pub static TERMINAL: TerminalApp = TerminalApp;
const TAG: Tag = Tag("terminal");
struct TerminalMode(Mode);

/// Registry capability for other apps. A build without Terminal has no service.
#[derive(Clone, Copy)]
pub struct TerminalService {
    mode: Mode,
}
impl TerminalService {
    pub fn is_real(&self) -> bool {
        self.mode == Mode::Real
    }
    pub fn create(&self, store: &Store, cwd: &Path) -> Result<SessionHandle, String> {
        create_session(store, self.mode, cwd)
    }
    pub fn get(&self, store: &Store, id: &str) -> Option<SessionHandle> {
        get_session(store, id)
    }
    pub fn list(&self, store: &Store) -> Vec<SessionHandle> {
        store
            .local::<Sessions>()
            .entries
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }
    pub fn close(&self, store: &Store, id: &str) -> bool {
        close_session(store, id)
    }
    pub fn panel_id(&self, handle: &SessionHandle) -> PanelId {
        session_panel_id(handle)
    }
}

#[derive(Default)]
struct Sessions {
    next: AtomicU64,
    entries: Mutex<HashMap<String, SessionHandle>>,
}

pub fn create_session(store: &Store, mode: Mode, cwd: &Path) -> Result<SessionHandle, String> {
    if mode == Mode::Real && !cwd.is_dir() {
        return Err("The terminal directory is unavailable".into());
    }
    let registry = store.local::<Sessions>();
    let sequence = registry.next.fetch_add(1, Ordering::Relaxed);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!("{epoch:x}-{sequence:x}");
    let handle = SessionHandle::start(id.clone(), mode, cwd.to_owned());
    registry.entries.lock().unwrap().insert(id, handle.clone());
    Ok(handle)
}
pub fn get_session(store: &Store, id: &str) -> Option<SessionHandle> {
    store
        .local::<Sessions>()
        .entries
        .lock()
        .unwrap()
        .get(id)
        .cloned()
}
pub fn close_session(store: &Store, id: &str) -> bool {
    if let Some(h) = store.local::<Sessions>().entries.lock().unwrap().remove(id) {
        h.close();
        true
    } else {
        false
    }
}
pub fn session_panel_id(handle: &SessionHandle) -> PanelId {
    PanelId::new(
        TAG,
        [
            "session".to_owned(),
            handle.id.clone(),
            handle.cwd.to_string_lossy().into_owned(),
            "half".to_owned(),
        ],
    )
}

impl App for TerminalApp {
    fn id(&self) -> &'static str {
        "terminal"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        &[&Kind]
    }
    fn roots(&self) -> Vec<Root> {
        vec![Root::new(
            PanelId::bare(TAG),
            "new terminal",
            "shell command line console pty",
        )
        .fresh()]
    }
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        let mode = if env.scripted { Mode::Fake } else { mode };
        caps.insert(Box::new(TerminalMode(mode)));
        caps.insert(Box::new(TerminalService { mode }));
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Kind;
impl PanelKind for Kind {
    fn tag(&self) -> Tag {
        TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let mode = cx
            .session()
            .world()
            .caps(|caps| caps.get::<TerminalMode>().map_or(Mode::Deny, |mode| mode.0));
        let registry = cx.session().store().local::<Sessions>();
        let shared = if id.arg(0) == Some("session") {
            id.arg(1).map(|key| {
                let mut entries = registry.entries.lock().unwrap();
                entries
                    .entry(key.to_owned())
                    .or_insert_with(|| {
                        SessionHandle::start(
                            key.to_owned(),
                            mode,
                            PathBuf::from(id.arg(2).unwrap_or(".")),
                        )
                    })
                    .clone()
            })
        } else {
            None
        };
        Box::new(TerminalPanel {
            id: id.clone(),
            width: if id.arg(0) == Some("full") || id.arg(3) == Some("full") {
                PanelWidth::Full
            } else {
                PanelWidth::Half
            },
            mode,
            engine: None,
            shared,
            registry: Some(Arc::downgrade(&registry)),
            close_shared: true,
            error: None,
        })
    }
}

pub(super) struct TerminalPanel {
    id: PanelId,
    width: PanelWidth,
    mode: Mode,
    engine: Option<engine::Engine>,
    shared: Option<SessionHandle>,
    registry: Option<Weak<Sessions>>,
    close_shared: bool,
    error: Option<String>,
}

impl TerminalPanel {
    fn start(&mut self) {
        if self.engine.is_some() || self.shared.is_some() || self.error.is_some() {
            return;
        }
        match engine::Engine::new(self.mode) {
            Ok(engine) => self.engine = Some(engine),
            Err(error) => self.error = Some(format!("could not start terminal: {error}")),
        }
    }
    fn engine_mut(&mut self) -> Option<&mut dyn engine::SurfaceEngine> {
        if let Some(shared) = &mut self.shared {
            Some(shared)
        } else {
            self.engine
                .as_mut()
                .map(|e| e as &mut dyn engine::SurfaceEngine)
        }
    }
    fn engine_status(&self) -> Option<String> {
        self.shared
            .as_ref()
            .and_then(SessionHandle::status)
            .or_else(|| {
                self.engine
                    .as_ref()
                    .and_then(|e| e.status().map(str::to_owned))
            })
    }
    fn bound(shared: SessionHandle) -> Self {
        Self {
            id: session_panel_id(&shared),
            width: PanelWidth::Half,
            mode: Mode::Real,
            engine: None,
            shared: Some(shared),
            registry: None,
            close_shared: false,
            error: None,
        }
    }
}

impl Drop for TerminalPanel {
    fn drop(&mut self) {
        if self.close_shared {
            if let Some(shared) = &self.shared {
                shared.close();
                if let Some(registry) = self.registry.as_ref().and_then(Weak::upgrade) {
                    registry.entries.lock().unwrap().remove(&shared.id);
                }
            }
        }
    }
}

impl Panel for TerminalPanel {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        let title = self
            .shared
            .as_ref()
            .map(SessionHandle::title)
            .unwrap_or_else(|| {
                self.engine
                    .as_ref()
                    .map_or("shell", |engine| engine.title())
                    .to_owned()
            });
        format!("terminal: {title}")
    }
    fn about(&self) -> String {
        "A local interactive shell. Its process and output live only on this device, in this panel. Closing stops the shell; restoring starts a new one. The width control keeps the same running session.".into()
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (6, u32::MAX)
    }
    fn width(&self) -> Option<PanelWidth> {
        Some(self.width)
    }
    fn set_width(&mut self, width: PanelWidth) {
        self.width = width;
    }
    fn persist(&self) -> PanelId {
        if let Some(shared) = &self.shared {
            return PanelId::new(
                TAG,
                [
                    "session".to_owned(),
                    shared.id.clone(),
                    shared.cwd.to_string_lossy().into_owned(),
                    if self.width == PanelWidth::Full {
                        "full"
                    } else {
                        "half"
                    }
                    .to_owned(),
                ],
            );
        }
        PanelId::new(
            TAG,
            [if self.width == PanelWidth::Full {
                "full"
            } else {
                "half"
            }],
        )
    }
    fn verbs(&self) -> Vec<Verb> {
        if self.error.is_some()
            || self.shared.as_ref().is_some_and(SessionHandle::finished)
            || self.engine.as_ref().is_some_and(|engine| engine.finished())
        {
            vec![Verb::run("terminal.restart", "restart shell", None)]
        } else {
            Vec::new()
        }
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "terminal.restart" {
            if let Some(shared) = self.shared.take() {
                shared.close();
                let replacement = SessionHandle::start(shared.id.clone(), self.mode, shared.cwd);
                if let Some(registry) = self.registry.as_ref().and_then(Weak::upgrade) {
                    registry
                        .entries
                        .lock()
                        .unwrap()
                        .insert(replacement.id.clone(), replacement.clone());
                }
                self.shared = Some(replacement);
            }
            self.engine = None;
            self.error = None;
            s.redraw();
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
