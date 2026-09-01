//! CR-002: retained panel content. The semantic widget library — makepad
//! primitives wrapped once and themed to the design language — and the
//! per-kind panel widgets composed from it (Robrix's patterns; same
//! script_mod generation).
//!
//! Data flows in per draw via [`PanelProps`] on the scope; intent flows out
//! as [`PanelAction`]s (global actions the shell catches and turns into
//! store actions — so undo semantics never enter this module).

use makepad_widgets::*;

use crate::mail;
use crate::store::Store;

/// What a panel widget may read while drawing: the store and its own
/// panel identity. Passed through `Scope` props each draw (props ride an
/// `Any`, hence the `Rc` — scope wants `'static`).
pub struct PanelProps {
    pub store: std::rc::Rc<Store>,
    pub pid: u64,
}

/// Intent bubbled from panel widgets to the shell. The shell owns turning
/// these into undoable store actions.
#[derive(Debug, Clone)]
pub enum PanelAction {
    AddAccount {
        /// The settings panel that submitted (its form clears on success).
        pid: u64,
        email: String,
        pass: String,
        imap: String,
        smtp: String,
    },
    RemoveAccount(i64),
    /// A compose panel's fields changed — the shell persists the draft
    /// (plain upkeep, not an action).
    DraftEdited {
        pid: u64,
        to: String,
        subject: String,
        body: String,
    },
    /// Open a mail from the inbox (the solid-link semantics; `fresh` is
    /// the workspace modifier).
    OpenMail { pid: u64, id: i64, fresh: bool },
    /// Panel-internal: an inbox row was tapped outside its subject.
    SelectMail { pid: u64, id: i64 },
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the design language, as widget theming ---------------------------
    // INK #141414 · BG #ffffff · TEXT2 #5a5a5a · MUTED #909090
    // RULE #dcdcdc · HOVER #efefef · SEL #e7e7e7 · ERR #a01500

