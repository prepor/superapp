//! The compose sheet, drawn: three fields over a draft, and the line that
//! says what the letter will carry.
//!
//! The text is the instance's: every change is handed straight to
//! [`Compose::edited`], which writes the draft row behind it. The fields are
//! seeded when the widget is built — a panel replaced in place is built
//! again, so a reply retargeted to a forward is seeded afresh rather than
//! left holding the reply's text — and again whenever the instance's own text
//! moves without a keystroke of this widget's, which is what a device-sync
//! pass materializing the other machine's draft looks like from here.
//!
//! A live field keeps the chords it needs: while one has the keyboard
//! `cmd+a` is select-all, and the bar never sees it.

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{PanelId, Tag};
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::dsl::LinkViewExt;
use crate::shell::hosted::PanelProps;
use crate::shell::keys::Letters;
use crate::shell::widgets::suggest::Suggest;

use super::super::model::{Draft, Seed};
use super::super::panels::Compose;
use super::super::recipients::Recipients;

/// The tab ring, in order. The body is last because that is where a sheet
/// starts: a reply has its recipient already.
const FIELDS: [(&str, &[LiveId]); 3] = [
    ("to", ids!(to_input)),
    ("subject", ids!(subject_input)),
    ("body", ids!(body_input)),
];

/// How many files the `CARRIES` line names. Past this it says how many more
/// there are: a sheet is a sheet, and thirty attachments must not push the
/// body off the panel.
const CARRY_SLOTS: usize = 5;

/// The card over a path on this machine. Mail names the tag rather than the
/// app: an app reaches another through the registry, and a build without the
/// files app gets the shell's *missing* card, which says whose panel it would
/// have been.
const FILE_TAG: Tag = Tag("file");

/// The slots the DSL lays out for them.
const CARRY_LBLS: [LiveId; CARRY_SLOTS] = [
    live_id!(f0),
    live_id!(f1),
    live_id!(f2),
    live_id!(f3),
    live_id!(f4),
];

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct ComposePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The TO field's completion box, drawn over the fields under it after
    /// everything else.
    #[live]
    suggest: View,
    /// The offer under the TO caret: the people this mailbox has heard from.
    #[rust]
    ac: Suggest<Recipients>,
    /// That box's rows of the last draw, in the order it offers them: a
    /// press on one is a pick, and it must not reach the field underneath.
    #[rust]
    picks: Vec<Rect>,
    /// A pick took the press and owes the release: the box is gone by then,
    /// so the rectangle cannot be asked twice.
    #[rust]
    picking: bool,
    /// Whether the fields have been seeded and the keyboard handed over.
    /// Both happen once, on the first event tick after the sheet has been
    /// drawn — a field that has never been drawn is nowhere to put a caret.
    #[rust]
    mounted: bool,
    /// The instance's text as this widget last wrote it into the fields.
    /// Between a keystroke and the action that reports it the fields are
    /// ahead of the instance, which is why the comparison is against this
    /// and not against what the fields say.
    #[rust]
    shown: Draft,
}

