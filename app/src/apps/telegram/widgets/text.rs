//! Shared message-text rendering and link handling for transcripts and cards.

use makepad_widgets::*;
use std::collections::VecDeque;
use std::sync::Arc;

use crate::shell::hosted::PanelProps;

use super::super::text::{self, Entity};

pub(super) fn set(cx: &mut Cx, widget: &WidgetRef, body: &str, entities: Option<&[Entity]>) {
    let html = cx.global::<TextCache>().get(body, entities);
    widget.as_html().set_text(cx, &html);
}

/// Row recycling and cursor/mark twins share the same conversion. Html itself
/// skips parsing unchanged markup, but producing that markup still detects
/// links, resolves UTF-16 entities and escapes the whole body on each draw.
#[derive(Default)]
struct TextCache {
    entries: VecDeque<PreparedText>,
}

struct PreparedText {
    body: String,
    entities: Option<Vec<Entity>>,
    html: Arc<str>,
}

const TEXT_CACHE_SIZE: usize = 128;

impl TextCache {
    fn get(&mut self, body: &str, entities: Option<&[Entity]>) -> Arc<str> {
        let entry = self.entries.iter().rposition(|e| e.body == body && e.entities.as_deref() == entities)
            .and_then(|i| self.entries.remove(i))
            .unwrap_or_else(|| PreparedText {
                body: body.to_string(), entities: entities.map(<[Entity]>::to_vec),
                html: Arc::from(text::html(body, entities)),
            });
        let html = entry.html.clone();
        self.entries.push_back(entry);
        while self.entries.len() > TEXT_CACHE_SIZE { self.entries.pop_front(); }
        html
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_text_reuses_markup_but_refreshes_text_and_entities() {
        let mut cache = TextCache::default();
        let body = "https://example.org";
        let detected = cache.get(body, None);
        assert!(Arc::ptr_eq(&detected, &cache.get(body, None)));
        assert!(detected.contains("<a "));
        let plain = cache.get(body, Some(&[]));
        assert_eq!(plain.as_ref(), body, "an explicit empty entity list disables detection");
        let entity = |url: &str| Entity {
            offset: 0, length: 4,
            kind: text::EntityKind::TextUrl { url: url.into() },
        };
        assert!(cache.get("link", Some(&[entity("https://one.example")])).contains("https://one.example"));
        let changed = cache.get("link", Some(&[entity("https://two.example")]));
        assert!(changed.contains("https://two.example"));
        assert!(!changed.contains("https://one.example"));
        assert_eq!(cache.get("<&>", Some(&[])).as_ref(), "&lt;&amp;&gt;");
        for i in 0..TEXT_CACHE_SIZE * 2 { cache.get(&format!("message {i}"), None); }
        assert_eq!(cache.entries.len(), TEXT_CACHE_SIZE);
        assert_eq!(cache.get(body, None), detected, "evicted content is regenerated correctly");
    }
}
