//! The keyboard: the workspace's reserved chords, then the routing order.
//!
//! Cmd is the workspace modifier. A chord is offered in one order and stops
//! at the first taker:
//!
//! 1. the workspace's **reserved** chords — arrows and digits with and
//!    without shift, `w`, `z`, `u`, `i`, the column keys, `enter`, and the
//!    three shifted letters: `shift+l`, which puts the panels library up
//!    over the workspace, `shift+s`, the search panel, and `shift+a`, which
//!    offers the focused panel to whichever app takes one as context;
//! 2. the **focused widget**, which may take one (a live text field takes
//!    `cmd+a`) and says so in the same event;
//! 3. the **focused panel's bar**;
//! 4. the bar of the panel it **previews**, if it drives one — which is what
//!    lets a list act on the thing under its cursor without moving focus.
//!
//! Nothing in that order names a kind.
//!
//! A bold letter is a promise that the chord fires that verb *now*, so the
//! bars draw what this order would reach and nothing else: the set is
//! [`bar::bold`], over the [`Letters`] a widget keeps while one of its
//! fields has the keyboard.

use kernel::layout::Dir;
use kernel::panel::{slot_entity, VerbAct};
use kernel::session::Action;
use makepad_widgets::*;

use super::bar;
use super::overlays::Overlay;
use super::stage::{Shell, Stage};

/// The letters the workspace keeps for itself. A bar that wore one of them
/// would promise a chord that never arrives.
const RESERVED: [char; 6] = ['w', 'z', 'u', 't', 'i', 'l'];

/// The chords a caret keeps wherever one blinks: cut, copy, paste, and
/// select-all. The floor of what a widget's live field takes.
const TEXT: [char; 4] = ['x', 'c', 'v', 'a'];

/// Whether a letter is one of the workspace's own.
#[must_use]
pub fn is_reserved(c: char) -> bool {
    Letters::RESERVED.has(c)
}

/// A set of letters, as a mask over the alphabet: what a widget's live field
/// keeps to itself, and what a bar draws bold.
///
/// Copying and subtracting one is a couple of instructions, which is what the
/// drawing wants: [`bar::bold`] takes one bar's letters away from another's
/// on every draw of every panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Letters(u32);

/// The bit a letter stands for; anything else is no bit at all.
const fn bit(c: char) -> u32 {
    let c = c as u32;
    if c >= 'a' as u32 && c <= 'z' as u32 {
        1 << (c - 'a' as u32)
    } else if c >= 'A' as u32 && c <= 'Z' as u32 {
        1 << (c - 'A' as u32)
    } else {
        0
    }
}

impl Letters {
    /// No letter at all: what a bar that a chord never reaches draws bold.
    pub const NONE: Letters = Letters(0);
    /// Every letter. What a field that answers each cmd chord itself keeps —
    /// which is what every field in this build does.
    pub const ALL: Letters = Letters((1 << 26) - 1);
    /// The text chords, `x`, `c`, `v` and `a`.
    pub const TEXT: Letters = Letters::of(&TEXT);
    /// The workspace's own, read off the one list of them: a reserved letter
    /// is never bold, whatever a bar wears.
    pub const RESERVED: Letters = Letters::of(&RESERVED);

    #[must_use]
    pub const fn of(letters: &[char]) -> Letters {
        let (mut m, mut i) = (0, 0);
        while i < letters.len() {
            m |= bit(letters[i]);
            i += 1;
        }
        Letters(m)
    }

    #[must_use]
    pub const fn has(self, c: char) -> bool {
        let b = bit(c);
        b != 0 && self.0 & b != 0
    }

    /// This set with one more letter in it.
    #[must_use]
    pub const fn with(self, c: char) -> Letters {
        Letters(self.0 | bit(c))
    }

    /// The two sets together.
    #[must_use]
    pub const fn plus(self, other: Letters) -> Letters {
        Letters(self.0 | other.0)
    }

    /// This set with another taken out of it.
    #[must_use]
    pub const fn minus(self, other: Letters) -> Letters {
        Letters(self.0 & !other.0)
    }
}

