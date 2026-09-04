//! The compose sheet: a draft, what it will carry, its send window, and the
//! two ways out of it.
//!
//! A draft belongs to the slot it is written in — slot ids are stable and
//! persisted — so half-written text survives a restart, and the outbox row a
//! send files shares that id: one pending send per compose, and an undo
//! entity (`outbox:N`) that exists before the row does.
//!
//! *attach* is the one verb here that reaches another app: it appears while
//! the files app is in the build and holding something, and it is the files
//! clipboard's other destination.

use std::any::Any;
use std::rc::Rc;

use kernel::effect::World;
use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::{Action, Session};
use kernel::store::Store;

use crate::apps::files::Files;

use super::super::carry::{self, DraftFile};
use super::super::effects::{outbox_entity, Attached, Discarded, Sent};
use super::super::model::{self, Draft, Seed};

/// A compose panel.
pub struct Compose {
    id: PanelId,
    seed: Seed,
    /// The world it was opened in: the store it writes its row through, and
    /// the clock that stamps it.
    world: Rc<World>,
    slot: SlotId,
    /// The text as it stands. The row is written behind it, on every edit.
    draft: Draft,
    /// What the files app was holding when the widget last looked.
    /// [`Panel::verbs`] has no session, so the panel is told rather than
    /// asking: the widget calls [`Compose::observe`] at the top of every
    /// draw and event, and the bar reads the snapshot.
    held: Vec<String>,
}

impl Compose {
    pub const TAG: Tag = Tag("compose");

    /// The identity of a compose on this seed: `[]`, `["reply", id]`, or
    /// `["forward", id]`.
    #[must_use]
    pub fn id(seed: Seed) -> PanelId {
        PanelId::new(Self::TAG, seed.args())
    }

    /// What a `compose` panel started from; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<Seed> {
        (id.tag == Self::TAG).then(|| Seed::of_args(&id.args))
    }

    #[must_use]
    pub fn seed(&self) -> Seed {
        self.seed
    }

    #[must_use]
    pub fn slot(&self) -> SlotId {
        self.slot
    }

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        self.world.store()
    }

    /// The text the fields show.
    #[must_use]
    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    /// What this sheet will carry: the `CARRIES` line, and what a send would
    /// take if the prototype had any MIME to put it in.
    #[must_use]
    pub fn carrying(&self) -> Rc<Vec<DraftFile>> {
        carry::files(self.store(), i64_of(self.slot), self.seed)
    }

    /// Looks at the shell for the one thing this panel cannot ask for while
    /// it builds its bar: whether another app is holding files. Called by the
    /// widget at the top of every draw and every event, so *attach* comes and
    /// goes with the clipboard and no subscription is needed.
    ///
    /// A build without the files app holds nothing, and the verb never
    /// appears — which is the whole of "works when the answer is `None`".
    pub fn observe(&mut self, s: &Session) {
        self.held = s
            .apps()
            .get_as::<Files>()
            .map(|f| f.clipboard().paths)
            .unwrap_or_default();
    }

    /// What the files app is holding, as of the last look.
    #[must_use]
    pub fn held(&self) -> &[String] {
        &self.held
    }

    /// A field changed: the panel keeps the text and the row follows.
    ///
    /// Deliberately **not** an action — typing is the future editor's local
    /// undo, not the workspace's — so it writes straight through the store
    /// the panel was opened with, and only when something actually moved.
    pub fn edited(&mut self, to: &str, subject: &str, body: &str) {
        let next = Draft {
            to: to.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
        };
        if next == self.draft {
            return;
        }
        self.draft = next;
        self.save();
    }

    /// Writes the draft row as it stands.
    fn save(&self) {
        let (slot, seed, d) = (i64_of(self.slot), self.seed, self.draft.clone());
        let now = self.world.now();
        let _ = self
            .world
            .store()
            .write(move |c| model::upsert_draft_tx(c, slot, seed, &d, now));
    }
}

