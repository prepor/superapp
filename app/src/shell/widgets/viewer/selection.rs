//! Selection addresses Unicode in page order, independently of either cache.

use std::{collections::BTreeMap, ops::Range};
use makepad_widgets::{dvec2, DVec2};
use crate::reader::pdf::{TextGlyph, TextPage, TEXT_PAGE_BYTES};

pub(super) const TEXT_CACHE_BYTES: usize = 8 * 1024 * 1024;
const TEXT_CACHE_PAGES: usize = 64;
pub(super) const COPY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Position { pub page: usize, pub byte: usize }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Span { pub start: Position, pub end: Position }

impl Span {
    fn all(count: usize) -> Self {
        Self { start: Position { page: 0, byte: 0 }, end: Position { page: count.saturating_sub(1), byte: usize::MAX } }
    }

    pub fn range(self, page: usize, length: usize) -> Range<usize> {
        if page < self.start.page || page > self.end.page { return 0..0; }
        (if page == self.start.page { self.start.byte.min(length) } else { 0 })..
            (if page == self.end.page { self.end.byte.min(length) } else { length })
    }
}

/// A copy has a separate output budget; an oversized selection is never truncated.
pub(super) fn append_copy(output: &mut String, text: &str) -> Result<(), String> {
    if text.is_empty() { return Ok(()); }
    let separator = if output.is_empty() { "" } else { "\n\n" };
    let extra = separator.len() + text.len();
    if output.len() + extra > COPY_BYTES {
        return Err("Selection is too large to copy (8 MiB maximum); select a smaller range".into());
    }
    // String's usual growth may double beyond the output budget.
    output.reserve_exact(extra);
    output.push_str(separator);
    output.push_str(text);
    Ok(())
}

#[derive(Default)]
pub(super) struct Selection {
    pub pages: BTreeMap<usize, TextPage>,
    range: Option<(Position, Position)>,
    anchor: Option<(Position, Position)>,
    unit: u32,
    all: bool,
    copy: Option<(Span, Option<Result<String, String>>)>,
}

impl Selection {
    pub fn clear(&mut self) { self.range = None; self.anchor = None; self.all = false; self.copy = None; }
    pub fn has_selection(&self) -> bool { self.all || self.range.is_some_and(|(a, b)| a != b) }

    pub fn insert(&mut self, page: usize, text: TextPage) {
        self.pages.remove(&page);
        while self.pages.len() >= TEXT_CACHE_PAGES || self.bytes() + text.bytes() > TEXT_CACHE_BYTES {
            if self.pages.pop_first().is_none() { return; }
        }
        self.pages.insert(page, text);
    }

    fn bytes(&self) -> usize { self.pages.values().map(TextPage::bytes).sum() }

