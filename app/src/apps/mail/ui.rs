//! Mail's Makepad half: its templates, and the tag each of them draws.
//!
//! One `script_mod!` block, its ids all prefixed `mail_`, which is what
//! keeps two apps apart in one script virtual machine. The chassis is the
//! shell's — the table's filter and row twins, the field, the label, the
//! rule — so an app declares what is its own and nothing else: a mailbox
//! row's two lines, a thread row's header and letter, a sheet's three
//! fields.

use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::*;

use crate::shell::app_ui::{AppUi, Setup};

use super::model::Role;
use super::panels::{AddAccount, Card, Compose, Contact, Message, Settings};
use super::widgets::{
    AddAccountPanel, AttachmentPanel, ComposePanel, ContactPanel, HtmlImage, MailboxPanel,
    MessagePanel, SettingsPanel,
};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the mailboxes -----------------------------------------------------

    /** One conversation as a mailbox lists it, two lines: who wrote in it
        and when, then its topic. Bold while anything in it is unread — a
        twin per line rather than a weight, because a label's style is not a
        runtime value.

        The body is declared once and hung in each of the row's four twins,
        so the cursor wash and the mark bar stay the shell's. */
    mod.widgets.MailMailboxBody = View {
        width: Fill, height: Fit
        flow: Down
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            // The participants ride a Fill View whose flow is Down: a Fill
            // label on a Right flow's main axis defer-walks.
            View {
                width: Fill, height: Fit
                flow: Down
                who_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
                who_b := mod.widgets.SBoldLabel {
                    visible: false
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
            }
            View { width: 10, height: 1 }
            date_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        topic_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        topic_b := mod.widgets.SBoldLabel {
            visible: false
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
    }

    /** A row of a mailbox: the four twins, and the hairline under them. */
    mod.widgets.MailMailboxRow = mod.widgets.TblRow {
        line          := mod.widgets.TblLine        { body := mod.widgets.MailMailboxBody {} }
        line_sel      := mod.widgets.TblLineSel     { body := mod.widgets.MailMailboxBody {} }
        line_mark     := mod.widgets.TblLineMark    { body := mod.widgets.MailMailboxBody {} }
        line_mark_sel := mod.widgets.TblLineMarkSel { body := mod.widgets.MailMailboxBody {} }
        mod.widgets.TblHairline {}
    }

    /** A mailbox: the filter over the column heads over the virtualized
        list. A rich table like any other — what mail adds is the row body
        above and the four functions beside it. */
    mod.widgets.MailMailboxPanel = set_type_default() do #(MailboxPanel::register_widget(vm)) {
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
        // Header cells for the columns the first line has; the topic rides
        // the row's second line, owns no column, and so gets no header.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "FROM" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "DATE" }
        }
        mod.widgets.TblHeadRule {}
        empty_lbl := mod.widgets.TblEmpty {}
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            // A row that scrolls out is kept for the next one that scrolls
            // in, rather than built again.
            reuse_items: true
            row := mod.widgets.MailMailboxRow {}
            caption := mod.widgets.TblCaption {}
            band_rule := mod.widgets.TblBandRule {}
        }
        suggest: mod.widgets.TblSuggest {}
    }

    // ---- the HTML reading -----------------------------------------------------

    /** An image in a letter. It fits the column and shows muted alternative
        text until the picture lands, or when it cannot. Its bytes come from
        `widgets::pictures`, never from the frame that draws it. */
    mod.widgets.MailHtmlImage = set_type_default() do #(HtmlImage::register_widget(vm)) {
        width: Fit, height: Fit
        image: mod.widgets.Image { width: Fill, height: Fill }
        draw_text +: {
            text_style: mod.widgets.SMonoStyle{}
            color: #909090
        }
    }

    /** Geist Mono's italic face at the bold weight — the one style the
        chassis does not carry, because an HTML letter is the only thing in
        this build that draws a bold italic. */
    mod.widgets.MailMonoBoldItalicStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: crate_resource("self:resources/geist_mono_italic_variable.ttf") asc: 0.0 desc: 0.0 weight: 700.0}
            fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
            symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
            emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
        }
        font_size: 10.5
        line_spacing: 1.0
    }

    /** An HTML letter in the app's own face and colours. What reaches this
        widget has been through `mail::html`: no fetching element survives,
        link hrefs carry only http, https and mailto, and everything is
        escaped. */
    mod.widgets.MailHtml = Html {
        width: Fill, height: Fit
        padding: 0
        margin: 0
        // A letter is read, so it is selectable.
        selectable: true

        font_size: 10.5
        font_color: #141414
        draw_text +: { color: #141414 }

        text_style_normal: mod.widgets.SMonoStyle{}
        text_style_italic: mod.widgets.SMonoItalicStyle{}
        text_style_bold: mod.widgets.SMonoBoldStyle{}
        text_style_bold_italic: mod.widgets.MailMonoBoldItalicStyle{}
        text_style_fixed: mod.widgets.SMonoStyle{}

        // Different marks, so a nested list is easy to scan.
        ul_markers: ["•", "-"]
        ol_separator: "."

        a := mod.widgets.HtmlLink {
            color: #141414
            pressed_color: #5a5a5a
        }
        img := mod.widgets.MailHtmlImage {}

        // The selection stays above the panel background.
        draw_selection +: {
            draw_call_group: @selection
            color: #00000020
        }

        draw_block +: {
            line_color: #141414
            sep_color: #dcdcdc
            quote_bg_color: #f4f4f4
            quote_fg_color: #141414
            code_color: #f4f4f4
            table_border_color: #dcdcdc
            // A background here would cover the header text; bold already
            // tells a header cell apart.
            table_header_bg_color: #0000
            selection_color: #00000020
        }
    }

    // ---- the reader ---------------------------------------------------------

    /** One message of a conversation: a header row that is the same row open
        or closed — the sender, the date at the right edge — with the letter
        unfolded under it while open. Closed, it previews the first line the
        author wrote, or the status line, in the colour errors get. */
    mod.widgets.MailThreadRow = View {
        width: Fill, height: Fit
        flow: Down
        head := View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            padding: Inset{top: 4, bottom: 4}
            cursor: MouseCursor.Hand
            name_lbl := mod.widgets.SLabel { width: Fit, max_lines: 1, text: "" }
            View { width: 10, height: 1 }
            // The preview rides a Fill View whose flow is Down, for the
            // reason a mailbox row's participants do.
            preview_wrap := View {
                width: Fill, height: Fit
                flow: Down
                preview_lbl := mod.widgets.SLabel {
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #909090 }
                }
                preview_err := mod.widgets.SLabel {
                    visible: false
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #a01500 }
                }
            }
            // Open, the row has no preview to give the width to, so the
            // date is pushed to the right edge by this instead.
            spacer := View { visible: false, width: Fill, height: 1 }
            View { width: 10, height: 1 }
            // Passed on: the `$Forwarded` keyword, drawn as the one mark
            // every other client draws for it. Muted — it is a fact about
            // the letter, not a thing to press.
            fwd_lbl := mod.widgets.SLabel {
                visible: false
                width: Fit, text: "↪", draw_text +: { color: #909090 }
            }
            date_lbl := mod.widgets.SLabel {
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        body := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            spacing: 6
            padding: Inset{top: 2, bottom: 6}
            status_lbl := mod.widgets.SLabel {
                visible: false, text: "", draw_text +: { color: #5a5a5a }
            }
            status_err_lbl := mod.widgets.SLabel {
                visible: false, text: "", draw_text +: { color: #a01500 }
            }
            // A text input carries no `visible` of its own; the box around
            // it is what shows and hides the letter. Multiline, or it lays
            // out on one row and honours neither wrap nor newline.
            text_wrap := View {
                width: Fill, height: Fit
                body_txt := mod.widgets.SText { is_multiline: true }
            }
            /* The other reading. Both are written on every populate — the
               hidden one emptied rather than merely hidden — so no letter
               can leave its text behind for the next one to show. */
            html_wrap := View {
                visible: false
                width: Fill, height: Fit
                body_html := mod.widgets.MailHtml {}
            }
            /* The quoted tail, folded behind one line: in a conversation it
               is the message above. A press unfolds it in place. */
            quote_fold := View {
                visible: false
                width: Fit, height: Fit
                cursor: MouseCursor.Hand
                mod.widgets.SLabel { text: "› quoted", draw_text +: { color: #909090 } }
            }
            quote_wrap := View {
                visible: false
                width: Fill, height: Fit
                quote_txt := mod.widgets.SText {
                    is_multiline: true
                    draw_text +: {
                        color: #5a5a5a
                        color_hover: #5a5a5a
                        color_focus: #5a5a5a
                        color_down: #5a5a5a
                        color_empty: #5a5a5a
                    }
                }
            }
            quote_html := View {
                visible: false
                width: Fill, height: Fit
                quote_body := mod.widgets.MailHtml {
                    font_color: #5a5a5a
                    draw_text +: { color: #5a5a5a }
                }
            }
            /* What the letter carries: one link a part, each opening the
               card over it. Five by name, then a count. */
            atts := View {
                visible: false
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                spacing: 14
                a0 := mod.widgets.SLink {}
                a1 := mod.widgets.SLink {}
                a2 := mod.widgets.SLink {}
                a3 := mod.widgets.SLink {}
                a4 := mod.widgets.SLink {}
                more_lbl := mod.widgets.SLabel {
                    visible: false
                    width: Fit, text: "", draw_text +: { color: #909090 }
                }
            }
        }
        mod.widgets.TblHairline {}
    }

    /** One conversation: the account it came to, once, then every message of
        it, oldest first, open or closed. The two links that answer it are on
        the bar at the foot, with the two buttons that file it. */
    mod.widgets.MailMessagePanel = set_type_default() do #(MessagePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 60, text: "TO" }
            to_lbl := mod.widgets.SLabel {
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                draw_text +: { color: #909090 }
            }
        }
        mod.widgets.TblHairline {}
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            // A finger drags the thread; a mouse button on it is a
            // selection, never a scroll. The list would otherwise take the
            // drag a letter's own text is selected with — and turn a press
            // that lands while a coast is still live into one too, pulling
            // the letter out from under a selection begun a moment after
            // scrolling.
            drag_scrolling: #(cfg!(target_os = "android"))
            msg := mod.widgets.MailThreadRow {}
        }
    }

    // ---- the compose sheet ---------------------------------------------------

    /** The sheet: to and subject over a multiline body, with the line that
        says what the letter will carry between them. Send, discard and —
        while another app is holding something — attach are on the bar at the
        foot, where every verb is. */
    mod.widgets.MailComposePanel = set_type_default() do #(ComposePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 7

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "TO" }
            to_input := mod.widgets.SField {
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "SUBJECT" }
            subject_input := mod.widgets.SField {}
        }
        // What the draft will carry, while it carries anything: one link a
        // file, each opening the card over it — the files app's, when it is
        // in the build, and the missing card when it is not.
        carries := View {
            visible: false
            width: Fill, height: Fit
            align: Align{y: 0.5}
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
        View { width: Fill, height: 2 }
        body_input := mod.widgets.SField {
            width: Fill, height: Fill
            is_multiline: true
            empty_text: ""
            // Multiline: the return key stays a newline.
            return_key_type: ReturnKeyType.Default
        }
        // The TO field's offer, hung under that field and drawn last, so it
        // covers the subject and the body rather than pushing them down.
        suggest: mod.widgets.TblSuggest {}
    }

    // ---- a correspondent's card ---------------------------------------------

    /** One sender: their name, their address, how much they have written,
        and the link to the letters. The link is on the bar as well, where
        every navigation is; this one is in the body because a card is read
        and the link is the thing it is for. */
    mod.widgets.MailContactPanel = set_type_default() do #(ContactPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 8

        View {
            width: Fill, height: Fit
            name_lbl := mod.widgets.SBoldLabel {
                width: Fill
                draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 13.0} }
            }
        }
        email_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #909090 } }
        View { width: Fill, height: 6 }
        count_lbl := mod.widgets.SLabel { text: "" }
        View { width: Fill, height: 6 }
        from_link := mod.widgets.SLink {}
    }

    // ---- the card over a part ------------------------------------------------

    /** One part of a letter, shown: the shell's own card — the same one a
        file browser draws a path with — and under it the line a refused verb
        leaves.

        Everything it draws is filled from the instance through `card::fill`,
        which is what lets one card draw a file on a disk and a part of a
        letter without knowing there is a difference. What tells them apart on
        screen is the selectable line under the three: a path there, a media
        type here. */
    mod.widgets.MailAttachmentPanel = set_type_default() do #(AttachmentPanel::register_widget(vm)) {
        ..mod.widgets.CardFile
        status_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            margin: Inset{top: 6}
            text: "", draw_text +: { color: #a01500 }
        }
    }

    // ---- the accounts --------------------------------------------------------

    /** One account: address and host on a line, the remove button at the
        right edge, and the status line under them.

        The three runs are selectable, not labels: a sync error is the one
        line here a human needs to *act* on — to carry to a search, or to
        paste into a bug report — and it wraps rather than clipping at the
        panel's edge. */
    mod.widgets.MailAccountRow = View {
        width: Fill, height: Fit
        flow: Down
        padding: Inset{top: 6, bottom: 6}
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            email_lbl := mod.widgets.SText { width: Fit, is_multiline: false }
            View { width: 12, height: 1 }
            host_lbl := mod.widgets.SText {
                width: Fit
                is_multiline: false
                draw_text +: {
                    color: #909090
                    color_hover: #909090
                    color_focus: #909090
                    color_down: #909090
                }
            }
            View { width: Fill, height: 1 }
            remove_btn := mod.widgets.SBtn { text: "remove" }
        }
        status_lbl := mod.widgets.SText {
            width: Fill, is_multiline: true
            margin: Inset{top: 6}
            text: ""
            draw_text +: {
                color: #909090
                color_hover: #909090
                color_focus: #909090
                color_down: #909090
                color_empty: #909090
            }
        }
        // A text input carries no `visible` of its own in the DSL; the row
        // stands it down at draw time, where it knows which of the two
        // lines this account's status is.
        status_err_lbl := mod.widgets.SText {
            width: Fill, is_multiline: true
            margin: Inset{top: 6}
            text: ""
            draw_text +: {
                color: #a01500
                color_hover: #a01500
                color_focus: #a01500
                color_down: #a01500
                color_empty: #a01500
            }
        }
        View { width: Fill, height: 8 }
        mod.widgets.TblHairline {}
    }

    /** Mail's settings: the accounts and their sync state. The link to the
        form is on the bar, where this shell keeps every navigation. */
    mod.widgets.MailSettingsPanel = set_type_default() do #(SettingsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        mod.widgets.SSection { text: "ACCOUNTS" }
        mod.widgets.SRule {}
        none_lbl := mod.widgets.SLabel {
            margin: Inset{top: 6}
            text: "no accounts yet", draw_text +: { color: #909090 }
        }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            account_row := mod.widgets.MailAccountRow {}
        }
    }

    /** The add-account form: the Google row above, then four labelled
        fields. *add* and *sign in with google* are on the bar, because a
        button that acts on what the panel shows lives there.

        Google is first because it is one press against four fields — and
        because a Gmail address typed into the form below cannot work at
        all: Google stopped accepting passwords on IMAP. */
    mod.widgets.MailAddAccountPanel = set_type_default() do #(AddAccountPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        // The one line the flow speaks through: what it is waiting for, who
        // signed in, or why it could not. Hidden until it has something to
        // say — an empty line would still take its height. The 82-wide
        // spacer is the same one the section labels are, so the line starts
        // where the fields below it do.
        View {
            width: Fill, height: Fit
            mod.widgets.SSection { width: 82, text: "GOOGLE" }
            google_lbl := mod.widgets.SLabel {
                visible: false
                width: Fill
                text: "", draw_text +: { color: #909090 }
            }
            google_err_lbl := mod.widgets.SLabel {
                visible: false
                width: Fill
                text: "", draw_text +: { color: #a01500 }
            }
        }
        View { width: Fill, height: 10 }
        mod.widgets.SRule {}
        View { width: Fill, height: 10 }

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "ADDRESS" }
            email_input := mod.widgets.SField {
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "PASSWORD" }
            pass_input := mod.widgets.SField {
                is_password: true
                // The placeholder carries the one hint worth keeping (the
                // masking skips empty text — it renders plain).
                empty_text: "app password"
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "IMAP" }
            imap_input := mod.widgets.SField {
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "SMTP" }
            smtp_input := mod.widgets.SField {
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
    }
}

/// Mail's Makepad half.
pub struct Ui;

/// The one in this build.
pub static UI: Ui = Ui;

impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }

    /// Ten tags, seven templates: a mailbox draws the same whichever folder
    /// it is over, so all four of its tags name one widget — hung on the
    /// stage four times, because a template is instantiated per slot and the
    /// four lists are four panels.
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Role::INBOX => Some(live_id!(mail_inbox_tpl)),
            Role::ARCHIVE => Some(live_id!(mail_archive_tpl)),
            Role::SENT => Some(live_id!(mail_sent_tpl)),
            Role::SPAM => Some(live_id!(mail_spam_tpl)),
            Message::TAG => Some(live_id!(mail_message_tpl)),
            Compose::TAG => Some(live_id!(mail_compose_tpl)),
            Contact::TAG => Some(live_id!(mail_contact_tpl)),
            Card::TAG => Some(live_id!(mail_attachment_tpl)),
            Settings::TAG => Some(live_id!(mail_settings_tpl)),
            AddAccount::TAG => Some(live_id!(mail_add_account_tpl)),
            _ => None,
        }
    }

    /// Mail's own entries on the panels library's canvas.
    fn scenes(&self) -> Vec<Scene<Setup>> {
        super::scenes::scenes()
    }
}
