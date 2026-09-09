//! One mailbox: a rich table of conversations, and the batch verbs over what
//! is marked in it.
//!
//! One instance type for both tags. The role is what the source, the base
//! condition and the bar are picked by; nothing else about a list of mail
//! changes with the folder it is over.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

use kernel::history::{Intent, UiIntent};
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlCursor, SqlLanding, SqlSource};
use kernel::session::{Edit, Instance, Session};
use kernel::store::Store;

use super::super::effects::{Filed, PutBack};
use super::super::filing::{self, Scope};
use super::super::model::{self, MailId, Role, ThreadHead, MAILBOX_PAGE};
use super::Message;

/// A mailbox panel: the folder's conversations, its cursor, and its marks.
pub struct Mailbox {
    id: PanelId,
    role: Role,
    store: Rc<Store>,
    slot: SlotId,
    list: ListState<&'static SqlSource<ThreadHead, i64>>,
    filing: Rc<Cell<bool>>,
}

impl Mailbox {
    /// The identity of one role's mailbox.
    #[must_use]
    pub fn id(role: Role) -> PanelId {
        role.id()
    }

    /// The filter the panel comes up under, in the list's own grammar. The
    /// widget seeds its field with this once; from then on the field is the
    /// person's, and this stays what the panel's identity says.
    #[must_use]
    pub fn seed_filter(&self) -> String {
        Role::sender_of(&self.id)
            .map(Role::filter_expr)
            .unwrap_or_default()
    }

    /// The table, its cursor and its marks, read-only. The widget drives
    /// them through [`Mailbox::list_mut`]; this read is the tests' own door
    /// onto what it did.
    #[cfg(test)]
    #[must_use]
    pub fn list(&self) -> &ListState<&'static SqlSource<ThreadHead, i64>> {
        &self.list
    }

    pub fn list_mut(&mut self) -> &mut ListState<&'static SqlSource<ThreadHead, i64>> {
        &mut self.list
    }

    /// How many rows the filter shows.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len(&self.store)
    }

    /// Rows `lo..hi`, as far as the table has them.
    #[cfg(test)]
    #[must_use]
    pub fn rows(&self, lo: usize, hi: usize) -> Vec<ThreadHead> {
        self.list.rows(&self.store, lo, hi)
    }

    /// Space: the mark on the cursor's row, toggled.
    #[cfg(test)]
    pub fn toggle_mark(&mut self) -> bool {
        let store = self.store.clone();
        self.list.toggle_mark(&store)
    }

    /// Steps the cursor and answers the preview that follows. A walk of these
    /// coalesces into one undo node, because every one of them starts from
    /// this slot.
    #[cfg(test)]
    pub fn walk(&mut self, d: isize) -> Option<Nav> {
        let store = self.store.clone();
        let row = self.list.move_cursor(&store, d)?;
        Some(self.preview(row.target))
    }

    /// Puts the cursor on row `i` — a click — and answers the preview.
    #[cfg(test)]
    pub fn go(&mut self, i: usize) -> Option<Nav> {
        let store = self.store.clone();
        let row = self.list.set_cursor(&store, i)?;
        Some(self.preview(row.target))
    }

    /// Capture the cursor for the writer to resolve after filing.
    pub(super) fn after_removal(&self) -> Option<SqlCursor<ThreadHead, i64>> {
        self.list.after_removal()
    }

    /// Apply the committed successor while this gesture still owns the cursor.
    pub(super) fn land(&mut self, landing: SqlLanding<ThreadHead>) -> Option<Nav> {
        let row = self.list.land(landing)?;
        Some(self.preview(row.target))
    }

    /// The verb this list wears beside *delete*, by id — the same answer
    /// [`Mailbox::verbs`] gives the bar. A gesture asks it too, so a finger
    /// and a button can never offer different verbs.
    #[must_use]
    pub fn keeps(&self) -> Option<&'static str> {
        keep_verb(self.role).map(|(id, ..)| id)
    }

    /// The verb that files a conversation away, by id — *delete* everywhere
    /// but in the trash, which is where delete goes. Asked by the bar and by
    /// the sweep, for the same reason [`Mailbox::keeps`] is.
    #[must_use]
    pub fn deletes(&self) -> Option<&'static str> {
        (self.role != Role::Trash).then_some("mail.delete")
    }

    /// Marks the table again — what undo hands back after a batch verb took
    /// them off.
    pub fn restore_marks(&mut self, keys: &[i64]) {
        self.list.marks_mut().extend(keys.iter().copied());
    }

    /// Takes them all off.
    pub fn clear_marks(&mut self) {
        self.list.clear_marks();
    }

    fn preview(&self, target: MailId) -> Nav {
        Nav::Preview {
            from: self.slot,
            id: Message::id(target),
        }
    }
}

