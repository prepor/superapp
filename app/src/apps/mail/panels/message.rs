//! The reader: one conversation, and which of its letters are unfolded.
//!
//! Opening it is what marks a conversation read — claimed on the same
//! undoable node as the layout change, so one undo closes the panel *and*
//! gives the flags back.

use std::any::Any;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::{Edit, Session};
use kernel::store::Store;

use super::super::display::Conversation;
use super::super::effects::MarkRead;
use super::super::model::{self, MailId, Role, Seed};
use super::mailbox::{Mailbox, To};
use super::super::filing::{self, Scope};

/// Roughly how many lines of letter one grid row holds. An estimate, like the
/// chrome allowance below: the wish only has to land on the right grid row,
/// and the layout clamps it to the active one.
const LINES_PER_ROW: f64 = 7.0;

/// Roughly how many lines the panel spends on everything that is not the
/// letters: its own header, the TO line and its rule, the bar at the foot,
/// and the padding around them.
const CHROME_LINES: f64 = 6.0;

/// The rows a reader asks for at the least — a two-line "see you Thursday"
/// has no reason to be tall.
const FLOOR_ROWS: u32 = 3;

/// One conversation, read.
pub struct Message {
    id: PanelId,
    mail: MailId,
    store: Rc<Store>,
    slot: SlotId,
    /// Which of the thread's letters are unfolded. Opening seeds it with
    /// the conversation from its first unread letter on, and the mail the
    /// panel was opened on.
    open: BTreeSet<MailId>,
    /// Which of them are showing the quoted tail they were written over.
    /// Panel context like [`Message::open`], and folded to begin with: in a
    /// conversation the quote is the message above.
    quotes: BTreeSet<MailId>,
    filing: Rc<Cell<bool>>,
    display: Arc<Conversation>,
    read_at: Vec<u64>,
    reading: Option<Reading>,
    factory: Option<kernel::app::WorldFactory>,
    initial: bool,
    initial_unread: Arc<Mutex<Option<BTreeSet<MailId>>>>,
    failure: Option<ReadFailure>,
}

const READING_TABLES: &[&str] = &["message", "folder", "attachment", "account"];

struct Reading {
    revision: Vec<u64>,
    receive: tokio::sync::oneshot::Receiver<Result<Conversation, String>>,
}

struct ReadFailure {
    attempts: u32,
    retry_at: std::time::Instant,
}

impl Message {
    pub const TAG: Tag = Tag("message");

    /// The identity of the panel that reads this mail's conversation.
    #[must_use]
    pub fn id(mail: MailId) -> PanelId {
        PanelId::new(Self::TAG, [mail.to_string()])
    }

    /// The mail a `message` panel names; `None` for any other tag, or for an
    /// argument this build cannot read.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<MailId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// The mail the panel was opened on.
    #[cfg(test)]
    #[must_use]
    pub fn mail(&self) -> MailId {
        self.mail
    }

    /// The conversation, oldest first.
    #[must_use]
    pub fn reading(&self) -> Arc<Conversation> {
        self.display.clone()
    }