    mod.widgets.SMonoStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: file_resource("/System/Library/Fonts/Menlo.ttc") asc: 0.0 desc: 0.0}
            fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
            symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
        }
        font_size: 8.0
        line_spacing: 1.0
    }

    /** Body text in the mono face. */
    mod.widgets.SLabel = Label {
        width: Fit, height: Fit
        draw_text +: {
            color: #141414
            text_style: mod.widgets.SMonoStyle{}
        }
    }

    /** A tracked uppercase section label (the char grid's Style::Label). */
    mod.widgets.SSection = Label {
        width: Fit, height: Fit
        draw_text +: {
            color: #5a5a5a
            text_style: mod.widgets.SMonoStyle{font_size: 6.6}
        }
    }

    /** The flat monochrome text field: white well, hairline border that
        inks on focus, ink caret, grey selection — the design language over
        makepad's whole TextInput behaviour (click-to-caret, selection,
        IME/soft keyboard). */
    mod.widgets.SField = TextInputFlat {
        width: 260, height: Fit
        padding: Inset{left: 6, right: 6, top: 4, bottom: 4}
        margin: 0
        empty_text: " "
        draw_bg +: {
            border_radius: 1.0
            border_size: 1.0
            color: #ffffff
            color_hover: #ffffff
            color_focus: #ffffff
            color_down: #ffffff
            color_empty: #ffffff
            color_disabled: #ffffff
            border_color: #dcdcdc
            border_color_hover: #909090
            border_color_focus: #141414
            border_color_down: #141414
            border_color_empty: #dcdcdc
            border_color_disabled: #dcdcdc
        }
        draw_text +: {
            color: #141414
            color_hover: #141414
            color_focus: #141414
            color_down: #141414
            color_empty: #909090
            color_empty_hover: #909090
            color_empty_focus: #909090
            color_disabled: #909090
            text_style: mod.widgets.SMonoStyle{}
        }
        draw_cursor +: {
            color: #141414
        }
        draw_selection +: {
            color: #e7e7e7
            color_hover: #e7e7e7
            color_focus: #e7e7e7
            color_down: #e7e7e7
            color_empty: #e7e7e7
        }
    }

    /** The bordered side-effect button (the design language's one button). */
    mod.widgets.SBtn = ButtonFlat {
        width: Fit, height: Fit
        padding: Inset{left: 7, right: 7, top: 3, bottom: 3}
        margin: 0
        draw_bg +: {
            border_radius: 1.0
            border_size: 1.0
            color: #ffffff
            color_hover: #efefef
            color_down: #e7e7e7
            color_focus: #ffffff
            color_disabled: #ffffff
            border_color: #141414
            border_color_hover: #141414
            border_color_down: #141414
            border_color_focus: #141414
            border_color_disabled: #dcdcdc
        }
        draw_text +: {
            color: #141414
            color_hover: #141414
            color_down: #141414
            color_focus: #141414
            color_disabled: #909090
            text_style: mod.widgets.SMonoStyle{font_size: 6.6}
        }
    }

    // ---- settings ----------------------------------------------------------

    /** One account: address + host, status underneath, remove on the right. */
    mod.widgets.AccountRow = set_type_default() do #(AccountRow::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        padding: Inset{bottom: 10}
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            email_lbl := mod.widgets.SLabel { text: "" }
            mod.widgets.SLabel { width: 10, text: "" }
            host_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #909090 } }
            View { width: Fill, height: 1 }
            remove_btn := mod.widgets.SBtn { text: "remove" }
        }
        status_lbl := mod.widgets.SLabel {
            margin: Inset{left: 14, top: 2}
            text: "", draw_text +: { color: #909090 }
        }
        status_err_lbl := mod.widgets.SLabel {
            visible: false
            margin: Inset{left: 14, top: 2}
            text: "", draw_text +: { color: #a01500 }
        }
    }

    /** The settings panel: accounts with live status, the add form. */
    mod.widgets.SettingsPanel = set_type_default() do #(SettingsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 10, right: 10, top: 8, bottom: 8}
        spacing: 6

        mod.widgets.SSection { text: "A C C O U N T S" }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }
        View { width: Fill, height: 4 }

        accounts_list := PortalList {
            // PortalList virtualizes against a fixed viewport — Fit would
            // collapse it. Four rows of air; it scrolls past that.
            width: Fill, height: 150
            flow: Down
            account_row := mod.widgets.AccountRow {}
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "A D D   A C C O U N T" }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }
        View { width: Fill, height: 4 }

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 74, text: "ADDRESS" }
            email_input := mod.widgets.SField {}
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 74, text: "PASSWORD" }
            pass_input := mod.widgets.SField { is_password: true }
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 74, text: "IMAP" }
            imap_input := mod.widgets.SField { text: "imap.fastmail.com" }
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 74, text: "SMTP" }
            smtp_input := mod.widgets.SField { text: "smtp.fastmail.com" }
        }
        View { width: Fill, height: 4 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            hint_lbl := mod.widgets.SLabel {
                text: "an app password — tab walks, enter submits"
                draw_text +: { color: #909090 }
            }
            View { width: Fill, height: 1 }
            add_btn := mod.widgets.SBtn { text: "add account" }
        }
    }

    // ---- compose -----------------------------------------------------------

    /** The compose panel: to/subject fields over a multiline body. Send
        and discard live in the panel chrome, with the other side effects. */
    mod.widgets.ComposePanel = set_type_default() do #(ComposePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 10, right: 10, top: 8, bottom: 8}
        spacing: 6

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 74, text: "TO" }
            to_input := mod.widgets.SField {}
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 74, text: "SUBJECT" }
            subject_input := mod.widgets.SField {}
        }
        View { width: Fill, height: 4 }
        body_input := mod.widgets.SField {
            width: Fill, height: Fill
            is_multiline: true
            empty_text: ""
        }
    }

    // ---- inbox -------------------------------------------------------------

    /** One mail row: from · subject · date, bold while unread, an inverted
        wash while selected. Tap the subject to open; tap elsewhere to
        select (the j/k cursor). */
    mod.widgets.InboxRow = set_type_default() do #(InboxRow::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Overlay
        sel_bg := View {
            visible: false
            width: Fill, height: Fill
            show_bg: true, draw_bg +: { color: #e7e7e7 }
        }
        View {
            width: Fill, height: Fit
            padding: Inset{left: 4, right: 4, top: 3, bottom: 3}
            align: Align{y: 0.5}
            from_lbl := mod.widgets.SLabel { width: 96, text: "" }
            from_lbl_b := mod.widgets.SLabel {
                visible: false, width: 96, text: ""
                draw_text +: { text_style: mod.widgets.SMonoStyle{} }
            }
            subject_lbl := mod.widgets.SLabel { width: Fill, text: "" }
            subject_lbl_b := mod.widgets.SLabel {
                visible: false, width: Fill, text: ""
            }
            date_lbl := mod.widgets.SLabel {
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        rule := View {
            width: Fill, height: Fit, flow: Down
            View { width: Fill, height: 21 }
            View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #dcdcdc } }
        }
    }

    /** The inbox: the filter over the header over the virtualized list. */
    mod.widgets.InboxPanel = set_type_default() do #(InboxPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 10, right: 10, top: 8, bottom: 8}
        spacing: 4

        filter_input := mod.widgets.SField {
            width: Fill
            empty_text: "filter…  ( / )"
        }
        View {
            width: Fill, height: Fit
            padding: Inset{left: 4, right: 4}
            mod.widgets.SSection { width: 96, text: "FROM" }
            mod.widgets.SSection { width: Fill, text: "SUBJECT" }
            mod.widgets.SSection { width: Fit, text: "DATE" }
        }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            row := mod.widgets.InboxRow {}
        }
    }
}

