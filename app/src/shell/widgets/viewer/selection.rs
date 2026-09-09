//! Selection addresses Unicode in page order; bitmap eviction cannot lose it.

use std::{collections::BTreeMap, ops::Range};
use makepad_widgets::{dvec2, DVec2};
use crate::reader::pdf::{TextGlyph, TextPage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Position { pub page: usize, pub byte: usize }

#[derive(Default)]
pub(super) struct Selection {
    pub pages: BTreeMap<usize, TextPage>,
    range: Option<(Position, Position)>,
    anchor: Option<(Position, Position)>,
    unit: u32,
    all: bool,
}

impl Selection {
    pub fn clear(&mut self) { self.range = None; self.anchor = None; self.all = false; }
    pub fn has_selection(&self) -> bool { self.all || self.range.is_some_and(|(a, b)| a != b) }

    pub fn hit(&self, page: usize, point: DVec2) -> bool {
        self.pages.get(&page).is_some_and(|page| page.glyphs.iter().any(|g| {
            let (u, v, _) = project(g, point);
            (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)
        }))
    }

    pub fn begin(&mut self, page: usize, point: DVec2, taps: u32, extend: bool) {
        self.unit = taps.min(3);
        let Some(position) = self.nearest(page, point) else { self.clear(); return; };
        let span = self.span(position);
        self.anchor = if extend { self.range.map(|(a, _)| (a, a)).or(Some(span)) } else { Some(span) };
        self.all = false;
        self.extend(page, point);
    }

    pub fn extend(&mut self, page: usize, point: DVec2) {
        let Some((start, end)) = self.anchor else { return; };
        let Some(position) = self.nearest(page, point) else { return; };
        let (a, b) = self.span(position);
        self.range = Some(if a < start { (end, a) } else { (start, b) });
    }

    pub fn select_all(&mut self) { self.all = true; self.anchor = None; }

    pub fn pending(&self, count: usize) -> bool {
        if !self.has_selection() { return false; }
        let (first, last) = if self.all { (0, count.saturating_sub(1)) }
            else { let (a, b) = self.range.unwrap(); (a.page.min(b.page), a.page.max(b.page)) };
        (first..=last).any(|page| !self.pages.contains_key(&page))
    }

    pub fn range(&self, page: usize) -> Range<usize> {
        let length = self.pages.get(&page).map_or(0, |p| p.text.len());
        if self.all { return 0..length; }
        let Some((a, b)) = self.range else { return 0..0; };
        let (a, b) = (a.min(b), a.max(b));
        if page < a.page || page > b.page { return 0..0; }
        (if page == a.page { a.byte.min(length) } else { 0 })..
            (if page == b.page { b.byte.min(length) } else { length })
    }

    /// None means some selected text is still arriving. Never copy a silent
    /// partial result merely because a page has not been rendered or visited.
    pub fn text(&self, count: usize) -> Option<String> {
        if !self.has_selection() { return Some(String::new()); }
        let (first, last) = if self.all { (0, count.saturating_sub(1)) }
            else { let (a, b) = self.range?; (a.page.min(b.page), a.page.max(b.page)) };
        let mut parts = Vec::new();
        for page in first..=last {
            let text = &self.pages.get(&page)?.text;
            let range = self.range(page);
            if !range.is_empty() { parts.push(text.get(range)?.to_string()); }
        }
        Some(parts.join("\n\n"))
    }

    pub fn full_text(&self) -> String { self.pages.values().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n\n") }

    fn nearest(&self, page: usize, point: DVec2) -> Option<Position> {
        let text = self.pages.get(&page)?;
        let (glyph, u) = text.glyphs.iter().map(|g| { let (u, _, distance) = project(g, point); (g, u, distance) })
            .min_by(|a, b| a.2.total_cmp(&b.2)).map(|(g, u, _)| (g, u))?;
        Some(Position { page, byte: if self.unit > 1 || u < 0.5 { glyph.range.start } else { glyph.range.end } })
    }

    fn span(&self, position: Position) -> (Position, Position) {
        if self.unit < 2 { return (position, position); }
        let text = &self.pages[&position.page].text;
        let at = position.byte.min(text.len().saturating_sub(1));
        let (start, end) = if self.unit == 3 {
            let start = text[..position.byte].rfind('\n').map_or(0, |i| i + 1);
            let end = text[position.byte..].find('\n').map_or(text.len(), |i| position.byte + i);
            (start, end)
        } else {
            let chars: Vec<_> = text.char_indices().collect();
            let i = chars.partition_point(|(byte, _)| *byte <= at).saturating_sub(1);
            let kind = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 };
            let class = kind(chars[i].1);
            let mut start = i;
            let mut end = i + 1;
            if class != 2 {
                while start > 0 && kind(chars[start - 1].1) == class { start -= 1; }
                while end < chars.len() && kind(chars[end].1) == class { end += 1; }
            }
            (chars[start].0, chars.get(end).map_or(text.len(), |c| c.0))
        };
        (Position { byte: start, ..position }, Position { byte: end, ..position })
    }
}

fn project(glyph: &TextGlyph, point: DVec2) -> (f64, f64, f64) {
    let point_of = |i: usize| dvec2(glyph.quad[i][0], glyph.quad[i][1]);
    let origin = point_of(0);
    let a = point_of(1) - origin;
    let b = point_of(3) - origin;
    let delta = point - origin;
    let det = a.x * b.y - a.y * b.x;
    if det.abs() < 1e-20 { return (0.0, 0.0, f64::INFINITY); }
    let u = (delta.x * b.y - delta.y * b.x) / det;
    let v = (a.x * delta.y - a.y * delta.x) / det;
    let nearest = origin + a * u.clamp(0.0, 1.0) + b * v.clamp(0.0, 1.0);
    (u, v, (point - nearest).length())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::pdf::Document;

    fn point(page: &TextPage, byte: usize, fraction: f64) -> DVec2 {
        let g = page.glyphs.iter().find(|g| g.range.start == byte).unwrap();
        let q = g.quad.map(|p| dvec2(p[0], p[1]));
        q[0] + (q[1] - q[0]) * fraction + (q[3] - q[0]) * 0.5
    }

    #[test]
    fn selection_copies_words_lines_and_rotated_pages_in_reading_order() {
        let document = Document::open(kernel::caps::demo::PDF.to_vec()).unwrap();
        let first = document.text(0);
        let second = document.text(1);
        let mut selection = Selection::default();
        selection.pages.insert(0, first.clone());
        selection.pages.insert(1, second.clone());
        selection.begin(0, point(&first, 3, 0.8), 2, false);
        assert_eq!(selection.text(2).unwrap(), "shared");
        selection.begin(0, point(&first, 3, 0.5), 3, false);
        assert_eq!(selection.text(2).unwrap(), "A shared PDF viewer");
        let end = second.text.find('\n').unwrap();
        selection.begin(0, point(&first, first.text.find("https").unwrap(), 0.1), 1, false);
        selection.extend(1, point(&second, end - 1, 0.9));
        assert_eq!(selection.text(2).unwrap(), "https://example.com\n\nThe next page");
        // The same range swept backwards, through the rotated text.
        selection.begin(1, point(&second, end - 1, 0.9), 1, false);
        selection.extend(0, point(&first, first.text.find("https").unwrap(), 0.1));
        assert_eq!(selection.text(2).unwrap(), "https://example.com\n\nThe next page");
    }

    #[test]
    fn select_all_waits_for_unvisited_pages_and_keeps_unicode_boundaries() {
        let mut selection = Selection::default();
        selection.pages.insert(0, TextPage { text: "Привет 🌍".into(), glyphs: Vec::new() });
        selection.select_all();
        assert!(selection.pending(2));
        assert_eq!(selection.text(2), None, "never silently copy only the pages already loaded");
        selection.pages.insert(1, TextPage { text: "第二页".into(), glyphs: Vec::new() });
        assert!(!selection.pending(2));
        assert_eq!(selection.text(2).unwrap(), "Привет 🌍\n\n第二页");
    }
}
