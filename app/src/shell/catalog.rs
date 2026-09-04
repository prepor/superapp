//! What a panels-library node comes up as, and the shell's own scenes.
//!
//! A node holds a bare widget populated from a fixture, a stage solo on one
//! panel, or a whole workspace; a stage node may replay e2e steps to reach
//! its state. Fixtures are built with real types and the widgets' own APIs,
//! so a change that would break a scene breaks the build instead.
//!
//! The shell's own scenes are here: the link grammar, an overlay row, the
//! launcher's sheet, the workspace, and the phone grid. Everything else the
//! canvas shows comes from an app, through
//! [`AppUi::scenes`](super::app_ui::AppUi::scenes) — the shell's own app
//! included, which is how it proves the seam on itself.

use std::rc::Rc;

use kernel::app::Mode;
use kernel::e2e::{self, Step};
use kernel::layout::Grid;
use kernel::nav::Nav;
use kernel::panel::PanelId;
use kernel::scene::Scene;
use kernel::store::Store;
use makepad_widgets::*;

use super::dsl::{OverlayProps, OverlayRowData, OverlayRowWidgetRefExt, SLinkWidgetRefExt};

/// Sets a component's state through its own API, once, when it mounts.
pub type Populate = Rc<dyn Fn(&mut Cx, &WidgetRef)>;

/// What a solo stage opens on, resolved against its own seeded store — and
/// the one place a node may put something *into* that store, for a subject
/// the demo seed does not cover.
pub type Open = Rc<dyn Fn(&Store) -> PanelId>;

/// How a node comes up.
#[derive(Clone)]
pub enum Setup {
    /// A bare widget from the library's template `tpl`, populated once when
    /// it is mounted. `overlay` rides the scope for the sheets, whose rows
    /// come from their props on every draw.
    Widget {
        tpl: LiveId,
        populate: Populate,
        overlay: Option<OverlayProps>,
    },
    /// A stage on a world of its own: solo on the one panel `open` names,
    /// the whole workspace starting from that panel, or the default
    /// session. `steps` lead to the state.
    Stage {
        open: Option<Open>,
        /// With `open`: the panel alone at the viewport (a panel node)
        /// rather than as the first column of a strip.
        solo: bool,
        steps: Option<Vec<Step>>,
        grid: Option<Grid>,
        /// Which outside the mount's world gets. `Deny` unless the subject
        /// reads past its store.
        mode: Mode,
    },
}

/// A bare widget, populated once.
#[must_use]
pub fn widget(tpl: LiveId, f: impl Fn(&mut Cx, &WidgetRef) + 'static) -> Setup {
    Setup::Widget {
        tpl,
        populate: Rc::new(f),
        overlay: None,
    }
}

/// The same, with the props a sheet draws its rows from.
#[must_use]
pub fn sheet(tpl: LiveId, props: OverlayProps, f: impl Fn(&mut Cx, &WidgetRef) + 'static) -> Setup {
    Setup::Widget {
        tpl,
        populate: Rc::new(f),
        overlay: Some(props),
    }
}

/// One panel alone at the viewport, on a world with no outside: an effect
/// it files fails in words rather than quietly working.
#[must_use]
pub fn panel(open: impl Fn(&Store) -> PanelId + 'static, script: &str) -> Setup {
    Setup::Stage {
        open: Some(Rc::new(open)),
        solo: true,
        steps: steps(script),
        grid: None,
        mode: Mode::Deny,
    }
}

/// A panel on a **fake** outside: what reads beyond the store — a listing
/// reads its directory through a capability — draws the demo tree rather
/// than *this world has no …*.
#[must_use]
pub fn panel_fake(open: impl Fn(&Store) -> PanelId + 'static, script: &str) -> Setup {
    Setup::Stage {
        open: Some(Rc::new(open)),
        solo: true,
        steps: steps(script),
        grid: None,
        mode: Mode::Fake,
    }
}

/// The default session — whatever the app list's first root is — for the
/// shell's own subjects.
#[must_use]
pub fn workspace(script: &str) -> Setup {
    Setup::Stage {
        open: None,
        solo: false,
        steps: steps(script),
        grid: None,
        mode: Mode::Deny,
    }
}

/// A workspace that starts from one panel and nothing else: a story about
/// what that panel opens beside itself.
#[must_use]
pub fn workspace_on(open: impl Fn(&Store) -> PanelId + 'static, script: &str) -> Setup {
    Setup::Stage {
        open: Some(Rc::new(open)),
        solo: false,
        steps: steps(script),
        grid: None,
        mode: Mode::Fake,
    }
}

