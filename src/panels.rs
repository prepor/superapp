//! CR-002: retained panel content. The semantic widget library — makepad
//! primitives wrapped once and themed to the design language — and the
//! per-kind panel widgets composed from it (Robrix's patterns; same
//! script_mod generation).
//!
//! Data flows in per draw via [`PanelProps`] on the scope; intent flows out
//! as [`PanelAction`]s (global actions the shell catches and turns into
//! store actions — so undo semantics never enter this module).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use makepad_widgets::makepad_platform::event::{ScrollEvent, ScrollPhase};
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::image_cache::{
    looks_like_svg, process_async_image_load, AsyncImageLoad, AsyncLoadResult, ImageCacheImpl,
};
use makepad_widgets::*;

use crate::core::Seed;
use crate::effect::{self, Job};
use crate::mail;
use crate::richtable::{self, Completion, Marks, SqlSource, Suggestion, Table};
use crate::files;
use crate::store::Store;
use crate::ui;

/// What a panel widget may read while drawing: the store and its own
/// panel identity. Passed through `Scope` props each draw (props ride an
/// `Any`, hence the `Rc` — scope wants `'static`).
pub struct PanelProps {
    pub store: std::rc::Rc<Store>,
    /// The effect registry, for the one thing a panel does with it: turn a
    /// filed payload back into the sentence the effect describes itself
    /// with (the log panel). Performing anything needs an
    /// [`Outside`](crate::effect::Outside), which stays behind the world.
    pub registry: std::rc::Rc<effect::Registry>,
    /// The world, for the one kind that reads outside the store during
    /// draw: a files panel lists its directory through the outside
    /// (CR-008).
    pub world: std::rc::Rc<crate::effect::World>,
    pub pid: u64,
    pub kind: crate::core::Kind,
    /// Which messages of its thread a message panel shows open (CR-007).
    /// Panel context, owned by the shell; `None` for every other kind.
    pub expand: Option<Expansion>,
    /// The standing problems — the problems panel's rows. Derived by the
    /// shell, since device sync's entry lives outside the store; empty for
    /// every other kind.
    pub problems: std::rc::Rc<Vec<crate::problems::Problem>>,
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
    /// Sign in to Gmail: the shell opens the browser, waits for the
    /// loopback redirect on a thread of its own, and adds the account when
    /// it lands (see [`crate::oauth`]).
    GoogleSignIn {
        /// The add-account panel that asked — where the flow reports back.
        pid: u64,
    },
    /// The device-sync form submitted (CR-005): point this device at a
    /// bucket. An empty secret keeps whatever key the device already holds.
    ConnectBucket {
        /// The bucket panel that submitted (its secret field clears).
        pid: u64,
        url: String,
        key_id: String,
        secret: String,
    },
    /// A compose panel's fields changed — the shell persists the draft
    /// (plain upkeep, not an action).
    DraftEdited {
        pid: u64,
        to: String,
        subject: String,
        body: String,
    },
    /// **The list verbs.** Every list panel — the inbox over threads, the
    /// effect log over jobs, a files panel over entries — says the same
    /// three things about the row under its cursor, and the shell answers
    /// each the same way whatever the domain, so there is one of each
    /// rather than one per table. What differs is only the `target`: the
    /// kind that row names.
    ///
    /// Open it — the solid-link semantics; `fresh` is the workspace
    /// modifier.
    Open {
        pid: u64,
        target: crate::core::Kind,
        fresh: bool,
    },
    /// The cursor landed on it (CR-005): open it joined but leave focus in
    /// the list, so the walk carries on. Deliberately not a flag on
    /// [`PanelAction::Open`] — a preview is never the `fresh` variant, and
    /// two bools would let that nonsense be spelled.
    Preview { pid: u64, target: crate::core::Kind },
    /// Panel-internal: put the cursor on this row. The shell raises it
    /// when it moved the cursor on a list's behalf — a row clicked, or the
    /// walk carried past what was just filed away.
    Select { pid: u64, target: crate::core::Kind },
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
    /// A problems row's *sync*: kick the account's worker.
    SyncAccount(i64),
    /// A problems row's *retry*: file the failed send again — the send
    /// action, with its window.
    RetrySend(i64),
    /// A problems row's *reopen* link: the failed send back as a compose
    /// panel joined to the right, its draft along with it.
    ReopenSend {
        pid: u64,
        outbox: i64,
        /// What the draft started from — the reopened compose gets it back.
        seed: crate::core::Seed,
        fresh: bool,
    },
    /// A files panel's `new dir` field was submitted (CR-008). The shell
    /// toasts what it would have made; the effect and its undo are open
    /// work (see the book's open questions).
    NewDir { pid: u64, dir: String, name: String },
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

    /** Body text in the mono face.

        No padding of its own: a label sits exactly where its row puts it,
        so every line a panel writes shares the panel's inset, and the
        spacing between lines belongs to the rows (`SRow`, `SRule`, a
        margin). makepad's Label ships a theme inset on every side, which
        every panel used to zero, or pad around, by hand. */
    mod.widgets.SLabel = Label {
        width: Fit, height: Fit
        padding: 0
        draw_text +: {
            color: #141414
            text_style: mod.widgets.SMonoStyle{}
        }
    }

