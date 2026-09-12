//! A local shell in a PTY, with Ghostty's VT state drawn by Makepad.

use kernel::app::{App, Capabilities, Env, Mode, Root};
use kernel::panel::{Opening, Panel, PanelId, PanelKind, PanelWidth, Tag, Verb};
use kernel::session::Session;
use std::any::Any;

mod engine;
#[cfg(all(test, headless))]
mod input_tests;
mod process;
mod search;
#[cfg(test)]
mod tests;
mod ui;
mod widget;

pub use ui::UI;
pub struct TerminalApp;
pub static TERMINAL: TerminalApp = TerminalApp;
const TAG: Tag = Tag("terminal");
struct TerminalMode(Mode);

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
        caps.insert(Box::new(TerminalMode(if env.scripted {
            Mode::Fake
        } else {
            mode
        })));
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
        Box::new(TerminalPanel {
            id: id.clone(),
            width: if id.arg(0) == Some("full") {
                PanelWidth::Full
            } else {
                PanelWidth::Half
            },
            mode,
            engine: None,
            error: None,
            find: None,
        })
    }
}

pub(super) struct TerminalPanel {
    id: PanelId,
    width: PanelWidth,
    mode: Mode,
    engine: Option<engine::Engine>,
    error: Option<String>,
    /// The find bar, while it is up.
    find: Option<Find>,
}

/// The find bar's state the widget reads: whether its field is to be given
/// the keyboard, and whether it has it — while it does, the grid neither
/// takes plain keys nor takes the keyboard back.
#[derive(Default)]
pub(super) struct Find {
    pub land: bool,
    pub typing: bool,
}

impl TerminalPanel {
    fn start(&mut self) {
        if self.engine.is_some() || self.error.is_some() {
            return;
        }
        match engine::Engine::new(self.mode) {
            Ok(engine) => self.engine = Some(engine),
            Err(error) => self.error = Some(format!("could not start terminal: {error}")),
        }
    }
}

impl Panel for TerminalPanel {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        let title = self
            .engine
            .as_ref()
            .map_or("shell", |engine| engine.title());
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
        let mut verbs = Vec::new();
        if self.engine.is_some() {
            verbs.push(Verb::run("terminal.find", "find", Some('f')));
        }
        if self.error.is_some() || self.engine.as_ref().is_some_and(|engine| engine.finished()) {
            verbs.push(Verb::run("terminal.restart", "restart shell", None));
        }
        verbs
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "terminal.restart" => {
                self.engine = None;
                self.error = None;
                self.find = None;
                s.redraw();
            }
            // Raise the bar, or put the caret back in it: the widget lands
            // the keyboard there on its next draw.
            "terminal.find" => {
                let find = self.find.get_or_insert_with(Find::default);
                find.land = true;
                find.typing = true;
                s.redraw();
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
