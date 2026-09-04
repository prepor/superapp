//! A mailbox, drawn: the shared rich table over the panel's own list.
//!
//! Everything a list does — the filter and its completion, the cursor walk
//! that previews, the marks, the band the filter hides them in, the keys —
//! is the table widget's. What mail supplies is the four short functions of
//! a [`RowSpec`]: the row template, how to fill a row, what a script calls
//! it, and what it opens.
//!
//! The bar is the instance's: *sync*, the batch verbs while there are marks,
//! and the two verbs about the set itself.

use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::ThreadHead;
use super::super::panels::{Mailbox, Message};

/// What the table needs to know about a mailbox's rows.
pub struct MailboxRows;

impl RowSpec for MailboxRows {
    type Src = &'static SqlSource<ThreadHead, i64>;
    type Panel = Mailbox;

    fn list(panel: &mut Mailbox) -> &mut ListState<Self::Src> {
        panel.list_mut()
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// Two lines: who wrote in the conversation and when, then the topic
    /// under them. A conversation with anything unread in it is bold — both
    /// lines, so the row reads as one thing.
    fn populate(cx: &mut Cx, row: &WidgetRef, t: &ThreadHead, selected: bool, marked: bool) {
        let line = table::line(cx, row, selected, marked);
        let who = t.who_line();
        // Bold is a twin, not a runtime weight. The hidden one is emptied
        // rather than merely stood down, so no row can leave its text
        // behind for the next one that reuses it.
        for (path, text, on) in [
            (ids!(body.who_lbl), who.as_str(), !t.unread),
            (ids!(body.who_b), who.as_str(), t.unread),
            (ids!(body.topic_lbl), t.topic.as_str(), !t.unread),
            (ids!(body.topic_b), t.topic.as_str(), t.unread),
        ] {
            let lbl = line.label(cx, path);
            lbl.set_text(cx, if on { text } else { "" });
            lbl.set_visible(cx, on);
        }
        line.label(cx, ids!(body.date_lbl))
            .set_text(cx, &fmt_date(t.last));
    }

    /// The topic: what a conversation is called, whichever of its letters
    /// you read it off.
    fn label(t: &ThreadHead) -> String {
        t.topic.clone()
    }

    /// The mail the row opens: the folder's oldest unread message of the
    /// conversation, else its newest.
    fn target(t: &ThreadHead) -> PanelId {
        Message::id(t.target)
    }

    fn empty_line(_panel: &Self::Panel, filter: &str) -> String {
        if filter.trim().is_empty() {
            "nothing here".to_string()
        } else {
            "no conversation under this filter".to_string()
        }
    }

    /// Triage under a finger: left keeps the conversation, right deletes —
    /// the same two verbs the bar wears over the marks, and asked of the
    /// same panel, so a finger and a button can never offer different ones.
    /// Only the inbox archives and only the spam list takes a conversation
    /// out of the junk; the other two sweep one way alone. Sweeping left
    /// brings the keeping verb in from the right, which is the side of the
    /// bar its button sits on.
    fn swipe_verbs(panel: &Mailbox) -> [Option<&'static str>; 2] {
        [panel.keeps(), Some("mail.delete")]
    }
}

/// The widget: the shared table, and the one thing a mailbox adds to it —
/// the filter a panel opened on.
#[derive(Script, ScriptHook, Widget)]
pub struct MailboxPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The completion box, drawn over the rows after everything else.
    #[live]
    suggest: View,
    #[rust]
    table: TableView<MailboxRows>,
    /// Whether the panel's own filter has been typed in. Once, before the
    /// first draw; after that the field is the person's, empty included. A
    /// panel replaced in place is a new widget, so a mailbox that lands on
    /// another identity is seeded afresh rather than left holding the old
    /// one's filter.
    #[rust]
    seeded: bool,
}

impl Widget for MailboxPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Self { view, table, .. } = self;
        table.handle_event(cx, event, scope, view);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.seed(cx, scope);
        let Self {
            view,
            suggest,
            table,
            ..
        } = self;
        table.draw(cx, scope, walk, view, suggest)
    }
}

impl MailboxPanel {
    /// The filter the panel's identity carries, typed into the field before
    /// the table reads it back. The table's own seed is a constant per
    /// spec, and this one is per panel — a contact's *messages from …*.
    fn seed(&mut self, cx: &mut Cx2d, scope: &mut Scope) {
        if self.seeded {
            return;
        }
        self.seeded = true;
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let filter = {
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<Mailbox>()
                .map(|m| m.seed_filter())
        };
        if let Some(f) = filter.filter(|f| !f.is_empty()) {
            self.view.text_input(cx, ids!(filter_input)).set_text(cx, &f);
        }
    }
}