// ---------------------------------------------------------------------------
// AccountRow
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct AccountRow {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    account_id: i64,
}

impl Widget for AccountRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(remove_btn)).clicked(actions) {
                cx.action(PanelAction::RemoveAccount(self.account_id));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl AccountRowRef {
    fn populate(&self, cx: &mut Cx, a: &mail::Account) {
        let Some(mut row) = self.borrow_mut() else {
            return;
        };
        row.account_id = a.id;
        let host = a.imap_host.clone().unwrap_or_default();
        row.view.label(cx, ids!(email_lbl)).set_text(cx, &a.email);
        row.view.label(cx, ids!(host_lbl)).set_text(
            cx,
            if host.is_empty() { "local demo" } else { &host },
        );
        let status = a.status.clone().unwrap_or_else(|| "never synced".into());
        let err = status.starts_with("error");
        let ok_lbl = row.view.label(cx, ids!(status_lbl));
        let err_lbl = row.view.label(cx, ids!(status_err_lbl));
        ok_lbl.set_text(cx, if err { "" } else { &status });
        ok_lbl.set_visible(cx, !err);
        err_lbl.set_text(cx, if err { &status } else { "" });
        err_lbl.set_visible(cx, err);
    }
}

// ---------------------------------------------------------------------------
// SettingsPanel
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct SettingsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl SettingsPanel {
    /// Advance focus the way forms expect: focus + select-all, so typing
    /// replaces and backspace clears.
    pub(crate) fn focus_input(cx: &mut Cx, input: &TextInputRef) {
        input.set_key_focus(cx);
        if let Some(mut t) = input.borrow_mut() {
            t.select_all(cx);
        }
    }

    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 4] {
        [
            self.view.text_input(cx, ids!(email_input)),
            self.view.text_input(cx, ids!(pass_input)),
            self.view.text_input(cx, ids!(imap_input)),
            self.view.text_input(cx, ids!(smtp_input)),
        ]
    }

    fn submit(&mut self, cx: &mut Cx, pid: u64) {
        let [email, pass, imap, smtp] = self.inputs(cx);
        cx.action(PanelAction::AddAccount {
            pid,
            email: email.text().trim().to_string(),
            pass: pass.text(),
            imap: imap.text().trim().to_string(),
            smtp: smtp.text().trim().to_string(),
        });
    }

    /// The form's current values — the e2e bridge submits through the
    /// same PanelAction the button emits.
    pub fn form_values(&mut self, cx: &mut Cx) -> (String, String, String, String) {
        let [email, pass, imap, smtp] = self.inputs(cx);
        (
            email.text().trim().to_string(),
            pass.text(),
            imap.text().trim().to_string(),
            smtp.text().trim().to_string(),
        )
    }

    /// Clears the form after a successful add (the shell calls this).
    pub fn clear_form(&mut self, cx: &mut Cx) {
        let [email, pass, _, _] = self.inputs(cx);
        email.set_text(cx, "");
        pass.set_text(cx, "");
    }
}

