//! The theme and the base widgets every panel is built from.
//!
//! The design language in one block: the mono face and its four styles, a
//! label, a section, a field, a button, a link, and the overlay rows. An
//! app's own `script_mod` builds on these, so two apps look like one
//! product without either of them saying so.
//!
//! Colours are spelled out here rather than read from [`kernel::theme`] —
//! the DSL is a script, not Rust — and the two are kept in step by hand:
//! INK #141414 · BG #ffffff · TEXT2 #5a5a5a · MUTED #909090 ·
//! RULE #dcdcdc · HOVER #efefef · SEL #e7e7e7 · ERR #a01500.

use kernel::nav::Nav;
use kernel::session::Session;
use makepad_widgets::*;

use super::draw::{DrawFlat, DrawPanel};
use super::hosted::PanelProps;
use super::stage::Stage;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the chrome's two shaders -----------------------------------------

    // A panel quad: flat fill + hard 1 pt border, sharp corners.
    set_type_default() do #(DrawPanel::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(1.0, 1.0, 1.0, 1.0)
        border_color: vec4(0.078, 0.078, 0.078, 1.0)
        border_size: 1.0
        alpha: 1.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.0 0.0 self.rect_size.x self.rect_size.y 1.0)
            sdf.fill_keep(vec4(self.color.xyz, self.color.w * self.alpha))
            if self.border_size > 0.0 {
                sdf.stroke(vec4(self.border_color.xyz, self.border_color.w * self.alpha) self.border_size)
            }
            return sdf.result
        }
    }

    // Flat quad: rules, hovers, selections, underlines, bridges.
    set_type_default() do #(DrawFlat::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(0.078, 0.078, 0.078, 1.0)
        pixel: fn() {
            return vec4(self.color.xyz * self.color.w, self.color.w)
        }
    }

    // ---- the mono face -----------------------------------------------------
    //
    // Bundled Geist Mono on every platform, so a screenshot is the same
    // picture everywhere. Keep the sizes in step with `kernel::theme`.

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

    /** Bold Geist Mono: headings and shortcut letters. */
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

    /** Geist Mono's italic face. */
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

    // ---- the base widgets --------------------------------------------------

    /** Body text with no padding. Its row controls spacing. */
    mod.widgets.SLabel = Label {
        width: Fit, height: Fit
        padding: 0
        draw_text +: {
            color: #141414
            text_style: mod.widgets.SMonoStyle{}
        }
    }

    /** Uppercase section label. */
    mod.widgets.SSection = Label {
        width: Fit, height: Fit
        padding: 0
        draw_text +: {
            color: #5a5a5a
            text_style: mod.widgets.SMonoStyle{font_size: 8.25}
        }
    }

    /** Bold body text. */
    mod.widgets.SBoldLabel = mod.widgets.SLabel {
        draw_text +: { text_style: mod.widgets.SMonoBoldStyle{} }
    }

    /** Plain text field. Its border darkens on focus. */
    mod.widgets.SField = TextInputFlat {
        width: Fill, height: Fit
        padding: Inset{left: 7, right: 7, top: 5, bottom: 5}
        margin: 0
        empty_text: " "
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
        draw_cursor +: { color: #141414 }
        draw_selection +: {
            color: #00000020
            color_hover: #00000020
            color_focus: #00000020
            color_down: #00000020
            color_empty: #00000000
        }
    }

    /** A selectable run of read-only text: a payload, a path, an error a
        person copies into a report. It draws as body text and carries no
        box, but it selects, and a drag across it is a real selection. */
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
        // Read-only text has no caret.
        draw_cursor +: { color: #00000000 }
        draw_selection +: {
            color: #00000020
            color_hover: #00000020
            color_focus: #00000020
            color_down: #00000020
            color_empty: #00000000
        }
    }

    /** Bordered action button. */
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

    /** A key name inside a thin box. Display only; it cannot take focus. */
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

    /** One line of text and inline controls with shared spacing. */
    mod.widgets.SRow = View {
        width: Fill, height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{top: 6, bottom: 6}
    }

    /** Thin rule below a section label. */
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

    /** Underlined link. Solid links open a joined panel; dotted links
        replace the panel they are in. */
    mod.widgets.SLink = set_type_default() do #(SLink::register_widget(vm)) {
        ..mod.widgets.View
        width: Fit, height: Fit
        flow: Down
        cursor: MouseCursor.Hand
        // Split so the shortcut letter can be bold.
        row := View {
            width: Fit, height: Fit
            flow: Right
            pre := mod.widgets.SLabel { text: "" }
            key := mod.widgets.SBoldLabel { text: "" }
            post := mod.widgets.SLabel { text: "" }
        }
        // A separate shader keeps the underline above the panel background.
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

    // ---- the modal overlays ------------------------------------------------

    /** Renders a subtree to a texture and applies one alpha value. Widgets
        cannot fade as a group, but an offscreen pass can. */
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

    /** One overlay row's card. The shell owns the click: a `PortalList`
        item's area goes stale the moment a mid-gesture redraw lands. */
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
        // Three cards rather than one recoloured: a DrawQuad's shader vars
        // are not struct fields, so a quad's colour cannot be set at draw
        // time. Exactly one of these draws.
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

    /** The overlay chassis: a column of rows on the shell's sheet, faded as
        one surface. Workspaces and history use it bare. */
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

    /** The launcher: one real field over the hits, so the query has a
        caret, a selection, and the platform's own input method. */
    mod.widgets.LauncherOverlay = set_type_default() do #(LauncherOverlay::register_widget(vm)) {
        ..mod.widgets.FadeView
        width: Fill, height: Fill
        flow: Down
        query_input := mod.widgets.SField {
            width: Fill
            empty_text: "search panels…"
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

    // ---- the stage ---------------------------------------------------------

    /** The workspace itself. The binary instantiates one and hangs every
        app's panel templates on it as named children. */
    mod.widgets.Stage = set_type_default() do #(Stage::register_widget(vm)) {
        width: Fill
        height: Fill
        draw_mono +: {
            text_style: mod.widgets.SMonoStyle{}
            color: #141414ff
        }
        draw_mono_bold +: {
            text_style: mod.widgets.SMonoBoldStyle{}
            color: #141414ff
        }
    }
}

