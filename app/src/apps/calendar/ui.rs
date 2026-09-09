use super::{availability_ui::*, panels::*, widgets::*};
use crate::shell::{
    app_ui::{AppUi, Setup},
    catalog::{panel, workspace_on},
};
use kernel::{panel::Tag, scene::Scene};
use makepad_widgets::*;
script_mod! {
 use mod.prelude.widgets.*
 use mod.widgets.*
 mod.widgets.CalendarEventBody = View {
    width: Fill, height: Fit, flow: Down, spacing: 5
    title_lbl := mod.widgets.SBoldLabel { width: Fill, max_lines: 2 }
    time_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { color: #5a5a5a } }
    detail_lbl := mod.widgets.SLabel { width: Fill, max_lines: 2, draw_text +: { color: #909090 } }
 }
 mod.widgets.CalendarEventRow = mod.widgets.TblRow {
    line := mod.widgets.TblLine { body := mod.widgets.CalendarEventBody {} }
    line_sel := mod.widgets.TblLineSel { body := mod.widgets.CalendarEventBody {} }
    line_mark := mod.widgets.TblLineMark { body := mod.widgets.CalendarEventBody {} }
    line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.CalendarEventBody {} }
    mod.widgets.TblHairline {}
 }
 mod.widgets.CalendarTimelinePanel = set_type_default() do #(CalendarTimelinePanel::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: Fill, flow: Down, padding: 12
    filter_input := mod.widgets.TblFilter { empty_text: "filter events · @calendar @with @invited" }
    filter_err_lbl := mod.widgets.TblErr {}
    View { width: Fill, height: 10 }
    status_lbl := mod.widgets.SSection { width: Fill, max_lines: 2 }
    mod.widgets.TblHeadRule {}
    empty_lbl := mod.widgets.TblEmpty {}
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        reached_end_margin: 10
        row := mod.widgets.CalendarEventRow {}
        caption := mod.widgets.TblCaption {}
        band_rule := mod.widgets.TblBandRule {}
    }
    suggest: mod.widgets.TblSuggest {}
 }
 mod.widgets.CalendarMonthEvent = View {
    width: Fill, height: 19, flow: Right, align: Align{y:0.5}
    padding: Inset{left:5,right:3}, show_bg: true
    draw_bg +: {
        dotted: uniform(0.0)
        pixel: fn() {
            let p = self.pos * self.rect_size
            if p.x < 1.5 && (self.dotted < 0.5 || fract(p.y/4.0) < 0.5) {
                return vec4(0.078,0.078,0.078,1.0)
            }
            return vec4(0.937,0.937,0.937,1.0)
        }
    }
    title_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis
        draw_text +: { text_style: mod.widgets.SMonoStyle{font_size:8.25} }
    }
 }
 mod.widgets.CalendarDay = View {
    width: Fill, height: 116, flow: Down, padding: Inset{left:4,right:5,top:6,bottom:6}, spacing: 4
    show_bg: true
    draw_bg +: {
        quiet: uniform(0.0)
        selected: uniform(0.0)
        pixel: fn() {
            if self.pos.x*self.rect_size.x > self.rect_size.x-1.0 {return vec4(0.86,0.86,0.86,1.0)}
            if self.selected > 0.5 {return vec4(0.906,0.906,0.906,1.0)}
            if self.quiet > 0.5 {return vec4(0.976,0.976,0.976,1.0)}
            return vec4(1.0,1.0,1.0,1.0)
        }
    }
    View { width: Fill, height: 22, flow: Right, align: Align{y:0.5}
        View { width: Fill, height: 1 }
        day_lbl := mod.widgets.SLabel { width: 22, align: Align{x:0.5} }
        today_badge := View { width: 22, height: 22, visible: false, show_bg: true, align: Align{x:0.5,y:0.5}
            draw_bg +: { color: #141414 }
            today_lbl := mod.widgets.SBoldLabel { draw_text +: { color: #ffffff } }
        }
    }
    e0 := mod.widgets.CalendarMonthEvent {}
    e1 := mod.widgets.CalendarMonthEvent {}
    e2 := mod.widgets.CalendarMonthEvent {}
    more_lbl := mod.widgets.SSection { width: Fill, max_lines: 1, padding: Inset{left:5,top:2} }
 }
 mod.widgets.CalendarWeek = View {
    width: Fill, height: Fit, flow: Down
    View { width: Fill, height: Fit, flow: Right
        d0 := mod.widgets.CalendarDay {}
        d1 := mod.widgets.CalendarDay {}
        d2 := mod.widgets.CalendarDay {}
        d3 := mod.widgets.CalendarDay {}
        d4 := mod.widgets.CalendarDay {}
        d5 := mod.widgets.CalendarDay {}
        d6 := mod.widgets.CalendarDay {}
    }
    mod.widgets.TblHairline {}
 }
 mod.widgets.CalendarMonthPanel = set_type_default() do #(CalendarMonthPanel::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: Fill, flow: Down, padding: 20, spacing: 12
    month_lbl := mod.widgets.SBoldLabel { width: Fill, draw_text +: { text_style: mod.widgets.SProseBoldStyle{font_size:20} } }
    month_detail_lbl := mod.widgets.SSection { width: Fill }
    filter_input := mod.widgets.TblFilter { empty_text: "filter events · @calendar @with" }
    filter_err_lbl := mod.widgets.TblErr {}
    week_head := View { width: Fill, height: Fit, flow: Right
        mod.widgets.SSection { width: Fill, text: "MON" }
        mod.widgets.SSection { width: Fill, text: "TUE" }
        mod.widgets.SSection { width: Fill, text: "WED" }
        mod.widgets.SSection { width: Fill, text: "THU" }
        mod.widgets.SSection { width: Fill, text: "FRI" }
        mod.widgets.SSection { width: Fill, text: "SAT" }
        mod.widgets.SSection { width: Fill, text: "SUN" }
    }
    mod.widgets.TblHeadRule {}
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        week := mod.widgets.CalendarWeek {}
    }
 }
 mod.widgets.CalendarEventPanel = set_type_default() do #(CalendarEventPanel::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: Fill, flow: Down, padding: 16
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        content := View { width: Fill, height: Fit, flow: Down, spacing: 14
            title_lbl := mod.widgets.SBoldLabel { width: Fill, draw_text +: { text_style: mod.widgets.SProseBoldStyle{font_size:18} } }
            when_lbl := mod.widgets.SLabel { width: Fill }
            source_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { color: #909090 } }
            mod.widgets.SRule {}
            details_html := mod.widgets.ReaderHtml {}
            people_lbl := mod.widgets.SText { width: Fill, is_multiline: true }
            notes_html := mod.widgets.ReaderHtml {}
            state_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { color: #5a5a5a } }
            delete_view := View { width: Fill, height: Fit, flow: Down, spacing: 8
                delete_lbl := mod.widgets.SLabel { width: Fill }
                scope_btn := mod.widgets.SBtn { text: "scope: this event" }
                notify_btn := mod.widgets.SBtn { text: "notify guests: yes" }
            }
        }
    }
 }
 mod.widgets.CalendarEditorPanel = set_type_default() do #(CalendarEditorPanel::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: Fill, flow: Down, padding: 14
    status_lbl := mod.widgets.SLabel { width: Fill, max_lines: 3 }
    error_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { color: #a01500 } }
    suggest: mod.widgets.TblSuggest {}
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        form := View { width: Fill, height: Fit, flow: Down, spacing: 9
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "CALENDAR" } source_btn := mod.widgets.SSelect {} }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "TITLE" } title_input := mod.widgets.SField { empty_text: "event title" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "START" } start_input := mod.widgets.SField { empty_text: "YYYY-MM-DDTHH:MM" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "END" } end_input := mod.widgets.SField { empty_text: "YYYY-MM-DDTHH:MM" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "TIME ZONE" } zone_input := mod.widgets.SField { empty_text: "Europe/Berlin" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "ALL DAY" } all_day_btn := mod.widgets.SBtn { text: "no" } }
            day_hint := mod.widgets.SSection { width: Fill, text: "All-day events use an exclusive end date." }
            mod.widgets.SRule {}
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "GUESTS" } guests_input := mod.widgets.SField { empty_text: "email, email · ?optional@email" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "LOCATION" } location_input := mod.widgets.SField { empty_text: "place or address" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "GOOGLE MEET" } meet_btn := mod.widgets.SBtn { text: "off" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "REPEAT" } repeat_btn := mod.widgets.SBtn { text: "does not repeat" } }
            recurrence_input := mod.widgets.SField { width: Fill, empty_text: "custom: FREQ=WEEKLY;BYDAY=MO,WE;COUNT=12" }
            scope_row := mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "EDIT SCOPE" } scope_btn := mod.widgets.SBtn { text: "this event" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "SHOW AS" } busy_btn := mod.widgets.SSelect {} }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "VISIBILITY" } visibility_btn := mod.widgets.SSelect {} }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "REMINDERS" } reminders_input := mod.widgets.SField { empty_text: "default or popup:10, email:60" } }
            mod.widgets.SSection { text: "NOTES" }
            notes_input := mod.widgets.SField { width: Fill, height: 100, is_multiline: true, empty_text: "event notes" }
            mod.widgets.SRule {}
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "GUESTS CAN" } modify_btn := mod.widgets.SBtn { text: "edit: no" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "" } invite_btn := mod.widgets.SBtn { text: "invite others: yes" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "" } see_btn := mod.widgets.SBtn { text: "see guest list: yes" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "UPDATES" } notify_btn := mod.widgets.SBtn { text: "notify guests: yes" } }
            View { width: Fill, height: 12 }
        }
    }
 }
 mod.widgets.CalendarTimeTrack = set_type_default() do #(CalendarTimeTrack::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: 34, show_bg: true
    fill +: { color: #ffffff }
    draw_bg +: {
        unknown: uniform(0.0)
        pixel: fn() {
            if self.unknown > 0.5 && fract((self.pos.x*self.rect_size.x + self.pos.y*self.rect_size.y)/8.0) < 0.15 {
                return vec4(0.87, 0.87, 0.87, 1.0)
            }
            return vec4(1.0, 1.0, 1.0, 1.0)
        }
    }
 }
 mod.widgets.CalendarTimeBox = View {
    show_bg: true
    draw_bg +: {
        color: #ffffff
        border_color: uniform(#aaaaaa)
        pixel: fn() {
            let p = self.pos * self.rect_size
            if p.x < 1.0 || p.y < 1.0 || p.x > self.rect_size.x - 1.0 || p.y > self.rect_size.y - 1.0 {
                return self.border_color
            }
            return self.color
        }
    }
 }
 mod.widgets.CalendarTimeChoice = View {
    width: Fill, height: Fit, flow: Right, spacing: 12, padding: Inset{left:12,right:12,top:13,bottom:13}
    align: Align{y:0.5}
    View { width: Fill, height: Fit, flow: Down, spacing: 5
        time_lbl := mod.widgets.SBoldLabel { width: Fill, draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size:12} } }
        date_lbl := mod.widgets.SSection { width: Fill, max_lines: 2 }
    }
    indicator := mod.widgets.CalendarTimeBox { width: 13, height: 13
        draw_bg +: { border_color: #141414 }
    }
 }
 mod.widgets.CalendarAvailabilityPanel = set_type_default() do #(CalendarAvailabilityPanel::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: Fill, flow: Down, padding: 20
    suggest: mod.widgets.TblSuggest {}
    tooltip: mod.widgets.CalendarTimeBox {
        width: 330, height: Fit, flow: Down, padding: 12
        body_lbl := mod.widgets.SLabel { width: Fill, max_lines: 12
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size:9.5} }
        }
    }
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        controls := View { width: Fill, height: Fit, flow: Down, spacing: 14
            View { width: Fill, height: Fit, flow: Right, spacing: 12, align: Align{y:0.5}
                mod.widgets.SBoldLabel { width: Fill, text: "Find a time together", draw_text +: { text_style: mod.widgets.SProseBoldStyle{font_size:18} } }
                count_lbl := mod.widgets.SSection {}
            }
            View { width: Fill, height: Fit, flow: Right, spacing: 12
                View { width: Fill, height: Fit, flow: Down, spacing: 6
                    mod.widgets.SSection { text: "DATE" }
                    View { width: Fill, height: Fit, flow: Right, spacing: 4, align: Align{y:0.5}
                        previous_btn := mod.widgets.SBtn { text: "‹", width: 24 }
                        date_input := mod.widgets.SField { empty_text: "YYYY-MM-DD" }
                        next_btn := mod.widgets.SBtn { text: "›", width: 24 }
                    }
                }
                View { width: 85, height: Fit, flow: Down, spacing: 6
                    mod.widgets.SSection { text: "MINUTES" }
                    duration_input := mod.widgets.SField { empty_text: "30" }
                }
                View { width: Fill, height: Fit, flow: Down, spacing: 6
                    mod.widgets.SSection { text: "LOOK BETWEEN" }
                    View { width: Fill, height: Fit, flow: Right, spacing: 6, align: Align{y:0.5}
                        from_input := mod.widgets.SField { empty_text: "09:00" }
                        mod.widgets.SSection { text: "—" }
                        until_input := mod.widgets.SField { empty_text: "17:00" }
                    }
                }
            }
            range_row := mod.widgets.SFormRow { visible: false
                mod.widgets.SFormLabel { text: "THROUGH DATE" }
                end_date_input := mod.widgets.SField { empty_text: "YYYY-MM-DD" }
            }
            mod.widgets.SFormRow {
                mod.widgets.SFormLabel { text: "TIME ZONE" }
                zone_input := mod.widgets.SField {}
            }
            status_lbl := mod.widgets.SSection { width: Fill, max_lines: 3 }
            // Reserve all three lines so overlap warnings never move a track
            // under the pointer, including when the participant names wrap.
            selection := View { width: Fill, height: 56, flow: Down
                selection_lbl := mod.widgets.SBoldLabel { width: Fill, max_lines: 3 }
            }
            drag_hint := mod.widgets.SSection { width: Fill, max_lines: 2
                text: "Drag to change time · snaps every 15 minutes\nHover busy blocks for event details"
            }
            View { width: Fill, height: Fit, flow: Right, spacing: 8, align: Align{y:0.5}, margin: Inset{top:4,bottom:6}
                View { width: Fill, height: 1 }
                mod.widgets.CalendarTimeBox { width: 12, height: 9 }
                mod.widgets.SSection { text: "free" }
                View { width: 10, height: 1 }
                View { width: 12, height: 9, show_bg: true, draw_bg +: { color: #bdbdbd } }
                mod.widgets.SSection { text: "busy" }
                View { width: 10, height: 1 }
                View { width: 12, height: 9, show_bg: true, draw_bg +: { pixel:fn(){
                    if fract((self.pos.x*self.rect_size.x+self.pos.y*self.rect_size.y)/6.0)<0.2 {return vec4(0.75,0.75,0.75,1.0)}
                    return vec4(1.0,1.0,1.0,1.0)
                } } }
                mod.widgets.SSection { text: "unknown" }
            }
            View { width: Fill, height: Fit, flow: Right, spacing: 12, margin: Inset{bottom:6}
                View { width: 140, height: 1 }
                View { width: Fill, height: Fit, flow: Right
                    t0 := mod.widgets.SSection { width: Fill }
                    t1 := mod.widgets.SSection { width: Fill }
                    t2 := mod.widgets.SSection { width: Fill }
                    t3 := mod.widgets.SSection { width: Fill }
                    t4 := mod.widgets.SSection {}
                }
            }
        }
        person := View { width: Fill, height: 76, flow: Right, spacing: 12, align: Align{y:0.5}
            View { width: 140, height: Fit, flow: Down, spacing: 3
                name_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
                detail_lbl := mod.widgets.SSection { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
                sharing_lbl := mod.widgets.SSection { width: Fill, max_lines: 2, text_overflow: TextOverflow.Ellipsis, draw_text +: { text_style: mod.widgets.SMonoStyle{font_size:7.5} } }
            }
            track := mod.widgets.CalendarTimeTrack {}
        }
        notice := mod.widgets.CalendarTimeBox { width: Fill, height: Fit, flow: Down, padding: 12, margin: Inset{top:16,bottom:6}
            draw_bg +: { color: #fafafa }
            body_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { text_style: mod.widgets.SProseStyle{font_size:10.5} } }
        }
        heading := View { width: Fill, height: Fit, flow: Down, padding: Inset{top:24,bottom:10}
            title_lbl := mod.widgets.SBoldLabel { width: Fill }
        }
        slot := View { width: Fill, height: Fit, flow: Down
            mod.widgets.TblHairline {}
            normal := mod.widgets.CalendarTimeChoice {}
            selected := mod.widgets.CalendarTimeChoice { visible: false, show_bg: true
                draw_bg +: { pixel:fn(){
                    if self.pos.x*self.rect_size.x<3.0 {return vec4(0.078,0.078,0.078,1.0)}
                    return vec4(0.906,0.906,0.906,1.0)
                } }
                indicator +: { draw_bg +: { color: #141414 } }
            }
        }
        more := mod.widgets.SBtn { text: "show more times", margin: Inset{top:12,bottom:6} }
        footer := View { width: Fill, height: Fit, flow: Down, padding: Inset{top:14,bottom:12}
            body_lbl := mod.widgets.SSection { width: Fill }
        }
    }
 }
 mod.widgets.CalendarSourcesPanel = set_type_default() do #(CalendarSourcesPanel::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: Fill, flow: Down, padding: 14, spacing: 9
    status_lbl := mod.widgets.SLabel { width: Fill, max_lines: 4 }
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        source := View { width: Fill, height: Fit, flow: Down, spacing: 6, padding: 8
            title_lbl := mod.widgets.SBoldLabel { width: Fill }
            detail_lbl := mod.widgets.SLabel { width: Fill, max_lines: 4 }
            open_btn := mod.widgets.SBtn { text: "show events" }
            new_btn := mod.widgets.SBtn { text: "new event here" }
            mod.widgets.SRule {}
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
        match tag {
            Timeline::TAG => Some(live_id!(calendar_timeline_tpl)),
            Month::TAG => Some(live_id!(calendar_month_tpl)),
            super::panels::Event::TAG => Some(live_id!(calendar_event_tpl)),
            Editor::TAG => Some(live_id!(calendar_editor_tpl)),
            Availability::TAG => Some(live_id!(calendar_availability_tpl)),
            Sources::TAG => Some(live_id!(calendar_sources_tpl)),
            _ => None,
        }
    }
    fn scenes(&self) -> Vec<Scene<Setup>> {
        vec![Scene::new("calendar", (680.0, 820.0))
            .node("timeline", panel(|_| Timeline::id(), ""))
            .node("month", panel(|_| Month::id("", ""), ""))
            .node("calendars", panel(|_| Sources::id(), ""))
            .node(
                "event draft",
                workspace_on(|_| Timeline::id(), "key n\nwait 700"),
            )
            .sized((1400.0, 820.0))]
    }
}
