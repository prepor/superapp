//! CR-002: retained panel content. The semantic widget library — makepad
//! primitives wrapped once and themed to the design language — and the
//! per-kind panel widgets composed from it (Robrix's patterns; same
//! script_mod generation).
//!
//! Data flows in per draw via [`PanelProps`] on the scope; intent flows out
//! as [`PanelAction`]s (global actions the shell catches and turns into
//! store actions — so undo semantics never enter this module).

use std::collections::HashMap;

use makepad_widgets::makepad_platform::event::{ScrollEvent, ScrollPhase};
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;

use crate::mail;
use crate::richtable::{self, Completion, SqlSource, Suggestion, Table};
use crate::store::Store;
use crate::ui;

/// What a panel widget may read while drawing: the store and its own
/// panel identity. Passed through `Scope` props each draw (props ride an
/// `Any`, hence the `Rc` — scope wants `'static`).
pub struct PanelProps {
    pub store: std::rc::Rc<Store>,
    pub pid: u64,
    pub kind: crate::core::Kind,
    /// Which messages of its thread a message panel shows open (CR-007).
    /// Panel context, owned by the shell; `None` for every other kind.
    pub expand: Option<Expansion>,
}

/// Which messages of a conversation a panel shows open, and whose quoted
/// tails are unfolded (CR-007). Seeded by the shell when the panel opens —
/// the mail it opened on plus whatever was unread — and toggled by touch.
/// Context, not history: it persists no further than the process.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Expansion {
    /// The mail the set was seeded for. A panel re-targeted since starts
    /// over with only its own mail open.
    pub for_mail: i64,
    pub open: std::collections::BTreeSet<i64>,
    pub quotes: std::collections::BTreeSet<i64>,
}

impl Expansion {
    /// What a panel shows when nothing seeded it: its own mail, open.
    #[must_use]
    pub fn just(mail: i64) -> Self {
        Expansion {
            for_mail: mail,
            open: std::iter::once(mail).collect(),
            quotes: std::collections::BTreeSet::new(),
        }
    }

    /// The set a panel on `mail` should draw: this one if it was seeded
    /// for that mail, the bare default otherwise.
    #[must_use]
    pub fn for_panel(this: Option<&Expansion>, mail: i64) -> Expansion {
        match this {
            Some(e) if e.for_mail == mail => e.clone(),
            _ => Expansion::just(mail),
        }
    }
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
    /// Under the pointer: a grey wash, the way a button or a tab takes one.
    pub hovered: bool,
}

/// One overlay row's height, in points — the shell sizes the sheet to fit.
pub const OVERLAY_ROW_H: f64 = 40.0;

