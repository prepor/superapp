//! The reader: one conversation, and which of its letters are unfolded.
//!
//! Opening it is what marks a conversation read — claimed on the same
//! undoable node as the layout change, so one undo closes the panel *and*
//! gives the flags back.

use std::any::Any;
use std::collections::BTreeSet;
use std::rc::Rc;

use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::{Action, Session};
use kernel::store::Store;

use super::super::effects::{Filed, MarkRead};
use super::super::model::{self, MailId, Seed, ThreadMail};
use super::super::{parts, reading};
use super::mailbox::{word_of, Mailbox};

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

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }

    /// The conversation, oldest first.
    #[must_use]
    pub fn thread(&self) -> Vec<ThreadMail> {
        model::thread(&self.store, self.mail)
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
        model::thread_topic(&self.store, self.mail).unwrap_or_else(|| "message".into())
    }

    /// As many rows as the conversation reads as, three at the least. The
    /// letter that does not fit is the whole reason a wish takes the column
    /// width.
    fn wish(&self, cols: usize) -> (u32, u32) {
        let msgs = self.thread();
        if msgs.is_empty() {
            return (4, FLOOR_ROWS);
        }
        // A letter that carries anything lists its parts on a line of their
        // own, so the wish counts that line too.
        let carries = parts::thread_carriers(&self.store, self.mail);
        let need = reading::thread_lines(&msgs, &self.open, &carries, cols) as f64;
        let rows = ((need + CHROME_LINES) / LINES_PER_ROW).ceil() as u32;
        (4, rows.max(FLOOR_ROWS))
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// Two buttons that file the conversation, and two links that answer it.
    ///
    /// *forward* is a link like *reply*: opening a sheet claims nothing. The
    /// `$Forwarded` keyword is set when the letter has actually **left** —
    /// [`Submit`](super::super::effects::Submit) writes it as the send
    /// settles — because that is when a letter has been passed on.
    fn verbs(&self) -> Vec<Verb> {
        let (slot, mail) = (self.slot, self.mail);
        vec![
            Verb::run("mail.archive", "archive", Some('a')),
            Verb::run("mail.delete", "delete", Some('d')),
            Verb::go(
                "mail.reply",
                "reply",
                Some('r'),
                Nav::Open {
                    from: slot,
                    id: super::Compose::id(Seed::Reply(mail)),
                    fresh: false,
                },
            ),
            Verb::go(
                "mail.forward",
                "forward",
                Some('f'),
                Nav::Open {
                    from: slot,
                    id: super::Compose::id(Seed::Forward(mail)),
                    fresh: false,
                },
            ),
        ]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "mail.archive" => self.file_thread(s, "archive"),
            "mail.delete" => self.file_thread(s, "trash"),
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
        let unread = model::thread_unread(&store, mail);
        // Where the catching up starts: the conversation from its first
        // unread letter down, and the mail the panel was opened on. Only the
        // read run above that letter folds — a letter *under* an unread one
        // belongs to the same sitting, whether or not another client has
        // already flagged it. Read before the claim below, which is what
        // makes them all read.
        let msgs = model::thread(&store, mail);
        let first = msgs
            .iter()
            .position(|t| t.mail.head.unread)
            .unwrap_or(msgs.len());
        let open: BTreeSet<MailId> = msgs[first..]
            .iter()
            .map(|t| t.mail.head.id)
            .chain(Some(mail))
            .collect();
        if !unread.is_empty() {
            let marks = unread.clone();
            cx.claim(
                Box::new(move |tx: &rusqlite::Transaction| {
                    for m in &marks {
                        model::mark_read_tx(tx, *m)?;
                    }
                    Ok(())
                }),
                unread
                    .iter()
                    .map(|m| Box::new(MarkRead { mail: *m }) as Box<dyn Intent>)
                    .collect(),
            );
        }
        Box::new(Message {
            id: id.clone(),
            mail,
            store,
            slot: 0,
            open,
            quotes: BTreeSet::new(),
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
    /// is a preview like any other.
    fn file_thread(&mut self, s: &mut Session, role: &'static str) {
        let (store, slot, mail) = (self.store.clone(), self.slot, self.mail);
        // Asked before acting, of the mail the panel was opened on: without
        // the folder the move is a no-op, and so is filing a mail into the
        // folder it is already in — a mail read out of the archive still
        // wears the *archive* button, because the button is about the mail.
        // An action that changes nothing records no node, so the answer has
        // to be a word rather than silence.
        if !model::can_file(&store, mail, role) {
            s.notify(format!("this account has no {role} folder"), true);
            return;
        }
        if model::already_filed(&store, mail, role) {
            s.notify(format!("already in the {role}"), true);
            return;
        }
        let moving: Vec<(MailId, i64)> = model::thread_siblings(&store, mail)
            .into_iter()
            .filter(|id| {
                model::can_file(&store, *id, role) && !model::already_filed(&store, *id, role)
            })
            .map(|id| (id, model::folder_of(&store, id)))
            .collect();
        // The mail itself moves, so this is the rest of the conversation
        // having nowhere to go — which is not a refusal of the verb.
        if moving.is_empty() {
            s.notify(format!("nothing to {}", word_of(role)), false);
            return;
        }

        let driver = s.join_parent_of(slot);

        let intents: Vec<Box<dyn Intent>> = moving
            .iter()
            .map(|(mail, from)| {
                Box::new(Filed {
                    mail: *mail,
                    from_folder: *from,
                    role,
                }) as Box<dyn Intent>
            })
            .collect();
        let ids: Vec<MailId> = moving.iter().map(|(id, _)| *id).collect();
        let title = self.title();
        let done = s.act(
            Action::writing("file", format!("{} “{title}”", word_of(role)), move |tx| {
                for id in &ids {
                    model::file_tx(tx, *id, role)?;
                }
                Ok(())
            })
            .claiming(intents)
            .moving(move |wm| wm.close(slot)),
        );
        if done.is_none() {
            return;
        }

        // The list that was driving this reader moves on. It finds its own
        // row by the cursor it still holds — the filed one is gone, so the
        // index it stood at is now the row below.
        let Some(driver) = driver else { return };
        let nav = s.panel(driver).and_then(|d| {
            let mut b = d.borrow_mut();
            b.as_any().downcast_mut::<Mailbox>()?.advance()
        });
        if let Some(nav) = nav {
            s.nav(nav);
        }
    }
}
