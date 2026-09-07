//! The messages list, drawn: the shared rich table over the panel's own
//! list, seeded with the chat it is about.

use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use makepad_widgets::*;

use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::{self, MsgHit};
use super::super::panels::{Chat, Messages};

/// What the table needs to know about a messages list's rows.
pub struct MessagesRows;

impl RowSpec for MessagesRows {
    type Src = &'static SqlSource<MsgHit, i64>;
    type Panel = Messages;

    fn list(panel: &mut Messages) -> &mut ListState<Self::Src> {
        panel.list_mut()
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// Two lines: where and who with the time, then the line itself.
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &MsgHit, selected: bool, marked: bool) {
        let line = table::line(cx, row, selected, marked);
        let who = r.writer();
        let place = if who.is_empty() || who == r.chat_title {
            r.chat_title.clone()
        } else {
            format!("{} · {who}", r.chat_title)
        };
        line.label(cx, ids!(body.where_lbl)).set_text(cx, &place);
        line.label(cx, ids!(body.when_lbl))
            .set_text(cx, &model::when(r.date, model::now()));
        line.label(cx, ids!(body.line_lbl))
            .set_text(cx, &r.line(model::now()));
    }

    fn label(r: &MsgHit) -> String {
        r.line(model::now())
    }

    /// The chat, opened at that line.
    fn target(r: &MsgHit) -> PanelId {
        Chat::at(r.chat, r.id)
    }

    fn seed_filter(panel: &Messages) -> String {
        panel.seed_filter()
    }

    fn empty_line(_panel: &Messages, filter: &str) -> String {
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
        super::tell_now(scope);
        let Self {
            view,
            suggest,
            table,
            ..
        } = self;
        table.draw(cx, scope, walk, view, suggest)
    }
}
