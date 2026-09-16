//! The catalogue, drawn: the shell's table over the pages, each row the
//! title over its summary, the slug and the date muted at the right, the
//! kind's caption before the first row of each kind.

use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::{self, PageRow};
use super::super::panels::{self, Catalogue, Page};

pub struct Rows;
impl RowSpec for Rows {
    type Src = &'static SqlSource<PageRow, String>;
    type Panel = Catalogue;
    fn list(p: &mut Catalogue) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Catalogue) -> String {
        p.filter.clone()
    }
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &PageRow, selected: bool, marked: bool, _: f64) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.title_lbl)).set_text(cx, &r.title);
        line.label(cx, ids!(body.summary_lbl)).set_text(cx, &r.summary);
        line.label(cx, ids!(body.slug_lbl)).set_text(cx, &r.slug);
        line.label(cx, ids!(body.date_lbl)).set_text(cx, &fmt_date(r.updated));
    }
    fn section(row: &PageRow, previous: Option<&PageRow>) -> Option<String> {
        previous
            .is_none_or(|p| p.kind != row.kind)
            .then(|| model::caption(&row.kind).to_string())
    }
    fn label(r: &PageRow, _: f64) -> String {
        r.title.clone()
    }
    fn target(r: &PageRow) -> PanelId {
        Page::id(&r.slug)
    }
    fn empty_line(_: &Catalogue, filter: &str) -> String {
        if filter.is_empty() {
            "no pages yet — new page, or import the folder"
        } else {
            "no pages under this filter"
        }
        .into()
    }
    fn swipe_verbs(_: &Catalogue) -> [Option<&'static str>; 2] {
        [Some("kb.delete"), None]
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CataloguePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<Rows>,
}

impl Widget for CataloguePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // The **ask kb** root: the chat it promised, opened on the first
        // draw that has a session to open it with.
        if let Some(props) = scope.props.get::<PanelProps>().cloned() {
            let owed = super::with::<Catalogue, _>(&props, |c| c.take_ask().then_some(c.slot()));
            if let Some(Some(slot)) = owed {
                if let Some(s) = scope.data.get_mut::<Session>() {
                    panels::ask(s, slot);
                }
            }
        }
        self.table.draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}
