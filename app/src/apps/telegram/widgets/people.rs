//! The people, drawn: the shared rich table over the address book or a
//! group's members, one row shape for both.

use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use makepad_widgets::*;

use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::Person;
use super::super::panels::{Chat, Peer, People};

/// What the table needs to know about a people list's rows.
pub struct PeopleRows;

impl RowSpec for PeopleRows {
    type Src = &'static SqlSource<Person, i64>;
    type Panel = People;

    fn list(panel: &mut People) -> &mut ListState<Self::Src> {
        panel.list_mut()
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// The name, and under it the presence and the username, muted.
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &Person, selected: bool, marked: bool, _now: f64) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.name_lbl)).set_text(cx, &r.name);
        line.label(cx, ids!(body.detail_lbl))
            .set_text(cx, &r.detail());
    }

    fn label(r: &Person, _now: f64) -> String {
        r.name.clone()
    }

    /// A contact opens as the chat with them — a new conversation starts
    /// here — and a member as their card.
    fn target(r: &Person) -> PanelId {
        if r.group.is_some() {
            Peer::id(r.id)
        } else {
            Chat::id(r.id)
        }
    }

    fn seed_filter(panel: &People) -> String {
        panel.seed_filter()
    }

    fn empty_line(panel: &People, filter: &str) -> String {
        if !filter.trim().is_empty() {
            "nobody under this filter".to_string()
        } else if panel.group().is_some() {
            "nobody in it".to_string()
        } else {
            "nobody in the address book".to_string()
        }
    }
}

/// The widget: the shared table.
#[derive(Script, ScriptHook, Widget)]
pub struct PeoplePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<PeopleRows>,
}

impl Widget for PeoplePanel {
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