// ---------------------------------------------------------------------------
// SLink
// ---------------------------------------------------------------------------

/// An underlined link: a solid one opens a joined panel, a dotted one
/// replaces the panel it is in. It carries the [`Nav`] it means, so where
/// the click goes is the kernel's to decide and not the widget's.
#[derive(Script, ScriptHook, Widget)]
pub struct SLink {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    nav: Option<Nav>,
    #[rust]
    dotted: bool,
    /// The whole text, for the hit a script addresses it by.
    #[rust]
    label: String,
}

impl Widget for SLink {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Hit::FingerUp(fe) = event.hits(cx, self.view.area()) else {
            return;
        };
        if !fe.is_over || !fe.was_tap() {
            return;
        }
        let Some(nav) = self.nav.clone() else { return };
        // cmd (alt as a quiet alias) always opens a fresh, un-joined panel.
        let nav = match nav {
            Nav::Open { from, id, .. } if fe.modifiers.logo || fe.modifiers.alt => Nav::Open {
                from,
                id,
                fresh: true,
            },
            n => n,
        };
        if let Some(session) = scope.data.get_mut::<Session>() {
            session.nav(nav);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step = self.view.draw_walk(cx, scope, walk);
        // A link registers its own text: that is all a script needs to
        // click it, and all the shell needs to put a hand cursor on it.
        if let Some(props) = scope.props.get::<PanelProps>() {
            let r = self.view.area().rect(cx);
            if r.size.x > 0.0 && !self.label.is_empty() {
                props
                    .hits
                    .add(self.label.clone(), r, MouseCursor::Hand, props.slot);
            }
        }
        step
    }
}

impl SLinkRef {
    /// What the link says and where it goes. `accel` names the key it
    /// carries; its letter is drawn bold.
    pub fn set(&self, cx: &mut Cx, text: &str, nav: Nav, dotted: bool, accel: Option<char>) {
        let Some(mut l) = self.borrow_mut() else {
            return;
        };
        l.nav = Some(nav);
        l.dotted = dotted;
        l.label = text.to_string();
        let (pre, key, post) = super::draw::split_accel(text, accel);
        // An empty label still reserves width, which would push the text
        // right of an underline spanning the whole row — so the unused
        // parts stand down entirely rather than render nothing.
        for (path, s) in [
            (ids!(row.pre), &pre),
            (ids!(row.key), &key),
            (ids!(row.post), &post),
        ] {
            let lbl = l.view.label(cx, path);
            lbl.set_text(cx, s);
            lbl.set_visible(cx, !s.is_empty());
        }
        l.view.view(cx, ids!(ul)).set_visible(cx, !dotted);
        l.view.view(cx, ids!(ul_dotted)).set_visible(cx, dotted);
    }
}

// ---------------------------------------------------------------------------
// The overlays
// ---------------------------------------------------------------------------

