//! The chat list, drawn: the shared rich table over the panel's own list.
//!
//! Everything a list does — the filter and its completion, the cursor walk
//! that previews, the marks, the keys — is the table widget's. What telegram
//! supplies is the [`RowSpec`]: the row template, how to fill a row's two
//! lines, what a script calls it, and the chat it opens.

use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::{self, ChatRow};
use super::super::panels::Chats;

/// What the table needs to know about a chat list's rows.
pub struct ChatsRows;

impl RowSpec for ChatsRows {
    type Src = &'static SqlSource<ChatRow, String>;
    type Panel = Chats;

    fn list(panel: &mut Chats) -> &mut ListState<Self::Src> {
        panel.list_mut()
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// Two lines: the title and the last message's time, then what the
    /// chat last said and the count. Bold while anything is unread — a
    /// twin, not a weight, because a label's style is not a runtime value.
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &ChatRow, selected: bool, marked: bool, now: f64) {
        let line = table::line(cx, row, selected, marked);
        let unread = !r.is_forum && (r.unread > 0 || r.unread_mentions > 0);
        for (path, on) in [(ids!(body.title_lbl), !unread), (ids!(body.title_b), unread)] {
            let lbl = line.label(cx, path);
            lbl.set_text(cx, if on { &r.title } else { "" });
            lbl.set_visible(cx, on);
        }
        // My last line's state, before the time: the failed one in the
        // colour errors get.
        let mark = r.state_mark();
        let failed = mark == "failed";
        let state = line.label(cx, ids!(body.state_lbl));
        state.set_text(cx, if failed { "" } else { mark });
        state.set_visible(cx, !mark.is_empty() && !failed);
        line.label(cx, ids!(body.state_err)).set_visible(cx, failed);
        line.label(cx, ids!(body.when_lbl))
            .set_text(cx, &model::when(r.last, now));
        line.label(cx, ids!(body.preview_lbl))
            .set_text(cx, &if r.is_forum { "select topics to show as chats".into() } else { r.preview(now) });
        line.label(cx, ids!(body.pinned_lbl))
            .set_visible(cx, r.pinned > 0);
        // Personal replies and mentions stay visible beside the ordinary
        // unread count, including in muted groups.
        let mentions = line.view(cx, ids!(body.mentions));
        mentions.set_visible(cx, r.unread_mentions > 0);
        mentions.label(cx, ids!(lbl)).set_text(cx, &format!("@{}", r.unread_mentions));
        let count = r.unread.to_string();
        let (ink, outlined) = (r.unread > 0 && !r.muted, r.unread > 0 && r.muted);
        let badge = line.view(cx, ids!(body.badge));
        badge.set_visible(cx, ink);
        badge.label(cx, ids!(lbl)).set_text(cx, &count);
        let muted = line.view(cx, ids!(body.badge_muted));
        muted.set_visible(cx, outlined);
        muted.label(cx, ids!(lbl)).set_text(cx, &count);
    }

    fn label(r: &ChatRow, _now: f64) -> String {
        r.title.clone()
    }

    fn target(r: &ChatRow) -> PanelId {
        Chats::target(r)
    }

    fn empty_line(panel: &Chats, filter: &str) -> String {
        panel.empty_line(filter)
    }
}

/// The widget: the shared table, and nothing else of its own.
#[derive(Script, ScriptHook, Widget)]
pub struct ChatsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The completion box, drawn over the rows after everything else.
    #[live]
    suggest: View,
    #[rust]
    table: TableView<ChatsRows>,
}

impl Widget for ChatsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Esc over a list empties its marks, which is the table's own; while
        // lines wait here for the chat they are forwarded to, it lets those
        // go as well — one gesture, meaning *let this go*, and *clear* on the
        // bar is the same word for the same two things. Only the focused
        // list's esc, a key reaching every panel drawn.
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Escape {
                let slot = scope.props.get::<PanelProps>().map(|p| p.slot);
                if let Some(s) = scope.data.get_mut::<Session>() {
                    if slot.is_some() && s.focus() == slot && super::super::runtime::of(s.store()).take_forward().is_some() {
                        s.redraw();
                    }
                }
            }
        }
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
