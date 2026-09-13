//! One grammar topic, drawn: its sections as rows of a list, each of its
//! own kind.

use kernel::nav::Nav;
use makepad_widgets::*;

use crate::shell::dsl::SLinkWidgetRefExt;
use crate::shell::hosted::PanelProps;

use super::super::model::{self, category_word, table_text, Section};
use super::super::panels::Topic;
use super::{text_hit, with};

/// One row of the topic's list.
enum Block {
    Text(String),
    Tip(String),
    Table(String, String),
    Example(String, String),
    Head(String),
    Note(String, String),
    Link(String, String),
}

#[derive(Script, ScriptHook, Widget)]
pub struct TopicPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for TopicPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((topic, sections, notes, related, slot)) =
            with::<Topic, _>(&props, |t| (t.topic(), t.sections(), t.notes(), t.related(), t.slot()))
        else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some(t) = topic else {
            self.view.label(cx, ids!(title_txt)).set_text(cx, "no such topic");
            self.view.label(cx, ids!(meta_lbl)).set_text(cx, "");
            self.view.label(cx, ids!(lessons_lbl)).set_text(cx, "");
            return self.view.draw_walk(cx, scope, walk);
        };
        self.view.label(cx, ids!(title_txt)).set_text(cx, &t.title);
        let mastery = t.mastery.map_or("not tracked yet".to_string(), |m| format!("mastery {m}/5"));
        self.view
            .label(cx, ids!(meta_lbl))
            .set_text(cx, &format!("{} · {} · {mastery}", t.level, category_word(&t.category)));
        let lessons = match (t.introduced, t.practiced) {
            (Some(i), Some(p)) if i != p => format!("introduced in lesson {i} · last practiced in lesson {p}"),
            (Some(i), _) => format!("introduced in lesson {i}"),
            _ => String::new(),
        };
        self.view.label(cx, ids!(lessons_lbl)).set_text(cx, &lessons);

        let mut blocks: Vec<Block> = Vec::new();
        for s in sections {
            match s {
                Section::Text(body) => blocks.push(Block::Text(body)),
                Section::Tip(body) => blocks.push(Block::Tip(body)),
                Section::Table { caption, columns, rows } => {
                    blocks.push(Block::Table(caption, table_text(&columns, &rows)));
                }
                Section::Examples(items) => {
                    for (text, note) in items {
                        blocks.push(Block::Example(text, note));
                    }
                }
            }
        }
        if !notes.is_empty() {
            blocks.push(Block::Head("FROM YOUR LESSONS".into()));
            for n in notes.iter() {
                let at = n.lesson.map_or_else(|| model::fmt_day(n.at), |l| format!("lesson {l} · {}", model::fmt_day(n.at)));
                blocks.push(Block::Note(n.note.clone(), at));
            }
        }
        if !related.is_empty() {
            blocks.push(Block::Head("RELATED".into()));
            for (id, title) in related {
                blocks.push(Block::Link(id, title));
            }
        }

        let mut drawn: Vec<(usize, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, blocks.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(b) = blocks.get(i) else { continue };
                let row = match b {
                    Block::Text(body) => {
                        let row = list.item(cx, i, live_id!(text));
                        row.label(cx, ids!(txt)).set_text(cx, body);
                        row
                    }
                    Block::Tip(body) => {
                        let row = list.item(cx, i, live_id!(tip));
                        row.label(cx, ids!(box.txt)).set_text(cx, body);
                        row
                    }
                    Block::Table(caption, text) => {
                        let row = list.item(cx, i, live_id!(table));
                        let cap = row.label(cx, ids!(box.cap_lbl));
                        cap.set_visible(cx, !caption.is_empty());
                        cap.set_text(cx, &caption.to_uppercase());
                        row.text_input(cx, ids!(box.table_txt)).set_text(cx, text);
                        row
                    }
                    Block::Example(text, note) => {
                        let row = list.item(cx, i, live_id!(example));
                        row.label(cx, ids!(txt)).set_text(cx, text);
                        let n = row.label(cx, ids!(note_lbl));
                        n.set_visible(cx, !note.is_empty());
                        n.set_text(cx, note);
                        row
                    }
                    Block::Head(cap) => {
                        let row = list.item(cx, i, live_id!(head));
                        row.label(cx, ids!(cap_lbl)).set_text(cx, cap);
                        row
                    }
                    Block::Note(note, at) => {
                        let row = list.item(cx, i, live_id!(note));
                        row.label(cx, ids!(note_lbl)).set_text(cx, note);
                        row.label(cx, ids!(where_lbl)).set_text(cx, at);
                        row
                    }
                    Block::Link(id, title) => {
                        let row = list.item(cx, i, live_id!(link));
                        row.widget(cx, ids!(link)).as_slink().set(
                            cx,
                            title,
                            Nav::Replace { slot, id: Topic::id(id) },
                            true,
                            None,
                        );
                        row
                    }
                };
                row.draw_all(cx, scope);
                drawn.push((i, row));
            }
        }
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        let title = self.view.label(cx, ids!(title_txt));
        text_hit(cx, &props, &title, None);
        let meta = self.view.label(cx, ids!(meta_lbl));
        text_hit(cx, &props, &meta, None);
        for (i, row) in drawn {
            match blocks.get(i) {
                Some(Block::Text(_) | Block::Example(_, _)) => {
                    let l = row.label(cx, ids!(txt));
                    text_hit(cx, &props, &l, Some(clip));
                }
                Some(Block::Tip(_)) => {
                    let l = row.label(cx, ids!(box.txt));
                    text_hit(cx, &props, &l, Some(clip));
                }
                Some(Block::Table(_, _)) => {
                    let w = row.widget(cx, ids!(box.table_txt));
                    props.hits.add_clipped("table", w.area().rect(cx), clip, MouseCursor::Text, props.slot);
                }
                Some(Block::Note(_, _)) => {
                    let l = row.label(cx, ids!(note_lbl));
                    text_hit(cx, &props, &l, Some(clip));
                }
                Some(Block::Head(_)) => {
                    let l = row.label(cx, ids!(cap_lbl));
                    text_hit(cx, &props, &l, Some(clip));
                }
                _ => {}
            }
        }
        DrawStep::done()
    }
}
