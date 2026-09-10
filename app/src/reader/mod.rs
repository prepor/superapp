//! Shared HTML reading for articles and mail.

use makepad_widgets::*;

pub mod html;
pub mod document;
pub mod pdf;
pub mod pictures;
pub mod ui;
mod content;
pub use content::{html_landed, HtmlContent};

/// Consume links emitted by this reader, keeping global action broadcasts
/// from opening the same destination once for every visible panel.
pub fn handle_links(view: &mut View, cx: &mut Cx, event: &Event, scope: &mut Scope) {
    let mut actions = cx.capture_actions(|cx| view.handle_event(cx, event, scope));
    actions.retain(|action| {
        if let Some(action) = action.as_widget_action() {
            if let HtmlLinkAction::Clicked { url, .. } = action.cast() {
                if let Some(url) = html::link_target(&url) {
                    crate::platform::browser::open_or_notify(cx, &url, scope);
                }
                return false;
            }
        }
        true
    });
    cx.extend_actions(actions);
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
