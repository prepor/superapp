//! Telegram's Makepad half: its templates, and the tag each of them draws.
//!
//! One `script_mod!` block, its ids all prefixed `telegram_`, which is what
//! keeps two apps apart in one script virtual machine. The chassis is the
//! shell's — the table's filter and row twins, the field, the label, the
//! rule — so an app declares what is its own and nothing else: a chat row's
//! two lines and its count, a message's header and body, the composer.

use kernel::panel::{Panel, Tag};
use kernel::scene::Scene;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::app_ui::{AppUi, Setup};
use crate::shell::hosted::PanelProps;

use super::panels::{
    Attach, Chat, Chats, Contacts, Line, Members, Messages, Peer, Place, SignIn, Viewer,
};
use super::widgets::feedback::TelegramFeedback;
use super::widgets::inline_video::InlineVideoSlot;
use super::widgets::{
    AttachPanel, ChatPanel, ChatsPanel, LinePanel, MessagesPanel, PeerPanel, PeoplePanel,
    PlacePanel, ViewerPanel,
};
use super::{panels::Topics, widgets::TopicsPanel};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.TelegramInlineVideo = set_type_default() do #(InlineVideoSlot::register_widget(vm)) {
        ..mod.widgets.View
        visible: false
        width: 320, height: 180
        flow: Overlay
        margin: Inset{top: 2, bottom: 2}
        show_bg: true
        draw_bg +: { color: #141414 }
        poster := mod.widgets.MediaPicture {
            width: Fill, height: Fill
            margin: Inset{}
            img +: { width: Fill, height: Fill, fit: ImageFit.CropToFill }
        }
        playback := View { width: Fill, height: Fill }
        status := View {
            visible: false
            width: Fill, height: Fill
            align: Align{y: 1.0}
            View {
                width: Fill, height: Fit
                show_bg: true
                draw_bg +: { color: #141414 }
                download_lbl := mod.widgets.SLabel {
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis
                    padding: Inset{left: 6, right: 6, top: 4, bottom: 4}
                    text: "", draw_text +: { color: #ffffff }
                }
            }
        }
    }

    mod.widgets.TelegramFeedback = set_type_default() do #(TelegramFeedback::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        status := mod.widgets.SLabel {
            visible: false, width: Fill, text: ""
            draw_text +: { color: #5a5a5a }
        }
        error := mod.widgets.SLabel {
            visible: false, width: Fill, text: ""
            draw_text +: { color: #b23b2a }
        }
        controls := View {
            visible: false, width: Fill, height: Fit, spacing: 8
            retry := mod.widgets.SBtn { text: "retry" }
            dismiss := mod.widgets.SBtn { text: "dismiss" }
        }
    }

    // ---- the count -----------------------------------------------------------

    /** The unread count: white digits in an ink box, the one filled box in
        the language. A custom pixel fn, because a stock quad inside a
        portal item merges under the panel background. */
    mod.widgets.TelegramBadge = View {
        visible: false
        width: Fit, height: Fit
        margin: Inset{left: 8}
        padding: Inset{left: 5, right: 5, top: 1, bottom: 1}
        show_bg: true
        draw_bg +: {
            color: #141414
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
        lbl := mod.widgets.SLabel {
            text: ""
            draw_text +: {
                color: #ffffff
                text_style: mod.widgets.SMonoStyle{font_size: 8.25}
            }
        }
    }

    /** The same for a muted chat: outlined, quiet. */
    mod.widgets.TelegramBadgeMuted = View {
        visible: false
        width: Fit, height: Fit
        margin: Inset{left: 8}
        padding: Inset{left: 5, right: 5, top: 1, bottom: 1}
        show_bg: true
        draw_bg +: {
            color: #909090
            pixel: fn() {
                let px = 1.0 / self.rect_size.x
                let py = 1.0 / self.rect_size.y
                if self.pos.x < px || self.pos.x > 1.0 - px || self.pos.y < py || self.pos.y > 1.0 - py {
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
                return vec4(0.0, 0.0, 0.0, 0.0)
            }
        }
        lbl := mod.widgets.SLabel {
            text: ""
            draw_text +: {
                color: #5a5a5a
                text_style: mod.widgets.SMonoStyle{font_size: 8.25}
            }
        }
    }

    /** A message's text and caption, with selectable, underlined links. */
    mod.widgets.TelegramText = Html {
        width: Fill, height: Fit
        padding: 0, margin: 0
        selectable: true
        font_size: 10.5
        font_color: #141414
        draw_text +: { color: #141414 }
        text_style_normal: mod.widgets.SMonoStyle{}
        a := mod.widgets.HtmlLink {
            color: #5a5a5a
            pressed_color: #141414
        }
        draw_selection +: {
            draw_call_group: @selection
            color: #00000020
        }
    }

    // ---- the chat list ---------------------------------------------------------

    /** One chat as the list shows it, two lines: the title and the last
        line's time, then what it last said and the count. Bold while
        anything is unread — a twin per title, because a label's style is
        not a runtime value.

        The body is declared once and hung in each of the row's four twins,
        so the cursor wash and the mark bar stay the shell's. */
    mod.widgets.TelegramChatBody = View {
        width: Fill, height: Fit
        flow: Down
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            // The title rides a Fill View whose flow is Down: a Fill label
            // on a Right flow's main axis defer-walks.
            View {
                width: Fill, height: Fit
                flow: Down
                title_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
                title_b := mod.widgets.SBoldLabel {
                    visible: false
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
            }
            View { width: 10, height: 1 }
            // My last line's state, before the time.
            state_lbl := mod.widgets.SLabel {
                visible: false
                padding: 0, width: Fit, text: "", margin: Inset{right: 6}
                draw_text +: { color: #909090 }
            }
            state_err := mod.widgets.SLabel {
                visible: false
                padding: 0, width: Fit, text: "failed", margin: Inset{right: 6}
                draw_text +: { color: #a01500 }
            }
            when_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            View {
                width: Fill, height: Fit
                flow: Down
                preview_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #5a5a5a }
                }
            }
            pinned_lbl := mod.widgets.SLabel {
                visible: false
                padding: 0, width: Fit, text: "pinned", margin: Inset{left: 10}
                draw_text +: { color: #909090 }
            }
            mentions := mod.widgets.TelegramBadge {}
            badge := mod.widgets.TelegramBadge {}
            badge_muted := mod.widgets.TelegramBadgeMuted {}
        }
    }

    /** A row of the chat list: the four twins, and the hairline under them. */
    mod.widgets.TelegramChatRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.TelegramChatBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.TelegramChatBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.TelegramChatBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.TelegramChatBody {} }
        mod.widgets.TblHairline {}
    }

    /** The chat list: the filter over the column heads over the rows. A
        rich table like any other — what telegram adds is the row body above
        and the functions beside it. */
    mod.widgets.TelegramChatsPanel = set_type_default() do #(ChatsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 6 }
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "CHAT" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "LAST" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            row := mod.widgets.TelegramChatRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    mod.widgets.TelegramTopicBody = View {
        width: Fill, height: Fit
        spacing: 10
        check_lbl := mod.widgets.SLabel { padding: 0, width: Fit, text: "[ ]" }
        View {
            width: Fill, height: Fit, flow: Down
            name_lbl := mod.widgets.SLabel {
                padding: 0, width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis
            }
            detail_lbl := mod.widgets.SLabel {
                padding: 0, width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis
                draw_text +: { color: #5a5a5a }
            }
        }
    }
    mod.widgets.TelegramTopicRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.TelegramTopicBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.TelegramTopicBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.TelegramTopicBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.TelegramTopicBody {} }
        mod.widgets.TblHairline {}
    }
    mod.widgets.TelegramTopicsPanel = set_type_default() do #(TopicsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill, flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 8
        filter_input := mod.widgets.TblFilter { empty_text: "filter topics" }
        status_lbl := mod.widgets.SLabel { width: Fill, draw_text +: { color: #5a5a5a } }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill, flow: Down, reuse_items: true
            row := mod.widgets.TelegramTopicRow {}
        }
    }
    // ---- the transcript ----------------------------------------------------------

    /** One message: a header line — the writer, and at the right what the
        line carries about itself and the time — then, each only when there,
        whom it was forwarded from, the line it answers, the text as a
        selectable run, the media in a word, and the reactions with a
        channel post's comments. */
    mod.widgets.TelegramMsgBody = View {
        width: Fill, height: Fit
        flow: Down
        spacing: 2
        head := View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            name_b := mod.widgets.SBoldLabel { width: Fit, max_lines: 1, text: "" }
            name_me := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "me", draw_text +: { color: #909090 }
            }
            View { width: Fill, height: 1 }
            edited_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "edited", margin: Inset{right: 8}
                draw_text +: { color: #909090 }
            }
            views_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "", margin: Inset{right: 8}
                draw_text +: { color: #909090 }
            }
            state_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "", margin: Inset{right: 6}
                draw_text +: { color: #909090 }
            }
            state_err := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "failed", margin: Inset{right: 6}
                draw_text +: { color: #a01500 }
            }
            time_lbl := mod.widgets.SLabel {
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        fwd_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
        reply_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
        /* What the line carries, through the shell's kit: a picture — a
           photo, or a video's poster — a map for a place, the player over
           a recording, and a sticker as its emoji drawn large until stickers
           are drawn. */
        img_box := mod.widgets.MediaPicture {}
        clip_box := mod.widgets.TelegramInlineVideo {}
        map := mod.widgets.MediaMap {}
        sticker_lbl := mod.widgets.SLabel {
            visible: false
            width: Fit, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 30.0} }
        }
        // Html has no `visible` of its own; the wrapper hides empty captions.
        text_wrap := View {
            width: Fill, height: Fit
            body_txt := mod.widgets.TelegramText {}
        }
        media_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #5a5a5a }
        }
        player := mod.widgets.MediaPlayer {}
        foot := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            spacing: 4
            reactions_lbl := mod.widgets.SLabel {
                visible: false
                width: Fill
                text: "", draw_text +: { color: #5a5a5a }
            }
            comments_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
    }

    /** One row of the transcript: a day's caption, the unread line, a
        service line, or a message — one template, one part shown. The
        message hangs its body in the four twins, so the cursor wash and
        the mark bar stay the shell's. */
    mod.widgets.TelegramMsgRow = View {
        width: Fill, height: Fit
        flow: Down
        day := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            padding: Inset{top: 10, bottom: 2}
            View {
                width: Fill, height: Fit
                padding: Inset{left: 8, right: 8, bottom: 3}
                day_lbl := mod.widgets.SSection { text: "" }
            }
            mod.widgets.TblHairline {}
        }
        unread := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            padding: Inset{top: 6, bottom: 2}
            View {
                width: Fill, height: Fit
                padding: Inset{left: 8, right: 8, bottom: 3}
                mod.widgets.SSection { text: "UNREAD" }
            }
            mod.widgets.TblBandRule {}
        }
        service := View {
            visible: false
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 5, bottom: 5}
            service_lbl := mod.widgets.SLabel {
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                draw_text +: { color: #909090 }
            }
        }
        msg := mod.widgets.TblRow {
            line          := mod.widgets.TblLine        { padding: Inset{left: 8, right: 8, top: 5, bottom: 5}, body := mod.widgets.TelegramMsgBody {} }
            line_sel      := mod.widgets.TblLineSel     { padding: Inset{left: 8, right: 8, top: 5, bottom: 5}, body := mod.widgets.TelegramMsgBody {} }
            line_mark     := mod.widgets.TblLineMark    { padding: Inset{left: 8, right: 8, top: 5, bottom: 5}, body := mod.widgets.TelegramMsgBody {} }
            line_mark_sel := mod.widgets.TblLineMarkSel { padding: Inset{left: 8, right: 8, top: 5, bottom: 5}, body := mod.widgets.TelegramMsgBody {} }
        }
    }

    /** One conversation: the status line under the header, the transcript,
        and the composer at the foot with the reply line above it while
        replying. The verbs are on the bar, where every verb is. */
    mod.widgets.TelegramChatPanel = set_type_default() do #(ChatPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        // Native events reach one stable player, independent of virtual rows.
        video_source := View {
            visible: false
            clip_box := mod.widgets.MediaVideo {}
        }

        status_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            margin: Inset{left: 8, bottom: 6}
            draw_text +: { color: #909090 }
        }
        mod.widgets.TblHairline {}
        empty_lbl := mod.widgets.TblEmpty { text: "no messages here yet" }
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            // At the end, the transcript stays at the end as lines arrive;
            // scrolled up, it stays put. Scrolling back down to the end
            // re-arms it (Andrey, 2026-09-07: new messages follow if I am
            // at the bottom).
            auto_tail: true
            row := mod.widgets.TelegramMsgRow {}
            end_space := View { width: Fill, height: 0 }
        }
        drop_hint := mod.widgets.SLabel {
            visible: false, width: Fill, text: "drop files to attach · enter to send"
            draw_text +: { color: #5a5a5a }
        }
        reply_row := View {
            visible: false
            width: Fill, height: Fit
            align: Align{y: 0.5}
            padding: Inset{left: 8, top: 6}
            reply_lbl := mod.widgets.SLabel {
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                draw_text +: { color: #5a5a5a }
            }
        }
        /* What the composer will send with the text: one link a file, each
           opening the card over that path — as a letter's CARRIES line
           does. */
        carries := View {
            visible: false
            width: Fill, height: Fit
            align: Align{y: 0.5}
            padding: Inset{left: 8, top: 6}
            mod.widgets.SSection { width: 82, text: "CARRIES" }
            files := View {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                spacing: 14
                f0 := mod.widgets.SLink {}
                f1 := mod.widgets.SLink {}
                f2 := mod.widgets.SLink {}
                f3 := mod.widgets.SLink {}
                f4 := mod.widgets.SLink {}
                more_lbl := mod.widgets.SLabel {
                    visible: false
                    width: Fit, text: "", draw_text +: { color: #909090 }
                }
            }
        }
        composer := View {
            width: Fill, height: Fit
            margin: Inset{top: 8}
            input := mod.widgets.SField {
                // Long pasted, synced or agent-written drafts scroll inside
                // the field instead of taking the transcript off the panel.
                height: Fit{max: FitBound.Abs(110)}
                is_multiline: true
                empty_text: "write a message…  ( enter )"
                // Enter is answered by the panel — it sends — before the
                // field sees it; shift+enter is the field's newline.
                return_key_type: ReturnKeyType.Default
            }
        }
        cannot_lbl := mod.widgets.SLabel {
            visible: false
            margin: Inset{left: 8, top: 8}
            text: "you can't post here", draw_text +: { color: #909090 }
        }
    }

    // ---- the messages list -------------------------------------------------------

    /** One message as a list finds it: where and who with the time, then the
        line itself. */
    mod.widgets.TelegramHitBody = View {
        width: Fill, height: Fit
        flow: Down
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            View {
                width: Fill, height: Fit
                flow: Down
                where_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #5a5a5a }
                }
            }
            View { width: 10, height: 1 }
            when_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        line_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
    }

    mod.widgets.TelegramHitRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.TelegramHitBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.TelegramHitBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.TelegramHitBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.TelegramHitBody {} }
        mod.widgets.TblHairline {}
    }

    /** The messages a filter finds: the filter, the heads, the rows. */
    mod.widgets.TelegramMessagesPanel = set_type_default() do #(MessagesPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 6 }
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "WHERE" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "WHEN" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            row := mod.widgets.TelegramHitRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    // ---- the people ------------------------------------------------------------------

    /** One person: the name, and under it the presence and the username,
        muted. */
    mod.widgets.TelegramPersonBody = View {
        width: Fill, height: Fit
        flow: Down
        name_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        detail_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
    }

    mod.widgets.TelegramPersonRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.TelegramPersonBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.TelegramPersonBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.TelegramPersonBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.TelegramPersonBody {} }
        mod.widgets.TblHairline {}
    }

    /** The address book, or a group's members: the filter, one head, the
        rows. */
    mod.widgets.TelegramPeoplePanel = set_type_default() do #(PeoplePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        filter_input := mod.widgets.TblFilter {}
        filter_err_lbl := mod.widgets.TblErr {}
        View { width: Fill, height: 6 }
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            mod.widgets.SSection { padding: 0, text: "NAME" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            row := mod.widgets.TelegramPersonRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    // ---- one line, as a card ----------------------------------------------------------

    /** One line of a chat, whole: the header the transcript draws, what it
        answers or was forwarded from, its media at the card's width — a
        picture, a map, the player — and its text as a selectable run. The
        verbs on one line are on the bar. */
    mod.widgets.TelegramLinePanel = set_type_default() do #(LinePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 4

        gone_lbl := mod.widgets.SLabel {
            visible: false
            text: "this line is gone", draw_text +: { color: #909090 }
        }
        head := View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            name_b := mod.widgets.SBoldLabel { width: Fit, max_lines: 1, text: "" }
            name_me := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "me", draw_text +: { color: #909090 }
            }
            View { width: Fill, height: 1 }
            edited_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "edited", margin: Inset{right: 8}
                draw_text +: { color: #909090 }
            }
            views_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "", margin: Inset{right: 8}
                draw_text +: { color: #909090 }
            }
            state_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "", margin: Inset{right: 6}
                draw_text +: { color: #909090 }
            }
            state_err := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "failed", margin: Inset{right: 6}
                draw_text +: { color: #a01500 }
            }
            time_lbl := mod.widgets.SLabel {
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        mod.widgets.TblHairline {}
        fwd_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
        reply_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
        img_box := mod.widgets.MediaPicture {}
        clip_box := mod.widgets.TelegramInlineVideo {}
        video_source := View {
            visible: false
            clip_box := mod.widgets.MediaVideo {}
        }
        map := mod.widgets.MediaMap {}
        sticker_lbl := mod.widgets.SLabel {
            visible: false
            width: Fit, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 30.0} }
        }
        text_wrap := View {
            width: Fill, height: Fit
            body_txt := mod.widgets.TelegramText {}
        }
        media_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #5a5a5a }
        }
        player := mod.widgets.MediaPlayer {}
        foot := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            spacing: 4
            reactions_lbl := mod.widgets.SLabel {
                visible: false
                width: Fill
                text: "", draw_text +: { color: #5a5a5a }
            }
            comments_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
    }

    // ---- the viewer -------------------------------------------------------------------

    /** One line's media sized from its content: the picture fitted to
        the panel, or the clip playing where the file is here, or the word
        where there is neither, the player over a recording, and the caption
        under it. The walk through the chat's media and the system's opener
        are on the bar. */
    mod.widgets.TelegramViewerPanel = set_type_default() do #(ViewerPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 8

        file_name := mod.widgets.SBoldLabel { visible: false, width: Fill, text: "" }
        file_view := mod.widgets.FileViewer { visible: false }

        // The middle of the panel: the picture fitted to it, or — for what
        // has no face — the sticker's emoji drawn large, or the word over
        // the player.
        body := View {
            width: Fill, height: Fill
            flow: Down
            align: Align{x: 0.5, y: 0.5}
            spacing: 12
            big := View {
                visible: false
                width: Fill, height: Fill
                align: Align{x: 0.5, y: 0.5}
                img := mod.widgets.Image {
                    width: Fill, height: Fill
                    fit: ImageFit.Smallest
                }
            }
            sticker_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: ""
                draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 96.0} }
            }
            word_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: ""
                draw_text +: { color: #5a5a5a, text_style: mod.widgets.SMonoStyle{font_size: 13.0} }
            }
            // The clip, once its file is on this device. Hidden until then,
            // and hidden for good on a line that carries no moving picture,
            // so the poster keeps the whole box.
            clip_box := mod.widgets.MediaVideo {}
            player_box := View {
                width: Fill, height: Fit
                player := mod.widgets.MediaPlayer {}
            }
            // Downloaded / total bytes for the clip or picture on its way.
            // Empty and hidden once the file is here.
            note_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: ""
                draw_text +: { color: #909090 }
            }
        }
        caption_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 2, text_overflow: TextOverflow.Ellipsis, text: ""
        }
    }

    // ---- what goes with the next message -----------------------------------------------

    /** One file the composer will send: the name, and under it what it goes
        as and where it is, muted. */
    mod.widgets.TelegramAttachBody = View {
        width: Fill, height: Fit
        flow: Down
        name_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        detail_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
    }

    mod.widgets.TelegramAttachRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.TelegramAttachBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.TelegramAttachBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.TelegramAttachBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.TelegramAttachBody {} }
        mod.widgets.TblHairline {}
    }

    /** What goes with the next message, in the order it will go, under the
        caption the composer's own line wears; or, while a recording runs,
        the strip alone — what and how long, the keys under it, the level,
        and the camera's picture for a video message. The verbs are on the
        bar. */
    mod.widgets.TelegramAttachPanel = set_type_default() do #(AttachPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        lead_lbl := mod.widgets.SSection {
            visible: false
            margin: Inset{left: 8, bottom: 6}
            text: "CARRIES"
        }
        gone_lbl := mod.widgets.SLabel {
            visible: false
            margin: Inset{left: 8}
            text: "open this from its chat to attach to it", draw_text +: { color: #909090 }
        }
        mod.widgets.TblHairline {}
        empty_lbl := mod.widgets.TblEmpty { visible: false, text: "nothing goes with the next message yet" }
        /* The list gives way while a recording runs: a voice note or a
           video message is a message of its own. */
        list_wrap := View {
            width: Fill, height: Fill
            list := mod.widgets.SList {
                width: Fill, height: Fill
                flow: Down
                reuse_items: true
                row := mod.widgets.TelegramAttachRow {}
            }
        }
        recording := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            spacing: 6
            padding: Inset{left: 8, right: 8, top: 8}
            rec_lbl := mod.widgets.SLabel {
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            }
            keys_lbl := mod.widgets.SLabel {
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                draw_text +: { color: #909090 }
            }
            meter := mod.widgets.MediaMeter {}
            preview := mod.widgets.MediaPicture { width: 160 }
        }
    }

    // ---- the place to send ---------------------------------------------------------------

    /** Where the device says I am, on the map, and the coordinates as a
        selectable run. Sending it — once, or live for a while — is on the
        bar. Reached from the attach panel. */
    mod.widgets.TelegramPlacePanel = set_type_default() do #(PlacePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

        mod.widgets.SLabel {
            text: "where you are, as the device says", draw_text +: { color: #909090 }
        }
        map := mod.widgets.MediaMap {}
        coords_txt := mod.widgets.SText { text: "" }
    }

    // ---- the card --------------------------------------------------------------------

    /** Who or what a chat is with: the name, one line saying what it is,
        the phone number as a selectable run where there is one, and the bio
        or the description under a rule. */
    mod.widgets.TelegramPeerPanel = set_type_default() do #(PeerPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

        name_lbl := mod.widgets.SBoldLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 13.0} }
        }
        kind_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #5a5a5a }
        }
        prompt_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, height: Fit, text: ""
            draw_text +: { color: #a01500 }
        }
        phone_wrap := View {
            visible: false
            width: Fill, height: Fit
            phone_txt := mod.widgets.SText { text: "" }
        }
        mod.widgets.SRule {}
        about_wrap := View {
            visible: false
            width: Fill, height: Fit
            about_txt := mod.widgets.SText { is_multiline: true }
        }
        none_lbl := mod.widgets.SLabel {
            visible: false
            text: "nothing written here", draw_text +: { color: #909090 }
        }
    }

    // ---- signing in ------------------------------------------------------------------

    /** The account's door: the line where the flow stands, the hint a code or
        password step carries, the one field the state asks for — the phone,
        the code, the password — and, once in, the note that the chats are
        coming down. One label, one field, one bar verb, from the shell's own
        kit. */
    mod.widgets.TelegramSigninPanel = set_type_default() do #(SigninPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        feedback := mod.widgets.TelegramFeedback {}
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 8

        line_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 2, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        hint_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
        field_wrap := View {
            visible: false
            width: Fill, height: Fit
            input := mod.widgets.SField {
                empty_text: " "
                return_key_type: ReturnKeyType.Default
            }
        }
        note_lbl := mod.widgets.SLabel {
            visible: false
            width: Fill, max_lines: 2, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
    }
}

