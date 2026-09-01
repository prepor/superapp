//! CR-002: retained panel content. The semantic widget library — makepad
//! primitives wrapped once and themed to the design language — and the
//! per-kind panel widgets composed from it (Robrix's patterns; same
//! script_mod generation).
//!
//! Data flows in per draw via [`PanelProps`] on the scope; intent flows out
//! as [`PanelAction`]s (global actions the shell catches and turns into
//! store actions — so undo semantics never enter this module).

use makepad_widgets::makepad_platform::event::{ScrollEvent, ScrollPhase};
use makepad_widgets::*;

use crate::mail;
use crate::store::Store;
use crate::ui;

/// What a panel widget may read while drawing: the store and its own
/// panel identity. Passed through `Scope` props each draw (props ride an
/// `Any`, hence the `Rc` — scope wants `'static`).
pub struct PanelProps {
    pub store: std::rc::Rc<Store>,
    pub pid: u64,
    pub kind: crate::core::Kind,
}

/// One row of a modal overlay, already reduced to what it draws. The shell
/// assembles these per draw — overlays read no store of their own.
#[derive(Clone, Default)]
pub struct OverlayRowData {
    /// The big left-hand number (workspaces) — empty elsewhere.
    pub num: String,
    /// The row's subject: a workspace summary, an action label, a hit.
    pub main: String,
    /// Dimmed trailing text on the same line (a launcher hit's detail).
    pub detail: String,
    /// Right-aligned: a date, a workspace badge.
    pub right: String,
    /// Inverted: the current workspace, the selected hit, the DAG's head.
    pub current: bool,
    /// Undone history branches draw muted but stay walkable.
    pub muted: bool,
}

/// What an overlay widget draws. Assembled by the shell each frame from the
/// workspace roster, the undo DAG, or the launcher's live search.
#[derive(Clone, Default)]
pub struct OverlayProps {
    pub rows: Vec<OverlayRowData>,
    /// The launcher's query, pushed into the field when the overlay opens.
    pub query: String,
}