/// The cover display: a 4×3 grid, panels clamped to it.
#[must_use]
pub fn phone(script: &str) -> Setup {
    Setup::Stage {
        open: None,
        solo: false,
        steps: steps(script),
        grid: Some(Grid { w: 4, h: 3 }),
        mode: Mode::Deny,
    }
}

/// How long a stage settles before its state counts as reached: the springs
/// a boot starts — a panel's fade-in, the camera's pan to focus — have to
/// land before the node freezes into a picture.
const SETTLE_MS: u64 = 900;

/// Steps in the harness's grammar, ending in the arrival the stage waits
/// for. An empty script is the boot itself, settled.
///
/// # Panics
///
/// If the script does not parse — a scene is source, so a typo in one is a
/// build-time mistake found at the first boot of the canvas.
#[must_use]
pub fn steps(script: &str) -> Option<Vec<Step>> {
    if script.trim().is_empty() {
        return Some(vec![Step::Wait(SETTLE_MS), Step::Quit]);
    }
    let mut steps = e2e::parse(script).unwrap_or_else(|e| panic!("catalog: {e}: {script:?}"));
    if steps.last() != Some(&Step::Quit) {
        steps.push(Step::Quit);
    }
    Some(steps)
}

// ---------------------------------------------------------------------------
// The canvas
// ---------------------------------------------------------------------------

/// Every scene on the canvas: the shell's own, then each app's in app-list
/// order. The shell names no app — it asks the list it was booted with.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    let mut all = vec![
        link(),
        overlay_row(),
        launcher(),
        workspace_scene(),
        phone_scene(),
    ];
    for ui in super::uis() {
        all.extend(ui.scenes());
    }
    all
}

// ---------------------------------------------------------------------------
// The shell's own scenes
// ---------------------------------------------------------------------------

/// The link grammar, on the widget that implements it. The navigation is
/// never run — a fixture is a picture — so every one of them is a focus of
/// the slot it is in.
fn link() -> Scene<Setup> {
    let l = |text: &'static str, dotted: bool, accel: Option<char>| {
        widget(live_id!(link_tpl), move |cx, w| {
            w.as_slink().set(cx, text, Nav::Focus(0), dotted, accel);
        })
    };
    Scene::new("link", (280.0, 28.0))
        .note("The underline grammar: solid opens beside, dotted replaces in place.")
        .note("A link that has a chord wears its letter, drawn bold.")
        .node("solid", l("Elena Petrova", false, None))
        .about("opens joined, to the right")
        .node("dotted", l("back to the manual", true, None))
        .about("replaces this panel")
        .node("accelerator", l("reply", false, Some('r')))
        .about("cmd+r")
        .edge("solid", "dotted", "the same link, dotted")
}

/// One row of a modal sheet, in each of its states.
fn overlay_row() -> Scene<Setup> {
    let row = |d: OverlayRowData| {
        widget(live_id!(overlay_row_tpl), move |cx, w| {
            w.as_overlay_row().populate(cx, &d);
        })
    };
    let plain = |main: &str| OverlayRowData {
        main: main.to_string(),
        ..Default::default()
    };
    Scene::new("overlay row", (520.0, 40.0))
        .note("One row of a modal sheet — the workspaces roster, the undo history, a launcher hit.")
        .note("The sheet is the chassis; this is what it stacks.")
        .node("plain", row(plain("the manual")))
        .node(
            "hovered",
            row(OverlayRowData {
                hovered: true,
                ..plain("the manual")
            }),
        )
        .about("the wash a button takes under the pointer")
        .node(
            "current",
            row(OverlayRowData {
                current: true,
                ..plain("the manual")
            }),
        )
        .about("inverted: the current workspace, the selected hit, the head of the history")
        .node(
            "muted",
            row(OverlayRowData {
                muted: true,
                ..plain("close “the manual”")
            }),
        )
        .about("an undone branch: quiet, still walkable")
        .node(
            "numbered",
            row(OverlayRowData {
                num: "3".into(),
                detail: "two panels".into(),
                ..plain("the manual · the colophon")
            }),
        )
        .about("a workspace wears its number and what stands on it")
        .node(
            "hit",
            row(OverlayRowData {
                detail: "everything that left the process".into(),
                right: "ws 4".into(),
                ..plain("the effect log")
            }),
        )
        .about("a launcher hit on another workspace wears its badge")
        .edge("plain", "hovered", "pointer over")
        .edge("plain", "current", "arrow / enter")
}

