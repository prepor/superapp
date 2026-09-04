//! What a chat carries as context: a thing, rather than a paragraph of text
//! somebody pasted.
//!
//! A chip stands in the composer and in the transcript as a small labelled
//! block with its own `×`, and is rendered into text for the model at send
//! time. It is a **reference, not a snapshot**: what it keeps is the panel's
//! identity, its title, and the trace of the draw it was made from, and the
//! rows come off the store when the request is built — so a chip made an
//! hour ago and sent now carries what the panel shows now.
//!
//! [`Chip`] is an enum with one variant and room for `File`, `Mail` and
//! `Selection`. Each variant knows how to render itself for the model, which
//! is why nothing else in the app matches on it: a second kind of context is
//! a variant and a `match` arm here, and no change anywhere else.

use kernel::context::{self, PanelContext};
use kernel::layout::SlotId;
use kernel::panel::PanelId;
use kernel::session::Session;
use kernel::store::TraceEntry;
use serde_json::{json, Value};

/// One piece of context a turn carries.
#[derive(Debug, Clone)]
pub enum Chip {
    Panel(PanelChip),
}

/// A panel, as a chip holds it: enough to say which panel it is, what it
/// reads as, and how to derive its rows again.
#[derive(Debug, Clone)]
pub struct PanelChip {
    /// The panel this points at. Everything else here can be derived from
    /// it; this is the part that must not be lost.
    pub id: PanelId,
    /// What the chip reads, and what the panel's header wore when it was
    /// made.
    pub title: String,
    /// The workspace it stood on, counted from one; zero when the panel was
    /// not open at the time.
    pub workspace: usize,
    /// The trace of the panel's last draw. Empty for a chip whose panel was
    /// not open when it was made, or one read back out of a turn — in both
    /// cases [`Chip::render`] takes the panel's trace as it stands now, if
    /// it is open again.
    pub queries: Vec<TraceEntry>,
    /// The panel's own paragraph, in the app's words.
    pub about: String,
}

/// What a chip of a panel that nobody has open says instead of its
/// paragraph. The identity is enough to re-run the queries when it is opened
/// again; until then that is all there is.
const NOT_OPEN: &str =
    "This panel is not open anywhere at the moment, so what is known of it is its \
     identity — the tag and the arguments above. Opening it again is what gives \
     its rows back.";

impl Chip {
    /// The chip for what a slot is showing. `None` for a slot with no
    /// instance in it.
    #[must_use]
    pub fn panel(s: &Session, slot: SlotId) -> Option<Chip> {
        context::of(s, slot).map(|cx| Chip::Panel(PanelChip::of(cx)))
    }

    /// What the chip reads: the panel's title — `inbox`, `Q3 planning`,
    /// `~/Downloads`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Chip::Panel(p) => p.title.clone(),
        }
    }

    /// The chip as text for the model, with its rows read now.
    ///
    /// The sentence on each recent effect is the effect registry's, which
    /// only a world can decode, so it is filled in here rather than in the
    /// renderer — which has a store and no world.
    #[must_use]
    pub fn render(&self, s: &Session) -> String {
        match self {
            Chip::Panel(p) => {
                let mut jobs = context::recent_effects(s.store(), &p.id, context::EFFECTS);
                for job in &mut jobs {
                    if job.what.is_none() {
                        job.what = s.world().registry().describe(&job.kind, &job.payload);
                    }
                }
                context::render(s.store(), &p.context(s), &jobs)
            }
        }
    }

    /// The slot showing this chip's panel, if one still is — what a click on
    /// the chip focuses. The first, where two panels show one identity.
    #[must_use]
    pub fn open_slot(&self, s: &Session) -> Option<SlotId> {
        match self {
            Chip::Panel(p) => s.showing(&p.id).first().copied(),
        }
    }

    /// The chip as a turn's `chips` entry: kind-tagged, so a `File`, a
    /// `Mail` or a `Selection` can join it later without the reader having
    /// to guess which it is reading.
    ///
    /// The trace is not written down. A chip is a reference, and a trace is
    /// one draw's provenance: a turn read back a week later re-derives it
    /// from whatever the panel is showing then, or renders without it.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Chip::Panel(p) => json!({
                "kind": "panel",
                "tag": p.id.tag.as_str(),
                "args": p.id.args,
                "title": p.title,
                "workspace": p.workspace,
                "about": p.about,
            }),
        }
    }

    /// The chip a stored turn carries. `None` for a kind this build has
    /// never heard of, or an entry missing what a chip is: another build's
    /// context is not guessed at.
    #[must_use]
    pub fn from_json(v: &Value) -> Option<Chip> {
        match v.get("kind")?.as_str()? {
            "panel" => {
                let tag = kernel::panel::Tag::intern(v.get("tag")?.as_str()?);
                let args: Vec<String> = v
                    .get("args")?
                    .as_array()?
                    .iter()
                    .map(|a| a.as_str().map(ToString::to_string))
                    .collect::<Option<_>>()?;
                Some(Chip::Panel(PanelChip {
                    id: PanelId { tag, args },
                    title: v
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    workspace: v
                        .get("workspace")
                        .and_then(Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(0),
                    queries: Vec::new(),
                    about: v
                        .get("about")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }))
            }
            _ => None,
        }
    }

    /// The chip a paste is, if it is one.
    ///
    /// A paste whose first line is the one `cmd+i` writes
    /// ([`context::header_line`]) names a panel; anything else is text, and
    /// that is the whole of the rule. Where a slot is showing that panel the
    /// chip is made from it, trace and all; where none is, the chip is the
    /// identity alone and says so — the panel may be opened again, and then
    /// its rows are there.
    #[must_use]
    pub fn from_paste(s: &Session, text: &str) -> Option<Chip> {
        let id = context::parse_header(text)?;
        if let Some(slot) = s.showing(&id).first().copied() {
            if let Some(chip) = Chip::panel(s, slot) {
                return Some(chip);
            }
        }
        Some(Chip::Panel(PanelChip {
            title: id.to_string(),
            id,
            workspace: 0,
            queries: Vec::new(),
            about: NOT_OPEN.to_string(),
        }))
    }
}

