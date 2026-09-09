//! Shared HTML reading for articles and mail.

use makepad_widgets::*;

pub mod html;
pub mod document;
pub mod pdf;
pub mod pictures;
pub mod ui;

/// Apply the reader's heading scale to the parsed display document. Makepad
/// fixes heading sizes by tag: h3 is 1.17× body text and h4 is body-sized.
/// Use those for titles (h1–h2) and subheadings (h3–h6), respectively, so
/// headings stay bold without either towering over prose or becoming tiny.
/// The stored HTML and the widget's source keep their original structure.
pub fn set_html(cx: &mut Cx, view: HtmlRef, text: &str) {
    use makepad_html::HtmlNode;

    let Some(mut view) = view.borrow_mut() else {
        return;
    };
    // Older stored readings still need the entity repair at the point of use.
    let text = html::guard(text);
    if view.body.as_ref() == text.as_ref() {
        return;
    }
    view.set_text(cx, &text);
    // Only a fresh parse is remapped: revisiting unchanged content must not
    // demote the headings again or reset selection and expanded details.
    for node in &mut view.doc.nodes {
        let (HtmlNode::OpenTag { lc, nc } | HtmlNode::CloseTag { lc, nc }) = node else {
            continue;
        };
        let heading = match *lc {
            live_id!(h1) | live_id!(h2) => live_id!(h3),
            live_id!(h3) | live_id!(h4) | live_id!(h5) | live_id!(h6) => live_id!(h4),
            _ => continue,
        };
        *lc = heading;
        *nc = heading;
    }
}

/// Where one reading's own controls landed: every run the `Html` widget
/// tracked as it drew, and the pictures that stood in a link, kept inside
/// the reading `area`.
///
/// The widget tracks a rect so something *in* it can answer a press — the
/// runs of a link, the summary line of a fold — so that list is exactly what
/// wants a hand. A picture stays off it: a summary takes its own click
/// target from the runs tracked while it is open, and a picture tracked
/// there would hand it the tap the picture's link should have. It leaves its
/// rectangle with [`pictures`] instead, and `pics` is what that came to.
pub fn link_runs(
    cx: &Cx2d,
    row: &WidgetRef,
    path: &[LiveId],
    area: Rect,
    pics: &[Rect],
) -> Vec<Rect> {
    let html = row.widget(cx, path).as_html();
    let Some(html) = html.borrow() else {
        return Vec::new();
    };
    let bounds = (area.pos, area.pos + area.size);
    html.text_flow
        .areas_tracker
        .areas
        .iter()
        .filter(|a| a.is_valid(cx))
        .map(|a| a.rect(cx))
        .chain(pics.iter().copied())
        .map(|r| r.clip(bounds))
        .filter(|r| r.size.x > 0.0 && r.size.y > 0.0)
        .collect()
}
