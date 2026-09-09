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
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::{Edit, Session};
use kernel::store::{PendingWrite, Store};

use crate::apps::files::Files;

use super::super::carry::{self, DraftFile};
use super::super::effects::{outbox_entity, Attached, Discarded, Sent};
use super::super::model::{self, Draft, Seed};

/// Keeps an approved tool send exclusive through preparation and commit.
/// Cancellation or failure drops the hold and makes the sheet editable again.
pub(crate) struct SendHold(Arc<AtomicBool>);
impl Drop for SendHold {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

type AdoptedDraft = rusqlite::Result<Option<(Draft, Seed)>>;
type AdoptionReply = Rc<RefCell<Option<AdoptedDraft>>>;

type DraftReply = Result<Option<(Draft, Seed)>, String>;
struct DraftLoading {
    sequence: u64,
    revision: Vec<u64>,
    receive: tokio::sync::oneshot::Receiver<DraftReply>,
}

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
    /// The failed send this sheet is the reopening of, until it has adopted
    /// it. See [`Compose::adopt`].
    reopen: Option<i64>,
    adopting: bool,
    adopted: AdoptionReply,
    saving: Option<PendingWrite<()>>,
    dirty: bool,
    save_failed: bool,
    sequence: u64,
    observed: Vec<u64>,
    loading: Option<DraftLoading>,
    ready: bool,
    busy: Arc<AtomicBool>,
    error: Option<String>,
    picking: Option<tokio::sync::oneshot::Receiver<Result<Vec<DraftFile>, String>>>,
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
    #[cfg(test)]
    #[must_use]
    pub fn of(id: &PanelId) -> Option<Seed> {
        (id.tag == Self::TAG).then(|| Seed::of_args(&id.args))
    }

    /// The identity of the sheet a failed send reopens as:
    /// `("compose", ["reopen", "9"])`.
    ///
    /// A third spelling beside *reply* and *forward*, because what it is is a
    /// third thing: the letter is already written, and which mail it answers
    /// is on the draft row rather than in the argument.
    #[must_use]
    pub fn reopen(outbox: i64) -> PanelId {
        PanelId::new(Self::TAG, ["reopen".to_string(), outbox.to_string()])
    }

    /// The failed send a `compose` panel is the reopening of, if it is one.
    #[must_use]
    pub fn reopens(id: &PanelId) -> Option<i64> {
        (id.tag == Self::TAG && id.arg(0) == Some("reopen"))
            .then(|| id.arg(1)?.parse().ok())
            .flatten()
    }

