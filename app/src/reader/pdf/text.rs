//! Unicode and glyph geometry from the same interpreter that paints the page.

use std::ops::Range;
use hayro::hayro_interpret::{self as pdf, font::Glyph, hayro_cmap::BfString, TransformExt};
use hayro::hayro_syntax::page::Page;
use hayro::vello_cpu::kurbo::{Affine, BezPath, Point, Rect, Shape};

#[derive(Clone, Debug)]
pub struct TextGlyph {
    pub range: Range<usize>,
    /// Normalized corners, in reading order: top start/end, bottom end/start.
    pub quad: [[f64; 2]; 4],
}

#[derive(Clone, Debug, Default)]
pub struct TextPage {
    pub text: String,
    pub glyphs: Vec<TextGlyph>,
}

pub(super) fn of(page: &Page<'_>) -> TextPage {
    let (w, h) = page.render_dimensions();
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 { return TextPage::default(); }
    let mut device = TextDevice { page: TextPage::default(), size: (w as f64, h as f64), previous: None };
    let cache = pdf::InterpreterCache::new();
    let mut context = pdf::Context::new(page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, w as f64, h as f64), &cache, page.xref(), pdf::InterpreterSettings::default());
    pdf::interpret_page(page, &mut context, &mut device);
    device.page
}

struct TextDevice {
    page: TextPage,
    size: (f64, f64),
    previous: Option<(Point, Affine, String)>,
}

impl<'a> pdf::Device<'a> for TextDevice {
    fn draw_glyph(&mut self, glyph: &Glyph<'a>, transform: Affine, glyph_transform: Affine,
        _: &pdf::Paint<'a>, _: &pdf::GlyphDrawMode) {
        let text = match glyph.as_unicode() {
            Some(BfString::Char(c)) => c.to_string(),
            Some(BfString::String(s)) => s,
            None => return,
        };
        if text.is_empty() || text.chars().any(char::is_control) { return; }
        let affine = transform * glyph_transform;
        if !affine.as_coeffs().iter().all(|v| v.is_finite()) || affine.determinant().abs() < 1e-12 { return; }
        let (width, bottom, top) = match glyph {
            Glyph::Outline(glyph) => {
                let bounds = glyph.outline().bounding_box();
                (glyph.advance_width().map(f64::from).unwrap_or(bounds.x1).max(1.0),
                    bounds.y0.min(-200.0), bounds.y1.max(800.0))
            }
            Glyph::Type3(_) => (500.0, -200.0, 800.0),
        };
        let quad = [(0.0, top), (width, top), (width, bottom), (0.0, bottom)]
            .map(|p| { let p = affine * Point::from(p); [p.x / self.size.0, p.y / self.size.1] });
        if (0..2).any(|axis| quad.iter().all(|p| p[axis] < 0.0) || quad.iter().all(|p| p[axis] > 1.0)) { return; }
        if let Some((end, previous, value)) = &self.previous {
            // Fill-and-stroke paints the same glyph twice.
            if *previous == affine && *value == text { return; }
            let delta = affine.inverse() * *end;
            if delta.y.abs() > 650.0 || delta.x > width + 500.0 {
                if !self.page.text.ends_with('\n') { self.page.text.push('\n'); }
            } else if delta.x < -150.0 && !self.page.text.ends_with(char::is_whitespace)
                && !text.starts_with(char::is_whitespace) { self.page.text.push(' '); }
        }
        let begin = self.page.text.len();
        self.page.text.push_str(&text);
        self.page.glyphs.push(TextGlyph { range: begin..self.page.text.len(), quad });
        self.previous = Some((affine * Point::new(width, 0.0), affine, text));
    }

    fn set_soft_mask(&mut self, _: Option<pdf::SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: pdf::BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &pdf::Paint<'a>, _: &pdf::PathDrawMode) {}
    fn push_clip_path(&mut self, _: &pdf::ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<pdf::SoftMask<'a>>, _: pdf::BlendMode) {}
    fn draw_image(&mut self, _: pdf::Image<'a, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro::hayro_syntax::Pdf;

    #[test]
    fn unicode_and_rotated_glyphs_follow_the_rendered_pages() {
        let pdf = Pdf::new(kernel::caps::demo::PDF.to_vec()).unwrap();
        let first = of(&pdf.pages()[0]);
        assert_eq!(first.text, "A shared PDF viewer\nFiles, mail attachments, and Telegram.\nPage 1 - portrait\nGo to page 2\nhttps://example.com");
        assert_eq!(first.glyphs.len(), first.text.chars().filter(|c| *c != '\n').count());
        let second = of(&pdf.pages()[1]);
        assert!(second.text.contains("Back to page 1"));
        let g = &second.glyphs[0];
        assert!((g.quad[1][0] - g.quad[0][0]).abs() < 1e-6);
        assert!(g.quad[1][1] > g.quad[0][1], "rotated text reads down the rendered page");
    }

    #[test]
    fn unicode_maps_ligatures_and_fill_stroke_keep_the_crop_transform() {
        use std::fmt::Write;
        for (rotation, first_corner) in [
            (0, [30.0 / 380.0, 494.0 / 580.0]),
            (90, [86.0 / 580.0, 30.0 / 380.0]),
            (180, [350.0 / 380.0, 86.0 / 580.0]),
            (270, [494.0 / 580.0, 350.0 / 380.0]),
        ] {
            let content = "BT /F1 20 Tf 2 Tr 50 100 Td (AB) Tj ET";
            let cmap = "begincmap 1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <41> <03A9> <42> <00660069> endbfchar endcmap";
            let objects = [
                "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
                format!("<< /Type /Page /Parent 2 0 R /MediaBox [10 20 410 620] /CropBox [20 30 400 610] /Rotate {rotation} /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>".to_string(),
                format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
                format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
            ];
            let mut bytes = "%PDF-1.4\n".to_string();
            let mut offsets = Vec::new();
            for (i, object) in objects.iter().enumerate() {
                offsets.push(bytes.len());
                writeln!(bytes, "{} 0 obj\n{object}\nendobj", i + 1).unwrap();
            }
            let xref = bytes.len();
            writeln!(bytes, "xref\n0 7\n0000000000 65535 f ").unwrap();
            for offset in offsets { writeln!(bytes, "{offset:010} 00000 n ").unwrap(); }
            write!(bytes, "trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
            let pdf = Pdf::new(bytes.into_bytes()).unwrap();
            let page = of(&pdf.pages()[0]);
            assert_eq!(page.text, "Ωfi", "fill and stroke must not duplicate the Unicode or ligature");
            assert_eq!(page.glyphs[0].range, 0..2);
            assert_eq!(page.glyphs[1].range, 2..4);
            for (actual, expected) in page.glyphs[0].quad[0].iter().zip(first_corner) {
                assert!((actual - expected).abs() < 1e-6, "rotation {rotation}: {actual} != {expected}");
            }
        }
    }
}