/// What an overlay widget draws. Assembled by the shell each frame from the
/// workspace roster, the undo DAG, or the launcher's live search.
#[derive(Clone, Default)]
pub struct OverlayProps {
    pub rows: Vec<OverlayRowData>,
    /// The launcher's query, pushed into the field when the overlay opens.
    pub query: String,
    /// The chassis' presence, 0..1: the widget composites its whole
    /// subtree at this alpha — the open/close fade.
    pub alpha: f32,
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
    /// The inbox cursor landed on a mail (CR-005): open it joined but leave
    /// focus in the list, so the walk carries on. Deliberately not a flag on
    /// [`PanelAction::OpenMail`] — a preview is never the `fresh` variant,
    /// and two bools would let that nonsense be spelled.
    PreviewMail { pid: u64, id: i64 },
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
    // The one face, carried rather than borrowed. Menlo fronted the family
    // until HTML mail asked it for a weight it does not have: Menlo.ttc
    // yields only its regular face, so `<b>` drew as body text. It was
    // also macOS furniture — the Fold never had it and fell through to
    // Liberation, so "the app's face" was already two faces depending on
    // which screen you read it from.
    //
    // Geist Mono replaces it on both, from `resources/` (OFL, shipped
    // alongside in OFL.txt). It is 0.600 em wide against Menlo's 0.6021,
    // so the character grid moves by a third of a percent — and that is
    // Liberation's ratio to four places, so the Fold has been drawing this
    // width all along. macOS is the side that changes.
    //
    // Two files, four styles: both faces are variable on `wght` (100–900),
    // and the second is a true italic rather than a slant. makepad has no
    // synthetic oblique — `FontMember` exposes only `weight`, and nothing
    // in `TextStyle` skews — so italic could only ever come from its own
    // file. The `[wght]` of the upstream filenames is dropped because the
    // brackets are awkward in a resource path; the fonts are unmodified.
    mod.widgets.SMonoStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: crate_resource("self:resources/geist_mono_variable.ttf") asc: 0.0 desc: 0.0}
            fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
            symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
            emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
        }
        font_size: 10.5
        line_spacing: 1.0
    }

    /** The same face leaning on its weight axis. This is what retired the
        char grid's fake bold: unread rows, contact headers and the
        accelerator marks were all the same run drawn two or three times,
        each copy nudged a fraction of a pixel, because Menlo had no weight
        to ask for. See `SBoldLabel`. */
    mod.widgets.SMonoBoldStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: crate_resource("self:resources/geist_mono_variable.ttf") asc: 0.0 desc: 0.0 weight: 700.0}
            fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
            symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
            emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
        }
        font_size: 10.5
        line_spacing: 1.0
    }

    /** The drawn italic, not a skewed roman: Geist Mono ships its own. */
    mod.widgets.SMonoItalicStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: crate_resource("self:resources/geist_mono_italic_variable.ttf") asc: 0.0 desc: 0.0}
            fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
            symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
            emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
        }
        font_size: 10.5
        line_spacing: 1.0
    }

    /** Both at once — `<b><i>`, and the `<em>` inside a heading. */
    mod.widgets.SMonoBoldItalicStyle = TextStyle{
        font_family: FontFamily{
            latin := FontMember{res: crate_resource("self:resources/geist_mono_italic_variable.ttf") asc: 0.0 desc: 0.0 weight: 700.0}
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

    /** Body text at the family's bold weight.

        This used to be a widget that drew the same run twice with the twin
        nudged 0.4 px, because Menlo ships no weight to ask for. Geist Mono
        does, so the trick is gone and with it the overlays, the twin
        labels and the pair of set_texts each one needed. */
    mod.widgets.SBoldLabel = mod.widgets.SLabel {
        draw_text +: { text_style: mod.widgets.SMonoBoldStyle{} }
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

        Emphasis is real: `<b>` is the weight axis and `<i>` is Geist
        Mono's drawn italic, so the four `text_style_*` slots are four
        actual faces rather than one face repeated (see `SMonoStyle`). */
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
        text_style_italic: mod.widgets.SMonoItalicStyle{}
        text_style_bold: mod.widgets.SMonoBoldStyle{}
        text_style_bold_italic: mod.widgets.SMonoBoldItalicStyle{}
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
        // mark (CR-003): prefix, the key, suffix. The split stays even now
        // that the key is real bold — `←` arrives from the symbol
        // fallback, whose advance is not the mono cell, so padding a twin
        // with spaces would not line up.
        // Label's base padding is mspace_1 — invisible around a single run,
        // but it would open a gap between each of the three, so the split
        // parts zero it and the row carries the word's own spacing.
        row := View {
            width: Fit, height: Fit
            flow: Right
            pre := mod.widgets.SLabel { padding: 0, text: "" }
            // One pass. It took three nudged copies to make a single
            // character read as bold at this size; the weight axis does it
            // properly.
            key := mod.widgets.SBoldLabel { padding: 0, text: "" }
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

    // ---- autocomplete ------------------------------------------------------

    /** One autocomplete row: the pick, then what it means, muted. Twin
        lines again (see `InboxRow`): the highlighted one is inverted, and
        a quad's colour is not a runtime value. */
    mod.widgets.SuggestLine = View {
        width: Fill, height: Fit
        align: Align{y: 0.5}
        padding: Inset{left: 8, right: 8, top: 3, bottom: 3}
        lbl := mod.widgets.SLabel { padding: 0, width: Fit, max_lines: 1, text: "" }
        View { width: 10, height: 1 }
        desc := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { color: #909090 }
        }
    }

    mod.widgets.SuggestRow = View {
        width: Fill, height: Fit
        flow: Down
        line := mod.widgets.SuggestLine {}
        line_sel := mod.widgets.SuggestLine {
            visible: false
            show_bg: true
            draw_bg +: {
                color: #141414
                pixel: fn() {
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
            }
        }
    }

    /** A field's autocomplete (CR-006): a bordered box hung under the
        field, over whatever follows it — the inbox filter's `@tag` names
        and values, the compose TO field's addresses. Eight fixed slots,
        shown as needed; the offer is capped there. The panel holds one as
        a `suggest:` property and drives it through `Suggest`. */
    // Drawn after everything else in the panel, at an absolute position,
    // so it must land in a draw call *after* the content it covers. The
    // background's own pixel fn (a hairline ink border, the design's) makes
    // it a shader no earlier call shares — the same ordering trap the
    // selection wash documents, answered the same way.
    mod.widgets.SuggestBox = View {
        width: Fill, height: Fit
        flow: Down
        show_bg: true
        padding: Inset{left: 1, right: 1, top: 1, bottom: 1}
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
        s0 := mod.widgets.SuggestRow {}
        s1 := mod.widgets.SuggestRow {}
        s2 := mod.widgets.SuggestRow {}
        s3 := mod.widgets.SuggestRow {}
        s4 := mod.widgets.SuggestRow {}
        s5 := mod.widgets.SuggestRow {}
        s6 := mod.widgets.SuggestRow {}
        s7 := mod.widgets.SuggestRow {}
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
        // The TO field's autocomplete, drawn last and over the fields under
        // it (see `SuggestBox`).
        suggest: mod.widgets.SuggestBox {}
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
        // The row's inset is the one source of spacing: every label in it
        // sheds the theme padding (upstream Label ships mspace_1, 3 pt a
        // side), so the text sits 8 pt inside the row — the filter's own
        // text inset (1 pt border + 7 pt padding) — and the two lines
        // stack on their bare line boxes, 3 pt above and below.
        padding: Inset{left: 8, right: 8, top: 3, bottom: 3}
        spacing: 0
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            // A Fill *Label* on a Right flow's main axis defer-walks. So
            // the from twins ride a Fill View whose flow is Down: there
            // Fill width is the cross axis — no defer, and the left edge
            // matches the subject line (the same construction).
            View {
                width: Fill, height: Fit
                flow: Down
                from_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
                from_b := mod.widgets.SBoldLabel {
                    visible: false
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
            }
            View { width: 10, height: 1 }
            date_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        subject_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        subject_b := mod.widgets.SBoldLabel {
            visible: false
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
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

    /** The inbox: the filter over the header over the virtualized list —
        a rich table (CR-006) over `mail::INBOX`. */
    mod.widgets.InboxPanel = set_type_default() do #(InboxPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        // No flow spacing: the gaps are explicit, so a rule sits the same
        // 3 pt under the header as under every row, and the first row
        // hangs off its rule exactly like the rest.
        spacing: 0

        filter_input := mod.widgets.SField {
            width: Fill
            empty_text: "filter…  ( / )   @ for tags"
            return_key_type: ReturnKeyType.Search
            autocapitalize: AutoCapitalize.None
            autocorrect: AutoCorrect.Disabled
        }
        // What the filter could not read, in the one colour errors get.
        err_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            margin: Inset{left: 8, top: 4}
            text: "", draw_text +: { color: #a01500 }
        }
        View { width: Fill, height: 6 }
        // Header cells for the columns only — the subject rides each row's
        // extra line, owns no column, and so gets no header. The header
        // wears the rows' inset and its labels shed the theme padding, so
        // FROM shares the rows' left edge and DATE their right. FROM sits
        // in a Fill View, not at Fill itself — the rows' construction,
        // so it walks exactly like their from label.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "FROM" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "DATE" }
        }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            // A row that scrolls out is kept for the next one that scrolls
            // in, rather than built again: a long list scrolls without
            // minting widgets.
            reuse_items: true
            row := mod.widgets.InboxRow {}
        }
        // The autocomplete, drawn last and over the rows (see `SuggestBox`).
        suggest: mod.widgets.SuggestBox {}
    }

    // ---- the read panels ---------------------------------------------------

    /** One message of a conversation (CR-007): a header row that is the
        same row open or closed — the sender, the date at the right edge —
        with the letter unfolded under it while open. Closed, the row
        previews the first line the author wrote, or the status line, red
        when it is an error. */
    mod.widgets.ThreadMsg = set_type_default() do #(ThreadMsg::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        head := View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            padding: Inset{top: 4, bottom: 4}
            name_lbl := mod.widgets.SLabel { padding: 0, width: Fit, max_lines: 1, text: "" }
            from_link := mod.widgets.SLink { visible: false }
            View { width: 10, height: 1 }
            // The preview rides a Fill View whose flow is Down, for the
            // reason the inbox row's from label does: a Fill label on a
            // Right flow's main axis defer-walks.
            preview_wrap := View {
                width: Fill, height: Fit
                flow: Down
                preview_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #909090 }
                }
                preview_err := mod.widgets.SLabel {
                    visible: false
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #a01500 }
                }
            }
            spacer := View { visible: false, width: Fill, height: 1 }
            View { width: 10, height: 1 }
            date_lbl := mod.widgets.SLabel {
                padding: 0
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
            // Two readings of one letter; the row shows whichever the mail
            // actually carries, never both — each in its own View, the
            // widget that honours `visible`.
            text_wrap := View {
                width: Fill, height: Fit
                body_lbl := mod.widgets.SText { is_multiline: true }
            }
            html_wrap := View {
                width: Fill, height: Fit
                visible: false
                body_html := mod.widgets.SHtml {}
            }
            // The quoted tail, folded behind one line: in a thread it is
            // the message above. Touch unfolds it in place.
            quote_fold := View {
                visible: false
                width: Fit, height: Fit
                mod.widgets.SLabel { padding: 0, text: "› quoted", draw_text +: { color: #909090 } }
            }
            quote_text := View {
                width: Fill, height: Fit
                visible: false
                quote_lbl := mod.widgets.SText {
                    is_multiline: true
                    draw_text +: {
                        color: #5a5a5a
                        color_hover: #5a5a5a
                        color_focus: #5a5a5a
                        color_down: #5a5a5a
                    }
                }
            }
            quote_html := View {
                width: Fill, height: Fit
                visible: false
                quote_body := mod.widgets.SHtml {}
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

    /** One mail, in its conversation (CR-007): the account it came to,
        once; every message of the thread, oldest first, open or closed;
        reply at the foot. */
    mod.widgets.MessagePanel = set_type_default() do #(MessagePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

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
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #dcdcdc } }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            msg := mod.widgets.ThreadMsg {}
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
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
        // Short lines: a panel wide enough for one long row of these was
        // already a squeeze before delete joined the message's chrome.
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "  message " }
            mod.widgets.SKbd { text: "cmd+a" }
            mod.widgets.SLabel { text: "rchive " }
            mod.widgets.SKbd { text: "cmd+d" }
            mod.widgets.SLabel { text: "elete " }
            mod.widgets.SKbd { text: "cmd+r" }
            mod.widgets.SLabel { text: "eply" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "  inbox   " }
            mod.widgets.SKbd { text: "cmd+s" }
            mod.widgets.SLabel { text: "ync  " }
            mod.widgets.SKbd { text: "enter" }
            mod.widgets.SLabel { text: " goes  " }
            mod.widgets.SKbd { text: "/" }
            mod.widgets.SLabel { text: " filters" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "          arrows walk the rows" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "clicking a row, or walking onto it, opens the thread beside the list without leaving it — and that preview lends the list its own keys" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "in a thread, a closed message is a row: click it to open it in place, click an open one's header to close it" }
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
            mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "drag up/down — scroll a panel's content" } }
            mod.widgets.SRow { mod.widgets.SLabel { width: Fill, text: "drag a mail sideways — left archives, right deletes; let go before the ink fills and nothing happens" } }
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

    /** A view that renders its subtree to a texture and composites it at
        one alpha. Widgets cannot alpha-fade as a subtree (CR-002's named
        cost) — an offscreen pass can, and the composite is a single quad
        whose `alpha` uniform the shell drives per frame. This is what lets
        an overlay's field, caret, rows and all fade as one surface. */
    mod.widgets.FadeView = mod.widgets.View {
        texture_caching: true
        draw_bg +: {
            alpha: uniform(0.0)
            image: texture_2d(float)
            scale: varying(vec2(0))
            shift: varying(vec2(0))
            vertex: fn() {
                let dpi = self.draw_pass.dpi_factor
                let ceil_size = ceil(self.rect_size * dpi) / dpi
                let floor_pos = floor(self.rect_pos * dpi) / dpi
                self.scale = self.rect_size / ceil_size
                self.shift = (self.rect_pos - floor_pos) / ceil_size
                return self.clip_and_transform_vertex(self.rect_pos self.rect_size)
            }
            pixel: fn() {
                return self.image.sample(self.pos * self.scale + self.shift) * self.alpha
            }
        }
    }

    /** One overlay row on the sheet: bare, inverted while current, a grey
        wash under the pointer. The shell registers the click (rows live
        in a PortalList, whose item areas go stale mid-gesture — CR-002's
        fifth defect), so this is presentation only. The bg is on its own
        shader — a stock-shader quad merges into a call that paints under
        the wash (CR-002's sixth defect). */
    mod.widgets.OverlayCard = View {
        width: Fill, height: 40
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{left: 16, right: 16}
        show_bg: true
        draw_bg +: {
            color: #ffffff00
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
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
        // Three cards rather than one recoloured: a DrawQuad's shader
        // vars are not struct fields, so a quad's colour cannot be set at
        // draw time. Exactly one of these draws; label colours are
        // painted per draw (a Label's draw_text.color IS reachable), so
        // only the background needs the twins.
        card := mod.widgets.OverlayCard {}
        card_hover := mod.widgets.OverlayCard {
            visible: false
            draw_bg +: { color: #efefef }
        }
        card_inv := mod.widgets.OverlayCard {
            visible: false
            draw_bg +: { color: #141414 }
        }
    }

    /** The overlay chassis: a column of rows on the shell's sheet, faded
        as one surface. Workspaces and history use it bare; the launcher
        puts a field on top. */
    mod.widgets.RowsOverlay = set_type_default() do #(RowsOverlay::register_widget(vm)) {
        ..mod.widgets.FadeView
        width: Fill, height: Fill
        flow: Down
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            row := mod.widgets.OverlayRow {}
        }
    }

    /** The launcher: one field over the hits. The field is a real
        `SField`, so the query has a caret, selection, and the platform
        IME — the char grid drew a rectangle and tracked an index. Its own
        frame is off: the sheet is the frame, and an ink rule parts the
        query from the hits. */
    mod.widgets.LauncherOverlay = set_type_default() do #(LauncherOverlay::register_widget(vm)) {
        ..mod.widgets.FadeView
        width: Fill, height: Fill
        flow: Down
        query_input := mod.widgets.SField {
            width: Fill
            empty_text: "search panels, mail, people…"
            return_key_type: ReturnKeyType.Go
            autocapitalize: AutoCapitalize.None
            autocorrect: AutoCorrect.Disabled
            padding: Inset{left: 16, right: 16, top: 15, bottom: 15}
            draw_bg +: {
                border_size: 0.0
                border_color: #00000000
                border_color_hover: #00000000
                border_color_focus: #00000000
                border_color_down: #00000000
                border_color_empty: #00000000
                border_color_disabled: #00000000
            }
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 13.0} }
        }
        query_rule := View {
            width: Fill, height: 1
            show_bg: true
            draw_bg +: {
                color: #141414
                pixel: fn() {
                    return vec4(self.color.xyz * self.color.w, self.color.w)
                }
            }
        }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            row := mod.widgets.OverlayRow {}
        }
        empty_row := View {
            visible: false
            width: Fill, height: 40
            flow: Right
            align: Align{y: 0.5}
            padding: Inset{left: 16, right: 16}
            mod.widgets.SLabel {
                text: "nothing matches"
                draw_text +: { color: #909090 }
            }
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
        // The remove button's clicks resolve through the shell's semantic
        // rect (list-item areas go stale mid-gesture); `account_id` stays
        // for the settings tab ring.
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl AccountRowRef {
    pub fn populate(&self, cx: &mut Cx, a: &mail::Account) {
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
// Suggest — the autocomplete any field hangs under itself
// ---------------------------------------------------------------------------

/// The eight suggestion slots of a `SuggestBox`, by name.
const SUGGEST_SLOTS: [LiveId; richtable::MAX_SUGGESTIONS] = [
    live_id!(s0),
    live_id!(s1),
    live_id!(s2),
    live_id!(s3),
    live_id!(s4),
    live_id!(s5),
    live_id!(s6),
    live_id!(s7),
];

/// A field's autocomplete (CR-006): the offer under the caret, where the
/// highlight is, and a dismissal that holds until the caret moves on.
/// Generic over a [`Completion`] — the part that differs between fields:
/// the filter's tag grammar, compose's recipient list — while the box, the
/// keys and the pick are this, once.
///
/// The panel owns the box (a `#[live] suggest: View` its DSL fills with
/// `SuggestBox`) and lends it per call, and it draws the box **last**, at
/// an absolute rect hung under the field, so the box covers what follows
/// the field instead of pushing it. The completion is lent per call too,
/// because for the inbox it is the table the panel already holds.
pub struct Suggest<C: Completion> {
    ctx: Option<C::Ctx>,
    items: Vec<Suggestion>,
    sel: usize,
    dismissed: Option<C::Ctx>,
}

impl<C: Completion> Default for Suggest<C> {
    fn default() -> Self {
        Suggest {
            ctx: None,
            items: Vec::new(),
            sel: 0,
            dismissed: None,
        }
    }
}

impl<C: Completion> Suggest<C> {
    /// Whether the box is up: a context with an offer, not put away.
    pub fn open(&self) -> bool {
        self.ctx.is_some() && self.dismissed != self.ctx && !self.items.is_empty()
    }

    /// The keys the box owns while it is open and its field holds the
    /// keyboard: the arrows walk the offer, enter and tab take it, esc puts
    /// it away. `true` when the key was one of them — the field must not
    /// see it (a swallowed enter is the point), and the panel redraws.
    pub fn key(&mut self, cx: &mut Cx, c: &C, field: &TextInputRef, k: &KeyEvent) -> bool {
        if !self.open() || !field.key_focus(cx) {
            return false;
        }
        match k.key_code {
            KeyCode::ArrowDown => self.sel = (self.sel + 1).min(self.items.len() - 1),
            KeyCode::ArrowUp => self.sel = self.sel.saturating_sub(1),
            KeyCode::ReturnKey | KeyCode::NumpadEnter | KeyCode::Tab => {
                self.pick(cx, c, field, self.sel);
            }
            KeyCode::Escape => self.dismissed = self.ctx.clone(),
            _ => return false,
        }
        true
    }

    /// Commits suggestion `i`: splices it over what the caret was typing,
    /// parks the caret after it and keeps the field's focus, so a picked
    /// `@from:` opens its values without another keystroke.
    pub fn pick(&mut self, cx: &mut Cx, c: &C, field: &TextInputRef, i: usize) {
        let (Some(ctx), Some(item)) = (self.ctx.as_ref(), self.items.get(i)) else {
            return;
        };
        let text = field.text();
        let (line, at) = c.splice(&text, field.cursor().index, ctx, item);
        field.set_text(cx, &line);
        field.set_cursor(
            cx,
            Cursor {
                index: at,
                prefer_next_row: false,
            },
            false,
        );
        field.set_key_focus(cx);
        self.dismissed = None;
    }

    /// Re-derives the offer from the caret — while the field holds the
    /// keyboard; a blurred field offers nothing — fills the slots and draws
    /// the box under the field. Call it after the rest of the panel has
    /// drawn, so the box lands in a draw call over what it covers.
    pub fn draw(
        &mut self,
        cx: &mut Cx2d,
        scope: &mut Scope,
        store: &Store,
        c: &C,
        field: &TextInputRef,
        view: &mut View,
    ) {
        let ctx = if field.key_focus(cx) {
            c.context(&field.text(), field.cursor().index)
        } else {
            None
        };
        if ctx != self.ctx {
            self.items = ctx.as_ref().map(|x| c.offer(store, x)).unwrap_or_default();
            self.ctx = ctx;
            self.sel = 0;
            if self.dismissed != self.ctx {
                self.dismissed = None;
            }
        }
        let open = self.open();
        view.set_visible(cx, open);
        if !open {
            return;
        }
        let (ink, bg, dim, dim_inv) = (
            vec4(0.078, 0.078, 0.078, 1.0),
            vec4(1.0, 1.0, 1.0, 1.0),
            vec4(0.565, 0.565, 0.565, 1.0),
            vec4(0.75, 0.75, 0.75, 1.0),
        );
        for (i, slot) in SUGGEST_SLOTS.iter().enumerate() {
            let row = view.view(cx, &[*slot]);
            let Some(it) = self.items.get(i) else {
                row.set_visible(cx, false);
                continue;
            };
            row.set_visible(cx, true);
            let selected = i == self.sel;
            for (line, on, fg, fg_dim) in [
                (live_id!(line), !selected, ink, dim),
                (live_id!(line_sel), selected, bg, dim_inv),
            ] {
                view.view(cx, &[*slot, line]).set_visible(cx, on);
                if !on {
                    continue;
                }
                let lbl = view.label(cx, &[*slot, line, live_id!(lbl)]);
                lbl.set_text(cx, &it.label);
                lbl.set_text_color(cx, fg);
                let desc = view.label(cx, &[*slot, line, live_id!(desc)]);
                desc.set_text(cx, &it.describe);
                desc.set_visible(cx, !it.describe.is_empty());
                desc.set_text_color(cx, fg_dim);
            }
        }
        let fr = field.area().rect(cx);
        if fr.size.x <= 0.0 {
            return;
        }
        view.draw_walk_all(
            cx,
            scope,
            Walk {
                abs_pos: Some(dvec2(fr.pos.x, fr.pos.y + fr.size.y + 2.0)),
                width: Size::Fixed(fr.size.x),
                ..Walk::fit()
            },
        );
    }

    /// The open box's rows, `(label, rect)`, for the shell's hit table — a
    /// click on one is a [`Suggest::pick`].
    pub fn hits(&self, cx: &mut Cx, view: &View) -> Vec<(String, Rect)> {
        if !self.open() {
            return Vec::new();
        }
        self.items
            .iter()
            .zip(SUGGEST_SLOTS.iter())
            .map(|(it, slot)| (it.label.clone(), view.view(cx, &[*slot]).area().rect(cx)))
            .filter(|(_, r)| r.size.x > 0.0)
            .collect()
    }
}

/// The store a panel widget reads while drawing, off the scope.
fn panel_store(scope: &Scope) -> Option<std::rc::Rc<Store>> {
    scope.props.get::<PanelProps>().map(|p| p.store.clone())
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
    /// The TO field's autocomplete box, drawn over the fields under it
    /// after everything else.
    #[live]
    suggest: View,
    /// The offer under the TO caret: the senders the store knows.
    #[rust]
    ac: Suggest<mail::Recipients>,
}

impl ComposePanel {
    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 3] {
        [
            self.view.text_input(cx, ids!(to_input)),
            self.view.text_input(cx, ids!(subject_input)),
            self.view.text_input(cx, ids!(body_input)),
        ]
    }

    /// The fields changed — hands them to the shell, which persists the
    /// draft.
    fn draft_edited(&self, cx: &mut Cx, pid: u64) {
        let [to, subject, body] = self.inputs(cx);
        cx.action(PanelAction::DraftEdited {
            pid,
            to: to.text(),
            subject: subject.text(),
            body: body.text(),
        });
    }
}

impl Widget for ComposePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
        // The TO field's autocomplete owns the arrows, enter, tab and esc
        // while it is open (see `Suggest`); neither the field nor the tab
        // ring sees them. A pick is an edit like any typing.
        if let Event::KeyDown(k) = event {
            let to = self.view.text_input(cx, ids!(to_input));
            let before = to.text();
            if self.ac.key(cx, &mail::Recipients, &to, k) {
                if to.text() != before {
                    self.draft_edited(cx, pid);
                }
                self.redraw(cx);
                return;
            }
        }
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
                self.draft_edited(cx, pid);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk_all(cx, scope, walk);
        // The TO field's offer, over the subject and the body.
        if let Some(store) = panel_store(scope) {
            let to = self.view.text_input(cx, ids!(to_input));
            self.ac
                .draw(cx, scope, &store, &mail::Recipients, &to, &mut self.suggest);
        }
        DrawStep::done()
    }
}

impl ComposePanelRef {
    /// The open autocomplete's rows, `(label, rect)`, for the shell's hit
    /// table — a click on one is [`ComposePanelRef::pick`].
    pub fn suggestion_hits(&self, cx: &mut Cx) -> Vec<(String, Rect)> {
        self.borrow()
            .map_or_else(Vec::new, |p| p.ac.hits(cx, &p.suggest))
    }

    /// Commits the `i`-th address on offer in the TO field; the draft
    /// follows, as for any edit.
    pub fn pick(&self, cx: &mut Cx, pid: u64, i: usize) {
        let Some(mut p) = self.borrow_mut() else { return };
        let p = &mut *p;
        let to = p.view.text_input(cx, ids!(to_input));
        p.ac.pick(cx, &mail::Recipients, &to, i);
        p.draft_edited(cx, pid);
    }

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
    pub fn populate(&self, cx: &mut Cx, m: &mail::ThreadHead) {
        let Some(inner) = self.borrow() else { return };
        let from = m.who_line();
        let fp = inner.view.label(cx, ids!(from_lbl));
        let fb = inner.view.label(cx, ids!(from_b));
        let sp = inner.view.label(cx, ids!(subject_lbl));
        let sb = inner.view.label(cx, ids!(subject_b));
        fp.set_text(cx, if m.unread { "" } else { &from });
        fb.set_text(cx, if m.unread { &from } else { "" });
        fp.set_visible(cx, !m.unread);
        fb.set_visible(cx, m.unread);
        sp.set_text(cx, if m.unread { "" } else { &m.topic });
        sb.set_text(cx, if m.unread { &m.topic } else { "" });
        sp.set_visible(cx, !m.unread);
        sb.set_visible(cx, m.unread);
        inner
            .view
            .label(cx, ids!(date_lbl))
            .set_text(cx, &mail::fmt_date(m.last));
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
    pub fn populate(&self, cx: &mut Cx, m: &mail::ThreadHead, selected: bool) {
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

/// The inbox's table: the shared engine over the thread datasource.
type InboxTable = Table<&'static SqlSource<mail::ThreadHead>>;

#[derive(Script, ScriptHook, Widget)]
pub struct InboxPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The autocomplete box, drawn over the rows after everything else.
    #[live]
    suggest: View,
    /// The rich table (CR-006): the filter and the paging window. It holds
    /// no rows — every row a draw needs is a page lookup in the store.
    #[rust(Table::new(&mail::THREADS, mail::INBOX_PAGE))]
    table: InboxTable,
    /// The cursor: which mail, and the row it sat on. The index is the
    /// fallback — a mail archived out from under the cursor is no longer in
    /// the table, and without it the walk would resolve to nothing and snap
    /// back to the top of the inbox instead of carrying on where it stood.
    #[rust]
    sel: Option<(i64, usize)>,
    /// What each live row was last populated with, by index: a draw
    /// repopulates only the rows whose thread or selection changed, so
    /// scrolling a long list costs its new rows and nothing else.
    #[rust]
    stamps: HashMap<usize, (mail::ThreadHead, bool)>,
    /// The filter's autocomplete: the table is its completion — tag
    /// names, then a tag's values.
    #[rust]
    ac: Suggest<InboxTable>,
}

impl InboxPanel {
    fn store(scope: &Scope) -> Option<std::rc::Rc<Store>> {
        panel_store(scope)
    }

    /// Hands the field's text to the table. The field is the one source of
    /// the filter — a pick, the shell's seed of a baked param and typing
    /// all land there — so the table follows it rather than the events.
    fn sync_filter(&mut self, cx: &mut Cx) {
        let text = self.view.text_input(cx, ids!(filter_input)).text();
        if self.table.set_filter(&text) {
            self.sel = None;
        }
    }

    /// Where the cursor stands now: the remembered row if it still holds
    /// the thread, else the thread's rank (a sync landed above it), else
    /// the row clamped into the table (the thread left; carry on from
    /// there). The cursor's identity is the thread anchor (CR-007): which
    /// mail a row opens can change under it as replies arrive.
    fn cursor_index(&self, store: &Store) -> Option<usize> {
        let (th, idx) = self.sel?;
        if self.table.row(store, idx).is_some_and(|t| t.thread == th) {
            return Some(idx);
        }
        if let Some(i) = self.index_of_thread(store, th) {
            return Some(i);
        }
        let n = self.table.len(store);
        (n > 0).then(|| idx.min(n - 1))
    }

    /// A thread's row: a live row first (it is usually on screen), else its
    /// rank in the table. The anchor is a mail id, so the row is re-derived
    /// from it exactly as from any of its mails.
    fn index_of_thread(&self, store: &Store, th: i64) -> Option<usize> {
        if let Some((i, _)) = self.stamps.iter().find(|(_, (t, _))| t.thread == th) {
            if self.table.row(store, *i).is_some_and(|t| t.thread == th) {
                return Some(*i);
            }
        }
        let head = mail::thread_head(store, th)?;
        self.table.index_of(store, &head)
    }

    /// A mail's row: the row of the thread it belongs to.
    fn index_of_id(&self, store: &Store, id: i64) -> Option<usize> {
        self.index_of_thread(store, mail::thread_of(store, id)?)
    }

    /// Puts the cursor on row `i` and previews what it lands on — every
    /// cursor move goes through here, so walking and previewing can never
    /// disagree.
    fn set_sel(&mut self, cx: &mut Cx, pid: u64, store: &Store, i: usize) {
        let Some(m) = self.table.row(store, i) else { return };
        self.sel = Some((m.thread, i));
        // Keep the cursor on screen: a row without a live item is off-view.
        let list = self.view.widget(cx, ids!(list)).as_portal_list();
        let visible = list
            .borrow()
            .is_some_and(|l| l.items().iter().any(|(idx, _)| *idx == i));
        if !visible {
            list.smooth_scroll_to(cx, i, 90.0, None, 0.0);
        }
        cx.action(PanelAction::PreviewMail { pid, id: m.target });
        self.redraw(cx);
    }

    fn move_sel(&mut self, cx: &mut Cx, store: &Store, pid: u64, d: isize) {
        let n = self.table.len(store);
        if n == 0 {
            return;
        }
        let i = match self.cursor_index(store) {
            Some(i) => (i as isize + d).clamp(0, n as isize - 1) as usize,
            None => 0,
        };
        self.set_sel(cx, pid, store, i);
    }
}

impl InboxPanelRef {
    /// The thread under the cursor, if any — the shell asks so it can carry
    /// the cursor forward when that thread is filed away.
    pub fn selected_thread(&self) -> Option<i64> {
        self.borrow().and_then(|p| p.sel).map(|(th, _)| th)
    }

    /// Whether the filter owns the keyboard. The fifth accelerator rule
    /// (CR-005) stands the borrowed chords down while it does, so `cmd+a`
    /// stays select-all in a live field.
    pub fn filter_focused(&self, cx: &mut Cx) -> bool {
        self.borrow()
            .is_some_and(|p| p.view.text_input(cx, ids!(filter_input)).key_focus(cx))
    }

    /// Row `i` of the table as this panel has it — its own filter included.
    pub fn row_at(&self, store: &Store, i: usize) -> Option<mail::ThreadHead> {
        self.borrow().and_then(|p| p.table.row(store, i))
    }

    /// The mail a cursor standing on `id`'s thread should land on once that
    /// thread is filed away: the next row's, or the one above if it was the
    /// last.
    pub fn neighbour_of(&self, store: &Store, id: i64) -> Option<i64> {
        let p = self.borrow()?;
        let i = p.index_of_id(store, id)?;
        p.table
            .row(store, i + 1)
            .or_else(|| i.checked_sub(1).and_then(|j| p.table.row(store, j)))
            .map(|t| t.target)
    }

    /// The open autocomplete's rows, `(label, rect)`, for the shell's hit
    /// table — a click on one is [`InboxPanelRef::pick`].
    pub fn suggestion_hits(&self, cx: &mut Cx) -> Vec<(String, Rect)> {
        self.borrow()
            .map_or_else(Vec::new, |p| p.ac.hits(cx, &p.suggest))
    }

    /// Commits the `i`-th suggestion on offer.
    pub fn pick(&self, cx: &mut Cx, i: usize) {
        let Some(mut p) = self.borrow_mut() else { return };
        let p = &mut *p;
        let filter = p.view.text_input(cx, ids!(filter_input));
        p.ac.pick(cx, &p.table, &filter, i);
    }
}

impl Widget for InboxPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let filter = self.view.text_input(cx, ids!(filter_input));
        let filter_focused = filter.key_focus(cx);
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);

        // The autocomplete owns the arrows, enter, tab and esc while it is
        // open (see `Suggest`); the field never sees them — a swallowed
        // enter is the point.
        if let Event::KeyDown(k) = event {
            if self.ac.key(cx, &self.table, &filter, k) {
                self.redraw(cx);
                return;
            }
        }
        self.view.handle_event(cx, event, scope);
        let Some(store) = Self::store(scope) else { return };

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
                        let target = self
                            .cursor_index(&store)
                            .or(Some(0))
                            .and_then(|i| self.table.row(&store, i))
                            .map(|t| t.target);
                        if let Some(id) = target {
                            // Enter *goes*: unlike the walk's preview, it
                            // hands focus to the mail (the solid-link rule).
                            cx.action(PanelAction::OpenMail {
                                pid,
                                id,
                                fresh: k.modifiers.logo || k.modifiers.alt,
                            });
                        }
                    }
                    // The row walk, with scroll-follow (CR-003: the arrows
                    // are the whole walk now, j/k having gone). Each step
                    // previews what it lands on and keeps the keyboard.
                    KeyCode::ArrowDown => self.move_sel(cx, &store, pid, 1),
                    KeyCode::ArrowUp => self.move_sel(cx, &store, pid, -1),
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
                    self.sync_filter(cx);
                    self.set_sel(cx, pid, &store, 0);
                }
                self.redraw(cx);
            }
            if filter.changed(actions).is_some() {
                self.sel = None;
                self.redraw(cx);
            }
            // The end of the list came on screen: a source without a count
            // loads its next page here (the inbox counts, so this is its
            // no-op — the seam is what a remote table will use).
            if self.view.widget(cx, ids!(list)).as_portal_list().reached_end(actions)
                && self.table.extend(&store)
            {
                self.redraw(cx);
            }
            for a in actions {
                if let Some(PanelAction::SelectMail { pid: p, id }) =
                    a.downcast_ref::<PanelAction>()
                {
                    if *p == pid {
                        // The shell moved the cursor for us (a mail opened by
                        // click, or the walk carried past one just filed
                        // away). Take the row from the table so the index
                        // fallback stays honest.
                        if let Some(th) = mail::thread_of(&store, *id) {
                            let i = self.index_of_thread(&store, th).unwrap_or(0);
                            self.sel = Some((th, i));
                            self.redraw(cx);
                        }
                    }
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(store) = Self::store(scope) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        self.sync_filter(cx);
        let filter = self.view.text_input(cx, ids!(filter_input));
        let focused = filter.key_focus(cx);
        // What the filter could not read — minus the tag still being typed,
        // which is not wrong yet.
        let err = if focused {
            self.table.errors_while_typing().first().map(|e| e.message.clone())
        } else {
            self.table.errors().first().map(|e| e.message.clone())
        };
        let err_lbl = self.view.label(cx, ids!(err_lbl));
        err_lbl.set_text(cx, err.as_deref().unwrap_or(""));
        err_lbl.set_visible(cx, err.is_some());

        let sel = self.sel.map(|(th, _)| th);
        let n = self.table.len(&store);
        let mut live: Vec<usize> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, n);
                while let Some(idx) = list.next_visible_item(cx) {
                    let Some(m) = self.table.row(&store, idx) else { continue };
                    let (row, existed) = list.item_with_existed(cx, idx, live_id!(row));
                    let selected = sel == Some(m.thread);
                    let stamp = (m, selected);
                    if !existed || self.stamps.get(&idx) != Some(&stamp) {
                        row.as_inbox_row().populate(cx, &stamp.0, selected);
                        self.stamps.insert(idx, stamp);
                    }
                    live.push(idx);
                    row.draw_all(cx, scope);
                }
            }
        }
        self.stamps.retain(|k, _| live.contains(k));
        // The filter's offer, over the rows.
        self.ac
            .draw(cx, scope, &store, &self.table, &filter, &mut self.suggest);
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
        let key_l = l.view.label(cx, ids!(row.key));
        key_l.set_text(cx, &key);
        key_l.set_visible(cx, !key.is_empty());
        l.view.view(cx, ids!(ul)).set_visible(cx, !dotted);
        l.view.view(cx, ids!(ul_dotted)).set_visible(cx, dotted);
    }
}

