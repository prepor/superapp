//! What a panel is: its identity, the factory that opens one, the live
//! instance, and the bar it wears.
//!
//! The kernel never reads an argument of a [`PanelId`]; their meaning and
//! spelling are the owning app's. What it needs is to compare them, hash
//! them, print them, and store them.

use std::any::Any;
use std::fmt;
use std::rc::Rc;

use crate::history::Intent;
use crate::layout::SlotId;
use crate::nav::Nav;
use crate::session::{Claim, Session, Write};

/// A kind's name. A plain static string, compared by content; never renamed
/// once written to a store.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Tag(pub &'static str);

impl Tag {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.0
    }

    /// A tag read back out of a store, where the spelling is a `String` and
    /// no app in this build need own it.
    ///
    /// Tags are compared by content, so an interned tag equals the
    /// `&'static str` its app declares — this only buys the `'static` the
    /// type asks for. One leak per distinct spelling for the life of the
    /// process, and the spellings come from a store's `panel.kind` column,
    /// so the set is as small as the panels a person has ever opened.
    #[must_use]
    pub fn intern(name: &str) -> Tag {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};
        static SEEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
        let mut seen = SEEN
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .expect("the tag interner");
        if let Some(s) = seen.get(name) {
            return Tag(s);
        }
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        seen.insert(leaked);
        Tag(leaked)
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// What a panel shows: the whole identity of it, as the tag of the kind
/// that opens it and the arguments that say which one. `Eq` and `Hash` are
/// all the layout needs: wishes are keyed by it and `showing` compares it.
/// The kernel never reads an argument; their meaning and spelling are the
/// owning app's. Stored as `panel(kind, args)`, the arguments as one JSON
/// array in a text column, readable in `sqlite3` and reachable with
/// `args ->> 0` should a query ever want one.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PanelId {
    pub tag: Tag,
    pub args: Vec<String>,
}

impl PanelId {
    /// An identity from a tag and its arguments.
    #[must_use]
    pub fn new<S: Into<String>>(tag: Tag, args: impl IntoIterator<Item = S>) -> PanelId {
        PanelId {
            tag,
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// A tag with no arguments — most panels.
    #[must_use]
    pub fn bare(tag: Tag) -> PanelId {
        PanelId {
            tag,
            args: Vec::new(),
        }
    }

    /// Argument `i`, or `None` where the panel carries fewer.
    #[must_use]
    pub fn arg(&self, i: usize) -> Option<&str> {
        self.args.get(i).map(String::as_str)
    }

    /// The arguments as the store keeps them: one JSON array of strings.
    /// Always a valid array, so the column can carry a `json_valid` check
    /// and `args ->> 0` reads.
    #[must_use]
    pub fn args_json(&self) -> String {
        serde_json::to_string(&self.args).unwrap_or_else(|_| "[]".to_string())
    }

    /// The identity a stored row names. `None` for text that is not a JSON
    /// array of strings — a row another build wrote in some other shape is
    /// skipped, not guessed at.
    #[must_use]
    pub fn from_row(tag: Tag, args_json: &str) -> Option<PanelId> {
        let args: Vec<String> = serde_json::from_str(args_json).ok()?;
        Some(PanelId { tag, args })
    }
}

impl fmt::Display for PanelId {
    /// `inbox`, `message(42)`, `attachment(42, 3)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag.0)?;
        if self.args.is_empty() {
            return Ok(());
        }
        write!(f, "({})", self.args.join(", "))
    }
}

/// The factory for one tag: what an app registers. It opens instances and
/// nothing else; everything a panel knows lives on the instance.
pub trait PanelKind: Sync + Send {
    /// The persisted spelling. Unique across the app list.
    fn tag(&self) -> Tag;

    /// A live instance for `id`. Runs inside the action that is opening,
    /// replacing, or previewing the panel, so what the open claims of the
    /// world (mail marks the thread read) is added through `cx` and lands
    /// on the same undoable node as the layout change. Also runs at
    /// session restore, with a `cx` that takes no claims.
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel>;
}

/// Why a panel is being opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// A solid link, or the launcher: a new slot that takes focus.
    Open,
    /// A dotted link: the slot shows something else in place.
    Replace,
    /// A cursor walk: a new slot beside the driver, focus staying put.
    Preview,
    /// Boot, off the saved session. Claims are ignored.
    Restore,
}

impl Open {
    /// Whether an open of this sort may claim anything of the world.
    #[must_use]
    pub fn claims(self) -> bool {
        !matches!(self, Open::Restore)
    }
}

/// What an opening panel may reach and claim.
pub struct Opening<'a> {
    session: &'a Session,
    how: Open,
    /// The writes and intents the open asked for, collected for the action
    /// that is running.
    pub(crate) claimed: Vec<Claim>,
}