    #[must_use]
    pub fn seed(&self) -> Seed {
        self.seed
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

    /// What this sheet will carry: the `CARRIES` line, and what a send reads
    /// off the disk as the letter leaves.
    #[must_use]
    pub fn carrying(&self) -> Rc<Vec<DraftFile>> {
        carry::files(self.store(), i64_of(self.slot), self.seed)
    }

    /// Looks at what this panel cannot ask for while it builds its bar, and
    /// at what may have changed under it. Called by the widget at the top of
    /// every draw and every event, so *attach* comes and goes with the
    /// clipboard and no subscription is needed.
    ///
    /// A build without the files app holds nothing, and the verb never
    /// appears — which is the whole of "works when the answer is `None`".
    pub fn observe(&mut self, s: &Session) {
        self.held = s
            .apps()
            .get_as::<Files>()
            .map(|f| f.clipboard().paths)
            .unwrap_or_default();
        self.reread();
    }

    /// Re-reads the draft row, in case something other than this panel wrote
    /// it: a device-sync pass materializes the other machine's half-written
    /// letter, and no keystroke of this one's was involved. Typing writes the
    /// row through [`Compose::edited`], so in the ordinary case the two agree
    /// and this changes nothing.
    ///
    /// Answers whether the text moved, which is what the widget re-seeds its
    /// fields on.
    pub fn reread(&mut self) -> bool {
        if self.reopen.is_some()
            || self.dirty
            || self.saving.is_some()
            || self.busy.load(Ordering::Acquire)
        {
            return false;
        }
        if self.store().ui_attached() {
            let changed = self.poll_load();
            if self.loading.is_none() && self.store().revision(&["draft"]) != self.observed {
                self.load(!self.ready);
            }
            return changed;
        }
        let Some(d) = model::draft_for(self.store(), i64_of(self.slot), self.seed) else {
            return false;
        };
        if d == self.draft {
            return false;
        }
        self.draft = d;
        true
    }

    /// A field changed: the panel keeps the text and the row follows.
    ///
    /// Deliberately **not** an action — typing is the future editor's local
    /// undo, not the workspace's — so it writes straight through the store
    /// the panel was opened with, and only when something actually moved.
    pub fn edited(&mut self, to: &str, subject: &str, body: &str) {
        if self.busy() {
            return;
        }
        let next = Draft {
            to: to.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
        };
        if next == self.draft {
            return;
        }
        self.draft = next;
        self.sequence += 1;
        self.dirty = true;
        self.error = None;
        self.save_failed = false;
        self.save(false);
    }

    /// Takes the failed send's draft over: the row moves under this slot's
    /// id — a compose reads its draft by its own slot — the failed outbox row
    /// goes, and the sheet takes on whatever that letter answered.
    ///
    /// It happens here rather than in the verb that opened the panel because
    /// a [`Nav`](kernel::nav::Nav) cannot name a slot that does not exist
    /// yet. The slot is written into the cell the *reopen* verb's intent
    /// holds, so undo can put the draft back where it came from.
    ///
    /// Idempotent: a restored session runs it again against an outbox row
    /// that is already gone, and nothing moves.
    fn adopt(&mut self) {
        if self.store().ui_attached() {
            return;
        }
        let Some(old) = self.reopen else {
            return;
        };
        let (key, now) = (i64_of(self.slot), self.world.now());
        let result = self.store().write(move |tx| {
            model::reopen_send_tx(tx, old, key, now)?;
            model::draft_any_in(tx, key)
        });
        self.finish_adoption(result);
    }

    /// The UI can navigate freely while a draft loads or commits, but the
    /// captured send/discard must not race another edit in the same sheet.
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire) || !self.ready || self.reopen.is_some()
    }

    pub(crate) fn hold_send(&mut self) -> Result<(Draft, Seed, SendHold), String> {
        if self.busy() {
            return Err("this compose is still loading or saving".into());
        }
        if self.draft.to.trim().is_empty() {
            return Err("no recipient".into());
        }
        self.save(true);
        self.busy.store(true, Ordering::Release);
        Ok((self.draft.clone(), self.seed, SendHold(self.busy.clone())))
    }

    pub(crate) fn holds(&self, hold: &SendHold) -> bool {
        Arc::ptr_eq(&self.busy, &hold.0)
    }

    fn load(&mut self, initial: bool) {
        if self.loading.is_some() {
            return;
        }
        let Some(factory) = self.world.factory() else {
            return;
        };
        let (slot, seed) = (i64_of(self.slot), self.seed);
        let wake = self.store().ui_waker();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.loading = Some(DraftLoading {
            sequence: self.sequence,
            revision: self.store().revision(&["draft"]),
            receive: rx,
        });
        kernel::runtime::spawn_blocking(move || {
            let result = factory.build().map_err(|e| e.to_string()).map(|world| {
                model::draft_for(world.store(), slot, seed)
                    .map(|draft| (draft, seed))
                    .or_else(|| initial.then(|| (model::seed_draft(world.store(), seed), seed)))
            });
            let _ = tx.send(result);
            if let Some(wake) = wake {
                wake();
            }
        });
    }

    fn poll_load(&mut self) -> bool {
        let Some(loading) = &mut self.loading else {
            return false;
        };
        let result = match loading.receive.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return false,
            Err(_) => Err("draft loading stopped".into()),
        };
        let loading = self.loading.take().unwrap();
        self.observed = loading.revision;
        if loading.sequence != self.sequence {
            return false;
        }
        match result {
            Ok(draft) => {
                self.ready = true;
                if let Some((draft, seed)) = draft {
                    self.seed = seed;
                    let changed = draft != self.draft;
                    self.draft = draft;
                    return changed;
                }
            }
            Err(error) => self.error = Some(error),
        }
        false
    }

    pub fn poll_work(&mut self, s: &mut Session) {
        let store = self.world.store().clone();
        if let Some(old) = self
            .reopen
            .filter(|_| !self.adopting && store.ui_attached())
        {
            self.adopting = true;
            let (key, now) = (i64_of(self.slot), self.world.now());
            let adopted = self.adopted.clone();
            s.act_async_result(
                Edit::writing("mail.reopen", "reopen failed send", move |tx| {
                    model::reopen_send_tx(tx, old, key, now)?;
                    model::draft_any_in(tx, key)
                })
                .record_if(|_| false),
                move |s, result| {
                    if result.is_ok() {
                        super::super::effects::reopen_cell(old).store(key, Ordering::Release);
                    }
                    *adopted.borrow_mut() = Some(result);
                    s.redraw();
                },
            );
        }
        let adopted = self.adopted.borrow_mut().take();
        if let Some(result) = adopted {
            self.finish_adoption(result);
        }
        if let Some(result) = self
            .saving
            .as_mut()
            .and_then(|pending| pending.poll(&store))
        {
            self.saving = None;
            if let Err(error) = result {
                self.dirty = true;
                self.save_failed = true;
                self.error = Some(format!("could not save draft: {error}"));
            }
            self.observed = store.revision(&["draft"]);
        }
        if self.error.is_none() {
            self.save(false);
        }
        if let Some(rx) = &mut self.picking {
            let picked = match rx.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                Err(_) => Some(Err("attachment lookup stopped".into())),
            };
            if let Some(picked) = picked {
                self.picking = None;
                match picked {
                    Ok(picked) => self.attach_picked(s, picked),
                    Err(error) => {
                        self.busy.store(false, Ordering::Release);
                        s.notify(error, true);
                    }
                }
            }
        }
        self.reread();
        if let Some(error) = self.error.take() {
            s.notify(error, true);
        }
    }

    fn finish_adoption(&mut self, result: rusqlite::Result<Option<(Draft, Seed)>>) {
        match result {
            Ok(draft) => {
                let old = self.reopen.take().expect("one adoption");
                super::super::effects::reopen_cell(old)
                    .store(i64_of(self.slot), std::sync::atomic::Ordering::Relaxed);
                if let Some((draft, seed)) = draft {
                    self.seed = seed;
                    self.draft = draft;
                }
                self.ready = true;
                self.observed = self.store().revision(&["draft"]);
            }
            Err(error) => self.error = Some(format!("could not reopen draft: {error}")),
        }
    }

    /// Enqueue the latest draft. A barrier submits it after an in-flight save
    /// so send/discard/close cannot overtake the final keystroke.
    fn save(&mut self, barrier: bool) {
        if !self.dirty || (!barrier && (self.saving.is_some() || self.save_failed)) {
            return;
        }
        let (slot, seed, draft, now) = (
            i64_of(self.slot),
            self.seed,
            self.draft.clone(),
            self.world.now(),
        );
        if self.store().ui_attached() {
            match self
                .store()
                .submit_write(move |tx| model::upsert_draft_tx(tx, slot, seed, &draft, now))
            {
                Ok(pending) => {
                    self.saving = Some(pending);
                    self.dirty = false;
                }
                Err(error) => {
                    self.save_failed = true;
                    self.error = Some(format!("could not save draft: {error}"));
                }
            }
        } else {
            match self
                .store()
                .write(move |tx| model::upsert_draft_tx(tx, slot, seed, &draft, now))
            {
                Ok(()) => {
                    self.dirty = false;
                    self.observed = self.store().revision(&["draft"]);
                }
                Err(error) => {
                    self.save_failed = true;
                    self.error = Some(format!("could not save draft: {error}"));
                }
            }
        }
    }
}

