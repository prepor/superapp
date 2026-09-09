//! A local shell in a PTY, with Ghostty's VT state drawn by Makepad.

use kernel::app::{App, Capabilities, Env, Mode, Root};
use kernel::panel::{Opening, Panel, PanelId, PanelKind, PanelWidth, Tag, Verb};
use kernel::session::Session;
use std::any::Any;

mod engine;
mod process;
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
            "terminal",
            "shell command line console pty",
        )]
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
        })
    }
}

pub(super) struct TerminalPanel {
    id: PanelId,
    width: PanelWidth,
    mode: Mode,
    engine: Option<engine::Engine>,
    error: Option<String>,
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
        match self
            .engine
            .as_ref()
            .map(|engine| engine.title())
            .filter(|title| !title.is_empty())
        {
            Some(title) => format!("terminal — {title}"),
            None => "terminal".into(),
        }
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
        if self.error.is_some() || self.engine.as_ref().is_some_and(|engine| engine.finished()) {
            vec![Verb::run("terminal.restart", "restart shell", None)]
        } else {
            Vec::new()
        }
    }
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "terminal.restart" {
            self.engine = None;
            self.error = None;
            s.redraw();
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