// ---------------------------------------------------------------------------
// ThreadMsg
// ---------------------------------------------------------------------------

/// A touchable part of a thread row, for the shell's hit table (CR-007):
/// the header (a toggle), the contact link and the readings while open,
/// the quote fold while it is folded.
pub struct MsgHit {
    pub id: i64,
    pub open: bool,
    /// The sender as the row names them.
    pub name: String,
    pub email: String,
    pub date: String,
    /// The line a closed row shows: the first line written, or the status.
    pub preview: String,
    pub head: Rect,
    pub link: Option<Rect>,
    pub quote: Option<Rect>,
    pub text: Option<Rect>,
    pub html: Option<Rect>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ThreadMsg {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    id: i64,
    #[rust]
    open: bool,
    #[rust]
    has_quote: bool,
    #[rust]
    quoted: bool,
    #[rust]
    is_html: bool,
    #[rust]
    name: String,
    #[rust]
    email: String,
    #[rust]
    date: String,
    #[rust]
    preview: String,
}

impl Widget for ThreadMsg {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // Touches resolve through the shell's registered rects, as inbox
        // rows do — a list item's own area goes stale on any mid-gesture
        // redraw. The row's share is the cursor.
        if let Hit::FingerHoverIn(_) = event.hits(cx, self.view.view(cx, ids!(head)).area()) {
            cx.set_cursor(MouseCursor::Hand);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ThreadMsgRef {
    pub fn populate(&self, cx: &mut Cx, pid: u64, t: &mail::ThreadMail, open: bool, quoted: bool) {
        let Some(mut w) = self.borrow_mut() else { return };
        let m = &t.mail;
        let name = if m.head.from_name.is_empty() {
            m.head.from_email.clone()
        } else {
            m.head.from_name.clone()
        };
        w.id = m.head.id;
        w.open = open;
        w.quoted = quoted;
        w.is_html = m.html.is_some();
        w.name = name.clone();
        w.email = m.head.from_email.clone();
        w.date = mail::fmt_date(m.head.date);
        let (preview, err) = match &m.status {
            Some((s, e)) => (s.clone(), *e),
            None => (
                mail::own_text(m)
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .to_string(),
                false,
            ),
        };
        let (own_text, own_html, quote): (String, String, Option<String>) = if !open {
            (String::new(), String::new(), None)
        } else if let Some(h) = &m.html {
            let (own, q) = mail::split_quote_html(h);
            (String::new(), own, q)
        } else {
            let (own, q) = mail::split_quote(&m.body);
            (own, String::new(), q)
        };
        w.preview = preview.clone();
        w.has_quote = quote.is_some();
        let v = &w.view;

        // The header row: name, preview and date closed; the link and the
        // date open. Same row, same height, so it toggles in place.
        let nl = v.label(cx, ids!(name_lbl));
        nl.set_text(cx, &name);
        nl.set_visible(cx, !open);
        let link = v.link(cx, ids!(from_link));
        link.set(
            cx,
            pid,
            &format!("{name} <{}>", m.head.from_email),
            crate::core::Kind::Contact {
                email: m.head.from_email.clone(),
            },
            false,
        );
        link.set_visible(cx, open);
        let pl = v.label(cx, ids!(preview_lbl));
        let pe = v.label(cx, ids!(preview_err));
        pl.set_text(cx, if err { "" } else { &preview });
        pl.set_visible(cx, !err);
        pe.set_text(cx, if err { &preview } else { "" });
        pe.set_visible(cx, err);
        v.view(cx, ids!(preview_wrap)).set_visible(cx, !open);
        v.view(cx, ids!(spacer)).set_visible(cx, open);
        v.label(cx, ids!(date_lbl)).set_text(cx, &w.date);

        // The letter, while open. Both readings are written every time —
        // the hidden one emptied rather than merely hidden, so no mail
        // can leave its text behind for the next one to show.
        v.view(cx, ids!(body)).set_visible(cx, open);
        let (ok_l, err_l) = (v.label(cx, ids!(status_lbl)), v.label(cx, ids!(status_err_lbl)));
        match (&m.status, open) {
            (Some((txt, true)), true) => {
                err_l.set_text(cx, txt);
                err_l.set_visible(cx, true);
                ok_l.set_visible(cx, false);
            }
            (Some((txt, false)), true) => {
                ok_l.set_text(cx, txt);
                ok_l.set_visible(cx, true);
                err_l.set_visible(cx, false);
            }
            _ => {
                ok_l.set_visible(cx, false);
                err_l.set_visible(cx, false);
            }
        }
        let is_html = open && m.html.is_some();
        // Guarded on the way in, not merely on the way into the store:
        // rows narrowed by an older build are still out there, and one the
        // parser cannot read takes the whole app down every frame.
        v.text_input(cx, ids!(body_lbl)).set_text(cx, &own_text);
        v.html(cx, ids!(body_html))
            .set_text(cx, &crate::html::guard(&own_html));
        v.view(cx, ids!(text_wrap)).set_visible(cx, open && !is_html);
        v.view(cx, ids!(html_wrap)).set_visible(cx, is_html);
        let show_quote = quote.is_some() && quoted;
        v.view(cx, ids!(quote_fold)).set_visible(cx, quote.is_some() && !quoted);
        let q = quote.unwrap_or_default();
        v.text_input(cx, ids!(quote_lbl))
            .set_text(cx, if show_quote && !is_html { &q } else { "" });
        v.html(cx, ids!(quote_body)).set_text(
            cx,
            &crate::html::guard(if show_quote && is_html { &q } else { "" }),
        );
        v.view(cx, ids!(quote_text)).set_visible(cx, show_quote && !is_html);
        v.view(cx, ids!(quote_html)).set_visible(cx, show_quote && is_html);
    }

    /// The row's touchable parts, for the shell's hit table. Rects of
    /// hidden parts are stale, so each one is gated on the state that
    /// drew it.
    pub fn hits(&self, cx: &mut Cx) -> Option<MsgHit> {
        let w = self.borrow()?;
        let rect = |path: &[LiveId]| {
            let r = w.view.widget(cx, path).area().rect(cx);
            (r.size.x > 0.0 && r.size.y > 0.0).then_some(r)
        };
        let head = rect(ids!(head))?;
        let open = w.open;
        Some(MsgHit {
            id: w.id,
            open,
            name: w.name.clone(),
            email: w.email.clone(),
            date: w.date.clone(),
            preview: w.preview.clone(),
            head,
            link: if open { rect(ids!(from_link)) } else { None },
            quote: if open && w.has_quote && !w.quoted { rect(ids!(quote_fold)) } else { None },
            text: if open && !w.is_html { rect(ids!(body_lbl)) } else { None },
            html: if open && w.is_html { rect(ids!(body_html)) } else { None },
        })
    }
}

// ---------------------------------------------------------------------------
// MessagePanel
// ---------------------------------------------------------------------------

/// What a live thread row was last populated with: which mail, open or
/// not, quote unfolded or not, and enough of the mail to notice it changed
/// under the row (a body that arrived, a status that landed).
type MsgStamp = (i64, bool, bool, usize, Option<usize>, Option<(String, bool)>, bool);

#[derive(Script, ScriptHook, Widget)]
pub struct MessagePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    stamps: HashMap<usize, MsgStamp>,
    /// `(mail, seeded for)` the list was last scrolled for: a panel opens
    /// with the mail it opened on at the top, once per seeding.
    #[rust]
    scrolled_for: Option<(i64, i64)>,
}

impl Widget for MessagePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // Arrows scroll the thread three lines — synthesized as a Scroll
        // event over the list, so it keeps clamping and position itself,
        // no shadow state.
        if let Event::KeyDown(k) = event {
            let d = match k.key_code {
                KeyCode::ArrowDown => 3.0,
                KeyCode::ArrowUp => -3.0,
                _ => 0.0,
            };
            if d != 0.0 {
                let r = self.view.widget(cx, ids!(list)).area().rect(cx);
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
        // The message panel's link accelerator (CR-003): reply is cmd+r,
        // drawn onto the link itself. The shell forwards any cmd chord it
        // does not own itself. Reply answers the newest mail of the
        // thread — the conventional reply to a conversation.
        if let Event::KeyDown(k) = event {
            if !k.modifiers.logo || k.key_code != KeyCode::KeyR {
                return;
            }
            let Some(p) = scope.props.get::<PanelProps>() else {
                return;
            };
            let crate::core::Kind::Message { id } = p.kind else {
                return;
            };
            let re = mail::thread(&p.store, id)
                .last()
                .map_or(id, |t| t.mail.head.id);
            cx.action(PanelAction::FollowLink {
                pid: p.pid,
                target: crate::core::Kind::Compose { re },
                dotted: false,
                fresh: false,
            });
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(p) = scope.props.get::<PanelProps>() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let crate::core::Kind::Message { id } = p.kind else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let pid = p.pid;
        let msgs = mail::thread(&p.store, id);
        let expand = Expansion::for_panel(p.expand.as_ref(), id);
        if let Some(first) = msgs.first() {
            self.view
                .text_input(cx, ids!(to_lbl))
                .set_text(cx, &first.mail.to);
        }
        let newest = msgs.last().map_or(id, |t| t.mail.head.id);
        self.view.link(cx, ids!(reply_link)).set_accel(
            cx,
            pid,
            "reply",
            crate::core::Kind::Compose { re: newest },
            false,
            Some(ui::ACCEL_REPLY),
        );
        // Open on the mail the row pointed at: once per seeding, so a
        // touch that opens an older message is not scrolled away from.
        let seed = (id, expand.for_mail);
        if self.scrolled_for != Some(seed) {
            if let Some(i) = msgs.iter().position(|t| t.mail.head.id == id) {
                self.view
                    .widget(cx, ids!(list))
                    .as_portal_list()
                    .set_first_id(i);
            }
            self.scrolled_for = Some(seed);
        }
        let n = msgs.len();
        let mut live: Vec<usize> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, n);
                while let Some(idx) = list.next_visible_item(cx) {
                    let Some(t) = msgs.get(idx) else { continue };
                    let (row, existed) = list.item_with_existed(cx, idx, live_id!(msg));
                    let mid = t.mail.head.id;
                    let open = expand.open.contains(&mid);
                    let quoted = expand.quotes.contains(&mid);
                    let stamp: MsgStamp = (
                        mid,
                        open,
                        quoted,
                        t.mail.body.len(),
                        t.mail.html.as_ref().map(String::len),
                        t.mail.status.clone(),
                        t.mail.head.unread,
                    );
                    if !existed || self.stamps.get(&idx) != Some(&stamp) {
                        row.as_thread_msg().populate(cx, pid, t, open, quoted);
                        self.stamps.insert(idx, stamp);
                    }
                    live.push(idx);
                    row.draw_all(cx, scope);
                }
            }
        }
        self.stamps.retain(|k, _| live.contains(k));
        DrawStep::done()
    }
}

