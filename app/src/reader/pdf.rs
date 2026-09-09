//! PDF parsing and rasterization. Called on a viewer worker, never in a draw.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::{RenderCache, RenderSettings};

mod links;
mod text;
pub use links::{Link, Target};
pub use text::{TextGlyph, TextPage, TEXT_PAGE_BYTES};

pub struct Document(Pdf);

pub struct Page {
    pub number: usize,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub links: Vec<Link>,
}

/// A page is at most 32 MiB, independent of a document's physical size.
pub(crate) fn bitmap_size(w: f64, h: f64) -> (f64, u16, u16) {
    let scale = (4096.0 / w.max(h)).min(4.0).min((8_000_000.0 / (w * h)).sqrt());
    (scale, (w * scale).ceil().clamp(1.0, 4096.0) as u16,
        (h * scale).ceil().clamp(1.0, 4096.0) as u16)
}

impl Document {
    pub fn open(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() > kernel::caps::PDF_PREVIEW_MAX {
            return Err("PDF is too large to preview (64 MB maximum)".into());
        }
        let pdf = Pdf::new(bytes).map_err(|error| match error {
            LoadPdfError::Decryption(_) => {
                "This PDF needs a password; open it with the system viewer"
            }
            LoadPdfError::Invalid => "Could not read this PDF; the file may be damaged",
        })?;
        if pdf.pages().is_empty() {
            return Err("This PDF has no pages".into());
        }
        Ok(Self(pdf))
    }

    /// Geometry arrives before any rasterization, so layout never waits for
    /// a complex first page and every page has a place in the document.
    pub fn sizes(&self) -> Vec<(u32, u32)> {
        self.0.pages().iter().map(|page| {
            let (w, h) = page.render_dimensions();
            if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
                (w.ceil() as u32, h.ceil() as u32)
            } else { (612, 792) }
        }).collect()
    }

    pub fn render(&self, number: usize) -> Result<Page, String> {
        let page = self
            .0
            .pages()
            .get(number)
            .ok_or("This page is outside the PDF")?;
        let (w, h) = page.render_dimensions();
        if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
            return Err("This PDF page has invalid dimensions".into());
        }
        let (scale, width, height) = bitmap_size(w as f64, h as f64);
        let pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings {
                x_scale: scale as f32,
                y_scale: scale as f32,
                width: Some(width),
                height: Some(height),
                bg_color: hayro::vello_cpu::color::palette::css::WHITE,
            },
        );
        let pixels = pixmap
            .data()
            .iter()
            .map(|p| u32::from_be_bytes([p.a, p.r, p.g, p.b]))
            .collect();
        Ok(Page {
            number,
            width: width as usize,
            height: height as usize,
            pixels,
            links: links::of(&self.0, page),
        })
    }

    pub fn text(&self, number: usize) -> TextPage { text::of(&self.0.pages()[number]) }
}

/// Dense, multi-page text used by extraction, cache, worker and widget regressions.
#[cfg(test)]
pub(crate) fn dense_fixture(pages: usize, lines: usize, columns: usize) -> Vec<u8> {
    use std::fmt::Write;
    let kids = (0..pages).map(|p| format!("{} 0 R", 4 + p * 2)).collect::<Vec<_>>().join(" ");
    let mut objects = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()];
    for page in 0..pages {
        let step = 720.0 / lines as f64;
        let mut content = format!("BT /F1 {} Tf {step} TL 40 760 Td\n", (step * 0.65).min(10.0));
        for line in 0..lines {
            let text = format!("p{page:03} line{line:03} ");
            writeln!(content, "({text}{}) Tj T*", "a".repeat(columns.saturating_sub(text.len()))).unwrap();
        }
        content.push_str("ET");
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + page * 2));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
    }
    let mut bytes = "%PDF-1.4\n".to_string();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        writeln!(bytes, "{} 0 obj\n{object}\nendobj", i + 1).unwrap();
    }
    let xref = bytes.len();
    let size = objects.len() + 1;
    writeln!(bytes, "xref\n0 {size}\n0000000000 65535 f ").unwrap();
    for offset in offsets { writeln!(bytes, "{offset:010} 00000 n ").unwrap(); }
    write!(bytes, "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
    bytes.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_actual_page_contents_and_bounds_the_bitmap() {
        let doc = Document::open(crate::reader::document::test_pdf("Hello PDF")).unwrap();
        let page = doc.render(0).unwrap();
        assert_eq!(page.number, 0);
        assert_eq!(doc.sizes(), vec![(300, 200)]);
        assert_eq!(page.pixels.len(), page.width * page.height);
        assert!(page.pixels.contains(&0xffffffff), "white paper");
        assert!(
            page.pixels.iter().any(|p| p & 0xffffff < 0x888888),
            "visible ink"
        );
        assert!(doc.render(1).is_err());
        assert!(Document::open(b"%PDF-broken".to_vec()).is_err());
    }

    #[test]
    fn pages_have_independent_dimensions_including_rotation() {
        let doc = Document::open(kernel::caps::demo::PDF.to_vec()).unwrap();
        let first = doc.render(0).unwrap();
        let second = doc.render(1).unwrap();
        assert_eq!(doc.sizes(), vec![(420, 595), (595, 420)]);
        assert_eq!(second.number, 1);
        assert_ne!(first.pixels, second.pixels);
        assert!(doc.render(2).is_err());
    }
}