impl<'a> Opening<'a> {
    /// An opening context over a session.
    pub(crate) fn new(session: &'a Session, how: Open) -> Opening<'a> {
        Opening {
            session,
            how,
            claimed: Vec::new(),
        }
    }

    /// The session, read-only: the store, the world, the apps, the layout.
    #[must_use]
    pub fn session(&self) -> &Session {
        self.session
    }

    /// Why the panel is being opened. A cursor walk previews; a solid link
    /// opens; a dotted link replaces; a restore is none of these.
    #[must_use]
    pub fn how(&self) -> Open {
        self.how
    }

    /// A write to run in the opening action's transaction, with the intents
    /// that reverse it. Ignored on restore. Consecutive previews from one
    /// slot coalesce into one node, so a cursor walk is one undo.
    pub fn claim(&mut self, write: Write, intents: Vec<Box<dyn Intent>>) {
        if !self.how.claims() {
            return;
        }
        self.claimed.push((write, intents));
    }

    /// Whether anything was claimed — what [`Nav`] reads to decide the
    /// history kind of a replace.
    #[must_use]
    pub fn claimed(&self) -> bool {
        !self.claimed.is_empty()
    }
}

/// One panel in one slot. Owns its state between draws: its table, its
/// cursor and marks, which messages are open, what it measured, the text of
/// its fields. The widget that draws it borrows it from the scope and calls
/// its methods on input; a data change is the instance writing through the
/// store it was opened with. Apps downcast their own instances through
/// `as_any`.
pub trait Panel: Any {
    fn id(&self) -> &PanelId;

    /// The header, the tab strip, the launcher label, and the action labels
    /// (*open "…"*). Called on every draw, so it reads only cached queries
    /// or its own state.
    fn title(&self) -> String;

    /// The size the panel asks for, width and height in grid units, given
    /// the column's width in characters. Constant for most kinds; a letter
    /// or a file card asks for the rows its content needs. The layout
    /// clamps it to the active grid. Asked on every relayout, so a measure
    /// that costs anything is taken once and remembered on the instance.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        crate::layout::DEFAULT_WISH
    }

    /// The bar at the panel's foot, left to right: the buttons that act on
    /// what the panel shows and the links that go somewhere from it, and,
    /// while the panel's table has marks, the batch verbs over the marked
    /// set with their count. Pulled on every draw, so a panel whose bar
    /// changed only has to ask for a redraw. The header wears nothing but
    /// the title and the close button.
    fn verbs(&self) -> Vec<Verb> {
        Vec::new()
    }

    /// The slot this instance landed in, told to it once the layout has
    /// run. An open cannot say: the slot does not exist until the action's
    /// layout half has placed it.
    ///
    /// Only a panel that has to name its own slot needs it — a
    /// [`VerbAct::Go`] carries a [`Nav`] and every `Nav` names a slot, and
    /// a verb that closes its own panel says `wm.close(slot)` in its
    /// action's layout half. Everything else ignores it.
    fn placed(&mut self, _slot: SlotId) {}

    /// The identity to save in the session for this instance. A job panel
    /// on an in-memory effect saves as the effects list, because ring ids
    /// do not survive the process.
    fn persist(&self) -> PanelId {
        self.id().clone()
    }

    /// One of this panel's own verbs was pressed, or its chord struck: the
    /// verb by its id, with the session to act on. The instance holds
    /// `&mut self` throughout, so reading its own table and then acting is
    /// one method; `act` and `nav` never touch instances (see
    /// [`settle`](Session::settle)).
    fn run(&mut self, _verb: &str, _s: &mut Session) {}

    fn as_any(&mut self) -> &mut dyn Any;
}

/// One entry of a panel's bar. Two entries are the same verb when they
/// carry the same `id`, which is how a test checks that a batch verb and
/// its single-row twin wear one letter.
pub struct Verb {
    /// Stable and prefixed (`mail.archive`): what history labels, the e2e
    /// harness, and tests name it by.
    pub id: &'static str,
    /// The text drawn. Contains `accel` when there is one; the letter is
    /// drawn bold. A batch verb says so in its label (*archive 3*).
    pub label: String,
    /// The letter, or none. Never one of the workspace's reserved chords;
    /// unique within one bar. The bar asserts both in debug builds; each
    /// app tests its own bars.
    pub accel: Option<char>,
    pub act: VerbAct,
}

impl Verb {
    /// A button of the panel's own: pressing it calls [`Panel::run`] with
    /// this id.
    #[must_use]
    pub fn run(id: &'static str, label: impl Into<String>, accel: Option<char>) -> Verb {
        Verb {
            id,
            label: label.into(),
            accel,
            act: VerbAct::Run,
        }
    }