/// The alphabet, both ways — read by the chord parser and by the
/// accelerator resolver, so a key's name and its code cannot drift.
const ALPHABET: [(char, KeyCode); 26] = [
    ('a', KeyCode::KeyA),
    ('b', KeyCode::KeyB),
    ('c', KeyCode::KeyC),
    ('d', KeyCode::KeyD),
    ('e', KeyCode::KeyE),
    ('f', KeyCode::KeyF),
    ('g', KeyCode::KeyG),
    ('h', KeyCode::KeyH),
    ('i', KeyCode::KeyI),
    ('j', KeyCode::KeyJ),
    ('k', KeyCode::KeyK),
    ('l', KeyCode::KeyL),
    ('m', KeyCode::KeyM),
    ('n', KeyCode::KeyN),
    ('o', KeyCode::KeyO),
    ('p', KeyCode::KeyP),
    ('q', KeyCode::KeyQ),
    ('r', KeyCode::KeyR),
    ('s', KeyCode::KeyS),
    ('t', KeyCode::KeyT),
    ('u', KeyCode::KeyU),
    ('v', KeyCode::KeyV),
    ('w', KeyCode::KeyW),
    ('x', KeyCode::KeyX),
    ('y', KeyCode::KeyY),
    ('z', KeyCode::KeyZ),
];

/// The key code a letter presses.
#[must_use]
pub fn letter_key(c: char) -> Option<KeyCode> {
    let lower = c.to_ascii_lowercase();
    ALPHABET.iter().find(|(l, _)| *l == lower).map(|(_, k)| *k)
}

/// The letter a key code types, if it is one.
#[must_use]
pub fn key_char(k: KeyCode) -> Option<char> {
    ALPHABET.iter().find(|(_, c)| *c == k).map(|(l, _)| *l)
}

/// How a scripted `key` chord executes: as a synthesized key event, as text
/// (plain letters reach panels the way real typing does), or as a bare
/// modifier tap.
pub enum ChordExec {
    Ev(KeyEvent),
    Text(String),
    Tap(KeyCode),
}