/// Telegram's Makepad half.
pub struct Ui;

/// The one in this build.
pub static UI: Ui = Ui;

impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }

    /// Eleven tags, ten templates: the address book and a group's members
    /// draw with one widget, hung twice.
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Attach::TAG => Some(live_id!(telegram_attach_tpl)),
            Chats::TAG => Some(live_id!(telegram_chats_tpl)),
            Chat::TAG => Some(live_id!(telegram_chat_tpl)),
            Messages::TAG => Some(live_id!(telegram_messages_tpl)),
            Contacts::TAG => Some(live_id!(telegram_contacts_tpl)),
            Members::TAG => Some(live_id!(telegram_members_tpl)),
            Peer::TAG => Some(live_id!(telegram_peer_tpl)),
            Line::TAG => Some(live_id!(telegram_line_tpl)),
            Viewer::TAG => Some(live_id!(telegram_media_tpl)),
            Place::TAG => Some(live_id!(telegram_place_tpl)),
            SignIn::TAG => Some(live_id!(telegram_signin_tpl)),
            Topics::TAG => Some(live_id!(telegram_topics_tpl)),
            _ => None,
        }
    }

    /// Telegram's own entries on the panels library's canvas.
    fn scenes(&self) -> Vec<Scene<Setup>> {
        super::scenes::scenes()
    }
}

