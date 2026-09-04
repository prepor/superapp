//! One widget per slot, instantiated from its tag's template.
//!
//! A panel's body is a retained widget tree, built from the template its
//! app registered and kept across draws — the `PortalList` pattern at panel
//! scale. The scope carries the session (`&mut`, so a widget that changes
//! something calls straight into the kernel) and [`PanelProps`]: which slot
//! it is, the instance it draws, and the hit collector it registers into.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::session::Instance;
use makepad_widgets::*;

use super::hits::Hits;
use super::keys::Letters;
use super::overlays::Overlay;
use super::stage::{Shell, Stage};

/// What a hosted widget is handed beside the session.
///
/// A widget that needs the store, the world, or another app reads them off
/// the session; one that changes data calls its instance; one that
/// navigates calls [`Session::nav`](kernel::session::Session::nav).
#[derive(Clone)]
pub struct PanelProps {
    pub slot: SlotId,
    pub panel: Instance,
    pub hits: Hits,
    /// The chord the stage is offering, if it is offering one.
    pub chord: Chord,
}

/// A chord offered to the focused widget before the bar sees it, and what
/// the widget says about its own keyboard.
///
/// The widget takes the chord by calling [`Chord::take`] in the same event;
/// the stage checks before it runs a verb. It reports a live field of its
/// own through [`Chord::field`] — on every draw and every event, not only
/// on the event it takes, because a bar has to draw the promise before the
/// chord is pressed.
///
/// Shared cells rather than bubbled actions: makepad delivers actions on the
/// *next* event, and the stage has to know now.
#[derive(Clone, Default)]
pub struct Chord {
    taken: Rc<Cell<bool>>,
    field: Rc<Cell<Option<Letters>>>,
}

impl Chord {
    /// The widget handled it; nothing further should. The table's filter
    /// field is the one that does: while it is live `cmd+a` is select-all,
    /// not a verb on the bar.
    pub fn take(&self) {
        self.taken.set(true);
    }

    #[must_use]
    pub fn taken(&self) -> bool {
        self.taken.get()
    }

    /// A field of this widget has the keyboard. It keeps the text chords
    /// while it does — `x`, `c`, `v`, `a`, which belong to a caret wherever
    /// one blinks — and `extra` beside them: [`Letters::NONE`] for a field
    /// that keeps only those, [`Letters::ALL`] for one that answers every
    /// cmd chord itself.
    ///
    /// Said again on every draw and every event: it is a fact about now, and
    /// the frame after the caret leaves must not still be drawn as if it
    /// were there.
    pub fn field(&self, extra: Letters) {
        self.field.set(Some(extra.plus(Letters::TEXT)));
    }

    /// What the widget said, if it said anything this pass.
    #[must_use]
    pub fn field_keeps(&self) -> Option<Letters> {
        self.field.get()
    }
}

/// The two slots the modal overlays are hosted under. They are keyed
/// outside the layout's numbering, which is why they are the top of it.
pub const OVERLAY_ROWS: SlotId = SlotId::MAX;
pub const OVERLAY_LAUNCHER: SlotId = SlotId::MAX - 1;

/// Whether a key is one of the overlays rather than a slot.
#[must_use]
pub fn is_overlay(slot: SlotId) -> bool {
    slot == OVERLAY_ROWS || slot == OVERLAY_LAUNCHER
}

/// The card a slot gets when no app in this build owns its tag. The shell's
/// own fallback, drawn by the system app inside the shell.
pub const MISSING_TPL: LiveId = live_id!(sys_missing_tpl);

impl Stage {
    /// The template a tag draws with: the app that owns it answers, and a
    /// tag nobody owns gets the missing card.
    pub(super) fn template_for(&self, sh: &Shell, tag: kernel::panel::Tag) -> LiveId {
        if sh.session.apps().kind(tag).is_none() {
            return MISSING_TPL;
        }
        super::uis()
            .iter()
            .find_map(|ui| ui.template(tag))
            .unwrap_or(MISSING_TPL)
    }

