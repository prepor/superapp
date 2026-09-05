//! The manual, and the design language's own showcase.

use std::any::Any;

use kernel::caps::Clip;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::dsl::LinkViewExt;
use crate::shell::hosted::PanelProps;

/// What the demo verb puts on the clipboard. Short, so the sentence the
/// log draws for it is one a script can name.
const DEMO: &str = "superapp";

/// The help panel's instance. It owns nothing but where it is: everything
/// it shows is static, and the two demo links are built on every draw from
/// the slot it landed in.
pub struct Help {
    id: PanelId,
    slot: SlotId,
}

impl Help {
    pub const TAG: Tag = Tag("help");

    /// The identity of the one help panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}

impl Panel for Help {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "help".into()
    }

    /// The manual is not data, and says so.
    fn about(&self) -> String {
        "The manual, and the design language's own showcase: the legend for \
         the three interactive signals, the workspace's keys, and the grammar \
         of a panel — drawn with the very widgets it describes, so the links \
         on it really open and the bar at its foot really carries this \
         panel's verbs. It takes no arguments and reads no rows at all: there \
         is nothing here to query. It is what a person reads to learn the \
         shell, and the one panel a fresh store comes up on."
            .into()
    }

    /// Wide and tall enough for the legend and the keys together.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (5, 5)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// A button that does something outside, and a link that goes
    /// somewhere: the two halves of a bar, one of each.
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::run("system.demo", "try it", Some('y')),
            Verb::go(
                "system.about",
                "about",
                Some('b'),
                Nav::Open {
                    from: self.slot,
                    id: super::About::id(),
                    fresh: false,
                },
            ),
        ]
    }

    /// The demo's effect is real — a line onto the clipboard, through the
    /// world's [`Clip`] capability — so the log has a row to show for it
    /// and the demo is not a mime of one.
    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb != "system.demo" {
            return;
        }
        let world = s.world().clone();
        match world.run(&Clip {
            text: DEMO,
            what: "the demo line",
        }) {
            Ok(()) => s.notify("copied — the effects list has the row", false),
            Err(e) => s.notify(e, true),
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct HelpKind;

impl PanelKind for HelpKind {
    fn tag(&self) -> Tag {
        Help::TAG
    }

    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Help {
            id: id.clone(),
            slot: 0,
        })
    }
}

/// The widget: static prose in the DSL, the two demo links settled here.
#[derive(Script, ScriptHook, Widget)]
pub struct HelpPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for HelpPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let slot = scope.props.get::<PanelProps>().map_or(0, |p| p.slot);
        // The legend demonstrates the grammar with the real thing: these
        // links open and replace exactly like any other.
        self.view.link(cx, ids!(solid_link)).set(
            cx,
            "solid underline",
            Nav::Open {
                from: slot,
                id: super::About::id(),
                fresh: false,
            },
            false,
            None,
        );
        self.view.link(cx, ids!(dotted_link)).set(
            cx,
            "dotted underline",
            Nav::Replace {
                slot,
                id: super::About::id(),
            },
            true,
            None,
        );
        self.view.draw_walk(cx, scope, walk)
    }
}
