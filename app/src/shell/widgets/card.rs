//! File metadata around the shared text, image and PDF viewer.

use makepad_widgets::*;
use super::viewer::FileViewerWidgetRefExt;
pub use super::viewer::Preview;

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

/// Fill only when the source changes, preserving selection and PDF position.
pub fn fill(cx: &mut Cx, card: &View, d: &CardData) {
    card.label(cx, ids!(name_lbl)).set_text(cx, &d.name);
    let kind = [d.kind_word.as_str(), d.size.as_str()].into_iter()
        .filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    card.label(cx, ids!(kind_lbl)).set_text(cx, &kind);
    card.label(cx, ids!(when_lbl)).set_text(cx, &d.modified);
    card.text_input(cx, ids!(detail_txt)).set_text(cx, &d.detail);
    card.widget(cx, ids!(viewer)).as_file_viewer().show(cx, d.preview.clone());
}

pub fn bind(cx: &mut Cx, card: &View, control: super::viewer::Controller) {
    card.widget(cx, ids!(viewer)).as_file_viewer().bind(control);
}
