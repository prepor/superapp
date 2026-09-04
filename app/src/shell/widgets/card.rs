//! The file card: one file, as a panel shows it.
//!
//! The card does no reading. What a file *is* — its size, its date, whether
//! it has a preview worth attempting and what those bytes are — is the
//! panel instance's to work out, through the [`Disk`](kernel::caps::Disk)
//! capability or out of a letter; the card is handed the answer. That is
//! what lets one card draw a path and an attachment without knowing there
//! is a difference.
//!
//! Decoding a picture is the card's own, because it is the drawing: the
//! bytes are decoded by what they say they are, never by the name a panel
//! gave them.

use makepad_widgets::*;

/// What one card shows, as the panel filling it worked it out.
#[derive(Debug, Clone, Default)]
pub struct CardData {
    /// The file's own name, large and bold.
    pub name: String,
    /// What it is, in a word or two: *folder*, *text · 4 KB*.
    pub kind_word: String,
    /// Its size, already spelled for a human; empty for a directory.
    pub size: String,
    /// When it last changed.
    pub modified: String,
    /// The line under the three: a path, or a media type. Selectable, so it
    /// can be copied into a report.
    pub detail: String,
    pub preview: Preview,
}

/// What there is to show of a file's contents.
#[derive(Debug, Clone, Default)]
pub enum Preview {
    /// The first however-many bytes of a text file, in the app's one face.
    Text(String),
    /// A picture, as bytes: decoded here, by the two magic numbers below.
    Image(Vec<u8>),
    /// Nothing worth attempting — the card alone.
    #[default]
    None,
}

/// The two picture formats a card decodes. The card's own, like the
/// decoding: nothing outside asks what a file is by its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFormat {
    Png,
    Jpeg,
}

/// What these bytes actually are, by their magic: PNG's signature, a JPEG's
/// `FF D8 FF`. The card decodes by this and never by the name, because a
/// name lies often enough — a PNG saved as `.jpg` — that trusting it would
/// leave a picture unshown.
#[must_use]
fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else {
        None
    }
}

/// The three lines under the name, joined the way the card reads them:
/// *text · 4 KB · 30.08.2026*, with whichever parts there are.
#[must_use]
fn kind_line(d: &CardData) -> String {
    [d.kind_word.as_str(), d.size.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Writes one card into a `CardFile` view: the lines, then whichever
/// preview there is.
///
/// A picture is decoded here, into a texture of its own — so this is called
/// when what the card shows has changed and not once a frame.
pub fn fill(cx: &mut Cx, card: &View, d: &CardData) {
    card.label(cx, ids!(name_lbl)).set_text(cx, &d.name);
    card.label(cx, ids!(kind_lbl)).set_text(cx, &kind_line(d));
    card.label(cx, ids!(when_lbl)).set_text(cx, &d.modified);
    card.text_input(cx, ids!(detail_txt))
        .set_text(cx, &d.detail);

    let text = match &d.preview {
        Preview::Text(t) => Some(t.as_str()),
        Preview::Image(_) | Preview::None => None,
    };
    // A picture the decoder refuses is a card with no preview, not a card
    // with an empty box: the bytes were read, and they were not one.
    let image = match &d.preview {
        Preview::Image(bytes) => draw_image(cx, card, bytes),
        _ => false,
    };
    card.text_input(cx, ids!(text_box.text_prev))
        .set_text(cx, text.unwrap_or(""));
    card.view(cx, ids!(text_box)).set_visible(cx, text.is_some());
    card.view(cx, ids!(img_box)).set_visible(cx, image);
    card.label(cx, ids!(none_lbl))
        .set_visible(cx, text.is_none() && !image);
}

/// The picture into the card's own image box, through makepad's image
/// cache. Answers whether anything is there to show.
///
/// A template with no picture box in it answers no: makepad's loaders take a
/// missing widget silently, so the box is looked for rather than written to
/// and the card falls back to saying there is no preview. Every template in
/// this build hangs off `CardFile`, which has one.
fn draw_image(cx: &mut Cx, card: &View, bytes: &[u8]) -> bool {
    let img = card.widget(cx, ids!(img_box.img_prev)).as_image();
    if img.is_empty() {
        return false;
    }
    match sniff(bytes) {
        Some(ImageFormat::Png) => img.load_png_from_data(cx, bytes).is_ok(),
        Some(ImageFormat::Jpeg) => img.load_jpg_from_data(cx, bytes).is_ok(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{sniff, ImageFormat};

    /// What decodes a picture is the bytes' own account of themselves. The
    /// demo tree's `.jpg` is a PNG, and it draws.
    #[test]
    fn a_picture_is_decoded_by_its_bytes_and_never_by_its_name() {
        let png = kernel::caps::demo::bytes_of("~/Pictures/fold-cover.png");
        assert_eq!(sniff(&png.expect("the fixture has it")), Some(ImageFormat::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageFormat::Jpeg));
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b""), None);
        let jpg = kernel::caps::demo::bytes_of("~/Downloads/2026/photo-lisbon.jpg");
        assert_eq!(
            sniff(&jpg.expect("the fixture has it")),
            Some(ImageFormat::Png)
        );
    }
}
