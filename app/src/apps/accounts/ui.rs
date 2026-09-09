use super::{
    panels::{AddAccount, Settings},
    widgets::{AddAccountPanel, SettingsPanel},
};
use crate::shell::{
    app_ui::{AppUi, Setup},
    catalog::panel,
};
use kernel::{panel::Tag, scene::Scene};
use makepad_widgets::*;
script_mod! {
 use mod.prelude.widgets.*
 use mod.widgets.*
    /** One account: address and host on a line, the remove button at the
        right edge, and the status line under them.

        The three runs are selectable, not labels: a sync error is the one
        line here a human needs to *act* on — to carry to a search, or to
        paste into a bug report — and it wraps rather than clipping at the
        panel's edge. */
    mod.widgets.AccountsRow = View {
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
        }
        View {
            width: Fill, height: Fit, flow: Right, spacing: 6
            margin: Inset{top:8}
            mail_btn := mod.widgets.SBtn { text: "Mail" }
            calendar_btn := mod.widgets.SBtn { text: "Calendar" }
            reconnect_btn := mod.widgets.SBtn { text: "reconnect" }
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
    mod.widgets.AccountsPanel = set_type_default() do #(SettingsPanel::register_widget(vm)) {
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
        list := mod.widgets.SList {
            width: Fill, height: Fill
            flow: Down
            account_row := mod.widgets.AccountsRow {}
        }
    }

    /** The add-account form: the Google row above, then four labelled
        fields. *add* and *sign in with google* are on the bar, because a
        button that acts on what the panel shows lives there.

        Google is first because it is one press against four fields — and
        because a Gmail address typed into the form below cannot work at
        all: Google stopped accepting passwords on IMAP. */
    mod.widgets.AccountsAddPanel = set_type_default() do #(AddAccountPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        View { width: Fill, height: Fit, flow: Right, spacing: 6
            mod.widgets.SSection { width: 82, text: "SERVICES" }
            mail_btn := mod.widgets.SBtn { text: "Mail: on" }
            calendar_btn := mod.widgets.SBtn { text: "Calendar: on" }
        }
        View { width: Fill, height: 10 }
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
pub struct Ui;
pub static UI: Ui = Ui;
impl AppUi for Ui {
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue {
        self::script_mod(vm)
    }
    fn template(&self, tag: Tag) -> Option<LiveId> {
        match tag {
            Settings::TAG | Tag("accounts") => Some(live_id!(accounts_tpl)),
            AddAccount::TAG => Some(live_id!(accounts_add_tpl)),
            _ => None,
        }
    }
    fn scenes(&self) -> Vec<Scene<Setup>> {
        vec![Scene::new("accounts", (650.0, 700.0))
            .node("shared accounts", panel(|_| Settings::shared(), ""))
            .node("connect", panel(|_| AddAccount::id(), ""))]
    }
}