impl Panel for Compose {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        match self.seed {
            Seed::Blank => "new mail".into(),
            Seed::Reply(id) => model::display_head(self.store(), id)
                .map_or_else(|| "new mail".into(), |m| format!("re: {}", m.subject)),
            Seed::Forward(id) => model::display_head(self.store(), id)
                .map_or_else(|| "new mail".into(), |m| format!("fwd: {}", m.subject)),
        }
    }

    /// A draft is not a letter, and what this one started from.
    fn about(&self) -> String {
        let from = match self.seed {
            Seed::Blank => "It started blank.".to_string(),
            Seed::Reply(id) => format!(
                "Its arguments say it is a reply to message {id}, so it came \
                 up with the recipient and subject filled and that letter \
                 quoted under them."
            ),
            Seed::Forward(id) => format!(
                "Its arguments say it is a forward of message {id}, so it came \
                 up with an empty recipient and that letter under a \
                 forwarded-message header."
            ),
        };
        format!(
            "A draft, not a letter: the recipient, the subject and the body of \
             something nobody has sent, written to `draft` as it is typed and \
             keyed by this panel's own slot — slot ids are stable and \
             persisted, so half-written text survives a restart. {from} \
             *send* files one `outbox` row with a ten-second window and closes \
             the sheet, both on one undoable node; nothing here reaches a \
             server, and until that window runs out an undo takes the letter \
             back."
        )
    }

    /// Three fields and room to write in the third.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    /// The slot is also the draft's key, so this is where a restored compose
    /// finds the text it was left with — and where a reopened send adopts the
    /// draft that failed.
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
        if self.reopen.is_some() {
            self.adopt();
            return;
        }
        if self.store().ui_attached() {
            self.load(true);
            return;
        }
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

    fn flush(&mut self) {
        self.save(true);
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
            reopen: Compose::reopens(id),
            id: id.clone(),
            seed,
            draft: if world.store().ui_attached() {
                Draft::default()
            } else {
                model::seed_draft(world.store(), seed)
            },
            ready: !world.store().ui_attached(),
            adopting: false,
            adopted: Rc::new(RefCell::new(None)),
            saving: None,
            dirty: false,
            save_failed: false,
            sequence: 0,
            observed: Vec::new(),
            loading: None,
            busy: Arc::new(AtomicBool::new(false)),
            error: None,
            picking: None,
            world,
            slot: 0,
            held: Vec::new(),
        })
    }
}