impl MessagePanelRef {
    /// The live rows' touchable parts, for the shell's hit table.
    pub fn msg_hits(&self, cx: &mut Cx) -> Vec<MsgHit> {
        let Some(p) = self.borrow() else { return Vec::new() };
        let mut out = Vec::new();
        if let Some(list) = p.view.widget(cx, ids!(list)).as_portal_list().borrow() {
            for (_, item) in list.items().iter() {
                if let Some(h) = item.widget.as_thread_msg().hits(cx) {
                    out.push(h);
                }
            }
        }
        out
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
    pub fn populate(&self, cx: &mut Cx, d: &OverlayRowData) {
        let Some(row) = self.borrow() else { return };
        // Inverted while current; an undone branch stays legible but quiet.
        let (fg, dim) = if d.current {
            (vec4(1.0, 1.0, 1.0, 1.0), vec4(0.75, 0.75, 0.75, 1.0))
        } else if d.muted {
            (vec4(0.565, 0.565, 0.565, 1.0), vec4(0.72, 0.72, 0.72, 1.0))
        } else {
            (vec4(0.078, 0.078, 0.078, 1.0), vec4(0.353, 0.353, 0.353, 1.0))
        };
        // One card draws; the others stand down. A quad's shader vars are
        // not struct fields (no runtime colour), but a Label's draw_text
        // colour is — so the twins are only for the background.
        let hovered = d.hovered && !d.current;
        row.view
            .view(cx, ids!(card))
            .set_visible(cx, !d.current && !hovered);
        row.view.view(cx, ids!(card_hover)).set_visible(cx, hovered);
        row.view.view(cx, ids!(card_inv)).set_visible(cx, d.current);
        let c = if d.current {
            ids!(card_inv)
        } else if hovered {
            ids!(card_hover)
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
        let (rows, alpha) = scope
            .props
            .get::<OverlayProps>()
            .map(|p| (p.rows.clone(), p.alpha))
            .unwrap_or((Vec::new(), 1.0));
        // The subtree renders to a texture; this is the alpha it lands at.
        self.view
            .draw_bg
            .set_uniform(cx, live_id!(alpha), &[alpha]);
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
        let (rows, query, alpha) = scope
            .props
            .get::<OverlayProps>()
            .map(|p| (p.rows.clone(), p.query.clone(), p.alpha))
            .unwrap_or((Vec::new(), String::new(), 1.0));
        // The subtree renders to a texture; this is the alpha it lands at.
        self.view
            .draw_bg
            .set_uniform(cx, live_id!(alpha), &[alpha]);
        // A query nothing answers says so, instead of an empty sheet.
        self.view
            .view(cx, ids!(empty_row))
            .set_visible(cx, rows.is_empty() && !query.is_empty());
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
