//! One mailbox: a rich table of conversations, and the two batch verbs over
//! what is marked in it.
//!
//! One instance type for both tags. The role is what the source, the base
//! condition and the bar are picked by; nothing else about a list of mail
//! changes with the folder it is over.

use std::any::Any;
use std::rc::Rc;

use kernel::effect::World;
use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlSource};
use kernel::session::{Action, Instance, Session};
use kernel::store::Store;

use super::super::effects::Filed;
use super::super::model::{self, MailId, Role, ThreadHead, MAILBOX_PAGE};
use super::Message;

/// A mailbox panel: the folder's conversations, its cursor, and its marks.
pub struct Mailbox {
    id: PanelId,
    role: Role,
    store: Rc<Store>,
    slot: SlotId,
    list: ListState<&'static SqlSource<ThreadHead, i64>>,
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
        self.list.table().rows(&self.store, lo, hi)
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

    /// The cursor after a row has been filed out from under it: it stays
    /// where it stood, which is now the row below. Answers the preview of
    /// whatever it landed on.
    pub fn advance(&mut self) -> Option<Nav> {
        let store = self.store.clone();
        self.list.sync(&store);
        let i = self.list.cursor_index(&store)?;
        let row = self.list.set_cursor(&store, i)?;
        Some(self.preview(row.target))
    }

    /// Whether this list's rows may be archived: only the inbox's, which is
    /// the same answer [`Mailbox::verbs`] gives the bar. A gesture asks it
    /// too, so a finger and a button can never offer different verbs.
    #[must_use]
    pub fn archives(&self) -> bool {
        self.role == Role::Inbox
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
    /// Only the inbox archives: everywhere else the mail is already out of it,
    /// and a bar may not wear a verb that would do nothing (or, from Sent,
    /// something nobody asked for). Delete is the one move every mailbox has —
    /// the trash is where mail goes from anywhere.
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
        if self.role == Role::Inbox {
            v.push(Verb::run("mail.archive", format!("archive {n}"), Some('a')));
        }
        v.push(Verb::run("mail.delete", format!("delete {n}"), Some('d')));
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
            "mail.archive" => self.file_marked(s, "archive"),
            "mail.delete" => self.file_marked(s, "trash"),
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
        })
    }
}

// -- the batch verbs ---------------------------------------------------------

impl Mailbox {
    /// Files every marked conversation into `role`, as one undoable action:
    /// the marks come off, the folder's own copies of each conversation
    /// move, and undo brings both back.
    ///
    /// It closes nothing. A panel reading one of the filed conversations is
    /// somebody's own window; what happens instead is the cursor walk — it
    /// lands on the nearest row that stayed and previews it, and the join
    /// rule puts that preview where the old one was.
    fn file_marked(&mut self, s: &mut Session, role: &'static str) {
        let keys = self.list.marks().keys();
        if keys.is_empty() {
            return;
        }
        let store = self.store.clone();

        // Which mails move: the folder's own copies of each marked
        // conversation, minus the ones there is nowhere to put or nothing
        // to do for.
        let mut moving: Vec<(MailId, i64)> = Vec::new();
        for th in &keys {
            for id in folder_mails(&store, self.role, *th) {
                if !model::can_file(&store, id, role) || model::already_filed(&store, id, role) {
                    continue;
                }
                moving.push((id, model::folder_of(&store, id)));
            }
        }
        if moving.is_empty() {
            s.notify(format!("nothing to {}", word_of(role)), false);
            return;
        }

        // The marks come off before the action, so the bar it redraws has no
        // count left on it; a refused write puts them straight back.
        self.clear_marks();

        let mut intents: Vec<Box<dyn Intent>> = Vec::new();
        // A mark is context rather than a row, so putting it back is putting
        // it back *here*: the intent holds the instance this verb is running
        // on, which the session is holding too.
        if let Some(inst) = s.panel(self.slot) {
            intents.push(Box::new(RestoredMarks {
                panel: inst,
                keys: keys.clone(),
            }));
        }
        intents.extend(moving.iter().map(|(mail, from)| {
            Box::new(Filed {
                mail: *mail,
                from_folder: *from,
                role,
            }) as Box<dyn Intent>
        }));

        let ids: Vec<MailId> = moving.iter().map(|(id, _)| *id).collect();
        let label = format!("{} {}", word_of(role), threads_said(keys.len()));
        let done = s.act(
            Action::writing("file", label, move |tx| {
                for id in &ids {
                    model::file_tx(tx, *id, role)?;
                }
                Ok(())
            })
            .claiming(intents),
        );
        if done.is_none() {
            self.restore_marks(&keys);
            return;
        }

        // The cursor stands where it stood; the rows under it may have left,
        // so it lands on the nearest one that stayed and previews it — a step
        // like any other in the walk.
        if let Some(nav) = self.advance() {
            s.nav(nav);
        }
    }
}

/// The mails of one conversation that sit in this mailbox — what filing the
/// row moves. The row's `target` is a mail of the folder by construction, so
/// asking the mail what it is filed as cannot disagree with the list.
fn folder_mails(store: &Store, role: Role, thread: i64) -> Vec<MailId> {
    let Some(head) = model::thread_head(store, role, thread) else {
        return Vec::new();
    };
    model::thread_siblings(store, head.target)
}

/// The verb's own word for a role.
pub(super) fn word_of(role: &str) -> &'static str {
    if role == "trash" {
        "delete"
    } else {
        "archive"
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

impl Intent for RestoredMarks {
    fn describe(&self) -> String {
        format!("{} marked", threads_said(self.keys.len()))
    }

    fn reverse(&self, _w: &World) -> Result<(), String> {
        self.edit(|m| m.restore_marks(&self.keys));
        Ok(())
    }

    fn reapply(&self, _w: &World) -> Result<(), String> {
        self.edit(Mailbox::clear_marks);
        Ok(())
    }
}
