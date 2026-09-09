use super::{
    markdown, model,
    panels::{Editor, NoteList},
};
use crate::shell::hosted::PanelProps;
use crate::shell::keys::Letters;
use crate::shell::widgets::{
    source_input,
    table::{self, RowSpec, TableView},
};
use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;
use source_input::SourceInputWidgetRefExt;

pub struct Rows;
impl RowSpec for Rows {
    type Src = &'static SqlSource<model::Note, i64>;
    type Panel = NoteList;
    fn list(p: &mut NoteList) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &NoteList) -> String {
        p.filter.clone()
    }
    fn populate(
        cx: &mut Cx,
        row: &WidgetRef,
        r: &model::Note,
        selected: bool,
        marked: bool,
        _: f64,
    ) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.title_lbl)).set_text(cx, &r.title);
        line.label(cx, ids!(body.date_lbl))
            .set_text(cx, &fmt_date(r.modified));
    }
    fn label(r: &model::Note, _: f64) -> String {
        r.title.clone()
    }
    fn target(r: &model::Note) -> PanelId {
        Editor::note(r.id)
    }
    fn empty_line(_: &NoteList, filter: &str) -> String {
        if filter.is_empty() {
            "no notes yet — create a new note"
        } else {
            "no notes under this filter"
        }
        .into()
    }
    fn swipe_verbs(_: &NoteList) -> [Option<&'static str>; 2] {
        [Some("notes.delete"), None]
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct NotesPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<Rows>,
}
impl Widget for NotesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table
            .draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct EditorPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    shown: Option<u64>,
    #[rust]
    mounted: bool,
}
impl EditorPanel {
    fn input(&self, cx: &mut Cx) -> source_input::SourceInputRef {
        self.view.widget(cx, ids!(body_input)).as_source_input()
    }
}
impl Widget for EditorPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let input = self.input(cx);
        self.view.handle_event(cx, event, scope);
        if !self.mounted && self.shown.is_some() {
            self.mounted = true;
            if scope
                .data
                .get_mut::<Session>()
                .is_some_and(|s| s.focus() == Some(props.slot))
            {
                input.take_key_focus(cx);
            }
        }
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab && !input.key_focus(cx) {
                input.take_key_focus(cx);
            }
        }
        if let Event::Actions(actions) = event {
            if let Some(text) = input.changed(actions) {
                if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Editor>() {
                    let spans = markdown::spans(&text);
                    p.edited(text);
                    if let Some(mut input) = input.borrow_mut() {
                        input.set_spans(cx, spans);
                    }
                }
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.redraw();
                }
                self.view.redraw(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let input = self.input(cx);
        if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Editor>() {
            p.observe();
            if self.shown != Some(p.revision) {
                input.set_text(cx, &p.text);
                if let Some(mut input) = input.borrow_mut() {
                    input.set_spans(cx, markdown::spans(&p.text));
                }
                self.shown = Some(p.revision);
            }
            let read_only = !p.available
                || !scope
                    .data
                    .get_mut::<Session>()
                    .is_some_and(|s| s.writable());
            if input.is_read_only() != read_only {
                input.set_is_read_only(cx, read_only);
            }
            self.view
                .label(cx, ids!(status_lbl))
                .set_text(cx, p.status());
            self.view.label(cx, ids!(error_lbl)).set_text(cx, &p.error);
            self.view
                .widget(cx, ids!(error_lbl))
                .set_visible(cx, !p.error.is_empty());
            let path = self.view.label(cx, ids!(path_lbl));
            path.set_text(cx, p.path().unwrap_or(""));
            path.set_visible(cx, p.is_file());
            // The save chord is available while the file's caret is active.
            props.keyboard.keep(
                &self.view.widget(cx, ids!(body_input)),
                if p.is_file() {
                    Letters::ALL.minus(Letters::of(&['s']))
                } else {
                    Letters::ALL
                },
            );
        }
        self.view.draw_walk_all(cx, scope, walk);
        props.hits.add(
            "editor",
            input.area().rect(cx),
            MouseCursor::Text,
            props.slot,
        );
        for (label, path) in [
            ("editor status", ids!(status_lbl)),
            ("editor error", ids!(error_lbl)),
        ] {
            let widget = self.view.widget(cx, path);
            if widget.visible() {
                props.hits.add(
                    label,
                    widget.area().rect(cx),
                    MouseCursor::Default,
                    props.slot,
                );
            }
        }
        DrawStep::done()
    }
}
