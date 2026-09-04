//! The shared components' templates.
//!
//! The table's chassis — the filter and its error line, the four row twins
//! that carry the cursor wash and the mark bar, the hidden-marks band, the
//! completion box — and the file card. An app composes them: it declares
//! its own row *body* once and hangs it in each twin, so the look of a
//! marked or selected row is settled here and never in an app.
//!
//! Colours follow [`shell::dsl`](super::super::dsl): INK #141414 ·
//! SEL #e7e7e7 · RULE #dcdcdc · MUTED #909090 · TEXT2 #5a5a5a · ERR #a01500.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // ---- the filter --------------------------------------------------------

    /** The field over a table's rows. `/` puts the caret in it, `tab`
        returns to it, and `↓` hands the keyboard back to the first row. */
    mod.widgets.TblFilter = mod.widgets.SField {
        width: Fill
        empty_text: "filter…  ( / )   @ for tags"
        return_key_type: ReturnKeyType.Search
        autocapitalize: AutoCapitalize.None
        autocorrect: AutoCorrect.Disabled
    }

    /** What the filter could not read, in the one colour errors get. */
    mod.widgets.TblErr = mod.widgets.SLabel {
        visible: false
        padding: 0
        margin: Inset{left: 8, top: 4}
        text: ""
        draw_text +: { color: #a01500 }
    }

    /** The strong rule under a table's column heads. */
    mod.widgets.TblHeadRule = View {
        width: Fill, height: 1
        show_bg: true
        draw_bg +: { color: #141414 }
    }

    /** An empty list says why, rather than showing nothing at all. */
    mod.widgets.TblEmpty = mod.widgets.SLabel {
        visible: false
        margin: Inset{left: 8, top: 10}
        text: ""
        draw_text +: { color: #909090 }
    }

    // ---- a row and its four twins -----------------------------------------

    /** One row of a table: the four twins, then the hairline under them. An
        app fills each twin with one instance of its own body. */
    mod.widgets.TblRow = View {
        width: Fill, height: Fit
        flow: Down
    }

    /** A row's line, plain. The row's inset is the one source of spacing:
        the body's labels shed the theme padding, so text sits 8 pt inside
        the row — the filter's own text inset — and the mark bar has three
        of those points to live in. */
    mod.widgets.TblLine = View {
        width: Fill, height: Fit
        flow: Down
        padding: Inset{left: 8, right: 8, top: 3, bottom: 3}
    }

    /** The cursor's row: the wash.

        A twin rather than one line recoloured, because a quad's colour is
        not a runtime value; and a custom pixel fn, because portal-item
        quads on the stock shader merge into a call that paints under the
        panel background and is never seen. */
    mod.widgets.TblLineSel = mod.widgets.TblLine {
        visible: false
        show_bg: true
        draw_bg +: {
            color: #e7e7e7
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    /** A marked row: a 3 pt ink bar down its left edge, inside the row's
        own inset — shader-drawn, so a mark costs no layout and the text
        stays on the header's columns. */
    mod.widgets.TblLineMark = mod.widgets.TblLine {
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

    /** Marked, and under the cursor. */
    mod.widgets.TblLineMarkSel = mod.widgets.TblLine {
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

    /** The hairline that closes a row. */
    mod.widgets.TblHairline = View {
        width: Fill, height: 1
        show_bg: true
        draw_bg +: {
            color: #dcdcdc
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    // ---- the hidden-marks band ---------------------------------------------

    /** The caption over the marks the filter hides. They ride above the
        rows *in the same list*, so the group scrolls with it and the
        arrows, which walk the table, never visit it. */
    mod.widgets.TblCaption = View {
        width: Fill, height: Fit
        padding: Inset{left: 8, right: 8, top: 6, bottom: 2}
        mod.widgets.SSection { text: "MARKED · HIDDEN BY THE FILTER" }
    }

    /** The strong rule that closes the band. */
    mod.widgets.TblBandRule = View {
        width: Fill, height: 1
        show_bg: true
        draw_bg +: {
            color: #141414
            pixel: fn() {
                return vec4(self.color.xyz * self.color.w, self.color.w)
            }
        }
    }

    // ---- the completion box -------------------------------------------------

    mod.widgets.TblSuggestLine = View {
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

    mod.widgets.TblSuggestRow = View {
        width: Fill, height: Fit
        flow: Down
        line := mod.widgets.TblSuggestLine {}
        line_sel := mod.widgets.TblSuggestLine {
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

    /** A field's autocomplete: a bordered box hung under the field, over
        whatever follows it. Eight fixed slots, shown as needed; the offer
        is capped there.

        Drawn after everything else in the panel, at an absolute position,
        so it must land in a draw call *after* the content it covers. Its
        own pixel fn (the hairline ink border) makes it a shader no earlier
        call shares, which is what earns that ordering. */
    mod.widgets.TblSuggest = View {
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
        s0 := mod.widgets.TblSuggestRow {}
        s1 := mod.widgets.TblSuggestRow {}
        s2 := mod.widgets.TblSuggestRow {}
        s3 := mod.widgets.TblSuggestRow {}
        s4 := mod.widgets.TblSuggestRow {}
        s5 := mod.widgets.TblSuggestRow {}
        s6 := mod.widgets.TblSuggestRow {}
        s7 := mod.widgets.TblSuggestRow {}
    }

    // ---- the file card ------------------------------------------------------

    /** One file, as a panel shows it: its name, what it is, when it
        changed, the line under the three, and whatever preview there is — a
        reading, or a picture.

        A panel fills it from a `CardData` it built itself — the reads are
        the instance's, so the card draws the same whether its bytes came off
        a disk or out of a letter. Two apps hang their own template on this
        one: files adds the status line a refused verb leaves, mail the same
        line and nothing else. */
    mod.widgets.CardFile = View {
        width: Fill, height: Fill
        flow: Down
        padding: Inset{left: 12, right: 12, top: 10, bottom: 10}
        spacing: 6

        name_lbl := mod.widgets.SBoldLabel {
            width: Fill, max_lines: 1, text_overflow: TextOverflow.Ellipsis, text: ""
            draw_text +: { text_style: mod.widgets.SMonoBoldStyle{font_size: 13.0} }
        }
        /* The name, edited where it is drawn: a panel that offers to rename
           what its card shows raises this in the name's place. Hidden until
           one does, which is every card that offers no such verb. */
        rename_row := View {
            visible: false
            width: Fill, height: Fit
            flow: Right
            align: Align{y: 0.5}
            rename_input := mod.widgets.SField {
                empty_text: "name"
                return_key_type: ReturnKeyType.Done
                autocapitalize: AutoCapitalize.None
                autocorrect: AutoCorrect.Disabled
            }
        }
        kind_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #5a5a5a } }
        when_lbl := mod.widgets.SLabel { text: "", draw_text +: { color: #909090 } }
        // Selectable: a path is something one copies into a report.
        detail_txt := mod.widgets.SText { text: "" }
        mod.widgets.SRule {}
        // A text input carries no `visible` of its own; the box around it
        // is what shows and hides the preview.
        text_box := View {
            visible: false
            width: Fill, height: Fill
            text_prev := mod.widgets.SText {
                width: Fill, height: Fill
                is_multiline: true
            }
        }
        /* `Image` carries no `visible` of its own either, so the box around
           it is what shows and hides the picture. Drawn at the text's width,
           which is the width a card's wish measured its rows against. */
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
}