    /// The live widget for a slot, instantiated from its template on first
    /// use. Answers whether it was created now.
    pub(super) fn hosted_widget(
        &mut self,
        cx: &mut Cx,
        slot: SlotId,
        tpl: LiveId,
    ) -> Option<(WidgetRef, bool)> {
        if let Some(w) = self.hosted.get(&slot) {
            return Some((w.clone(), false));
        }
        let template_ref = self.tpl.get(&tpl)?;
        let template_value: ScriptValue = template_ref.as_object().into();
        let vm_id = cx.script_ref_vm_id(template_ref)?;
        let widget =
            cx.with_script_vm_id(vm_id, |vm| WidgetRef::script_from_value(vm, template_value));
        self.hosted.insert(slot, widget.clone());
        Some((widget, true))
    }

    /// Draws a slot's retained content into the body rectangle.
    pub(super) fn draw_hosted(&mut self, cx: &mut Cx2d, sh: &mut Shell, slot: SlotId, body: Rect) {
        let Some(inst) = sh.session.panel(slot) else {
            return;
        };
        let id = inst.borrow().id().clone();
        let tpl = self.template_for(sh, id.tag);
        // A slot replaced in place keeps its number, and with it the widget
        // built for what it showed before. Another identity is another
        // panel: the widget is built again rather than re-seeded by hand.
        if self
            .hosted_for
            .get(&slot)
            .is_some_and(|(t, i)| *t != tpl || *i != id)
        {
            self.hosted.remove(&slot);
        }
        let Some((w, _)) = self.hosted_widget(cx, slot, tpl) else {
            eprintln!("shell: no template for {id}");
            return;
        };
        self.hosted_for.insert(slot, (tpl, id));

        let props = PanelProps {
            slot,
            panel: inst,
            hits: self.hits.clone(),
            chord: Chord::default(),
        };
        let mut scope = Scope::with_data_props(&mut sh.session, &props);
        cx.begin_turtle(
            Walk::abs_rect(body),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        w.draw_all(cx, &mut scope);
        cx.end_turtle();
        // Right after the widget drew, when its rectangles are this frame's.
        self.heard_field(cx, sh, slot, &w, &props.chord);
    }

    /// Files what a widget's keyboard is doing: what it said through
    /// [`Chord::field`] this draw, or — for one that has not been taught
    /// that seam — what makepad's own key focus says.
    ///
    /// Called at the end of the widget's draw and nowhere else. A field's
    /// rectangle is only its own frame's: read a moment later, from an
    /// event, the very same area answers with nothing. So the answer is
    /// taken where it is true and kept here until the next draw — which the
    /// bars want anyway, being drawn before the bodies that report, and one
    /// panel's bar while another panel's widget holds the caret.
    fn heard_field(&mut self, cx: &Cx, sh: &Shell, slot: SlotId, w: &WidgetRef, chord: &Chord) {
        // A widget that has not been taught the seam keeps the lot: each of
        // the fields in this build answers any cmd chord while its caret
        // blinks (`e2e/files/walk.txt` pins it), so no bar may promise one.
        // One that keeps less says so, and keeps its other letters bold.
        let keeps = chord.field_keeps().or_else(|| {
            let own = sh.overlay == Overlay::None && field_focus(cx, w);
            own.then_some(Letters::ALL)
        });
        match keeps {
            Some(keeps) => {
                self.field_keeps.insert(slot, keeps);
            }
            None => {
                self.field_keeps.remove(&slot);
            }
        }
    }

    /// The letters the widget in `slot` keeps from every bar while one of its
    /// fields has the keyboard: nothing at all when none has.
    #[must_use]
    pub(super) fn field_letters(&self, slot: Option<SlotId>) -> Letters {
        slot.and_then(|s| self.field_keeps.get(&s))
            .copied()
            .unwrap_or(Letters::NONE)
    }

    /// Forwards an event to every live content widget with its own slot's
    /// props on the scope. Widgets gate themselves on areas and key focus.
    ///
    /// The widgets are collected first: one of them may navigate, and the
    /// map it was read from would then be mutated under the walk.
    pub(super) fn forward_to_hosted(&mut self, cx: &mut Cx, sh: &mut Shell, event: &Event) {
        if self.hosted.is_empty() {
            return;
        }
        let live: Vec<(SlotId, WidgetRef)> = self
            .hosted
            .iter()
            .filter(|(slot, _)| !is_overlay(**slot))
            .map(|(slot, w)| (*slot, w.clone()))
            .collect();
        for (slot, w) in live {
            self.forward_one(cx, sh, slot, &w, event);
        }
    }

    /// Keys and text go to the focused panel's widget alone: the pointer is
    /// positional, but the keyboard belongs to one panel. Answers whether
    /// the widget took the chord.
    pub(super) fn forward_to_focused(
        &mut self,
        cx: &mut Cx,
        sh: &mut Shell,
        event: &Event,
    ) -> bool {
        let Some(f) = sh.session.focus() else {
            return false;
        };
        self.forward_to_slot(cx, sh, f, event)
    }

    /// The same, to a named slot — what a chord offered to a preview needs.
    pub(super) fn forward_to_slot(
        &mut self,
        cx: &mut Cx,
        sh: &mut Shell,
        slot: SlotId,
        event: &Event,
    ) -> bool {
        let Some(w) = self.hosted.get(&slot).cloned() else {
            return false;
        };
        self.forward_one(cx, sh, slot, &w, event)
    }

    /// One widget, one event. Answers whether it took the chord.
    fn forward_one(
        &mut self,
        cx: &mut Cx,
        sh: &mut Shell,
        slot: SlotId,
        w: &WidgetRef,
        event: &Event,
    ) -> bool {
        let Some(panel) = sh.session.panel(slot) else {
            return false;
        };
        let props = PanelProps {
            slot,
            panel,
            hits: self.hits.clone(),
            chord: Chord::default(),
        };
        let mut scope = Scope::with_data_props(&mut sh.session, &props);
        w.handle_event(cx, event, &mut scope);
        props.chord.taken()
    }

    /// Whether the focused slot has a hosted widget at all — every panel
    /// does, so in practice this is "a panel is focused".
    pub(super) fn hosted_focus(&self, sh: &Shell) -> bool {
        sh.session
            .focus()
            .is_some_and(|f| self.hosted.contains_key(&f))
    }

    /// Drops the widgets of slots that have closed. The session has already
    /// let their instances go; this is the Makepad half of the same step.
    pub(super) fn prune_hosted(&mut self, sh: &Shell) {
        let live: std::collections::HashSet<SlotId> =
            sh.session.panels().iter().map(|(s, _)| *s).collect();
        self.hosted
            .retain(|slot, _| is_overlay(*slot) || live.contains(slot));
        self.hosted_for.retain(|slot, _| live.contains(slot));
        self.field_keeps.retain(|slot, _| live.contains(slot));
    }
}

/// Whether a field inside this widget has the keyboard, as makepad sees it —
/// the answer for a widget that has not been taught to say so itself.
///
/// One area owns the keyboard at a time, and it is a field of this widget's
/// when it is neither nothing, nor the widget's own root — where a panel
/// parks the keyboard while its rows have it — nor anything outside the
/// rectangle the widget drew into: the stage parks the keyboard on *itself*
/// whenever a click lands on anything but a panel's own widget.
///
/// Both rectangles must be of the pass that just ran, which is why this is
/// asked at the end of a draw. An area not drawn since gives none at all.
fn field_focus(cx: &Cx, w: &WidgetRef) -> bool {
    let (focus, root) = (cx.key_focus(), w.area());
    if focus == Area::Empty || focus == root {
        return false;
    }
    let (body, field) = (root.rect(cx), focus.rect(cx));
    field.size.y > 0.0 && body.contains(field.center())
}

/// The widgets a stage is holding, by slot.
pub type Hosted = HashMap<SlotId, WidgetRef>;