    /// Admit nearby pages in display priority order, reserving a full page's
    /// extraction budget for each miss. This avoids evict/reload loops at zoom out.
    pub fn request(&mut self, wanted: &[usize]) -> Option<usize> {
        let mut bytes = 0;
        let mut admitted = Vec::new();
        for &page in wanted.iter().take(TEXT_CACHE_PAGES) {
            let cost = self.pages.get(&page).map_or(TEXT_PAGE_BYTES, TextPage::bytes);
            if bytes + cost <= TEXT_CACHE_BYTES { admitted.push(page); bytes += cost; }
        }
        self.pages.retain(|page, _| admitted.contains(page));
        admitted.into_iter().find(|page| !self.pages.contains_key(page))
    }

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
        self.copy = None;
        self.extend(page, point);
    }

    pub fn extend(&mut self, page: usize, point: DVec2) {
        let Some((start, end)) = self.anchor else { return; };
        let Some(position) = self.nearest(page, point) else { return; };
        let (a, b) = self.span(position);
        let range = Some(if a < start { (end, a) } else { (start, b) });
        if self.range != range { self.copy = None; }
        self.range = range;
    }

    pub fn select_all(&mut self) { if !self.all { self.copy = None; } self.all = true; self.anchor = None; }

    fn bounds(&self, count: usize) -> Option<Span> {
        if self.all { return Some(Span::all(count)); }
        let (a, b) = self.range.filter(|(a, b)| a != b)?;
        Some(Span { start: a.min(b), end: a.max(b) })
    }

    pub fn range(&self, page: usize) -> Range<usize> {
        let length = self.pages.get(&page).map_or(0, |p| p.text.len());
        if self.all { return 0..length; }
        self.bounds(0).map_or(0..0, |span| span.range(page, length))
    }

    /// None needs a worker copy. Never return a partial result merely because
    /// a page has not been rendered or visited, or has left the text cache.
    pub fn text(&self, count: usize) -> Option<String> {
        let Some(span) = self.bounds(count) else { return Some(String::new()); };
        self.text_in(span)
    }

    fn text_in(&self, span: Span) -> Option<String> {
        if let Some((copied, Some(Ok(text)))) = &self.copy {
            if *copied == span { return Some(text.clone()); }
        }
        let mut output = String::new();
        for page in span.start.page..=span.end.page {
            let text = self.pages.get(&page)?;
            if text.error.is_some() { return None; }
            append_copy(&mut output, text.text.get(span.range(page, text.text.len()))?).ok()?;
        }
        Some(output)
    }

    pub fn full_text(&self, count: usize) -> String { self.text_in(Span::all(count)).unwrap_or_default() }

    pub fn copy(&mut self, count: usize) -> Option<String> {
        if let Some(text) = self.text(count) { self.cancel_copy(); return Some(text); }
        self.copy = self.bounds(count).map(|span| (span, None));
        None
    }

    pub fn copy_request(&self) -> Option<Span> { self.copy.as_ref().filter(|(_, result)| result.is_none()).map(|(span, _)| *span) }

    pub fn cancel_copy(&mut self) { if self.copy_request().is_some() { self.copy = None; } }

    pub fn copied(&mut self, span: Span, result: Result<String, String>) -> Option<&str> {
        if self.copy_request() != Some(span) { return None; }
        self.copy = Some((span, Some(result)));
        self.copy.as_ref()?.1.as_ref()?.as_ref().ok().map(String::as_str)
    }

    pub fn error(&self, page: usize) -> Option<String> {
        if let Some((_, Some(Err(error)))) = &self.copy { return Some(error.clone()); }
        self.pages.get(&page).and_then(|p| p.error.clone())
    }

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
        selection.insert(0, TextPage { text: "Привет 🌍".into(), ..TextPage::default() });
        selection.select_all();
        assert_eq!(selection.text(2), None, "never silently copy only the pages already loaded");
        assert!(selection.copy(2).is_none());
        assert!(selection.copy_request().is_some());
        selection.insert(1, TextPage { text: "第二页".into(), ..TextPage::default() });
        assert_eq!(selection.text(2).unwrap(), "Привет 🌍\n\n第二页");
    }

    #[test]
    fn cache_is_bounded_and_scrolling_preserves_selection_endpoints() {
        let document = Document::open(crate::reader::pdf::dense_fixture(1, 50, 80)).unwrap();
        let text = document.text(0);
        let mut selection = Selection::default();
        selection.insert(0, text.clone());
        selection.begin(0, point(&text, 0, 0.5), 2, false);
        for page in 1..300 {
            selection.insert(page, text.clone());
            assert!(selection.bytes() <= TEXT_CACHE_BYTES);
            assert!(selection.pages.len() <= TEXT_CACHE_PAGES);
        }
        assert!(selection.has_selection());
        assert!(selection.text(300).is_none(), "eviction must not shorten the selection");
        let wanted: Vec<_> = (200..300).collect();
        for _ in 0..wanted.len() {
            let Some(page) = selection.request(&wanted) else { break; };
            selection.insert(page, text.clone());
        }
        assert!(selection.request(&wanted).is_none(), "admitted pages must stop requesting, even at the budget");
        assert!(selection.pages.len() < TEXT_CACHE_PAGES && selection.bytes() <= TEXT_CACHE_BYTES);
        assert!(selection.pages.keys().all(|page| wanted.contains(page)));
        assert_eq!(selection.request(&[0]), Some(0));
        selection.insert(0, text);
        assert_eq!(selection.text(300).as_deref(), Some("p000"));
        selection.select_all();
        assert!(selection.copy(300).is_none());
        let old = selection.copy_request().unwrap();
        selection.clear();
        assert!(selection.copied(old, Ok("obsolete copy".into())).is_none());
    }

    #[test]
    fn copy_output_and_empty_page_metadata_have_budgets() {
        let mut text = "a".repeat(COPY_BYTES - 4);
        append_copy(&mut text, "Ω").unwrap();
        assert_eq!(text.len(), COPY_BYTES);
        assert!(text.capacity() <= COPY_BYTES);
        assert!(append_copy(&mut text, "more").is_err());
        let mut selection = Selection::default();
        for page in 0..1000 { selection.insert(page, TextPage::default()); }
        assert_eq!(selection.pages.len(), TEXT_CACHE_PAGES);
    }
}
