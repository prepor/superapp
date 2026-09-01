//! The semantic content vocabulary — panel bodies as styled lines on a
//! character grid, plus the **form components** built from them.
//!
//! This is the seed of the high-level component library (stelaxis's
//! philosophy: name the *meaning* — section, field, action — never the
//! pixels). Panels compose these; the shell draws [`Line`]s and knows
//! nothing about forms. As kinds grow richer, vocabulary lands here, not
//! in per-panel ad-hoc line building.

use crate::core::{Kind, MailId};
use crate::theme;

// ---------------------------------------------------------------------------
// Content model: panel bodies as styled lines on a character grid
// ---------------------------------------------------------------------------

/// Text styles the content grammar needs. Everything monochrome except `Err`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Style {
    /// Body text.
    N,
    /// Fake-bold body text (unread rows).
    Bold,
    /// A bigger fake-bold heading (the contact name).
    Big,
    /// Secondary.
    T2,
    /// Muted.
    Muted,
    /// Uppercase small tracked label.
    Label,
    /// The one colour: errors.
    Err,
}

impl Style {
    pub fn color(self) -> theme::Rgba {
        match self {
            Style::N | Style::Bold | Style::Big => theme::INK,
            Style::T2 => theme::TEXT2,
            Style::Muted => theme::MUTED,
            Style::Label => theme::TEXT2,
            Style::Err => theme::ERR,
        }
    }
    pub fn size(self) -> f64 {
        match self {
            Style::Label => theme::LABEL_SIZE,
            Style::Big => theme::FONT_SIZE * 1.25,
            _ => theme::FONT_SIZE,
        }
    }
}

/// Side-effect buttons (never navigation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BtnAct {
    Refresh,
    Archive,
    Send,
    Discard,
    TryIt,
}

/// Text fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldId {
    Filter,
    To,
    Subject,
    Body,
    /// Settings form: address, app password, IMAP host, SMTP host.
    SetEmail,
    SetPass,
    SetImap,
    SetSmtp,
}

/// One run inside a line.
#[derive(Debug, Clone)]
pub enum Seg {
    /// Plain text.
    T(String, Style),
    /// A link: solid underline opens joined, dotted replaces in place.
    Link {
        label: String,
        target: Kind,
        dotted: bool,
    },
    /// A bordered side-effect button.
    Btn { label: String, act: BtnAct },
    /// A bordered, inert key-cap chip.
    Kbd(String),
    /// A single-line text field, `w` chars wide.
    Fld { id: FieldId, w: usize },
    /// Horizontal gap, in chars.
    Sp(usize),
}

impl Seg {
    pub fn chars(&self) -> usize {
        match self {
            Seg::T(s, _) => s.chars().count(),
            Seg::Link { label, .. } => label.chars().count(),
            Seg::Btn { label, .. } => label.chars().count() + 2,
            Seg::Kbd(s) => s.chars().count() + 2,
            Seg::Fld { w, .. } => *w,
            Seg::Sp(n) => *n,
        }
    }
}

/// One line of panel content: left-aligned runs, right-aligned runs, an
/// optional hairline under it, and an optional full-row selection identity.
#[derive(Debug, Clone, Default)]
pub struct Line {
    pub left: Vec<Seg>,
    pub right: Vec<Seg>,
    pub rule: bool,
    /// Draw the rule in ink (table headers) instead of the hairline grey.
    pub rule_ink: bool,
    /// This line is a selectable inbox row for the given mail.
    pub row: Option<MailId>,
    /// Pinned above the scrolling region (the filter, table headers). Only a
    /// leading run of pinned lines is honoured.
    pub pin: bool,
}

impl Line {
    pub fn text(s: impl Into<String>, st: Style) -> Self {
        Line {
            left: vec![Seg::T(s.into(), st)],
            ..Default::default()
        }
    }
    pub fn blank() -> Self {
        Line::default()
    }
}

