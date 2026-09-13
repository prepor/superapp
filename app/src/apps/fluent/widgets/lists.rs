//! The three tables: the deck, the grammar, the lessons played.

use kernel::panel::PanelId;
use kernel::richtable::{ListState, SqlSource};
use kernel::store::Store;
use makepad_widgets::*;
use std::task::Poll;

use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::model::{self, category_word, percent, relative_day, CardRow, LessonRow, TopicRow};
use super::super::panels::{Card, Cards, Grammar, History, Lesson, Topic};

pub struct CardRows;
impl RowSpec for CardRows {
    type Src = &'static SqlSource<CardRow, String>;
    type Panel = Cards;
    fn list(p: &mut Cards) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Cards) -> String {
        p.filter.clone()
    }
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &CardRow, selected: bool, marked: bool, now: f64) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.front_lbl)).set_text(cx, &r.front);
        line.label(cx, ids!(body.back_lbl)).set_text(cx, &r.back);
        line.label(cx, ids!(body.due_lbl)).set_text(cx, &due_word(r, now));
        line.label(cx, ids!(body.mastery_lbl))
            .set_text(cx, &format!("{}/5", r.mastery));
    }
    fn label(r: &CardRow, _: f64) -> String {
        r.front.clone()
    }
    fn target(r: &CardRow) -> PanelId {
        Card::id(&r.item)
    }
    fn sync_preview(p: &mut Cards, store: &Store, id: &PanelId) -> Poll<Option<usize>> {
        if id.tag != Card::TAG {
            return Poll::Ready(None);
        }
        let Some(key) = id.arg(0).map(str::to_string) else {
            return Poll::Ready(None);
        };
        if p.list.cursor_key() == Some(&key) {
            return Poll::Ready(None);
        }
        p.list.select_key(store, &key)
    }
    fn empty_line(_: &Cards, filter: &str) -> String {
        if filter.trim().is_empty() {
            "no cards yet — the tutor adds one for every word a lesson introduces"
        } else {
            "no card under this filter"
        }
        .into()
    }
}

/// `due` / `new` / `in 3 days` / `2 days ago`.
fn due_word(r: &CardRow, now: f64) -> String {
    if r.reps == 0 {
        "new".to_string()
    } else {
        match super::super::sm2::days_between(now, r.due) {
            d if d <= 0 => "due".to_string(),
            _ => relative_day(r.due, now),
        }
    }
}

pub struct TopicRows;
impl RowSpec for TopicRows {
    type Src = &'static SqlSource<TopicRow, String>;
    type Panel = Grammar;
    fn list(p: &mut Grammar) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Grammar) -> String {
        p.filter.clone()
    }
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &TopicRow, selected: bool, marked: bool, _: f64) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.title_lbl)).set_text(cx, &r.title);
        line.label(cx, ids!(body.summary_lbl)).set_text(cx, &r.summary);
        let mastery = r.mastery.map_or(String::new(), |m| format!(" · {m}/5"));
        line.label(cx, ids!(body.level_lbl))
            .set_text(cx, &format!("{}{mastery}", r.level));
    }
    fn section(row: &TopicRow, previous: Option<&TopicRow>) -> Option<String> {
        (previous.is_none_or(|p| p.category != row.category))
            .then(|| category_word(&row.category).to_uppercase())
    }
    fn label(r: &TopicRow, _: f64) -> String {
        r.title.clone()
    }
    fn target(r: &TopicRow) -> PanelId {
        Topic::id(&r.id)
    }
    fn sync_preview(p: &mut Grammar, store: &Store, id: &PanelId) -> Poll<Option<usize>> {
        if id.tag != Topic::TAG {
            return Poll::Ready(None);
        }
        let Some(key) = id.arg(0).map(str::to_string) else {
            return Poll::Ready(None);
        };
        if p.list.cursor_key() == Some(&key) {
            return Poll::Ready(None);
        }
        p.list.select_key(store, &key)
    }
    fn empty_line(_: &Grammar, filter: &str) -> String {
        if filter.trim().is_empty() {
            "noch keine Grammatik hier — sie füllt sich nach jeder Lektion"
        } else {
            "no topic under this filter"
        }
        .into()
    }
}

pub struct LessonRows;
impl RowSpec for LessonRows {
    type Src = &'static SqlSource<LessonRow, i64>;
    type Panel = History;
    fn list(p: &mut History) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &History) -> String {
        p.filter.clone()
    }
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &LessonRow, selected: bool, marked: bool, _: f64) {
        let line = table::line(cx, row, selected, marked);
        line.label(cx, ids!(body.title_lbl)).set_text(cx, &r.title);
        line.label(cx, ids!(body.date_lbl)).set_text(cx, &model::fmt_day(r.for_date));
        let detail = [
            r.accuracy.map(percent),
            r.minutes.map(|m| format!("{} min", m as i64)),
            (!r.focus.is_empty()).then(|| r.focus.join(" · ")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        line.label(cx, ids!(body.detail_lbl)).set_text(cx, &detail);
    }
    fn label(r: &LessonRow, _: f64) -> String {
        r.title.clone()
    }
    fn target(r: &LessonRow) -> PanelId {
        Lesson::id(r.id)
    }
    fn sync_preview(p: &mut History, store: &Store, id: &PanelId) -> Poll<Option<usize>> {
        if id.tag != Lesson::TAG {
            return Poll::Ready(None);
        }
        let Some(key) = id.arg(0).and_then(|k| k.parse().ok()) else {
            return Poll::Ready(None);
        };
        if p.list.cursor_key() == Some(&key) {
            return Poll::Ready(None);
        }
        p.list.select_key(store, &key)
    }
    fn empty_line(_: &History, filter: &str) -> String {
        if filter.trim().is_empty() {
            "no lesson played yet"
        } else {
            "no lesson under this filter"
        }
        .into()
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CardsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<CardRows>,
}
impl Widget for CardsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table.draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct GrammarPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<TopicRows>,
}
impl Widget for GrammarPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table.draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct HistoryPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<LessonRows>,
}
impl Widget for HistoryPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.table.draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}