/// Reads a chord as a script spells it: `cmd+shift+left`, `enter`, `j`.
#[must_use]
pub fn parse_chord(s: &str) -> Option<ChordExec> {
    if let "cmd" | "logo" | "super" = s {
        return Some(ChordExec::Tap(KeyCode::Logo));
    }
    let mut mods = KeyModifiers::default();
    let mut key: Option<&str> = None;
    for part in s.split('+') {
        match part {
            "cmd" | "logo" | "super" => mods.logo = true,
            "shift" => mods.shift = true,
            "alt" | "option" => mods.alt = true,
            "ctrl" | "control" => mods.control = true,
            k => key = Some(k),
        }
    }
    let key = key?;
    let code = match key {
        "left" => Some(KeyCode::ArrowLeft),
        "right" => Some(KeyCode::ArrowRight),
        "up" => Some(KeyCode::ArrowUp),
        "down" => Some(KeyCode::ArrowDown),
        "enter" | "return" => Some(KeyCode::ReturnKey),
        "esc" | "escape" => Some(KeyCode::Escape),
        "backspace" => Some(KeyCode::Backspace),
        "delete" => Some(KeyCode::Delete),
        "tab" => Some(KeyCode::Tab),
        "comma" | "," => Some(KeyCode::Comma),
        "period" | "." => Some(KeyCode::Period),
        "bracketleft" | "[" => Some(KeyCode::LBracket),
        "bracketright" | "]" => Some(KeyCode::RBracket),
        "1" => Some(KeyCode::Key1),
        "2" => Some(KeyCode::Key2),
        "3" => Some(KeyCode::Key3),
        "4" => Some(KeyCode::Key4),
        "5" => Some(KeyCode::Key5),
        "6" => Some(KeyCode::Key6),
        "7" => Some(KeyCode::Key7),
        "8" => Some(KeyCode::Key8),
        "9" => Some(KeyCode::Key9),
        // Every letter parses, so a script can drive any accelerator.
        k => k
            .chars()
            .next()
            .filter(|_| k.chars().count() == 1)
            .and_then(letter_key),
    };
    let plain = !mods.logo && !mods.control && !mods.alt;
    match (code, plain, key.chars().count()) {
        // A modified chord, or a control key: a real key event.
        (Some(c), false, _)
        | (
            Some(
                c @ (KeyCode::ArrowLeft
                | KeyCode::ArrowRight
                | KeyCode::ArrowUp
                | KeyCode::ArrowDown
                | KeyCode::ReturnKey
                | KeyCode::Escape
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Tab),
            ),
            true,
            _,
        ) => Some(ChordExec::Ev(KeyEvent {
            key_code: c,
            modifiers: mods,
            is_repeat: false,
            time: 0.0,
        })),
        // A plain letter: panels receive it as text, like real typing.
        (_, true, 1) => Some(ChordExec::Text(key.to_string())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Double-cmd
// ---------------------------------------------------------------------------

/// Max gap between the two taps, seconds.
const CMD_TAP_GAP: f64 = 0.35;
/// Max hold of each tap — longer means a chord was intended, not a tap.
const CMD_TAP_HOLD: f64 = 0.5;

/// The double-cmd detector (the JetBrains double-shift move, on the
/// workspace key). A tap only counts while *clean*: any other key, click or
/// scroll while cmd is down means a chord and dirties it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum CmdTap {
    #[default]
    Idle,
    /// First press down, so far clean.
    Down { t: f64, dirty: bool },
    /// One clean tap done; a second press within the gap arms the trigger.
    Up { t: f64 },
    /// Second press down; a clean release fires.
    Down2 { t: f64, dirty: bool },
}

impl CmdTap {
    pub fn press(&mut self, t: f64) {
        *self = match *self {
            CmdTap::Up { t: t0 } if t - t0 <= CMD_TAP_GAP => CmdTap::Down2 { t, dirty: false },
            _ => CmdTap::Down { t, dirty: false },
        };
    }

    /// Answers whether the double tap fired.
    pub fn release(&mut self, t: f64) -> bool {
        let (next, fire) = match *self {
            CmdTap::Down {
                t: t0,
                dirty: false,
            } if t - t0 <= CMD_TAP_HOLD => (CmdTap::Up { t }, false),
            CmdTap::Down2 {
                t: t0,
                dirty: false,
            } if t - t0 <= CMD_TAP_HOLD => (CmdTap::Idle, true),
            _ => (CmdTap::Idle, false),
        };
        *self = next;
        fire
    }

    /// Any other input: a held press turns into a chord, a pending second
    /// tap is abandoned.
    pub fn other_input(&mut self) {
        *self = match *self {
            CmdTap::Down { t, .. } => CmdTap::Down { t, dirty: true },
            CmdTap::Down2 { t, .. } => CmdTap::Down2 { t, dirty: true },
            _ => CmdTap::Idle,
        };
    }
}

// ---------------------------------------------------------------------------
// The routing
// ---------------------------------------------------------------------------

impl Stage {
    pub(super) fn handle_key_down(&mut self, cx: &mut Cx, sh: &mut Shell, k: &KeyEvent) {
        // A bare cmd press only feeds the double-tap detector; the firing
        // side lives in `handle_key_up`.
        if k.key_code == KeyCode::Logo {
            if !k.is_repeat {
                self.cmd_tap.press(k.time);
            }
            return;
        }
        self.cmd_tap.other_input();

        if sh.overlay != Overlay::None && self.overlay_key(cx, sh, k) {
            return;
        }
        if k.modifiers.logo && self.reserved_chord(cx, sh, k) {
            return;
        }
        if k.modifiers.logo {
            // Past the reserved set the chord belongs to the panel: its
            // widget first, then its bar, then the bar of what it previews.
            if self.forward_to_focused(cx, sh, &Event::KeyDown(*k)) {
                return;
            }
            if let Some(c) = key_char(k.key_code) {
                let focus = sh.session.focus();
                if let Some(f) = focus {
                    if self.bar_chord(sh, f, c) {
                        return;
                    }
                    // The last step is what lets a list act on the thing
                    // under its cursor without moving focus.
                    //
                    // A caret in that panel's own widget keeps its letters
                    // there as surely as one on the focused panel does —
                    // and a click puts a caret in a previewed panel without
                    // moving focus at all, so this is the only place that
                    // hears of it. The chord goes to the widget, its bar
                    // never sees it, and `bar::bold` drew exactly that.
                    if let Some(child) = sh.session.joined_child(f) {
                        if self.field_letters(Some(child)).has(c) {
                            self.forward_to_slot(cx, sh, child, &Event::KeyDown(*k));
                            return;
                        }
                        if self.bar_chord(sh, child, c) {
                            return;
                        }
                    }
                }
            }
            return;
        }
        // A plain key belongs to the focused panel's widget.
        self.forward_to_focused(cx, sh, &Event::KeyDown(*k));
    }

    /// Only the launcher trigger cares about key releases: a clean second
    /// cmd tap fires here.
    pub(super) fn handle_key_up(&mut self, cx: &mut Cx, sh: &mut Shell, k: &KeyEvent) {
        if k.key_code == KeyCode::Logo && self.cmd_tap.release(k.time) {
            self.toggle_launcher(cx, sh);
            return;
        }
        self.forward_to_focused(cx, sh, &Event::KeyUp(*k));
    }

    /// Text into whatever owns the keyboard: the launcher's field while it
    /// is up, the focused panel otherwise.
    pub(super) fn handle_text(&mut self, cx: &mut Cx, sh: &mut Shell, input: &str) {
        self.text_in(cx, sh, input, false);
    }

    /// The same door, for text that arrived as a **paste**. Two differences,
    /// and both matter to a widget that reads one: the event says
    /// `was_paste`, so a composer can take a pasted panel context as a chip
    /// while a typed line that happens to read the same stays typed; and the
    /// control characters are kept, because a pasted document is one with
    /// newlines in it.
    pub(super) fn handle_paste(&mut self, cx: &mut Cx, sh: &mut Shell, input: &str) {
        self.text_in(cx, sh, input, true);
    }

    fn text_in(&mut self, cx: &mut Cx, sh: &mut Shell, input: &str, was_paste: bool) {
        if input.is_empty() || (!was_paste && input.chars().any(char::is_control)) {
            return;
        }
        let ev = Event::TextInput(TextInputEvent {
            input: input.to_string(),
            was_paste,
            ..Default::default()
        });
        if sh.overlay == Overlay::Launcher {
            self.forward_to_overlay(cx, sh, &ev);
            return;
        }
        self.forward_to_focused(cx, sh, &ev);
    }

    /// The workspace's own chords. Answers whether one of them took it.
    fn reserved_chord(&mut self, cx: &mut Cx, sh: &mut Shell, k: &KeyEvent) -> bool {
        let num = match k.key_code {
            KeyCode::Key1 => Some(0),
            KeyCode::Key2 => Some(1),
            KeyCode::Key3 => Some(2),
            KeyCode::Key4 => Some(3),
            KeyCode::Key5 => Some(4),
            KeyCode::Key6 => Some(5),
            KeyCode::Key7 => Some(6),
            KeyCode::Key8 => Some(7),
            KeyCode::Key9 => Some(8),
            _ => None,
        };
        if let Some(n) = num {
            if k.modifiers.shift {
                self.move_to_ws(sh, n);
            } else {
                self.switch_ws(sh, n);
            }
            return true;
        }
        let dir = match k.key_code {
            KeyCode::ArrowLeft => Some(Dir::Left),
            KeyCode::ArrowRight => Some(Dir::Right),
            KeyCode::ArrowUp => Some(Dir::Up),
            KeyCode::ArrowDown => Some(Dir::Down),
            _ => None,
        };
        if let Some(dir) = dir {
            if k.modifiers.shift {
                if let Some(f) = sh.session.focus() {
                    let label = format!("move “{}”", title_of(sh, f));
                    sh.session.act(
                        Action::new("move", label)
                            .about(slot_entity(f))
                            .moving(move |wm| wm.move_slot(f, dir)),
                    );
                }
            } else {
                // Focus walks are context, not actions — never undo nodes.
                sh.session.focus_dir(dir);
            }
            return true;
        }
        match k.key_code {
            KeyCode::KeyZ => {
                if k.modifiers.shift {
                    self.do_redo(sh);
                } else {
                    self.do_undo(sh);
                }
                true
            }
            // The focused panel's identity, what it was asked for, and the
            // queries its last draw ran — to the clipboard.
            KeyCode::KeyI => {
                self.copy_panel_context(sh);
                true
            }
            KeyCode::KeyU => {
                sh.overlay = if sh.overlay == Overlay::History {
                    Overlay::None
                } else {
                    Overlay::History
                };
                sh.session.redraw();
                true
            }
            KeyCode::KeyW => {
                if let Some(f) = sh.session.focus() {
                    self.close_slot(sh, f);
                }
                true
            }
            // niri's column operations.
            KeyCode::LBracket
            | KeyCode::RBracket
            | KeyCode::Comma
            | KeyCode::Period
            | KeyCode::KeyT => {
                let Some(f) = sh.session.focus() else {
                    return true;
                };
                let (label, code) = (column_label(k.key_code), k.key_code);
                sh.session
                    .act(
                        Action::new("column", label)
                            .about(slot_entity(f))
                            .moving(move |wm| match code {
                                KeyCode::LBracket => wm.consume_or_expel(f, Dir::Left),
                                KeyCode::RBracket => wm.consume_or_expel(f, Dir::Right),
                                KeyCode::Comma => wm.consume_from_right(f),
                                KeyCode::Period => wm.expel_bottom(f),
                                _ => wm.toggle_tabbed(f),
                            }),
                    );
                true
            }
            // The Dev chord: the panels library, over the workspace. The
            // root acts on it — the canvas is the stage's sibling, not its
            // child — so the stage only says so.
            KeyCode::KeyL if k.modifiers.shift => {
                cx.action(super::library::DevAction::ToggleLibrary);
                true
            }
            // Search: the panel that puts one question to every app's
            // sources, focused wherever it already is.
            //
            // Only the shifted chord is taken. `s` is not in [`RESERVED`],
            // because plain `cmd+s` still belongs to whatever bar wears it
            // — mail's *sync* and *send* both do — and a bar is only ever
            // reached without shift.
            KeyCode::KeyS if k.modifiers.shift => {
                self.go_to(sh, super::search_panel());
                true
            }
            // A panel as context for an agent: the focused slot is offered
            // to the apps and the first one that answers a panel takes it.
            //
            // Only the shifted chord is taken, for the reason `shift+s` is:
            // plain `cmd+a` belongs to whatever bar wears it — mail's
            // *archive n* does — and to the select-all of every field.
            KeyCode::KeyA if k.modifiers.shift => {
                self.ask_about_focused(sh);
                true
            }
            // Reserved so that no bar may claim it: a list reads it as
            // *open un-joined*, which the shell leaves to the panel.
            KeyCode::ReturnKey => {
                self.forward_to_focused(cx, sh, &Event::KeyDown(*k));
                true
            }
            _ => false,
        }
    }

    /// Fires the verb a letter names on one panel's bar. Answers whether
    /// the bar had one.
    fn bar_chord(&mut self, sh: &mut Shell, slot: kernel::layout::SlotId, c: char) -> bool {
        let Some(inst) = sh.session.panel(slot) else {
            return false;
        };
        let id = {
            let verbs = inst.borrow().verbs();
            bar::chord(&verbs, c)
        };
        let Some(id) = id else { return false };
        self.run_verb(sh, slot, id);
        true
    }

    /// Runs one verb by its id, pulling the bar again as it fires: the bar
    /// is a view of the instance, never a copy of it.
    ///
    /// A verb of the panel's own is the panel's own method: the instance is
    /// borrowed for the whole of it, and the session it is handed touches
    /// no instance until the stage settles — so a verb may read its table
    /// and then close its own slot.
    pub(super) fn run_verb(&mut self, sh: &mut Shell, slot: kernel::layout::SlotId, id: &str) {
        self.run_verb_fresh(sh, slot, id, false);
    }

    /// The same, with the workspace modifier held: an entry that goes
    /// somewhere opens it un-joined, exactly as `cmd+click` does on a row or
    /// `cmd+enter` on a list. A verb that opens nothing does not care.
    pub(super) fn run_verb_fresh(
        &mut self,
        sh: &mut Shell,
        slot: kernel::layout::SlotId,
        id: &str,
        fresh: bool,
    ) {
        let Some(inst) = sh.session.panel(slot) else {
            return;
        };
        let act = {
            let verbs = inst.borrow().verbs();
            verbs.into_iter().find(|v| v.id == id).map(|v| v.act)
        };
        match act {
            Some(VerbAct::Run) => inst.borrow_mut().run(id, &mut sh.session),
            Some(VerbAct::Call(f)) => f(&mut sh.session),
            Some(VerbAct::Go(nav)) => sh.session.nav(un_join(nav, fresh)),
            None => {}
        }
    }

    /// Offers the focused panel to the apps as context, in list order, and
    /// stops at the first taker. Shared with the menu, which offers the same
    /// move as an item.
    ///
    /// Nothing here names an app or knows what taking one means: an app that
    /// answers a panel with a chat opens it joined to the panel and says so,
    /// and a build with nothing that does says that instead of doing
    /// nothing.
    pub(super) fn ask_about_focused(&mut self, sh: &mut Shell) {
        let Some(slot) = sh.session.focus() else {
            sh.session.notify("no focused panel", false);
            return;
        };
        // The list outlives the session it came from, so it is read out
        // before the session is borrowed to act on.
        let apps = sh.session.apps().list();
        for app in apps {
            if app.ask(&mut sh.session, slot) {
                return;
            }
        }
        sh.session
            .notify("nothing takes a panel as context in this build", false);
    }

    /// One step back through the history, and one step forward. Shared with
    /// the menu, which offers the same two moves as items.
    pub(super) fn do_undo(&mut self, sh: &mut Shell) {
        if !sh.session.undo() {
            sh.session.notify("nothing to undo", false);
        }
    }

    pub(super) fn do_redo(&mut self, sh: &mut Shell) {
        if !sh.session.redo() {
            sh.session.notify("nothing to redo", false);
        }
    }

    /// Goes to a workspace. Attention, not an action.
    pub(super) fn switch_ws(&mut self, sh: &mut Shell, k: usize) {
        sh.overlay = Overlay::None;
        if sh.session.switch(k) {
            let cam = sh.session.scene().camera_x;
            sh.anim.camera().jump_to(cam);
        }
        sh.session.redraw();
    }

    /// Moves the focused panel to workspace `k` and follows it: the whole
    /// viewport slides, the panel rides along.
    pub(super) fn move_to_ws(&mut self, sh: &mut Shell, k: usize) {
        sh.overlay = Overlay::None;
        let Some(f) = sh.session.focus() else { return };
        let label = format!("move “{}” to workspace {}", title_of(sh, f), k + 1);
        sh.session
            .act(Action::new("movews", label).moving(move |wm| {
                wm.send_focused_to(k);
            }));
        // The scene is recomputed by the settle, and the camera the panel
        // rides to is read off it.
        sh.session.settle();
        let cam = sh.session.scene().camera_x;
        sh.anim.camera().jump_to(cam);
    }
}

/// A navigation with the workspace modifier applied: a join becomes a fresh
/// column of its own, and a replace becomes an open beside the panel that
/// asked — a bar's link answers `cmd` the way a row does.
fn un_join(nav: kernel::nav::Nav, fresh: bool) -> kernel::nav::Nav {
    use kernel::nav::Nav;
    if !fresh {
        return nav;
    }
    match nav {
        Nav::Open { from, id, .. } | Nav::Preview { from, id } => Nav::Open {
            from,
            id,
            fresh: true,
        },
        Nav::Replace { slot, id } => Nav::Open {
            from: slot,
            id,
            fresh: true,
        },
        other => other,
    }
}

/// A panel's title, as an action labels it.
fn title_of(sh: &Shell, slot: kernel::layout::SlotId) -> String {
    sh.session
        .panel(slot)
        .map(|p| p.borrow().title())
        .unwrap_or_else(|| "panel".into())
}

fn column_label(k: KeyCode) -> &'static str {
    match k {
        KeyCode::LBracket => "consume left",
        KeyCode::RBracket => "expel right",
        KeyCode::Comma => "pull from the right",
        KeyCode::Period => "push the bottom out",
        _ => "toggle tabs",
    }
}