pub fn trunc(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn pad_to(s: &str, w: usize) -> String {
    let mut out = trunc(s, w);
    let n = out.chars().count();
    out.extend(std::iter::repeat(' ').take(w.saturating_sub(n)));
    out
}

pub fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(8);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_n = 0usize;
    for word in s.split(' ') {
        let wn = word.chars().count();
        if cur_n > 0 && cur_n + 1 + wn > cols {
            lines.push(std::mem::take(&mut cur));
            cur_n = 0;
        }
        if cur_n > 0 {
            cur.push(' ');
            cur_n += 1;
        }
        cur.push_str(word);
        cur_n += wn;
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

// ---------------------------------------------------------------------------
// Per-panel volatile UI state
// ---------------------------------------------------------------------------

/// A single-line text field.
#[derive(Debug, Clone, Default)]
pub struct TextField {
    pub text: String,
    pub caret: usize, // chars
}

impl TextField {
    pub fn insert(&mut self, s: &str) {
        let byte = char_byte(&self.text, self.caret);
        self.text.insert_str(byte, s);
        self.caret += s.chars().count();
    }
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let b0 = char_byte(&self.text, self.caret - 1);
        let b1 = char_byte(&self.text, self.caret);
        self.text.replace_range(b0..b1, "");
        self.caret -= 1;
    }
    pub fn delete(&mut self) {
        if self.caret >= self.text.chars().count() {
            return;
        }
        let b0 = char_byte(&self.text, self.caret);
        let b1 = char_byte(&self.text, self.caret + 1);
        self.text.replace_range(b0..b1, "");
    }
    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }
    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.text.chars().count());
    }
}

pub fn char_byte(s: &str, ch: usize) -> usize {
    s.char_indices()
        .nth(ch)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// The form vocabulary
// ---------------------------------------------------------------------------

/// An inert key-cap chip.
pub fn kbd(s: &str) -> Seg {
    Seg::Kbd(s.to_string())
}

/// A blank line — vertical air. Forms breathe with these.
pub fn gap() -> Line {
    Line::blank()
}

/// A section heading: tracked label over an inked rule.
pub fn section(title: &str) -> Line {
    Line {
        left: vec![Seg::T(title.into(), Style::Label)],
        rule: true,
        rule_ink: true,
        ..Default::default()
    }
}

/// A labelled single-line input, label column aligned at `label_w` chars.
pub fn field_row(label: &str, label_w: usize, id: FieldId, w: usize) -> Line {
    Line {
        left: vec![Seg::T(pad_to(label, label_w), Style::Label), Seg::Fld { id, w }],
        ..Default::default()
    }
}

/// A right-aligned action row with a muted hint on the left.
pub fn actions(hint: &str, buttons: &[(&str, BtnAct)]) -> Line {
    let mut right = Vec::new();
    for (i, (label, act)) in buttons.iter().enumerate() {
        if i > 0 {
            right.push(Seg::Sp(1));
        }
        right.push(Seg::Btn {
            label: (*label).into(),
            act: *act,
        });
    }
    Line {
        left: vec![Seg::T(hint.into(), Style::Muted)],
        right,
        ..Default::default()
    }
}

/// The tab/enter walk order of a kind's fields. Tab cycles, shift+tab
/// reverses, enter advances (and the form's submit lives past the end).
#[must_use]
pub fn field_order(kind: &Kind) -> &'static [FieldId] {
    match kind {
        Kind::Settings => &[
            FieldId::SetEmail,
            FieldId::SetPass,
            FieldId::SetImap,
            FieldId::SetSmtp,
        ],
        Kind::Compose { .. } => &[FieldId::To, FieldId::Subject, FieldId::Body],
        Kind::Inbox { .. } => &[FieldId::Filter],
        _ => &[],
    }
}

/// The field after `cur` in the walk (`dir` = ±1); `None` past either end.
#[must_use]
pub fn next_field(kind: &Kind, cur: FieldId, dir: isize) -> Option<FieldId> {
    let order = field_order(kind);
    let i = order.iter().position(|f| *f == cur)? as isize + dir;
    if i < 0 {
        return None;
    }
    order.get(i as usize).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_field_walk_matches_the_forms() {
        let s = Kind::Settings;
        assert_eq!(next_field(&s, FieldId::SetEmail, 1), Some(FieldId::SetPass));
        assert_eq!(next_field(&s, FieldId::SetSmtp, 1), None, "past the end: submit");
        assert_eq!(next_field(&s, FieldId::SetEmail, -1), None);
        let c = Kind::Compose { re: 0 };
        assert_eq!(next_field(&c, FieldId::Subject, 1), Some(FieldId::Body));
        assert_eq!(next_field(&c, FieldId::Body, -1), Some(FieldId::Subject));
    }
}
