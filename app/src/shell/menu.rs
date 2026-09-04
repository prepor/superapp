//! The macOS menu bar: the workspaces, the launcher, undo and redo, the
//! history, the panels library, and what stands wrong.
//!
//! The bar mirrors the workspaces — one menu per roster entry, the current
//! one bracketed — and, while anything stands, one more menu holding the
//! problems as items. Both come off the session as data: the roster from the
//! layout, the problems from every app's own sources, so nothing here names
//! an app or a kind.
//!
//! The items carry no key equivalents. The chords live in [`keys`](super::keys)
//! and the labels document them; the shifted-digit menu-key table is off by
//! one upstream, and a menu that answered a chord twice would run it twice.
//! The bar is rebuilt only when its signature changes, never per keystroke.

use kernel::layout::WS_N;
use kernel::nav::Nav;
use kernel::problems::Announced;
use kernel::session::Session;
use makepad_widgets::*;

use super::library::DevAction;
use super::overlays::Overlay;
use super::stage::{Shell, Stage};

/// Menu command id bases: workspace `k`'s items are `base + k`. Plain
/// numbers, not `live_id!` hashes — the ranges cannot collide with the one
/// hashed command makepad special-cases, `quit`.
const WS_MENU_SWITCH: u64 = 0x5753_0100;
const WS_MENU_MOVE: u64 = 0x5753_0200;
const MENU_LAUNCHER: u64 = 0x5753_0300;
const MENU_UNDO: u64 = 0x5753_0400;
const MENU_REDO: u64 = 0x5753_0401;
const MENU_HISTORY: u64 = 0x5753_0500;
const MENU_LIBRARY: u64 = 0x5753_0600;
/// The problems menu: every item goes to the panel that lists them.
const MENU_PROBLEMS: u64 = 0x5753_0700;

/// What the bar is a picture of: the roster with the current space marked,
/// and the problems as lines. Nothing else on it changes, so nothing else
/// makes it rebuild.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MenuSig {
    /// Every workspace the roster lists, and whether it is the current one.
    pub ws: Vec<(usize, bool)>,
    /// `label — line`, one per standing problem.
    pub problems: Vec<String>,
}

impl MenuSig {
    /// The bar as the session would have it now. The whole of what the menu
    /// knows, and the only part of it that is not AppKit's — which is what
    /// makes it testable without a window.
    #[must_use]
    pub fn of(session: &Session) -> MenuSig {
        let active = session.ws().active;
        MenuSig {
            ws: session
                .ws()
                .roster()
                .into_iter()
                .map(|k| (k, k == active))
                .collect(),
            problems: session
                .problems()
                .iter()
                .map(|p| format!("{} — {}", p.label, p.line))
                .collect(),
        }
    }
}

/// An item with no key equivalent, which is every item here.
fn item(command: u64, name: impl Into<String>, enabled: bool, shift: bool) -> MacosMenu {
    MacosMenu::Item {
        command: LiveId(command),
        key: KeyCode::Unknown,
        shift,
        enabled,
        name: name.into(),
    }
}

/// The app menu, which AppKit insists on: replacing the bar without it would
/// take Quit — and ⌘Q — with it.
fn app_menu(items: Vec<MacosMenu>) -> MacosMenu {
    let mut items = items;
    items.push(MacosMenu::Item {
        command: live_id!(quit),
        key: KeyCode::KeyQ,
        shift: false,
        enabled: true,
        name: "Quit superapp".into(),
    });
    MacosMenu::Sub {
        name: "superapp".into(),
        items,
    }
}

/// The Dev menu: the one item that is not a chord away from everywhere.
fn dev_sub() -> MacosMenu {
    MacosMenu::Sub {
        name: "Dev".into(),
        items: vec![item(MENU_LIBRARY, "Panels Library — ⇧⌘L", true, true)],
    }
}

/// The menu bar of a window opened on the library: the Dev menu alone, until
/// the workspace boots and the stage builds the full set. Without it the
/// toggle back would have no item to live in.
pub fn dev_menu(cx: &mut Cx) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.update_macos_menu(MacosMenu::Main {
        items: vec![app_menu(Vec::new()), dev_sub()],
    });
}

impl Stage {
    /// Rebuilds the bar when what it shows has changed. Called from the
    /// stage's settle, after any event that left something dirty — nothing
    /// on this bar can move without one.
    pub(super) fn update_menu(&mut self, cx: &mut Cx, sh: &Shell) {
        if !cfg!(target_os = "macos") || self.is_mount() {
            return;
        }
        let sig = MenuSig::of(&sh.session);
        if sig == self.menu_sig {
            return;
        }
        self.menu_sig.clone_from(&sig);
        let MenuSig {
            ws: roster,
            problems,
        } = sig;

        let mut items = vec![app_menu(vec![
            item(MENU_LAUNCHER, "Launcher — ⌘ ⌘", true, false),
            item(MENU_UNDO, "Undo — ⌘Z", true, false),
            item(MENU_REDO, "Redo — ⇧⌘Z", true, false),
            item(MENU_HISTORY, "History — ⌘U", true, false),
        ])];
        for (k, current) in roster {
            let name = if current {
                format!("[{}]", k + 1)
            } else {
                format!("{}", k + 1)
            };
            items.push(MacosMenu::Sub {
                name,
                items: vec![
                    item(
                        WS_MENU_SWITCH + k as u64,
                        format!("Switch Here — ⌘{}", k + 1),
                        !current,
                        false,
                    ),
                    item(
                        WS_MENU_MOVE + k as u64,
                        format!("Move Panel Here — ⇧⌘{}", k + 1),
                        !current,
                        true,
                    ),
                ],
            });
        }
        items.push(dev_sub());
        // The problems, mirrored the way the workspaces are: a menu that
        // exists only while something stands, one item per problem, each
        // opening the panel that lists them. Plain text — AppKit draws these
        // titles itself, so the one colour stays in the window.
        if !problems.is_empty() {
            items.push(MacosMenu::Sub {
                name: format!("! {}", Announced::count_line(problems.len())),
                items: problems
                    .into_iter()
                    .map(|line| item(MENU_PROBLEMS, line, true, false))
                    .collect(),
            });
        }
        cx.update_macos_menu(MacosMenu::Main { items });
    }

