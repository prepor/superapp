//! How a letter reads: the two readings, the quoted tail folded away, and
//! how long the whole of it is.
//!
//! A letter arrives as text, or as text *and* HTML. A reader draws the HTML
//! when there is one; a reply quotes the text. Both are split the same way —
//! what the author wrote, then the letter they wrote it over — because a
//! conversation shows the quote as the message above it.
//!
//! The measures here feed a panel's height wish and nothing else: nobody sees
//! [`html::plain`](super::html::plain)'s output, and a wish only has to land
//! on the right grid row.

use std::collections::BTreeSet;

use super::html;
use super::model::{MailFull, MailId, ThreadMail};

/// A plain-text letter split into what its author wrote and the quoted tail
/// they wrote it over — the `On … wrote:` line and the `>` block under it,
/// when that is how the text ends. A letter that is all quote stays whole.
#[must_use]
pub fn split_quote(text: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = lines.len();
    while i > 0 && {
        let l = lines[i - 1].trim();
        l.is_empty() || l.starts_with('>')
    } {
        i -= 1;
    }
    if !lines[i..].iter().any(|l| l.trim_start().starts_with('>')) {
        return (text.to_string(), None);
    }
    let mut start = i;
    let mut j = i;
    while j > 0 && lines[j - 1].trim().is_empty() {
        j -= 1;
    }
    if j > 0 && lines[j - 1].trim_end().ends_with("wrote:") {
        start = j - 1;
        // A wrapped attribution: `On …` on one line, `… wrote:` on the next.
        if !lines[start].trim_start().starts_with("On ")
            && start > 0
            && lines[start - 1].trim_start().starts_with("On ")
        {
            start -= 1;
        }
    }
    let own = lines[..start].join("\n").trim_end().to_string();
    if own.is_empty() {
        return (text.to_string(), None);
    }
    (own, Some(lines[start..].join("\n").trim().to_string()))
}

/// The HTML reading split the same way: at the first `<blockquote>`, with
/// the attribution line right before it going along — a paragraph of its
/// own, or the last of a run of `<br>`-separated lines, which is what the
/// narrowing makes of the `<div>` Gmail and Apple Mail write it in. A
/// wrapped attribution (`On …` above `… wrote:`) goes as a whole.
#[must_use]
pub fn split_quote_html(doc: &str) -> (String, Option<String>) {
    let Some(at) = doc.find("<blockquote") else {
        return (doc.to_string(), None);
    };
    let head = &doc[..at];
    let wrote = |from: usize| html::plain(&head[from..]).trim_end().ends_with("wrote:");
    let on = |from: usize, to: usize| html::plain(&head[from..to]).trim_start().starts_with("On ");
    // Where the line ending at `end` begins: at its own `<p>`, or after the
    // last `<br>` — unless that `<br>` sits inside a paragraph still open,
    // in which case the paragraph is the line.
    let line_start = |end: usize| -> usize {
        let h = &head[..end];
        let p = h.rfind("<p>");
        let closed = h.rfind("</p>");
        let br = h.rfind("<br>").map(|i| i + 4);
        match (p, br) {
            (Some(p), Some(b)) if b > p && closed.is_some_and(|c| p < c && c < b) => b,
            (Some(p), _) => p,
            (None, Some(b)) => b,
            (None, None) => 0,
        }
    };
    let mut cut = at;
    let last = line_start(at);
    if wrote(last) {
        cut = last;
        if !on(last, at) {
            let end = if head[..last].ends_with("<br>") {
                last - 4
            } else {
                last
            };
            let prev = line_start(end);
            if prev < last && on(prev, last) {
                cut = prev;
            }
        }
    }
    let mut own = doc[..cut].trim_end().to_string();
    while own.ends_with("<br>") {
        own.truncate(own.len() - 4);
    }
    if html::plain(&own).trim().is_empty() {
        return (doc.to_string(), None);
    }
    (own, Some(doc[cut..].to_string()))
}

/// What the author wrote, as plain text — the reading a collapsed line
/// previews and the height wish measures.
#[must_use]
pub fn own_text(m: &MailFull) -> String {
    match &m.html {
        Some(h) => html::plain(&split_quote_html(h).0),
        None => split_quote(&m.body).0,
    }
}

/// Lines a text takes wrapped at `cols` columns, counted by character.
fn wrapped_lines(text: &str, cols: usize) -> usize {
    let cols = cols.max(1);
    text.lines()
        .map(|l| l.chars().count().div_ceil(cols).max(1))
        .sum::<usize>()
        .max(1)
}

/// How many lines the letter reads as when wrapped at `cols` columns — how
/// *long* a mail is. A test's own measure: a reader asks
/// [`thread_lines`] for the whole conversation.
///
/// The reading measured is the one the panel draws: the HTML when the sender
/// sent one, the plain text otherwise. Wrapping is counted by character, so a
/// real word wrap breaks a line or two earlier than this says; the wish
/// rounds up to whole rows and swallows the difference.
#[cfg(test)]
#[must_use]
pub fn reading_lines(m: &MailFull, cols: usize) -> usize {
    let text = match &m.html {
        Some(h) => html::plain(h),
        None => m.body.clone(),
    };
    wrapped_lines(&text, cols)
}

/// How many lines a conversation reads as, wrapped at `cols`. A closed
/// message is its one row, which with its inset stands half a line taller
/// than a line of text; an open one is that row, its own text (the quote
/// folded), the status line if it has one, the line its parts are listed on
/// if it carries any, and the spacing and rule around them — about four lines
/// beyond the text. An estimate, like the chrome allowance it feeds: the wish
/// only has to land on the right grid row.
///
/// `carries` is which mails have parts; the caller has the store and this
/// does not.
#[must_use]
pub fn thread_lines(
    msgs: &[ThreadMail],
    open: &BTreeSet<MailId>,
    carries: &BTreeSet<MailId>,
    cols: usize,
) -> usize {
    let lines: f64 = msgs
        .iter()
        .map(|t| {
            if open.contains(&t.mail.head.id) {
                4.0 + wrapped_lines(&own_text(&t.mail), cols) as f64
                    + if t.mail.status.is_some() { 1.0 } else { 0.0 }
                    + if carries.contains(&t.mail.head.id) {
                        1.0
                    } else {
                        0.0
                    }
            } else {
                1.5
            }
        })
        .sum();
    (lines.ceil() as usize).max(1)
}
