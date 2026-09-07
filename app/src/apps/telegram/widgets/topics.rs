//! A searchable checklist. A click or space toggles persistent visibility.

use super::super::panels::Topics;
use crate::shell::{
    hosted::{Ask, PanelProps},
    keys::Letters,
    widgets::table,
};
use kernel::{nav::Nav, session::Session};
use makepad_widgets::*;

#[derive(Script, ScriptHook, Widget)]
pub struct TopicsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    rows: Vec<(i64, Rect)>,
    #[rust]
    filter_rect: Rect,
    /// A click can lose its caret to Makepad's pointer-event default. Keep
    /// asking until the field has received focus on a later event.
    #[rust]
    focus_filter: bool,
}

fn with_topics<R>(props: &PanelProps, f: impl FnOnce(&mut Topics) -> R) -> Option<R> {
    let mut panel = props.panel.borrow_mut();
    Some(f(panel.as_any().downcast_mut::<Topics>()?))
}

impl Widget for TopicsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let field = self.view.text_input(cx, ids!(filter_input));
        let focused = scope
            .data
            .get_mut::<Session>()
            .is_some_and(|s| s.focus() == Some(props.slot));
        if self.focus_filter && focused {
            if field.key_focus(cx) {
                self.focus_filter = false;
            } else {
                field.set_key_focus(cx);
                self.view.redraw(cx);
            }
        }
        let field_focused = field.key_focus(cx);
        if field_focused {
            props.chord.field(Letters::ALL);
        }
        let mut toggle = None;
        if let Some(ask) = props.grab.ask() {
            if let Ask::Mark(at) = ask {
                toggle = self
                    .rows
                    .iter()
                    .find(|(_, rect)| rect.contains(at))
                    .map(|r| r.0);
            }
        } else {
            if let Event::KeyDown(k) = event {
                if focused && field_focused && k.modifiers.logo {
                    props.chord.take();
                }
                if focused && !k.modifiers.logo && !k.modifiers.control && !k.modifiers.alt {
                    match k.key_code {
                        KeyCode::Slash if !field_focused => {
                            self.focus_filter = true;
                            field.set_key_focus(cx);
                            self.view.redraw(cx);
                            return;
                        }
                        KeyCode::Space if !field_focused => {
                            toggle = with_topics(&props, |p| p.cursor()).flatten();
                        }
                        KeyCode::ArrowDown | KeyCode::ArrowUp
                            if !field_focused || k.key_code == KeyCode::ArrowDown =>
                        {
                            self.focus_filter = false;
                            cx.set_key_focus(self.view.area());
                            let delta = if k.key_code == KeyCode::ArrowDown {
                                1
                            } else {
                                -1
                            };
                            if let Some(index) = with_topics(&props, |p| p.walk(delta)).flatten() {
                                self.view
                                    .portal_list(cx, ids!(list))
                                    .smooth_scroll_to(cx, index, 90.0, None, 0.0);
                            }
                            self.view.redraw(cx);
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                s.redraw();
                            }
                            return;
                        }
                        _ => {}
                    }
                }
            }
            self.view.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                if field.changed(actions).is_some() {
                    with_topics(&props, |p| p.set_filter(field.text()));
                    self.view
                        .portal_list(cx, ids!(list))
                        .set_first_id_and_scroll(0, 0.0);
                    self.view.redraw(cx);
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        s.redraw();
                    }
                }
                if field.returned(actions).is_some() || field.escaped(actions) {
                    self.focus_filter = false;
                    cx.set_key_focus(self.view.area());
                    with_topics(&props, |p| p.walk(0));
                }
            }
            if let Event::MouseDown(e) = event {
                let hit = props.hits.at(e.abs).filter(|h| h.slot == Some(props.slot));
                if hit.as_ref().is_some_and(|h| h.rect == self.filter_rect) {
                    self.focus_filter = true;
                    field.set_key_focus(cx);
                    self.view.redraw(cx);
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        s.nav(Nav::Focus(props.slot));
                        s.redraw();
                    }
                    return;
                }
                if hit.is_some() {
                    self.focus_filter = false;
                    cx.set_key_focus(self.view.area());
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        s.nav(Nav::Focus(props.slot));
                    }
                }
                toggle = hit.and_then(|hit| {
                    self.rows
                        .iter()
                        .find(|(_, rect)| *rect == hit.rect)
                        .map(|r| r.0)
                });
            }
        }
        if let Some(id) = toggle {
            self.focus_filter = false;
            cx.set_key_focus(self.view.area());
            if let Some(s) = scope.data.get_mut::<Session>() {
                with_topics(&props, |p| p.toggle(id, s));
                s.nav(Nav::Focus(props.slot));
            }
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let field = self.view.text_input(cx, ids!(filter_input));
        if field.key_focus(cx) {
            props.chord.field(Letters::ALL);
        }
        with_topics(&props, |p| p.set_filter(field.text()));
        let Some((topics, cursor, status, empty)) =
            with_topics(&props, |p| (p.rows(), p.cursor(), p.status(), p.empty()))
        else {
            return DrawStep::done();
        };
        self.view.label(cx, ids!(status_lbl)).set_text(cx, &status);
        self.view.label(cx, ids!(empty_lbl)).set_text(cx, empty);
        self.view
            .label(cx, ids!(empty_lbl))
            .set_visible(cx, topics.is_empty());
        let mut drawn = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, topics.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(topic) = topics.get(index) else {
                    continue;
                };
                let row = list.item(cx, index, live_id!(row));
                let line = table::line(cx, &row, cursor == Some(topic.id), topic.selected);
                line.label(cx, ids!(body.check_lbl))
                    .set_text(cx, if topic.selected { "[x]" } else { "[ ]" });
                line.label(cx, ids!(body.name_lbl))
                    .set_text(cx, &topic.name);
                let mut detail = if topic.selected {
                    "shown in chats".to_string()
                } else {
                    "hidden from chats".to_string()
                };
                if topic.unread > 0 {
                    detail.push_str(&format!(" · {} unread", topic.unread));
                }
                if topic.closed {
                    detail.push_str(" · closed");
                }
                if topic.hidden {
                    detail.push_str(" · hidden in Telegram");
                }
                line.label(cx, ids!(body.detail_lbl)).set_text(cx, &detail);
                row.draw_all(cx, scope);
                drawn.push((index, row));
            }
        }
        self.rows.clear();
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        for (index, row) in drawn {
            let mut rect = row.area().rect(cx);
            if clip.size.y > 0.0 {
                let top = rect.pos.y.max(clip.pos.y);
                let bottom = (rect.pos.y + rect.size.y).min(clip.pos.y + clip.size.y);
                rect.pos.y = top;
                rect.size.y = bottom - top;
            }
            if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
                continue;
            }
            props.hits.add_row(
                topics[index].name.clone(),
                rect,
                MouseCursor::Hand,
                props.slot,
            );
            self.rows.push((topics[index].id, rect));
        }
        for path in [ids!(status_lbl), ids!(empty_lbl)] {
            let label = self.view.label(cx, path);
            let rect = label.area().rect(cx);
            if rect.size.x > 0.0 && rect.size.y > 0.0 {
                props
                    .hits
                    .add(label.text(), rect, MouseCursor::Default, props.slot);
            }
        }
        let rect = self.view.text_input(cx, ids!(filter_input)).area().rect(cx);
        self.filter_rect = rect;
        props
            .hits
            .add("filter topics", rect, MouseCursor::Text, props.slot);
        if self.focus_filter
            && scope
                .data
                .get_mut::<Session>()
                .is_some_and(|s| s.focus() == Some(props.slot))
        {
            field.set_key_focus(cx);
            self.view.redraw(cx);
        }
        DrawStep::done()
    }
}