/// Intent bubbled from panel widgets to the shell. The shell owns turning
/// these into undoable store actions.
#[derive(Debug, Clone)]
pub enum PanelAction {
    AddAccount {
        /// The add-account panel that submitted (its form clears on success).
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
    /// A link was followed: solid opens joined, dotted replaces in place,
    /// `fresh` (the workspace modifier) always opens un-joined.
    FollowLink {
        pid: u64,
        target: crate::core::Kind,
        dotted: bool,
        fresh: bool,
    },
    /// Help's demo button: the one side effect that does nothing, so the
    /// legend can show what a button is without moving anything.
    TryIt { pid: u64 },
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the design language, as widget theming ---------------------------
    // INK #141414 · BG #ffffff · TEXT2 #5a5a5a · MUTED #909090
    // RULE #dcdcdc · HOVER #efefef · SEL #e7e7e7 · ERR #a01500

    // Sizes mirror theme.rs: FONT_SIZE 10.5 body, LABEL_SIZE 8.25 labels —
    // the same numbers the char-grid renderer draws with, so migrated and
    // unmigrated panels read as one app.
    // The one face, bundled rather than borrowed. Menlo fronted the family
    // until HTML mail asked for a weight it does not have: Menlo.ttc yields
    // only its regular face, so `<b>` drew as body text. It was also macOS
    // furniture — the Fold never had it and fell through to Liberation, so
    // "the app's face" was already two faces depending on where you read.
    //
    // JetBrains Mono is variable on `wght` (100–800), ships inside
    // makepad_widgets (no new asset, OFL), and is 0.600 em wide against
    // Menlo's 0.6021 — the character grid moves by a third of a percent.
    // Its ratio is Liberation's to four places, so this is the face the
    // Fold has been drawing all along; macOS is the side that changes.
    //
    // What it still cannot do is italic: makepad's `FontMember` exposes
    // only `weight`, with no slant or skew anywhere in `TextStyle`, so an
    // oblique needs a second file on disk whatever family we pick.
    mod.widgets.SMonoStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf") asc: 0.0 desc: 0.0}
            fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
            symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
            emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
        }
        font_size: 10.5
        line_spacing: 1.0
    }

    /** The same face leaning on its weight axis — real bold, not the two
        nudged passes `SBold` draws (which stay, for now: they are what the
        char grid's headers and the accelerator marks are built from). */
    mod.widgets.SMonoBoldStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: crate_resource("makepad_widgets:resources/jetbrains_mono_variable.ttf") asc: 0.0 desc: 0.0 weight: 700.0}
            fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
            symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
            emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
        }
        font_size: 10.5
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

    /** An uppercase section label (the char grid's Style::Label). */
    mod.widgets.SSection = Label {
        width: Fit, height: Fit
        draw_text +: {
            color: #5a5a5a
            text_style: mod.widgets.SMonoStyle{font_size: 8.25}
        }
    }

    /** Fake-bold text, the char grid's way: the same run twice, the twin
        nudged 0.4 px — Menlo ships no variable weight to ask for. */
    mod.widgets.SBold = set_type_default() do #(SBold::register_widget(vm)) {
        ..mod.widgets.View
        width: Fit, height: Fit
        flow: Overlay
        a := mod.widgets.SLabel { width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
        b := mod.widgets.SLabel { width: Fill, margin: Inset{left: 0.4}, max_lines: 1, text_overflow: TextOverflow.Ellipsis }
    }

    /** The flat monochrome text field: white well, hairline border that
        inks on focus, ink caret, grey selection — the design language over
        makepad's whole TextInput behaviour (click-to-caret, selection,
        IME/soft keyboard). */
    mod.widgets.SField = TextInputFlat {
        width: Fill, height: Fit
        padding: Inset{left: 7, right: 7, top: 5, bottom: 5}
        margin: 0
        empty_text: " "
        // Forms advance: the soft keyboard's action key reads "next" and
        // lands as the same Returned action the enter-chain walks. Fields
        // that end a chain override with Done/Search.
        return_key_type: ReturnKeyType.Next
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
            // The selection quad paints OVER the glyphs and the state-mix
            // does not engage reliably — so one translucent ink on every
            // state: text reads through, and "no selection when blurred"
            // is enforced by collapsing the selection on focus-lost
            // instead of by colour.
            color: #00000020
            color_hover: #00000020
            color_focus: #00000020
            color_down: #00000020
            color_empty: #00000000
        }
    }

    /** Selectable body text (CR-003): makepad's TextInput held read-only and
        stripped of every field affordance — no well, no border, no caret —
        so it reads exactly as an `SLabel` but can be dragged over,
        double-clicked and copied. Editing is impossible, and a read-only
        input gates off `Hit::TextInput`, so it cannot swallow a panel's
        letters; copy still arrives as a platform TextCopy hit. */
    mod.widgets.SText = TextInputFlat {
        width: Fill, height: Fit
        padding: 0
        margin: 0
        empty_text: ""
        is_read_only: true
        draw_bg +: {
            border_size: 0.0
            color: #00000000
            color_hover: #00000000
            color_focus: #00000000
            color_down: #00000000
            color_empty: #00000000
            color_disabled: #00000000
            border_color: #00000000
            border_color_hover: #00000000
            border_color_focus: #00000000
            border_color_down: #00000000
            border_color_empty: #00000000
            border_color_disabled: #00000000
        }
        draw_text +: {
            color: #141414
            color_hover: #141414
            color_focus: #141414
            color_down: #141414
            color_empty: #141414
            color_empty_hover: #141414
            color_empty_focus: #141414
            color_disabled: #141414
            text_style: mod.widgets.SMonoStyle{}
        }
        // Nothing is being typed, so the caret never shows.
        draw_cursor +: { color: #00000000 }
        draw_selection +: {
            color: #00000020
            color_hover: #00000020
            color_focus: #00000020
            color_down: #00000020
            color_empty: #00000000
        }
    }

    /** An HTML mail body, in the app's one face.

        makepad's `Html` draws a semantic vocabulary and no CSS, which is
        the whole reason it suits this app: a sender's brand colours never
        arrive to fight the monochrome, because there is no mechanism by
        which they could. What arrives is structure — lists, quotes,
        emphasis, links — drawn in Menlo at body size like everything else.
        [`crate::html`] narrows the letter to this vocabulary first.

        Links need no colour: `HtmlLink` underlines, and in this app the
        underline *is* the link (CR-003's grammar), so they read correctly
        in plain #141414.

        `<b>` is real weight now that the family is variable (see
        `SMonoStyle`). `<i>` is not: makepad exposes no slant, so italic
        would need a second file on disk, and until one is there it reads
        as body text. */
    mod.widgets.SHtml = Html {
        width: Fill, height: Fit
        padding: 0
        margin: 0
        // The body is prose, and CR-003 made prose selectable.
        selectable: true

        font_size: 10.5
        font_color: #141414
        draw_text +: { color: #141414 }

        text_style_normal: mod.widgets.SMonoStyle{}
        text_style_italic: mod.widgets.SMonoStyle{}
        text_style_bold: mod.widgets.SMonoBoldStyle{}
        text_style_bold_italic: mod.widgets.SMonoBoldStyle{}
        text_style_fixed: mod.widgets.SMonoStyle{}

        // `-` for the nested level: `•` comes from the symbol fallback,
        // whose advance is not the mono cell, and two of them stacked read
        // as a smudge rather than a hierarchy.
        ul_markers: ["•", "-"]
        ol_separator: "."

        a := mod.widgets.HtmlLink {
            color: #141414
            pressed_color: #5a5a5a
        }

        // The wash SText already wears. `Html` is its own widget type, not
        // a `TextFlow` derivation, so it inherits none of `TextFlowBase`'s
        // theming — including `draw_call_group`, without which the quad
        // merges into the call that paints under the panel background and
        // the selection never appears (CR-002's sixth defect, again).
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
            table_header_bg_color: #f4f4f4
            selection_color: #00000020
        }
    }

    /** The bordered side-effect button (the design language's one button). */
    mod.widgets.SBtn = ButtonFlat {
        width: Fit, height: Fit
        padding: Inset{left: 12, right: 12, top: 5, bottom: 5}
        margin: 0
        draw_bg +: {
            border_radius: 1.0
            border_size: 1.0
            color: #ffffff
            color_hover: #efefef
            color_down: #e7e7e7
            // Keyboard focus reads as the selection wash — enter/space
            // will press this button.
            color_focus: #e7e7e7
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
            text_style: mod.widgets.SMonoStyle{font_size: 8.25}
        }
    }

    /** The link grammar as a widget: label over a 1 px underline — solid
        opens joined, dotted replaces in place (the dashes are shader-drawn). */
    mod.widgets.SLink = set_type_default() do #(SLink::register_widget(vm)) {
        ..mod.widgets.View
        width: Fit, height: Fit
        flow: Down
        cursor: MouseCursor.Hand
        // The label is split so one character can carry the accelerator
        // mark (CR-003): prefix, the key drawn twice, suffix. Splitting
        // beats padding a twin with spaces — `←` arrives from the symbol
        // fallback, whose advance is not the mono cell.
        // Label's base padding is mspace_1 — invisible around a single run,
        // but it would open a gap between each of the three, so the split
        // parts zero it and the row carries the word's own spacing.
        row := View {
            width: Fit, height: Fit
            flow: Right
            pre := mod.widgets.SLabel { padding: 0, text: "" }
            // Three passes, not the usual two: one nudge is legible in a
            // run of body text but disappears in a short label, and the
            // mark has to read at a glance to be worth anything.
            key := View {
                width: Fit, height: Fit
                flow: Overlay
                k1 := mod.widgets.SLabel { padding: 0, text: "" }
                k2 := mod.widgets.SLabel { padding: 0, margin: Inset{left: 0.35}, text: "" }
                k3 := mod.widgets.SLabel { padding: 0, margin: Inset{left: 0.7}, text: "" }
            }
            post := mod.widgets.SLabel { padding: 0, text: "" }
        }
        // The solid underline needs its own `pixel` for the same reason the
        // row wash does (CR-002's sixth defect): a stock-shader quad merges
        // into a draw call that paints *under* the panel background, so it
        // never appears. A distinct shader earns a correctly-ordered call.
        ul := View {
            width: Fill, height: 1
            show_bg: true
            draw_bg +: {
                color: #141414
                pixel: fn() {
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
            }
        }
        ul_dotted := View {
            visible: false
            width: Fill, height: 1
            show_bg: true
            draw_bg +: {
                color: #141414
                // `Math` carries only rotate_2d/random_2d on this pin, so
                // the period comes from fract, not a mod that never
                // compiled (and so never dashed anything).
                pixel: fn() {
                    let x = self.pos.x * self.rect_size.x
                    if fract(x / 6.0) > 0.5 {
                        return vec4(0.0, 0.0, 0.0, 0.0)
                    }
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
            }
        }
    }

    /** A key cap: the char grid's `Seg::Kbd` as a widget — a hairline box
        around the key's name, sized to it. Built on ButtonFlat because it
        carries `text` on the instance (a named child's properties cannot be
        overridden per instance at this makepad generation: the override
        parses and is silently dropped). It is inert by construction — no
        action reads it, and it never takes key focus. */
    mod.widgets.SKbd = ButtonFlat {
        width: Fit, height: Fit
        margin: Inset{left: 1, right: 1}
        padding: Inset{left: 4, right: 4, top: 1, bottom: 1}
        grab_key_focus: false
        draw_bg +: {
            border_radius: 1.0
            border_size: 1.0
            color: #ffffff
            color_hover: #ffffff
            color_down: #ffffff
            color_focus: #ffffff
            color_disabled: #ffffff
            border_color: #5a5a5a
            border_color_hover: #5a5a5a
            border_color_down: #5a5a5a
            border_color_focus: #5a5a5a
            border_color_disabled: #5a5a5a
        }
        draw_text +: {
            color: #5a5a5a
            color_hover: #5a5a5a
            color_down: #5a5a5a
            color_focus: #5a5a5a
            color_disabled: #5a5a5a
            text_style: mod.widgets.SMonoStyle{font_size: 8.25}
        }
    }

    /** One line of prose: children laid out left to right, shared baseline. */
    mod.widgets.SRow = View {
        width: Fill, height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{top: 1, bottom: 1}
    }

    /** The hairline under a section label. */
    mod.widgets.SRule = View {
        width: Fill, height: 1
        margin: Inset{top: 3, bottom: 5}
        show_bg: true
        draw_bg +: {
            color: #141414
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    // ---- settings ----------------------------------------------------------

    /** One account: address + host, status underneath, remove on the right. */
    mod.widgets.AccountRow = set_type_default() do #(AccountRow::register_widget(vm)) {
        ..mod.widgets.View
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
        status_lbl := mod.widgets.SLabel {
            margin: Inset{left: 17, top: 3}
            text: "", draw_text +: { color: #909090 }
        }
        status_err_lbl := mod.widgets.SLabel {
            visible: false
            margin: Inset{left: 17, top: 3}
            text: "", draw_text +: { color: #a01500 }
        }
        View { width: Fill, height: 8 }
        View {
            width: Fill, height: 1
            show_bg: true
            draw_bg +: {
                color: #dcdcdc
                // Distinct shader ⇒ distinct draw call: portal-item quads
                // on the stock shader paint under the panel background.
                pixel: fn() {
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
            }
        }
    }

    /** The settings panel: the accounts and their sync state, then the link
        to the form (solid: the add-account panel opens joined to the right). */
    mod.widgets.SettingsPanel = set_type_default() do #(SettingsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        mod.widgets.SSection { text: "ACCOUNTS" }
        View { width: Fill, height: 5 }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }

        accounts_list := PortalList {
            // PortalList virtualizes against a fixed viewport (Fit would
            // collapse it) — so it fills the panel above the link.
            width: Fill, height: Fill
            flow: Down
            account_row := mod.widgets.AccountRow {}
        }

        // The link belongs to the content, not to the section label: a
        // heading row is not where this language puts navigation.
        View { width: Fill, height: 8 }
        add_link := mod.widgets.SLink {}
    }

    /** The add-account form, a panel of its own: four labelled fields and
        the one button, top-aligned in a compact panel. */
    mod.widgets.AddAccountPanel = set_type_default() do #(AddAccountPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "ADDRESS" }
            email_input := mod.widgets.SField {
                content_type: TextInputContentType.EmailAddress
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
                text: "imap.fastmail.com"
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "SMTP" }
            smtp_input := mod.widgets.SField {
                text: "smtp.fastmail.com"
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 12 }
        View {
            width: Fill, height: Fit
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
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 7

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "TO" }
            to_input := mod.widgets.SField {
                content_type: TextInputContentType.EmailAddress
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "SUBJECT" }
            subject_input := mod.widgets.SField {}
        }
        View { width: Fill, height: 2 }
        body_input := mod.widgets.SField {
            width: Fill, height: Fill
            is_multiline: true
            empty_text: ""
            // Multiline: the keyboard's return stays a newline.
            return_key_type: ReturnKeyType.Default
        }
    }

    // ---- inbox -------------------------------------------------------------

    /** One mail row, two lines: the columns line (from · date), then the
        subject alone on the richtable's *extra line* — full-width row
        content that belongs to no column. Bold while unread, an inverted
        wash while selected, every run one line, ellipsized. The row is one
        target: a click anywhere on either line opens the mail and leaves
        the j/k cursor there. */
    // No Overlay anywhere in a row, deliberately: quads under an Overlay
    // ancestor inside a PortalList item never paint (Fill defers, and a
    // deferred overlay walk never resolves) — so the selection wash is a
    // twin line with its own bg, toggled like the bold pairs.
    mod.widgets.InboxLine = set_type_default() do #(InboxLine::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        padding: Inset{left: 4, right: 4, top: 5, bottom: 5}
        spacing: 2
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            // A Fill *Label* on a Right flow's main axis defer-walks and
            // drops its theme padding (upstream Label ships mspace_1),
            // landing 3 px left of every normally-walked label. So the
            // from twins ride a Fill View whose flow is Down: there Fill
            // width is the cross axis — no defer, padding kept, and the
            // left edge matches the subject line (the same construction).
            View {
                width: Fill, height: Fit
                flow: Down
                from_lbl := mod.widgets.SLabel {
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
                from_b := mod.widgets.SBold { visible: false, width: Fill }
            }
            View { width: 10, height: 1 }
            date_lbl := mod.widgets.SLabel {
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        subject_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        subject_b := mod.widgets.SBold { visible: false, width: Fill }
    }

    mod.widgets.InboxRow = set_type_default() do #(InboxRow::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        line := mod.widgets.InboxLine {}
        line_sel := mod.widgets.InboxLine {
            visible: false
            show_bg: true
            draw_bg +: {
                color: #e7e7e7
                // A custom pixel fn forces a distinct shader and so a
                // distinct draw call: portal-item quads on the stock
                // shader merge into a call that paints under the panel
                // background — invisible.
                pixel: fn() {
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
            }
        }
        View {
            width: Fill, height: 1
            show_bg: true
            draw_bg +: {
                color: #dcdcdc
                pixel: fn() {
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
            }
        }
    }

    /** The inbox: the filter over the header over the virtualized list. */
    mod.widgets.InboxPanel = set_type_default() do #(InboxPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 8}
        spacing: 6

        filter_input := mod.widgets.SField {
            width: Fill
            empty_text: "filter…  ( / )"
            return_key_type: ReturnKeyType.Search
            autocapitalize: AutoCapitalize.None
            autocorrect: AutoCorrect.Disabled
        }
        // Header cells for the columns only — the subject rides each row's
        // extra line, owns no column, and so gets no header. FROM sits in
        // a Fill View, not at Fill itself: a deferred Fill label drops its
        // theme padding and drifts off the rows' shared left edge.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 4, right: 4}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { text: "FROM" }
            }
            mod.widgets.SSection { width: Fit, text: "DATE" }
        }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            row := mod.widgets.InboxRow {}
        }
    }

    // ---- the read panels ---------------------------------------------------

    /** One mail, whole: headers with a contact link, the body, the walk. */
    mod.widgets.MessagePanel = set_type_default() do #(MessagePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 60, text: "FROM" }
            from_link := mod.widgets.SLink {}
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 60, text: "TO" }
            to_lbl := mod.widgets.SText {
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
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 60, text: "DATE" }
            date_lbl := mod.widgets.SText { is_multiline: false }
        }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #dcdcdc } }
        status_lbl := mod.widgets.SLabel {
            visible: false, text: "", draw_text +: { color: #5a5a5a }
        }
        status_err_lbl := mod.widgets.SLabel {
            visible: false, text: "", draw_text +: { color: #a01500 }
        }
        // Two readings of one letter; the panel shows whichever the mail
        // actually carries, never both. Each sits in its own View because
        // that is the widget that honours `visible` — neither `Html` nor
        // `TextInput` carries one, so hiding either directly is a silent
        // no-op, and the previous mail reads on under the next.
        body_scroll := View {
            width: Fill, height: Fill
            flow: Down
            scroll_bars: ScrollBars{ show_scroll_x: false }
            text_wrap := View {
                width: Fill, height: Fit
                body_lbl := mod.widgets.SText { is_multiline: true }
            }
            html_wrap := View {
                width: Fill, height: Fit
                visible: false
                body_html := mod.widgets.SHtml {}
            }
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            newer_link := mod.widgets.SLink {}
            newer_off := mod.widgets.SLabel {
                visible: false, text: "← newer", draw_text +: { color: #909090 }
            }
            mod.widgets.SLabel { width: 16, text: "" }
            older_link := mod.widgets.SLink {}
            older_off := mod.widgets.SLabel {
                visible: false, text: "older →", draw_text +: { color: #909090 }
            }
            View { width: Fill, height: 1 }
            reply_link := mod.widgets.SLink {}
        }
    }

    /** A sender's card. */
    mod.widgets.ContactPanel = set_type_default() do #(ContactPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

        View {
            width: Fill, height: Fit, flow: Overlay
            name_lbl := mod.widgets.SLabel {
                width: Fill
                draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 13.0} }
            }
            name_lbl2 := mod.widgets.SLabel {
                width: Fill, margin: Inset{left: 0.4}
                draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 13.0} }
            }
        }
        email_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #909090 } }
        View { width: Fill, height: 6 }
        count_lbl := mod.widgets.SLabel { text: "" }
        View { width: Fill, height: 6 }
        from_link := mod.widgets.SLink {}
    }

    // ---- help and about ----------------------------------------------------

    /** The manual, and the design language's own showcase: every grammar it
        describes is drawn with the widget that implements it — the links
        really open and replace, the button really fires a side effect, the
        key caps are the same `SKbd` the rest of the app would use.

        Platform-specific rows are all here and hidden per target in
        `HelpPanel::draw_walk` (the DSL cannot see `cfg!`). */
    mod.widgets.HelpPanel = set_type_default() do #(HelpPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        scroll_bars: ScrollBars{ show_scroll_x: false }

        mod.widgets.SSection { text: "LEGEND" }
        mod.widgets.SRule {}
        mod.widgets.SRow {
            solid_link := mod.widgets.SLink {}
            mod.widgets.SLabel { text: " — opens a panel to the right, joined" }
        }
        mod.widgets.SRow {
            dotted_link := mod.widgets.SLink {}
            mod.widgets.SLabel { text: " — replaces this panel in place" }
        }
        mod.widgets.SRow {
            try_btn := mod.widgets.SBtn { text: "button" }
            mod.widgets.SLabel { text: " — side effect only, never navigation" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SLabel { text: "+click / " }
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "enter" }
            mod.widgets.SLabel { text: " — always a fresh, un-joined panel" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "a ═ bridge marks a joined pair: the next solid link in the parent replaces the joined panel; replacing a panel closes its joined chain" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "color is reserved for errors: " }
            mod.widgets.SLabel { text: "like this", draw_text +: { color: #a01500 } }
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "KEYS" }
        mod.widgets.SRule {}
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SLabel { text: "+arrows — focus panels" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SLabel { text: "+same — move the panel" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "w" }
            mod.widgets.SLabel { text: " — close the focused panel" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "z" }
            mod.widgets.SLabel { text: " — undo (open, close, move, archive…)" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SKbd { text: "z" }
            mod.widgets.SLabel { text: " — redo" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "u" }
            mod.widgets.SLabel { text: " — history: the whole tree, walkable" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "i" }
            mod.widgets.SLabel { text: " — copy the panel's context (its queries)" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "[" }
            mod.widgets.SKbd { text: "]" }
            mod.widgets.SLabel { text: " — consume into / expel out of a column" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "," }
            mod.widgets.SKbd { text: "." }
            mod.widgets.SLabel { text: " — pull from the right / push bottom out" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "t" }
            mod.widgets.SLabel { text: " — column tabs (click a tab or cmd+↑/↓)" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "a control wearing a bold letter is cmd+that letter:" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "  message " }
            mod.widgets.SKbd { text: "cmd+a" }
            mod.widgets.SLabel { text: "rchive " }
            mod.widgets.SKbd { text: "cmd+r" }
            mod.widgets.SLabel { text: "eply " }
            mod.widgets.SKbd { text: "cmd+n" }
            mod.widgets.SLabel { text: "/" }
            mod.widgets.SKbd { text: "cmd+o" }
            mod.widgets.SLabel { text: " walk" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "  inbox " }
            mod.widgets.SKbd { text: "cmd+r" }
            mod.widgets.SLabel { text: "efresh  " }
            mod.widgets.SKbd { text: "enter" }
            mod.widgets.SLabel { text: " opens  " }
            mod.widgets.SKbd { text: "/" }
            mod.widgets.SLabel { text: " filters  arrows walk the rows" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "esc" }
            mod.widgets.SLabel { text: " leaves a text field" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "trackpad: scroll the strip and the panels" }
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "WORKSPACES" }
        mod.widgets.SRule {}
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "1" }
            mod.widgets.SLabel { text: "…" }
            mod.widgets.SKbd { text: "9" }
            mod.widgets.SLabel { text: " — switch workspace" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "cmd" }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SLabel { text: "+№ — move the panel there" }
        }
        menu_row := mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "the menu bar lists them; [n] is current" }
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "LAUNCHER" }
        mod.widgets.SRule {}
        desk_launch := View {
            width: Fill, height: Fit, flow: Down
            mod.widgets.SRow {
                mod.widgets.SKbd { text: "cmd" }
                mod.widgets.SKbd { text: "cmd" }
                mod.widgets.SLabel { text: " — the launcher: search everything" }
            }
            mod.widgets.SRow {
                mod.widgets.SLabel { width: Fill, text: "type to find panels, mail, people; enter goes to it — or opens it fresh" }
            }
        }
        touch_launch := View {
            visible: false
            width: Fill, height: Fit, flow: Down
            mod.widgets.SRow {
                mod.widgets.SLabel { width: Fill, text: "the overlay's search row opens it: find open panels, mail, people" }
            }
        }

        touch_help := View {
            visible: false
            width: Fill, height: Fit, flow: Down
            View { width: Fill, height: 10 }
            mod.widgets.SSection { text: "TOUCH" }
            mod.widgets.SRule {}
            mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "tap — follow links, press buttons" } }
            mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "drag — scroll a panel's content" } }
            mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "two fingers — scroll the workspace" } }
            mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "two fingers down — workspaces overlay" } }
            mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "hold a header — pick the panel up; drop on a column to stack, between columns for a fresh one" } }
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "TRY" }
        mod.widgets.SRule {}
        mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "1. click a subject — a message opens, joined (bridge)" } }
        mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "2. click another subject — it replaces the joined message" } }
        mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "3. from → contact joins the chain; the next subject click closes the chain" } }
        mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "4. cmd+shift+← the message — moved away, it un-joins" } }
    }

    /** The colophon. */
    mod.widgets.AboutPanel = set_type_default() do #(AboutPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 2

        mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "superapp — rust + makepad prototype." } }
        mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "no apps, no windows: specialized panels" } }
        mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "on one scrolling gridded workspace." } }
        View { width: Fill, height: 8 }
        mod.widgets.SRow { help_link := mod.widgets.SLink {} }
    }

    // ---- the modal overlays ------------------------------------------------

    /** One overlay row: a bordered card, inverted while current. The
        shell registers the click (rows live in a PortalList, whose item
        areas go stale mid-gesture — CR-002's fifth defect), so this is
        presentation only. */
    /** The card, in the one shape both variants share. A hand-drawn 1 px
        frame on its own shader — a stock-shader quad merges into a call
        that paints under the wash (CR-002's sixth defect). */
    mod.widgets.OverlayCard = View {
        width: Fill, height: 40
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{left: 16, right: 16}
        show_bg: true
        draw_bg +: {
            color: #ffffff
            border_color: #141414
            pixel: fn() {
                let p = self.pos * self.rect_size
                if p.x < 1.0 || p.y < 1.0
                    || p.x > self.rect_size.x - 1.0
                    || p.y > self.rect_size.y - 1.0 {
                    return vec4(self.border_color.xyz, 1.0)
                }
                return vec4(self.color.xyz, 1.0)
            }
        }
        num_lbl := mod.widgets.SLabel {
            width: Fit, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 13.0} }
        }
        num_gap := View { width: 20, height: 1, visible: false }
        main_lbl := mod.widgets.SLabel {
            width: Fit, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        detail_lbl := mod.widgets.SLabel {
            width: Fit, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            margin: Inset{left: 8}
            draw_text +: { color: #5a5a5a }
        }
        View { width: Fill, height: 1 }
        right_lbl := mod.widgets.SLabel {
            width: Fit, text: ""
            draw_text +: { color: #5a5a5a }
        }
    }

    mod.widgets.OverlayRow = set_type_default() do #(OverlayRow::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        padding: Inset{bottom: 8}
        // Twin cards rather than one card recoloured: a DrawQuad's shader
        // vars are not struct fields, so a quad's colour cannot be set at
        // draw time — and an Overlay-flow wash never resolves its walk.
        // Exactly one of these draws; text colours stay static in the DSL.
        // Label colours are painted per draw (a Label's draw_text.color IS
        // reachable); only the quad needs a twin.
        card := mod.widgets.OverlayCard {}
        card_inv := mod.widgets.OverlayCard {
            visible: false
            draw_bg +: { color: #141414, border_color: #141414 }
        }
    }

    /** The overlay chassis: a centred column of rows over the shell's wash.
        Workspaces and history use it bare; the launcher puts a field on top. */
    mod.widgets.RowsOverlay = set_type_default() do #(RowsOverlay::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            row := mod.widgets.OverlayRow {}
        }
    }

    /** The launcher: one field over the hits. The field is a real `SField`,
        so the query has a caret, selection, and the platform IME — the char
        grid drew a rectangle and tracked an index. */
    mod.widgets.LauncherOverlay = set_type_default() do #(LauncherOverlay::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        query_input := mod.widgets.SField {
            width: Fill
            empty_text: "search panels, mail, people…"
            return_key_type: ReturnKeyType.Go
            autocapitalize: AutoCapitalize.None
            autocorrect: AutoCorrect.Disabled
            padding: Inset{left: 14, right: 14, top: 14, bottom: 14}
        }
        View { width: Fill, height: 12 }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            row := mod.widgets.OverlayRow {}
        }
    }
}