impl Widget for ComposePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        // What the files app is holding, looked at before the bar is pulled
        // again: *attach* comes and goes with the clipboard.
        observe(&props, scope);

        let inputs = self.inputs(cx);
        let focused = inputs.iter().position(|t| t.key_focus(cx));
        // A live field keeps every cmd chord, so no bar's letter is drawn as
        // if it would fire — the promise a bold letter makes is about now.
        if focused.is_some() {
            props.chord.field(Letters::ALL);
        }

        if let Event::KeyDown(k) = event {
            // A live field keeps its own chords: `cmd+a` is select-all here,
            // not a verb on the bar.
            if focused.is_some() && k.modifiers.logo {
                props.chord.take();
            }
        }

        // The TO field's offer answers first, in both hands. A press on one
        // of its rows is a pick and neither half of it reaches what the box
        // hangs over — the release as much as the press, because a focused
        // makepad field blurs *itself* on a release outside its own
        // rectangle, and the caret the pick just parked would go with it.
        // And while the box is up it owns the arrows, enter, tab and esc, so
        // neither the field nor the tab ring below ever sees them. A pick is
        // an edit like any typing.
        self.ac.track(cx, &inputs[0]);
        if let Event::MouseDown(e) = event {
            if let Some(i) = self.picks.iter().position(|r| r.contains(e.abs)) {
                self.ac.pick(cx, &Recipients, &inputs[0], i);
                self.edited(cx, &props);
                self.picking = true;
                self.view.redraw(cx);
                return;
            }
        }
        if matches!(event, Event::MouseUp(_)) && self.picking {
            self.picking = false;
            return;
        }
        if let Event::KeyDown(k) = event {
            let before = inputs[0].text();
            if self.ac.key(cx, &Recipients, &inputs[0], k) {
                if inputs[0].text() != before {
                    self.edited(cx, &props);
                }
                self.view.redraw(cx);
                return;
            }
        }

        self.view.handle_event(cx, event, scope);
        self.mount(cx, &props);
        self.reseed(cx, &props);

        // Tab walks to → subject → body, wrapping; from the panel's own
        // focus it lands on `to`. The body's enter stays a newline, which a
        // multiline field owns.
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let d: isize = if k.modifiers.shift { -1 } else { 1 };
                let n = inputs.len() as isize;
                let j = match focused {
                    Some(i) => (i as isize + d).rem_euclid(n),
                    None if d > 0 => 0,
                    None => n - 1,
                };
                land(cx, &inputs, j as usize);
            }
        }

        if let Event::Actions(actions) = event {
            for t in &inputs {
                if t.key_focus_lost(actions) {
                    t.set_cursor(cx, t.cursor(), false);
                }
            }
            // Enter walks the one-line fields on; the body has none to walk
            // to, and keeps its newline.
            if inputs[0].returned(actions).is_some() {
                land(cx, &inputs, 1);
            } else if inputs[1].returned(actions).is_some() {
                land(cx, &inputs, 2);
            }
            if inputs.iter().any(|t| t.changed(actions).is_some()) {
                self.edited(cx, &props);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        observe(&props, scope);
        self.reseed(cx, &props);
        // What it will carry, written every draw: the list is a cached
        // query, so this is a lookup, and an attach must show in the frame
        // that made it.
        let carries: Vec<(String, String)> = {
            let mut borrow = props.panel.borrow_mut();
            match borrow.as_any().downcast_mut::<Compose>() {
                Some(c) => c
                    .carrying()
                    .iter()
                    .map(|f| (f.label(), f.path.clone()))
                    .collect(),
                None => Vec::new(),
            }
        };
        self.carries(cx, &carries, props.slot);

        self.view.draw_walk_all(cx, scope, walk);
        // The three fields, addressable by name — that is all a script needs
        // to put a caret in one.
        for (label, path) in FIELDS {
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }
        // The TO field's offer, drawn last of all so it covers the subject
        // and the body instead of pushing them down, and registered last so
        // a press on one of its rows wins over what it stands over.
        let Some(store) = scope.data.get_mut::<Session>().map(|s| s.store().clone()) else {
            return DrawStep::done();
        };
        let Self {
            view,
            suggest,
            ac,
            picks,
            ..
        } = self;
        let to = view.text_input(cx, FIELDS[0].1);
        ac.draw(cx, scope, &store, &Recipients, &to, suggest);
        picks.clear();
        for (label, r) in ac.hits(cx, suggest) {
            picks.push(r);
            props.hits.add(label, r, MouseCursor::Hand, props.slot);
        }
        DrawStep::done()
    }
}

