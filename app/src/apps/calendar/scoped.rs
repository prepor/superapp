use super::model::{Event, EVENTS};
use kernel::{
    filter::{Ast, Op},
    richtable::{Datasource, Suggestion, TagDef},
    store::Store,
};
use std::rc::Rc;
/// The panel's date bounds stay separate from the person's editable filter.
pub struct Events {
    pub start: f64,
    pub end: Option<f64>,
    pub zone: String,
}
impl Events {
    fn display(&self, mut event: Event) -> Event {
        if !event.all_day {
            event.day = super::dates::day(event.start, &self.zone);
            event.zone = self.zone.clone();
        }
        event
    }
    fn query(&self, ast: Option<&Ast>) -> Ast {
        let mut a = vec![Ast::Op {
            tag: "after".into(),
            op: Op::Gt,
            value: self.start.to_string(),
        }];
        if let Some(end) = self.end {
            a.push(Ast::Op {
                tag: "before".into(),
                op: Op::Lt,
                value: end.to_string(),
            });
        }
        if let Some(ast) = ast {
            a.push(ast.clone());
        }
        Ast::And(a)
    }
}
impl Datasource for Events {
    type Row = Event;
    type Key = i64;
    fn tags(&self) -> &'static [TagDef] {
        EVENTS.tags()
    }
    fn key(&self, r: &Event) -> i64 {
        r.id
    }
    fn key_text(&self, k: &i64) -> String {
        k.to_string()
    }
    fn key_parse(&self, s: &str) -> Option<i64> {
        s.parse().ok()
    }
    fn count(&self, s: &Store, a: Option<&Ast>) -> Option<usize> {
        EVENTS.count(s, Some(&self.query(a)))
    }
    fn page(&self, s: &Store, a: Option<&Ast>, offset: usize, limit: usize) -> Rc<Vec<Event>> {
        Rc::new(
            EVENTS
                .page(s, Some(&self.query(a)), offset, limit)
                .iter()
                .cloned()
                .map(|e| self.display(e))
                .collect(),
        )
    }
    fn keys(&self, s: &Store, a: Option<&Ast>) -> Option<Vec<i64>> {
        EVENTS.keys(s, Some(&self.query(a)))
    }
    fn by_key(&self, s: &Store, k: &i64) -> Option<Event> {
        EVENTS
            .by_key(s, k)
            .filter(|e| e.end > self.start && self.end.is_none_or(|end| e.start < end))
            .map(|e| self.display(e))
    }
    fn poll_keys(&self, s: &Store, a: Option<&Ast>) -> std::task::Poll<Option<Vec<i64>>> {
        EVENTS.poll_keys(s, Some(&self.query(a)))
    }
    fn poll_present(&self, s: &Store, a: Option<&Ast>, keys: &[i64]) -> std::task::Poll<Vec<i64>> {
        EVENTS.poll_present(s, Some(&self.query(a)), keys)
    }
    fn poll_by_key(&self, s: &Store, k: &i64) -> std::task::Poll<Option<Event>> {
        EVENTS.poll_by_key(s, k).map(|row| row
            .filter(|e| e.end > self.start && self.end.is_none_or(|end| e.start < end))
            .map(|e| self.display(e)))
    }
    fn index_of(&self, s: &Store, a: Option<&Ast>, r: &Event) -> Option<usize> {
        EVENTS.index_of(s, Some(&self.query(a)), r)
    }
    fn suggest(&self, s: &Store, t: &str, p: &str) -> Vec<Suggestion> {
        EVENTS.suggest(s, t, p)
    }
}
