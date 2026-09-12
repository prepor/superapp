//! *device sync*: this device, its ticket, and the devices it knows.
//!
//! Device sync is not an app — it carries every app's declared tables — so
//! its panel is the shell's, drawn by the shell's own app like every other
//! panel here. It owns no state beyond three strings: what the name field
//! and the *pair with* field say, and the pairing window it opened. The
//! rest is the service's snapshot, read on every draw.
//!
//! The window opens with the panel and closes with it, which is the whole
//! of what makes a ticket safe to paste anywhere: it names sixteen random
//! bytes that are worthless the moment this panel goes.

use std::any::Any;

use kernel::caps::Clip;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::sync::{Pairing, SyncStatus};
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

/// The two fields, in the order tab walks them.
const FIELDS: [&[LiveId]; 2] = [ids!(name_input), ids!(pair_input)];

/// What a script — and a finger — addresses each by.
const LABELS: [&str; 2] = ["device name", "pair with"];

/// What the ticket line says when there is no endpoint to show one for.
const NOT_RUNNING: &str = "not running — this run has no endpoint";

/// What it says while the endpoint is still finding its way out.
const CONNECTING: &str = "connecting…";

/// The panel. The instance owns the two fields' text: the bar's *pair* and
/// the field's own enter reach the same string.
pub struct Sync {
    id: PanelId,
    /// This device's name, as the field has it.
    name: String,
    /// What was pasted into *pair with*.
    pair: String,
    /// The pairing window this panel opened. Dropping it closes the window,
    /// which is what closing the panel does.
    pairing: Option<Pairing>,
}

impl Sync {
    pub const TAG: Tag = Tag("sync");

    /// The identity of the one device-sync panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// Opening it asks for a ticket; closing it takes the ticket back.
    fn opened(id: &PanelId, cx: &Opening<'_>) -> Sync {
        Sync {
            id: id.clone(),
            name: String::new(),
            pair: String::new(),
            pairing: cx.session().sync_begin_pairing(),
        }
    }

    /// What the widget writes back on every keystroke.
    pub fn edited(&mut self, name: String, pair: String) {
        self.name = name;
        self.pair = pair;
    }

    /// Files this device's name, on enter or when the field is left.
    fn rename(&self, s: &mut Session) {
        let name = self.name.trim();
        if s.sync_status().is_some_and(|st| st.name == name) {
            return;
        }
        s.sync_rename(name);
    }

    /// Pairs with what was pasted. Answers whether the field should be
    /// cleared — a ticket that took is one nobody needs to look at again.
    fn pair_with(&mut self, s: &mut Session) -> bool {
        if self.pair.trim().is_empty() {
            s.notify("paste the other device's ticket first", true);
            return false;
        }
        match s.sync_pair(self.pair.trim()) {
            Ok(()) => {
                s.notify("pairing with that device", false);
                self.pair.clear();
                true
            }
            Err(e) => {
                s.notify(e, true);
                false
            }
        }
    }

    /// Puts the ticket on the system clipboard, through the effect
    /// boundary, so a scripted run copies into memory.
    fn copy(&self, s: &mut Session) {
        let ticket = s.sync_status().map(|st| st.ticket).unwrap_or_default();
        if ticket.is_empty() {
            s.notify("there is no ticket to copy yet", true);
            return;
        }
        s.run_effect(
            Clip {
                text: ticket,
                what: "the pairing ticket",
            },
            |s, result| match result {
                Ok(()) => s.notify("the ticket is on the clipboard", false),
                Err(e) => s.notify(e, true),
            },
        );
    }
}

/// Closing the panel closes the pairing window. The guard the open took out
/// goes with the instance, and the ticket it showed is worthless from that
/// moment — which is what makes a ticket safe to paste into a chat.
impl Drop for Sync {
    fn drop(&mut self) {
        drop(self.pairing.take());
    }
}

