//! The agent's Makepad half: its templates, and the tag each of them draws.
//!
//! One `script_mod!` block, its ids all prefixed `agent_`, which is what
//! keeps two apps apart in one script virtual machine. The chassis is the
//! shell's — the table's filter and row twins, the field, the label, the
//! hairline — so this app declares what is its own: a chip's pill, the
//! wash a person's turn sits in, a tool call's card, and the row of the
//! agents list.
//!
//! The colours are the shell's four and no others: INK `#141414` for what
//! was said, SEL `#e7e7e7` for the wash behind a person's block, RULE
//! `#dcdcdc` for every border and hairline, MUTED `#909090` for reasoning,
//! usage and the marks a turn ended on, and ERR `#a01500` for a failure and
//! nothing else.

use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::*;

use crate::shell::app_ui::{AppUi, Setup};

use super::panels::{Agents, Chat};
use super::widgets::{AgentAgentsPanel, AgentChatPanel};
use super::AGENT;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the pieces a chat is built from -----------------------------------

    /** The wash behind a person's turn.

        A twin of the plain view rather than a colour set at draw time, and
        a custom pixel fn because a portal-item quad on the stock shader
        merges into a call that paints under the panel background and is
        never seen. */
    mod.widgets.AgentWash = View {
        width: Fit, height: Fit
        flow: Down
        show_bg: true
        draw_bg +: {
            color: #e7e7e7
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    /** A hairline box: a chip's pill and a call's card are the same idea at
        two sizes — one rule of a border, paper inside it. */
    mod.widgets.AgentBox = View {
        width: Fit, height: Fit
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

    /** One chip: a panel's title in the section register, in a pill. The
        `\u{d7}` is there only where the chip can still be taken off, which
        is the composer — a chip that has gone with a turn is what was
        asked, and there is no unasking it. */
    mod.widgets.AgentChip = mod.widgets.AgentBox {
        width: Fit, height: Fit
        flow: Right
        align: Align{y: 0.5}
        spacing: 5
        padding: Inset{left: 5, right: 5, top: 2, bottom: 2}
        cursor: MouseCursor.Hand
        chip_lbl := mod.widgets.SLabel {
            width: Fit, max_lines: 1, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 8.25} }
        }
        chip_x := View {
            visible: false
            width: Fit, height: Fit
            cursor: MouseCursor.Hand
            mod.widgets.SLabel {
                text: "×"
                draw_text +: {
                    color: #909090
                    text_style: mod.widgets.SMonoStyle{font_size: 8.25}
                }
            }
        }
    }

    /** A row of chips, wrapping: five by name, then a count. */
    mod.widgets.AgentChipRow = View {
        visible: false
        width: Fill, height: Fit
        flow: Flow.Right{wrap: true}
        spacing: 5
        wrap_spacing: 4
        k0 := mod.widgets.AgentChip {}
        k1 := mod.widgets.AgentChip {}
        k2 := mod.widgets.AgentChip {}
        k3 := mod.widgets.AgentChip {}
        k4 := mod.widgets.AgentChip {}
        chip_more := mod.widgets.SLabel {
            visible: false
            width: Fit, text: ""
            draw_text +: {
                color: #909090
                text_style: mod.widgets.SMonoStyle{font_size: 8.25}
            }
        }
    }

    /** A button on a card, drawn as the bar draws one: a lowercase label in
        INK inside a rule of the same colour.

        A view rather than a `ButtonFlat` because it lives inside the
        transcript's list, where an item's area goes stale on any redraw —
        so the press is answered by the rectangles of the last draw, like
        every other pressable thing here. */
    mod.widgets.AgentCardBtn = View {
        width: Fit, height: Fit
        show_bg: true
        cursor: MouseCursor.Hand
        padding: Inset{left: 12, right: 12, top: 3, bottom: 3}
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
        btn_lbl := mod.widgets.SLabel {
            width: Fit, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 8.25} }
        }
    }

    /** A muted run of text: the model's own reasoning, and what a call came
        to. Selectable like everything else the agent writes. */
    mod.widgets.AgentMuted = mod.widgets.SText {
        is_multiline: true
        draw_text +: {
            color: #909090
            color_hover: #909090
            color_focus: #909090
            color_down: #909090
            color_empty: #909090
        }
    }

    /** The same, in the one colour errors get. */
    mod.widgets.AgentErr = mod.widgets.SText {
        is_multiline: true
        draw_text +: {
            color: #a01500
            color_hover: #a01500
            color_focus: #a01500
            color_down: #a01500
            color_empty: #a01500
        }
    }

    // ---- one line of the transcript ------------------------------------------

    /** One item: a person's turn, the agent's, a tool call's card, or the
        sentence a run that came to nothing left. One template draws all of
        them — the parts an item is not made of are emptied, never merely
        hidden, because the next row to scroll in reuses this one. */
    mod.widgets.AgentTurn = View {
        width: Fill, height: Fit
        flow: Down
        padding: Inset{top: 4, bottom: 4}

        /* A person's: right, and washed. Its width is arithmetic — see
           `widgets::chat` — because a field lays its text out against the
           width it is given, and a `Fit` gives it none. */
        mine := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            align: Align{x: 1.0}
            wash := mod.widgets.AgentWash {
                width: Fit, height: Fit
                flow: Down
                spacing: 5
                padding: Inset{left: 8, right: 8, top: 6, bottom: 6}
                mine_chips := mod.widgets.AgentChipRow {}
                mine_txt := mod.widgets.SText { width: Fill, is_multiline: true }
            }
        }

        /* The agent's: left, plain, filling the column. */
        theirs := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            spacing: 3
            reason_fold := View {
                visible: false
                width: Fit, height: Fit
                cursor: MouseCursor.Hand
                mod.widgets.SLabel {
                    text: "› reasoning", draw_text +: { color: #909090 }
                }
            }
            reason_wrap := View {
                visible: false
                width: Fill, height: Fit
                reason_txt := mod.widgets.AgentMuted {}
            }
            theirs_txt := mod.widgets.SText { is_multiline: true }
            foot_lbl := mod.widgets.SLabel {
                visible: false
                text: ""
                draw_text +: {
                    color: #909090
                    text_style: mod.widgets.SMonoStyle{font_size: 8.25}
                }
            }
        }

        /* One tool call: what it did on the first line, and behind it what
           it came to — or why it did not, or, while it waits for the
           person's word, the two buttons that give it. */
        card := mod.widgets.AgentBox {
            visible: false
            width: Fill, height: Fit
            flow: Down
            spacing: 4
            margin: Inset{top: 3, bottom: 3}
            padding: Inset{left: 8, right: 8, top: 6, bottom: 6}
            card_line := View {
                width: Fill, height: Fit
                cursor: MouseCursor.Hand
                card_lbl := mod.widgets.SLabel {
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
            }
            card_out := View {
                visible: false
                width: Fill, height: Fit
                card_out_txt := mod.widgets.AgentMuted {}
            }
            card_err := View {
                visible: false
                width: Fill, height: Fit
                card_err_txt := mod.widgets.AgentErr {}
            }
            /* Up only while the call is asked: the person's two words,
               under the line saying what it would do. */
            card_btns := View {
                visible: false
                width: Fill, height: Fit
                flow: Right
                spacing: 8
                margin: Inset{top: 2}
                card_allow := mod.widgets.AgentCardBtn {}
                card_refuse := mod.widgets.AgentCardBtn {}
            }
        }

        /* What a round that came to nothing left. The bar offers *retry*
           beside it. */
        err := View {
            visible: false
            width: Fill, height: Fit
            err_txt := mod.widgets.AgentErr {}
        }
    }

    /** One conversation: the transcript above, the composer below.

        The list tails: an answer that lands while one is reading the foot
        of the transcript keeps the foot of the transcript on screen, and
        one scrolled back stays where it was put. */
    mod.widgets.AgentChatPanel = set_type_default() do #(AgentChatPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        // Measured, never drawn: a person's block is as wide as its longest
        // line, and the mono advance is what says how wide that is.
        draw_mono +: {
            text_style: mod.widgets.SMonoStyle{}
            color: #141414ff
        }

        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            auto_tail: true
            turn := mod.widgets.AgentTurn {}
        }
        View { width: Fill, height: 8 }
        mod.widgets.TblHairline {}
        /* *add panel*: the panels that are open, one pick apiece, over the
           chips the pick joins. Up while the verb asked for it; enter takes
           what the offer is showing, esc puts it away. */
        pick_row := View {
            visible: false
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            margin: Inset{top: 8}
            mod.widgets.SSection { width: 82, text: "ADD PANEL" }
            pick_input := mod.widgets.SField {
                empty_text: "a panel that is open"
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        chips := mod.widgets.AgentChipRow { margin: Inset{top: 8} }
        View { width: Fill, height: 8 }
        /* Two lines tall to start with, growing to six and scrolling past
           that. `enter` sends and `shift+enter` is a newline, which is the
           widget's doing: a multi-line field would take both. */
        ask_input := mod.widgets.SField {
            width: Fill
            height: Fit{min: FitBound.Abs(42), max: FitBound.Abs(110)}
            is_multiline: true
            empty_text: "ask…"
            return_key_type: ReturnKeyType.Default
        }
        // The pick field's own box, hung under it and drawn after
        // everything else so it covers the composer rather than moving it.
        suggest_pick: mod.widgets.TblSuggest {}
    }

    // ---- the agents list -----------------------------------------------------

    /** One chat as the list shows it: its title, then the model, what its
        newest round is doing, and when it last moved, on the columns the
        header draws.

        The body is declared once and hung in each of the row's four twins,
        so the cursor wash and the mark bar stay the shell's. */
    mod.widgets.AgentChatBody = View {
        width: Fill, height: Fit
        flow: Right
        align: Align{y: 0.5}
        // The title rides a Fill View whose flow is Down: a Fill label on a
        // Right flow's main axis defer-walks.
        View {
            width: Fill, height: Fit
            flow: Down
            title_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            }
        }
        View { width: 10, height: 1 }
        model_lbl := mod.widgets.SLabel {
            padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
        }
        View { width: 10, height: 1 }
        // A word for a round that is still going, muted; the same word in
        // ink where it failed, which is the one this list is for.
        state_lbl := mod.widgets.SLabel {
            padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
        }
        state_ink := mod.widgets.SLabel {
            visible: false
            padding: 0, width: Fit, text: ""
        }
        View { width: 10, height: 1 }
        date_lbl := mod.widgets.SLabel {
            padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
        }
    }

    /** A row of the agents list: the four twins, and the hairline under
        them. */
    mod.widgets.AgentChatsRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.AgentChatBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.AgentChatBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.AgentChatBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.AgentChatBody {} }
        mod.widgets.TblHairline {}
    }

    /** The chats: the filter over the column heads over the virtualized
        list. A rich table like any other — what this app adds is the row
        body above and the four functions beside it. */
    mod.widgets.AgentAgentsPanel = set_type_default() do #(AgentAgentsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        // No flow spacing: the gaps are explicit, so a rule sits the same
        // 3 pt under the header as under every row.
        spacing: 0

        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 6 }
        // Two heads for four columns: the model and the run's word ride
        // with the date at the right edge and own no column of their own.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "CHAT" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "WHEN" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            row := mod.widgets.AgentChatsRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }
}

/// The agent's Makepad half.
pub struct Ui;

/// The one in this build.
pub static UI: Ui = Ui;

impl AppUi for Ui {
    /// Installed once, at startup, which is also the one place with a
    /// window in reach: the engine asks for a frame through makepad's own
    /// signal, the way the kernel's workers wake it.
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        AGENT.set_wake(SignalToUI::set_ui_signal);
        self::script_mod(vm)
    }

    /// Two tags, two templates: a conversation, and the list of them.
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Chat::TAG => Some(live_id!(agent_chat_tpl)),
            Agents::TAG => Some(live_id!(agent_agents_tpl)),
            _ => None,
        }
    }

    fn scenes(&self) -> Vec<Scene<Setup>> {
        super::scenes::scenes()
    }
}