impl Panel for Mailbox {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// The folder's own word, and what the panel was filtered to when it was
    /// opened on one: `inbox · vera@kovac.io`.
    fn title(&self) -> String {
        match Role::sender_of(&self.id) {
            Some(who) => format!("{} · {who}", self.role.as_str()),
            None => self.role.as_str().to_string(),
        }
    }

    /// The folder, what a row stands for, and the filter this one opened
    /// under.
    fn about(&self) -> String {
        let role = self.role.as_str();
        let what = match self.role {
            Role::Inbox => "the letters that arrived and have not been filed away",
            Role::Archive => "the letters that were kept",
            Role::Sent => "the letters this account has sent",
            Role::Spam => "the letters a provider called junk",
            Role::Trash => "the letters that were deleted, and can still be put back",
        };
        // What the marks are for here — a bar may not promise a verb this
        // list does not wear.
        let marked_for = match self.role {
            Role::Inbox => "archive or delete them together",
            Role::Spam => "un-junk or delete them together",
            Role::Archive | Role::Sent => "delete them together",
            Role::Trash => "put them back where they were deleted from",
        };
        let narrowed = match Role::sender_of(&self.id) {
            Some(who) => format!(
                "Its argument is a sender, {who}, so it came up filtered to that \
                 address; the field above the rows is the person's from then on \
                 and may say something else by now."
            ),
            None => "It carries no argument, so it came up unfiltered: whatever \
                     is in the field above the rows is all that narrows it."
                .to_string(),
        };
        format!(
            "{role}: {what}, one row a conversation rather than a letter — who \
             is in it with this account's own address written as `me`, how many \
             letters it holds, the subject of the oldest with its reply \
             prefixes stripped, and the date of the newest one in this folder; \
             a row is bold while anything in it is unread. The rows are \
             `message` joined to `folder` where the folder's role is \
             '{role}', grouped by `message.thread`. {narrowed} Here a person \
             walks the rows, opens one to read the whole conversation, and \
             marks some to {marked_for}."
        )
    }

    /// Four wide, six tall: a list is the one panel that wants the column.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// *sync* always; the batch verbs while there are marks, with their
    /// count, and the two verbs about the set itself.
    ///
    /// Which verb keeps a conversation is [`keep_verb`]'s answer, and a bar
    /// may not wear a verb that would do nothing (or, from Sent, something
    /// nobody asked for). Delete is the move every mailbox has but the
    /// trash, which is where delete goes: there the keeping verb is *put
    /// back* and it stands alone.
    ///
    /// *mark all* wears `m` rather than the obvious `l`: this shell keeps
    /// `cmd+l` for itself (see [`keys`](crate::shell::keys)), and a bar may
    /// not promise a chord that never arrives. *clear* wears none — `esc` is
    /// the table's own.
    fn verbs(&self) -> Vec<Verb> {
        let mut v = vec![Verb::run("mail.sync", "sync", Some('s'))];
        let n = self.list.marks().len();
        if n == 0 {
            return v;
        }
        if let Some((id, word, key)) = keep_verb(self.role) {
            v.push(Verb::run(id, format!("{word} {n}"), Some(key)));
        }
        if let Some(id) = self.deletes() {
            v.push(Verb::run(id, format!("delete {n}"), Some('d')));
        }
        v.push(Verb::run("mail.all", "mark all", Some('m')));
        v.push(Verb::run("mail.clear", "clear", None));
        v
    }

