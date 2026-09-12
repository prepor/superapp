//! Workshop uses the shell's table, form, button and panel vocabulary.

use super::widgets::{WorkshopDetail, WorkshopProjects, WorkshopReview, WorkshopWorkspaces};
use crate::shell::app_ui::AppUi;
use kernel::panel::Tag;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.WorkshopRowBody = View {
        width: Fill, height: Fit, flow: Right, align: Align{y:0.5}, spacing: 10
        View { width: Fill, height: Fit, flow: Down
            title_lbl := mod.widgets.SLabel { width: Fill, max_lines:1, text_overflow: TextOverflow.Ellipsis }
            unread_lbl := mod.widgets.SLabel { visible:false, width: Fill, max_lines:1, text_overflow: TextOverflow.Ellipsis
                draw_text +: { text_style: mod.widgets.SMonoStyle{font_family +: {latin +: {weight:700.0}}} }
            }
            detail_lbl := mod.widgets.SLabel { width: Fill, max_lines:1, text_overflow: TextOverflow.Ellipsis, draw_text +: {color:#909090} }
        }
        meta_lbl := mod.widgets.SLabel { draw_text +: {color:#909090} }
    }
    mod.widgets.WorkshopRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.WorkshopRowBody{} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.WorkshopRowBody{} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.WorkshopRowBody{} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.WorkshopRowBody{} }
        mod.widgets.TblHairline{}
    }
    mod.widgets.WorkshopTable = View {
        width: Fill, height: Fill, flow: Down
        padding: Inset{left:12,right:12,top:10,bottom:10}
        progress_lbl := mod.widgets.SLabel { visible:false, width:Fill, margin:Inset{bottom:8} }
        comparison := mod.widgets.SSelect { visible:false, width:Fill, margin:Inset{bottom:8} }
        filter_input := mod.widgets.TblFilter{}
        filter_err_lbl := mod.widgets.TblErr{}
        View {width:Fill,height:8}
        columns := View {width:Fill,height:Fit,flow:Right,padding:Inset{left:8,right:8,bottom:3}
            column_lbl := mod.widgets.SSection {width:Fill}
            right_lbl := mod.widgets.SSection{}
        }
        mod.widgets.TblHeadRule{}
        empty_lbl := mod.widgets.TblEmpty{}
        list := mod.widgets.SList {
            width:Fill,height:Fill,flow:Down,reuse_items:true
            row := mod.widgets.WorkshopRow{}
            caption := mod.widgets.TblCaption{}
            band_rule := mod.widgets.TblBandRule{}
        }
    }
    mod.widgets.WorkshopProjectsPanel = set_type_default() do #(WorkshopProjects::register_widget(vm)) {
        ..mod.widgets.WorkshopTable
        columns +: {column_lbl +: {text:"REPOSITORY"}, right_lbl +: {text:""}}
        suggest: mod.widgets.TblSuggest{}
    }
    mod.widgets.WorkshopWorkspacesPanel = set_type_default() do #(WorkshopWorkspaces::register_widget(vm)) {
        ..mod.widgets.WorkshopTable
        columns +: {column_lbl +: {text:"WORKSPACE"}, right_lbl +: {text:"ACTIVITY"}}
        suggest: mod.widgets.TblSuggest{}
    }
    mod.widgets.WorkshopReviewPanel = set_type_default() do #(WorkshopReview::register_widget(vm)) {
        ..mod.widgets.WorkshopTable
        progress_lbl +: {visible:true}
        comparison +: {visible:true}
        columns +: {column_lbl +: {text:"FILE"}, right_lbl +: {text:"LINES"}}
        suggest: mod.widgets.TblSuggest{}
    }

    mod.widgets.WorkshopMessage = View {
        width:Fill,height:Fit,flow:Down,spacing:5,padding:Inset{top:8,bottom:10}
        author_lbl := mod.widgets.SSection{}
        text_lbl := mod.widgets.SProseText {width:Fill,height:Fit,is_multiline:true}
        detail_lbl := mod.widgets.SLabel {width:Fill,draw_text +: {color:#909090}}
        open_btn := mod.widgets.SBtn {visible:false,text:"preview diff"}
        approval := View {visible:false,width:Fill,height:Fit,flow:Right,spacing:6
            approve_btn := mod.widgets.SBtn{text:"approve"}
            refuse_btn := mod.widgets.SBtn{text:"refuse"}
        }
        mod.widgets.TblHairline{}
    }
    mod.widgets.WorkshopCodeLine = View {
        width:Fill,height:Fit,flow:Right,spacing:6,padding:Inset{top:1,bottom:1}
        old_line := mod.widgets.SLabel {width:42,draw_text +: {color:#909090}}
        new_line := mod.widgets.SLabel {width:42,draw_text +: {color:#909090}}
        code_lbl := mod.widgets.SText {width:Fill,height:Fit,is_multiline:false}
    }
    mod.widgets.WorkshopDetailPanel = set_type_default() do #(WorkshopDetail::register_widget(vm)) {
        ..mod.widgets.View
        width:Fill,height:Fill,flow:Down,spacing:8
        padding:Inset{left:12,right:12,top:10,bottom:10}
        meta_lbl := mod.widgets.SLabel {width:Fill,height:Fit}
        status_lbl := mod.widgets.SLabel {width:Fill,height:Fit,draw_text +: {color:#909090}}
        error_lbl := mod.widgets.SLabel {visible:false,width:Fill,height:Fit,draw_text +: {color:#a01500}}
        workspace_actions := View {visible:false,width:Fill,height:Fit,flow:Flow.Right{wrap:true},spacing:6,wrap_spacing:6
            push_btn := mod.widgets.SBtn {text:"push"}
            pr_btn := mod.widgets.SBtn {text:"create PR"}
            review_btn := mod.widgets.SBtn {text:"review changes"}
        }
        providers := View {visible:false,width:Fill,height:Fit,flow:Right,spacing:6
            provider_btn := mod.widgets.SSelect{}
            model_btn := mod.widgets.SSelect{}
        }
        merge_picker := View {visible:false,width:Fill,height:Fit,flow:Right,spacing:8,align:Align{y:0.5}
            mod.widgets.SSection{text:"MERGE METHOD"}
            merge_btn := mod.widgets.SSelect{}
            github_open_btn := mod.widgets.SBtn{text:"open on GitHub"}
        }
        provider_hint := mod.widgets.SSection {visible:false,text:"OPENS IN NEW CHAT"}
        form := View {visible:false,width:Fill,height:Fit,flow:Down,spacing:8
            field_label := mod.widgets.SSection{}
            field_input := mod.widgets.SField {width:Fill}
            apply_model_btn := mod.widgets.SBtn{visible:false,text:"set model"}
            second_label := mod.widgets.SSection{visible:false}
            second_field := View {visible:false,width:Fill,height:Fit
                second_input := mod.widgets.SField {width:Fill}
            }
        }
        list := mod.widgets.SList {width:Fill,height:Fill,flow:Down,reuse_items:true
            message := mod.widgets.WorkshopMessage{}
            row := mod.widgets.WorkshopRow{}
            code := mod.widgets.WorkshopCodeLine{}
        }
        terminal := View {visible:false,width:Fill,height:140,flow:Down,spacing:5
            View {width:Fill,height:Fit,flow:Right,align:Align{y:0.5}
                mod.widgets.SSection{width:Fill,text:"TERMINAL"}
                terminal_btn := mod.widgets.SBtn{text:"open panel"}
            }
            terminal_host := mod.widgets.TerminalPanel{width:Fill,height:Fill}
        }
        composer := View {visible:false,width:Fill,height:Fit,flow:Down,spacing:6
            ask_input := mod.widgets.SField {width:Fill,height:100,is_multiline:true,empty_text:"message"}
            View {width:Fill,height:Fit,flow:Right,spacing:6
                send_btn := mod.widgets.SBtn{text:"send"}
                stop_btn := mod.widgets.SBtn{text:"stop",visible:false}
            }
        }
    }
}

pub struct Ui;
pub static UI: Ui = Ui;
impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag.0 {
            "workshop_projects" => Some(live_id!(workshop_projects_tpl)),
            "workshop_workspaces" => Some(live_id!(workshop_workspaces_tpl)),
            "workshop_review" => Some(live_id!(workshop_review_tpl)),
            "workshop_workspace"
            | "workshop_chat"
            | "workshop_diff"
            | "workshop_activity"
            | "workshop_github"
            | "workshop_comment"
            | "workshop_settings"
            | "workshop_add_project" => Some(live_id!(workshop_detail_tpl)),
            _ => None,
        }
    }
}