// ---- the sign-in widget ----------------------------------------------------------
//
// The one telegram widget defined beside its template rather than under
// `widgets/`: a form of one field, small enough that the template and the
// handful of methods that feed it read as one thing.

/// The children the sign-in panel expects in its template.
const SIGNIN_LINE: &[LiveId] = ids!(line_lbl);
const SIGNIN_HINT: &[LiveId] = ids!(hint_lbl);
const SIGNIN_NOTE: &[LiveId] = ids!(note_lbl);
const SIGNIN_FIELD: &[LiveId] = ids!(field_wrap);
const SIGNIN_INPUT: &[LiveId] = ids!(field_wrap.input);

/// The sign-in panel, drawn: the line where the flow stands, the hint a code
/// or password step carries, the one field the state asks for, and the note
/// once signed in. Every change is handed to the instance, so the bar's verb
/// has the text; enter in the field sends, as the verb does. The session row
/// moves under the panel as the worker drives TDLib, so while the flow is
/// still running a redraw is asked for each frame — the way the transcript
/// redraws while a player moves.
#[derive(Script, ScriptHook, Widget)]
pub struct SigninPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The state the field was last seeded for, so a step forward reseeds it —
    /// the phone prefilled, a code or password emptied — while a keystroke
    /// within one step is left alone.
    #[rust]
    seeded: Option<String>,
}

