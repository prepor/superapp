//! The chats, as a rich table: the title, the model, when it last moved,
//! and what its newest run is doing.
//!
//! It is where one finds the chat that stopped short — a run *waiting* on a
//! call nobody has shown, a run that *failed* — so the run's word is a
//! column and two of the tags narrow to it.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlLanding, SqlSource};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, ChatId, ChatRow, CHATS, CHATS_PAGE};
use super::Chat;

/// The list's own state: the shared engine over one static source.
type ChatList = ListState<&'static SqlSource<ChatRow, i64>>;

/// The agents list.
pub struct Agents {
    id: PanelId,
    store: Rc<Store>,
    slot: SlotId,
    list: ChatList,
    deleting: Option<PendingDelete>,
}

struct PendingDelete {
    keys: Vec<ChatId>,
    result: tokio::sync::oneshot::Receiver<(bool, Option<SqlLanding<ChatRow>>)>,
}

impl Agents {
    pub const TAG: Tag = Tag("agents");

    /// The identity of the one list.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// The table, its cursor and its marks — what the widget draws and
    /// walks, and the tests' door onto what it did.
    pub fn list_mut(&mut self) -> &mut ChatList {
        &mut self.list
    }

    /// How many rows the filter shows.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len(&self.store)
    }

    /// Puts the cursor on row `i` — a click — and answers the preview.
    #[cfg(test)]
    pub fn go(&mut self, i: usize) -> Option<Nav> {
        let store = self.store.clone();
        let row = self.list.set_cursor(&store, i)?;
        Some(self.preview(row.id))
    }

    /// Space: the mark on the cursor's row, toggled.
    #[cfg(test)]
    pub fn toggle_mark(&mut self) -> bool {
        let store = self.store.clone();
        self.list.toggle_mark(&store)
    }

    /// The chat this row shows, beside the list.
    #[must_use]
    pub fn preview(&self, chat: ChatId) -> Nav {
        Nav::Preview {
            from: self.slot,
            id: Chat::id(chat),
        }
    }

    /// Marks the table again — what a refused write hands back.
    pub fn restore_marks(&mut self, keys: &[ChatId]) {
        self.list.marks_mut().extend(keys.iter().copied());
    }

    /// Takes them all off.
    pub fn clear_marks(&mut self) {
        self.list.clear_marks();
    }

    /// The batch verb: the marked chats, with their turns, runs and calls,
    /// as one undoable node.
    fn delete_marked(&mut self, s: &mut Session) {
        if self.deleting.is_some() { return; }
        let keys = self.list.marks().keys();
        if keys.is_empty() { return; }
        let cursor = self.list.after_removal();
        self.clear_marks();
        let edit = model::delete_chats_plan(&keys).and_then(move |tx, count| {
            Ok((count, cursor.map(|cursor| cursor.read(tx)).transpose()?))
        });
        let (done, result) = tokio::sync::oneshot::channel();
        let slot = self.slot;
        s.act_async(edit, move |s, committed| {
            let (count, landing) = committed.unwrap_or((0, None));
            let _ = done.send((count > 0, landing));
            // Fixtures can complete while this panel is still borrowed.
            // The session drains this event before the next edit completion,
            // keeping the preview on the deletion's own history node.
            s.after_event(move |s| {
                if let Some(panel) = s.panel(slot) {
                    if let Some(agents) = panel.borrow_mut().as_any().downcast_mut::<Agents>() { agents.poll(s); }
                }
            });
        });
        self.deleting = Some(PendingDelete { keys, result });
        self.poll(s);
    }

    pub fn poll(&mut self, s: &mut Session) {
        let Some(pending) = &mut self.deleting else { return; };
        let (changed, landing) = match pending.result.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => (false, None),
        };
        let pending = self.deleting.take().expect("one deletion completion");
        if !changed { self.restore_marks(&pending.keys); }
        else if let Some(row) = landing.and_then(|landing| self.list.land(landing)) {
            s.nav_within(self.preview(row.id));
        }
        s.redraw();
    }
}

impl Panel for Agents {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "agents".into()
    }

    /// Four wide, six tall: a list is the one panel that wants the column.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 6)
    }

    fn about(&self) -> String {
        format!(
            "agents: every conversation with the assistant, newest first — its title, \
             the model that answered in it, when it last moved, and what its newest \
             round is doing ({} of them under this filter). The cursor shows a chat \
             beside the list; marking rows offers to delete them, which is undoable.",
            self.list.len(&self.store)
        )
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// *new* is always there — a fresh conversation opens joined to the
    /// list, where the cursor's previews go — and then, while something is
    /// marked, the one thing a list of chats can do to them.
    fn verbs(&self) -> Vec<Verb> {
        let mut v = vec![Verb::go(
            "agent.new",
            "new",
            Some('n'),
            Nav::Open {
                from: self.slot,
                id: Chat::new_id(),
                fresh: false,
            },
        )];
        let n = self.list.marks().len();
        if n > 0 {
            v.push(Verb::run("agent.delete", format!("delete {n}"), Some('d')));
        }
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "agent.delete" {
            self.delete_marked(s);
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct AgentsKind;

impl PanelKind for AgentsKind {
    fn tag(&self) -> Tag {
        Agents::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Agents {
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
            list: ListState::new(&CHATS, CHATS_PAGE),
            deleting: None,
        })
    }
}
