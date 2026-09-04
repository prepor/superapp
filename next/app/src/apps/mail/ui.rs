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
use super::panels::{Compose, Message};
use super::widgets::{ComposePanel, MailboxPanel, MessagePanel};

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
        // What the draft will carry, while it carries anything. Names, not
        // links: the card over a file belongs to the files app, and this
        // build does not list it.
        carries := View {
            visible: false
            width: Fill, height: Fit
            align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "CARRIES" }
            files := View {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                spacing: 14
                f0 := mod.widgets.SLabel { visible: false, width: Fit, text: "" }
                f1 := mod.widgets.SLabel { visible: false, width: Fit, text: "" }
                f2 := mod.widgets.SLabel { visible: false, width: Fit, text: "" }
                f3 := mod.widgets.SLabel { visible: false, width: Fit, text: "" }
                f4 := mod.widgets.SLabel { visible: false, width: Fit, text: "" }
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

    /// Four tags, three templates: a mailbox draws the same whichever folder
    /// it is over, so both of its tags name one widget — hung on the stage
    /// twice, because a template is instantiated per slot and the two lists
    /// are two panels.
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Role::INBOX => Some(live_id!(mail_inbox_tpl)),
            Role::ARCHIVE => Some(live_id!(mail_archive_tpl)),
            Message::TAG => Some(live_id!(mail_message_tpl)),
            Compose::TAG => Some(live_id!(mail_compose_tpl)),
            _ => None,
        }
    }

    /// Mail's own entries on the panels library's canvas.
    fn scenes(&self) -> Vec<Scene<Setup>> {
        super::scenes::scenes()
    }
}
