//! The file card: one file, as a panel shows it.
//!
//! The card does no reading. What a file *is* — its size, its date, whether
//! it has a preview worth attempting and what those bytes are — is the
//! panel instance's to work out, through the [`Disk`](kernel::caps::Disk)
//! capability or out of a letter; the card is handed the answer. That is
//! what lets one card draw a path and an attachment without knowing there
//! is a difference.

// The files app is the card's first user, and it is being written beside
// this. Nothing in the shell shows a file, so the component is complete and
// unreferenced until then.
#![allow(dead_code)]

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
    /// A picture, as bytes. A seam: the prototype draws the card without
    /// it and says so, and the port decodes it here.
    Image(Vec<u8>),
    /// Nothing worth attempting — the card alone.
    #[default]
    None,
}

/// The three lines under the name, joined the way the card reads them:
/// *text · 4 KB · 30.08.2026*, with whichever parts there are.
#[must_use]
pub fn kind_line(d: &CardData) -> String {
    [d.kind_word.as_str(), d.size.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Writes one card into a `CardFile` view: the lines, then whichever
/// preview there is.
pub fn fill(cx: &mut Cx, card: &View, d: &CardData) {
    card.label(cx, ids!(name_lbl)).set_text(cx, &d.name);
    card.label(cx, ids!(kind_lbl)).set_text(cx, &kind_line(d));
    card.label(cx, ids!(when_lbl)).set_text(cx, &d.modified);
    card.text_input(cx, ids!(detail_txt))
        .set_text(cx, &d.detail);

    let text = match &d.preview {
        Preview::Text(t) => Some(t.as_str()),
        // The picture is a seam: the interfaces carry the bytes, and the
        // prototype draws the card without them.
        Preview::Image(_) | Preview::None => None,
    };
    card.text_input(cx, ids!(text_box.text_prev))
        .set_text(cx, text.unwrap_or(""));
    card.view(cx, ids!(text_box)).set_visible(cx, text.is_some());
    card.label(cx, ids!(none_lbl)).set_visible(cx, text.is_none());
}