impl Compose {
    /// The attach: what the files app is holding becomes what this draft
    /// will carry, as one undoable action.
    ///
    /// Three refusals, each in words. A directory is not a file and is passed
    /// over; anything past [`ATTACH_MAX`] is refused **with its size**, since
    /// the only useful thing to say about a file too big is how big it is;
    /// and a path the draft already carries is ignored — it was not this
    /// action's to add, and so not its to take away.
    ///
    /// Which install is picking them is recorded with the row: a path is a
    /// file on the machine it was picked on, and these rows replicate.
    ///
    /// The draft row is written in the same transaction, as a send writes it:
    /// the files hang off the slot *and* its seed, so the row has to exist for
    /// them to be this sheet's rather than the panel-before's.
    fn attach(&mut self, s: &mut Session) {
        if self.busy() {
            return;
        }
        if !s.writable() {
            s.notify("another device holds the write lease", true);
            return;
        }
        self.busy.store(true, Ordering::Release);
        if self.store().ui_attached() {
            let Some(factory) = self.world.factory() else {
                self.busy.store(false, Ordering::Release);
                s.notify("cannot inspect attachments in this world", true);
                return;
            };
            let paths = self.held.clone();
            let wake = self.store().ui_waker();
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.picking = Some(rx);
            kernel::runtime::spawn_blocking(move || {
                let result = factory
                    .build()
                    .map_err(|e| e.to_string())
                    .and_then(|world| picked_files(&world, &paths));
                let _ = tx.send(result);
                if let Some(wake) = wake {
                    wake();
                }
            });
        } else {
            match picked_files(s.world(), &self.held) {
                Ok(picked) => self.attach_picked(s, picked),
                Err(error) => {
                    self.busy.store(false, Ordering::Release);
                    s.notify(error, true);
                }
            }
        }
    }