/// The launcher's sheet: the field over the hits.
fn launcher() -> Scene<Setup> {
    let hits = |query: &str, rows: Vec<OverlayRowData>| {
        let q = query.to_string();
        let props = OverlayProps {
            rows,
            query: q.clone(),
            alpha: 1.0,
        };
        sheet(live_id!(launcher_overlay_tpl), props, move |cx, w| {
            w.text_input(cx, ids!(query_input)).set_text(cx, &q);
        })
    };
    let row = |main: &str, detail: &str, right: &str| OverlayRowData {
        main: main.to_string(),
        detail: detail.to_string(),
        right: right.to_string(),
        ..Default::default()
    };
    Scene::new("launcher", (560.0, 300.0))
        .note("Double-cmd raises it: one field over the hits — what is open first, then every root the app list offers.")
        .node(
            "empty",
            hits(
                "",
                vec![
                    row("the manual", "", ""),
                    row("the colophon", "", ""),
                    row("the effect log", "", ""),
                    row("problems", "", ""),
                ],
            ),
        )
        .about("nothing typed: what is open, then what can be")
        .node(
            "hits",
            hits(
                "log",
                vec![
                    OverlayRowData {
                        current: true,
                        ..row("the effect log", "everything that left the process", "")
                    },
                    row("one job", "the row as sqlite3 shows it", "ws 4"),
                ],
            ),
        )
        .about("the selected hit is inverted; one elsewhere wears its workspace")
        .node("nothing", hits("zzz", Vec::new()))
        .about("a query nothing answers says so")
        .edge("empty", "hits", "type log")
        .edge("hits", "nothing", "type zzz")
}

/// The shell's own subject: columns, the launcher, the history, the second
/// workspace. Nothing in the script names a panel — every step is a chord
/// the workspace itself answers.
fn workspace_scene() -> Scene<Setup> {
    Scene::new("workspace", (1440.0, 900.0))
        .note("The shell's own subjects: columns, joins, the sheet over them — twelve by six units, panels placed niri-style.")
        .note("Live — enter a node and work it; shift+cmd+l puts the canvas back.")
        .node("boot", workspace(""))
        .about("the first root the app list offers, on an empty session")
        .node(
            "launcher",
            workspace("key cmd 2\nwait 500\ntype \"the\"\nwait 600"),
        )
        .about("double-cmd, then a query over everything open and every root")
        .node("history", workspace("key cmd+u\nwait 600"))
        .about("the whole tree of what has been done, walkable")
        .node("empty workspace", workspace("key cmd+2\nwait 700"))
        .about("a workspace nobody has put anything on names itself")
        .edge("boot", "launcher", "cmd cmd")
        .edge("boot", "history", "cmd+u")
        .edge("boot", "empty workspace", "cmd+2")
}

/// The cover display: the same session on a 4×3 grid.
fn phone_scene() -> Scene<Setup> {
    Scene::new("phone", (380.0, 780.0))
        .note("The cover display: a 4×3 grid, panels clamp to it.")
        .node("cover", phone(""))
        .about("one panel fills the screen")
        .node("second", phone("key cmd+2\nwait 700"))
        .about("the workspaces are the same nine")
        .edge("cover", "second", "cmd+2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_is_a_dag_with_a_name_per_state() {
        let all = scenes();
        assert!(all.len() >= 5, "the shell's own scenes at least");
        for s in &all {
            s.check().unwrap_or_else(|e| panic!("{e}"));
            assert!(!s.nodes.is_empty(), "{}: no nodes", s.name);
        }
        // The canvas's script addresses scenes by name.
        let mut names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len(), "two scenes of one name");
    }

    #[test]
    fn steps_end_in_an_arrival() {
        assert_eq!(steps(""), Some(vec![Step::Wait(SETTLE_MS), Step::Quit]));
        assert_eq!(steps("  \n"), Some(vec![Step::Wait(SETTLE_MS), Step::Quit]));
        let s = steps("wait 10\nkey down 2").unwrap();
        assert_eq!(s.last(), Some(&Step::Quit));
        assert_eq!(s.len(), 3);
        assert_eq!(steps("wait 10\nquit").unwrap().len(), 2);
    }
}
