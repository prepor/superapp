//! Shared message-text rendering and link handling for transcripts and cards.

use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::text::{self, Entity};

pub(super) fn set(cx: &mut Cx, widget: &WidgetRef, body: &str, entities: Option<&[Entity]>) {
    widget.as_html().set_text(cx, &text::html(body, entities));
}

/// Handle only actions emitted by this view. Global action broadcasts reach
/// every open panel, so acting on those would open a URL more than once.
pub(super) fn handle_event(view: &mut View, cx: &mut Cx, event: &Event, scope: &mut Scope) {
    let mut actions = cx.capture_actions(|cx| view.handle_event(cx, event, scope));
    actions.retain(|action| {
        if let Some(action) = action.as_widget_action() {
            if let HtmlLinkAction::Clicked { url, .. } = action.cast() {
                if let Some(url) = text::destination(&url) {
                    cx.open_url(&url, OpenUrlInPlace::No);
                }
                return false;
            }
        }
        true
    });
    cx.extend_actions(actions);
}

/// Link glyphs take precedence over a row's hit rectangle. HtmlLink owns the
/// actual tap/drag decision; these hits supply the hand cursor and script label.
pub(super) fn hits(cx: &Cx, widget: &WidgetRef, props: &PanelProps, clip: Rect) {
    let html = widget.as_html();
    let Some(html) = html.borrow() else { return };
    let rect = widget
        .area()
        .rect(cx)
        .clip((clip.pos, clip.pos + clip.size));
    let bounds = (rect.pos, rect.pos + rect.size);
    for area in &html.text_flow.areas_tracker.areas {
        if !area.is_valid(cx) {
            continue;
        }
        let rect = area.rect(cx).clip(bounds);
        if rect.size.x > 0.0 && rect.size.y > 0.0 {
            props
                .hits
                .add("message link", rect, MouseCursor::Hand, props.slot);
        }
    }
}
