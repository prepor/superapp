//! Finding text in a terminal's scrollback and screen.
//!
//! Ghostty's own search is not in its C API yet, so this is the terminal's
//! own: the active screen is formatted as plain text, one line per grid row,
//! and scanned for the query without regard to case. A match is a row and a
//! span of that row's characters. Cells come back into it in two places
//! only — the current match, which becomes the selection and is scrolled
//! into view, and the rows the viewport shows, which are highlighted — so
//! a query that matches on every line costs one pass over the text and
//! nothing per match.
//!
//! The current match is kept as a tracked grid reference, so it follows its
//! cell through new output and pruned scrollback; a rescan finds it again by
//! its cell rather than by its row number.

use libghostty_vt::error::Error as VtError;
use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::screen::{CellWide, TrackedGridRef};
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::{Point, PointCoordinate, PointSpace, ScrollViewport};
use libghostty_vt::Terminal;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// One place the query was found: a screen row — counted from the top of
/// the scrollback — and a span of that row's text, in characters of the
/// plain line Ghostty formats for it. Sorted by row, then by start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Match {
    pub row: u32,
    pub start: u32,
    pub end: u32,
}

pub(super) struct Search {
    /// The query, case-folded; empty finds nothing.
    needle: Vec<char>,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    /// The first cell of the current match. It follows the cell through
    /// output and pruning, so a rescan can find the same match again.
    anchor: Option<TrackedGridRef>,
    /// The screen changed since the last scan.
    pub dirty: bool,
}

/// Lowercase where that keeps the character count, so that an offset into
/// the folded text is an offset into the text.
pub(super) fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// Every non-overlapping place `needle` occurs in `text`, line by line.
pub(super) fn scan(text: &str, needle: &[char]) -> Vec<Match> {
    let mut matches = Vec::new();
    if needle.is_empty() {
        return matches;
    }
    let mut line_chars = Vec::new();
    for (row, line) in text.split('\n').enumerate() {
        line_chars.clear();
        line_chars.extend(line.chars().map(fold));
        let mut i = 0;
        while i + needle.len() <= line_chars.len() {
            if line_chars[i..i + needle.len()] == needle[..] {
                matches.push(Match {
                    row: row as u32,
                    start: i as u32,
                    end: (i + needle.len()) as u32,
                });
                i += needle.len();
            } else {
                i += 1;
            }
        }
    }
    matches
}

/// Where each cell's text starts in the row's plain line, plus the line's
/// length at the end. `units` is what each cell contributes: nothing for
/// the spacer of a wide character, one character for a blank, and every
/// codepoint of a grapheme cluster — exactly what Ghostty's formatter
/// writes for it.
pub(super) fn starts(units: impl Iterator<Item = u32>) -> Vec<u32> {
    let mut at = 0;
    let mut starts = Vec::new();
    for unit in units {
        starts.push(at);
        at += unit;
    }
    starts.push(at);
    starts
}

/// The cell whose text holds character `c` of the line: the last one that
/// starts at or before it, which skips a spacer, since a spacer starts
/// where the cell after it does.
pub(super) fn cell_at(starts: &[u32], c: u32) -> u16 {
    let after = starts.partition_point(|&s| s <= c);
    after.saturating_sub(1).min(starts.len().saturating_sub(2)) as u16
}

impl Search {
    pub fn new() -> Self {
        Search {
            needle: Vec::new(),
            matches: Vec::new(),
            current: None,
            anchor: None,
            dirty: true,
        }
    }

    pub fn set_needle(&mut self, query: &str) {
        self.needle = query.chars().map(fold).collect();
    }

    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// Scan the screen again and keep the current match if the query still
    /// matches at its cell; otherwise pick one afresh.
    pub fn rescan(&mut self, term: &Terminal) -> Result<()> {
        self.dirty = false;
        self.matches.clear();
        self.current = None;
        if self.needle.is_empty() {
            self.anchor = None;
            return Ok(());
        }
        let options = FormatterOptions::new()
            .with_format(Format::Plain)
            .with_unwrap(false)
            .with_trim(true);
        // One pass and one allocation: sizing a buffer first would format
        // the whole screen twice.
        let text = Formatter::new(term, options)?.format_alloc(None)?;
        self.matches = scan(&String::from_utf8_lossy(&text), &self.needle);
        let anchored = self
            .anchor
            .as_ref()
            .map(|anchor| anchor.point(PointSpace::Screen))
            .transpose()?
            .flatten();
        if let Some(cell) = anchored {
            let lo = self.matches.partition_point(|m| m.row < cell.y);
            let hi = self.matches.partition_point(|m| m.row <= cell.y);
            if lo < hi {
                let starts = char_starts(term, cell.y)?;
                self.current =
                    (lo..hi).find(|&i| cell_at(&starts, self.matches[i].start) == cell.x);
            }
        }
        if self.current.is_none() {
            self.current = self.default_current(term)?;
        }
        Ok(())
    }

