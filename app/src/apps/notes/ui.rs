use super::{
    panels::{Editor, NoteList},
    widgets::{EditorPanel, NotesPanel},
};
use crate::shell::app_ui::{AppUi, Setup};
use crate::shell::catalog::{panel, workspace_on};
use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.NotesRowBody = View {
        width: Fill, height: Fit, flow: Right, align: Align{y: 0.5}, spacing: 12
        View { width: Fill, height: Fit, flow: Down
            title_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
        }
        date_lbl := mod.widgets.SLabel { draw_text +: { color: #909090 } }
    }
    mod.widgets.NotesRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.NotesRowBody{} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.NotesRowBody{} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.NotesRowBody{} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.NotesRowBody{} }
        mod.widgets.TblHairline{}
    }
    mod.widgets.NotesPanel = set_type_default() do #(NotesPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left:12,right:12,top:10,bottom:10}
        filter_input := mod.widgets.TblFilter{}
        filter_err_lbl := mod.widgets.TblErr{}
        View { width: Fill, height: 8 }
        View { width: Fill, height: Fit, flow: Right, padding: Inset{left:8,right:8,bottom:3}
            View { width: Fill, height: Fit
                mod.widgets.SSection { text: "NOTE" }
            }
            mod.widgets.SSection { text: "MODIFIED" }
        }
        mod.widgets.TblHeadRule{}
        empty_lbl := mod.widgets.TblEmpty{}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.NotesRow{}
            caption := mod.widgets.TblCaption{}
            band_rule := mod.widgets.TblBandRule{}
        }
        suggest: mod.widgets.TblSuggest{}
    }
    mod.widgets.NotesEditorPanel = set_type_default() do #(EditorPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        path_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis
            margin: Inset{left:12,right:12,top:10,bottom:6}
            draw_text +: { color: #909090 }
        }
        body_input := mod.widgets.SourceInput{}
        error_lbl := mod.widgets.SLabel {
            width: Fill, margin: Inset{left:12,right:12,top:6,bottom:6}
            draw_text +: { color: #a01500 }
        }
        status_lbl := mod.widgets.SSection { margin: Inset{left:12,right:12,top:6,bottom:10} }
    }
}

pub struct Ui;
pub static UI: Ui = Ui;
impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            NoteList::TAG => Some(live_id!(notes_list_tpl)),
            Editor::TAG => Some(live_id!(notes_editor_tpl)),
            _ => None,
        }
    }
    fn scenes(&self) -> Vec<Scene<Setup>> {
        vec![Scene::new("notes",(600.0,700.0))
            .node("empty",panel(|_|NoteList::id(),""))
            .node("writing",workspace_on(|_|NoteList::id(),"click \"new note\"\nwait 600\nclick \"editor\"\npaste \"# A place to think\\n\\nKeep **bold ideas**, *small details*, and `plain code`.\\n\\n- Write freely\\n- Everything is saved automatically\"\nwait 600"))
            .sized((1200.0,700.0))]
    }
}