impl Widget for SigninPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        self.reseed(cx, &props);
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let field = self.view.text_input(cx, SIGNIN_INPUT);
            if field.changed(actions).is_some() {
                let text = field.text();
                with_signin(&props, |p| p.edited(text));
            }
            // Enter sends, the bar's own verb.
            if field.returned(actions).is_some() {
                let text = field.text();
                with_signin(&props, |p| p.edited(text));
                if let Some(s) = scope.data.get_mut::<Session>() {
                    let mut borrow = props.panel.borrow_mut();
                    if let Some(p) = borrow.as_any().downcast_mut::<SignIn>() {
                        p.run("telegram.signin", s);
                    }
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((line, hint, note, has_field)) = with_signin(&props, |p| {
            (
                p.line(),
                p.hint().unwrap_or_default(),
                p.note().unwrap_or_default(),
                p.field_kind().is_some(),
            )
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };

        self.view.label(cx, SIGNIN_LINE).set_text(cx, &line);
        let hint_lbl = self.view.label(cx, SIGNIN_HINT);
        hint_lbl.set_text(cx, &hint);
        hint_lbl.set_visible(cx, !hint.is_empty());
        let note_lbl = self.view.label(cx, SIGNIN_NOTE);
        note_lbl.set_text(cx, &note);
        note_lbl.set_visible(cx, !note.is_empty());
        self.view.view(cx, SIGNIN_FIELD).set_visible(cx, has_field);

        let step = self.view.draw_walk(cx, scope, walk);

        // What a script addresses the panel by: the line and the hint as
        // text, and the field, so a caret can be put in it.
        for (label, path) in [(line, SIGNIN_LINE), (hint, SIGNIN_HINT)] {
            if label.is_empty() {
                continue;
            }
            let r = self.view.label(cx, path).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add(label, r, MouseCursor::Default, props.slot);
            }
        }
        if has_field {
            let r = self.view.text_input(cx, SIGNIN_INPUT).area().rect(cx);
            if r.size.x > 0.0 {
                props
                    .hits
                    .add("field".to_string(), r, MouseCursor::Text, props.slot);
            }
        }
        // The worker moves the session row under the panel from its own
        // thread; while a build has one and the flow is not yet done, a redraw
        // each frame picks the next state up promptly — the way the transcript
        // redraws while a player moves. The demo build has no worker, so there
        // is nothing to poll for and no tick.
        #[cfg(feature = "tdlib")]
        if !with_signin(&props, |p| p.state() == "ready").unwrap_or(true) {
            self.view.redraw(cx);
        }
        step
    }
}

impl SigninPanel {
    /// Seeds the field when the state moves on — the phone prefilled, a code
    /// or a password emptied — and puts the caret in it. A keystroke within
    /// one step is left alone: the seed changes only when the step does.
    fn reseed(&mut self, cx: &mut Cx, props: &PanelProps) {
        let field = self.view.text_input(cx, SIGNIN_INPUT);
        if field.area().rect(cx).size.x <= 0.0 {
            return;
        }
        let Some(state) = with_signin(props, |p| p.state()) else {
            return;
        };
        if self.seeded.as_deref() == Some(state.as_str()) {
            return;
        }
        let (seed, focus) =
            with_signin(props, |p| (p.field_seed(), p.field_kind().is_some())).unwrap_or_default();
        field.set_text(cx, &seed);
        with_signin(props, |p| p.edited(seed));
        self.seeded = Some(state);
        if focus {
            field.set_key_focus(cx);
        }
    }
}

/// Runs `f` on the instance, the borrow ending with the call.
fn with_signin<R>(props: &PanelProps, f: impl FnOnce(&mut SignIn) -> R) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    let p = borrow.as_any().downcast_mut::<SignIn>()?;
    Some(f(p))
}