/// One overlay row's data, as the shell hands it over each draw.
#[derive(Clone, Debug, Default)]
pub struct OverlayRowData {
    /// The workspace number, where a row has one.
    pub num: String,
    pub main: String,
    pub detail: String,
    pub right: String,
    /// The selected row, drawn inverted.
    pub current: bool,
    pub hovered: bool,
    /// An undone branch of the history tree: legible, but quiet.
    pub muted: bool,
}

/// One overlay row's height, which is what the sheet is measured against.
pub const OVERLAY_ROW_H: f64 = 40.0;

/// What an overlay widget is handed on the scope.
#[derive(Clone, Debug, Default)]
pub struct OverlayProps {
    pub rows: Vec<OverlayRowData>,
    pub query: String,
    pub alpha: f32,
}

/// Intent from an overlay widget. Rows resolve through the shell's own hit
/// table, so only the query field speaks here.
#[derive(Debug, Clone)]
pub enum OverlayAction {
    /// The launcher's field changed — ask the question again.
    Query(String),
}

/// One overlay row. Presentation only: the shell owns the click.
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
    /// Fills the row in. One card draws; the others stand down — a quad's
    /// colour cannot be set at draw time, but a label's can.
    pub fn populate(&self, cx: &mut Cx, d: &OverlayRowData) {
        let Some(row) = self.borrow() else { return };
        let (fg, dim) = if d.current {
            (vec4(1.0, 1.0, 1.0, 1.0), vec4(0.75, 0.75, 0.75, 1.0))
        } else if d.muted {
            (vec4(0.565, 0.565, 0.565, 1.0), vec4(0.72, 0.72, 0.72, 1.0))
        } else {
            (
                vec4(0.078, 0.078, 0.078, 1.0),
                vec4(0.353, 0.353, 0.353, 1.0),
            )
        };
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
        let paint = |lbl: &LabelRef, col: Vec4f| {
            if let Some(mut l) = lbl.borrow_mut() {
                l.draw_text.color = col;
            }
        };
        let num = row.view.label(cx, &[c[0], live_id!(num_lbl)]);
        num.set_text(cx, &d.num);
        num.set_visible(cx, !d.num.is_empty());
        paint(&num, fg);
        row.view
            .view(cx, &[c[0], live_id!(num_gap)])
            .set_visible(cx, !d.num.is_empty());
        let main = row.view.label(cx, &[c[0], live_id!(main_lbl)]);
        main.set_text(cx, &d.main);
        paint(&main, fg);
        let detail = row.view.label(cx, &[c[0], live_id!(detail_lbl)]);
        detail.set_text(cx, &d.detail);
        detail.set_visible(cx, !d.detail.is_empty());
        paint(&detail, dim);
        let right = row.view.label(cx, &[c[0], live_id!(right_lbl)]);
        right.set_text(cx, &d.right);
        right.set_visible(cx, !d.right.is_empty());
        paint(&right, dim);
    }
}

/// Draws a column of rows into a `PortalList`, at the chassis' alpha.
fn draw_rows(
    view: &mut View,
    cx: &mut Cx2d,
    scope: &mut Scope,
    walk: Walk,
    rows: &[OverlayRowData],
    alpha: f32,
) -> DrawStep {
    // The subtree renders to a texture; this is the alpha it lands at.
    view.draw_bg.set_uniform(cx, live_id!(alpha), &[alpha]);
    while let Some(item) = view.draw_walk(cx, scope, walk).step() {
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

/// A column of overlay rows — the workspaces roster, the undo tree.
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
            .unwrap_or_default();
        draw_rows(&mut self.view, cx, scope, walk, &rows, alpha)
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
            .unwrap_or_default();
        // A query nothing answers says so, instead of an empty sheet.
        self.view
            .view(cx, ids!(empty_row))
            .set_visible(cx, rows.is_empty() && !query.is_empty());
        draw_rows(&mut self.view, cx, scope, walk, &rows, alpha)
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

    /// The field's rectangle, for the hit that puts a caret in it.
    #[must_use]
    pub fn query_rect(&self, cx: &mut Cx) -> Rect {
        let Some(inner) = self.borrow() else {
            return Rect::default();
        };
        inner.view.widget(cx, ids!(query_input)).area().rect(cx)
    }
}

/// `WidgetRef → SLinkRef`, mirroring the generated accessors.
pub trait LinkViewExt {
    fn link(&self, cx: &mut Cx, path: &[LiveId]) -> SLinkRef;
}

impl LinkViewExt for View {
    fn link(&self, cx: &mut Cx, path: &[LiveId]) -> SLinkRef {
        self.widget(cx, path).as_slink()
    }
}