    /// Its own verbs, on its own table: the marks are read straight off
    /// `self`, and what they name is filed in one action.
    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            // *sync* is the one gesture that means "go and look now": the
            // pass would otherwise push what is waiting and leave the
            // outside until its interval ran out.
            "mail.sync" => {
                super::super::sync::pull_now();
                s.workers().kick_all();
                s.notify("syncing", false);
            }
            "mail.archive" => self.file_marked(s, To::Role("archive")),
            "mail.not_spam" => self.file_marked(s, To::Role("inbox")),
            "mail.delete" => self.file_marked(s, To::Role("trash")),
            "mail.put_back" => self.file_marked(s, To::Back),
            "mail.all" => {
                let store = self.store.clone();
                self.list.mark_all(&store);
                s.redraw();
            }
            "mail.clear" => {
                self.clear_marks();
                s.redraw();
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// The factory: one value per tag, both opening the same instance.
pub struct MailboxKind(pub Role);

impl PanelKind for MailboxKind {
    fn tag(&self) -> Tag {
        self.0.tag()
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let mut list = ListState::new(model::threads(self.0), MAILBOX_PAGE);
        // A panel opened on a filter is filtered from the first draw, widget
        // or no widget: the field is seeded from the same string, and from
        // then on the field is the one source of it.
        if let Some(sender) = Role::sender_of(id) {
            list.set_filter(&Role::filter_expr(sender));
        }
        Box::new(Mailbox {
            id: id.clone(),
            role: self.0,
            store: cx.session().store().clone(),
            slot: 0,
            list,
            filing: Rc::new(Cell::new(false)),
        })
    }
}

// -- the batch verbs ---------------------------------------------------------

/// Where a filing verb sends what it moves: a folder role, or back where
/// each letter was deleted from.
///
/// The trash is the one mailbox whose keeping verb has no role to name —
/// mail arrives in it from everywhere, so what a *put back* moves a letter
/// to is a folder id per letter, read off the row the delete wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum To {
    Role(&'static str),
    Back,
}

impl To {
    /// The verb's own word — what its button says, and what history calls
    /// the node it recorded.
    pub fn word(self) -> &'static str {
        match self {
            To::Role("trash") => "delete",
            To::Role("inbox") => "not spam",
            To::Role(_) => "archive",
            To::Back => "put back",
        }
    }

    /// What a verb with nothing left to move says instead. *not spam* is the
    /// odd one out: "nothing to not spam" is not a sentence.
    pub fn nothing_said(self) -> String {
        if self == To::Role("inbox") {
            "nothing to take out of the spam".to_string()
        } else {
            format!("nothing to {}", self.word())
        }
    }
}

impl Mailbox {
    /// Files every marked conversation into `to`, as one undoable action:
    /// the marks come off, the folder's own copies of each conversation
    /// move, and undo brings both back.
    ///
    /// It closes nothing. A panel reading one of the filed conversations is
    /// somebody's own window; what happens instead is the cursor walk — it
    /// lands on the nearest row that stayed and previews it, and the join
    /// rule puts that preview where the old one was. That walk is part of
    /// the action, not a second one: one undo takes the whole gesture back.
    fn file_marked(&mut self, s: &mut Session, to: To) {
        if self.filing.get() {
            return;
        }
        let keys = self.list.marks().keys();
        if keys.is_empty() {
            return;
        }
        let cursor = self.after_removal();
        let role = self.role;
        let threads = keys.clone();
        let slot = self.slot;
        self.clear_marks();
        let marks = s.panel(slot).map(|panel| RestoredMarks {
            panel,
            keys: keys.clone(),
        });
        self.filing.set(true);
        let filing = self.filing.clone();
        s.act_async(
            Edit::writing("file", format!("{} {}", to.word(), threads_said(keys.len())), move |tx| {
                filing::file(tx, Scope::Mailbox { role, threads }, to, cursor)
            })
            .record_if(|outcome| outcome.changed)
            .wake_if(|outcome| outcome.changed)
            .claiming_with(|outcome| std::mem::take(&mut outcome.claims)),
            move |s, done| {
                filing.set(false);
                if done.as_ref().is_some_and(|outcome| outcome.changed) {
                    if let Some(marks) = marks {
                        s.claim_ui(Box::new(marks));
                    }
                }
                // Inline worlds complete while the verb still borrows its
                // panel. Land after that event, on this same action's node.
                s.after_event(move |s| {
                    let Some(panel) = s.panel(slot) else { return };
                    let mut panel = panel.borrow_mut();
                    let Some(mailbox) = panel.as_any().downcast_mut::<Mailbox>() else { return };
                    if !Rc::ptr_eq(&mailbox.filing, &filing) { return; }
                    let nav = match done {
                        Some(outcome) if outcome.changed => outcome.landing.and_then(|landing| mailbox.land(landing)),
                        other => {
                            mailbox.restore_marks(&keys);
                            if let Some((message, error)) = other.and_then(|outcome| outcome.notice) {
                                s.notify(message, error);
                            }
                            None
                        }
                    };
                    drop(panel);
                    if let Some(nav) = nav { s.nav_within(nav); }
                });
            },
        );
    }
}

/// Whether this move would actually move this letter: it needs somewhere to
/// go, and it may not be there already.
pub fn movable(store: &Store, id: MailId, to: To) -> bool {
    match to {
        To::Role(role) => {
            model::can_file(store, id, role) && !model::already_filed(store, id, role)
        }
        To::Back => {
            model::put_back_target(store, id).is_some_and(|f| f != model::folder_of(store, id))
        }
    }
}

/// The claim one letter's move makes on the world, ready to be reversed.
pub fn moved(store: &Store, mail: MailId, from: i64, to: To) -> Box<dyn Intent> {
    match to {
        To::Role(role) => Box::new(Filed {
            mail,
            from_folder: from,
            role,
            was_trashed: model::trashed_from(store, mail),
        }),
        To::Back => Box::new(PutBack {
            mail,
            trash: from,
            to: model::put_back_target(store, mail).unwrap_or(from),
        }),
    }
}

/// The verb a mailbox wears beside *delete* — its id, the word its button
/// says, and the letter it wears — or `None` for a list that has none.
///
/// The inbox archives; the spam list takes a conversation back out of the
/// junk, which is the same move in the other direction; the trash puts a
/// conversation back where it was deleted from, which is that move again
/// and the only one it has. The archive has none, because the mail is
/// already out of the inbox, and Sent has none, because filing what you
/// wrote is not what anybody asked for.
fn keep_verb(role: Role) -> Option<(&'static str, &'static str, char)> {
    match role {
        Role::Inbox => Some(("mail.archive", "archive", 'a')),
        Role::Spam => Some(("mail.not_spam", "not spam", 'n')),
        Role::Trash => Some(("mail.put_back", "put back", 'p')),
        Role::Archive | Role::Sent => None,
    }
}

fn threads_said(n: usize) -> String {
    if n == 1 {
        "1 conversation".into()
    } else {
        format!("{n} conversations")
    }
}

/// The marks a batch verb consumed, held as a handle to the table that had
/// them: a mark is context rather than a row, so putting it back is putting
/// it back *there*.
struct RestoredMarks {
    panel: Instance,
    keys: Vec<i64>,
}

impl RestoredMarks {
    fn edit(&self, f: impl FnOnce(&mut Mailbox)) {
        let mut b = self.panel.borrow_mut();
        if let Some(m) = b.as_any().downcast_mut::<Mailbox>() {
            f(m);
        }
    }
}

impl UiIntent for RestoredMarks {
    fn describe(&self) -> String {
        format!("{} marked", threads_said(self.keys.len()))
    }

    fn reverse(&self) {
        self.edit(|m| m.restore_marks(&self.keys));
    }

    fn reapply(&self) {
        self.edit(Mailbox::clear_marks);
    }
}