    pub fn poll_read(&mut self, s: &mut Session) -> bool {
        let mut changed = false;
        if let Some(reading) = &mut self.reading {
            let ready = match reading.receive.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                Err(_) => Some(Err("mail reading stopped".into())),
            };
            if let Some(ready) = ready {
                let reading = self.reading.take().unwrap();
                self.read_at = reading.revision;
                match ready {
                    Ok(display) if self.read_at == self.store.revision(READING_TABLES) => {
                        self.failure = None;
                        self.publish(display);
                        changed = true;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let attempts = self
                            .failure
                            .as_ref()
                            .map_or(1, |failure| failure.attempts.saturating_add(1));
                        let delay =
                            std::time::Duration::from_secs(1 << attempts.saturating_sub(1).min(5));
                        if self.failure.is_none() {
                            s.notify(error, true);
                        }
                        self.failure = Some(ReadFailure {
                            attempts,
                            retry_at: std::time::Instant::now() + delay,
                        });
                        if let Some(wake) = self.store.ui_waker() {
                            kernel::runtime::spawn(async move {
                                tokio::time::sleep(delay).await;
                                wake();
                            });
                        }
                    }
                }
            }
        }
        let revision = self.store.revision(READING_TABLES);
        if self.reading.is_some()
            || (revision == self.read_at
                && self
                    .failure
                    .as_ref()
                    .is_none_or(|failure| std::time::Instant::now() < failure.retry_at))
        {
            return changed;
        }
        let Some(factory) = self.factory.clone() else {
            self.publish(Conversation::read(&self.store, self.mail));
            self.read_at = revision;
            return true;
        };
        let mail = self.mail;
        let db = self.store.db();
        let wake = self.store.ui_waker();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.reading = Some(Reading { revision, receive });
        kernel::runtime::spawn(async move {
            // The opening claim publishes the original unread IDs on the
            // writer. Read only after it, so a completed mark-read cannot
            // erase which letters this first opening should unfold.
            let result = match db.flush_async().await {
                Ok(()) => kernel::runtime::spawn_blocking(move || {
                    let world = factory.build().map_err(|error| error.to_string())?;
                    Ok(Conversation::read(world.store(), mail))
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string())),
                Err(error) => Err(error.to_string()),
            };
            let _ = send.send(result);
            if let Some(wake) = wake {
                wake();
            }
        });
        changed
    }

    fn publish(&mut self, display: Conversation) {
        if self.initial {
            let unread = self
                .initial_unread
                .lock()
                .expect("initial mail flags")
                .take()
                .unwrap_or_else(|| {
                    display
                        .letters
                        .iter()
                        .filter(|letter| letter.mail.head.unread)
                        .map(|letter| letter.mail.head.id)
                        .collect()
                });
            self.open = display.initially_open(self.mail, &unread);
            self.initial = false;
        }
        self.display = Arc::new(display);
    }

    /// Whether a letter of it is unfolded.
    #[must_use]
    pub fn is_open(&self, mail: MailId) -> bool {
        self.open.contains(&mail)
    }

    /// Folds a letter, or unfolds it. Not an action: what is open is the
    /// panel's own context, not a claim on the world.
    pub fn toggle(&mut self, mail: MailId) {
        if !self.open.remove(&mail) {
            self.open.insert(mail);
        }
    }

    /// Whether an open letter is showing its quoted tail.
    #[must_use]
    pub fn quoted(&self, mail: MailId) -> bool {
        self.quotes.contains(&mail)
    }

    /// Unfolds the quoted tail, or folds it back. The wish does not change
    /// with it — the quote is inside the letter's own scroll — so this asks
    /// for a redraw and nothing else.
    pub fn toggle_quote(&mut self, mail: MailId) {
        if !self.quotes.remove(&mail) {
            self.quotes.insert(mail);
        }
    }
}