impl Panel for Compose {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        match self.seed {
            Seed::Blank => "new mail".into(),
            Seed::Reply(id) => model::mail(self.store(), id)
                .map_or_else(|| "new mail".into(), |m| format!("re: {}", m.head.subject)),
            Seed::Forward(id) => model::mail(self.store(), id)
                .map_or_else(|| "new mail".into(), |m| format!("fwd: {}", m.head.subject)),
        }
    }

    /// Three fields and room to write in the third.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    /// The slot is also the draft's key, so this is where a restored compose
    /// finds the text it was left with.
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
        if let Some(d) = model::draft_for(self.store(), i64_of(slot), self.seed) {
            self.draft = d;
        }
    }

    /// The two ways out of a sheet, and — while another app is holding
    /// something — the way into it. *attach* is appended rather than
    /// inserted, so the two verbs that are always there never move under the
    /// hand as a clipboard fills.
    fn verbs(&self) -> Vec<Verb> {
        let mut v = vec![
            Verb::run("mail.send", "send", Some('s')),
            Verb::run("mail.discard", "discard", Some('d')),
        ];
        if !self.held.is_empty() {
            v.push(Verb::run("mail.attach", "attach", Some('h')));
        }
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "mail.send" => self.send(s),
            "mail.discard" => self.discard(s),
            "mail.attach" => self.attach(s),
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// The factory. The text a fresh compose starts from is its seed's — a reply
/// answers its mail, a forward passes it on — and a row the slot already has
/// wins over it, once [`Panel::placed`] has said which slot that is.
pub struct ComposeKind;

impl PanelKind for ComposeKind {
    fn tag(&self) -> Tag {
        Compose::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let world = cx.session().world().clone();
        let seed = Seed::of_args(&id.args);
        Box::new(Compose {
            id: id.clone(),
            seed,
            draft: model::seed_draft(world.store(), seed),
            world,
            slot: 0,
            held: Vec::new(),
        })
    }
}

impl Compose {
    /// The attach: what the files app is holding becomes what this draft
    /// will carry, as one undoable action. A path the draft already carries
    /// is ignored — it was not this action's to add, and so not its to take
    /// away.
    ///
    /// The draft row is written in the same transaction, as a send writes
    /// it: the files hang off the slot *and* its seed, so the row has to
    /// exist for them to be this sheet's rather than the panel-before's.
    fn attach(&mut self, s: &mut Session) {
        let key = i64_of(self.slot);
        let carried: Vec<String> = carry::files(s.store(), key, self.seed)
            .iter()
            .map(|f| f.path.clone())
            .collect();
        let fresh: Vec<String> = self
            .held
            .iter()
            .filter(|p| !carried.contains(p))
            .cloned()
            .collect();
        if fresh.is_empty() {
            s.notify("nothing to attach — it carries those already", false);
            return;
        }

        let n = fresh.len();
        let (seed, draft) = (self.seed, self.draft.clone());
        let (paths, now) = (fresh.clone(), s.now());
        let done = s.act(
            Action::writing("attach", format!("attach {}", files_said(n)), move |tx| {
                model::upsert_draft_tx(tx, key, seed, &draft, now)?;
                carry::attach_tx(tx, key, &paths, now).map(|_| ())
            })
            .claiming(vec![Box::new(Attached {
                slot: key,
                paths: fresh,
            }) as Box<dyn Intent>]),
        );
        if done.is_some() {
            s.notify(format!("carrying {}", files_said(n)), false);
        }
    }

    /// The send: the draft as it stands, an outbox row that comes due after
    /// the window, and the slot closed behind it. One action, so one undo
    /// takes the letter back and the panel with it — until the sender has
    /// taken the row, which is what [`Sent::blocked`] guards.
    fn send(&mut self, s: &mut Session) {
        if self.draft.to.trim().is_empty() {
            s.notify("no recipient", true);
            return;
        }
        let (slot, seed, draft, title) = (self.slot, self.seed, self.draft.clone(), self.title());
        let key = i64_of(slot);
        let delay = model::send_delay();
        let (now, after) = (s.now(), s.now() + delay);
        let done = s.act(
            Action::writing("send", format!("send “{title}”"), move |tx| {
                model::upsert_draft_tx(tx, key, seed, &draft, now)?;
                model::file_send_tx(tx, key, after)
            })
            .about(outbox_entity(key))
            .claiming(vec![Box::new(Sent { slot: key, delay })])
            .moving(move |wm| wm.close(slot)),
        );
        if done.is_some() {
            s.notify(format!("sending in {delay:.0}s"), false);
        }
    }

    /// The discard: the row goes with the panel, and undo puts both back.
    fn discard(&mut self, s: &mut Session) {
        let (slot, seed, draft, title) = (self.slot, self.seed, self.draft.clone(), self.title());
        let key = i64_of(slot);
        s.act(
            Action::writing("discard", format!("discard “{title}”"), move |tx| {
                model::discard_draft_tx(tx, key)
            })
            .claiming(vec![Box::new(Discarded {
                slot: key,
                draft,
                seed,
            }) as Box<dyn Intent>])
            .moving(move |wm| wm.close(slot)),
        );
    }
}

/// How many files, in words.
fn files_said(n: usize) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{n} files")
    }
}

/// A slot id as the draft table keys by it. Slot numbers are namespaced per
/// workspace and count up from there, so they are unique across the layout
/// and well inside what a SQLite integer holds.
fn i64_of(slot: SlotId) -> i64 {
    i64::try_from(slot).unwrap_or(i64::MAX)
}
