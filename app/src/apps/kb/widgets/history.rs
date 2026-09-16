//! The history, drawn as a list of revisions, and one revision as a
//! reading.

use kernel::nav::Nav;
use kernel::panel::{PanelId, Tag};
use makepad_widgets::*;

use crate::reader::{self, pictures, HtmlContent};
use crate::shell::dsl::SLinkWidgetRefExt;
use crate::shell::hosted::PanelProps;

use super::super::panels::{History, Revision};
use super::{follow, text_hit, with};

/// The agent's chat panel, named by tag rather than by app: the history's
/// link opens it where the build has one.
const CHAT: Tag = Tag("chat");

#[derive(Script, ScriptHook, Widget)]
pub struct HistoryPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for HistoryPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((revisions, slot)) = with::<History, _>(&props, |h| (h.revisions(), h.slot())) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let empty = self.view.label(cx, ids!(empty_lbl));
        empty.set_visible(cx, revisions.is_empty());
        let mut drawn: Vec<(usize, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, revisions.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(r) = revisions.get(i) else { continue };
                let row = list.item(cx, i, live_id!(row));
                row.widget(cx, ids!(line_link)).as_slink().set(
                    cx,
                    &History::line(r),
                    Nav::Open { from: slot, id: Revision::id(&r.uid), fresh: false },
                    false,
                    None,
                );
                let chat = row.widget(cx, ids!(chat_row));
                match r.chat() {
                    Some(id) => {
                        chat.set_visible(cx, true);
                        row.widget(cx, ids!(chat_row.chat_link)).as_slink().set(
                            cx,
                            "open the chat",
                            Nav::Open { from: slot, id: PanelId::new(CHAT, [id.to_string()]), fresh: false },
                            false,
                            None,
                        );
                    }
                    None => chat.set_visible(cx, false),
                }
                let msg = row.label(cx, ids!(msg_lbl));
                msg.set_text(cx, &r.message);
                msg.set_visible(cx, !r.message.is_empty());
                row.draw_all(cx, scope);
                drawn.push((i, row));
            }
        }
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        for (_, row) in drawn {
            let msg = row.label(cx, ids!(msg_lbl));
            text_hit(cx, &props, &msg, Some(clip));
        }
        if revisions.is_empty() {
            text_hit(cx, &props, &empty, None);
        }
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RevisionPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    body: HtmlContent,
}

impl Widget for RevisionPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if let Event::Actions(actions) = event {
            if pictures::landed(cx, actions) || reader::html_landed(actions) {
                self.view.redraw(cx);
            }
            let mine = self.view.widget(cx, ids!(list)).widget_uid();
            for action in actions {
                let Some(a) = action.as_widget_action() else {
                    continue;
                };
                if a.group.as_ref().map(|g| g.group_uid) != Some(mine) {
                    continue;
                }
                if let HtmlLinkAction::Clicked { url, .. } = a.cast() {
                    let props = props.clone();
                    follow(cx, scope, &url, |s, url| {
                        with::<Revision, _>(&props, |p| p.follow(s, url)).unwrap_or(false)
                    });
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((revision, page)) = with::<Revision, _>(&props, |r| (r.revision(), r.page())) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let title = self.view.label(cx, ids!(title_lbl));
        let meta = self.view.label(cx, ids!(meta_lbl));
        let Some((r, document)) = revision else {
            title.set_text(cx, "no such revision");
            meta.set_text(cx, "");
            return self.view.draw_walk(cx, scope, walk);
        };
        title.set_text(cx, &page.map_or_else(|| r.page.clone(), |p| p.title));
        meta.set_text(cx, &Revision::meta(&r));
        let html = with::<Revision, _>(&props, |p| p.html(&document)).unwrap_or_default();
        let mut drawn = None;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, 1);
            while let Some(i) = list.next_visible_item(cx) {
                if i != 0 {
                    continue;
                }
                let row = list.item(cx, i, live_id!(body));
                let view = row.html(cx, ids!(body_html));
                self.body.set(cx, view, &html);
                row.draw_all(cx, scope);
                drawn = Some(row);
            }
        }
        let pics = pictures::link_rects(cx);
        text_hit(cx, &props, &title, None);
        text_hit(cx, &props, &meta, None);
        if let Some(row) = drawn {
            let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
            let widget = row.widget(cx, ids!(body_html));
            if widget.area().is_valid(cx) {
                let area = widget.area().rect(cx);
                props.hits.add_clipped("revision body", area, clip, MouseCursor::Text, props.slot);
                for rect in reader::link_runs(cx, &row, ids!(body_html), area, &pics) {
                    props.hits.add_clipped("link", rect, clip, MouseCursor::Hand, props.slot);
                }
            }
        }
        DrawStep::done()
    }
}
