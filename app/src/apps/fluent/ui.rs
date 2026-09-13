//! Fluent's Makepad half: its templates, and the tag each of them draws.
//!
//! Content in the course's language — a prompt, a passage, a card's face,
//! a rule — is set in the shell's prose face, as a letter is; everything
//! that is chrome stays in the mono face. No colour but the shell's four
//! greys and the one red: a right answer is said in a word, a wrong one
//! too, and the grade pad is six buttons on the bar.

use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::*;

use crate::shell::app_ui::{AppUi, Setup as SceneSetup};

use super::panels::{Card, Cards, Desk, Grammar, History, Import, Lesson, Progress, Review, Setup, Topic};
use super::widgets::{
    CardPanel, CardsPanel, DeskPanel, GrammarPanel, HistoryPanel, ImportPanel, LessonPanel, ProgressPanel,
    ReviewPanel, SetupPanel, TopicPanel,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the pieces --------------------------------------------------------

    /** Content in the course's language: the prose face, a little larger
        than a letter's, because an exercise is one thing on the screen. */
    mod.widgets.FluentProse = Label {
        width: Fill, height: Fit, padding: 0
        draw_text +: { color: #141414, text_style: mod.widgets.SProseStyle{font_size: 12.75} }
    }
    /** The hero: a prompt, a card's face, a summary's first word. */
    mod.widgets.FluentProseBig = Label {
        width: Fill, height: Fit, padding: 0
        draw_text +: { color: #141414, text_style: mod.widgets.SProseStyle{font_size: 16.5} }
    }
    mod.widgets.FluentProseBold = Label {
        width: Fill, height: Fit, padding: 0
        draw_text +: { color: #141414, text_style: mod.widgets.SProseBoldStyle{font_size: 12.75} }
    }
    mod.widgets.FluentProseItalic = Label {
        width: Fill, height: Fit, padding: 0
        draw_text +: { color: #5a5a5a, text_style: mod.widgets.SProseItalicStyle{font_size: 12.75} }
    }
    /** A muted line of chrome. */
    mod.widgets.FluentMuted = mod.widgets.SLabel {
        width: Fill, draw_text +: { color: #909090 }
    }

    /** A hairline box: a passage, a model answer, a tile on the desk. */
    mod.widgets.FluentBox = View {
        width: Fill, height: Fit
        flow: Down, spacing: 6
        padding: Inset{left: 10, right: 10, top: 8, bottom: 8}
        show_bg: true
        draw_bg +: {
            color: #ffffff
            pixel: fn() {
                let px = 1.0 / self.rect_size.x
                let py = 1.0 / self.rect_size.y
                if self.pos.x < px || self.pos.x > 1.0 - px || self.pos.y < py || self.pos.y > 1.0 - py {
                    return vec4(0.078, 0.078, 0.078, 1.0)
                }
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    /** The wash a verdict sits in. */
    mod.widgets.FluentWash = View {
        width: Fill, height: Fit
        flow: Down, spacing: 4
        padding: Inset{left: 10, right: 10, top: 8, bottom: 8}
        show_bg: true
        draw_bg +: {
            color: #e7e7e7
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    /** A key's name in a thin box: the digit a choice answers to. Display
        only — a button would take the keyboard from the field. */
    mod.widgets.FluentKey = View {
        width: 22, height: 22
        align: Align{x: 0.5, y: 0.5}
        show_bg: true
        draw_bg +: {
            color: #ffffff
            pixel: fn() {
                let px = 1.0 / self.rect_size.x
                let py = 1.0 / self.rect_size.y
                if self.pos.x < px || self.pos.x > 1.0 - px || self.pos.y < py || self.pos.y > 1.0 - py {
                    return vec4(0.353, 0.353, 0.353, 1.0)
                }
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
        lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #5a5a5a, text_style: mod.widgets.SMonoStyle{font_size: 8.25} } }
    }

    /** One choice of a closed question. `state` is set at draw time: 0 plain,
        1 under the cursor (the wash), 2 the right one, 3 the wrong one picked. */
    mod.widgets.FluentChoice = View {
        visible: false
        width: Fill, height: Fit
        flow: Right, spacing: 10
        align: Align{y: 0.5}
        padding: Inset{left: 8, right: 10, top: 6, bottom: 6}
        show_bg: true
        draw_bg +: {
            state: uniform(0.0)
            pixel: fn() {
                let px = 1.0 / self.rect_size.x
                let py = 1.0 / self.rect_size.y
                if self.pos.x < px || self.pos.x > 1.0 - px || self.pos.y < py || self.pos.y > 1.0 - py {
                    if self.state > 0.5 && self.state < 2.5 {
                        return vec4(0.078, 0.078, 0.078, 1.0)
                    }
                    return vec4(0.863, 0.863, 0.863, 1.0)
                }
                if self.state > 0.5 && self.state < 1.5 {
                    return vec4(0.906, 0.906, 0.906, 1.0)
                }
                return vec4(1.0, 1.0, 1.0, 1.0)
            }
        }
        key := mod.widgets.FluentKey {}
        text_lbl := mod.widgets.FluentProse { width: Fill }
        mark_lbl := mod.widgets.SLabel { width: Fit, text: "", draw_text +: { color: #5a5a5a } }
    }

    /** The lesson's progress as a filled hairline. */
    mod.widgets.FluentProgress = View {
        width: Fill, height: 2
        show_bg: true
        draw_bg +: {
            progress: uniform(0.0)
            pixel: fn() {
                if self.pos.x < self.progress {
                    return vec4(0.078, 0.078, 0.078, 1.0)
                }
                return vec4(0.863, 0.863, 0.863, 1.0)
            }
        }
    }

    /** One cell of the activity strip or a mastery row: a hairline box
        filled by `level`, 0 to 1. */
    mod.widgets.FluentCell = View {
        width: 14, height: 14
        margin: Inset{right: 3}
        show_bg: true
        draw_bg +: {
            level: uniform(0.0)
            pixel: fn() {
                let px = 1.0 / self.rect_size.x
                let py = 1.0 / self.rect_size.y
                if self.pos.x < px || self.pos.x > 1.0 - px || self.pos.y < py || self.pos.y > 1.0 - py {
                    return vec4(0.863, 0.863, 0.863, 1.0)
                }
                let g = 1.0 - 0.922 * self.level
                return vec4(g, g, g, 1.0)
            }
        }
    }
    mod.widgets.FluentWeek = View {
        width: Fit, height: Fit, flow: Right
        d0 := mod.widgets.FluentCell {}
        d1 := mod.widgets.FluentCell {}
        d2 := mod.widgets.FluentCell {}
        d3 := mod.widgets.FluentCell {}
        d4 := mod.widgets.FluentCell {}
        d5 := mod.widgets.FluentCell {}
        d6 := mod.widgets.FluentCell {}
    }
    mod.widgets.FluentStrip = View {
        width: Fit, height: Fit, flow: Down, spacing: 3
        w0 := mod.widgets.FluentWeek {}
        w1 := mod.widgets.FluentWeek {}
        w2 := mod.widgets.FluentWeek {}
        w3 := mod.widgets.FluentWeek {}
        w4 := mod.widgets.FluentWeek {}
        w5 := mod.widgets.FluentWeek {}
        w6 := mod.widgets.FluentWeek {}
        w7 := mod.widgets.FluentWeek {}
    }
    /** Mastery, 0 to 5, as five cells. */
    mod.widgets.FluentStars = View {
        width: Fit, height: Fit, flow: Right
        s0 := mod.widgets.FluentCell {}
        s1 := mod.widgets.FluentCell {}
        s2 := mod.widgets.FluentCell {}
        s3 := mod.widgets.FluentCell {}
        s4 := mod.widgets.FluentCell {}
    }
    /** One lesson's accuracy as a bar, filled from the foot. */
    mod.widgets.FluentBar = View {
        width: 18, height: 44
        margin: Inset{right: 4}
        show_bg: true
        draw_bg +: {
            level: uniform(0.0)
            pixel: fn() {
                let px = 1.0 / self.rect_size.x
                let py = 1.0 / self.rect_size.y
                if self.pos.x < px || self.pos.x > 1.0 - px || self.pos.y < py || self.pos.y > 1.0 - py {
                    return vec4(0.863, 0.863, 0.863, 1.0)
                }
                if 1.0 - self.pos.y < self.level {
                    return vec4(0.078, 0.078, 0.078, 1.0)
                }
                return vec4(1.0, 1.0, 1.0, 1.0)
            }
        }
    }
    mod.widgets.FluentBars = View {
        width: Fit, height: Fit, flow: Right
        b0 := mod.widgets.FluentBar {}
        b1 := mod.widgets.FluentBar {}
        b2 := mod.widgets.FluentBar {}
        b3 := mod.widgets.FluentBar {}
        b4 := mod.widgets.FluentBar {}
        b5 := mod.widgets.FluentBar {}
        b6 := mod.widgets.FluentBar {}
        b7 := mod.widgets.FluentBar {}
        b8 := mod.widgets.FluentBar {}
        b9 := mod.widgets.FluentBar {}
    }

    /** A tile on the desk: a caption, a number, a word under it. */
    mod.widgets.FluentTile = mod.widgets.FluentBox {
        width: Fill, spacing: 2
        cap_lbl := mod.widgets.SSection { text: "" }
        val_lbl := mod.widgets.SBoldLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: "", draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 13.0} } }
        sub_lbl := mod.widgets.FluentMuted { max_lines: 2, text: "" }
    }

    // ---- the desk ----------------------------------------------------------

    mod.widgets.FluentDeskPanel = set_type_default() do #(DeskPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 10
        padding: Inset{left: 14, right: 14, top: 12, bottom: 10}
        greet_lbl := mod.widgets.FluentProseBig { text: "" }
        streak_lbl := mod.widgets.FluentMuted { text: "" }
        View { width: Fill, height: 4 }
        shelf := mod.widgets.FluentBox {
            shelf_cap := mod.widgets.SSection { width: Fill, text: "" }
            shelf_title := mod.widgets.FluentProseBold { text: "" }
            shelf_focus := mod.widgets.FluentMuted { max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: "" }
            shelf_meta := mod.widgets.SLabel { width: Fill, text: "" }
            shelf_note := mod.widgets.FluentMuted { visible: false, text: "" }
        }
        tiles := View {
            width: Fill, height: Fit, flow: Right, spacing: 8
            due_tile := mod.widgets.FluentTile {}
            acc_tile := mod.widgets.FluentTile {}
            words_tile := mod.widgets.FluentTile {}
        }
    }

    // ---- the lesson --------------------------------------------------------

    /** The stage: what one exercise is drawn from, every piece there and
        shown as the kind and the phase call for. */
    mod.widgets.FluentStage = View {
        width: Fill, height: Fit, flow: Down, spacing: 10
        passage_box := mod.widgets.FluentBox {
            visible: false
            passage_cap := mod.widgets.SSection { text: "TEXT" }
            passage_txt := mod.widgets.FluentProse { text: "" }
        }
        prompt_txt := mod.widgets.FluentProseBig { text: "" }
        direction_lbl := mod.widgets.FluentMuted { visible: false, text: "→ auf Deutsch" }
        transcript_txt := mod.widgets.FluentProseItalic { visible: false, text: "" }
        field_wrap := View {
            visible: false, width: Fill, height: Fit
            answer_input := mod.widgets.SField {
                empty_text: "…"
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
                draw_text +: { text_style: mod.widgets.SProseStyle{font_size: 12.75} }
            }
        }
        editor_wrap := View {
            visible: false, width: Fill, height: Fit
            editor_input := mod.widgets.SField {
                is_multiline: true
                height: 92
                empty_text: "…"
                return_key_type: ReturnKeyType.Default
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
                draw_text +: { text_style: mod.widgets.SProseStyle{font_size: 12.75} }
            }
        }
        c0 := mod.widgets.FluentChoice {}
        c1 := mod.widgets.FluentChoice {}
        c2 := mod.widgets.FluentChoice {}
        c3 := mod.widgets.FluentChoice {}
        h0 := mod.widgets.FluentProseItalic { visible: false, text: "" }
        h1 := mod.widgets.FluentProseItalic { visible: false, text: "" }
        h2 := mod.widgets.FluentProseItalic { visible: false, text: "" }
        verdict := mod.widgets.FluentWash {
            visible: false
            head_lbl := mod.widgets.FluentProseBold { text: "" }
            yours_lbl := mod.widgets.FluentMuted { visible: false, text: "" }
            key_txt := mod.widgets.FluentProse { visible: false, text: "" }
            expl_txt := mod.widgets.FluentProse { visible: false, text: "" }
        }
        model_box := mod.widgets.FluentBox {
            visible: false
            mod.widgets.SSection { text: "MODEL ANSWER" }
            model_txt := mod.widgets.FluentProse { text: "" }
            also_lbl := mod.widgets.FluentMuted { visible: false, text: "" }
            mine_cap := mod.widgets.SSection { text: "YOURS", margin: Inset{top: 4} }
            mine_txt := mod.widgets.FluentProse { text: "" }
            howdid_lbl := mod.widgets.SSection { text: "HOW DID YOU DO?", margin: Inset{top: 4} }
        }
    }

    /** The summary's head: the word, the numbers, the calibration, the
        tutor's notes, and the caption over the corrections. */
    mod.widgets.FluentSummary = View {
        width: Fill, height: Fit, flow: Down, spacing: 6
        done_lbl := mod.widgets.FluentProseBig { text: "Geschafft!" }
        stats_lbl := mod.widgets.SBoldLabel { width: Fill, text: "" }
        streak_lbl := mod.widgets.FluentMuted { text: "" }
        calib_lbl := mod.widgets.FluentMuted { visible: false, text: "" }
        notes_txt := mod.widgets.FluentProse { visible: false, text: "" }
        View { width: Fill, height: 6 }
        corr_cap := mod.widgets.SSection { text: "CORRECTIONS" }
        mod.widgets.TblHeadRule {}
        none_lbl := mod.widgets.FluentMuted { visible: false, text: "no mistakes — a clean lesson" }
    }
    /** One correction of the summary. */
    mod.widgets.FluentFix = View {
        width: Fill, height: Fit, flow: Down, spacing: 2
        padding: Inset{top: 6, bottom: 6}
        q_lbl := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #5a5a5a } }
        line_txt := mod.widgets.FluentProse { text: "" }
        note_lbl := mod.widgets.FluentMuted { visible: false, text: "" }
        mod.widgets.TblHairline {}
    }
    /** The line under the summary: what the tutor is doing now. */
    mod.widgets.FluentBuilding = View {
        width: Fill, height: Fit, flow: Down
        padding: Inset{top: 10}
        lbl := mod.widgets.FluentMuted { text: "" }
    }

    mod.widgets.FluentLessonPanel = set_type_default() do #(LessonPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 8
        padding: Inset{left: 14, right: 14, top: 10, bottom: 10}
        head := View {
            width: Fill, height: Fit, flow: Right, align: Align{y: 0.5}
            caption_lbl := mod.widgets.SSection { width: Fill, text: "" }
            count_lbl := mod.widgets.SSection { width: Fit, text: "" }
        }
        progress := mod.widgets.FluentProgress {}
        View { width: Fill, height: 4 }
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            stage := mod.widgets.FluentStage {}
            summary := mod.widgets.FluentSummary {}
            fix := mod.widgets.FluentFix {}
            building := mod.widgets.FluentBuilding {}
        }
    }

    // ---- the review --------------------------------------------------------

    mod.widgets.FluentReviewPanel = set_type_default() do #(ReviewPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 10
        padding: Inset{left: 14, right: 14, top: 10, bottom: 10}
        head := View {
            width: Fill, height: Fit, flow: Right, align: Align{y: 0.5}
            count_lbl := mod.widgets.SSection { width: Fill, text: "" }
            split_lbl := mod.widgets.SSection { width: Fit, text: "" }
        }
        card := mod.widgets.FluentBox {
            visible: false
            padding: Inset{left: 16, right: 16, top: 22, bottom: 22}
            spacing: 10
            front_txt := mod.widgets.FluentProseBig { text: "", align: Align{x: 0.5} }
            back_txt := mod.widgets.FluentProse { visible: false, text: "", align: Align{x: 0.5} }
            example_txt := mod.widgets.FluentProseItalic { visible: false, text: "", align: Align{x: 0.5} }
            notes_lbl := mod.widgets.FluentMuted { visible: false, text: "", align: Align{x: 0.5} }
        }
        finished := View {
            visible: false, width: Fill, height: Fit, flow: Down, spacing: 8
            done_lbl := mod.widgets.FluentProseBig { text: "Alles erledigt!" }
            stats_lbl := mod.widgets.SBoldLabel { width: Fill, text: "" }
            saved_lbl := mod.widgets.FluentMuted { text: "grades saved — the tutor folds them into tomorrow's lesson" }
        }
        empty := View {
            visible: false, width: Fill, height: Fit, flow: Down, spacing: 8
            nothing_lbl := mod.widgets.FluentProseBig { text: "Nothing due today." }
            next_lbl := mod.widgets.FluentMuted { text: "" }
        }
    }

    // ---- setting the course up ---------------------------------------------

    mod.widgets.FluentSetupPanel = set_type_default() do #(SetupPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 8
        padding: Inset{left: 16, right: 16, top: 14, bottom: 14}
        mod.widgets.SSection { text: "THE LEARNER" }
        mod.widgets.SFormRow {
            mod.widgets.SFormLabel { text: "NAME" }
            name_input := mod.widgets.SField { empty_text: "Andrey" }
        }
        mod.widgets.SFormRow {
            mod.widgets.SFormLabel { text: "SPEAKS" }
            native_input := mod.widgets.SField { empty_text: "Russian" }
        }
        mod.widgets.SFormRow {
            mod.widgets.SFormLabel { text: "LEARNING" }
            target_input := mod.widgets.SField { empty_text: "German" }
        }
        View { width: Fill, height: 6 }
        mod.widgets.SSection { text: "THE COURSE" }
        mod.widgets.SFormRow {
            mod.widgets.SFormLabel { text: "LEVEL" }
            level_input := mod.widgets.SField { empty_text: "A1" }
        }
        mod.widgets.SFormRow {
            mod.widgets.SFormLabel { text: "GOAL" }
            goal_input := mod.widgets.SField { empty_text: "B1" }
        }
        mod.widgets.SFormRow {
            mod.widgets.SFormLabel { text: "MINUTES A DAY" }
            minutes_input := mod.widgets.SField { empty_text: "30" }
        }
        ladder_lbl := mod.widgets.FluentMuted { text: "the ladder is A1 A2 B1 B2 C1 C2 — where you stand, and where you are going" }
        error_lbl := mod.widgets.SLabel { visible: false, width: Fill, text: "", draw_text +: { color: #a01500 } }
    }

    // ---- the lists ---------------------------------------------------------

    mod.widgets.FluentCardBody = View {
        width: Fill, height: Fit, flow: Down, spacing: 3
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                front_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
            }
            due_lbl := mod.widgets.SLabel { width: Fit, draw_text +: { color: #909090 } }
        }
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                back_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, draw_text +: { color: #909090 } }
            }
            mastery_lbl := mod.widgets.SLabel { width: Fit, draw_text +: { color: #909090 } }
        }
    }
    mod.widgets.FluentCardRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.FluentCardBody {} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.FluentCardBody {} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.FluentCardBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.FluentCardBody {} }
        mod.widgets.TblHairline {}
    }
    mod.widgets.FluentCardsPanel = set_type_default() do #(CardsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 8 }
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                mod.widgets.SSection { text: "CARD" }
            }
            mod.widgets.SSection { width: Fit, text: "DUE · MASTERY" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.FluentCardRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    mod.widgets.FluentTopicBody = View {
        width: Fill, height: Fit, flow: Down, spacing: 3
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                title_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
            }
            level_lbl := mod.widgets.SLabel { width: Fit, draw_text +: { color: #909090 } }
        }
        summary_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, draw_text +: { color: #909090 } }
    }
    mod.widgets.FluentTopicRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.FluentTopicBody {} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.FluentTopicBody {} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.FluentTopicBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.FluentTopicBody {} }
        mod.widgets.TblHairline {}
    }
    mod.widgets.FluentGrammarPanel = set_type_default() do #(GrammarPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 8 }
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                mod.widgets.SSection { text: "TOPIC" }
            }
            mod.widgets.SSection { width: Fit, text: "LEVEL · MASTERY" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.FluentTopicRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    mod.widgets.FluentLessonBody = View {
        width: Fill, height: Fit, flow: Down, spacing: 3
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                title_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
            }
            date_lbl := mod.widgets.SLabel { width: Fit, draw_text +: { color: #909090 } }
        }
        detail_lbl := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, draw_text +: { color: #909090 } }
    }
    mod.widgets.FluentLessonRow = mod.widgets.TblRow {
        line := mod.widgets.TblLine { body := mod.widgets.FluentLessonBody {} }
        line_sel := mod.widgets.TblLineSel { body := mod.widgets.FluentLessonBody {} }
        line_mark := mod.widgets.TblLineMark { body := mod.widgets.FluentLessonBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.FluentLessonBody {} }
        mod.widgets.TblHairline {}
    }
    mod.widgets.FluentHistoryPanel = set_type_default() do #(HistoryPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 8 }
        View { width: Fill, height: Fit, flow: Right
            View { width: Fill, height: Fit
                mod.widgets.SSection { text: "LESSON" }
            }
            mod.widgets.SSection { width: Fit, text: "DAY" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.FluentLessonRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    // ---- the card ----------------------------------------------------------

    mod.widgets.FluentReviewLine = View {
        width: Fill, height: Fit, flow: Right, spacing: 12
        padding: Inset{top: 3, bottom: 3}
        date_lbl := mod.widgets.SLabel { width: 90, text: "", draw_text +: { color: #909090 } }
        q_lbl := mod.widgets.SLabel { width: Fill, text: "" }
        where_lbl := mod.widgets.SLabel { width: Fit, text: "", draw_text +: { color: #909090 } }
    }
    mod.widgets.FluentCardPanel = set_type_default() do #(CardPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 6
        padding: Inset{left: 14, right: 14, top: 12, bottom: 10}
        front_txt := mod.widgets.FluentProseBig { text: "" }
        back_txt := mod.widgets.FluentProse { text: "" }
        example_txt := mod.widgets.FluentProseItalic { text: "" }
        notes_lbl := mod.widgets.FluentMuted { text: "" }
        View { width: Fill, height: 4 }
        sched_lbl := mod.widgets.SLabel { width: Fill, text: "" }
        stars := mod.widgets.FluentStars {}
        View { width: Fill, height: 6 }
        mod.widgets.SSection { text: "REVIEWS" }
        mod.widgets.TblHeadRule {}
        none_lbl := mod.widgets.FluentMuted { visible: false, text: "never reviewed yet" }
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            rev := mod.widgets.FluentReviewLine {}
        }
    }

    // ---- the topic ---------------------------------------------------------

    mod.widgets.FluentTopicText = View {
        width: Fill, height: Fit, flow: Down, padding: Inset{bottom: 8}
        txt := mod.widgets.FluentProse { text: "" }
    }
    mod.widgets.FluentTopicTip = View {
        width: Fill, height: Fit, flow: Down, padding: Inset{bottom: 8}
        box := mod.widgets.FluentBox {
            mod.widgets.SSection { text: "TIP" }
            txt := mod.widgets.FluentProse { text: "" }
        }
    }
    mod.widgets.FluentTopicTable = View {
        width: Fill, height: Fit, flow: Down, padding: Inset{bottom: 8}
        box := mod.widgets.FluentBox {
            cap_lbl := mod.widgets.SSection { visible: false, text: "" }
            table_txt := mod.widgets.SText { is_multiline: true, text: "" }
        }
    }
    mod.widgets.FluentTopicExample = View {
        width: Fill, height: Fit, flow: Down, spacing: 2, padding: Inset{bottom: 6}
        txt := mod.widgets.FluentProse { text: "" }
        note_lbl := mod.widgets.FluentMuted { visible: false, text: "" }
    }
    mod.widgets.FluentTopicHead = View {
        width: Fill, height: Fit, flow: Down, padding: Inset{top: 8, bottom: 4}
        cap_lbl := mod.widgets.SSection { text: "" }
        mod.widgets.TblHeadRule {}
    }
    mod.widgets.FluentTopicNote = View {
        width: Fill, height: Fit, flow: Down, spacing: 2, padding: Inset{top: 4, bottom: 4}
        note_lbl := mod.widgets.SLabel { width: Fill, text: "" }
        where_lbl := mod.widgets.FluentMuted { text: "" }
    }
    mod.widgets.FluentTopicLink = View {
        width: Fill, height: Fit, flow: Right, padding: Inset{top: 3, bottom: 3}
        link := mod.widgets.SLink {}
    }
    mod.widgets.FluentTopicPanel = set_type_default() do #(TopicPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 6
        padding: Inset{left: 14, right: 14, top: 12, bottom: 10}
        title_txt := mod.widgets.FluentProseBold { text: "", draw_text +: { text_style: mod.widgets.SProseBoldStyle{font_size: 15.0} } }
        meta_lbl := mod.widgets.SLabel { width: Fill, text: "" }
        lessons_lbl := mod.widgets.FluentMuted { text: "" }
        View { width: Fill, height: 6 }
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            text := mod.widgets.FluentTopicText {}
            tip := mod.widgets.FluentTopicTip {}
            table := mod.widgets.FluentTopicTable {}
            example := mod.widgets.FluentTopicExample {}
            head := mod.widgets.FluentTopicHead {}
            note := mod.widgets.FluentTopicNote {}
            link := mod.widgets.FluentTopicLink {}
        }
    }

    // ---- progress ----------------------------------------------------------

    mod.widgets.FluentSkillRow = View {
        width: Fill, height: Fit, flow: Right, spacing: 10, align: Align{y: 0.5}
        padding: Inset{top: 3, bottom: 3}
        name_lbl := mod.widgets.SLabel { width: 110, text: "" }
        stars := mod.widgets.FluentStars {}
        val_lbl := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #909090 } }
    }
    mod.widgets.FluentErrorRow = View {
        width: Fill, height: Fit, flow: Down, spacing: 2
        padding: Inset{top: 4, bottom: 4}
        id_lbl := mod.widgets.SLabel { width: Fill, text: "" }
        ex_lbl := mod.widgets.FluentMuted { text: "" }
    }
    mod.widgets.FluentProgressBody = View {
        width: Fill, height: Fit, flow: Down, spacing: 8
        streak_lbl := mod.widgets.FluentProseBig { text: "" }
        streak_note := mod.widgets.FluentMuted { text: "" }
        View { width: Fill, height: 4 }
        mod.widgets.SSection { text: "ACTIVITY · LAST 8 WEEKS" }
        strip := mod.widgets.FluentStrip {}
        View { width: Fill, height: 4 }
        mod.widgets.SSection { text: "MASTERY" }
        sk0 := mod.widgets.FluentSkillRow {}
        sk1 := mod.widgets.FluentSkillRow {}
        sk2 := mod.widgets.FluentSkillRow {}
        sk3 := mod.widgets.FluentSkillRow {}
        sk4 := mod.widgets.FluentSkillRow {}
        View { width: Fill, height: 4 }
        mod.widgets.SSection { text: "ACCURACY · LAST 10 LESSONS" }
        bars := mod.widgets.FluentBars {}
        acc_lbl := mod.widgets.FluentMuted { text: "" }
        View { width: Fill, height: 4 }
        mod.widgets.SSection { text: "TOP ERRORS" }
        e0 := mod.widgets.FluentErrorRow {}
        e1 := mod.widgets.FluentErrorRow {}
        e2 := mod.widgets.FluentErrorRow {}
        View { width: Fill, height: 4 }
        mod.widgets.SSection { text: "WORDS" }
        words_lbl := mod.widgets.SLabel { width: Fill, text: "" }
    }
    mod.widgets.FluentProgressPanel = set_type_default() do #(ProgressPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 14, right: 14, top: 12, bottom: 10}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            body := mod.widgets.FluentProgressBody {}
        }
    }

    // ---- the migration form ------------------------------------------------

    mod.widgets.FluentImportPanel = set_type_default() do #(ImportPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down, spacing: 10
        padding: Inset{left: 16, right: 16, top: 16, bottom: 16}
        mod.widgets.SSection { text: "COURSE FOLDER" }
        path_input := mod.widgets.SField { width: Fill, empty_text: "~/Library/Mobile Documents/com~apple~CloudDocs/fluent" }
        mod.widgets.FluentMuted { text: "Reads the notebooks under data/: the profile, the schedule and its grades, the deck, the grammar, the error patterns, the sittings, and the lesson authored for the next day. What the course already has is left alone." }
        error_lbl := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #a01500 } }
        status_lbl := mod.widgets.SLabel { width: Fill, max_lines: 3, text: "" }
    }
}

/// Fluent's Makepad half.
pub struct Ui;
pub static UI: Ui = Ui;

impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Desk::TAG => Some(live_id!(fluent_desk_tpl)),
            Import::TAG => Some(live_id!(fluent_import_tpl)),
            Lesson::TAG => Some(live_id!(fluent_lesson_tpl)),
            Review::TAG => Some(live_id!(fluent_review_tpl)),
            Cards::TAG => Some(live_id!(fluent_cards_tpl)),
            Grammar::TAG => Some(live_id!(fluent_grammar_tpl)),
            History::TAG => Some(live_id!(fluent_history_tpl)),
            Card::TAG => Some(live_id!(fluent_card_tpl)),
            Topic::TAG => Some(live_id!(fluent_topic_tpl)),
            Progress::TAG => Some(live_id!(fluent_progress_tpl)),
            Setup::TAG => Some(live_id!(fluent_setup_tpl)),
            _ => None,
        }
    }
    fn scenes(&self) -> Vec<Scene<SceneSetup>> {
        super::scenes::scenes()
    }
}