    /// A link.
    #[must_use]
    pub fn go(id: &'static str, label: impl Into<String>, accel: Option<char>, nav: Nav) -> Verb {
        Verb {
            id,
            label: label.into(),
            accel,
            act: VerbAct::Go(nav),
        }
    }

    /// A button that belongs to no panel: a problem row's *retry*. The
    /// closure is the whole behaviour.
    #[must_use]
    pub fn call(
        id: &'static str,
        label: impl Into<String>,
        accel: Option<char>,
        f: impl Fn(&mut Session) + 'static,
    ) -> Verb {
        Verb {
            id,
            label: label.into(),
            accel,
            act: VerbAct::Call(Rc::new(f)),
        }
    }
}

impl fmt::Debug for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Verb")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("accel", &self.accel)
            .finish_non_exhaustive()
    }
}

/// What a [`Verb`] does when its button is clicked or its letter typed.
pub enum VerbAct {
    /// A button of the panel's own: the bar calls [`Panel::run`] with the
    /// verb's id on click or chord.
    Run,
    /// A link: navigates on click or chord. Drawn as a link, not a button,
    /// so the three signals of the interaction grammar still hold.
    Go(Nav),
    /// A button that belongs to no panel: a problem row's *retry*. The
    /// closure is the whole behaviour.
    Call(Rc<dyn Fn(&mut Session)>),
}

impl VerbAct {
    /// Whether the entry is drawn as a button. Everything that acts is;
    /// only a [`VerbAct::Go`] is a link.
    #[must_use]
    pub fn button(&self) -> bool {
        !matches!(self, VerbAct::Go(_))
    }
}

/// The instance a restored slot gets when no app in this build owns its
/// tag. The slot is kept, not dropped: another build has the app, and the
/// session is shared.
pub struct Missing {
    id: PanelId,
}

impl Missing {
    #[must_use]
    pub fn new(id: PanelId) -> Missing {
        Missing { id }
    }

    /// The line the card shows under the tag.
    #[must_use]
    pub fn line() -> &'static str {
        "no app for this panel in this build"
    }
}

impl Panel for Missing {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        self.id.to_string()
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// The kind a slot with no owner is opened by. Not registered: the registry
/// answers `None` for its tag, and the session reaches for this instead.
#[must_use]
pub fn missing(id: &PanelId) -> Box<dyn Panel> {
    Box::new(Missing::new(id.clone()))
}

/// The slot a verb belongs to, in the `action.entity` vocabulary. One
/// spelling for the coalescing scope of a cursor walk, a move, and a
/// worker's kick address.
#[must_use]
pub fn slot_entity(slot: SlotId) -> String {
    format!("slot:{slot}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Tag = Tag("message");

    #[test]
    fn an_identity_prints_its_arguments() {
        assert_eq!(PanelId::bare(Tag("inbox")).to_string(), "inbox");
        assert_eq!(PanelId::new(T, ["42"]).to_string(), "message(42)");
        assert_eq!(
            PanelId::new(Tag("attachment"), ["42", "3"]).to_string(),
            "attachment(42, 3)"
        );
    }

    #[test]
    fn arguments_round_trip_through_json() {
        for args in [
            vec![],
            vec!["42".to_string()],
            vec!["forward".to_string(), "42".to_string()],
            vec![
                "a \"quoted\" one".to_string(),
                "~/Downloads/2026".to_string(),
            ],
        ] {
            let id = PanelId {
                tag: T,
                args: args.clone(),
            };
            let json = id.args_json();
            assert_eq!(PanelId::from_row(T, &json), Some(id));
        }
        assert_eq!(PanelId::bare(T).args_json(), "[]");
        // A row in some other shape is skipped, not guessed at.
        assert_eq!(PanelId::from_row(T, "not json"), None);
        assert_eq!(PanelId::from_row(T, "{\"a\":1}"), None);
    }

    #[test]
    fn a_missing_panel_says_so_and_persists_unchanged() {
        let id = PanelId::new(Tag("from_the_future"), ["7"]);
        let mut m = Missing::new(id.clone());
        assert_eq!(m.title(), "from_the_future(7)");
        assert_eq!(m.persist(), id);
        assert_eq!(m.wish(80), crate::layout::DEFAULT_WISH);
        assert!(m.verbs().is_empty());
        assert!(m.as_any().is::<Missing>());
    }

    #[test]
    fn a_restore_takes_no_claims() {
        assert!(!Open::Restore.claims());
        for how in [Open::Open, Open::Replace, Open::Preview] {
            assert!(how.claims());
        }
    }
}