    /// The match nearest the bottom of what is on screen, or the first one
    /// below it: the newest output is where a search usually starts.
    fn default_current(&self, term: &Terminal) -> Result<Option<usize>> {
        if self.matches.is_empty() {
            return Ok(None);
        }
        let bar = term.scrollbar()?;
        let bottom = bar.offset + bar.len;
        let above = self.matches.partition_point(|m| u64::from(m.row) < bottom);
        Ok(Some(above.saturating_sub(1)))
    }

    /// Move to the previous (older, further up) or the next match, around
    /// the ends.
    pub fn step(&mut self, term: &mut Terminal, older: bool) -> Result<()> {
        if self.dirty {
            self.rescan(term)?;
        }
        let n = self.matches.len();
        if n == 0 {
            return Ok(());
        }
        let i = match self.current {
            Some(i) if older => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None => self.default_current(term)?.unwrap_or(0),
        };
        self.go_to(term, i)
    }

    /// Make match `i` the current one: select it, remember its cell, and
    /// scroll it into the middle of the viewport if it is off screen.
    pub fn go_to(&mut self, term: &mut Terminal, i: usize) -> Result<()> {
        let Some(&m) = self.matches.get(i) else {
            return Ok(());
        };
        self.current = Some(i);
        let starts = char_starts(term, m.row)?;
        let x0 = cell_at(&starts, m.start);
        let x1 = cell_at(&starts, m.end.saturating_sub(1).max(m.start));
        let start = term.grid_ref(Point::Screen(PointCoordinate { x: x0, y: m.row }))?;
        let end = term.grid_ref(Point::Screen(PointCoordinate { x: x1, y: m.row }))?;
        term.set_selection(Some(&Selection::new(start, end, false)))?;
        self.anchor =
            Some(term.track_grid_ref(Point::Screen(PointCoordinate { x: x0, y: m.row }))?);
        let bar = term.scrollbar()?;
        let row = u64::from(m.row);
        if row < bar.offset || row >= bar.offset + bar.len {
            let top = row.saturating_sub(bar.len / 2);
            term.scroll_viewport(ScrollViewport::Row(top as usize));
        }
        Ok(())
    }

    /// Which matches lie on a screen row: an index range into `matches`.
    pub fn on_row(&self, row: u32) -> std::ops::Range<usize> {
        let lo = self.matches.partition_point(|m| m.row < row);
        let hi = self.matches.partition_point(|m| m.row <= row);
        lo..hi
    }
}

/// [`starts`] for a row read cell by cell off the grid. Only for the one
/// row a match is being selected on: the viewport's rows are read off the
/// frame instead.
fn char_starts(term: &Terminal, row: u32) -> Result<Vec<u32>> {
    let cols = term.cols()?;
    let mut buf = [char::default(); 32];
    let mut units = Vec::with_capacity(usize::from(cols));
    for x in 0..cols {
        let grid_ref = term.grid_ref(Point::Screen(PointCoordinate { x, y: row }))?;
        let cell = grid_ref.cell()?;
        let unit = match cell.wide()? {
            CellWide::SpacerHead | CellWide::SpacerTail => 0,
            _ if !cell.has_text()? => 1,
            _ => match grid_ref.graphemes(&mut buf) {
                Ok(n) => n.max(1) as u32,
                Err(VtError::OutOfSpace { required }) => required as u32,
                Err(error) => return Err(error.into()),
            },
        };
        units.push(unit);
    }
    Ok(starts(units.into_iter()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanning_is_case_insensitive_and_non_overlapping() {
        let needle: Vec<char> = "aa".chars().map(fold).collect();
        assert_eq!(
            scan("xAaaa\n\naA", &needle),
            vec![
                Match {
                    row: 0,
                    start: 1,
                    end: 3
                },
                Match {
                    row: 0,
                    start: 3,
                    end: 5
                },
                Match {
                    row: 2,
                    start: 0,
                    end: 2
                },
            ]
        );
        assert!(scan("anything", &[]).is_empty());
        // A fold that would change the length keeps the character instead.
        assert_eq!(fold('İ'), 'İ');
        assert_eq!(fold('Ä'), 'ä');
    }

    #[test]
    fn cells_are_found_from_the_units_each_contributes() {
        // "a", a wide character and its spacer, a blank, then "e" with a
        // combining mark: "a日 e\u{301}".
        let starts = starts([1, 1, 0, 1, 2].into_iter());
        assert_eq!(starts, vec![0, 1, 2, 2, 3, 5]);
        assert_eq!(cell_at(&starts, 0), 0);
        assert_eq!(cell_at(&starts, 1), 1);
        assert_eq!(cell_at(&starts, 2), 3);
        assert_eq!(cell_at(&starts, 3), 4);
        assert_eq!(cell_at(&starts, 4), 4);
        // Past the end lands on the last cell rather than the sentinel.
        assert_eq!(cell_at(&starts, 9), 4);
    }
}