// ---------------------------------------------------------------------------
// SBold
// ---------------------------------------------------------------------------

/// The char grid's fake bold as a widget: the same text on two overlaid
/// labels, the twin nudged 0.4 px right.
#[derive(Script, ScriptHook, Widget)]
pub struct SBold {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for SBold {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl SBoldRef {
    pub fn set_text(&self, cx: &mut Cx, text: &str) {
        let Some(inner) = self.borrow() else { return };
        inner.view.label(cx, ids!(a)).set_text(cx, text);
        inner.view.label(cx, ids!(b)).set_text(cx, text);
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
        // The remove button's clicks resolve through the shell's semantic
        // rect (list-item areas go stale mid-gesture); `account_id` stays
        // for the settings tab ring.
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
        row.view.text_input(cx, ids!(email_lbl)).set_text(cx, &a.email);
        row.view.text_input(cx, ids!(host_lbl)).set_text(
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

/// One stop on a tab ring — fields and buttons alike. (No focus-traversal
/// system exists upstream: makepad's TextInput doesn't own Tab and Robrix
/// stops at the enter-chain, so the ring is ours.)
enum RingStop {
    Input(TextInputRef),
    Remove(ButtonRef, i64),
    Add(ButtonRef),
}

impl RingStop {
    fn is_focused(&self, cx: &Cx) -> bool {
        match self {
            RingStop::Input(t) => t.key_focus(cx),
            RingStop::Remove(b, _) | RingStop::Add(b) => cx.has_key_focus(b.area()),
        }
    }

    fn focus(&self, cx: &mut Cx) {
        match self {
            RingStop::Input(t) => focus_input(cx, t),
            RingStop::Remove(b, _) | RingStop::Add(b) => cx.set_key_focus(b.area()),
        }
    }
}

/// Advance focus the way forms expect: focus + select-all, so typing
/// replaces and backspace clears.
fn focus_input(cx: &mut Cx, input: &TextInputRef) {
    input.set_key_focus(cx);
    if let Some(mut t) = input.borrow_mut() {
        t.select_all(cx);
    }
}

/// Walk a tab ring one step: wrap around; when the panel itself holds
/// focus, the first Tab lands on the first stop (last, shifted).
fn tab_ring(cx: &mut Cx, ring: &[RingStop], shift: bool) {
    if ring.is_empty() {
        return;
    }
    let dir: isize = if shift { -1 } else { 1 };
    let n = ring.len() as isize;
    let j = match ring.iter().position(|s| s.is_focused(cx)) {
        Some(i) => (i as isize + dir).rem_euclid(n),
        None if dir > 0 => 0,
        None => n - 1,
    };
    ring[j as usize].focus(cx);
}

impl SettingsPanel {
    /// The tab ring in visual order: the visible account rows' remove
    /// buttons.
    fn ring(&self, cx: &mut Cx) -> Vec<RingStop> {
        let mut v = Vec::new();
        if let Some(list) = self
            .view
            .widget(cx, ids!(accounts_list))
            .as_portal_list()
            .borrow()
        {
            let mut rows: Vec<(usize, WidgetRef)> = list
                .items()
                .iter()
                .map(|(i, item)| (*i, item.widget.clone()))
                .collect();
            rows.sort_by_key(|(i, _)| *i);
            for (_, row) in rows {
                let id = row.as_account_row().borrow().map_or(0, |r| r.account_id);
                v.push(RingStop::Remove(row.button(cx, ids!(remove_btn)), id));
            }
        }
        v
    }
}

impl Widget for SettingsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        // Tab walks the remove buttons; enter/space press the focused one.
        // The add-account link wears its own chord instead (CR-003): it is
        // the one control this panel has exactly one of.
        if let Event::KeyDown(k) = event {
            if k.modifiers.logo {
                if k.key_code == KeyCode::KeyD {
                    let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                    cx.action(PanelAction::FollowLink {
                        pid,
                        target: crate::core::Kind::AddAccount,
                        dotted: false,
                        fresh: false,
                    });
                }
                return;
            }
            if k.key_code == KeyCode::Tab {
                let ring = self.ring(cx);
                tab_ring(cx, &ring, k.modifiers.shift);
                self.redraw(cx);
            }
            if matches!(k.key_code, KeyCode::ReturnKey | KeyCode::Space) {
                for stop in self.ring(cx) {
                    if stop.is_focused(cx) {
                        if let RingStop::Remove(_, id) = stop {
                            cx.action(PanelAction::RemoveAccount(id));
                        }
                        break;
                    }
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let props = scope.props.get::<PanelProps>();
        let pid = props.map_or(0, |p| p.pid);
        let accounts = props.map(|p| mail::accounts(&p.store));
        self.view.link(cx, ids!(add_link)).set_accel(
            cx,
            pid,
            "add account",
            crate::core::Kind::AddAccount,
            false,
            Some(ui::ACCEL_ADD_ACCOUNT),
        );
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
// AddAccountPanel
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct AddAccountPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl AddAccountPanel {
    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 4] {
        [
            self.view.text_input(cx, ids!(email_input)),
            self.view.text_input(cx, ids!(pass_input)),
            self.view.text_input(cx, ids!(imap_input)),
            self.view.text_input(cx, ids!(smtp_input)),
        ]
    }

    /// The tab ring in visual order: the fields, the add button.
    fn ring(&self, cx: &mut Cx) -> Vec<RingStop> {
        let mut v: Vec<RingStop> = self.inputs(cx).into_iter().map(RingStop::Input).collect();
        v.push(RingStop::Add(self.view.button(cx, ids!(add_btn))));
        v
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

impl Widget for AddAccountPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);

        // Tab walks the fields and the button; enter/space press it.
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let ring = self.ring(cx);
                tab_ring(cx, &ring, k.modifiers.shift);
                self.redraw(cx);
            }
            if matches!(k.key_code, KeyCode::ReturnKey | KeyCode::Space) {
                let add = self.view.button(cx, ids!(add_btn));
                if cx.has_key_focus(add.area()) {
                    let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                    self.submit(cx, pid);
                }
            }
        }

        if let Event::Actions(actions) = event {
            let [email, pass, imap, smtp] = self.inputs(cx);
            // A blurred field keeps no selection (the frameworks' norm —
            // tab-in selects all, tab-out lets go).
            for t in [&email, &pass, &imap, &smtp] {
                if t.key_focus_lost(actions) {
                    t.set_cursor(cx, t.cursor(), false);
                }
            }
            // Enter advances; past the last field it submits.
            if email.returned(actions).is_some() {
                focus_input(cx, &pass);
            } else if pass.returned(actions).is_some() {
                focus_input(cx, &imap);
            } else if imap.returned(actions).is_some() {
                focus_input(cx, &smtp);
            } else if smtp.returned(actions).is_some()
                || self.view.button(cx, ids!(add_btn)).clicked(actions)
            {
                let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                self.submit(cx, pid);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
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

        // Tab walks to → subject → body, wrapping; from panel focus it
        // lands on "to" (the body's enter stays a newline — multiline
        // TextInput owns it).
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let inputs = self.inputs(cx);
                let dir: isize = if k.modifiers.shift { -1 } else { 1 };
                let n = inputs.len() as isize;
                let j = match inputs.iter().position(|t| t.key_focus(cx)) {
                    Some(i) => (i as isize + dir).rem_euclid(n),
                    None if dir > 0 => 0,
                    None => n - 1,
                };
                focus_input(cx, &inputs[j as usize]);
            }
        }

        if let Event::Actions(actions) = event {
            let [to, subject, body] = self.inputs(cx);
            for t in [&to, &subject, &body] {
                if t.key_focus_lost(actions) {
                    t.set_cursor(cx, t.cursor(), false);
                }
            }
            if to.returned(actions).is_some() {
                focus_input(cx, &subject);
            } else if subject.returned(actions).is_some() {
                focus_input(cx, &body);
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
pub struct InboxLine {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for InboxLine {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl InboxLineRef {
    fn populate(&self, cx: &mut Cx, m: &mail::MailHead) {
        let Some(inner) = self.borrow() else { return };
        let from = &m.from_name;
        let fp = inner.view.label(cx, ids!(from_lbl));
        let fb = inner.view.widget(cx, ids!(from_b));
        let sp = inner.view.label(cx, ids!(subject_lbl));
        let sb = inner.view.widget(cx, ids!(subject_b));
        fp.set_text(cx, if m.unread { "" } else { from });
        fb.as_sbold().set_text(cx, if m.unread { from } else { "" });
        fp.set_visible(cx, !m.unread);
        fb.set_visible(cx, m.unread);
        sp.set_text(cx, if m.unread { "" } else { &m.subject });
        sb.as_sbold().set_text(cx, if m.unread { &m.subject } else { "" });
        sp.set_visible(cx, !m.unread);
        sb.set_visible(cx, m.unread);
        inner
            .view
            .label(cx, ids!(date_lbl))
            .set_text(cx, &mail::fmt_date(m.date));
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct InboxRow {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for InboxRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // Clicks resolve through the shell's registered rects — a list
        // item's own area goes stale on any mid-gesture redraw, so a
        // down/up pair here cannot be trusted. The row's share is the
        // cursor.
        if let Hit::FingerHoverIn(_) = event.hits(cx, self.view.area()) {
            cx.set_cursor(MouseCursor::Hand);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl InboxRowRef {
    fn populate(&self, cx: &mut Cx, m: &mail::MailHead, selected: bool) {
        let Some(row) = self.borrow() else { return };
        let line = row.view.widget(cx, ids!(line));
        let line_sel = row.view.widget(cx, ids!(line_sel));
        line.as_inbox_line().populate(cx, m);
        line_sel.as_inbox_line().populate(cx, m);
        line.set_visible(cx, !selected);
        line_sel.set_visible(cx, selected);
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
        // Keep the cursor on screen: a row without a live item is off-view.
        let list = self.view.widget(cx, ids!(list)).as_portal_list();
        let visible = list
            .borrow()
            .is_some_and(|l| l.items().iter().any(|(idx, _)| *idx == i));
        if !visible {
            list.smooth_scroll_to(cx, i, 90.0, None, 0.0);
        }
        self.redraw(cx);
    }
}

impl Widget for InboxPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let filter = self.view.text_input(cx, ids!(filter_input));
        let filter_focused = filter.key_focus(cx);
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);

        // `/` focuses the filter — the one plain letter the grammar keeps
        // (CR-003 retired the vim walk; the arrows already mirrored it).
        // It arrives as a TextInput event, exactly like real typing.
        if let Event::TextInput(t) = event {
            if !filter_focused && t.input == "/" {
                focus_input(cx, &filter);
            }
        }
        if let Event::KeyDown(k) = event {
            if !filter_focused {
                match k.key_code {
                    KeyCode::ReturnKey => {
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
                    // The row walk, with scroll-follow (CR-003: the arrows
                    // are the whole walk now, j/k having gone).
                    KeyCode::ArrowDown => self.move_sel(cx, scope, 1),
                    KeyCode::ArrowUp => self.move_sel(cx, scope, -1),
                    // The inbox's one-stop tab ring: the filter.
                    KeyCode::Tab => focus_input(cx, &filter),
                    _ => {}
                }
            }
        }
        if let Event::Actions(actions) = event {
            if filter.key_focus_lost(actions) {
                filter.set_cursor(cx, filter.cursor(), false);
            }
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
        let sel = self.sel;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, rows.len());
                while let Some(idx) = list.next_visible_item(cx) {
                    if let Some(m) = rows.get(idx) {
                        let row = list.item(cx, idx, live_id!(row));
                        row.as_inbox_row().populate(cx, m, sel == Some(m.id));
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
    }
}


// ---------------------------------------------------------------------------
// SLink
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct SLink {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    pid: u64,
    #[rust]
    target: Option<crate::core::Kind>,
    #[rust]
    dotted: bool,
}

impl Widget for SLink {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Hit::FingerUp(fe) = event.hits(cx, self.view.area()) {
            if fe.is_over && fe.was_tap() {
                if let Some(target) = self.target.clone() {
                    cx.action(PanelAction::FollowLink {
                        pid: self.pid,
                        target,
                        dotted: self.dotted,
                        fresh: fe.modifiers.logo || fe.modifiers.alt,
                    });
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl SLinkRef {
    pub fn set(
        &self,
        cx: &mut Cx,
        pid: u64,
        text: &str,
        target: crate::core::Kind,
        dotted: bool,
    ) {
        self.set_accel(cx, pid, text, target, dotted, None);
    }

    /// As [`Self::set`], but `accel` names the key this link carries — its
    /// letter is drawn twice, nudged, so the link wears its own chord.
    pub fn set_accel(
        &self,
        cx: &mut Cx,
        pid: u64,
        text: &str,
        target: crate::core::Kind,
        dotted: bool,
        accel: Option<char>,
    ) {
        let Some(mut l) = self.borrow_mut() else { return };
        l.pid = pid;
        l.target = Some(target);
        l.dotted = dotted;
        let at = accel.and_then(|c| ui::accel_idx(text, c));
        let (pre, key, post) = match at {
            Some(i) => {
                let mut it = text.chars();
                let pre: String = it.by_ref().take(i).collect();
                let key: String = it.next().into_iter().collect();
                (pre, key, it.collect::<String>())
            }
            None => (text.to_string(), String::new(), String::new()),
        };
        // An empty label still reserves width, which would push the text
        // right of an underline that spans the whole row — so the unused
        // parts stand down entirely rather than render nothing.
        let pre_l = l.view.label(cx, ids!(row.pre));
        pre_l.set_text(cx, &pre);
        pre_l.set_visible(cx, !pre.is_empty());
        let post_l = l.view.label(cx, ids!(row.post));
        post_l.set_text(cx, &post);
        post_l.set_visible(cx, !post.is_empty());
        for p in [ids!(row.key.k1), ids!(row.key.k2), ids!(row.key.k3)] {
            l.view.label(cx, p).set_text(cx, &key);
        }
        l.view.view(cx, ids!(row.key)).set_visible(cx, !key.is_empty());
        l.view.view(cx, ids!(ul)).set_visible(cx, !dotted);
        l.view.view(cx, ids!(ul_dotted)).set_visible(cx, dotted);
    }
}

// ---------------------------------------------------------------------------
// MessagePanel
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct MessagePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for MessagePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // Arrows scroll the body three lines (the char grid's behaviour) —
        // synthesized as a Scroll event so the ScrollBars keep clamping
        // and position, no shadow state.
        if let Event::KeyDown(k) = event {
            let d = match k.key_code {
                KeyCode::ArrowDown => 3.0,
                KeyCode::ArrowUp => -3.0,
                _ => 0.0,
            };
            if d != 0.0 {
                let r = self.view.view(cx, ids!(body_scroll)).area().rect(cx);
                if r.size.y > 0.0 {
                    let ev = Event::Scroll(ScrollEvent {
                        window_id: CxWindowPool::id_zero(),
                        scroll: dvec2(0.0, d * 14.0),
                        abs: r.pos + r.size * 0.5,
                        modifiers: KeyModifiers::default(),
                        handled_x: std::cell::Cell::new(false),
                        handled_y: std::cell::Cell::new(false),
                        is_mouse: false,
                        time: 0.0,
                        phase: ScrollPhase::None,
                    });
                    self.view.handle_event(cx, &ev, scope);
                    self.redraw(cx);
                }
            }
        }
        // The message panel's link accelerators (CR-003): the walk that used
        // to be a hidden j/k is cmd+n / cmd+o now, drawn onto the links
        // themselves, and reply is cmd+r. The shell forwards any cmd chord
        // it does not own itself.
        if let Event::KeyDown(k) = event {
            if !k.modifiers.logo {
                return;
            }
            let Some(p) = scope.props.get::<PanelProps>() else {
                return;
            };
            let crate::core::Kind::Message { id } = p.kind else {
                return;
            };
            let c = match k.key_code {
                KeyCode::KeyN => ui::ACCEL_NEWER,
                KeyCode::KeyO => ui::ACCEL_OLDER,
                KeyCode::KeyR => ui::ACCEL_REPLY,
                _ => return,
            };
            if c == ui::ACCEL_REPLY {
                cx.action(PanelAction::FollowLink {
                    pid: p.pid,
                    target: crate::core::Kind::Compose { re: id },
                    dotted: false,
                    fresh: false,
                });
                return;
            }
            let (newer, older) = mail::neighbours(&p.store, id);
            let target = if c == ui::ACCEL_OLDER { older } else { newer };
            if let Some(nid) = target {
                cx.action(PanelAction::FollowLink {
                    pid: p.pid,
                    target: crate::core::Kind::Message { id: nid },
                    dotted: true,
                    fresh: false,
                });
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(p) = scope.props.get::<PanelProps>() {
            if let crate::core::Kind::Message { id } = p.kind {
                let pid = p.pid;
                if let Some(m) = mail::mail(&p.store, id) {
                    self.view.link(cx, ids!(from_link)).set(
                        cx,
                        pid,
                        &format!("{} <{}>", m.head.from_name, m.head.from_email),
                        crate::core::Kind::Contact {
                            email: m.head.from_email.clone(),
                        },
                        false,
                    );
                    // Selectable now, so they are TextInputs, not labels.
                    self.view.text_input(cx, ids!(to_lbl)).set_text(cx, &m.to);
                    self.view
                        .text_input(cx, ids!(date_lbl))
                        .set_text(cx, &mail::fmt_date(m.head.date));
                    let (ok_l, err_l) = (
                        self.view.label(cx, ids!(status_lbl)),
                        self.view.label(cx, ids!(status_err_lbl)),
                    );
                    match &m.status {
                        Some((txt, true)) => {
                            err_l.set_text(cx, txt);
                            err_l.set_visible(cx, true);
                            ok_l.set_visible(cx, false);
                        }
                        Some((txt, false)) => {
                            ok_l.set_text(cx, txt);
                            ok_l.set_visible(cx, true);
                            err_l.set_visible(cx, false);
                        }
                        None => {
                            ok_l.set_visible(cx, false);
                            err_l.set_visible(cx, false);
                        }
                    }
                    // The HTML reading wins when the sender sent one: it
                    // keeps the lists, the emphasis and the links that
                    // flattening to text throws away. Both are written
                    // every time — the hidden one is emptied rather than
                    // merely hidden, so no mail can leave its text behind
                    // for the next one to show.
                    let html = m.html.as_deref().unwrap_or("");
                    self.view
                        .text_input(cx, ids!(body_lbl))
                        .set_text(cx, if m.html.is_some() { "" } else { &m.body });
                    self.view.html(cx, ids!(body_html)).set_text(cx, html);
                    self.view
                        .view(cx, ids!(text_wrap))
                        .set_visible(cx, m.html.is_none());
                    self.view
                        .view(cx, ids!(html_wrap))
                        .set_visible(cx, m.html.is_some());
                    let (newer, older) = mail::neighbours(&p.store, id);
                    let nl = self.view.link(cx, ids!(newer_link));
                    let no = self.view.label(cx, ids!(newer_off));
                    if let Some(n) = newer {
                        nl.set_accel(
                            cx,
                            pid,
                            "← newer",
                            crate::core::Kind::Message { id: n },
                            true,
                            Some(ui::ACCEL_NEWER),
                        );
                    }
                    nl.set_visible(cx, newer.is_some());
                    no.set_visible(cx, newer.is_none());
                    let ol = self.view.link(cx, ids!(older_link));
                    let oo = self.view.label(cx, ids!(older_off));
                    if let Some(o) = older {
                        ol.set_accel(
                            cx,
                            pid,
                            "older →",
                            crate::core::Kind::Message { id: o },
                            true,
                            Some(ui::ACCEL_OLDER),
                        );
                    }
                    ol.set_visible(cx, older.is_some());
                    oo.set_visible(cx, older.is_none());
                    self.view.link(cx, ids!(reply_link)).set_accel(
                        cx,
                        pid,
                        "reply",
                        crate::core::Kind::Compose { re: id },
                        false,
                        Some(ui::ACCEL_REPLY),
                    );
                }
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

// ---------------------------------------------------------------------------
// ContactPanel
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct ContactPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for ContactPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(p) = scope.props.get::<PanelProps>() {
            if let crate::core::Kind::Contact { email } = &p.kind {
                let (name, count) = mail::contact(&p.store, email);
                let first = name.split(' ').next().unwrap_or(&name).to_lowercase();
                self.view.label(cx, ids!(name_lbl)).set_text(cx, &name);
                self.view.label(cx, ids!(name_lbl2)).set_text(cx, &name);
                self.view.label(cx, ids!(email_lbl)).set_text(cx, email);
                self.view
                    .label(cx, ids!(count_lbl))
                    .set_text(cx, &format!("{count} message(s) in mail"));
                self.view.link(cx, ids!(from_link)).set(
                    cx,
                    p.pid,
                    &format!("messages from {first}"),
                    crate::core::Kind::Inbox {
                        filter: Some(email.clone()),
                    },
                    false,
                );
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

// ---------------------------------------------------------------------------
// HelpPanel / AboutPanel
// ---------------------------------------------------------------------------

/// The manual. Static prose in the DSL; the live parts — the two demo links,
/// the demo button, and which platform's rows are visible — are settled here.
#[derive(Script, ScriptHook, Widget)]
pub struct HelpPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for HelpPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(try_btn)).clicked(actions) {
                let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                cx.action(PanelAction::TryIt { pid });
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
        // The legend demonstrates the grammar with the real thing: these
        // links open and replace exactly like any other.
        self.view.link(cx, ids!(solid_link)).set(
            cx,
            pid,
            "solid underline",
            crate::core::Kind::About,
            false,
        );
        self.view.link(cx, ids!(dotted_link)).set(
            cx,
            pid,
            "dotted underline",
            crate::core::Kind::About,
            true,
        );
        // Platform rows: the DSL holds every variant, `cfg!` picks.
        let android = cfg!(target_os = "android");
        self.view
            .view(cx, ids!(menu_row))
            .set_visible(cx, cfg!(target_os = "macos"));
        self.view.view(cx, ids!(desk_launch)).set_visible(cx, !android);
        self.view.view(cx, ids!(touch_launch)).set_visible(cx, android);
        self.view.view(cx, ids!(touch_help)).set_visible(cx, android);
        self.view.draw_walk(cx, scope, walk)
    }
}

/// The colophon: three lines and the way back.
#[derive(Script, ScriptHook, Widget)]
pub struct AboutPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for AboutPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
        self.view.link(cx, ids!(help_link)).set(
            cx,
            pid,
            "back to help",
            crate::core::Kind::Help,
            true,
        );
        self.view.draw_walk(cx, scope, walk)
    }
}

// ---------------------------------------------------------------------------
// The overlays
// ---------------------------------------------------------------------------

/// One overlay row. Presentation only — the shell owns the click.
#[derive(Script, ScriptHook, Widget)]
pub struct OverlayRow {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for OverlayRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Hit::FingerHoverIn(_) = event.hits(cx, self.view.area()) {
            cx.set_cursor(MouseCursor::Hand);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl OverlayRowRef {
    fn populate(&self, cx: &mut Cx, d: &OverlayRowData) {
        let Some(row) = self.borrow() else { return };
        // Inverted while current; an undone branch stays legible but quiet.
        let (bg, fg, dim) = if d.current {
            (vec4(0.078, 0.078, 0.078, 1.0), vec4(1.0, 1.0, 1.0, 1.0), vec4(0.75, 0.75, 0.75, 1.0))
        } else if d.muted {
            (vec4(1.0, 1.0, 1.0, 1.0), vec4(0.565, 0.565, 0.565, 1.0), vec4(0.72, 0.72, 0.72, 1.0))
        } else {
            (vec4(1.0, 1.0, 1.0, 1.0), vec4(0.078, 0.078, 0.078, 1.0), vec4(0.353, 0.353, 0.353, 1.0))
        };
        // One card draws; the other stands down. A quad's shader vars are
        // not struct fields (no runtime colour), but a Label's draw_text
        // colour is — so the twin is only for the background.
        row.view.view(cx, ids!(card)).set_visible(cx, !d.current);
        row.view.view(cx, ids!(card_inv)).set_visible(cx, d.current);
        let c = if d.current {
            ids!(card_inv)
        } else {
            ids!(card)
        };
        let paint = |_cx: &mut Cx, lbl: &LabelRef, col: Vec4f| {
            if let Some(mut l) = lbl.borrow_mut() {
                l.draw_text.color = col;
            }
        };
        let num = row.view.label(cx, &[c[0], live_id!(num_lbl)]);
        num.set_text(cx, &d.num);
        num.set_visible(cx, !d.num.is_empty());
        paint(cx, &num, fg);
        row.view
            .view(cx, &[c[0], live_id!(num_gap)])
            .set_visible(cx, !d.num.is_empty());
        let main = row.view.label(cx, &[c[0], live_id!(main_lbl)]);
        main.set_text(cx, &d.main);
        paint(cx, &main, fg);
        let detail = row.view.label(cx, &[c[0], live_id!(detail_lbl)]);
        detail.set_text(cx, &d.detail);
        detail.set_visible(cx, !d.detail.is_empty());
        paint(cx, &detail, dim);
        let right = row.view.label(cx, &[c[0], live_id!(right_lbl)]);
        right.set_text(cx, &d.right);
        right.set_visible(cx, !d.right.is_empty());
        paint(cx, &right, dim);
        let _ = bg;
    }
}

/// A column of overlay rows — the workspaces roster, the undo DAG.
#[derive(Script, ScriptHook, Widget)]
pub struct RowsOverlay {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for RowsOverlay {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rows = scope
            .props
            .get::<OverlayProps>()
            .map(|p| p.rows.clone())
            .unwrap_or_default();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, rows.len());
                while let Some(idx) = list.next_visible_item(cx) {
                    if let Some(d) = rows.get(idx) {
                        let row = list.item(cx, idx, live_id!(row));
                        row.as_overlay_row().populate(cx, d);
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
    }
}

/// The launcher: a real text field over the hits.
#[derive(Script, ScriptHook, Widget)]
pub struct LauncherOverlay {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for LauncherOverlay {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let q = self.view.text_input(cx, ids!(query_input));
            if q.changed(actions).is_some() {
                cx.action(OverlayAction::Query(q.text()));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rows = scope
            .props
            .get::<OverlayProps>()
            .map(|p| p.rows.clone())
            .unwrap_or_default();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, rows.len());
                while let Some(idx) = list.next_visible_item(cx) {
                    if let Some(d) = rows.get(idx) {
                        let row = list.item(cx, idx, live_id!(row));
                        row.as_overlay_row().populate(cx, d);
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
    }
}

impl LauncherOverlayRef {
    /// Seeds the field and takes the keyboard — called when the overlay
    /// opens, so typing lands in the query without a tap.
    pub fn focus_query(&self, cx: &mut Cx, text: &str) {
        let Some(inner) = self.borrow() else { return };
        let q = inner.view.text_input(cx, ids!(query_input));
        q.set_text(cx, text);
        q.set_key_focus(cx);
    }

    /// Keeps the selected hit on screen as arrows walk it.
    pub fn scroll_to(&self, cx: &mut Cx, idx: usize) {
        let Some(inner) = self.borrow() else { return };
        let list = inner.view.widget(cx, ids!(list)).as_portal_list();
        let visible = list
            .borrow()
            .is_some_and(|l| l.items().iter().any(|(i, _)| *i == idx));
        if !visible {
            list.smooth_scroll_to(cx, idx, 90.0, None, 0.0);
        }
    }
}

/// Intent from an overlay widget. Rows resolve through the shell's own hit
/// table (they live in a PortalList), so only the query field speaks here.
#[derive(Debug, Clone)]
pub enum OverlayAction {
    /// The launcher's field changed — re-run the search.
    Query(String),
}

/// `WidgetRef → SLinkRef` convenience mirroring the generated accessors.
trait LinkViewExt {
    fn link(&self, cx: &mut Cx, path: &[LiveId]) -> SLinkRef;
}
impl LinkViewExt for View {
    fn link(&self, cx: &mut Cx, path: &[LiveId]) -> SLinkRef {
        self.widget(cx, path).as_slink()
    }
}