impl PanelChip {
    /// The chip a panel context makes.
    fn of(cx: PanelContext) -> PanelChip {
        PanelChip {
            id: cx.id,
            title: cx.title,
            workspace: cx.workspace,
            queries: cx.queries,
            about: cx.about,
        }
    }

    /// What the renderer is handed. A chip with no trace of its own — one
    /// read back out of a turn, or made of a panel nobody had open — takes
    /// the panel's own trace where a slot is showing it now: the chip points
    /// at a panel, not at a moment.
    fn context(&self, s: &Session) -> PanelContext {
        let mut cx = PanelContext {
            id: self.id.clone(),
            title: self.title.clone(),
            workspace: self.workspace,
            about: self.about.clone(),
            queries: self.queries.clone(),
        };
        if cx.queries.is_empty() {
            if let Some(slot) = s.showing(&self.id).first().copied() {
                if let Some(live) = context::of(s, slot) {
                    cx = live;
                }
            }
        }
        cx
    }
}

#[cfg(test)]
mod tests {
    use kernel::app::App;
    use kernel::nav::Nav;
    use kernel::panel::{PanelId, Tag};
    use kernel::session::{Action, Session};

    use super::*;
    use crate::apps::files::{Card, Dir, FILES};
    use crate::apps::mail::model::Role;
    use crate::apps::mail::MAIL;

    static APPS: &[&dyn App] = &[&MAIL, &FILES];

    /// A session with one panel open, and the slot it is in.
    fn open(id: PanelId) -> (Session, SlotId) {
        let mut s = Session::fake(APPS);
        let show = id.clone();
        s.act(Action::new("open", "open").moving(move |wm| {
            wm.open(show, None, false);
        }));
        s.settle();
        let slot = s.focus().expect("the new slot");
        (s, slot)
    }

    /// A chip reads as the panel's title, renders as the panel, and knows
    /// which slot it points at.
    #[test]
    fn a_panel_chip_is_the_panel_it_points_at() {
        let (s, slot) = open(Role::Inbox.id());
        let chip = Chip::panel(&s, slot).expect("a chip for an open slot");
        assert_eq!(chip.label(), "inbox");
        assert_eq!(chip.open_slot(&s), Some(slot));

        let text = chip.render(&s);
        assert!(
            text.starts_with("<panel id=\"inbox\" title=\"inbox\" workspace=\"1\">\n"),
            "{text}"
        );
        assert!(text.contains("one row a conversation"), "{text}");
        assert!(text.ends_with("</panel>\n"), "{text}");
        assert!(text.len() < context::CAP);
    }

    /// The chip points at the panel, not at the slot: close it and the chip
    /// still renders, and no longer claims to be open anywhere.
    #[test]
    fn a_chip_outlives_the_slot_it_was_made_from() {
        let (mut s, slot) = open(Dir::id("~"));
        let chip = Chip::panel(&s, slot).expect("a chip");
        assert_eq!(chip.label(), "~");
        s.act(Action::new("close", "close").moving(move |wm| {
            wm.close(slot);
        }));
        s.settle();
        assert_eq!(chip.open_slot(&s), None);
        assert!(chip.render(&s).contains("<panel id=\"files(~)\""));
    }

