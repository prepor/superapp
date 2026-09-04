//! The colophon, and the way back.

use std::any::Any;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use makepad_widgets::*;

use crate::shell::dsl::LinkViewExt;
use crate::shell::hosted::PanelProps;

/// The about panel's instance.
pub struct About {
    id: PanelId,
    slot: SlotId,
}

impl About {
    pub const TAG: Tag = Tag("about");

    /// The identity of the one about panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}

impl Panel for About {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "about".into()
    }

    /// The colophon.
    fn about(&self) -> String {
        "The colophon: what this build is, what it was made of, and where it \
         keeps its one database. It takes no arguments and reads nothing but \
         the constants it was compiled with, so there is nothing here that can \
         be out of date. Its one link goes back to the manual."
            .into()
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// One link back, in place: a dotted link is a replace.
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::go(
            "system.help",
            "help",
            Some('h'),
            Nav::Replace {
                slot: self.slot,
                id: super::Help::id(),
            },
        )]
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct AboutKind;

impl PanelKind for AboutKind {
    fn tag(&self) -> Tag {
        About::TAG
    }

    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(About {
            id: id.clone(),
            slot: 0,
        })
    }
}

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct AboutPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for AboutPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let slot = scope.props.get::<PanelProps>().map_or(0, |p| p.slot);
        self.view.link(cx, ids!(help_link)).set(
            cx,
            "back to help",
            Nav::Replace {
                slot,
                id: super::Help::id(),
            },
            true,
            None,
        );
        self.view.draw_walk(cx, scope, walk)
    }
}
