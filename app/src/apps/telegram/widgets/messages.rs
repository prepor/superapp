//! The messages list, drawn: the shared rich table over the panel's own
//! list, seeded with the chat it is about.

use kernel::panel::PanelId;
use kernel::richtable::ListState;
use makepad_widgets::*;

use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::{self, MsgHit};
use super::super::panels::{Chat, Messages};
use super::super::search_index::MessageSource;

/// What the table needs to know about a messages list's rows.
pub struct MessagesRows;

impl RowSpec for MessagesRows {
    type Src = &'static MessageSource;
    type Panel = Messages;

    fn list(panel: &mut Messages) -> &mut ListState<Self::Src> {
        panel.list_mut()
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// Two lines: where and who with the time, then the line itself.
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &MsgHit, selected: bool, marked: bool, now: f64) {
        let line = table::line(cx, row, selected, marked);
        let who = r.writer();
        let place = if who.is_empty() || who == r.chat_title {
            r.chat_title.clone()
        } else {
            format!("{} · {who}", r.chat_title)
        };
        line.label(cx, ids!(body.where_lbl)).set_text(cx, &place);
        line.label(cx, ids!(body.when_lbl))
            .set_text(cx, &model::when(r.date, now));
        line.label(cx, ids!(body.line_lbl))
            .set_text(cx, &r.line(now));
    }

    fn label(r: &MsgHit, now: f64) -> String {
        r.line(now)
    }

    /// The chat, opened at that line.
    fn target(r: &MsgHit) -> PanelId {
        Chat::topic_at(r.chat, r.topic, r.id)
    }

    fn seed_filter(panel: &Messages) -> String {
        panel.seed_filter()
    }

    fn focus_filter_on_open() -> bool { true }

    fn empty_line(panel: &Messages, filter: &str) -> String {
        if panel.is_replies() {
            let (loading, failed) = panel.reply_status();
            return if loading {
                "loading unread replies and mentions…"
            } else if failed {
                "could not load replies and mentions · refresh to retry"
            } else if panel.pending_count() == 0 {
                "no unread replies or mentions"
            } else {
                "no loaded replies or mentions under this filter · refresh to retry"
            }.to_string();
        }
        if filter.trim().is_empty() {
            "no messages".to_string()
        } else {
            "nothing under this filter".to_string()
        }
    }
}

/// The widget: the shared table.
#[derive(Script, ScriptHook, Widget)]
pub struct MessagesPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<MessagesRows>,
}

impl Widget for MessagesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Self { view, table, .. } = self;
        table.handle_event(cx, event, scope, view);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Self {
            view,
            suggest,
            table,
            ..
        } = self;
        table.draw(cx, scope, walk, view, suggest)
    }
}