impl Panel for Sync {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "device sync".into()
    }

    /// What the panel is for, and what a ticket is.
    fn about(&self) -> String {
        "This device and the devices it syncs with: its name, which travels \
         with the roster, its short id, and the **ticket** another device \
         pastes to pair with it — an address plus sixteen random bytes, made \
         when this panel opened and worthless once it closes. *pair with* \
         takes a ticket the other way, from a device that is showing one. \
         Each peer is a row: what it calls itself, its short id, and whether \
         it is connected, when it was last heard from, or why the last \
         attempt failed. *forget* drops one on both devices. It takes no \
         arguments, and what replicates is decided by the apps, not here."
            .into()
    }

    /// A form over a list: as wide as a ticket reads in two lines.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (5, 4)
    }

    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::run("system.copy_ticket", "copy", Some('c')),
            Verb::run("system.pair", "pair", Some('p')),
        ]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "system.copy_ticket" => self.copy(s),
            "system.pair" => {
                self.pair_with(s);
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct SyncKind;

impl PanelKind for SyncKind {
    fn tag(&self) -> Tag {
        Sync::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Sync::opened(id, cx))
    }
}

/// The widget: the name field seeded once from the first snapshot with an
/// identity in it, the rest of that snapshot read on every draw, and one row
/// per peer.
#[derive(Script, ScriptHook, Widget)]
pub struct SyncPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Whether the name field has been seeded. Once, on the first draw the
    /// service had published an identity for; after that the text is the
    /// operator's, empty included.
    #[rust]
    primed: bool,
    /// Where each row's *forget* landed, as the last draw left it: a
    /// portal-list item's own area goes stale the moment a mid-gesture
    /// redraw does.
    #[rust]
    forgets: Vec<(String, Rect)>,
}

impl SyncPanel {
    /// What the two fields hold right now.
    fn values(&self, cx: &mut Cx) -> (String, String) {
        let [name, pair] = FIELDS.map(|p| self.view.text_input(cx, p));
        (name.text(), pair.text())
    }

    /// Copies the fields into the instance, so the bar's verbs act on what
    /// is on screen.
    fn write_back(&self, cx: &mut Cx, props: &PanelProps) {
        let (name, pair) = self.values(cx);
        if let Some(s) = props.panel.borrow_mut().as_any().downcast_mut::<Sync>() {
            s.edited(name, pair);
        }
    }

    /// Runs one of the panel's own verbs with the session, on the values
    /// the fields have now.
    fn act(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, verb: &'static str) {
        self.write_back(cx, props);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        props.panel.borrow_mut().run(verb, session);
        self.view.redraw(cx);
    }

    /// Files the name the field has now.
    fn rename(&self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        self.write_back(cx, props);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        if let Some(s) = props.panel.borrow_mut().as_any().downcast_ref::<Sync>() {
            s.rename(session);
        }
    }
}