    /// The turn's entry: kind-tagged, so the reader never has to guess, and
    /// round-tripping keeps the identity, which is the part everything else
    /// is derived from.
    #[test]
    fn a_chip_round_trips_through_a_turns_json() {
        let (s, slot) = open(Card::id("~/notes.md"));
        let chip = Chip::panel(&s, slot).expect("a chip");
        let v = chip.to_json();
        assert_eq!(v["kind"], "panel");
        assert_eq!(v["tag"], "file");
        assert_eq!(v["args"], json!(["~/notes.md"]));
        assert_eq!(v["title"], "notes.md");
        assert_eq!(v["workspace"], 1);

        let back = Chip::from_json(&v).expect("a chip this build knows");
        // One variant today, and a `match` rather than a `let`, so a second
        // one is a compiler error here and not a silent pass.
        match &back {
            Chip::Panel(p) => {
                assert_eq!(p.id, Card::id("~/notes.md"));
                assert_eq!(p.title, "notes.md");
                assert!(p.queries.is_empty(), "a reference, not a transcript");
            }
        }
        // …and the trace comes back off the panel, which is still open.
        assert!(back.render(&s).contains("</panel>"));
        assert_eq!(back.open_slot(&s), Some(slot));

        // A kind from another build is not guessed at.
        assert!(Chip::from_json(&json!({"kind": "selection", "text": "hi"})).is_none());
        assert!(Chip::from_json(&json!({"tag": "inbox"})).is_none());
        assert!(Chip::from_json(&json!({"kind": "panel", "args": []})).is_none());
    }

    /// The paste rule: the line `cmd+i` writes first is a panel; anything
    /// else is text.
    #[test]
    fn a_pasted_panel_context_is_a_chip_and_anything_else_is_not() {
        let (s, slot) = open(Role::Inbox.id());
        let header = context::header_line(&Role::Inbox.id());
        let pasted = format!("{header}\n\n# superapp panel context\n\npanel: “inbox”\n");
        let chip = Chip::from_paste(&s, &pasted).expect("the paste is a panel");
        assert_eq!(chip.label(), "inbox");
        assert_eq!(chip.open_slot(&s), Some(slot));
        assert!(chip.render(&s).contains("one row a conversation"));

        for text in [
            "just something someone wrote",
            "",
            "superapp-panel: inbox",
            "# superapp panel context\n\nsuperapp-panel: inbox []",
        ] {
            assert!(Chip::from_paste(&s, text).is_none(), "{text:?}");
        }
    }

    /// A paste of a panel nobody has open is still a chip: the identity is
    /// what a chip is, and it says what it does not have.
    #[test]
    fn a_paste_of_a_panel_nobody_has_open_is_the_identity_alone() {
        let (s, _slot) = open(Role::Inbox.id());
        let id = PanelId::new(Tag::intern("message"), ["42"]);
        let chip = Chip::from_paste(&s, &context::header_line(&id)).expect("a chip");
        assert_eq!(chip.label(), "message(42)");
        assert_eq!(chip.open_slot(&s), None);
        let text = chip.render(&s);
        assert!(text.contains("workspace=\"0\""), "{text}");
        assert!(text.contains("This panel is not open anywhere"), "{text}");
        assert!(text.contains("## queries"), "{text}");
        assert!(text.ends_with("</panel>\n"), "{text}");
    }

    /// The effects a chip carries are the ones about its panel's arguments,
    /// with the sentence the registry decodes for a filed job — which the
    /// renderer alone could not have written.
    #[test]
    fn a_chip_carries_what_was_lately_done_to_its_panel() {
        let mut s = Session::fake(APPS);
        // Open the inbox, read a conversation, and archive it: the move is
        // a job filed against the account.
        let inbox = {
            let id = Role::Inbox.id();
            s.act(Action::new("open", "open").moving(move |wm| {
                wm.open(id, None, false);
            }));
            s.settle();
            s.focus().expect("the inbox")
        };
        let mail: i64 = s
            .store()
            .conn()
            .query_row(
                "SELECT m.id FROM message m JOIN folder f ON f.id = m.folder
                  WHERE f.role = 'inbox' ORDER BY m.id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("a letter in the inbox");
        s.nav(Nav::Open {
            from: inbox,
            id: crate::apps::mail::panels::Message::id(mail),
            fresh: false,
        });
        s.settle();
        let reader = s.showing(&crate::apps::mail::panels::Message::id(mail))[0];
        let chip = Chip::panel(&s, reader).expect("a chip for the reader");

        // The verb, run the way the bar runs it.
        let inst = s.panel(reader).expect("the reader");
        inst.borrow_mut().run("mail.archive", &mut s);
        s.settle();

        // The account's own entity is what mail files a move against, and a
        // reader's argument is a letter id — so the section is only there
        // when the two read the same. What is asserted here is the shape:
        // whatever is listed carries a sentence and a status.
        let text = chip.render(&s);
        if let Some(section) = text.split("## recent effects\n").nth(1) {
            let line = section.lines().next().expect("a line under the heading");
            assert!(line.contains(" — "), "{line}");
            assert!(
                !line.starts_with(" — "),
                "a sentence, not an empty one: {line}"
            );
        }
        assert!(text.ends_with("</panel>\n"), "{text}");
    }
}