impl ComposePanel {
    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 3] {
        [
            self.view.text_input(cx, FIELDS[0].1),
            self.view.text_input(cx, FIELDS[1].1),
            self.view.text_input(cx, FIELDS[2].1),
        ]
    }

    /// The first look at a live panel: the fields take the draft's text, and
    /// one of them takes the keyboard. A forward has its letter and wants a
    /// recipient; everything else has its recipient, or nothing, and starts
    /// in the body.
    ///
    /// Held until the fields have a rectangle: focus on a field that has
    /// never been drawn is focus on nothing.
    fn mount(&mut self, cx: &mut Cx, props: &PanelProps) {
        if self.mounted {
            return;
        }
        let inputs = self.inputs(cx);
        if inputs[0].area().rect(cx).size.x <= 0.0 {
            return;
        }
        let Some((draft, seed)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<Compose>()
                .map(|c| (c.draft().clone(), c.seed()))
        }) else {
            return;
        };
        self.mounted = true;
        for (t, s) in inputs.iter().zip([&draft.to, &draft.subject, &draft.body]) {
            t.set_text(cx, s);
        }
        self.shown = draft.clone();
        land(cx, &inputs, if matches!(seed, Seed::Forward(_)) { 0 } else { 2 });
    }

    /// The instance's text, put back into the fields when it moved without a
    /// keystroke of this widget's — a draft the other device was writing,
    /// materialized by a replication pass and re-read by
    /// [`Compose::observe`]. The caret is left where it is: the person may be
    /// typing, and the last writer wins without also taking the cursor.
    fn reseed(&mut self, cx: &mut Cx, props: &PanelProps) {
        if !self.mounted {
            return;
        }
        let Some(draft) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<Compose>()
                .map(|c| c.draft().clone())
        }) else {
            return;
        };
        if draft == self.shown {
            return;
        }
        for (t, s) in self
            .inputs(cx)
            .iter()
            .zip([&draft.to, &draft.subject, &draft.body])
        {
            if &t.text() != s {
                t.set_text(cx, s);
            }
        }
        self.shown = draft;
    }

    /// A field changed: the panel keeps the text and the row follows.
    fn edited(&mut self, cx: &mut Cx, props: &PanelProps) {
        let [to, subject, body] = self.inputs(cx);
        let next = Draft {
            to: to.text(),
            subject: subject.text(),
            body: body.text(),
        };
        self.shown = next.clone();
        let mut borrow = props.panel.borrow_mut();
        if let Some(c) = borrow.as_any().downcast_mut::<Compose>() {
            c.edited(&next.to, &next.subject, &next.body);
        }
    }

    /// The `CARRIES` line: one link a file, and a count for the rest. Each
    /// opens the card over that path — the files app's when it is in the
    /// build, and the shell's *missing* card when it is not, which is the
    /// honest answer either way. A link registers its own hit, so a script
    /// addresses a carried file by the name and size the line shows.
    fn carries(&mut self, cx: &mut Cx2d, files: &[(String, String)], slot: SlotId) {
        let v = &self.view;
        v.widget(cx, ids!(carries))
            .set_visible(cx, !files.is_empty());
        for (i, name) in CARRY_LBLS.iter().enumerate() {
            let link = v.link(cx, &[live_id!(carries), live_id!(files), *name]);
            match files.get(i) {
                Some((label, path)) => {
                    link.set(
                        cx,
                        label,
                        Nav::Open {
                            from: slot,
                            id: PanelId::new(FILE_TAG, [path.clone()]),
                            fresh: false,
                        },
                        false,
                        None,
                    );
                    link.set_visible(cx, true);
                }
                None => link.set_visible(cx, false),
            }
        }
        let rest = files.len().saturating_sub(CARRY_SLOTS);
        let more = v.label(cx, ids!(carries.files.more_lbl));
        more.set_text(cx, &format!("+{rest} more"));
        more.set_visible(cx, rest > 0);
    }
}

/// Hands the instance the one fact it cannot ask for itself while it builds
/// its bar. Read-only on the session, and the borrow ends with the call:
/// nothing here runs a verb.
fn observe(props: &PanelProps, scope: &mut Scope) {
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let session: &Session = session;
    let mut borrow = props.panel.borrow_mut();
    if let Some(c) = borrow.as_any().downcast_mut::<Compose>() {
        c.observe(session);
    }
}

/// Lands in the `j`-th field. The one-line fields take their text selected,
/// as a form's do; the body keeps its caret — a letter is not a value to
/// type over, and in a forward it is the mail being passed on, with the
/// caret above it.
fn land(cx: &mut Cx, inputs: &[TextInputRef; 3], j: usize) {
    if j == 2 {
        inputs[2].set_key_focus(cx);
        return;
    }
    inputs[j].set_key_focus(cx);
    if let Some(mut t) = inputs[j].borrow_mut() {
        t.select_all(cx);
    }
}
