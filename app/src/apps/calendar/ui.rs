use super::{panels::*, widgets::*};
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
        row := mod.widgets.CalendarEventRow {}
        caption := mod.widgets.TblCaption {}
        band_rule := mod.widgets.TblBandRule {}
    }
    suggest: mod.widgets.TblSuggest {}
 }
 mod.widgets.CalendarDay = View {
    width: Fill, height: 116, flow: Down, padding: 7, spacing: 7
    show_bg: true
    draw_bg +: { color: #ffffff }
    day_lbl := mod.widgets.SBoldLabel { width: Fill }
    events_lbl := mod.widgets.SLabel { width: Fill, max_lines: 4, draw_text +: { text_style: mod.widgets.SMonoStyle{font_size:8.5} } }
 }
 mod.widgets.CalendarWeek = View {
    width: Fill, height: Fit, flow: Down
    View { width: Fill, height: Fit, flow: Right, spacing: 1
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
    width: Fill, height: Fill, flow: Down, padding: 12, spacing: 7
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
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        form := View { width: Fill, height: Fit, flow: Down, spacing: 9
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "CALENDAR" } source_btn := mod.widgets.SBtn { width: Fill, text: "choose calendar" } }
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
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "SHOW AS" } busy_btn := mod.widgets.SBtn { text: "busy" } }
            mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "VISIBILITY" } visibility_btn := mod.widgets.SBtn { text: "default" } }
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
 mod.widgets.CalendarAvailabilityPanel = set_type_default() do #(CalendarAvailabilityPanel::register_widget(vm)) {
    ..mod.widgets.View
    width: Fill, height: Fill, flow: Down, padding: 14, spacing: 9
    mod.widgets.SSection { text: "SEARCH WINDOW" }
    start_input := mod.widgets.SField { empty_text: "YYYY-MM-DDTHH:MM" }
    end_input := mod.widgets.SField { empty_text: "YYYY-MM-DDTHH:MM" }
    mod.widgets.SFormRow { mod.widgets.SFormLabel { text: "MINUTES" } duration_input := mod.widgets.SField {} }
    status_lbl := mod.widgets.SLabel { width: Fill, max_lines: 4 }
    list := mod.widgets.SList { width: Fill, height: Fill, flow: Down, reuse_items: true
        person := View { width: Fill, height: Fit, flow: Down, padding: 6, spacing: 5
            title_lbl := mod.widgets.SBoldLabel { width: Fill }
            detail_lbl := mod.widgets.SLabel { width: Fill }
            mod.widgets.SRule {}
        }
        slot := mod.widgets.SBtn { width: Fill, height: Fit, margin: Inset{top:4,bottom:4}, text: "use this time" }
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