impl Widget for SettingsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        // Tab walks the form (shift reverses); TextInput doesn't own Tab.
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let inputs = self.inputs(cx);
                if let Some(i) = inputs.iter().position(|t| t.key_focus(cx)) {
                    let n = inputs.len() as isize;
                    let j = i as isize + if k.modifiers.shift { -1 } else { 1 };
                    if (0..n).contains(&j) {
                        Self::focus_input(cx, &inputs[j as usize]);
                    }
                }
            }
        }

        if let Event::Actions(actions) = event {
            let [email, pass, imap, smtp] = self.inputs(cx);
            // Enter advances; past the last field it submits.
            if email.returned(actions).is_some() {
                Self::focus_input(cx, &pass);
            } else if pass.returned(actions).is_some() {
                Self::focus_input(cx, &imap);
            } else if imap.returned(actions).is_some() {
                Self::focus_input(cx, &smtp);
            } else if smtp.returned(actions).is_some()
                || self.view.button(cx, ids!(add_btn)).clicked(actions)
            {
                let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                self.submit(cx, pid);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let accounts = scope
            .props
            .get::<PanelProps>()
            .map(|p| mail::accounts(&p.store));
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let accounts = accounts.as_deref().map_or(&[][..], |a| &a[..]);
                list.set_item_range(cx, 0, accounts.len());
                while let Some(idx) = list.next_visible_item(cx) {
                    if let Some(a) = accounts.get(idx) {
                        let row = list.item(cx, idx, live_id!(account_row));
                        row.as_account_row().populate(cx, a);
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
    }
}


// ---------------------------------------------------------------------------
// ComposePanel
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct ComposePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl ComposePanel {
    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 3] {
        [
            self.view.text_input(cx, ids!(to_input)),
            self.view.text_input(cx, ids!(subject_input)),
            self.view.text_input(cx, ids!(body_input)),
        ]
    }
}

impl Widget for ComposePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        // Tab walks to → subject → body (shift reverses; the body's enter
        // stays a newline — multiline TextInput owns it).
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let inputs = self.inputs(cx);
                if let Some(i) = inputs.iter().position(|t| t.key_focus(cx)) {
                    let n = inputs.len() as isize;
                    let j = i as isize + if k.modifiers.shift { -1 } else { 1 };
                    if (0..n).contains(&j) {
                        SettingsPanel::focus_input(cx, &inputs[j as usize]);
                    }
                }
            }
        }

        if let Event::Actions(actions) = event {
            let [to, subject, body] = self.inputs(cx);
            if to.returned(actions).is_some() {
                SettingsPanel::focus_input(cx, &subject);
            } else if subject.returned(actions).is_some() {
                SettingsPanel::focus_input(cx, &body);
            }
            if to.changed(actions).is_some()
                || subject.changed(actions).is_some()
                || body.changed(actions).is_some()
            {
                let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                cx.action(PanelAction::DraftEdited {
                    pid,
                    to: to.text(),
                    subject: subject.text(),
                    body: body.text(),
                });
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ComposePanelRef {
    /// Seeds the fields (once, at instantiation) and focuses the body.
    pub fn prefill(&self, cx: &mut Cx, to: &str, subject: &str, body: &str) {
        let Some(inner) = self.borrow() else { return };
        let [to_i, subject_i, body_i] = [
            inner.view.text_input(cx, ids!(to_input)),
            inner.view.text_input(cx, ids!(subject_input)),
            inner.view.text_input(cx, ids!(body_input)),
        ];
        to_i.set_text(cx, to);
        subject_i.set_text(cx, subject);
        body_i.set_text(cx, body);
    }

    /// Focuses the body — deferred to an event tick, because key focus set
    /// during a draw pass does not take.
    pub fn focus_body(&self, cx: &mut Cx) {
        let Some(inner) = self.borrow() else { return };
        inner.view.text_input(cx, ids!(body_input)).set_key_focus(cx);
    }

    /// The current fields as a draft (send reads through this).
    pub fn values(&self, cx: &mut Cx) -> mail::Draft {
        let Some(inner) = self.borrow() else {
            return mail::Draft::default();
        };
        mail::Draft {
            to: inner.view.text_input(cx, ids!(to_input)).text().trim().to_string(),
            subject: inner.view.text_input(cx, ids!(subject_input)).text(),
            body: inner.view.text_input(cx, ids!(body_input)).text(),
        }
    }
}


// ---------------------------------------------------------------------------
// InboxRow
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct InboxRow {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    pid: u64,
    #[rust]
    mail_id: i64,
}

impl Widget for InboxRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        match event.hits(cx, self.view.area()) {
            Hit::FingerUp(fe) if fe.is_over && fe.was_tap() => {
                let subj = self.view.label(cx, ids!(subject_lbl)).area().rect(cx);
                let subj_b = self.view.label(cx, ids!(subject_lbl_b)).area().rect(cx);
                let over_subject = subj.contains(fe.abs) || subj_b.contains(fe.abs);
                if over_subject {
                    cx.action(PanelAction::OpenMail {
                        pid: self.pid,
                        id: self.mail_id,
                        fresh: fe.modifiers.logo || fe.modifiers.alt,
                    });
                } else {
                    cx.action(PanelAction::SelectMail {
                        pid: self.pid,
                        id: self.mail_id,
                    });
                }
            }
            Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl InboxRowRef {
    #[allow(clippy::too_many_arguments)]
    fn populate(
        &self,
        cx: &mut Cx,
        pid: u64,
        m: &mail::MailHead,
        selected: bool,
    ) {
        let Some(mut row) = self.borrow_mut() else { return };
        row.pid = pid;
        row.mail_id = m.id;
        let from = &m.from_name;
        let date = mail::fmt_date(m.date);
        let (fp, fb) = (
            row.view.label(cx, ids!(from_lbl)),
            row.view.label(cx, ids!(from_lbl_b)),
        );
        let (sp, sb) = (
            row.view.label(cx, ids!(subject_lbl)),
            row.view.label(cx, ids!(subject_lbl_b)),
        );
        fp.set_text(cx, if m.unread { "" } else { from });
        fb.set_text(cx, if m.unread { from } else { "" });
        fp.set_visible(cx, !m.unread);
        fb.set_visible(cx, m.unread);
        sp.set_text(cx, if m.unread { "" } else { &m.subject });
        sb.set_text(cx, if m.unread { &m.subject } else { "" });
        sp.set_visible(cx, !m.unread);
        sb.set_visible(cx, m.unread);
        row.view.label(cx, ids!(date_lbl)).set_text(cx, &date);
        row.view.view(cx, ids!(sel_bg)).set_visible(cx, selected);
    }
}

// ---------------------------------------------------------------------------
// InboxPanel
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct InboxPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    sel: Option<i64>,
}

impl InboxPanel {
    fn rows(&self, cx: &mut Cx, scope: &Scope) -> Vec<mail::MailHead> {
        let filter = self.view.text_input(cx, ids!(filter_input)).text();
        scope
            .props
            .get::<PanelProps>()
            .map(|p| mail::inbox_filtered(&p.store, &filter))
            .unwrap_or_default()
    }

    fn move_sel(&mut self, cx: &mut Cx, scope: &Scope, d: isize) {
        let rows = self.rows(cx, scope);
        if rows.is_empty() {
            return;
        }
        let i = self
            .sel
            .and_then(|s| rows.iter().position(|m| m.id == s))
            .map_or(0, |i| {
                (i as isize + d).clamp(0, rows.len() as isize - 1) as usize
            });
        self.sel = Some(rows[i].id);
        self.redraw(cx);
    }
}

impl Widget for InboxPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let filter = self.view.text_input(cx, ids!(filter_input));
        let filter_focused = filter.key_focus(cx);
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);

        // The panel's letter grammar, only while the filter is at rest —
        // letters arrive as TextInput events, exactly like real typing.
        if let Event::TextInput(t) = event {
            if !filter_focused {
                match t.input.as_str() {
                    "j" => self.move_sel(cx, scope, 1),
                    "k" => self.move_sel(cx, scope, -1),
                    "/" => SettingsPanel::focus_input(cx, &filter),
                    _ => {}
                }
            }
        }
        if let Event::KeyDown(k) = event {
            if !filter_focused && k.key_code == KeyCode::ReturnKey {
                let rows = self.rows(cx, scope);
                let target = self
                    .sel
                    .filter(|s| rows.iter().any(|m| m.id == *s))
                    .or_else(|| rows.first().map(|m| m.id));
                if let Some(id) = target {
                    cx.action(PanelAction::OpenMail {
                        pid,
                        id,
                        fresh: k.modifiers.logo || k.modifiers.alt,
                    });
                }
            }
        }
        if let Event::Actions(actions) = event {
            // Enter in the filter: select the first visible row and rest.
            if filter.returned(actions).is_some() || filter.escaped(actions) {
                cx.set_key_focus(Area::Empty);
                if filter.returned(actions).is_some() {
                    self.sel = self.rows(cx, scope).first().map(|m| m.id);
                }
                self.redraw(cx);
            }
            if filter.changed(actions).is_some() {
                self.sel = None;
                self.redraw(cx);
            }
            for a in actions {
                if let Some(PanelAction::SelectMail { pid: p, id }) =
                    a.downcast_ref::<PanelAction>()
                {
                    if *p == pid {
                        self.sel = Some(*id);
                        self.redraw(cx);
                    }
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rows = self.rows(cx, scope);
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
        let sel = self.sel;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, rows.len());
                while let Some(idx) = list.next_visible_item(cx) {
                    if let Some(m) = rows.get(idx) {
                        let row = list.item(cx, idx, live_id!(row));
                        row.as_inbox_row().populate(cx, pid, m, sel == Some(m.id));
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
    }
}