impl Panel for Message {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        if self.display.title.is_empty() {
            "message".into()
        } else {
            self.display.title.clone()
        }
    }

    /// The conversation, named, and what opening it already did.
    fn about(&self) -> String {
        let topic = self.title();
        format!(
            "The reader: one whole conversation — “{topic}” — oldest letter \
             first, deduplicated by `Message-ID` so a reply that sits in both \
             Sent and the folder it was filed in appears once. Its argument is \
             a `message.id`, {}, and the letters are every row of `message` \
             sharing that row's `thread`; opening the panel marked the unread \
             ones read, on the same undoable node as the panel itself. A \
             person reads here, folds a letter or its quoted tail open and \
             shut, and archives, deletes, replies or forwards.",
            self.mail
        )
    }

    /// As many rows as the conversation reads as, three at the least. The
    /// letter that does not fit is the whole reason a wish takes the column
    /// width.
    fn wish(&self, cols: usize) -> (u32, u32) {
        if self.display.letters.is_empty() {
            return (4, FLOOR_ROWS);
        }
        // A letter that carries anything lists its parts on a line of their
        // own, so the wish counts that line too.
        let need = self.display.lines(&self.open, cols) as f64;
        let rows = ((need + CHROME_LINES) / LINES_PER_ROW).ceil() as u32;
        (4, rows.max(FLOOR_ROWS))
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// The buttons that file the conversation, and two links that answer it.
    ///
    /// *not spam* is the one button that comes and goes: a letter is junk or
    /// it is not, and only one read out of the spam folder can be said not to
    /// be. *archive* is a move any letter has, so it is on the bar wherever
    /// it was read and refuses in words when there is nowhere to go. *delete*
    /// is that too, until the letter is already in the trash — there the same
    /// place on the bar wears *put back*, because deleting a deleted letter
    /// is the one filing with nothing to do.
    ///
    /// *forward* is a link like *reply*: opening a sheet claims nothing. The
    /// `$Forwarded` keyword is set when the letter has actually **left** —
    /// [`Submit`](super::super::effects::Submit) writes it as the send
    /// settles — because that is when a letter has been passed on.
    fn verbs(&self) -> Vec<Verb> {
        let (slot, mail) = (self.slot, self.mail);
        let mut v = vec![Verb::run("mail.archive", "archive", Some('a'))];
        match model::role_of(&self.store, mail) {
            Some(Role::Spam) => {
                v.push(Verb::run("mail.not_spam", "not spam", Some('n')));
                v.push(Verb::run("mail.delete", "delete", Some('d')));
            }
            Some(Role::Trash) => v.push(Verb::run("mail.put_back", "put back", Some('p'))),
            _ => v.push(Verb::run("mail.delete", "delete", Some('d'))),
        }
        v.push(Verb::go(
            "mail.reply",
            "reply",
            Some('r'),
            Nav::Open {
                from: slot,
                id: super::Compose::id(Seed::Reply(mail)),
                fresh: false,
            },
        ));
        v.push(Verb::go(
            "mail.forward",
            "forward",
            Some('f'),
            Nav::Open {
                from: slot,
                id: super::Compose::id(Seed::Forward(mail)),
                fresh: false,
            },
        ));
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "mail.archive" => self.file_thread(s, To::Role("archive")),
            "mail.not_spam" => self.file_thread(s, To::Role("inbox")),
            "mail.delete" => self.file_thread(s, To::Role("trash")),
            "mail.put_back" => self.file_thread(s, To::Back),
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// The factory. Opening a mail reads its whole conversation: every unread
/// letter of it is marked, one intent each, and the panel opens unfolded from
/// the first unread one down.
pub struct MessageKind;

impl PanelKind for MessageKind {
    fn tag(&self) -> Tag {
        Message::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let mail = Message::of(id).unwrap_or_default();
        let factory = cx
            .session()
            .world()
            .factory()
            .filter(|_| store.ui_attached());
        let initial = factory.is_some();
        let display = if initial {
            Conversation::default()
        } else {
            Conversation::read(&store, mail)
        };
        let unread = display
            .letters
            .iter()
            .filter(|letter| letter.mail.head.unread)
            .map(|letter| letter.mail.head.id)
            .collect();
        let open = display.initially_open(mail, &unread);
        let initial_unread = Arc::new(Mutex::new(None));
        let captured = initial_unread.clone();
        cx.claim_with(Box::new(move |tx| {
            let unread = model::thread_unread_in(tx, mail)?;
            for id in &unread {
                model::mark_read_tx(tx, *id)?;
            }
            *captured.lock().expect("initial mail flags") = Some(unread.iter().copied().collect());
            Ok(unread
                .into_iter()
                .map(|mail| Box::new(MarkRead { mail }) as Box<dyn Intent>)
                .collect())
        }));
        let read_at = if initial {
            Vec::new()
        } else {
            store.revision(READING_TABLES)
        };
        Box::new(Message {
            id: id.clone(),
            mail,
            store,
            slot: 0,
            open,
            quotes: BTreeSet::new(),
            filing: Rc::new(Cell::new(false)),
            display: Arc::new(display),
            read_at,
            reading: None,
            factory,
            initial,
            initial_unread,
            failure: None,
        })
    }
}

impl Message {
    /// Files this conversation's copies in its own folder, closes this
    /// reader, and moves the list that was driving it on to the next row.
    ///
    /// It closes its own slot and nothing else: closing is one rule, and the
    /// kernel takes the joined chain and the focus with it. Another panel
    /// reading the same conversation — on this workspace or another — is
    /// somebody's own window and stays where it is; what it shows has moved
    /// folder, which is a fact about the mail and not a reason to take the
    /// panel away.
    ///
    /// The mails move in the action's transaction, the reader closes in its
    /// layout half — the instance runs to the end of this method all the
    /// same, and is dropped at the settle — and the cursor walk that follows
    /// folds into the same node: filing is one gesture, so it is one undo.
    fn file_thread(&mut self, s: &mut Session, to: To) {
        if self.filing.get() {
            return;
        }
        let (slot, mail) = (self.slot, self.mail);
        let driver = s.join_parent_of(slot).and_then(|slot| s.panel(slot).map(|panel| (slot, panel)));
        let cursor = driver.as_ref().and_then(|(_, panel)| {
            panel.borrow_mut().as_any().downcast_mut::<Mailbox>()?.after_removal()
        });
        let title = self.title();
        self.filing.set(true);
        let filing = self.filing.clone();
        s.act_async(
            Edit::writing("file", format!("{} “{title}”", to.word()), move |tx| {
                filing::file(tx, Scope::Message(mail), to, cursor)
            })
            .record_if(|outcome| outcome.changed)
            .wake_if(|outcome| outcome.changed)
            .claiming_with(|outcome| std::mem::take(&mut outcome.claims)),
            move |s, done| {
                filing.set(false);
                let Some(outcome) = done else { return };
                if !outcome.changed {
                    if let Some((message, error)) = outcome.notice { s.notify(message, error); }
                    return;
                }
                s.after_event(move |s| {
                    if !s.panel(slot).is_some_and(|panel| {
                        panel.borrow_mut().as_any().downcast_mut::<Message>()
                            .is_some_and(|message| Rc::ptr_eq(&message.filing, &filing))
                    }) { return; }
                    s.nav_within(Nav::Close { slot, label: Some(title) });
                    let Some((driver, original)) = driver else { return };
                    let Some(panel) = s.panel(driver).filter(|panel| Rc::ptr_eq(panel, &original)) else { return };
                    let nav = outcome.landing.and_then(|landing| {
                        panel.borrow_mut().as_any().downcast_mut::<Mailbox>()?.land(landing)
                    });
                    if let Some(nav) = nav { s.nav_within(nav); }
                });
            },
        );
    }
}

#[cfg(test)]
mod async_tests {
    use super::*;
    use kernel::app::{world_for, Apps, Env, Mode, Workers};
    use kernel::session::Action;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    static APPS: &[&dyn kernel::app::App] = &[&super::super::super::MAIL];

    #[test]
    fn a_native_reader_opens_without_waiting_and_keeps_the_original_unread_selection() {
        let apps = Apps::new(APPS);
        let store = Store::open(None, &apps.schemas(), kernel::sync::Device::fake().replicating(apps.replicated())).unwrap();
        apps.seed(&store, Mode::Fake).unwrap();
        let mail: i64 = store
            .conn()
            .query_row(
                "SELECT id FROM message WHERE unread=1 ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let original_unread: BTreeSet<_> = model::thread_unread(&store, mail).into_iter().collect();
        let expected = Conversation::read(&store, mail).initially_open(mail, &original_unread);
        let world = Rc::new(world_for(APPS, store, Mode::Fake, &Env::default()));
        let workers = Workers::none(world.store().clone());
        let mut session = Session::new(apps, world, workers);
        session.act(Action::new("open", "open inbox").moving(|layout| {
            layout.open(Role::Inbox.id(), None, true);
        }));
        session.settle();
        let from = session.focus().unwrap();
        let (wake, mut woke) = tokio::sync::mpsc::unbounded_channel();
        session.store().attach_ui(move || {
            let _ = wake.send(());
        });
        let (entered, started) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let _held_write = session
            .store()
            .submit_write(move |_| {
                entered.send(()).unwrap();
                held.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        session.nav(Nav::Open {
            from,
            id: Message::id(mail),
            fresh: true,
        });
        session.settle();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "opening waited for SQLite"
        );
        let panel = session.panel(session.focus().unwrap()).unwrap();
        {
            let mut panel = panel.borrow_mut();
            let reader = panel.as_any().downcast_mut::<Message>().unwrap();
            assert!(
                reader.reading().letters.is_empty(),
                "the UI fetched full bodies while opening"
            );
            assert_eq!(reader.wish(80), (4, FLOOR_ROWS));
        }
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // The shell consumes the coalesced display event before polling
            // panels, so a later preparation can request another frame.
            session.store().poll_external();
            session.settle();
            let done = {
                let mut panel = panel.borrow_mut();
                let reader = panel.as_any().downcast_mut::<Message>().unwrap();
                if reader.initial {
                    false
                } else {
                    assert_eq!(reader.open, expected);
                    let before = reader.reading();
                    for width in [20, 40, 80, 120] {
                        reader.wish(width);
                        assert!(
                            Arc::ptr_eq(&before, &reader.reading()),
                            "resizing rebuilt the conversation"
                        );
                    }
                    true
                }
            };
            if done {
                break;
            }
            assert!(Instant::now() < deadline, "reader did not finish");
            kernel::runtime::block_on(async {
                tokio::time::timeout(Duration::from_secs(5), woke.recv())
                    .await
                    .unwrap();
            });
        }
        assert!(model::thread_unread(session.store(), mail).is_empty());
        session.shutdown();
    }
}