impl Widget for SyncPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            self.view.handle_event(cx, event, scope);
            return;
        };
        let fields = FIELDS.map(|p| self.view.text_input(cx, p));
        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            for f in &fields {
                // A blurred field keeps no selection (the frameworks' norm).
                if f.key_focus_lost(actions) {
                    f.set_cursor(cx, f.cursor(), false);
                }
            }
            // The name is filed when it is finished with: enter, or the
            // moment the field is left — but never before it was seeded,
            // which would file the empty field over a real name.
            if self.primed
                && (fields[0].returned(actions).is_some() || fields[0].key_focus_lost(actions))
            {
                self.rename(cx, &props, scope);
            }
            if fields[1].returned(actions).is_some() {
                self.act(cx, &props, scope, "system.pair");
                return;
            }
            if fields.iter().any(|f| f.changed(actions).is_some()) {
                self.write_back(cx, &props);
            }
        }

        // Tab walks the two, wrapping.
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let at = fields.iter().position(|f| f.key_focus(cx));
                let n = fields.len();
                let next = match (at, k.modifiers.shift) {
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                    (None, _) => 0,
                };
                fields[next].set_key_focus(cx);
                if let Some(mut t) = fields[next].borrow_mut() {
                    t.select_all(cx);
                }
                self.view.redraw(cx);
            }
        }

        // A row's *forget*, answered by where the last draw put it.
        if let Event::MouseDown(e) = event {
            if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
                return;
            }
            let Some((device, _)) = self.forgets.iter().rev().find(|(_, r)| r.contains(e.abs))
            else {
                return;
            };
            let device = device.clone();
            if let Some(session) = scope.data.get_mut::<Session>() {
                session.sync_forget(&device);
                session.notify("that device is forgotten", false);
            }
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let (status, now) = match scope.data.get_mut::<Session>() {
            Some(s) => (s.sync_status(), s.now()),
            None => (None, 0.0),
        };
        // The name comes off the first snapshot that has an identity in it,
        // not off the open: the service publishes one a moment after the
        // panel is up, and a field seeded before then would be an empty
        // name to file the moment it was left.
        if !self.primed {
            if let Some(st) = status.as_ref().filter(|st| !st.device.is_empty()) {
                self.primed = true;
                self.view.text_input(cx, FIELDS[0]).set_text(cx, &st.name);
                if let Some(s) = props.panel.borrow_mut().as_any().downcast_mut::<Sync>() {
                    s.name.clone_from(&st.name);
                }
            }
        }

        // A ticket that was taken comes off the screen. The instance is
        // what cleared it — the bar's *pair* reaches the panel and not this
        // widget — so the field follows it here rather than at the press.
        let taken = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_ref::<Sync>()
            .is_some_and(|s| s.pair.is_empty());
        let field = self.view.text_input(cx, FIELDS[1]);
        if taken && !field.text().is_empty() {
            field.set_text(cx, "");
        }

        // This device, and the ticket it is showing. An endpoint that never
        // bound says why in the ticket's place, because there will be no
        // ticket; a *pair with* that failed says so under the field it was
        // pasted into, and the ticket stays where it is.
        let ticket = match &status {
            None => NOT_RUNNING.to_string(),
            Some(st) if !st.online => match st.note.is_empty() {
                true => NOT_RUNNING.to_string(),
                false => st.note.clone(),
            },
            Some(st) if st.ticket.is_empty() => CONNECTING.to_string(),
            Some(st) => st.ticket.clone(),
        };
        let note = match &status {
            Some(st) if st.online => st.note.clone(),
            _ => String::new(),
        };
        let said = self.view.text_input(cx, ids!(note_lbl));
        said.set_visible(cx, !note.is_empty());
        said.set_text(cx, &note);
        let device = status.as_ref().map_or(String::new(), |st| SyncStatus::short(&st.device));
        self.view.label(cx, ids!(id_lbl)).set_text(cx, &device);
        self.view.text_input(cx, ids!(ticket_lbl)).set_text(cx, &ticket);
        let peers = status.map(|st| st.peers).unwrap_or_default();
        self.view
            .label(cx, ids!(none_lbl))
            .set_visible(cx, peers.is_empty());

        self.forgets.clear();
        let mut drawn: Vec<(usize, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, peers.len());
            while let Some(idx) = list.next_visible_item(cx) {
                let Some(p) = peers.get(idx) else { continue };
                let row = list.item(cx, idx, live_id!(peer_row));
                let name = if p.name.is_empty() {
                    SyncStatus::short(&p.device)
                } else {
                    p.name.clone()
                };
                row.text_input(cx, ids!(head.name_lbl)).set_text(cx, &name);
                // The id sits beside the name, and is the name when there
                // is none: a device that has not been named says its id
                // once rather than twice.
                let id = if p.name.is_empty() { String::new() } else { SyncStatus::short(&p.device) };
                row.label(cx, ids!(head.id_lbl)).set_text(cx, &id);
                row.text_input(cx, ids!(state_lbl))
                    .set_text(cx, &p.line(now));
                row.draw_all(cx, scope);
                drawn.push((idx, row));
            }
        }

        // The fields and the ticket, by the name a script calls them.
        for (p, label) in FIELDS.into_iter().zip(LABELS) {
            let r = self.view.text_input(cx, p).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }
        let r = self.view.text_input(cx, ids!(ticket_lbl)).area().rect(cx);
        if r.size.x > 0.0 {
            props.hits.add("ticket", r, MouseCursor::Text, props.slot);
        }
        // And each row's controls, once the rows have landed: a link's own
        // hit points where it drew, which is before the row's `Fill` name
        // deferred.
        for (idx, row) in drawn {
            let Some(p) = peers.get(idx) else { continue };
            let name = row.text_input(cx, ids!(head.name_lbl));
            let r = name.area().rect(cx);
            if r.size.x > 0.0 && !name.text().is_empty() {
                props.hits.add(name.text(), r, MouseCursor::Text, props.slot);
            }
            let r = row.widget(cx, ids!(head.forget)).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add("forget", r, MouseCursor::Hand, props.slot);
                self.forgets.push((p.device.clone(), r));
            }
        }
        DrawStep::done()
    }
}