    /** An uppercase section label (the char grid's Style::Label). Bare,
        like `SLabel`: it shares the inset of the lines under it. */
    mod.widgets.SSection = Label {
        width: Fit, height: Fit
        padding: 0
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
    /** Declared before `SHtml`, which names it: an image in a letter or an article (see `HtmlImage`): its own size,
        never wider than the column; its alt text in the muted ink until
        the bytes arrive, or for good when they never will. */
    mod.widgets.HtmlImage = set_type_default() do #(HtmlImage::register_widget(vm)) {
        width: Fit, height: Fit
        image: mod.widgets.Image { width: Fill, height: Fill }
        draw_text +: {
            text_style: mod.widgets.SMonoStyle{}
            color: #909090
        }
    }

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
        // The picture an `<img>` becomes (see `HtmlImage`).
        img := mod.widgets.HtmlImage {}

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
            // Painted over the header's text, not under it: a fill would
            // hide the words. Bold is the header's mark.
            table_header_bg_color: #0000
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
        // The three parts butt against each other: a label has no padding
        // of its own, so the split leaves no seam in the word.
        row := View {
            width: Fit, height: Fit
            flow: Right
            pre := mod.widgets.SLabel { text: "" }
            // One pass. It took three nudged copies to make a single
            // character read as bold at this size; the weight axis does it
            // properly.
            key := mod.widgets.SBoldLabel { text: "" }
            post := mod.widgets.SLabel { text: "" }
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
        // Keyboard focus on a link: the underline doubles, the way a focused
        // button wears the grey wash — visible, and still no second colour.
        ul_focus := View {
            visible: false
            width: Fill, height: 2
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

    /** One line of prose: children laid out left to right, shared
        baseline. The row carries the line's leading — the labels have
        none — so a line of text, a line with a key cap and a line with a
        button all sit on one rhythm. */
    mod.widgets.SRow = View {
        width: Fill, height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{top: 6, bottom: 6}
    }

    /** The hairline under a section label: a little air above it, and the
        first line hangs off it by its own leading. */
    mod.widgets.SRule = View {
        width: Fill, height: 1
        margin: Inset{top: 6, bottom: 4}
        show_bg: true
        draw_bg +: {
            color: #141414
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    // ---- problems ----------------------------------------------------------

    /** One standing problem: what it concerns, the error in the one colour,
        a muted detail — and what can be done. An account offers *sync* and
        a link to settings; a failed send offers *retry* (a button: it files
        the send again) and *reopen* (a link: it opens the draft). Device
        sync offers nothing: the network coming back is what fixes it.
        Every control is here; `ProblemRow::populate` shows the row's own. */
    mod.widgets.ProblemRow = set_type_default() do #(ProblemRow::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        padding: Inset{top: 6, bottom: 6}
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            label_lbl := mod.widgets.SText { width: Fit, is_multiline: false }
            View { width: Fill, height: 1 }
            sync_btn := mod.widgets.SBtn { text: "sync" }
            retry_btn := mod.widgets.SBtn { text: "retry" }
        }
        line_lbl := mod.widgets.SLabel {
            width: Fill
            margin: Inset{top: 6}
            text: "", draw_text +: { color: #a01500 }
        }
        View {
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            margin: Inset{top: 5}
            // Fill, so a long detail wraps on a phone grid rather than
            // running under the link.
            detail_lbl := mod.widgets.SLabel { width: Fill, text: "", draw_text +: { color: #909090 } }
            settings_link := mod.widgets.SLink { margin: Inset{left: 12} }
            reopen_link := mod.widgets.SLink { margin: Inset{left: 12} }
        }
        View { width: Fill, height: 8 }
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

    /** The problems panel: every standing problem as a row, or one muted
        line saying nothing is wrong. */
    mod.widgets.ProblemsPanel = set_type_default() do #(ProblemsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        mod.widgets.SSection { text: "PROBLEMS" }
        mod.widgets.SRule {}
        none_lbl := mod.widgets.SLabel {
            margin: Inset{top: 6}
            text: "nothing is wrong", draw_text +: { color: #909090 }
        }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            problem_row := mod.widgets.ProblemRow {}
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
        // The status line hangs under the address, on the same edge —
        // selectable and wrapping, both for the same reason: a sync error
        // is the one line here a human needs to *act* on, to carry to a
        // search or to read the whole of. A Label would clip it at the
        // panel's edge and refuse the drag. (`SText` is the read-only
        // TextInput the selectable content of CR-003 is made of.)
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
        status_err_lbl := mod.widgets.SText {
            visible: false
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
        mod.widgets.SRule {}

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
        View { width: Fill, height: 2 }
        bucket_link := mod.widgets.SLink {}
    }

    /** The add-account form, a panel of its own: the Google sign-in above,
        then four labelled fields and the one button, top-aligned in a
        compact panel.

        Google first because it is one press against four fields — and
        because a Gmail address typed into the form below cannot work at
        all: Google stopped accepting passwords on IMAP. */
    mod.widgets.AddAccountPanel = set_type_default() do #(AddAccountPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "GOOGLE" }
            google_btn := mod.widgets.SBtn { text: "sign in with google" }
        }
        // The one line the flow speaks through: what it is waiting for,
        // who signed in, or why it could not. Hidden until it has
        // something to say — an empty line would still take its height.
        // The 82-wide spacer is the same one the section labels are, so the
        // line starts where the fields below it do rather than at a margin
        // guessed to match.
        View { width: Fill, height: 5 }
        View {
            width: Fill, height: Fit
            View { width: 82, height: 1 }
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
        View { width: Fill, height: 12 }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #dcdcdc
            pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) } } }
        View { width: Fill, height: 12 }

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

    /** The device-sync form (CR-005): where the bucket is, and the key that
        opens it. The same three-field shape as the account form, because it
        is the same act — this is how a device that has no cable and no
        shell is given a credential. */
    mod.widgets.BucketPanel = set_type_default() do #(BucketPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "BUCKET" }
            url_input := mod.widgets.SField {
                empty_text: "https://<account>.r2.cloudflarestorage.com/<bucket>"
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "KEY ID" }
            key_input := mod.widgets.SField {
                empty_text: "access key id"
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 7 }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            mod.widgets.SSection { width: 82, text: "SECRET" }
            secret_input := mod.widgets.SField {
                is_password: true
                // Never read back, only written: an empty field on a
                // configured device means "keep the key you already have".
                empty_text: "secret access key"
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 12 }
        View {
            width: Fill, height: Fit
            View { width: Fill, height: 1 }
            connect_btn := mod.widgets.SBtn { text: "connect" }
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
        lbl := mod.widgets.SLabel { width: Fit, max_lines: 1, text: "" }
        View { width: 10, height: 1 }
        desc := mod.widgets.SLabel {
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
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
                from_b := mod.widgets.SBoldLabel {
                    visible: false
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                }
            }
            View { width: 10, height: 1 }
            date_lbl := mod.widgets.SLabel {
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        subject_lbl := mod.widgets.SLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        subject_b := mod.widgets.SBoldLabel {
            visible: false
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
        // The mark (CR-009): an ink bar down the row's left edge, inside
        // the row's own inset. Shader-drawn like the dotted underline, so
        // a mark costs no layout and the text stays on the header's
        // columns. Two more twins rather than a flag: a quad's colour is
        // not settable at draw time (see `OverlayRow`).
        line_mark := mod.widgets.InboxLine {
            visible: false
            show_bg: true
            draw_bg +: {
                color: #141414
                pixel: fn() {
                    let x = self.pos.x * self.rect_size.x
                    if x < 3.0 {
                        return vec4(self.color.xyz * self.color.w, self.color.w)
                    }
                    return vec4(0.0, 0.0, 0.0, 0.0)
                }
            }
        }
        line_mark_sel := mod.widgets.InboxLine {
            visible: false
            show_bg: true
            draw_bg +: {
                color: #e7e7e7
                pixel: fn() {
                    let x = self.pos.x * self.rect_size.x
                    if x < 3.0 {
                        return vec4(0.078, 0.078, 0.078, 1.0)
                    }
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

    // ---- marks (CR-009) ----------------------------------------------------

    /** A bordered side-effect button that wears its key: the label split
        the way `SLink` splits it, so one character draws bold. Presentation
        for now — the bar's clicks will resolve through the shell's hit
        table like every other in-list control. */
    mod.widgets.KeyBtn = set_type_default() do #(KeyBtn::register_widget(vm)) {
        ..mod.widgets.View
        width: Fit, height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{left: 10, right: 10, top: 4, bottom: 4}
        cursor: MouseCursor.Hand
        show_bg: true
        draw_bg +: {
            color: #ffffff
            // The 1 pt ink border, shader-drawn: a View's bg has no border
            // of its own, and a distinct shader earns the correctly
            // ordered draw call anyway.
            pixel: fn() {
                let p = self.pos * self.rect_size
                let d = min(min(p.x, p.y), min(self.rect_size.x - p.x, self.rect_size.y - p.y))
                if d < 1.0 {
                    return vec4(0.078, 0.078, 0.078, 1.0)
                }
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
        pre := mod.widgets.SLabel {
            padding: 0, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 8.25} }
        }
        key := mod.widgets.SBoldLabel {
            padding: 0, text: ""
            draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 8.25} }
        }
        post := mod.widgets.SLabel {
            padding: 0, text: ""
            draw_text +: { text_style: mod.widgets.SMonoStyle{font_size: 8.25} }
        }
    }

    /** The verbs of the marks bar as one group: inline after the count
        where the width allows, under it where it does not. A Fit child
        never wraps — the turtle cannot know its width in advance — so the
        bar decides at draw, where the width is known, and shows one of
        two copies. */
    mod.widgets.MarkVerbs = View {
        width: Fit, height: Fit
        flow: Right
        align: Align{y: 0.5}
        spacing: 8
        archive_btn := mod.widgets.KeyBtn {}
        delete_btn := mod.widgets.KeyBtn {}
        all_btn := mod.widgets.KeyBtn {}
        clear_btn := mod.widgets.KeyBtn {}
    }

    /** The marks bar (CR-009): what a list shows while any row is marked —
        how many of how many, how many the filter hides; then the verbs
        that act on the marked set, `all`, `clear`. It comes with the first
        mark and goes with the last: nothing is drawn for an empty set. The
        verbs wear the letters their single-row twins wear (the borrowed
        `a` and `d`, which stand down while the bar is up). */
    mod.widgets.MarkBar = set_type_default() do #(MarkBar::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        padding: Inset{left: 8, right: 8, top: 4, bottom: 6}
        spacing: 6
        line := View {
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            spacing: 8
            count_lbl := mod.widgets.SLabel { padding: 0, width: Fit, text: "" }
            hidden_lbl := mod.widgets.SLabel {
                padding: 0, width: Fit, text: ""
                draw_text +: { color: #909090 }
            }
            View { width: 4, height: 1 }
            verbs_inline := mod.widgets.MarkVerbs {}
        }
        verbs_below := mod.widgets.MarkVerbs { visible: false }
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
            margin: Inset{left: 8, top: 4}
            text: "", draw_text +: { color: #a01500 }
        }
        // The marks bar (CR-009): up while any row is marked, gone otherwise.
        bar := mod.widgets.MarkBar { visible: false }
        View { width: Fill, height: 6 }
        // Header cells for the columns only — the subject rides each row's
        // extra line, owns no column, and so gets no header. The header
        // wears the rows' inset, so FROM shares the rows' left edge and
        // DATE their right. FROM sits in a Fill View, not at Fill itself —
        // the rows' construction, so it walks exactly like their from
        // label.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
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
            // A row that scrolls out is kept for the next one that scrolls
            // in, rather than built again: a long list scrolls without
            // minting widgets.
            reuse_items: true
            row := mod.widgets.InboxRow {}
            // The marks the filter hides ride above the rows (CR-009): a
            // caption, the rows themselves, and a strong rule closing the
            // group. The caption wears the rows' inset the way the header
            // does; the rule has its own pixel fn, or it merges into a
            // call under the panel and never shows.
            caption := View {
                width: Fill, height: Fit
                padding: Inset{left: 8, right: 8, top: 6, bottom: 2}
                mod.widgets.SSection { text: "MARKED · HIDDEN BY THE FILTER" }
            }
            rule := View {
                width: Fill, height: 1
                show_bg: true
                draw_bg +: {
                    color: #141414
                    pixel: fn() {
                        return vec4(self.color.xyz * self.color.w, self.color.w)
                    }
                }
            }
        }
        // The autocomplete, drawn last and over the rows (see `SuggestBox`).
        suggest: mod.widgets.SuggestBox {}
    }

    // ---- the effect log ----------------------------------------------------

    /** One job as the log lists it: the verb and whose it was on the first
        line, the effect's own sentence under it, and — only when there is
        one — what went wrong, in the colour errors get.

        Twin lines again (see `InboxRow`): the cursor's row is the washed
        copy, because a quad's colour is not a runtime value. */
    mod.widgets.EffectLine = set_type_default() do #(EffectLine::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        // The row's inset is the one source of spacing, as the inbox row's
        // is: every label in it sheds the theme padding so the text sits
        // 8 pt inside the row, level with the filter's own text.
        padding: Inset{left: 8, right: 8, top: 4, bottom: 4}
        View {
            width: Fill, height: Fit
            align: Align{y: 0.5}
            kind_lbl := mod.widgets.SLabel { padding: 0, width: Fit, text: "" }
            View { width: 8, height: 1 }
            // The entity rides a Fill View whose flow is Down, for the
            // reason the inbox row's from label does: a Fill label on a
            // Right flow's main axis defer-walks.
            View {
                width: Fill, height: Fit
                flow: Down
                entity_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #909090 }
                }
            }
            View { width: 10, height: 1 }
            status_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fit, text: "", draw_text +: { color: #5a5a5a }
            }
            View { width: 10, height: 1 }
            date_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fit, text: "", draw_text +: { color: #909090 }
            }
        }
        what_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
        }
        err_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            width: Fill, max_lines: 2, text_overflow: TextOverflow.Ellipsis
            text: "", draw_text +: { color: #a01500 }
        }
    }

    /** A row of the log: the line, plain or washed, and the hairline under
        it. The job itself is a panel — the log previews into it exactly as
        the inbox previews a message — so a row is only ever one line's
        worth of it. */
    mod.widgets.EffectRow = set_type_default() do #(EffectRow::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        line := mod.widgets.EffectLine {}
        line_sel := mod.widgets.EffectLine {
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

    /** The effect log: the filter over the header over the virtualized list
        — a rich table (CR-006) over `effect::LOG`. Read-only by
        construction; the queue is the executor's to move. */
    mod.widgets.EffectsPanel = set_type_default() do #(EffectsPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        filter_input := mod.widgets.SField {
            width: Fill
            empty_text: "filter…  ( / )   @ for tags"
            return_key_type: ReturnKeyType.Search
            autocapitalize: AutoCapitalize.None
            autocorrect: AutoCorrect.Disabled
        }
        // Named apart from the row's own `err_lbl`: both live under this
        // panel, and a lookup by id must not be able to find the wrong one.
        filter_err_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            margin: Inset{left: 8, top: 4}
            text: "", draw_text +: { color: #a01500 }
        }
        View { width: Fill, height: 6 }
        // Header cells for the columns the head line actually has; the
        // sentence under it owns no column, exactly as the inbox's subject
        // does not.
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "EFFECT" }
            }
            mod.widgets.SSection { padding: 0, width: Fit, text: "STATUS" }
        }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }
        // Nothing has left the process yet — said, rather than left blank.
        // Above the list, because the list is what fills what is left.
        empty_lbl := mod.widgets.SLabel {
            visible: false
            margin: Inset{left: 8, top: 10}
            text: "", draw_text +: { color: #909090 }
        }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            row := mod.widgets.EffectRow {}
        }
        // The autocomplete, drawn last and over the rows (see `SuggestBox`).
        suggest: mod.widgets.SuggestBox {}
    }

    /** One job of the queue, in full — what the log previews into. The
        sentence the effect describes itself with reads as the subject, then
        what went wrong if anything did, then the row as `sqlite3` would
        show it: the job's own facts, the payload it was filed as, and the
        answer the world gave back. Everything below the subject is a
        selectable run; a payload is something one copies into a report. */
    mod.widgets.JobPanel = set_type_default() do #(JobPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0
        scroll_bars: ScrollBars{ show_scroll_x: false }

        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            kind_lbl := mod.widgets.SLabel { padding: 0, width: Fit, text: "" }
            View { width: 8, height: 1 }
            View {
                width: Fill, height: Fit
                flow: Down
                entity_lbl := mod.widgets.SLabel {
                    padding: 0
                    width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
                    draw_text +: { color: #909090 }
                }
            }
            View { width: 10, height: 1 }
            status_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fit, text: "", draw_text +: { color: #5a5a5a }
            }
        }
        // Every run here is `is_multiline`: without it a TextInput lays out
        // on one row and neither wraps nor honours a newline — and this
        // panel is nothing but long text (a payload is one unbroken token,
        // which the layouter then breaks by grapheme).
        what_txt := mod.widgets.SText { is_multiline: true, margin: Inset{top: 2} }
        // A run that comes and goes hangs on a View: `visible` is the View's
        // property, and a TextInput neither takes it in the DSL nor honours
        // `set_visible` — its default is "always visible", so an error line
        // hidden that way would simply draw empty.
        err_row := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            err_txt := mod.widgets.SText {
                is_multiline: true
                margin: Inset{top: 4}
                draw_text +: {
                    color: #a01500
                    color_hover: #a01500
                    color_focus: #a01500
                    color_down: #a01500
                    color_empty: #a01500
                }
            }
        }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "JOB" }
        mod.widgets.SRule {}
        meta_txt := mod.widgets.SText { is_multiline: true }

        View { width: Fill, height: 10 }
        mod.widgets.SSection { text: "PAYLOAD" }
        mod.widgets.SRule {}
        payload_txt := mod.widgets.SText { is_multiline: true }

        reply_block := View {
            visible: false
            width: Fill, height: Fit
            flow: Down
            View { width: Fill, height: 10 }
            mod.widgets.SSection { text: "REPLY" }
            mod.widgets.SRule {}
            reply_txt := mod.widgets.SText { is_multiline: true }
        }
    }

    // ---- files (CR-008) ----------------------------------------------------

    /** One entry of a directory: the name (a directory wears its slash),
        the size and the date at the right, on the columns the header
        above them draws. */
    mod.widgets.FilesLine = set_type_default() do #(FilesLine::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{left: 8, right: 8, top: 4, bottom: 4}
        View {
            width: Fill, height: Fit
            flow: Down
            name_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            }
        }
        View { width: 10, height: 1 }
        View {
            width: 60, height: Fit
            align: Align{x: 1.0}
            size_lbl := mod.widgets.SLabel {
                padding: 0
                width: Fit, text: "", draw_text +: { color: #5a5a5a }
            }
        }
        View { width: 12, height: 1 }
        date_lbl := mod.widgets.SLabel {
            padding: 0
            width: Fit, text: "", draw_text +: { color: #909090 }
        }
    }

    /** A files row: the line, its selected twin, a hairline. */
    mod.widgets.FilesRow = set_type_default() do #(FilesRow::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fit
        flow: Down
        line := mod.widgets.FilesLine {}
        line_sel := mod.widgets.FilesLine {
            visible: false
            show_bg: true
            draw_bg +: {
                color: #e7e7e7
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

    /** A directory as a column (CR-008): where the panel stands as crumbs,
        the filter, the `new dir` field while it is up, the header over
        the rows, the status line under them. */
    mod.widgets.FilesPanel = set_type_default() do #(FilesPanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 0

        // Every ancestor a dotted link — it replaces the panel with that
        // directory in place — and the directory itself plain, last.
        crumbs := View {
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            padding: Inset{left: 8, right: 8, bottom: 8}
            c0 := mod.widgets.SLink {}
            s0 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            c1 := mod.widgets.SLink {}
            s1 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            c2 := mod.widgets.SLink {}
            s2 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            c3 := mod.widgets.SLink {}
            s3 := mod.widgets.SLabel { padding: 0, text: " / ", draw_text +: { color: #909090 } }
            here_lbl := mod.widgets.SLabel { padding: 0, text: "" }
        }
        // `go to`: the crumbs as a field — the path, completed segment by
        // segment; enter goes there, esc puts the crumbs back.
        path_row := View {
            visible: false
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            margin: Inset{bottom: 6}
            mod.widgets.SSection { width: 82, text: "GO TO" }
            path_input := mod.widgets.SField {
                empty_text: "~/ or /"
                return_key_type: ReturnKeyType.Go
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        filter_input := mod.widgets.SField {
            width: Fill
            empty_text: "filter…  ( / )   @ for tags"
            return_key_type: ReturnKeyType.Search
            autocapitalize: AutoCapitalize.None
            autocorrect: AutoCorrect.Disabled
        }
        err_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            margin: Inset{left: 8, top: 4}
            text: "", draw_text +: { color: #a01500 }
        }
        // The `new dir` field: up while the button asked for it; enter
        // creates, esc puts it away.
        newdir := View {
            visible: false
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            margin: Inset{top: 6}
            mod.widgets.SSection { width: 82, text: "NEW DIR" }
            newdir_input := mod.widgets.SField {
                empty_text: "name"
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        View { width: Fill, height: 6 }
        View {
            width: Fill, height: Fit
            padding: Inset{left: 8, right: 8, top: 0, bottom: 3}
            View {
                width: Fill, height: Fit
                mod.widgets.SSection { padding: 0, text: "NAME" }
            }
            View { width: 10, height: 1 }
            View {
                width: 60, height: Fit
                align: Align{x: 1.0}
                mod.widgets.SSection { padding: 0, width: Fit, text: "SIZE" }
            }
            View { width: 12, height: 1 }
            mod.widgets.SSection { padding: 0, width: Fit, text: "MODIFIED" }
        }
        View { width: Fill, height: 1, show_bg: true, draw_bg +: { color: #141414 } }
        list := PortalList {
            width: Fill, height: Fill
            flow: Down
            reuse_items: true
            row := mod.widgets.FilesRow {}
        }
        // A refused verb, a directory that is gone: the one colour
        // errors get.
        status_lbl := mod.widgets.SLabel {
            visible: false
            padding: 0
            margin: Inset{left: 8, top: 6}
            text: "", draw_text +: { color: #a01500 }
        }
        suggest: mod.widgets.SuggestBox {}
        // The path field's own box, under it.
        suggest_path: mod.widgets.SuggestBox {}
    }

    /** A file as a card (CR-008): name, kind and size, when it changed,
        the path selectable, and under a rule the preview — text or a
        picture; anything else says so. */
    mod.widgets.FilePanel = set_type_default() do #(FilePanel::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

        name_lbl := mod.widgets.SBoldLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis
            draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 13.0} }
        }
        kind_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #5a5a5a } }
        when_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #909090 } }
        path_txt := mod.widgets.SText { text: "" }
        mod.widgets.SRule {}
        // A text input carries no `visible` either; its box does.
        text_box := View {
            visible: false
            width: Fill, height: Fill
            text_prev := mod.widgets.SText {
                width: Fill, height: Fill
                is_multiline: true
            }
        }
        // `Image` carries no `visible` of its own, so the box around it
        // is what shows and hides the picture.
        img_box := View {
            visible: false
            width: Fill, height: Fit
            img_prev := mod.widgets.Image {
                width: Fill, height: Fit
                fit: ImageFit.Horizontal
            }
        }
        none_lbl := mod.widgets.SLabel {
            visible: false
            text: "no preview — open shows it"
            draw_text +: { color: #909090 }
        }
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
            name_lbl := mod.widgets.SLabel { width: Fit, max_lines: 1, text: "" }
            from_link := mod.widgets.SLink { visible: false }
            View { width: 10, height: 1 }
            // The preview rides a Fill View whose flow is Down, for the
            // reason the inbox row's from label does: a Fill label on a
            // Right flow's main axis defer-walks.
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
            spacer := View { visible: false, width: Fill, height: 1 }
            View { width: 10, height: 1 }
            // Passed on: the mark every client draws for `$Forwarded`, by
            // the date, muted like it.
            fwd_lbl := mod.widgets.SLabel {
                visible: false
                padding: 0
                width: Fit, text: "↪ ", draw_text +: { color: #909090 }
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
                mod.widgets.SLabel { text: "› quoted", draw_text +: { color: #909090 } }
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
        forward and reply at the foot. */
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
            // A finger drags the thread; a mouse button on it is a
            // selection, never a scroll. The list would otherwise turn a
            // press that lands while a coast is still live into a drag,
            // and pull the letter away from under a selection begun a
            // moment after scrolling.
            drag_scrolling: #(cfg!(target_os = "android"))
            msg := mod.widgets.ThreadMsg {}
        }
        View {
            width: Fill, height: Fit, align: Align{y: 0.5}
            spacing: 14
            View { width: Fill, height: 1 }
            forward_link := mod.widgets.SLink {}
            reply_link := mod.widgets.SLink {}
        }
    }

    /** A sender's card. */
    mod.widgets.ContactPanel = set_type_default() do #(ContactPanel::register_widget(vm)) {
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
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "a red mark in the corner counts what is wrong in the background (a sync, a send); it opens the problems panel" }
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
        // already a squeeze before delete joined the message's chrome, and
        // forward took the message onto a second line.
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "  message " }
            mod.widgets.SKbd { text: "cmd+a" }
            mod.widgets.SLabel { text: "rchive " }
            mod.widgets.SKbd { text: "cmd+d" }
            mod.widgets.SLabel { text: "elete" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "          " }
            mod.widgets.SKbd { text: "cmd+r" }
            mod.widgets.SLabel { text: "eply " }
            mod.widgets.SKbd { text: "cmd+f" }
            mod.widgets.SLabel { text: "orward" }
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
            mod.widgets.SLabel { text: "          " }
            mod.widgets.SKbd { text: "space" }
            mod.widgets.SLabel { text: " marks  " }
            mod.widgets.SKbd { text: "shift" }
            mod.widgets.SLabel { text: "+arrows a range" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { text: "  marked  " }
            mod.widgets.SKbd { text: "cmd+a" }
            mod.widgets.SLabel { text: "rchive " }
            mod.widgets.SKbd { text: "cmd+d" }
            mod.widgets.SLabel { text: "elete a" }
            mod.widgets.SKbd { text: "cmd+l" }
            mod.widgets.SLabel { text: "l" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "clicking a row, or walking onto it, opens the thread beside the list without leaving it — and that preview lends the list its own keys" }
        }
        mod.widgets.SRow {
            mod.widgets.SLabel { width: Fill, text: "in a thread, a closed message is a row: click it to open it in place, click an open one's header to close it" }
        }
        mod.widgets.SRow {
            mod.widgets.SKbd { text: "esc" }
            mod.widgets.SLabel { text: " leaves a text field, or clears the marks" }
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
            margin: Inset{left: 12}
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
// ProblemRow / ProblemsPanel
// ---------------------------------------------------------------------------

/// One standing problem. Presentation plus the intent its button fires;
/// clicks resolve through the shell's semantic rects, as every list item's
/// do (areas go stale mid-gesture), and the tab ring presses through
/// [`ProblemRow::action`].
#[derive(Script, ScriptHook, Widget)]
pub struct ProblemRow {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// What the row's button fires — `None` when nothing can be done.
    #[rust]
    action: Option<PanelAction>,
    /// What the row's link fires, for the tab ring; the pointer path goes
    /// through the shell's hit table like every list item's.
    #[rust]
    link_action: Option<PanelAction>,
}

impl Widget for ProblemRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl ProblemRowRef {
    pub fn populate(&self, cx: &mut Cx, pid: u64, p: &crate::problems::Problem) {
        use crate::problems::Source;
        let Some(mut row) = self.borrow_mut() else {
            return;
        };
        row.view.text_input(cx, ids!(label_lbl)).set_text(cx, &p.label);
        row.view.label(cx, ids!(line_lbl)).set_text(cx, &p.line);
        row.view.label(cx, ids!(detail_lbl)).set_text(cx, &p.detail);
        // A send the executor is still retrying offers nothing: the machine
        // is on it, and a second filing would only race the first.
        let (sync, retry, settings, reopen) = match &p.source {
            Source::Account { .. } => (true, false, true, false),
            Source::Send { given_up, .. } => (false, *given_up, false, *given_up),
            Source::Sync => (false, false, false, false),
        };
        row.view.widget(cx, ids!(sync_btn)).set_visible(cx, sync);
        row.view.widget(cx, ids!(retry_btn)).set_visible(cx, retry);
        row.view.widget(cx, ids!(settings_link)).set_visible(cx, settings);
        if settings {
            row.view.link(cx, ids!(settings_link)).set(
                cx,
                pid,
                "settings",
                crate::core::Kind::Settings,
                false,
            );
        }
        row.view.widget(cx, ids!(reopen_link)).set_visible(cx, reopen);
        if reopen {
            row.view.link(cx, ids!(reopen_link)).set_label(cx, "reopen");
        }
        row.action = match &p.source {
            Source::Account { id, .. } => Some(PanelAction::SyncAccount(*id)),
            Source::Send {
                outbox,
                given_up: true,
                ..
            } => Some(PanelAction::RetrySend(*outbox)),
            Source::Send { .. } | Source::Sync => None,
        };
        row.link_action = match &p.source {
            Source::Account { .. } => Some(PanelAction::FollowLink {
                pid,
                target: crate::core::Kind::Settings,
                dotted: false,
                fresh: false,
            }),
            Source::Send {
                outbox,
                seed,
                given_up: true,
                ..
            } => Some(PanelAction::ReopenSend {
                pid,
                outbox: *outbox,
                seed: *seed,
                fresh: false,
            }),
            Source::Send { .. } | Source::Sync => None,
        };
    }
}

/// Every standing problem as a row (see [`crate::problems`]). Its rows are
/// handed in through the props — the shell derives them, since device
/// sync's entry is not in the store.
#[derive(Script, ScriptHook, Widget)]
pub struct ProblemsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl ProblemsPanel {
    /// The tab ring in visual order: each visible row's button, then its
    /// link. No chords: a panel with a control per row gives none (rule 4).
    fn ring(&self, cx: &mut Cx) -> Vec<RingStop> {
        let mut v = Vec::new();
        if let Some(list) = self
            .view
            .widget(cx, ids!(list))
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
                let (act, link) = match row.as_problem_row().borrow() {
                    Some(r) => (r.action.clone(), r.link_action.clone()),
                    None => continue,
                };
                if let Some(act) = act {
                    let b = match act {
                        PanelAction::SyncAccount(_) => row.button(cx, ids!(sync_btn)),
                        _ => row.button(cx, ids!(retry_btn)),
                    };
                    v.push(RingStop::Act(b, act));
                }
                if let Some(link) = link {
                    let w = match link {
                        PanelAction::FollowLink { .. } => row.widget(cx, ids!(settings_link)),
                        _ => row.widget(cx, ids!(reopen_link)),
                    };
                    v.push(RingStop::Link(w, link));
                }
            }
        }
        v
    }
}

impl Widget for ProblemsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::KeyDown(k) = event {
            if k.modifiers.logo {
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
                        if let RingStop::Act(_, a) | RingStop::Link(_, a) = stop {
                            cx.action(a);
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
        let problems = props.map(|p| p.problems.clone()).unwrap_or_default();
        self.view
            .label(cx, ids!(none_lbl))
            .set_visible(cx, problems.is_empty());
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, problems.len());
                while let Some(idx) = list.next_visible_item(cx) {
                    if let Some(p) = problems.get(idx) {
                        let row = list.item(cx, idx, live_id!(problem_row));
                        row.as_problem_row().populate(cx, pid, p);
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
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
        let ok_lbl = row.view.text_input(cx, ids!(status_lbl));
        let err_lbl = row.view.text_input(cx, ids!(status_err_lbl));
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
    /// A problems row's button and the intent it fires.
    Act(ButtonRef, PanelAction),
    /// A problems row's link and the intent it fires: a link on the ring
    /// wears its focus as a doubled underline (see `SLink`).
    Link(WidgetRef, PanelAction),
}

impl RingStop {
    fn is_focused(&self, cx: &Cx) -> bool {
        match self {
            RingStop::Input(t) => t.key_focus(cx),
            RingStop::Remove(b, _) | RingStop::Add(b) | RingStop::Act(b, _) => {
                cx.has_key_focus(b.area())
            }
            RingStop::Link(w, _) => cx.has_key_focus(w.area()),
        }
    }

    fn focus(&self, cx: &mut Cx) {
        match self {
            RingStop::Input(t) => focus_input(cx, t),
            RingStop::Remove(b, _) | RingStop::Add(b) | RingStop::Act(b, _) => {
                cx.set_key_focus(b.area())
            }
            RingStop::Link(w, _) => cx.set_key_focus(w.area()),
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
                let target = match k.key_code {
                    KeyCode::KeyD => Some(crate::core::Kind::AddAccount),
                    KeyCode::KeyY => Some(crate::core::Kind::Bucket),
                    _ => None,
                };
                if let Some(target) = target {
                    let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                    cx.action(PanelAction::FollowLink {
                        pid,
                        target,
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
        self.view.link(cx, ids!(bucket_link)).set_accel(
            cx,
            pid,
            "device sync",
            crate::core::Kind::Bucket,
            false,
            Some(ui::ACCEL_DEVICE_SYNC),
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

    /// The tab ring in visual order: the Google button, the fields, the
    /// add button.
    fn ring(&self, cx: &mut Cx) -> Vec<RingStop> {
        let mut v = vec![RingStop::Add(self.view.button(cx, ids!(google_btn)))];
        v.extend(self.inputs(cx).into_iter().map(RingStop::Input));
        v.push(RingStop::Add(self.view.button(cx, ids!(add_btn))));
        v
    }

    /// What the Google flow has to say, if anything. The shell owns the
    /// flow and pokes the line in at each step, the way it pokes the form
    /// clear — a retained widget keeps it until the next word.
    pub fn set_google(&mut self, cx: &mut Cx, line: &str, err: bool) {
        for (id, mine) in [(ids!(google_lbl), !err), (ids!(google_err_lbl), err)] {
            let l = self.view.label(cx, id);
            l.set_text(cx, if mine { line } else { "" });
            l.set_visible(cx, mine && !line.is_empty());
        }
        self.redraw(cx);
    }

    /// The Google line as drawn: what the e2e bridge reads back, and `None`
    /// while the flow has said nothing.
    pub fn google_line(&self, cx: &mut Cx) -> Option<String> {
        [ids!(google_lbl), ids!(google_err_lbl)]
            .into_iter()
            .map(|id| self.view.label(cx, id))
            .find(|l| l.visible())
            .map(|l| l.text())
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
                let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                if cx.has_key_focus(self.view.button(cx, ids!(add_btn)).area()) {
                    self.submit(cx, pid);
                }
                if cx.has_key_focus(self.view.button(cx, ids!(google_btn)).area()) {
                    cx.action(PanelAction::GoogleSignIn { pid });
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
            if self.view.button(cx, ids!(google_btn)).clicked(actions) {
                let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                cx.action(PanelAction::GoogleSignIn { pid });
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}


// ---------------------------------------------------------------------------
// BucketPanel
// ---------------------------------------------------------------------------

/// The device-sync form (CR-005). `AddAccountPanel`'s sibling in every
/// respect but one: the secret field is write-only. It is seeded blank on a
/// configured device too, because a key that can be read back off a screen is
/// a key that leaves by a route nobody chose.
#[derive(Script, ScriptHook, Widget)]
pub struct BucketPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl BucketPanel {
    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 3] {
        [
            self.view.text_input(cx, ids!(url_input)),
            self.view.text_input(cx, ids!(key_input)),
            self.view.text_input(cx, ids!(secret_input)),
        ]
    }

    /// The tab ring in visual order: the fields, the connect button.
    fn ring(&self, cx: &mut Cx) -> Vec<RingStop> {
        let mut v: Vec<RingStop> = self.inputs(cx).into_iter().map(RingStop::Input).collect();
        v.push(RingStop::Add(self.view.button(cx, ids!(connect_btn))));
        v
    }

    fn submit(&mut self, cx: &mut Cx, pid: u64) {
        let (url, key_id, secret) = self.form_values(cx);
        cx.action(PanelAction::ConnectBucket {
            pid,
            url,
            key_id,
            secret,
        });
    }

    /// The form's current values — the e2e bridge submits through the same
    /// `PanelAction` the button emits.
    pub fn form_values(&mut self, cx: &mut Cx) -> (String, String, String) {
        let [url, key, secret] = self.inputs(cx);
        (
            url.text().trim().to_string(),
            key.text().trim().to_string(),
            secret.text(),
        )
    }

    /// Seeds the two public fields from what this device is configured with
    /// — the shell calls it once, when the widget is built. The secret is
    /// never seeded.
    pub fn prefill(&mut self, cx: &mut Cx, url: &str, key_id: &str) {
        let [u, k, _] = self.inputs(cx);
        u.set_text(cx, url);
        k.set_text(cx, key_id);
    }

    /// Clears the secret after a successful connect (the shell calls this):
    /// it is in the keychain now, and a form is not a place to keep one.
    pub fn clear_secret(&mut self, cx: &mut Cx) {
        let [_, _, secret] = self.inputs(cx);
        secret.set_text(cx, "");
    }
}

impl Widget for BucketPanel {
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
                let btn = self.view.button(cx, ids!(connect_btn));
                if cx.has_key_focus(btn.area()) {
                    let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
                    self.submit(cx, pid);
                }
            }
        }

        if let Event::Actions(actions) = event {
            let [url, key, secret] = self.inputs(cx);
            // A blurred field keeps no selection (the frameworks' norm).
            for t in [&url, &key, &secret] {
                if t.key_focus_lost(actions) {
                    t.set_cursor(cx, t.cursor(), false);
                }
            }
            // Enter advances; past the last field it submits.
            if url.returned(actions).is_some() {
                focus_input(cx, &key);
            } else if key.returned(actions).is_some() {
                focus_input(cx, &secret);
            } else if secret.returned(actions).is_some()
                || self.view.button(cx, ids!(connect_btn)).clicked(actions)
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

impl BucketPanelRef {
    /// Seeds the two public fields (once, at instantiation).
    pub fn prefill(&self, cx: &mut Cx, url: &str, key_id: &str) {
        let Some(mut p) = self.borrow_mut() else { return };
        p.prefill(cx, url, key_id);
    }

    /// Clears the secret field after a successful connect.
    pub fn clear_secret(&self, cx: &mut Cx) {
        let Some(mut p) = self.borrow_mut() else { return };
        p.clear_secret(cx);
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
    /// Whether the field held the keyboard at the last event the panel
    /// saw — [`Suggest::track`]. The draw reads this rather than polling
    /// key focus: a panels-library mount that has arrived is a picture
    /// which hears no events, and the one global keyboard has long moved
    /// on to the next node by the time it re-renders, so polling would
    /// close every offer a node was meant to show. In the app the events
    /// never stop, so this is key focus with one event of lag.
    focused: bool,
}

impl<C: Completion> Default for Suggest<C> {
    fn default() -> Self {
        Suggest {
            ctx: None,
            items: Vec::new(),
            sel: 0,
            dismissed: None,
            focused: false,
        }
    }
}

impl<C: Completion> Suggest<C> {
    /// Notes whether the field holds the keyboard now. Call it on every
    /// event the panel handles, before anything else reads the box.
    pub fn track(&mut self, cx: &mut Cx, field: &TextInputRef) {
        self.focused = field.key_focus(cx);
    }

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
        let ctx = if self.focused {
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

/// Lands in the `j`-th compose field. The one-line fields take their text
/// selected, as a form's do; the body keeps its caret — a letter is not a
/// value to type over, and in a forward it is the mail being passed on,
/// with the caret above it.
fn land(cx: &mut Cx, inputs: &[TextInputRef; 3], j: usize) {
    if j == 2 {
        inputs[2].set_key_focus(cx);
    } else {
        focus_input(cx, &inputs[j]);
    }
}

impl Widget for ComposePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
        let to = self.view.text_input(cx, ids!(to_input));
        self.ac.track(cx, &to);
        // The TO field's autocomplete owns the arrows, enter, tab and esc
        // while it is open (see `Suggest`); neither the field nor the tab
        // ring sees them. A pick is an edit like any typing.
        if let Event::KeyDown(k) = event {
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
                land(cx, &inputs, j as usize);
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
                land(cx, &[to.clone(), subject.clone(), body.clone()], 1);
            } else if subject.returned(actions).is_some() {
                land(cx, &[to.clone(), subject.clone(), body.clone()], 2);
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

    /// Focuses TO — where a forward starts, its letter already in the body.
    pub fn focus_to(&self, cx: &mut Cx) {
        let Some(inner) = self.borrow() else { return };
        inner.view.text_input(cx, ids!(to_input)).set_key_focus(cx);
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
    /// `selected` is the cursor's wash; `marked` the batch mark (CR-009).
    /// Exactly one of the four twins draws; only it is populated.
    pub fn populate(&self, cx: &mut Cx, m: &mail::ThreadHead, selected: bool, marked: bool) {
        let Some(row) = self.borrow() else { return };
        let twins = [
            (ids!(line), !selected && !marked),
            (ids!(line_sel), selected && !marked),
            (ids!(line_mark), !selected && marked),
            (ids!(line_mark_sel), selected && marked),
        ];
        for (id, on) in twins {
            let w = row.view.widget(cx, id);
            if on {
                w.as_inbox_line().populate(cx, m);
            }
            w.set_visible(cx, on);
        }
    }
}

// ---------------------------------------------------------------------------
// Marks (CR-009): the draft's widgets
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct KeyBtn {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for KeyBtn {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl KeyBtnRef {
    /// The label, its `accel` letter drawn bold where the label carries it.
    pub fn set(&self, cx: &mut Cx, text: &str, accel: Option<char>) {
        let Some(b) = self.borrow() else { return };
        let (pre, key, post) = ui::split_accel(text, accel);
        for (id, s) in [(ids!(pre), pre), (ids!(key), key), (ids!(post), post)] {
            let l = b.view.label(cx, id);
            l.set_text(cx, &s);
            l.set_visible(cx, !s.is_empty());
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct MarkBar {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The width the one-line layout needs, in points: measured off the
    /// texts at populate, held against the turtle's width at draw.
    #[rust]
    need: f64,
    /// Which copy of the verbs the last draw showed: beside the count, or
    /// under it. The shell registers that copy's buttons.
    #[rust]
    inline: bool,
}

impl Widget for MarkBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // The verbs beside the count where they fit, under it where they
        // do not — decided here, the one place the width is known.
        let fits = self.need <= cx.turtle().inner_width();
        self.inline = fits;
        self.view
            .view(cx, ids!(line.verbs_inline))
            .set_visible(cx, fits);
        self.view.view(cx, ids!(verbs_below)).set_visible(cx, !fits);
        self.view.draw_walk(cx, scope, walk)
    }
}

/// The bar's buttons, in [`ui::MARK_VERBS`] order.
const VERB_BTNS: [&[LiveId]; 4] = [
    ids!(archive_btn),
    ids!(delete_btn),
    ids!(all_btn),
    ids!(clear_btn),
];

impl MarkBarRef {
    /// The bar's buttons as drawn, `(label, rect, verb)` — the copy of the
    /// verbs the last draw showed, only the ones offered.
    pub fn verb_hits(&self, cx: &mut Cx) -> Vec<(String, Rect, ui::MarkVerb)> {
        let Some(b) = self.borrow() else { return Vec::new() };
        let group: &[LiveId] = if b.inline {
            ids!(line.verbs_inline)
        } else {
            ids!(verbs_below)
        };
        let g = b.view.widget(cx, group);
        ui::MARK_VERBS
            .iter()
            .zip(VERB_BTNS)
            .filter_map(|(v, id)| {
                let btn = g.widget(cx, id);
                let r = btn.area().rect(cx);
                (btn.visible() && r.size.x > 0.0).then(|| (v.hit_label().to_string(), r, *v))
            })
            .collect()
    }

    /// `marked` rows in all, `hidden` of them outside the list's filter,
    /// `total` rows under it. With every row under the filter marked,
    /// `all` stands down.
    pub fn populate(&self, cx: &mut Cx, marked: usize, total: usize, hidden: usize) {
        let Some(mut b) = self.borrow_mut() else { return };
        let shown = marked.saturating_sub(hidden);
        let all = shown >= total;
        let count = if hidden > 0 {
            format!("{marked} marked")
        } else if all {
            format!("all {total} marked")
        } else {
            format!("{marked} of {total} marked")
        };
        let hid = if hidden > 0 {
            format!("· {hidden} hidden by the filter")
        } else {
            String::new()
        };
        b.view.label(cx, ids!(line.count_lbl)).set_text(cx, &count);
        let h = b.view.label(cx, ids!(line.hidden_lbl));
        h.set_text(cx, &hid);
        h.set_visible(cx, hidden > 0);
        // The labels and the letters come from the one table the
        // accelerator rules are tested against — a button never advertises
        // a key the chord dispatch does not answer to.
        let verbs: Vec<(&'static str, Option<char>, bool)> = ui::MARK_VERBS
            .iter()
            .map(|v| (v.label(), v.accel(), *v != ui::MarkVerb::All || !all))
            .collect();
        let groups: [&[LiveId]; 2] = [ids!(line.verbs_inline), ids!(verbs_below)];
        for g in groups {
            let group = b.view.widget(cx, g);
            for (id, (text, accel, on)) in VERB_BTNS.iter().zip(verbs.iter()) {
                let btn = group.widget(cx, *id);
                btn.as_key_btn().set(cx, text, *accel);
                btn.set_visible(cx, *on);
            }
        }
        // What one line needs: the texts on the mono cell, the buttons
        // with their padding, the gaps and the bar's inset.
        let body = crate::theme::FONT_SIZE * crate::theme::MONO_ADV;
        let label = crate::theme::LABEL_SIZE * crate::theme::MONO_ADV;
        let text = (count.chars().count() + hid.chars().count()) as f64 * body
            + if hidden > 0 { 8.0 } else { 0.0 };
        let btns: f64 = verbs
            .iter()
            .filter(|(_, _, on)| *on)
            .map(|(t, _, _)| t.chars().count() as f64 * label + 20.0 + 8.0)
            .sum();
        b.need = 16.0 + text + 8.0 + 4.0 + 8.0 + btns;
    }
}

// ---------------------------------------------------------------------------
// InboxPanel
// ---------------------------------------------------------------------------

/// The inbox's table: the shared engine over the thread datasource.
type InboxTable = Table<&'static SqlSource<mail::ThreadHead, i64>>;

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
    /// What each live row was last populated with, by list index: a draw
    /// repopulates only the rows whose thread, cursor or mark changed, so
    /// scrolling a long list costs its new rows and nothing else.
    #[rust]
    stamps: HashMap<usize, (mail::ThreadHead, bool, bool)>,
    /// The marks (CR-009): the threads picked out for a batch verb, kept as
    /// keys so they survive the filter, the paging and a sync landing
    /// underneath. Context, not history — gone with the process.
    #[rust]
    marks: Marks<i64>,
    /// The marked threads the filter hides, as rows, in the marks' order:
    /// derived each draw from the table's answer and read fresh by key.
    /// They ride above the table's rows in the one list.
    #[rust]
    hidden: Vec<mail::ThreadHead>,
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
        let p = self.prefix();
        if let Some((i, _)) = self
            .stamps
            .iter()
            .find(|(idx, (t, _, _))| **idx >= p && t.thread == th)
        {
            let i = *i - p;
            if self.table.row(store, i).is_some_and(|t| t.thread == th) {
                return Some(i);
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
        let li = self.list_index(i);
        let visible = list
            .borrow()
            .is_some_and(|l| l.items().iter().any(|(idx, _)| *idx == li));
        if !visible {
            list.smooth_scroll_to(cx, li, 90.0, None, 0.0);
        }
        cx.action(PanelAction::Preview {
            pid,
            target: crate::core::Kind::Message { id: m.target },
        });
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

    /// The list items above the table's rows: none, or the caption, the
    /// marks the filter hides and the rule under them (CR-009).
    fn prefix(&self) -> usize {
        if self.hidden.is_empty() {
            0
        } else {
            self.hidden.len() + 2
        }
    }

    /// The list index of table row `i`.
    fn list_index(&self, i: usize) -> usize {
        i + self.prefix()
    }

    /// Space: the mark on the cursor's row, toggled. With no cursor — a
    /// fresh panel, a filter just cleared — the top row is the row, the
    /// rule `enter` and the arrows already follow.
    fn toggle_cursor_mark(&mut self, cx: &mut Cx, store: &Store) {
        // Without a cursor the first row is the one meant, as it is for
        // every other list key.
        let Some(m) = self
            .cursor_index(store)
            .or(Some(0))
            .and_then(|i| self.table.row(store, i))
        else {
            return;
        };
        self.marks.toggle(m.thread);
        self.redraw(cx);
    }

    /// Shift+arrow: marks the cursor's row, steps, and marks the row it
    /// lands on — a range, by the keys the walk already uses.
    fn mark_and_step(&mut self, cx: &mut Cx, store: &Store, pid: u64, d: isize) {
        self.mark_cursor_row(store);
        self.move_sel(cx, store, pid, d);
        self.mark_cursor_row(store);
        self.redraw(cx);
    }

    /// Marks the row the cursor stands on, if there is one to mark.
    fn mark_cursor_row(&mut self, store: &Store) {
        if let Some(m) = self
            .cursor_index(store)
            .or(Some(0))
            .and_then(|i| self.table.row(store, i))
        {
            self.marks.add(m.thread);
        }
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

    /// List item `idx` as this panel has it — a table row under its own
    /// filter, or one of the marks the filter hides, which ride above the
    /// rows; `None` for the caption and the rule.
    pub fn row_at(&self, store: &Store, idx: usize) -> Option<mail::ThreadHead> {
        let p = self.borrow()?;
        let pre = p.prefix();
        if pre == 0 {
            return p.table.row(store, idx);
        }
        if idx == 0 || idx == pre - 1 {
            return None;
        }
        if idx < pre {
            return p.hidden.get(idx - 1).cloned();
        }
        p.table.row(store, idx - pre)
    }

    /// Whether any row is marked (CR-009): the bar is up, and the chords
    /// the list borrows from its preview stand down.
    pub fn has_marks(&self) -> bool {
        self.borrow().is_some_and(|p| !p.marks.is_empty())
    }

    /// The marked threads, in key order.
    pub fn marks(&self) -> Vec<i64> {
        self.borrow().map_or_else(Vec::new, |p| p.marks.keys())
    }

    /// Toggles one thread's mark — the gutter's click, a long press, a tap
    /// while marks exist.
    pub fn toggle_mark(&self, cx: &mut Cx, thread: i64) {
        if let Some(mut p) = self.borrow_mut() {
            p.marks.toggle(thread);
            p.redraw(cx);
        }
    }

    /// Marks the rows from the cursor's to `thread`'s, both included —
    /// shift+click. Without a cursor, or with the thread outside the
    /// table, just the one.
    pub fn mark_range(&self, cx: &mut Cx, store: &Store, thread: i64) {
        let Some(mut p) = self.borrow_mut() else { return };
        let p = &mut *p;
        let (Some(a), Some(b)) = (p.cursor_index(store), p.index_of_thread(store, thread)) else {
            p.marks.add(thread);
            p.redraw(cx);
            return;
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let keys: Vec<i64> = p.table.rows(store, lo, hi + 1).iter().map(|t| t.thread).collect();
        p.marks.extend(keys);
        p.redraw(cx);
    }

    /// Marks every thread under the filter — `all`, honest about the rows
    /// off screen. A source that cannot list leaves the set as it is.
    pub fn mark_all(&self, cx: &mut Cx, store: &Store) {
        let Some(mut p) = self.borrow_mut() else { return };
        if let Some(keys) = p.table.keys(store) {
            p.marks.extend(keys);
            p.redraw(cx);
        }
    }

    pub fn clear_marks(&self, cx: &mut Cx) {
        if let Some(mut p) = self.borrow_mut() {
            p.marks.clear();
            p.redraw(cx);
        }
    }

    /// Marks these threads again — an undo putting a batch back.
    pub fn add_marks(&self, cx: &mut Cx, keys: &[i64]) {
        if let Some(mut p) = self.borrow_mut() {
            p.marks.extend(keys.iter().copied());
            p.redraw(cx);
        }
    }

    /// Unmarks these threads — what a batch verb filed, or a redo taking
    /// them again.
    pub fn remove_marks(&self, cx: &mut Cx, keys: &[i64]) {
        if let Some(mut p) = self.borrow_mut() {
            for k in keys {
                p.marks.remove(k);
            }
            p.redraw(cx);
        }
    }

    /// Once the `gone` threads are filed: the mail the cursor should land
    /// on — the nearest row that stays, below first, then above. The
    /// walk's own rule, over a set. `None` when the cursor's own row stayed
    /// (it carries on where it stands) or when there is no cursor at all.
    pub fn survivor(
        &self,
        store: &Store,
        gone: &std::collections::BTreeSet<i64>,
    ) -> Option<i64> {
        let p = self.borrow()?;
        let i = p.cursor_index(store)?;
        if !p.table.row(store, i).is_some_and(|t| gone.contains(&t.thread)) {
            return None;
        }
        let n = p.table.len(store);
        (i..n)
            .chain((0..i).rev())
            .filter_map(|j| p.table.row(store, j))
            .find(|t| !gone.contains(&t.thread))
            .map(|t| t.target)
    }

    /// The marks bar's buttons, `(label, rect, verb)`, for the shell's hit
    /// table — none while the set is empty.
    pub fn verb_hits(&self, cx: &mut Cx) -> Vec<(String, Rect, ui::MarkVerb)> {
        let Some(p) = self.borrow() else { return Vec::new() };
        if p.marks.is_empty() {
            return Vec::new();
        }
        p.view.widget(cx, ids!(bar)).as_mark_bar().verb_hits(cx)
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
        self.ac.track(cx, &filter);

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
            // Space marks the cursor's row (CR-009) — the other plain key
            // the grammar keeps, arriving as text the way `/` does. In a
            // live filter it is a space.
            if !filter_focused && t.input == " " {
                self.toggle_cursor_mark(cx, &store);
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
                            cx.action(PanelAction::Open {
                                pid,
                                target: crate::core::Kind::Message { id },
                                fresh: k.modifiers.logo || k.modifiers.alt,
                            });
                        }
                    }
                    // The row walk, with scroll-follow (CR-003: the arrows
                    // are the whole walk now, j/k having gone). Each step
                    // previews what it lands on and keeps the keyboard.
                    // Shift+arrow marks the row it leaves and the row it
                    // lands on: a range, by the walk's own keys (CR-009).
                    KeyCode::ArrowDown if k.modifiers.shift => {
                        self.mark_and_step(cx, &store, pid, 1);
                    }
                    KeyCode::ArrowUp if k.modifiers.shift => {
                        self.mark_and_step(cx, &store, pid, -1);
                    }
                    KeyCode::ArrowDown => self.move_sel(cx, &store, pid, 1),
                    KeyCode::ArrowUp => self.move_sel(cx, &store, pid, -1),
                    // Esc empties the marks — when no field is listening; a
                    // live field keeps its own esc.
                    KeyCode::Escape => {
                        if !self.marks.is_empty() {
                            self.marks.clear();
                            self.redraw(cx);
                        }
                    }
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
                if let Some(PanelAction::Select {
                    pid: p,
                    target: crate::core::Kind::Message { id },
                }) = a.downcast_ref::<PanelAction>()
                {
                    if *p == pid {
                        // The shell moved the cursor for us (a mail opened by
                        // click, or the walk carried past one just filed
                        // away). Take the row from the table so the index
                        // fallback stays honest.
                        if let Some(th) = mail::thread_of(&store, *id) {
                            // A mark the filter hides is outside the table:
                            // opening it moves no cursor.
                            let hidden = self.hidden.iter().any(|h| h.thread == th);
                            match self.index_of_thread(&store, th) {
                                Some(i) => self.sel = Some((th, i)),
                                None if hidden => {}
                                None => self.sel = Some((th, 0)),
                            }
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
        // The marks (CR-009): what the filter shows and what it hides, read
        // fresh by key each draw. A mark whose thread left the inbox
        // altogether goes with it — the bar counts rows that exist.
        let (shown, hidden) = self.table.split(&store, &self.marks);
        self.hidden = hidden
            .iter()
            .filter_map(|k| self.table.by_key(&store, k))
            .collect();
        let kept: std::collections::BTreeSet<i64> = shown
            .into_iter()
            .chain(self.hidden.iter().map(|h| h.thread))
            .collect();
        self.marks.retain(|k| kept.contains(k));
        let bar = self.view.widget(cx, ids!(bar));
        if self.marks.is_empty() {
            bar.set_visible(cx, false);
        } else {
            bar.as_mark_bar()
                .populate(cx, self.marks.len(), n, self.hidden.len());
            bar.set_visible(cx, true);
        }
        let p = self.prefix();
        let mut live: Vec<usize> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, n + p);
                while let Some(idx) = list.next_visible_item(cx) {
                    // The hidden marks ride above the table's rows: the
                    // caption, then the rows, then the rule.
                    if p > 0 && (idx == 0 || idx == p - 1) {
                        let tpl = if idx == 0 { live_id!(caption) } else { live_id!(rule) };
                        let (w, _) = list.item_with_existed(cx, idx, tpl);
                        live.push(idx);
                        w.draw_all(cx, scope);
                        continue;
                    }
                    let (m, marked) = if idx < p {
                        (self.hidden[idx - 1].clone(), true)
                    } else {
                        let Some(m) = self.table.row(&store, idx - p) else { continue };
                        let marked = self.marks.has(&m.thread);
                        (m, marked)
                    };
                    let (row, existed) = list.item_with_existed(cx, idx, live_id!(row));
                    let selected = sel == Some(m.thread);
                    let stamp = (m, selected, marked);
                    if !existed || self.stamps.get(&idx) != Some(&stamp) {
                        row.as_inbox_row().populate(cx, &stamp.0, selected, marked);
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
// EffectRow
// ---------------------------------------------------------------------------

/// The sentence a job's row shows: the effect decoded from its payload and
/// asked to describe itself, or the payload as it stands when this build
/// cannot read the kind — so a row is never nameless, whatever wrote it.
fn job_line(reg: &effect::Registry, j: &Job) -> String {
    reg.describe(&j.kind, &j.payload)
        .unwrap_or_else(|| j.payload.clone())
}

#[derive(Script, ScriptHook, Widget)]
pub struct EffectLine {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for EffectLine {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl EffectLineRef {
    pub fn populate(&self, cx: &mut Cx, j: &Job, what: &str) {
        let Some(inner) = self.borrow() else { return };
        inner.view.label(cx, ids!(kind_lbl)).set_text(cx, &j.kind);
        inner
            .view
            .label(cx, ids!(entity_lbl))
            .set_text(cx, j.entity.as_deref().unwrap_or(""));
        inner
            .view
            .label(cx, ids!(status_lbl))
            .set_text(cx, &j.status_line());
        // Filed at, not last touched: the log is a record of what was asked
        // for, in the order it was asked.
        inner
            .view
            .label(cx, ids!(date_lbl))
            .set_text(cx, &mail::fmt_date(j.created));
        inner.view.label(cx, ids!(what_lbl)).set_text(cx, what);
        let err = inner.view.label(cx, ids!(err_lbl));
        err.set_text(cx, j.error.as_deref().unwrap_or(""));
        err.set_visible(cx, j.error.is_some());
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct EffectRow {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The job this row was last populated for. The shell's hit table asks
    /// the row itself (see [`EffectRowRef::hit`]) rather than re-deriving
    /// the pair from the table, the way `ThreadMsg` answers for its header.
    #[rust]
    job: i64,
    /// Which of the twin lines is the visible one.
    #[rust]
    selected: bool,
}

impl Widget for EffectRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // Clicks resolve through the shell's registered rects, exactly as
        // an inbox row's do — a list item's own area goes stale on any
        // mid-gesture redraw. The row's share is the cursor.
        if let Hit::FingerHoverIn(_) = event.hits(cx, self.view.area()) {
            cx.set_cursor(MouseCursor::Hand);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl EffectRowRef {
    /// The job this row stands for and the sentence it *drew* — read back
    /// off the visible line's own label, not off what the panel meant to
    /// put there. A row whose labels never took their text is addressable
    /// by nothing, which is what makes the scripted click an assertion.
    pub fn hit(&self, cx: &mut Cx) -> Option<(i64, String)> {
        let row = self.borrow()?;
        let drawn = if row.selected {
            row.view.label(cx, ids!(line_sel.what_lbl)).text()
        } else {
            row.view.label(cx, ids!(line.what_lbl)).text()
        };
        (!drawn.is_empty()).then_some((row.job, drawn))
    }

    pub fn populate(&self, cx: &mut Cx, j: &Job, what: &str, selected: bool) {
        let Some(mut row) = self.borrow_mut() else { return };
        row.job = j.id;
        row.selected = selected;
        let line = row.view.widget(cx, ids!(line));
        let line_sel = row.view.widget(cx, ids!(line_sel));
        line.as_effect_line().populate(cx, j, what);
        line_sel.as_effect_line().populate(cx, j, what);
        line.set_visible(cx, !selected);
        line_sel.set_visible(cx, selected);
    }
}

// ---------------------------------------------------------------------------
// JobPanel
// ---------------------------------------------------------------------------

/// One job of the queue, in full: the log's detail panel. It reads its own
/// row on every draw, so a job that finishes while it is open finishes on
/// screen — the same reactive read every other panel does.
#[derive(Script, ScriptHook, Widget)]
pub struct JobPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for JobPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let crate::core::Kind::Job { id } = props.kind else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let job = effect::job(&props.store, id);
        let what = job.as_ref().map(|j| job_line(&props.registry, j));
        let v = &self.view;

        // A job the queue no longer holds — this build cannot invent one, so
        // the panel says what it is looking at and nothing else.
        let Some(j) = job else {
            v.label(cx, ids!(kind_lbl)).set_text(cx, &format!("job #{id}"));
            v.label(cx, ids!(entity_lbl)).set_text(cx, "");
            v.label(cx, ids!(status_lbl)).set_text(cx, "gone");
            v.text_input(cx, ids!(what_txt))
                .set_text(cx, "no such row in the effect queue");
            for path in [ids!(err_row), ids!(reply_block)] {
                v.widget(cx, path).set_visible(cx, false);
            }
            v.text_input(cx, ids!(meta_txt)).set_text(cx, "");
            v.text_input(cx, ids!(payload_txt)).set_text(cx, "");
            return self.view.draw_walk(cx, scope, walk);
        };

        v.label(cx, ids!(kind_lbl)).set_text(cx, &j.kind);
        v.label(cx, ids!(entity_lbl))
            .set_text(cx, j.entity.as_deref().unwrap_or(""));
        v.label(cx, ids!(status_lbl)).set_text(cx, &j.status_line());
        v.text_input(cx, ids!(what_txt))
            .set_text(cx, what.as_deref().unwrap_or(""));
        v.text_input(cx, ids!(err_txt))
            .set_text(cx, j.error.as_deref().unwrap_or(""));
        v.widget(cx, ids!(err_row)).set_visible(cx, j.error.is_some());
        v.text_input(cx, ids!(meta_txt)).set_text(cx, &job_meta(&j));
        v.text_input(cx, ids!(payload_txt)).set_text(cx, &j.payload);
        let reply = j.reply.as_deref().unwrap_or("");
        v.text_input(cx, ids!(reply_txt)).set_text(cx, reply);
        v.widget(cx, ids!(reply_block))
            .set_visible(cx, !reply.is_empty());

        self.view.draw_walk(cx, scope, walk)
    }
}

/// The job's own facts, as its panel lists them: what the row says about
/// itself once the effect has had its say.
fn job_meta(j: &Job) -> String {
    let mut lines = vec![
        format!("#{}", j.id),
        format!("filed {}", mail::fmt_date(j.created)),
        format!("last touched {}", mail::fmt_date(j.updated)),
        format!(
            "{} attempt{}",
            j.attempts,
            if j.attempts == 1 { "" } else { "s" }
        ),
        if j.idempotent {
            "safe to repeat after a crash".to_string()
        } else {
            "not safe to repeat: a crash asks a human".to_string()
        },
    ];
    // Only worth saying while it is still ahead of the job: a closed row's
    // `not_before` is the backoff it never needed again.
    if j.status == "pending" && j.not_before > j.created {
        lines.push(format!("not before {}", mail::fmt_date(j.not_before)));
    }
    lines.join("\n")
}

impl JobPanelRef {
    /// `(label, rect)` for every run that drew something — registered like
    /// any other selectable text, so a payload can be dragged over and
    /// copied. A run with nothing in it is addressable by nothing: an empty
    /// field still reserves its box, so the text is what has to be asked,
    /// and that is what makes a scripted click an assertion.
    pub fn runs(&self, cx: &mut Cx) -> Vec<(String, Rect)> {
        let Some(p) = self.borrow() else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        // `visible` is asked of the enclosing View, never of the run: a
        // TextInput answers that question with a flat `true`.
        for (label, fold, path) in [
            ("job effect", None, ids!(what_txt)),
            ("job error", Some(ids!(err_row)), ids!(err_txt)),
            ("job facts", None, ids!(meta_txt)),
            ("job payload", None, ids!(payload_txt)),
            ("job reply", Some(ids!(reply_block)), ids!(reply_txt)),
        ] {
            if fold.is_some_and(|f| !p.view.widget(cx, f).visible()) {
                continue;
            }
            let w = p.view.widget(cx, path);
            let r = w.area().rect(cx);
            if r.size.x > 0.0 && !w.as_text_input().text().is_empty() {
                hits.push((label.to_string(), r));
            }
        }
        hits
    }
}

// ---------------------------------------------------------------------------
// EffectsPanel
// ---------------------------------------------------------------------------

/// The log's table: the shared engine over the effect queue.
type LogTable = Table<&'static SqlSource<Job, i64>>;

/// One visible row's hit, for the shell's hit table: what a script (and a
/// finger) addresses it by, where it is, and which job it unfolds.
pub struct JobHit {
    pub id: i64,
    pub label: String,
    pub rect: Rect,
}

#[derive(Script, ScriptHook, Widget)]
pub struct EffectsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The autocomplete box, drawn over the rows after everything else.
    #[live]
    suggest: View,
    /// The rich table over `effect::LOG`: the filter and the paging window.
    #[rust(Table::new(&effect::LOG, effect::LOG_PAGE))]
    table: LogTable,
    /// The cursor: the job it stands on, and the row it sat on. The row is
    /// the fallback — jobs arrive at the *top* of this order, so every index
    /// below the newest shifts the moment the executor files one.
    #[rust]
    sel: Option<(Job, usize)>,
    /// What each live row was last populated with, by index.
    #[rust]
    stamps: HashMap<usize, (Job, String, bool)>,
    /// The filter's autocomplete: the table is its completion.
    #[rust]
    ac: Suggest<LogTable>,
}

impl EffectsPanel {
    /// Hands the field's text to the table. The field is the one source of
    /// the filter, exactly as in the inbox.
    fn sync_filter(&mut self, cx: &mut Cx) {
        let text = self.view.text_input(cx, ids!(filter_input)).text();
        if self.table.set_filter(&text) {
            self.sel = None;
        }
    }

    /// Where the cursor stands now: the remembered row if it still holds
    /// the job, else that job's rank (newer work landed above it), else the
    /// row clamped into the table (the filter no longer keeps it).
    fn cursor_index(&self, store: &Store) -> Option<usize> {
        let (j, idx) = self.sel.as_ref()?;
        if self.table.row(store, *idx).is_some_and(|r| r.id == j.id) {
            return Some(*idx);
        }
        if let Some(i) = self.table.index_of(store, j) {
            return Some(i);
        }
        let n = self.table.len(store);
        (n > 0).then(|| (*idx).min(n - 1))
    }

    /// Puts the cursor on row `i` and previews what it lands on — every
    /// cursor move goes through here, so walking and previewing can never
    /// disagree (the inbox's rule, and for the same reason).
    fn set_sel(&mut self, cx: &mut Cx, pid: u64, store: &Store, i: usize) {
        let Some(j) = self.table.row(store, i) else { return };
        let id = j.id;
        self.sel = Some((j, i));
        let list = self.view.widget(cx, ids!(list)).as_portal_list();
        let visible = list
            .borrow()
            .is_some_and(|l| l.items().iter().any(|(idx, _)| *idx == i));
        if !visible {
            list.smooth_scroll_to(cx, i, 90.0, None, 0.0);
        }
        cx.action(PanelAction::Preview {
            pid,
            target: crate::core::Kind::Job { id },
        });
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

impl EffectsPanelRef {
    /// Whether the filter owns the keyboard — the fifth accelerator rule
    /// stands the borrowed chords down while it does.
    pub fn filter_focused(&self, cx: &mut Cx) -> bool {
        self.borrow()
            .is_some_and(|p| p.view.text_input(cx, ids!(filter_input)).key_focus(cx))
    }

    /// The visible rows, as the shell's hit table wants them. The label is
    /// read back off each row widget, so a row that drew nothing is
    /// addressable by nothing.
    pub fn row_hits(&self, cx: &mut Cx) -> Vec<JobHit> {
        let Some(p) = self.borrow() else {
            return Vec::new();
        };
        let list_ref = p.view.widget(cx, ids!(list)).as_portal_list();
        let Some(list) = list_ref.borrow() else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        for (_, item) in list.items().iter() {
            let rect = item.widget.area().rect(cx);
            if rect.size.x <= 0.0 {
                continue;
            }
            if let Some((id, label)) = item.widget.as_effect_row().hit(cx) {
                hits.push(JobHit { id, label, rect });
            }
        }
        hits
    }

    /// The open autocomplete's rows, `(label, rect)`, for the shell's hit
    /// table — a click on one is [`EffectsPanelRef::pick`].
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

impl Widget for EffectsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let filter = self.view.text_input(cx, ids!(filter_input));
        let filter_focused = filter.key_focus(cx);
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);

        // The autocomplete owns the arrows, enter, tab and esc while it is
        // open; the field never sees them.
        if let Event::KeyDown(k) = event {
            if self.ac.key(cx, &self.table, &filter, k) {
                self.redraw(cx);
                return;
            }
        }
        self.view.handle_event(cx, event, scope);
        let Some(store) = panel_store(scope) else { return };

        if let Event::TextInput(t) = event {
            if !filter_focused && t.input == "/" {
                focus_input(cx, &filter);
            }
        }
        if let Event::KeyDown(k) = event {
            if !filter_focused {
                match k.key_code {
                    KeyCode::ReturnKey => {
                        let id = self
                            .cursor_index(&store)
                            .or(Some(0))
                            .and_then(|i| self.table.row(&store, i))
                            .map(|j| j.id);
                        if let Some(id) = id {
                            // Enter *goes*: unlike the walk's preview, it
                            // hands focus to the job (the solid-link rule).
                            cx.action(PanelAction::Open {
                                pid,
                                target: crate::core::Kind::Job { id },
                                fresh: k.modifiers.logo || k.modifiers.alt,
                            });
                        }
                    }
                    // The row walk, with scroll-follow. Each step previews
                    // what it lands on and keeps the keyboard.
                    KeyCode::ArrowDown => self.move_sel(cx, &store, pid, 1),
                    KeyCode::ArrowUp => self.move_sel(cx, &store, pid, -1),
                    KeyCode::Tab => focus_input(cx, &filter),
                    _ => {}
                }
            }
        }
        if let Event::Actions(actions) = event {
            if filter.key_focus_lost(actions) {
                filter.set_cursor(cx, filter.cursor(), false);
            }
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
            if self.view.widget(cx, ids!(list)).as_portal_list().reached_end(actions)
                && self.table.extend(&store)
            {
                self.redraw(cx);
            }
            for a in actions {
                if let Some(PanelAction::Select {
                    pid: p,
                    target: crate::core::Kind::Job { id },
                }) = a.downcast_ref::<PanelAction>()
                {
                    if *p == pid {
                        // The shell moved the cursor for us (a job opened by
                        // click). `index_of` confirms its answer by comparing
                        // the whole row, so it has to be handed the real one —
                        // a stub carrying only the id ranks correctly and is
                        // then rejected, and the cursor would never move.
                        if let Some(j) = effect::job(&store, *id) {
                            if let Some(i) = self.table.index_of(&store, &j) {
                                self.sel = Some((j, i));
                                self.redraw(cx);
                            }
                        }
                    }
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let store = props.store.clone();
        let reg = props.registry.clone();
        self.sync_filter(cx);
        let filter = self.view.text_input(cx, ids!(filter_input));
        let focused = filter.key_focus(cx);
        let err = if focused {
            self.table.errors_while_typing().first().map(|e| e.message.clone())
        } else {
            self.table.errors().first().map(|e| e.message.clone())
        };
        let err_lbl = self.view.label(cx, ids!(filter_err_lbl));
        err_lbl.set_text(cx, err.as_deref().unwrap_or(""));
        err_lbl.set_visible(cx, err.is_some());

        let n = self.table.len(&store);
        let empty = self.view.label(cx, ids!(empty_lbl));
        empty.set_text(
            cx,
            if self.table.filter().trim().is_empty() {
                "nothing has left the process yet"
            } else {
                "no effect under this filter"
            },
        );
        empty.set_visible(cx, n == 0 && err.is_none());

        let sel = self.sel.as_ref().map(|(j, _)| j.id);
        let mut live: Vec<usize> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, n);
                while let Some(idx) = list.next_visible_item(cx) {
                    let Some(j) = self.table.row(&store, idx) else { continue };
                    let (row, existed) = list.item_with_existed(cx, idx, live_id!(row));
                    let stamp = (j.clone(), job_line(&reg, &j), sel == Some(j.id));
                    if !existed || self.stamps.get(&idx) != Some(&stamp) {
                        row.as_effect_row().populate(cx, &stamp.0, &stamp.1, stamp.2);
                        self.stamps.insert(idx, stamp);
                    }
                    live.push(idx);
                    row.draw_all(cx, scope);
                }
            }
        }
        self.stamps.retain(|k, _| live.contains(k));
        self.ac
            .draw(cx, scope, &store, &self.table, &filter, &mut self.suggest);
        DrawStep::done()
    }
}

// ---------------------------------------------------------------------------
// Files (CR-008)
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct FilesLine {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for FilesLine {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl FilesLineRef {
    pub fn populate(&self, cx: &mut Cx, e: &files::Entry) {
        let Some(inner) = self.borrow() else { return };
        inner.view.label(cx, ids!(name_lbl)).set_text(cx, &e.label());
        let size = if e.is_dir { "—".to_string() } else { files::fmt_size(e.size) };
        inner.view.label(cx, ids!(size_lbl)).set_text(cx, &size);
        inner
            .view
            .label(cx, ids!(date_lbl))
            .set_text(cx, &mail::fmt_date(e.modified));
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FilesRow {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for FilesRow {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // Clicks resolve through the shell's registered rects, as an
        // inbox row's do; the row's share is the hand.
        if let Hit::FingerHoverIn(_) = event.hits(cx, self.view.area()) {
            cx.set_cursor(MouseCursor::Hand);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl FilesRowRef {
    pub fn populate(&self, cx: &mut Cx, e: &files::Entry, selected: bool) {
        let Some(row) = self.borrow() else { return };
        let line = row.view.widget(cx, ids!(line));
        let line_sel = row.view.widget(cx, ids!(line_sel));
        line.as_files_line().populate(cx, e);
        line_sel.as_files_line().populate(cx, e);
        line.set_visible(cx, !selected);
        line_sel.set_visible(cx, selected);
    }
}

/// The files panel's table: the shared engine over one directory.
type FilesTable = Table<files::DirSource>;

#[derive(Script, ScriptHook, Widget)]
pub struct FilesPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The filter's autocomplete box, drawn over the rows last.
    #[live]
    suggest: View,
    /// The path field's box, under that field.
    #[live]
    suggest_path: View,
    /// The rich table over the directory the panel's params name — its
    /// listing read through the outside when the panel landed on it; a
    /// replace onto another directory lists again.
    #[rust(Table::new(files::DirSource::new(files::HOME, Vec::new()), files::PAGE))]
    table: FilesTable,
    /// The world the last event or draw handed in, so a pick from the
    /// shell can complete a path through the same outside.
    #[rust]
    world: Option<std::rc::Rc<crate::effect::World>>,
    /// Whether the current directory has been listed at all.
    #[rust]
    listed: bool,
    /// The cursor: the entry's name, and the row it sat on.
    #[rust]
    sel: Option<(String, usize)>,
    #[rust]
    ac: Suggest<FilesTable>,
    /// The path field's completion: segment by segment, like a shell.
    #[rust]
    pac: Suggest<files::PathCompletion>,
    /// The `go to` field is up, in the crumbs' place.
    #[rust]
    path_open: bool,
    /// The path field wants the keyboard once it has been drawn in place.
    #[rust]
    focus_path_pending: bool,
    /// What the panel could not do, until the next verb.
    #[rust]
    status: Option<String>,
    /// The `new dir` field is up.
    #[rust]
    newdir_open: bool,
    /// The `new dir` field wants the keyboard once it has been drawn in
    /// place — focus set on a field with no area yet lands nowhere.
    #[rust]
    focus_pending: bool,
    /// The hidden field has been drawn once (see `draw_walk`): a
    /// never-drawn TextInput has `Area::Empty`, and makepad's
    /// `has_key_focus(Area::Empty)` is *true* whenever nothing has focus —
    /// so the phantom would clear the focus the filter just took on the
    /// mouse-up it still receives (only mouse-down is gated on
    /// visibility).
    #[rust]
    primed: bool,
}

impl FilesPanel {
    fn dir_of(scope: &Scope) -> Option<String> {
        match scope.props.get::<PanelProps>().map(|p| &p.kind) {
            Some(crate::core::Kind::Files { dir }) => Some(dir.clone()),
            _ => None,
        }
    }

    /// Follows the panel's params: a crumb replaced it onto an ancestor,
    /// or a preview re-aimed it. Everything that was about the old
    /// directory starts over: the filter (the field too, since the field
    /// is the filter's one source), the cursor, the status line, and a
    /// `new dir` row left open.
    fn sync_dir(&mut self, cx: &mut Cx, world: &crate::effect::World, dir: &str) {
        if self.table.source().dir == dir && self.listed {
            return;
        }
        self.relist(world, dir);
        self.sel = None;
        self.view.text_input(cx, ids!(filter_input)).set_text(cx, "");
        if self.newdir_open {
            let input = self.view.text_input(cx, ids!(newdir_input));
            if input.key_focus(cx) {
                cx.set_key_focus(Area::Empty);
            }
            input.set_text(cx, "");
            self.newdir_open = false;
            self.view.view(cx, ids!(newdir)).set_visible(cx, false);
        }
        self.focus_pending = false;
        if self.path_open {
            let input = self.view.text_input(cx, ids!(path_input));
            if input.key_focus(cx) {
                cx.set_key_focus(Area::Empty);
            }
            self.set_path_open(cx, false);
        }
        self.focus_path_pending = false;
    }

    /// Reads the directory through the outside and puts the listing under
    /// the table — the filter as typed stays. A directory the outside
    /// cannot list (gone, unreadable, a world with no outside) leaves an
    /// empty table and says why on the status line.
    fn relist(&mut self, world: &crate::effect::World, dir: &str) {
        let (entries, err) = match files::list_in(world, dir) {
            Ok(v) => (v, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let text = self.table.filter().to_string();
        self.table = Table::new(files::DirSource::new(dir, entries), files::PAGE);
        self.table.set_filter(&text);
        self.status = err;
        self.listed = true;
    }

    /// Swaps the crumbs for the path field, or back.
    fn set_path_open(&mut self, cx: &mut Cx, open: bool) {
        self.path_open = open;
        self.view.view(cx, ids!(path_row)).set_visible(cx, open);
        self.view.view(cx, ids!(crumbs)).set_visible(cx, !open);
    }

    fn close_path(&mut self, cx: &mut Cx) {
        self.set_path_open(cx, false);
        self.focus_path_pending = false;
        cx.set_key_focus(Area::Empty);
        self.redraw(cx);
    }

    /// Enter in the path field: a directory replaces the panel in place
    /// (the crumbs' own semantics), a file opens its card joined, and a
    /// path the tree does not have is refused on the status line.
    fn go_to(&mut self, cx: &mut Cx, pid: u64, world: &crate::effect::World, typed: &str) {
        let Some(path) = files::normalize(typed) else {
            self.status = Some(format!("not a path: {}", typed.trim()));
            self.redraw(cx);
            return;
        };
        let there = files::stat_in(world, &path);
        if there.as_ref().is_some_and(|e| e.is_dir) {
            cx.action(PanelAction::FollowLink {
                pid,
                target: crate::core::Kind::Files { dir: path },
                dotted: true,
                fresh: false,
            });
            self.close_path(cx);
        } else if there.is_some() {
            cx.action(PanelAction::FollowLink {
                pid,
                target: crate::core::Kind::File { path },
                dotted: false,
                fresh: false,
            });
            self.close_path(cx);
        } else {
            self.status = Some(format!("no such path: {path}"));
            self.redraw(cx);
        }
    }

    fn sync_filter(&mut self, cx: &mut Cx) {
        let text = self.view.text_input(cx, ids!(filter_input)).text();
        if self.table.set_filter(&text) {
            self.sel = None;
        }
    }

    /// Where the cursor stands: the remembered row if it still holds the
    /// name, else the name's row, else the row clamped into the table.
    fn cursor_index(&self, store: &Store) -> Option<usize> {
        let (name, idx) = self.sel.as_ref()?;
        if self.table.row(store, *idx).is_some_and(|e| &e.name == name) {
            return Some(*idx);
        }
        let n = self.table.len(store);
        (0..n)
            .find(|&i| self.table.row(store, i).is_some_and(|e| &e.name == name))
            .or_else(|| (n > 0).then(|| (*idx).min(n - 1)))
    }

    /// Puts the cursor on row `i` and previews what it names — every
    /// cursor move goes through here, so walking and previewing can never
    /// disagree (the inbox's rule).
    fn set_sel(&mut self, cx: &mut Cx, pid: u64, store: &Store, i: usize) {
        let Some(e) = self.table.row(store, i) else { return };
        let target = Self::target_of(&self.table.source().dir, &e);
        self.sel = Some((e.name, i));
        let list = self.view.widget(cx, ids!(list)).as_portal_list();
        let visible = list
            .borrow()
            .is_some_and(|l| l.items().iter().any(|(idx, _)| *idx == i));
        if !visible {
            list.smooth_scroll_to(cx, i, 90.0, None, 0.0);
        }
        cx.action(PanelAction::Preview { pid, target });
        self.redraw(cx);
    }

    fn move_sel(&mut self, cx: &mut Cx, pid: u64, store: &Store, d: isize) {
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

    /// The kind a row opens: a directory as a column, a file as a card.
    fn target_of(dir: &str, e: &files::Entry) -> crate::core::Kind {
        let path = files::join(dir, &e.name);
        if e.is_dir {
            crate::core::Kind::Files { dir: path }
        } else {
            crate::core::Kind::File { path }
        }
    }

    fn close_newdir(&mut self, cx: &mut Cx) {
        self.newdir_open = false;
        self.view.view(cx, ids!(newdir)).set_visible(cx, false);
        cx.set_key_focus(Area::Empty);
        self.redraw(cx);
    }
}

impl FilesPanelRef {
    /// Row `i` of the table as this panel has it — its own filter included.
    pub fn row_at(&self, store: &Store, i: usize) -> Option<files::Entry> {
        self.borrow().and_then(|p| p.table.row(store, i))
    }

    /// What row `i` opens.
    pub fn target_at(&self, store: &Store, i: usize) -> Option<crate::core::Kind> {
        let p = self.borrow()?;
        let e = p.table.row(store, i)?;
        Some(FilesPanel::target_of(&p.table.source().dir, &e))
    }

    /// Whether one of the panel's fields owns the keyboard, so borrowed
    /// chords stand down (the fifth accelerator rule).
    pub fn field_focused(&self, cx: &mut Cx) -> bool {
        self.borrow().is_some_and(|p| {
            p.view.text_input(cx, ids!(filter_input)).key_focus(cx)
                || p.view.text_input(cx, ids!(newdir_input)).key_focus(cx)
                || p.view.text_input(cx, ids!(path_input)).key_focus(cx)
        })
    }

    /// The `go to` button: the crumbs become a path field, prefilled with
    /// where the panel stands and a slash, so the offer opens on this
    /// directory's entries at once.
    pub fn open_path(&self, cx: &mut Cx) {
        let Some(mut p) = self.borrow_mut() else { return };
        p.status = None;
        let dir = p.table.source().dir.clone();
        let seed = if dir == files::ROOT { dir } else { format!("{dir}/") };
        p.view.text_input(cx, ids!(path_input)).set_text(cx, &seed);
        p.set_path_open(cx, true);
        p.focus_path_pending = true;
        p.redraw(cx);
    }

    /// Whether the path field is up — its rect counts only then.
    pub fn path_open(&self) -> bool {
        self.borrow().is_some_and(|p| p.path_open)
    }

    /// The `new dir` button: raise the field and put the caret in it.
    pub fn open_new_dir(&self, cx: &mut Cx) {
        let Some(mut p) = self.borrow_mut() else { return };
        p.status = None;
        p.newdir_open = true;
        p.view.view(cx, ids!(newdir)).set_visible(cx, true);
        p.view.text_input(cx, ids!(newdir_input)).set_text(cx, "");
        // The keyboard follows on the next event, once the row has been
        // drawn where it will stand.
        p.focus_pending = true;
        p.redraw(cx);
    }

    /// Whether the `new dir` field is up — its rect counts only then.
    pub fn new_dir_open(&self) -> bool {
        self.borrow().is_some_and(|p| p.newdir_open)
    }

    /// The shell's word on what a verb could not do.
    pub fn set_status(&self, cx: &mut Cx, msg: Option<String>) {
        let Some(mut p) = self.borrow_mut() else { return };
        p.status = msg;
        p.redraw(cx);
    }

    /// The crumbs on screen, `(label, rect, the directory it replaces
    /// with)`, for the shell's hit table.
    pub fn crumb_hits(&self, cx: &mut Cx) -> Vec<(String, Rect, crate::core::Kind)> {
        let Some(p) = self.borrow() else { return Vec::new() };
        let crumbs = files::crumbs(&p.table.source().dir);
        let n = crumbs.len();
        let ancestors: Vec<&(String, String)> =
            crumbs[..n.saturating_sub(1)].iter().rev().take(4).rev().collect();
        let slots = [ids!(crumbs.c0), ids!(crumbs.c1), ids!(crumbs.c2), ids!(crumbs.c3)];
        ancestors
            .iter()
            .zip(slots.iter())
            .filter_map(|((label, path), slot)| {
                let r = p.view.widget(cx, *slot).area().rect(cx);
                (r.size.x > 0.0).then(|| {
                    (
                        label.clone(),
                        r,
                        crate::core::Kind::Files { dir: path.clone() },
                    )
                })
            })
            .collect()
    }

    /// The open autocomplete's rows, `(label, rect)` — the filter's box
    /// or the path field's, whichever is up.
    pub fn suggestion_hits(&self, cx: &mut Cx) -> Vec<(String, Rect)> {
        self.borrow().map_or_else(Vec::new, |p| {
            if p.pac.open() {
                p.pac.hits(cx, &p.suggest_path)
            } else {
                p.ac.hits(cx, &p.suggest)
            }
        })
    }

    /// Commits the `i`-th suggestion on offer, in whichever box is up.
    pub fn pick(&self, cx: &mut Cx, i: usize) {
        let Some(mut p) = self.borrow_mut() else { return };
        let p = &mut *p;
        if p.pac.open() {
            let Some(world) = p.world.clone() else { return };
            let path = p.view.text_input(cx, ids!(path_input));
            p.pac.pick(cx, &files::PathCompletion { world }, &path, i);
        } else {
            let filter = p.view.text_input(cx, ids!(filter_input));
            p.ac.pick(cx, &p.table, &filter, i);
        }
    }
}

impl Widget for FilesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let filter = self.view.text_input(cx, ids!(filter_input));
        let newdir_input = self.view.text_input(cx, ids!(newdir_input));
        let path_input = self.view.text_input(cx, ids!(path_input));
        // The deferred focus of `new dir` and `go to`: the row is drawn
        // now, so the field has a place to take the keyboard at. The path
        // field keeps its seed: the caret lands at the end, not over a
        // selection that the first letter would replace.
        if self.focus_pending && newdir_input.area().rect(cx).size.y > 0.0 {
            self.focus_pending = false;
            focus_input(cx, &newdir_input);
        }
        if self.focus_path_pending && path_input.area().rect(cx).size.y > 0.0 {
            self.focus_path_pending = false;
            path_input.set_key_focus(cx);
            let end = path_input.text().len();
            path_input.set_cursor(cx, Cursor { index: end, prefer_next_row: false }, false);
        }
        let filter_focused = filter.key_focus(cx);
        let newdir_focused = newdir_input.key_focus(cx);
        let path_focused = path_input.key_focus(cx);
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
        if let Some(p) = scope.props.get::<PanelProps>() {
            self.world = Some(p.world.clone());
        }
        let Some(world) = self.world.clone() else { return };
        self.ac.track(cx, &filter);
        self.pac.track(cx, &path_input);

        if let Event::KeyDown(k) = event {
            // Enter in the path field goes to what is typed when that is
            // a directory the disk has — even with the offer open on its
            // entries; tab takes the offer, the shell's way.
            if path_focused && k.key_code == KeyCode::ReturnKey {
                let typed = path_input.text();
                let is_dir =
                    files::normalize(&typed).is_some_and(|p| files::is_dir_in(&world, &p));
                if is_dir || !self.pac.open() {
                    self.go_to(cx, pid, &world, &typed);
                    return;
                }
            }
            let pc = files::PathCompletion {
                world: world.clone(),
            };
            if self.pac.key(cx, &pc, &path_input, k) {
                self.redraw(cx);
                return;
            }
            if self.ac.key(cx, &self.table, &filter, k) {
                self.redraw(cx);
                return;
            }
        }
        self.view.handle_event(cx, event, scope);
        let Some(store) = panel_store(scope) else { return };
        let dir = self.table.source().dir.clone();

        // `/` focuses the filter, as in the inbox.
        if let Event::TextInput(t) = event {
            if !filter_focused && !newdir_focused && !path_focused && t.input == "/" {
                focus_input(cx, &filter);
            }
        }
        if let Event::KeyDown(k) = event {
            if !filter_focused && !newdir_focused && !path_focused {
                match k.key_code {
                    // Enter *goes*: the row's target opens with focus, the
                    // solid-link rule. The walk's preview is the
                    // shell's; the panel only moves its cursor.
                    KeyCode::ReturnKey => {
                        let target = self
                            .cursor_index(&store)
                            .or(Some(0))
                            .and_then(|i| self.table.row(&store, i))
                            .map(|e| Self::target_of(&dir, &e));
                        if let Some(target) = target {
                            cx.action(PanelAction::FollowLink {
                                pid,
                                target,
                                dotted: false,
                                fresh: k.modifiers.logo || k.modifiers.alt,
                            });
                        }
                    }
                    KeyCode::ArrowDown => self.move_sel(cx, pid, &store, 1),
                    KeyCode::ArrowUp => self.move_sel(cx, pid, &store, -1),
                    KeyCode::Tab => focus_input(cx, &filter),
                    _ => {}
                }
            }
        }
        if let Event::Actions(actions) = event {
            if filter.key_focus_lost(actions) {
                filter.set_cursor(cx, filter.cursor(), false);
            }
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
            // The `new dir` field: enter creates, esc puts it away. A
            // name that exists, or holds a separator, is refused.
            if newdir_input.returned(actions).is_some() {
                let name = newdir_input.text().trim().to_string();
                if name.is_empty() {
                    self.close_newdir(cx);
                } else if name.contains('/') {
                    self.status = Some("a name cannot hold a slash".into());
                    self.redraw(cx);
                } else if files::stat_in(&world, &files::join(&dir, &name)).is_some() {
                    self.status = Some(format!("{name} is already here"));
                    self.redraw(cx);
                } else {
                    cx.action(PanelAction::NewDir { pid, dir: dir.clone(), name });
                    self.close_newdir(cx);
                }
            }
            if newdir_input.escaped(actions) {
                self.close_newdir(cx);
            }
            // The path field: enter with no offer up goes; esc puts the
            // crumbs back.
            if path_input.returned(actions).is_some() {
                let typed = path_input.text();
                self.go_to(cx, pid, &world, &typed);
            }
            if path_input.escaped(actions) {
                self.close_path(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(store) = panel_store(scope) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let pid = scope.props.get::<PanelProps>().map_or(0, |p| p.pid);
        if let Some(p) = scope.props.get::<PanelProps>() {
            self.world = Some(p.world.clone());
        }
        let Some(world) = self.world.clone() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(dir) = Self::dir_of(scope) {
            self.sync_dir(cx, &world, &dir);
        }
        let dir = self.table.source().dir.clone();
        self.sync_filter(cx);

        // The crumbs: the last four ancestors as dotted links, the
        // directory itself plain.
        let crumbs = files::crumbs(&dir);
        let n = crumbs.len();
        let ancestors: Vec<&(String, String)> =
            crumbs[..n.saturating_sub(1)].iter().rev().take(4).rev().collect();
        let slots = [ids!(crumbs.c0), ids!(crumbs.c1), ids!(crumbs.c2), ids!(crumbs.c3)];
        let seps = [ids!(crumbs.s0), ids!(crumbs.s1), ids!(crumbs.s2), ids!(crumbs.s3)];
        for (i, (slot, sep)) in slots.iter().zip(seps.iter()).enumerate() {
            let link = self.view.link(cx, *slot);
            let shown = ancestors.get(i).is_some();
            // The disk's root is its own separator: `/ tmp`, not `/ / tmp`.
            let sep_shown = shown && ancestors.get(i).is_some_and(|(l, _)| l != files::ROOT);
            if let Some((label, path)) = ancestors.get(i) {
                link.set(
                    cx,
                    pid,
                    label,
                    crate::core::Kind::Files { dir: (*path).clone() },
                    true,
                );
            }
            self.view.widget(cx, *slot).set_visible(cx, shown);
            self.view.widget(cx, *sep).set_visible(cx, sep_shown);
        }
        let here = crumbs.last().map(|(l, _)| l.clone()).unwrap_or_default();
        self.view.label(cx, ids!(crumbs.here_lbl)).set_text(cx, &here);

        let filter = self.view.text_input(cx, ids!(filter_input));
        let focused = filter.key_focus(cx);
        let err = if focused {
            self.table.errors_while_typing().first().map(|e| e.message.clone())
        } else {
            self.table.errors().first().map(|e| e.message.clone())
        };
        let err_lbl = self.view.label(cx, ids!(err_lbl));
        err_lbl.set_text(cx, err.as_deref().unwrap_or(""));
        err_lbl.set_visible(cx, err.is_some());

        // A directory the outside could not list says why — gone,
        // unreadable, no outside at all; the crumbs still climb out.
        let status = self.status.clone();
        let status_lbl = self.view.label(cx, ids!(status_lbl));
        status_lbl.set_text(cx, status.as_deref().unwrap_or(""));
        status_lbl.set_visible(cx, status.is_some());

        // Prime the hidden `new dir` and `go to` rows with one draw into a
        // zero-size clipped turtle, so their fields own a real area from
        // the first frame and never pass for the focused one (see
        // `primed`).
        if !self.primed {
            self.primed = true;
            let at = cx.turtle().pos();
            for (row, open) in [(ids!(newdir), self.newdir_open), (ids!(path_row), self.path_open)] {
                if open {
                    continue;
                }
                let w = self.view.widget(cx, row);
                w.set_visible(cx, true);
                cx.begin_turtle(
                    Walk::abs_rect(Rect {
                        pos: at,
                        size: DVec2::default(),
                    }),
                    Layout {
                        clip_x: true,
                        clip_y: true,
                        ..Default::default()
                    },
                );
                w.draw_all(cx, scope);
                cx.end_turtle();
                w.set_visible(cx, false);
            }
        }

        let sel = self.sel.as_ref().map(|(name, _)| name.clone());
        let n = self.table.len(&store);
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, n);
                while let Some(idx) = list.next_visible_item(cx) {
                    let Some(e) = self.table.row(&store, idx) else { continue };
                    let (row, _) = list.item_with_existed(cx, idx, live_id!(row));
                    row.as_files_row()
                        .populate(cx, &e, sel.as_deref() == Some(e.name.as_str()));
                    row.draw_all(cx, scope);
                }
            }
        }
        self.ac
            .draw(cx, scope, &store, &self.table, &filter, &mut self.suggest);
        // The path field's offer, under it and over the rows.
        let path_input = self.view.text_input(cx, ids!(path_input));
        let pc = files::PathCompletion { world };
        self.pac
            .draw(cx, scope, &store, &pc, &path_input, &mut self.suggest_path);
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FilePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The path the card was last filled for, so a preview is decoded once.
    #[rust]
    shown: Option<String>,
}

impl Widget for FilePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let (path, world) = match scope.props.get::<PanelProps>() {
            Some(p) => match &p.kind {
                crate::core::Kind::File { path } => (Some(path.clone()), Some(p.world.clone())),
                _ => (None, None),
            },
            None => (None, None),
        };
        if let (Some(path), Some(world)) = (path, world) {
            if self.shown.as_deref() != Some(path.as_str()) {
                let v = &self.view;
                v.label(cx, ids!(name_lbl)).set_text(cx, files::basename(&path));
                v.text_input(cx, ids!(path_txt)).set_text(cx, &path);
                let text_prev = v.text_input(cx, ids!(text_box.text_prev));
                let text_box = v.view(cx, ids!(text_box));
                let img_prev = v.widget(cx, ids!(img_box.img_prev));
                let img_box = v.view(cx, ids!(img_box));
                let none_lbl = v.label(cx, ids!(none_lbl));
                match files::stat_in(&world, &path) {
                    Some(e) => {
                        v.label(cx, ids!(kind_lbl)).set_text(
                            cx,
                            &format!("{} · {}", e.kind().word(), files::fmt_size(e.size)),
                        );
                        v.label(cx, ids!(when_lbl))
                            .set_text(cx, &format!("modified {}", mail::fmt_date(e.modified)));
                        // The preview: the first 64 KB of a text file in the
                        // app's one face; a PNG or a JPEG decoded at up to
                        // 20 MB; anything else is the card alone.
                        let (mut text, mut image) = (None, false);
                        match e.kind() {
                            files::FileKind::Text => {
                                if let Ok(bytes) =
                                    files::read_in(&world, &path, files::TEXT_PREVIEW_MAX)
                                {
                                    text = Some(String::from_utf8_lossy(&bytes).into_owned());
                                }
                            }
                            files::FileKind::Image => {
                                // The name says whether to read it; the
                                // bytes say how to decode it.
                                let bytes = files::image_format(&e.name).and_then(|_| {
                                    files::read_in(&world, &path, files::IMAGE_PREVIEW_MAX).ok()
                                });
                                if let Some(bytes) = bytes {
                                    let img = img_prev.as_image();
                                    image = match files::sniff(&bytes) {
                                        Some(files::ImageFormat::Png) => {
                                            img.load_png_from_data(cx, &bytes).is_ok()
                                        }
                                        Some(files::ImageFormat::Jpeg) => {
                                            img.load_jpg_from_data(cx, &bytes).is_ok()
                                        }
                                        None => false,
                                    };
                                }
                            }
                            _ => {}
                        }
                        text_prev.set_text(cx, text.as_deref().unwrap_or(""));
                        text_box.set_visible(cx, text.is_some());
                        img_box.set_visible(cx, image);
                        none_lbl.set_visible(cx, text.is_none() && !image);
                    }
                    None => {
                        v.label(cx, ids!(kind_lbl)).set_text(cx, "gone");
                        v.label(cx, ids!(when_lbl)).set_text(cx, "");
                        text_box.set_visible(cx, false);
                        img_box.set_visible(cx, false);
                        none_lbl.set_visible(cx, false);
                    }
                }
                self.shown = Some(path);
            }
        }
        self.view.draw_walk(cx, scope, walk)
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
    /// Whether the last draw saw key focus on this link; the underline
    /// variants flip only when it changes.
    #[rust]
    focused: bool,
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
        let focused = cx.has_key_focus(self.view.area());
        if focused != self.focused {
            self.focused = focused;
            let solid = !self.dotted;
            self.view.view(cx, ids!(ul)).set_visible(cx, solid && !focused);
            self.view.view(cx, ids!(ul_focus)).set_visible(cx, solid && focused);
        }
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
        let (pre, key, post) = ui::split_accel(text, accel);
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

    /// A solid link with no target of its own: the same underline, but the
    /// tap is the *row's* to answer, through the shell's hit table. The
    /// problems panel's *reopen* is one — it opens a panel, so the grammar
    /// makes it a link, but what it opens carries a draft along, which no
    /// `Kind` can say.
    pub fn set_label(&self, cx: &mut Cx, text: &str) {
        let Some(mut l) = self.borrow_mut() else { return };
        l.target = None;
        l.dotted = false;
        let pre_l = l.view.label(cx, ids!(row.pre));
        pre_l.set_text(cx, text);
        pre_l.set_visible(cx, true);
        let key_l = l.view.label(cx, ids!(row.key));
        key_l.set_text(cx, "");
        key_l.set_visible(cx, false);
        let post_l = l.view.label(cx, ids!(row.post));
        post_l.set_text(cx, "");
        post_l.set_visible(cx, false);
        l.view.view(cx, ids!(ul)).set_visible(cx, true);
        l.view.view(cx, ids!(ul_dotted)).set_visible(cx, false);
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
            // Its images are filed under its own name (see `Pictures`).
            let h = crate::html::scope_cids(h, &format!("m{}", m.head.id));
            let (own, q) = mail::split_quote_html(&h);
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
        v.label(cx, ids!(fwd_lbl)).set_visible(cx, m.forwarded);
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
// Pictures: the images the open letters show
// ---------------------------------------------------------------------------

/// The bytes of every image an open letter or article refers to, by the
/// name [`pic_key`] files it under — fetched, read or un-base64'd by
/// whoever has them, and read back by [`HtmlImage`] as it draws. Lives on
/// `Cx` as a global: the items are minted by the `Html` widget from a
/// template and can reach nothing else.
///
/// Nothing here happens in the frame that first shows a picture. A letter's
/// own `cid:` parts come off a reader thread with its own connection to the
/// database the asking panel reads (CR-005 phase 0); a `data:` payload is
/// un-base64'd on that same thread; an image on the web is an ordinary HTTP
/// request. All three land in [`pictures_landed`], which redraws. The decode
/// from those bytes to a texture is makepad's, on its own pool, and lands
/// there too.
///
/// The names are one flat space over every store a process has open. That
/// is the panels library's business alone — a mount's world is its own
/// in-memory database, and two mounts can hold different letters under the
/// same id — and no scene of the catalogue has a `cid:` picture in it. A
/// mount that grows one wants its store's identity in the scope
/// `scope_cids` writes, not just the mail's.
#[derive(Default)]
pub struct Pictures {
    bytes: HashMap<String, Arc<[u8]>>,
    /// Requests out on the network, by request id.
    inflight: HashMap<LiveId, String>,
    /// Sources that did not arrive or did not decode: asked once, not again.
    failed: HashSet<String>,
    /// Jobs handed to the reader thread — a mail whose raw is being taken
    /// apart (`m{id}`), a `data:` source being un-base64'd. Asked once.
    asked: HashSet<String>,
    /// The reader thread, started with the first letter that has a picture
    /// in it.
    reader: Option<mpsc::Sender<PicJob>>,
}

/// One piece of work for the reader thread — the two ways a picture's bytes
/// are had without the network.
enum PicJob {
    /// Take one mail's raw apart: the `cid:` parts its HTML refers to.
    /// The database comes with the job rather than being held here: a
    /// panels-library mount boots a stage over a world of its own, and a
    /// reader that had bound one database at startup would answer every
    /// later panel out of whichever store happened to ask first.
    Cid { db: Arc<crate::store::Db>, mid: i64 },
    /// Un-base64 one `data:` source, filed under `key`.
    Data { key: String, src: String },
}

/// What the reader thread found, on its way back to the UI thread.
struct PicturesReady {
    items: Vec<(String, Arc<[u8]>)>,
    failed: Vec<String>,
    /// Jobs to forget having asked: the work found nothing, and the reason
    /// may not last — a letter whose raw has not been stored yet is worth
    /// asking about again the next time it opens.
    retry: Vec<String>,
}

impl std::fmt::Debug for PicturesReady {
    /// The bytes themselves are megabytes and never worth printing; what an
    /// action log wants is which sources landed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PicturesReady")
            .field("items", &self.items.iter().map(|(k, _)| k).collect::<Vec<_>>())
            .field("failed", &self.failed)
            .field("retry", &self.retry)
            .finish()
    }
}

/// The name a source is filed under: its own, unless it carries its bytes
/// inside it. A `data:` URL *is* its payload — megabytes of map key, and of
/// texture-cache path, for what sixteen hex characters name just as well.
fn pic_key(src: &str) -> String {
    if src.starts_with("data:") {
        format!("data:{:016x}", LiveId::from_str(src).0)
    } else {
        src.to_string()
    }
}

impl Pictures {
    /// The reader thread, started on first need. `None` under
    /// `MAKEPAD=headless`, where the caller does the work in the frame — a
    /// scripted run wants its pictures in the frame that drew them, which is
    /// the same bargain makepad's own decode strikes under that cfg.
    fn reader(&mut self) -> Option<mpsc::Sender<PicJob>> {
        // `cfg!` rather than `#[cfg]` so the thread and its jobs stay
        // compiled under headless: the branch folds away either way, and
        // code the linter can still see is code that cannot rot.
        if cfg!(headless) {
            return None;
        }
        if self.reader.is_none() {
            self.reader = Some(spawn_picture_reader());
        }
        self.reader.clone()
    }

    /// Files what the reader thread (or, with no thread, the frame) found.
    fn take(&mut self, ready: &PicturesReady) {
        for (key, bytes) in &ready.items {
            self.bytes.insert(key.clone(), bytes.clone());
        }
        for key in &ready.failed {
            self.failed.insert(key.clone());
        }
        for key in &ready.retry {
            self.asked.remove(key);
        }
    }
}

/// The reader thread: one mail's raw taken apart, one `data:` payload
/// un-base64'd, and back to the UI thread as a [`PicturesReady`] action.
/// Both jobs used to run inside the frame that first drew the picture — the
/// read is SQLite I/O over a whole RFC822 message, the MIME walk decodes
/// every part of it, and a letter with three screenshots in it made the
/// frame that opened it visibly late.
///
/// # Panics
///
/// If the thread cannot be spawned.
fn spawn_picture_reader() -> mpsc::Sender<PicJob> {
    let (tx, rx) = mpsc::channel::<PicJob>();
    std::thread::Builder::new()
        .name("pictures".into())
        .spawn(move || {
            // A reader over whichever *one* writer the job names (CR-005
            // phase 0), kept for as long as the jobs keep naming it — one
            // process can have several worlds open at once (the panels
            // library), and in every other run this opens exactly once.
            let mut held: Option<(Arc<crate::store::Db>, crate::store::Store)> = None;
            while let Ok(job) = rx.recv() {
                let ready = match job {
                    PicJob::Cid { db, mid } => {
                        if !held.as_ref().is_some_and(|(h, _)| Arc::ptr_eq(h, &db)) {
                            held = crate::store::Store::with_db(db.clone())
                                .ok()
                                .map(|s| (db, s));
                        }
                        cid_parts(held.as_ref().map(|(_, s)| s), mid)
                    }
                    PicJob::Data { key, src } => data_bytes(key, &src),
                };
                Cx::post_action(ready);
            }
        })
        .expect("spawn the picture reader");
    tx
}

/// One letter's own pictures: the `cid:` parts of its raw, under the names
/// the narrowing wrote (see [`crate::html::scope_cids`]). Pure, so the
/// reader thread and a frame with no thread behind it can both run it.
fn cid_parts(store: Option<&Store>, mid: i64) -> PicturesReady {
    let items: Vec<_> = store
        .and_then(|s| mail::raw(s, mid))
        .map(|raw| {
            crate::sync::inline_images(&raw)
                .into_iter()
                .map(|(cid, bytes)| (format!("cid:m{mid}/{cid}"), Arc::from(bytes)))
                .collect()
        })
        .unwrap_or_default();
    // A letter with no raw stored for it yet has nothing to take apart —
    // and may well have it by the next time it opens, so the ask is not
    // held against it (see `PicturesReady::retry`).
    let retry = if items.is_empty() {
        vec![format!("m{mid}")]
    } else {
        Vec::new()
    };
    PicturesReady {
        items,
        failed: Vec::new(),
        retry,
    }
}

/// The bytes a `data:` source carries, un-base64'd. Pure, as [`cid_parts`].
fn data_bytes(key: String, src: &str) -> PicturesReady {
    match src
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .and_then(|(_, b)| crate::html::base64_decode(b))
    {
        Some(bytes) => PicturesReady {
            items: vec![(key, Arc::from(bytes))],
            failed: Vec::new(),
            retry: Vec::new(),
        },
        None => PicturesReady {
            items: Vec::new(),
            failed: vec![key],
            retry: Vec::new(),
        },
    }
}

/// Asks for one letter's own pictures, once. The read and the MIME walk go
/// to the reader thread; the parts land in [`pictures_landed`].
pub fn want_cid_parts(cx: &mut Cx, store: &Store, mid: i64) {
    let p = cx.global::<Pictures>();
    if !p.asked.insert(format!("m{mid}")) {
        return;
    }
    if let Some(tx) = p.reader() {
        let _ = tx.send(PicJob::Cid { db: store.db(), mid });
        return;
    }
    let ready = cid_parts(Some(store), mid);
    cx.global::<Pictures>().take(&ready);
}

/// Asks for the bytes a `data:` source carries, once.
fn want_data_bytes(cx: &mut Cx, key: &str, src: &str) {
    let p = cx.global::<Pictures>();
    if !p.asked.insert(key.to_string()) {
        return;
    }
    if let Some(tx) = p.reader() {
        let _ = tx.send(PicJob::Data {
            key: key.to_string(),
            src: src.to_string(),
        });
        return;
    }
    let ready = data_bytes(key.to_string(), src);
    cx.global::<Pictures>().take(&ready);
}

/// Files what finished off the frame: bytes the reader thread found, and
/// textures makepad's decode pool finished. True when anything landed, so
/// the shell redraws — a picture that arrives has to be placed, and the
/// item that wants it may be anywhere in the tree.
pub fn pictures_landed(cx: &mut Cx, actions: &Actions) -> bool {
    let mut any = false;
    for a in actions {
        if let Some(ready) = a.downcast_ref::<PicturesReady>() {
            cx.global::<Pictures>().take(ready);
            any = true;
        }
        let Some(AsyncImageLoad { image_path, result }) = a.downcast_ref::<AsyncImageLoad>() else {
            continue;
        };
        // Taken here rather than left for the item that asked: the item may
        // be scrolled out of its list by now, and a decode nobody commits
        // leaves the cache entry pending for good. Committing it early costs
        // the item nothing — its own handler reads the texture back out of
        // the cache either way.
        let Some(result) = result.borrow_mut().take() else { continue };
        // A picture that would not decode is given up on, so the item that
        // asked stops holding a blank box open for it and shows its alt text.
        if result.is_err() {
            let key = image_path.to_string_lossy().to_string();
            cx.global::<Pictures>().failed.insert(key);
        }
        process_async_image_load(cx, image_path, result);
        any = true;
    }
    any
}

/// Asks the network for `src` unless it is here, on its way, or known not
/// to come. The reply lands in [`pictures_arrived`].
fn fetch_picture(cx: &mut Cx, src: &str) {
    let id = LiveId::from_str(src);
    {
        let p = cx.global::<Pictures>();
        if p.bytes.contains_key(src) || p.failed.contains(src) || p.inflight.contains_key(&id) {
            return;
        }
        p.inflight.insert(id, src.to_string());
    }
    cx.http_request(id, HttpRequest::new(src.to_string(), HttpMethod::GET));
}

/// Files the replies to [`fetch_picture`]; true when any image landed or
/// failed, so the shell redraws.
pub fn pictures_arrived(cx: &mut Cx, responses: &[NetworkResponse]) -> bool {
    let p = cx.global::<Pictures>();
    let mut any = false;
    for r in responses {
        match r {
            NetworkResponse::HttpResponse {
                request_id,
                response,
            } => {
                let Some(src) = p.inflight.remove(request_id) else { continue };
                match response.get_body() {
                    Some(body) if (200..300).contains(&response.status_code) && !body.is_empty() => {
                        p.bytes.insert(src, body.clone().into());
                    }
                    _ => {
                        p.failed.insert(src);
                    }
                }
                any = true;
            }
            NetworkResponse::HttpError { request_id, .. } => {
                if let Some(src) = p.inflight.remove(request_id) {
                    p.failed.insert(src);
                    any = true;
                }
            }
            _ => {}
        }
    }
    any
}

/// The largest SVG worth parsing in the frame that draws it. An SVG has no
/// texture and no cache: it becomes geometry on the widget's own script VM,
/// so it cannot leave the UI thread the way a raster decode can. makepad's
/// own ceiling is sixteen megabytes, which is a stalled frame; a picture in
/// a letter is a logo or a diagram, and this is generous for both.
const MAX_INLINE_SVG: usize = 64 << 10;

/// Whether the picture's EXIF says it is stored on its side — orientations
/// 5 to 8, the quarter turns, which swap width and height once decoded.
///
/// Read here because the header dimensions makepad reports before a decode
/// are the *encoded* ones, while the buffer it hands back afterwards has the
/// turn applied; the box reserved in between has to agree with the second or
/// it snaps. JPEG only: it is where a rotation tag actually comes from (a
/// photograph off a phone), and PNG and WebP can carry one in theory and
/// essentially never do.
fn exif_turns_the_picture(bytes: &[u8]) -> bool {
    jpeg_exif(bytes)
        .and_then(tiff_orientation)
        .is_some_and(|o| (5..=8).contains(&o))
}

/// The TIFF block of a JPEG's `APP1 Exif` segment, if it has one. Walks the
/// marker chain from `SOI` and stops at the first segment that is not one:
/// EXIF is written before the scan, and reading past it means reading the
/// entropy-coded image.
fn jpeg_exif(bytes: &[u8]) -> Option<&[u8]> {
    let mut at = bytes.strip_prefix(&[0xFF, 0xD8]).map(|_| 2)?;
    loop {
        // Any number of fill bytes may pad the run-up to a marker.
        while bytes.get(at) == Some(&0xFF) && bytes.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if bytes.get(at) != Some(&0xFF) {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        // Start of scan, or anything with no length: past the metadata.
        if marker == 0xDA || marker == 0xD9 || (0xD0..=0xD8).contains(&marker) {
            return None;
        }
        let len = u16::from_be_bytes([*bytes.get(at + 2)?, *bytes.get(at + 3)?]) as usize;
        let body = bytes.get(at + 4..at + 2 + len.max(2))?;
        if marker == 0xE1 {
            if let Some(tiff) = body.strip_prefix(b"Exif\0\0") {
                return Some(tiff);
            }
        }
        at += 2 + len.max(2);
    }
}

/// The Orientation tag (`0x0112`) of a TIFF header block, in either byte
/// order. Only the first IFD is walked — orientation lives there.
fn tiff_orientation(tiff: &[u8]) -> Option<u16> {
    let big = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |i: usize| -> Option<u16> {
        let b = [*tiff.get(i)?, *tiff.get(i + 1)?];
        Some(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    let u32_at = |i: usize| -> Option<u32> {
        let b = [*tiff.get(i)?, *tiff.get(i + 1)?, *tiff.get(i + 2)?, *tiff.get(i + 3)?];
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count).find_map(|i| {
        let e = ifd + 2 + i * 12;
        // A SHORT's value sits in the first two bytes of the value field,
        // whichever end of the four the byte order puts it at.
        (u16_at(e)? == 0x0112).then(|| u16_at(e + 8))?
    })
}

/// How far along one picture is. Nothing here is done in the frame that
/// asks for it: the bytes come from [`Pictures`] and the decode from
/// makepad's pool, so an item goes `Want` → `Loading` → `Shown` across at
/// least two frames, holding its own box open in between.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Pic {
    /// No bytes yet: asked for, or waiting on a source someone else files.
    #[default]
    Want,
    /// Bytes in hand, decoding on the pool. The natural size is known (it
    /// comes off the header), so the box is already the right one.
    Loading,
    /// A texture, or a drawn SVG.
    Shown,
    /// No source to be had, or nothing that decodes: its alt text, for good.
    Failed,
}

/// An `<img>` in a letter or an article: the image item the `Html` widget
/// places in its flow for the tag, sized to its own pixels or its `width`
/// hint and never wider than the column. Its bytes come from [`Pictures`]
/// — a `cid:` part off a letter's raw, a `data:` payload, an HTTP reply,
/// all of them found off the frame — and the decode from those bytes runs
/// on makepad's pool, keyed in its texture cache so the same picture in two
/// panels is decoded once. Until the bytes come it is its alt text; once
/// they do it holds the box the picture will fill, so nothing reflows when
/// the texture lands. With an `href` — the link the picture sat in — a tap
/// on it is a link click, the same action the text links raise.
#[derive(Script, Widget)]
pub struct HtmlImage {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[live]
    image: Image,
    #[live]
    draw_text: DrawText,
    #[rust]
    src: String,
    #[rust]
    alt: String,
    #[rust]
    width: Option<f64>,
    #[rust]
    href: String,
    #[rust]
    state: Pic,
    /// The name its bytes are filed under (see [`pic_key`]), and the key its
    /// texture sits in makepad's cache under. Computed once.
    #[rust]
    key: String,
    /// Its own pixels, off the header — known while the decode is still
    /// running, which is what keeps the box from jumping when it lands.
    #[rust]
    nat: Option<(f64, f64)>,
}

impl ScriptHook for HtmlImage {
    fn on_after_new_scoped(&mut self, _vm: &mut ScriptVm, scope: &mut Scope) {
        // The tag's attributes, the way `HtmlLink` reads its href.
        let Some(doc) = scope.props.get::<makepad_html::HtmlDoc>() else { return };
        let mut walker = doc.new_walker_with_index(scope.index + 1);
        while let Some((lc, attr)) = walker.while_attr_lc() {
            match lc {
                live_id!(src) => self.src = attr.into(),
                live_id!(alt) => self.alt = attr.into(),
                live_id!(width) => self.width = attr.parse().ok(),
                live_id!(href) => self.href = attr.into(),
                _ => {}
            }
        }
    }
}

impl Widget for HtmlImage {
    /// To the list around it, a picture that is a link is a control — a
    /// press on it taps or drag-scrolls, as on a text link — and any other
    /// is part of the prose: a selection can start on it.
    fn is_interactive(&self) -> bool {
        self.is_link()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // An animated texture ticks through the image's own next-frame.
        self.image.handle_event(cx, event, scope);
        if self.href.is_empty() || self.state != Pic::Shown {
            return;
        }
        match event.hits(cx, self.image.area()) {
            Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
            Hit::FingerHoverOut(_) => cx.set_cursor(MouseCursor::Default),
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() && fe.was_tap() => {
                cx.widget_action(
                    self.widget_uid(),
                    HtmlLinkAction::Clicked {
                        url: self.href.clone(),
                        key_modifiers: fe.modifiers,
                    },
                );
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, _walk: Walk) -> DrawStep {
        // Asked again while it decodes, not once: re-asking is an early
        // return inside makepad's cache while the job is still on the pool,
        // and the one way back when a *finished* texture is evicted under
        // the cache's 512-entry cap before this item next drew (which would
        // otherwise hold the box blank for good).
        if matches!(self.state, Pic::Want | Pic::Loading) {
            self.load(cx);
        }
        match self.state {
            Pic::Shown => {
                let nat = self
                    .image
                    .size_in_pixels(cx)
                    .map(|(w, h)| (w.max(1) as f64, h.max(1) as f64))
                    .or(self.nat)
                    .unwrap_or((1.0, 1.0));
                let walk = self.box_walk(cx, nat);
                self.image.draw_walk_image(cx, walk)
            }
            // Bytes in hand, decoding: hold the box the picture will fill
            // rather than lay out the alt text and reflow a frame later.
            Pic::Loading => {
                let walk = self.box_walk(cx, self.nat.unwrap_or((1.0, 1.0)));
                cx.walk_turtle(walk);
                DrawStep::done()
            }
            Pic::Want | Pic::Failed => {
                if !self.alt.is_empty() {
                    self.draw_text
                        .draw_walk(cx, Walk::fit(), Align::default(), &self.alt);
                }
                DrawStep::done()
            }
        }
    }
}

impl HtmlImage {
    /// Shown, and a link: a tap on it goes somewhere.
    pub fn is_link(&self) -> bool {
        self.state == Pic::Shown && !self.href.is_empty()
    }

    /// The box a picture of these pixels takes: its `width` hint or its own
    /// width, never wider than the column, and its own aspect either way.
    fn box_walk(&self, cx: &Cx2d, (nw, nh): (f64, f64)) -> Walk {
        let mut w = self.width.filter(|w| *w >= 1.0).unwrap_or(nw);
        let avail = cx.turtle().inner_width();
        if avail.is_finite() && avail > 1.0 {
            w = w.min(avail);
        }
        Walk {
            width: Size::Fixed(w),
            height: Size::Fixed(w * nh / nw),
            ..Walk::default()
        }
    }

    /// Asks for the bytes and starts the decode, once they are here. A
    /// source that cannot be had or read is given up on rather than asked
    /// every frame; one whose bytes are still coming is simply asked again
    /// next frame, since asking is one lookup.
    fn load(&mut self, cx: &mut Cx2d) {
        if self.key.is_empty() {
            self.key = pic_key(&self.src);
        }
        // Where the bytes come from, if nobody has filed them yet. A `cid:`
        // part is the letter's own and its panel asks for it (see
        // [`want_cid_parts`]); the other two are this item's to ask for.
        if self.src.starts_with("data:") {
            want_data_bytes(cx, &self.key, &self.src);
        } else if self.src.starts_with("http") {
            fetch_picture(cx, &self.src);
        }
        let p = cx.global::<Pictures>();
        if p.failed.contains(&self.key) {
            self.state = Pic::Failed;
            return;
        }
        let Some(bytes) = p.bytes.get(&self.key).cloned() else { return };
        // An SVG is drawn rather than decoded: it becomes geometry on the
        // widget's own VM, which no thread can be handed, so the parse can
        // only happen here. Hence the cap — makepad would take sixteen
        // megabytes of it, and a document that size is a stalled frame by
        // any other name. A picture in a letter is a logo or a diagram; one
        // past this is alt text, said once.
        if looks_like_svg(&bytes) {
            if bytes.len() > MAX_INLINE_SVG {
                self.fail(cx);
                return;
            }
            match self.image.load_svg_from_shared_data(cx, bytes) {
                Ok(()) => self.state = Pic::Shown,
                Err(_) => self.fail(cx),
            }
            return;
        }
        let key = PathBuf::from(&self.key);
        // The decode and its mip chain go to makepad's pool, keyed in its
        // texture cache; `Loading` carries the size off the header, which is
        // what lets the box be right before the pixels are. `Loaded` is a
        // decode that already happened — another item's, or this frame's,
        // since makepad decodes inline under `MAKEPAD=headless` — and the
        // texture is on the widget by the time it says so.
        match ImageCacheImpl::load_image_from_data_async_impl(
            &mut self.image,
            cx,
            &key,
            bytes.clone(),
            0,
        ) {
            Ok(AsyncLoadResult::Loaded) => self.state = Pic::Shown,
            Ok(AsyncLoadResult::Loading(w, h)) => {
                let (w, h) = (w.max(1) as f64, h.max(1) as f64);
                // The header's width and height are the *encoded* ones; a
                // quarter-turn of EXIF orientation is applied by the decoder
                // and not by the header, so the box has to turn with it or
                // a portrait photograph reserves a landscape hole and snaps
                // when the texture lands.
                self.nat = Some(if exif_turns_the_picture(&bytes) {
                    (h, w)
                } else {
                    (w, h)
                });
                self.state = Pic::Loading;
            }
            Err(_) => self.fail(cx),
        }
    }

    /// Gives up on this source — here and for every other item that names
    /// it, so one letter's broken picture is decoded no more than once.
    fn fail(&mut self, cx: &mut Cx2d) {
        self.state = Pic::Failed;
        cx.global::<Pictures>().failed.insert(self.key.clone());
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
    /// Mails whose own images (`cid:` parts) have been filed in
    /// [`Pictures`]: the raw is read and parsed once per panel.
    #[rust]
    pictured: HashSet<i64>,
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
        // The message panel's link accelerators (CR-003): reply is cmd+r
        // and forward cmd+f, each drawn onto its link. The shell forwards
        // any cmd chord it does not own itself. Both take the newest mail
        // of the thread — the conventional reply to a conversation, and
        // the mail a forward of it passes on.
        if let Event::KeyDown(k) = event {
            if !k.modifiers.logo {
                return;
            }
            let seed: fn(crate::core::MailId) -> Seed = match k.key_code {
                KeyCode::KeyR => Seed::Reply,
                KeyCode::KeyF => Seed::Forward,
                _ => return,
            };
            let Some(p) = scope.props.get::<PanelProps>() else {
                return;
            };
            let crate::core::Kind::Message { id } = p.kind else {
                return;
            };
            let newest = mail::thread(&p.store, id)
                .last()
                .map_or(id, |t| t.mail.head.id);
            cx.action(PanelAction::FollowLink {
                pid: p.pid,
                target: crate::core::Kind::Compose { seed: seed(newest) },
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
        self.view.link(cx, ids!(forward_link)).set_accel(
            cx,
            pid,
            "forward",
            crate::core::Kind::Compose {
                seed: Seed::Forward(newest),
            },
            false,
            Some(ui::ACCEL_FORWARD),
        );
        self.view.link(cx, ids!(reply_link)).set_accel(
            cx,
            pid,
            "reply",
            crate::core::Kind::Compose {
                seed: Seed::Reply(newest),
            },
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
        // A letter's own images — the `cid:` parts of its raw — are asked
        // for as its rows open: the read and the MIME walk happen off the
        // frame (see `want_cid_parts`) and the parts are filed under the
        // names the narrowing wrote (see `scope_cids`), which the image
        // items then look themselves up by.
        for t in msgs.iter() {
            let mid = t.mail.head.id;
            if !expand.open.contains(&mid)
                || self.pictured.contains(&mid)
                || !t.mail.html.as_deref().is_some_and(|h| h.contains("src=\"cid:"))
            {
                continue;
            }
            self.pictured.insert(mid);
            want_cid_parts(cx, &p.store, mid);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// A pasted screenshot the way a letter carries one: `multipart/related`,
    /// the HTML naming the image part by its Content-ID.
    const RAW: &str = "From: Max Ivanov <max@ivanov.dev>\r\n\
Subject: the sketch\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Like so:</p><img src=\"cid:sketch.png@ivanov.dev\" alt=\"the sketch\">\r\n\
--rel\r\n\
Content-Type: image/png; name=\"sketch.png\"\r\n\
Content-ID: <sketch.png@ivanov.dev>\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAC0lEQVR42mNgQAYAAA4AATo1BFYAAAAASUVORK5CYII=\r\n\
--rel--\r\n";

    /// A store holding one letter, with `raw` when asked for.
    fn store(raw: Option<&'static str>) -> Store {
        let s = Store::open(None).expect("in-memory store");
        s.write(move |c| {
            c.execute(
                "INSERT INTO account(id, label, email) VALUES(1, 't', 't@t')",
                [],
            )?;
            c.execute(
                "INSERT INTO folder(id, account, name, role) VALUES(1, 1, 'Inbox', 'inbox')",
                [],
            )?;
            c.execute(
                "INSERT INTO message(id, account, folder, from_email, subject, date, raw)
                 VALUES(7, 1, 1, 'max@ivanov.dev', 'the sketch', 1.0, ?1)",
                rusqlite::params![raw.map(str::as_bytes)],
            )
            .map(|_| ())
        })
        .expect("the letter");
        s
    }

    /// The reader thread files a letter's own pictures under the names the
    /// narrowing writes into its HTML — `scope_cids` prefixes every `cid:`
    /// with the mail's own `m{id}`, and this is the other half of that pact.
    #[test]
    fn cid_parts_are_filed_under_the_names_the_html_refers_to() {
        let ready = cid_parts(Some(&store(Some(RAW))), 7);
        assert_eq!(ready.items.len(), 1);
        assert_eq!(ready.items[0].0, "cid:m7/sketch.png@ivanov.dev");
        assert!(ready.items[0].1.starts_with(b"\x89PNG"));
        assert!(ready.retry.is_empty(), "found: nothing to ask again");
        let html = crate::html::scope_cids(
            "<img src=\"cid:sketch.png@ivanov.dev\">",
            &format!("m{}", 7),
        );
        assert!(
            html.contains(&format!("src=\"{}\"", ready.items[0].0)),
            "the panel and the reader name the same picture: {html}"
        );
    }

    /// A letter whose raw has not been stored yet is not held against: the
    /// ask is forgotten, so the next open asks again.
    #[test]
    fn a_letter_with_no_raw_is_worth_asking_about_again() {
        let ready = cid_parts(Some(&store(None)), 7);
        assert!(ready.items.is_empty());
        assert_eq!(ready.retry, vec!["m7".to_string()]);
    }

    /// A `data:` source carries its own bytes; base64 that will not read is
    /// a picture given up on rather than one asked for every frame.
    #[test]
    fn data_sources_carry_their_own_bytes() {
        let ready = data_bytes("k".into(), "data:image/png;base64,aGVsbG8=");
        assert_eq!(ready.items.len(), 1);
        assert_eq!(&*ready.items[0].1, b"hello");
        assert!(ready.failed.is_empty());

        let bad = data_bytes("k".into(), "data:image/png;base64,not base64!");
        assert!(bad.items.is_empty());
        assert_eq!(bad.failed, vec!["k".to_string()]);
    }

    /// The reader thread does its reading over the *one* writer, on a
    /// connection of its own (CR-005 phase 0) — a second reader on a second
    /// thread has to see the letter the UI thread just wrote.
    #[test]
    fn the_reader_reads_across_the_one_writer() {
        let db = store(Some(RAW)).db();
        let found = std::thread::spawn(move || {
            let store = Store::with_db(db).expect("a reader over the one writer");
            cid_parts(Some(&store), 7).items
        })
        .join()
        .expect("the reader thread");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "cid:m7/sketch.png@ivanov.dev");
    }

    /// A JPEG carrying one EXIF Orientation tag and nothing else: `SOI`,
    /// then an `APP1 Exif` segment over a one-entry TIFF header block.
    fn jpeg_oriented(o: u16, big: bool) -> Vec<u8> {
        let u16b = |v: u16| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        let u32b = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        let mut tiff: Vec<u8> = if big { b"MM".to_vec() } else { b"II".to_vec() };
        tiff.extend(u16b(42));
        tiff.extend(u32b(8)); // IFD0 begins right after this header
        tiff.extend(u16b(1)); // one entry
        tiff.extend(u16b(0x0112)); // Orientation
        tiff.extend(u16b(3)); // SHORT
        tiff.extend(u32b(1)); // one of them
        tiff.extend(u16b(o)); // …in the first two bytes of the value field
        tiff.extend([0, 0]);
        tiff.extend(u32b(0)); // no second IFD

        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(&tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend(((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
        out
    }

    /// A quarter turn of EXIF swaps the picture's axes on decode but not in
    /// its header, and the box is reserved off the header — so the turn has
    /// to be read here or a portrait photograph reserves a landscape hole.
    #[test]
    fn a_quarter_turn_of_exif_is_read_before_the_decode() {
        for o in 5..=8 {
            assert!(exif_turns_the_picture(&jpeg_oriented(o, false)), "little-endian {o}");
            assert!(exif_turns_the_picture(&jpeg_oriented(o, true)), "big-endian {o}");
        }
        for o in [1, 2, 3, 4] {
            assert!(!exif_turns_the_picture(&jpeg_oriented(o, false)), "upright {o}");
        }
    }

    /// Nothing to read is not a turn: a JPEG with no EXIF, a PNG, a truncated
    /// header and an empty source all keep the header's own axes.
    #[test]
    fn a_picture_with_no_exif_keeps_its_axes() {
        assert!(!exif_turns_the_picture(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]));
        assert!(!exif_turns_the_picture(b"\x89PNG\r\n\x1a\n"));
        assert!(!exif_turns_the_picture(&jpeg_oriented(6, false)[..8]));
        assert!(!exif_turns_the_picture(&[]));
    }

    /// A `data:` URL *is* its payload: filing it under itself would put
    /// megabytes of base64 in a map key and in a texture-cache path. Every
    /// other source is short, and stays readable.
    #[test]
    fn a_data_source_is_filed_under_a_hash_of_itself() {
        let long = format!("data:image/png;base64,{}", "A".repeat(4096));
        let key = pic_key(&long);
        assert!(key.len() < 32, "{key}");
        assert_eq!(key, pic_key(&long), "the same picture, the same name");
        assert_ne!(key, pic_key(&format!("{long}B")));
        assert_eq!(pic_key("cid:m7/sketch.png"), "cid:m7/sketch.png");
        assert_eq!(pic_key("https://x.dev/a.png"), "https://x.dev/a.png");
    }
}
