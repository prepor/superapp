//! The agents list, drawn: the shared rich table over the panel's own list.
//!
//! Everything a list does — the filter and its completion, the cursor walk
//! that previews, the marks, the band the filter hides them in, the keys —
//! is the table widget's. What this app supplies is the four short
//! functions of a [`RowSpec`]: the row template, how to fill a row, what a
//! script calls it, and what it opens.
//!
//! One line a chat: its title, the model that answered in it, what its
//! newest round is doing, and when it last moved. The run's word is the
//! column this list is for — it is where one finds the chat that stopped
//! short — so *failed* is drawn in the ink the rest of the row is, and the
//! two that are still going are muted.

use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::{self, ChatRow};
use super::super::panels::{Agents, Chat};

/// What the table needs to know about the agents list's rows.
pub struct ChatRows;

impl RowSpec for ChatRows {
    type Src = &'static SqlSource<ChatRow, i64>;
    type Panel = Agents;

    fn list(panel: &mut Agents) -> &mut ListState<Self::Src> {
        panel.list_mut()
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// One line: the title, then the model, the run's word and the date on
    /// the columns the header draws.
    fn populate(cx: &mut Cx, row: &WidgetRef, c: &ChatRow, selected: bool, marked: bool) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.title_lbl)).set_text(cx, &c.title);
        line.label(cx, ids!(body.model_lbl))
            .set_text(cx, short_model(&c.model));
        // A twin apiece rather than a colour set at draw time, and the one
        // not shown is emptied rather than merely stood down: a row that
        // scrolls out is reused by the next one in.
        let word = state_word(&c.status);
        let failed = c.status == model::FAILED;
        for (path, on) in [
            (ids!(body.state_lbl), !failed),
            (ids!(body.state_ink), failed),
        ] {
            let lbl = line.label(cx, path);
            lbl.set_text(cx, if on { word } else { "" });
            lbl.set_visible(cx, on && !word.is_empty());
        }
        line.label(cx, ids!(body.date_lbl))
            .set_text(cx, &fmt_date(c.updated));
    }

    /// The chat's title: what a person calls the conversation, and what a
    /// script addresses the row by.
    fn label(c: &ChatRow) -> String {
        c.title.clone()
    }

    fn target(c: &ChatRow) -> PanelId {
        Chat::id(c.id)
    }

    fn empty_line(_panel: &Agents, filter: &str) -> String {
        if filter.trim().is_empty() {
            "no chats yet".to_string()
        } else {
            "no chat under this filter".to_string()
        }
    }
}

/// The word a row wears for its newest run: nothing for a chat that is
/// done, or one nobody has sent in — a finished conversation is the
/// ordinary case, and the column is for the ones that are not.
fn state_word(status: &str) -> &str {
    match status {
        model::DONE | "" => "",
        other => other,
    }
}

/// The model, as a column has room for it: `@cf/zai-org/glm-5.3-flash`
/// reads as `glm-5.3-flash`. The whole name is on the chat's own panel.
fn short_model(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

/// The widget: the shared table, and nothing of its own.
#[derive(Script, ScriptHook, Widget)]
pub struct AgentAgentsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The filter's completion box, drawn over the rows after everything
    /// else.
    #[live]
    suggest: View,
    #[rust]
    table: TableView<ChatRows>,
}

impl Widget for AgentAgentsPanel {
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
