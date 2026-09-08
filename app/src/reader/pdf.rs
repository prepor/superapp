//! PDF parsing and rasterization. Called on a viewer worker, never in a draw.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::{RenderCache, RenderSettings};

mod links;
pub use links::{Link, Target};

pub struct Document(Pdf);

pub struct Page {
    pub number: usize,
    pub count: usize,
    pub size: (u32, u32),
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub links: Vec<Link>,
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
        // Render enough detail for zooming without re-parsing or rasterizing
        // during a gesture. Retain only one bitmap, at most 64 MiB.
        let scale = (4096.0 / w.max(h)).min(4.0);
        let width = (w * scale).ceil().clamp(1.0, 4096.0) as u16;
        let height = (h * scale).ceil().clamp(1.0, 4096.0) as u16;
        let pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings {
                x_scale: scale,
                y_scale: scale,
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
            count: self.0.pages().len(),
            size: (w.ceil() as u32, h.ceil() as u32),
            width: width as usize,
            height: height as usize,
            pixels,
            links: links::of(&self.0, page),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_actual_page_contents_and_bounds_the_bitmap() {
        let doc = Document::open(crate::reader::document::test_pdf("Hello PDF")).unwrap();
        let page = doc.render(0).unwrap();
        assert_eq!((page.number, page.count, page.size), (0, 1, (300, 200)));
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
        assert_eq!((first.count, first.size), (2, (420, 595)));
        assert_eq!(
            (second.count, second.number, second.size),
            (2, 1, (595, 420))
        );
        assert_ne!(first.pixels, second.pixels);
        assert!(doc.render(2).is_err());
    }
}
