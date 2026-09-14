//! Workshop uses the shell's table, form, button and panel vocabulary. The
//! chat draws its transcript as the prototype does: a washed block for the
//! person, prose for the agent, one bordered card per tool call, todo list,
//! subagent or background task, and a link to the changes a turn made.

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
            detail_lbl := mod.widgets.SLabel { width: Fill, max_lines:1, text_overflow: TextOverflow.Ellipsis, draw_text +: {color:#5a5a5a} }
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
    /** The review meter: reviewed over total changed lines, what is left,
        a 3 pt bar (ink for reviewed, hatched for reviewed files that changed
        since), and the recheck/new split under it. */
    mod.widgets.WorkshopMeter = View {
        visible:false, width:Fill, height:Fit, flow:Down, spacing:6, margin:Inset{bottom:8}
        View { width:Fill, height:Fit, flow:Right, spacing:8
            progress_lbl := mod.widgets.SLabel { width:Fill, max_lines:1, text_overflow: TextOverflow.Ellipsis }
            left_lbl := mod.widgets.SLabel { draw_text +: {color:#5a5a5a} }
        }
        bar := View {
            width:Fill, height:3, show_bg:true
            draw_bg +: {
                done: uniform(0.0)
                changed: uniform(0.0)
                pixel: fn() {
                    if self.pos.x < self.done {
                        return vec4(0.078, 0.078, 0.078, 1.0)
                    }
                    if self.pos.x < self.done + self.changed {
                        let d = self.pos.x * self.rect_size.x + self.pos.y * self.rect_size.y
                        if fract(d / 3.0) < 0.34 {
                            return vec4(0.467, 0.467, 0.467, 1.0)
                        }
                        return vec4(1.0, 1.0, 1.0, 1.0)
                    }
                    return vec4(0.863, 0.863, 0.863, 1.0)
                }
            }
        }
        meter_detail_lbl := mod.widgets.SLabel { visible:false, width:Fill, draw_text +: {color:#5a5a5a, text_style: mod.widgets.SMonoStyle{font_size: 8.25}} }
        notice_lbl := mod.widgets.SLabel { visible:false, width:Fill, draw_text +: {text_style: mod.widgets.SMonoStyle{font_size: 8.25}} }
    }
    mod.widgets.WorkshopTable = View {
        width: Fill, height: Fill, flow: Down
        padding: Inset{left:12,right:12,top:10,bottom:10}
        meter := mod.widgets.WorkshopMeter{}
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
        meter +: {visible:true}
        comparison +: {visible:true}
        columns +: {column_lbl +: {text:"FILE"}, right_lbl +: {text:"LINES"}}
        suggest: mod.widgets.TblSuggest{}
    }

    // ---- the transcript ----------------------------------------------------

    /** A muted selectable run: what a call came to, a subagent's report. */
    mod.widgets.WorkshopMuted = mod.widgets.SText {
        is_multiline: true
        draw_text +: { color:#5a5a5a, color_hover:#5a5a5a, color_focus:#5a5a5a, color_down:#5a5a5a, color_empty:#5a5a5a }
    }
    /** The same, in the one colour errors get. */
    mod.widgets.WorkshopErr = mod.widgets.SText {
        is_multiline: true
        draw_text +: { color:#a01500, color_hover:#a01500, color_focus:#a01500, color_down:#a01500, color_empty:#a01500 }
    }
    /** The agent's Markdown as prose, with inline links. */
    mod.widgets.WorkshopAnswer = Html {
        width: Fill, height: Fit
        padding: 0, margin: 0
        selectable: true
        font_size: 11.25
        font_color: #141414
        draw_text +: { color: #141414 }
        text_style_normal: mod.widgets.SProseStyle{}
        text_style_fixed: mod.widgets.SMonoStyle{}
        a := mod.widgets.HtmlLink { color: #141414 pressed_color: #141414 }
        draw_selection +: { draw_call_group: @selection color: #00000020 }
    }
    /** A bordered box: a card, a step link. Rule-coloured, as the prototype
        draws them. */
    mod.widgets.WorkshopBox = View {
        width: Fill, height: Fit
        show_bg: true
        draw_bg +: {
            color: #ffffff
            pixel: fn() {
                let px = 1.0 / self.rect_size.x
                let py = 1.0 / self.rect_size.y
                if self.pos.x < px || self.pos.x > 1.0 - px || self.pos.y < py || self.pos.y > 1.0 - py {
                    return vec4(0.863, 0.863, 0.863, 1.0)
                }
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }
    /** The person's turn: a washed block, full width. */
    mod.widgets.WorkshopUserTurn = View {
        width: Fill, height: Fit, flow: Down, padding: Inset{top:4, bottom:8}
        wash := View {
            width: Fill, height: Fit, flow: Down, spacing: 5
            padding: Inset{left:10, right:10, top:8, bottom:9}
            show_bg: true
            draw_bg +: {
                color: #e7e7e7
                pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) }
            }
            you_lbl := mod.widgets.SSection { text: "YOU" }
            user_txt := mod.widgets.SProseText { width: Fill, height: Fit }
        }
    }
    /** The agent's turn: who, when it is the first line of a turn, then prose. */
    mod.widgets.WorkshopAgentTurn = View {
        width: Fill, height: Fit, flow: Down, spacing: 5, padding: Inset{top:6, bottom:8}
        who_lbl := mod.widgets.SSection { visible:false, text: "" }
        answer := mod.widgets.WorkshopAnswer {}
    }
    /** One card: the call on its first line — the tool, what it was asked,
        and where it stands — and behind it what it came to, folded until a
        press. A todo list shows its items; a subagent its progress and,
        when open, the calls it made under it. */
    mod.widgets.WorkshopCard = mod.widgets.WorkshopBox {
        flow: Down, spacing: 4
        margin: Inset{top:3, bottom:3}
        padding: Inset{left:8, right:8, top:6, bottom:6}
        line := View {
            width: Fill, height: Fit, flow: Right, spacing: 8, align: Align{y:0.5}
            cursor: MouseCursor.Hand
            fold_lbl := mod.widgets.SLabel { text: "›", draw_text +: {color:#909090} }
            name_lbl := mod.widgets.SLabel { text: "" }
            title_lbl := mod.widgets.SLabel { width: Fill, max_lines:1, text_overflow: TextOverflow.Ellipsis, text: "", draw_text +: {color:#5a5a5a} }
            state_lbl := mod.widgets.SLabel { text: "", draw_text +: {color:#909090} }
            state_err := mod.widgets.SLabel { visible:false, text: "", draw_text +: {color:#a01500} }
        }
        /* Text runs keep their line even when hidden, so each sits in a
           view that stands down with it. */
        todo_wrap := View { visible:false, width:Fill, height:Fit
            todo_txt := mod.widgets.SText { is_multiline:true, width: Fill }
        }
        progress_lbl := mod.widgets.SLabel { visible:false, width: Fill, max_lines:1, text_overflow: TextOverflow.Ellipsis, text: "", draw_text +: {color:#909090, text_style: mod.widgets.SMonoStyle{font_size: 8.25}} }
        out := View { visible:false, width:Fill, height:Fit
            body_txt := mod.widgets.WorkshopMuted { width: Fill }
        }
        err := View { visible:false, width:Fill, height:Fit
            err_txt := mod.widgets.WorkshopErr { width: Fill }
        }
        approval := View { visible:false, width:Fill, height:Fit, flow:Right, spacing:6, margin:Inset{top:2}
            approve_btn := mod.widgets.SBtn{text:"approve"}
            refuse_btn := mod.widgets.SBtn{text:"refuse"}
        }
    }
    /** The same card one level in: a subagent's own calls. */
    mod.widgets.WorkshopChildCard = mod.widgets.WorkshopCard {
        margin: Inset{left:16, top:3, bottom:3}
    }
    /** What a turn changed: the link that opens the diff, and the count. */
    mod.widgets.WorkshopStepLink = mod.widgets.WorkshopBox {
        flow: Right, spacing: 9, align: Align{y:0.5}
        margin: Inset{top:6, bottom:6}
        padding: Inset{left:8, right:8, top:7, bottom:7}
        link := View {
            width: Fit, height: Fit, flow: Down, cursor: MouseCursor.Hand
            step_lbl := mod.widgets.SLabel { text: "view changes" }
            View { width: Fill, height: 1, show_bg: true
                draw_bg +: { color: #141414, pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) } }
            }
        }
        count_lbl := mod.widgets.SLabel { text: "", draw_text +: {color:#5a5a5a, text_style: mod.widgets.SMonoStyle{font_size: 8.25}} }
        View { width: Fill, height: 1 }
        mod.widgets.SLabel { text: "→", draw_text +: {color:#5a5a5a} }
    }
    /** One line of an activity, GitHub or settings list. */
    mod.widgets.WorkshopMessage = View {
        width:Fill,height:Fit,flow:Down,spacing:5,padding:Inset{top:8,bottom:10}
        author_lbl := mod.widgets.SSection{}
        text_lbl := mod.widgets.SText {width:Fill,height:Fit,is_multiline:true}
        detail_lbl := mod.widgets.SLabel {width:Fill,draw_text +: {color:#909090}}
        open_btn := mod.widgets.SBtn {visible:false,text:"view changes"}
        mod.widgets.TblHairline{}
    }

    // ---- the diff ------------------------------------------------------------

    /** One code line: both line numbers, the sign, the code. The row is as
        wide as the longest line of the file, inside one horizontal scroll. */
    mod.widgets.WorkshopCodeBase = View {
        width:Fill,height:Fit,flow:Right,align:Align{y:0.5},padding:Inset{top:2,bottom:2}
        old_box := View { width:40, height:Fit, align:Align{x:1.0}, padding:Inset{right:7}
            old_line := mod.widgets.SLabel {draw_text +: {color:#909090, text_style: mod.widgets.SMonoStyle{font_size: 8.25}}}
        }
        new_box := View { width:40, height:Fit, align:Align{x:1.0}, padding:Inset{right:7}
            new_line := mod.widgets.SLabel {draw_text +: {color:#909090, text_style: mod.widgets.SMonoStyle{font_size: 8.25}}}
        }
        sign_box := View { width:19, height:Fit, align:Align{x:0.5}
            sign_lbl := mod.widgets.SLabel {draw_text +: {color:#5a5a5a}}
        }
        code_lbl := mod.widgets.SText {width:Fill,height:Fit,is_multiline:false, margin:Inset{right:12}}
    }
    mod.widgets.WorkshopCodeCtx = mod.widgets.WorkshopCodeBase {}
    mod.widgets.WorkshopCodeAdd = mod.widgets.WorkshopCodeBase {
        show_bg:true
        draw_bg +: { color:#f1f1f1, pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) } }
    }
    mod.widgets.WorkshopCodeDel = mod.widgets.WorkshopCodeBase {
        show_bg:true
        draw_bg +: {
            pixel: fn() {
                let d = self.pos.x * self.rect_size.x - self.pos.y * self.rect_size.y
                if fract(d / 6.0) < 0.2 {
                    return vec4(0.929, 0.929, 0.929, 1.0)
                }
                return vec4(0.98, 0.98, 0.98, 1.0)
            }
        }
    }
    /** A hunk's location, and a note such as a missing final newline. */
    mod.widgets.WorkshopHunkLine = View {
        width:Fill,height:Fit,flow:Down,padding:Inset{left:12,right:12,top:6,bottom:6},margin:Inset{top:4,bottom:4}
        show_bg:true
        draw_bg +: {
            pixel: fn() {
                let py = 1.0 / self.rect_size.y
                if self.pos.y < py || self.pos.y > 1.0 - py {
                    return vec4(0.863, 0.863, 0.863, 1.0)
                }
                return vec4(0.98, 0.98, 0.98, 1.0)
            }
        }
        hunk_lbl := mod.widgets.SLabel {width:Fill, draw_text +: {color:#5a5a5a, text_style: mod.widgets.SMonoStyle{font_size: 8.25}}}
    }
    mod.widgets.WorkshopCodeNote = View {
        width:Fill,height:Fit,flow:Down,padding:Inset{left:12,right:12,top:3,bottom:3}
        note_lbl := mod.widgets.SLabel {width:Fill, draw_text +: {color:#909090, text_style: mod.widgets.SMonoStyle{font_size: 8.25}}}
    }

    mod.widgets.WorkshopDetailPanel = set_type_default() do #(WorkshopDetail::register_widget(vm)) {
        ..mod.widgets.View
        width:Fill,height:Fill,flow:Down,spacing:8
        padding:Inset{left:12,right:12,top:10,bottom:10}
        // Measured, never drawn: the code column is as wide as its longest
        // line, and the mono advance says how wide that is.
        draw_mono +: {
            text_style: mod.widgets.SMonoStyle{}
            color: #141414ff
        }
        meta_lbl := mod.widgets.SLabel {width:Fill,height:Fit,max_lines:1,text_overflow: TextOverflow.Ellipsis}
        status_lbl := mod.widgets.SLabel {width:Fill,height:Fit,draw_text +: {color:#909090}}
        error_lbl := mod.widgets.SLabel {visible:false,width:Fill,height:Fit,draw_text +: {color:#a01500}}
        /* The hub's Git lines: where the branch goes, the pull request or the
           button that asks for one, and what is not pushed yet. */
        hub := View {visible:false,width:Fill,height:Fit,flow:Down,spacing:7
            base_line := View {width:Fill,height:Fit,flow:Right,spacing:8,align:Align{y:0.5}
                base_lbl := mod.widgets.SLabel {draw_text +: {color:#5a5a5a}}
                View {width:Fill,height:1}
                pr_link := mod.widgets.SLink {visible:false}
                pr_btn := mod.widgets.SBtn {text:"create PR"}
                pr_state_lbl := mod.widgets.SLabel {draw_text +: {color:#5a5a5a, text_style: mod.widgets.SMonoStyle{font_size: 8.25}}}
                pr_state_err := mod.widgets.SLabel {visible:false, draw_text +: {color:#a01500, text_style: mod.widgets.SMonoStyle{font_size: 8.25}}}
            }
            push_line := View {visible:false,width:Fill,height:Fit,flow:Right,spacing:8,align:Align{y:0.5}
                mod.widgets.SLabel {text:"changes not pushed", draw_text +: {color:#5a5a5a, text_style: mod.widgets.SMonoStyle{font_size: 8.25}}}
                View {width:Fill,height:1}
                push_btn := mod.widgets.SBtn {text:"push"}
            }
            View {width:Fill,height:Fit,flow:Right,padding:Inset{left:8,right:8,bottom:3},margin:Inset{top:4}
                mod.widgets.SSection {width:Fill,text:"CHAT"}
                mod.widgets.SSection {text:"AGENT"}
            }
            mod.widgets.TblHeadRule{}
        }
        providers := View {visible:false,width:Fill,height:Fit,flow:Right,spacing:6,align:Align{y:0.5}
            provider_btn := mod.widgets.SSelect{}
            model_btn := mod.widgets.SSelect{}
            View {width:Fill,height:1}
            chat_status_lbl := mod.widgets.SLabel {draw_text +: {color:#5a5a5a}}
        }
        provider_hint := mod.widgets.SSection {visible:false,text:"OPENS IN NEW CHAT"}
        merge_picker := View {visible:false,width:Fill,height:Fit,flow:Right,spacing:8,align:Align{y:0.5}
            mod.widgets.SSection{text:"MERGE METHOD"}
            merge_btn := mod.widgets.SSelect{}
            github_open_btn := mod.widgets.SBtn{text:"open on GitHub"}
        }
        form := View {visible:false,width:Fill,height:Fit,flow:Down,spacing:8
            field_label := mod.widgets.SSection{}
            field_input := mod.widgets.SField {width:Fill}
            apply_model_btn := mod.widgets.SBtn{visible:false,text:"set model"}
            second_label := mod.widgets.SSection{visible:false}
            second_field := View {visible:false,width:Fill,height:Fit
                second_input := mod.widgets.SField {width:Fill}
            }
        }
        /* A list has no visibility of its own — it draws and claims its Fill
           whatever `visible` says — so the transcript hides behind a wrapper.
           Without it a diff shares the panel with the list it replaces and
           draws in half of it, and the list is handed the diff's rows. */
        list_wrap := View {width:Fill,height:Fill,flow:Down
            list := mod.widgets.SList {width:Fill,height:Fill,flow:Down,reuse_items:true
                message := mod.widgets.WorkshopMessage{}
                row := mod.widgets.WorkshopRow{}
                user := mod.widgets.WorkshopUserTurn{}
                agent := mod.widgets.WorkshopAgentTurn{}
                card := mod.widgets.WorkshopCard{}
                child := mod.widgets.WorkshopChildCard{}
                step := mod.widgets.WorkshopStepLink{}
            }
        }
        /* The diff: one horizontal scroll over rows as wide as the longest
           line; the list inside scrolls vertically as every list does. */
        diff := View {visible:false,width:Fill,height:Fill,flow:Down
            diff_scroll := View {width:Fill,height:Fill,flow:Down
                scroll_bars: ScrollBars{ show_scroll_x: true, show_scroll_y: false }
                code_wrap := View {width:Fill,height:Fill,flow:Down
                    code_list := mod.widgets.SList {width:Fill,height:Fill,flow:Down,reuse_items:true
                        ctx := mod.widgets.WorkshopCodeCtx{}
                        add := mod.widgets.WorkshopCodeAdd{}
                        del := mod.widgets.WorkshopCodeDel{}
                        hunk := mod.widgets.WorkshopHunkLine{}
                        note := mod.widgets.WorkshopCodeNote{}
                    }
                }
            }
        }
        terminal := View {visible:false,width:Fill,height:140,flow:Down,spacing:5
            View {width:Fill,height:Fit,flow:Right,align:Align{y:0.5}
                mod.widgets.SSection{width:Fill,text:"TERMINAL"}
                terminal_btn := mod.widgets.SBtn{text:"open panel"}
            }
            terminal_host := mod.widgets.TerminalGrid{width:Fill,height:Fill}
        }
        composer := View {visible:false,width:Fill,height:Fit,flow:Down,spacing:6
            ask_input := mod.widgets.SField {width:Fill,height:100,is_multiline:true,empty_text:"message"}
            View {width:Fill,height:Fit,flow:Right,spacing:6,align:Align{y:0.5}
                mode_btn := mod.widgets.SSelect{visible:false}
                View {width:Fill,height:1}
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
            | "workshop_closed_chats"
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
