//! Source text with native editing and optional, cached typographic spans.
//! Glyphs use the same positions as the unstyled input; emphasis cannot move
//! the caret or change wrapping. Nothing here interprets a markup language.

mod native;
pub use native::{SourceInput, SourceInputRef, SourceInputWidgetRefExt};

use makepad_widgets::makepad_draw::text::{color::Color, layouter::LaidoutText};
use makepad_widgets::*;
use std::ops::Range;
use std::rc::Rc;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.SourceInput = set_type_default() do #(SourceInput::register_widget(vm)) {
        ..mod.widgets.SField
        // The spread copies SField's own properties; its animator is inherited.
        animator: mod.widgets.SField.animator
        width: Fill, height: Fill
        padding: 12, margin: 0
        flow: Right {wrap: true}
        is_multiline: true
        empty_text: "Start writing…"
        autocapitalize: AutoCapitalize.None
        autocorrect: AutoCorrect.Disabled
        draw_bg +: { border_size: 0.0 }
        draw_text +: { get_color: fn() { return self.color }; text_style: mod.widgets.SMonoStyle{font_size: 11.25, line_spacing: 1.35} }
        draw_bold: mod.draw.DrawText { text_style: mod.widgets.SMonoBoldStyle{font_size: 11.25, line_spacing: 1.35} }
        draw_italic: mod.draw.DrawText { text_style: mod.widgets.SMonoItalicStyle{font_size: 11.25, line_spacing: 1.35} }
        draw_bold_italic: mod.draw.DrawText {
            text_style: mod.widgets.SMonoItalicStyle{
                font_size: 11.25, line_spacing: 1.35
                font_family +: { latin +: { weight: 700.0 } }
            }
        }
    }
}

fn styled_layout(
    cx: &mut Cx,
    plain: Rc<LaidoutText>,
    spans: &[Span],
    bold: &DrawText,
    italic: &DrawText,
    bold_italic: &DrawText,
) -> Rc<LaidoutText> {
    if spans.is_empty() {
        return plain;
    }
    let mut styled = (*plain).clone();
    let mut offset = 0;
    for row in &mut styled.rows {
        let end = offset + row.text.len();
        for glyph in &mut row.glyphs {
            glyph.color = Some(Color::new(20, 20, 20, 255));
        }
        // Spans are sorted, disjoint source byte ranges. Shape only emphasis
        // runs; ordinary text and code reuse the native layout verbatim.
        let start = spans.partition_point(|span| span.range.end <= offset);
        for span in spans[start..]
            .iter()
            .take_while(|span| span.range.start < end)
        {
            let from = span.range.start.max(offset) - offset;
            let to = span.range.end.min(end) - offset;
            let draw = match (span.style.bold, span.style.italic) {
                (true, true) => Some(bold_italic),
                (true, false) => Some(bold),
                (false, true) => Some(italic),
                _ => None,
            };
            let emphasis = draw.map(|draw| {
                source_layout(cx, draw, None, false, Align::default(), &row.text[from..to])
            });
            for glyph in row
                .glyphs
                .iter_mut()
                .filter(|g| (from..to).contains(&g.cluster))
            {
                if let Some(run) = &emphasis {
                    if let Some(replacement) = run.rows[0]
                        .glyphs
                        .binary_search_by_key(&(glyph.cluster - from), |g| g.cluster)
                        .ok()
                        .map(|i| &run.rows[0].glyphs[i])
                    {
                        glyph.font = replacement.font.clone();
                        glyph.id = replacement.id;
                        glyph.offset_in_ems = replacement.offset_in_ems;
                    }
                }
                glyph.color = Some(if span.style.dim {
                    Color {
                        r: 90,
                        g: 90,
                        b: 90,
                        a: 255,
                    }
                } else {
                    Color {
                        r: 20,
                        g: 20,
                        b: 20,
                        a: 255,
                    }
                });
            }
        }
        offset = end + usize::from(row.newline);
    }
    Rc::new(styled)
}

/// Expand tab stops for layout only, then map every row and glyph back to
/// source bytes. Selection and clipboard still see one literal tab.
fn source_layout(
    cx: &mut Cx,
    draw: &DrawText,
    width: Option<f32>,
    wrap: bool,
    align: Align,
    text: &str,
) -> Rc<LaidoutText> {
    if !text.contains('\t') {
        return draw.layout(cx, 0.0, 0.0, width, wrap, align, text);
    }
    let mut expanded = String::with_capacity(text.len());
    let mut source_bytes = Vec::with_capacity(text.len() + 1);
    let mut column = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        if grapheme == "\t" {
            let spaces = 4 - column % 4;
            for _ in 0..spaces {
                expanded.push(' ');
                source_bytes.push(index);
            }
            column += spaces;
        } else {
            expanded.push_str(grapheme);
            source_bytes.extend(index..index + grapheme.len());
            column = if grapheme.contains('\n') {
                0
            } else {
                column + 1
            };
        }
    }
    source_bytes.push(text.len());
    let mut laidout = (*draw.layout(cx, 0.0, 0.0, width, wrap, align, &expanded)).clone();
    let source: makepad_widgets::makepad_draw::text::substr::Substr = text.into();
    for row in &mut laidout.rows {
        let start = row.text.start_in_parent();
        let from = source_bytes[start];
        let to = source_bytes[row.text.end_in_parent()];
        row.text = source.substr(from..to);
        for glyph in &mut row.glyphs {
            glyph.cluster = source_bytes[start + glyph.cluster] - from;
        }
        // A soft wrap can split tab whitespace. Only the row containing
        // that source byte owns the tab; leading/trailing padding is blank.
        if let Some(glyph) = row.glyphs.iter().find(|g| g.cluster >= row.text.len()) {
            row.width_in_lpxs = glyph.origin_in_lpxs.x;
        }
        row.glyphs.retain(|g| g.cluster < row.text.len());
    }
    laidout.text = source;
    Rc::new(laidout)
}