    fn attach_picked(&mut self, s: &mut Session, picked: Vec<DraftFile>) {
        let key = i64_of(self.slot);
        let (ok, big): (Vec<_>, Vec<_>) = picked
            .into_iter()
            .partition(|f| f.size <= kernel::caps::ATTACH_MAX);
        if ok.is_empty() {
            // Nothing could be carried, and the refusal is the word.
            let why = if big.is_empty() {
                "nothing to attach — a letter carries files".to_string()
            } else {
                format!(
                    "too big to carry: {} (the limit is {})",
                    big.iter()
                        .map(|f| f.name.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    kernel::caps::fmt_size(kernel::caps::ATTACH_MAX)
                )
            };
            self.busy.store(false, Ordering::Release);
            s.notify(why, true);
            return;
        }

        let what = what_said(&ok);
        let (seed, draft) = (self.seed, self.draft.clone());
        let now = s.now();
        let busy = self.busy.clone();
        self.save(true);
        s.act_async(
            Edit::writing("attach", format!("attach {what}"), move |tx| {
                model::upsert_draft_tx(tx, key, seed, &draft, now)?;
                carry::attach_tx(tx, key, &ok, now)
            })
            .record_if(|files| !files.is_empty()),
            move |s, done| {
                busy.store(false, Ordering::Release);
                if let Some(files) = done {
                    if files.is_empty() {
                        s.notify(format!("already carrying {what}"), false);
                        return;
                    }
                    let what = what_said(&files);
                    s.claim(Box::new(Attached { slot: key, files }));
                    let but = if big.is_empty() {
                        String::new()
                    } else {
                        format!(" — {} too big", big.len())
                    };
                    s.notify(format!("carrying {what}{but}"), !big.is_empty());
                }
            },
        );
    }

    /// The send: the draft as it stands, an outbox row that comes due after
    /// the window, and the slot closed behind it. One action, so one undo
    /// takes the letter back and the panel with it — until the sender has
    /// taken the row, which is what [`Sent::blocked`] guards.
    fn send(&mut self, s: &mut Session) {
        if self.busy() {
            return;
        }
        if self.draft.to.trim().is_empty() {
            s.notify("no recipient", true);
            return;
        }
        let (slot, seed, draft, title) = (self.slot, self.seed, self.draft.clone(), self.title());
        let key = i64_of(slot);
        let delay = model::send_delay();
        let now = s.now();
        let clock = self.world.factory().map(|factory| factory.clock());
        self.busy.store(true, Ordering::Release);
        let busy = self.busy.clone();
        let background = self.store().ui_attached();
        self.save(true);
        s.act_async(
            Edit::writing("send", format!("send “{title}”"), move |tx| {
                model::upsert_draft_tx(tx, key, seed, &draft, now)?;
                model::file_send_tx(
                    tx,
                    key,
                    clock.as_ref().map_or(now, |clock| clock.read()) + delay,
                )
            })
            .about(outbox_entity(key))
            .claiming(vec![Box::new(Sent { slot: key, delay })]),
            move |s, done| {
                busy.store(false, Ordering::Release);
                if done.is_some() {
                    if !background || same_compose(s, slot, &busy) {
                        s.nav_within(Nav::Close {
                            slot,
                            label: Some(title),
                        });
                    }
                    s.notify(format!("sending in {delay:.0}s"), false);
                }
            },
        );
    }

    /// The discard: the row goes with the panel — and what it was going to
    /// carry goes with the row — and undo puts all three back.
    fn discard(&mut self, s: &mut Session) {
        if self.busy() {
            return;
        }
        let (slot, seed, draft, title) = (self.slot, self.seed, self.draft.clone(), self.title());
        let key = i64_of(slot);
        self.busy.store(true, Ordering::Release);
        let busy = self.busy.clone();
        let background = self.store().ui_attached();
        self.save(true);
        s.act_async(
            Edit::writing("discard", format!("discard “{title}”"), move |tx| {
                let files = carry::all(tx, key)?;
                model::discard_draft_tx(tx, key)?;
                Ok(files)
            }),
            move |s, done| {
                busy.store(false, Ordering::Release);
                if let Some(files) = done {
                    s.claim(Box::new(Discarded {
                        slot: key,
                        draft,
                        seed,
                        files,
                    }));
                    if !background || same_compose(s, slot, &busy) {
                        s.nav_within(Nav::Close {
                            slot,
                            label: Some(title),
                        });
                    }
                }
            },
        );
    }
}

impl Drop for Compose {
    fn drop(&mut self) {
        self.save(true);
    }
}

fn same_compose(s: &Session, slot: SlotId, busy: &Arc<AtomicBool>) -> bool {
    s.panel(slot).is_some_and(|panel| {
        panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<Compose>()
            .is_some_and(|compose| Arc::ptr_eq(&compose.busy, busy))
    })
}

fn picked_files(world: &World, paths: &[String]) -> Result<Vec<DraftFile>, String> {
    let device = world.store().device();
    world.with_cap::<dyn kernel::caps::Disk, _>(|disk| {
        let mut picked = Vec::new();
        for path in paths {
            if let Some(entry) = disk.stat(&kernel::caps::real_path(path))? {
                if !entry.is_dir {
                    picked.push(DraftFile {
                        path: path.clone(),
                        name: entry.name,
                        size: entry.size,
                        device: device.clone(),
                    });
                }
            }
        }
        Ok(picked)
    })?
}

/// What a set of files is called: the name where there is one, the count
/// where there is a set.
fn what_said(files: &[DraftFile]) -> String {
    match files {
        [one] => format!("“{}”", one.name),
        many if many.len() == 1 => "1 file".into(),
        many => format!("{} files", many.len()),
    }
}

/// A slot id as the draft table keys by it. Slot numbers are namespaced per
/// workspace and count up from there, so they are unique across the layout
/// and well inside what a SQLite integer holds.
fn i64_of(slot: SlotId) -> i64 {
    i64::try_from(slot).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod async_tests {
    use super::*;
    use kernel::app::{world_for, Apps, Env, Mode, Workers};
    use kernel::session::Action;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    static APPS: &[&dyn kernel::app::App] = &[&super::super::super::MAIL];

    fn session() -> (Session, kernel::session::Instance, SlotId) {
        let apps = Apps::new(APPS);
        let store = Store::open(None, &apps.schemas()).unwrap();
        apps.seed(&store, Mode::Fake).unwrap();
        let world = Rc::new(world_for(APPS, store, Mode::Fake, &Env::default()));
        let workers = Workers::none(world.store().clone());
        let mut s = Session::new(apps, world, workers, Mode::Fake);
        s.act(Action::new("open", "open compose").moving(|wm| {
            wm.open(Compose::id(Seed::Blank), None, true);
        }));
        s.settle();
        let slot = s.focus().unwrap();
        let panel = s.panel(slot).unwrap();
        s.store().attach_ui(|| {});
        (s, panel, slot)
    }

    #[test]
    fn slow_writer_does_not_block_typing_and_send_keeps_the_last_edit() {
        let (mut s, panel, slot) = session();
        let (entered, wait) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let _blocked = s
            .store()
            .submit_write(move |_| {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            })
            .unwrap();
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        {
            let mut panel = panel.borrow_mut();
            let compose = panel.as_any().downcast_mut::<Compose>().unwrap();
            compose.edited("a@example.com", "subject", "first");
            compose.edited("a@example.com", "subject", "last keystroke");
            compose.send(&mut s);
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the UI waited for the database writer"
        );
        assert!(
            s.panel(slot).is_some(),
            "the compose closes only after a successful commit"
        );
        release.send(()).unwrap();
        kernel::runtime::block_on(s.store().flush_async()).unwrap();
        s.settle();
        assert!(s.panel(slot).is_none());
        assert_eq!(
            model::draft_any(s.store(), i64_of(slot)).unwrap().0.body,
            "last keystroke"
        );
        assert_eq!(
            s.store()
                .conn()
                .query_row(
                    "SELECT status FROM outbox WHERE id=?",
                    [i64_of(slot)],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "pending"
        );
    }

    #[test]
    fn closing_a_compose_enqueues_its_latest_coalesced_draft() {
        let (mut s, panel, slot) = session();
        let (entered, wait) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let _blocked = s
            .store()
            .submit_write(move |_| {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            })
            .unwrap();
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        {
            let mut panel = panel.borrow_mut();
            let compose = panel.as_any().downcast_mut::<Compose>().unwrap();
            compose.edited("a@example.com", "subject", "first");
            compose.edited("a@example.com", "subject", "survives close");
        }
        s.nav(Nav::Close {
            slot,
            label: Some("compose".into()),
        });
        s.settle();
        drop(panel);
        release.send(()).unwrap();
        kernel::runtime::block_on(s.store().flush_async()).unwrap();
        assert_eq!(
            model::draft_any(s.store(), i64_of(slot)).unwrap().0.body,
            "survives close"
        );
    }
}
