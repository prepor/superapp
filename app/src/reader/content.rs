//! A reader owns preparation of its current HTML. Parsing never borrows a
//! widget; the UI only installs the completed document and draws it.

use makepad_html::{parse_html, HtmlDoc, HtmlNode};
use makepad_widgets::*;
use std::sync::{Arc, OnceLock};
use tokio::sync::{oneshot, Semaphore};

struct Prepared {
    body: String,
    doc: HtmlDoc,
}

fn prepare(source: &str) -> Prepared {
    let body = super::html::guard(source).into_owned();
    let mut doc = parse_html(&body, &mut None, InternLiveId::No);
    for node in &mut doc.nodes {
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
    Prepared { body, doc }
}

fn readers() -> &'static Arc<Semaphore> {
    static READERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    READERS.get_or_init(|| Arc::new(Semaphore::new(2)))
}

fn start(source: String, notify: impl FnOnce() + Send + 'static) -> oneshot::Receiver<Prepared> {
    let (send, receive) = oneshot::channel();
    kernel::runtime::spawn(async move {
        let permit = readers()
            .clone()
            .acquire_owned()
            .await
            .expect("HTML readers remain open");
        if send.is_closed() {
            return;
        }
        if let Ok(prepared) = kernel::runtime::spawn_blocking(move || {
            let _permit = permit;
            prepare(&source)
        })
        .await
        {
            if send.send(prepared).is_ok() {
                notify();
            }
        }
    });
    receive
}

#[derive(Debug)]
struct HtmlReady;

/// A completed parse requests a draw, including when no further input arrives.
pub fn html_landed(actions: &Actions) -> bool {
    actions
        .iter()
        .any(|action| action.downcast_ref::<HtmlReady>().is_some())
}

struct Pending {
    source: String,
    receive: oneshot::Receiver<Prepared>,
}

/// Keep this alongside the panel's HTML reading. A recycled row widget is a
/// new destination even if its source text is identical. At most one parse
/// per reading is active; intervening source changes coalesce into the latest.
#[derive(Default)]
pub struct HtmlContent {
    source: String,
    widget: Option<WidgetUid>,
    applied: bool,
    pending: Option<Pending>,
}

impl HtmlContent {
    pub fn set(&mut self, cx: &mut Cx, view: HtmlRef, text: &str) {
        let Some(mut view) = view.borrow_mut() else {
            return;
        };
        let widget = view.widget_uid();
        if self.widget != Some(widget) || self.source != text {
            self.source = text.to_owned();
            self.widget = Some(widget);
            self.applied = text.is_empty();
            // The widget's public text API resets selection, details and child
            // nodes. An empty document has constant parsing cost and ensures a
            // recycled row cannot show another message while this one loads.
            view.set_text(cx, "");
        }
        if self.applied {
            return;
        }
        let ready = if cfg!(headless) {
            Some(prepare(text))
        } else {
            let mut ready = None;
            if let Some(pending) = &mut self.pending {
                match pending.receive.try_recv() {
                    Ok(prepared) => {
                        if pending.source == self.source {
                            ready = Some(prepared);
                        }
                        self.pending = None;
                    }
                    Err(oneshot::error::TryRecvError::Closed) => self.pending = None,
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
            }
            if ready.is_none() && self.pending.is_none() {
                self.pending = Some(Pending {
                    source: self.source.clone(),
                    receive: start(self.source.clone(), || Cx::post_action(HtmlReady)),
                });
            }
            ready
        };
        if let Some(prepared) = ready {
            view.body.set(&prepared.body);
            view.doc = prepared.doc;
            view.redraw(cx);
            self.applied = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_parser_never_holds_the_requesting_thread() {
        let permit = kernel::runtime::block_on(readers().clone().acquire_many_owned(2)).unwrap();
        let mut receive = start("<h1>Title</h1><p>reading</p>".into(), || {});
        assert!(matches!(
            receive.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        drop(permit);
        let result = kernel::runtime::block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), receive)
                .await
                .unwrap()
                .unwrap()
        });
        assert_eq!(result.body, "<h1>Title</h1><p>reading</p>");
        assert!(result
            .doc
            .nodes
            .iter()
            .any(|n| matches!(n, HtmlNode::OpenTag { lc, .. } if *lc == live_id!(h3))));
        assert!(!result
            .doc
            .nodes
            .iter()
            .any(|n| matches!(n, HtmlNode::OpenTag { lc, .. } if *lc == live_id!(h1))));
    }
}
