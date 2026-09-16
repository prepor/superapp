//! The widgets that draw the KB's panels. Each borrows its instance from
//! the scope, reads what it shows off the store through the instance's own
//! queries, and hands presses and keys back to it.

use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

mod catalogue;
mod edit;
mod file;
mod history;
mod import;
mod page;

pub use catalogue::CataloguePanel;
pub use edit::EditPanel;
pub use file::FilePanel;
pub use history::{HistoryPanel, RevisionPanel};
pub use import::ImportPanel;
pub use page::PagePanel;

/// Runs `f` on the instance. The borrow lasts exactly as long as the call:
/// a navigation taken while it stood would find the session walking the
/// same instance.
pub(super) fn with<P: 'static, R>(props: &PanelProps, f: impl FnOnce(&mut P) -> R) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    let p = borrow.as_any().downcast_mut::<P>()?;
    Some(f(p))
}

/// Registers a drawn label's text as a hit, so a script can assert on it
/// and a pointer gets the text cursor over it.
pub(super) fn text_hit(cx: &mut Cx, props: &PanelProps, label: &LabelRef, clip: Option<Rect>) {
    let text = label.text();
    if text.trim().is_empty() {
        return;
    }
    let r = label.area().rect(cx);
    if r.size.x <= 0.0 {
        return;
    }
    props.hits.add_clipped(text, r, clip.unwrap_or(r), MouseCursor::Default, props.slot);
}

/// Files a reading's pictures with the reader, under the keys its markup
/// names, unless they are there already. The bytes are what the cache
/// would hand over; the reader decodes them once per key.
pub(super) fn file_pictures(cx: &mut Cx, pictures: Vec<(String, Vec<u8>)>) {
    use crate::reader::pictures::{Pictures, Ready};
    let p = cx.global::<Pictures>();
    let items: Vec<(String, std::sync::Arc<[u8]>)> = pictures
        .into_iter()
        .filter(|(k, _)| !p.bytes.contains_key(k))
        .map(|(k, b)| (k, std::sync::Arc::from(b)))
        .collect();
    if items.is_empty() {
        return;
    }
    p.take(&Ready { items, failed: Vec::new(), retry: Vec::new() });
}

/// A link followed in a reading: the KB's own forms open beside the panel,
/// the web opens in the browser.
pub(super) fn follow(cx: &mut Cx, scope: &mut Scope, url: &str, own: impl FnOnce(&mut kernel::session::Session, &str) -> bool) {
    if let Some(s) = scope.data.get_mut::<kernel::session::Session>() {
        if own(s, url) {
            return;
        }
    }
    if let Some(url) = crate::reader::html::link_target(url) {
        crate::platform::browser::open_or_notify(cx, &url, scope);
    }
}