    /// A menu item was picked. Every one of them lands on the same code the
    /// chord does.
    pub(super) fn menu_command(&mut self, cx: &mut Cx, sh: &mut Shell, cmd: LiveId) {
        self.cmd_tap.other_input();
        let id = cmd.0;
        if (WS_MENU_SWITCH..WS_MENU_SWITCH + WS_N as u64).contains(&id) {
            self.switch_ws(sh, (id - WS_MENU_SWITCH) as usize);
        } else if (WS_MENU_MOVE..WS_MENU_MOVE + WS_N as u64).contains(&id) {
            self.move_to_ws(sh, (id - WS_MENU_MOVE) as usize);
        } else if id == MENU_LAUNCHER {
            self.open_launcher(cx, sh);
        } else if id == MENU_UNDO {
            self.do_undo(sh);
        } else if id == MENU_REDO {
            self.do_redo(sh);
        } else if id == MENU_HISTORY {
            sh.overlay = Overlay::History;
            sh.session.redraw();
        } else if id == MENU_PROBLEMS {
            self.go_to_problems(sh);
        } else if id == MENU_LIBRARY {
            cx.action(DevAction::ToggleLibrary);
        }
    }

    /// The panel that lists what stands wrong: focused wherever it already
    /// is — another workspace included — or opened beside what has focus.
    /// The same move the launcher makes for a root, and the one door both
    /// ways in use — this bar's items, and the mark in the chrome's corner.
    pub(super) fn go_to_problems(&mut self, sh: &mut Shell) {
        sh.overlay = Overlay::None;
        let id = super::problems_panel();
        match sh.session.showing(&id).first().copied() {
            Some(slot) => sh.session.nav(Nav::Focus(slot)),
            None => self.open_root(sh, id),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use kernel::app::{App, Problem, ProblemSource};
    use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag};
    use kernel::session::Action;
    use kernel::store::Store;

    use super::*;

    // -- a tiny app: one panel, and one thing that can be wrong -------------

    const NOTE: Tag = Tag("note");

    struct NotePanel(PanelId);
    impl Panel for NotePanel {
        fn id(&self) -> &PanelId {
            &self.0
        }
        fn title(&self) -> String {
            "note".into()
        }
        fn as_any(&mut self) -> &mut dyn Any {
            self
        }
    }

    struct NoteKind;
    impl PanelKind for NoteKind {
        fn tag(&self) -> Tag {
            NOTE
        }
        fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
            Box::new(NotePanel(id.clone()))
        }
    }

    /// One standing condition, derived the way a real source derives one.
    struct Wrong;
    impl ProblemSource for Wrong {
        fn list(&self, _store: &Store) -> Vec<Problem> {
            vec![Problem::new("note:1", "the note", "will not save", "")]
        }
    }

    static NOTE_KIND: NoteKind = NoteKind;
    static KINDS: &[&dyn PanelKind] = &[&NOTE_KIND];
    static WRONG: Wrong = Wrong;
    static SOURCES: &[&dyn ProblemSource] = &[&WRONG];

    struct Demo;
    impl App for Demo {
        fn id(&self) -> &'static str {
            "demo"
        }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] {
            KINDS
        }
        fn problems(&self) -> &'static [&'static dyn ProblemSource] {
            SOURCES
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    static DEMO: Demo = Demo;
    static APPS: &[&dyn App] = &[&DEMO];

    /// The bar is the roster and what stands wrong, and it reads both off the
    /// session: the occupied workspaces and the first empty one, the current
    /// one marked, and one line per problem — whoever the problem belongs to.
    #[test]
    fn the_bar_is_the_roster_and_what_stands_wrong() {
        let mut s = kernel::session::Session::fake(APPS);
        s.act(Action::new("open", "open").moving(|wm| {
            wm.open(PanelId::bare(NOTE), None, false);
        }));
        s.settle();

        let sig = MenuSig::of(&s);
        assert_eq!(
            sig.ws,
            vec![(0, true), (1, false)],
            "the one occupied space, and the first empty one after it"
        );
        assert_eq!(sig.problems, vec!["the note — will not save".to_string()]);

        // A second space with something on it joins the roster, and the
        // current one moves with the switch.
        s.switch(2);
        s.act(Action::new("open", "open").moving(|wm| {
            wm.open(PanelId::bare(NOTE), None, false);
        }));
        s.settle();
        assert_eq!(MenuSig::of(&s).ws, vec![(0, false), (1, false), (2, true)]);
    }

    /// An app with nothing wrong puts no problems menu on the bar at all.
    #[test]
    fn a_world_with_nothing_wrong_has_no_problems_menu() {
        let s = kernel::session::Session::fake(&[]);
        assert!(MenuSig::of(&s).problems.is_empty());
    }
}
