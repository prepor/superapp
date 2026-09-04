//! The compose sheet's TO field as a completion: the people this mailbox has
//! heard from, offered under the caret.
//!
//! The rich table's box over the mail world rather than over the filter
//! grammar — the same `@from:` offer, landing in a different field. The token
//! under the caret, comma-separated from its neighbours, is matched as a
//! substring against every sender the store knows, by name or by address; a
//! pick lands the **bare address**, which is what a reply prefills and what
//! the send pipeline reads.
//!
//! Spam is not in the list ([`senders`](super::model::senders) is one side of
//! that line), so nothing a compose offers came out of the junk.

use kernel::richtable::{Completion, Suggestion, MAX_SUGGESTIONS};
use kernel::store::Store;

use super::model::senders;

/// The TO field's completion.
pub struct Recipients;

/// What the caret is in the middle of typing in a recipient list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientCtx {
    /// Where the token starts: after the last comma before the caret and the
    /// spaces that follow it.
    pub start: usize,
    /// The token as typed up to the caret, lowercased.
    pub partial: String,
    /// The addresses the other tokens already hold, lowercased — offered no
    /// second time.
    pub taken: Vec<String>,
}

impl Completion for Recipients {
    type Ctx = RecipientCtx;

    fn context(&self, text: &str, cursor: usize) -> Option<RecipientCtx> {
        context(text, cursor)
    }

    fn offer(&self, store: &Store, ctx: &RecipientCtx) -> Vec<Suggestion> {
        let typed = ctx.partial.trim_end();
        let mut out: Vec<Suggestion> = senders(store)
            .iter()
            .filter(|s| {
                let email = s.email.to_lowercase();
                // Typed out in full, an address needs no completing; one
                // already in the list needs no repeating.
                email != typed
                    && !ctx.taken.contains(&email)
                    && (email.contains(typed) || s.name.to_lowercase().contains(typed))
            })
            .map(|s| {
                if s.name.is_empty() {
                    Suggestion::value(s.email.clone())
                } else {
                    Suggestion::labeled(s.name.clone(), s.email.clone())
                }
            })
            .collect();
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    fn splice(
        &self,
        text: &str,
        cursor: usize,
        ctx: &RecipientCtx,
        pick: &Suggestion,
    ) -> (String, usize) {
        let cursor = cursor.min(text.len()).max(ctx.start);
        let out = format!("{}{}{}", &text[..ctx.start], pick.value, &text[cursor..]);
        (out, ctx.start + pick.value.len())
    }
}

/// Classifies the caret in a recipient list: the token is what sits between
/// the last comma before the caret and the caret itself, less the spaces
/// after the comma. An empty token is `None` — typing is what opens the
/// offer, not landing in the field.
#[must_use]
pub fn context(text: &str, cursor: usize) -> Option<RecipientCtx> {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let before = &text[..cursor];
    let after_comma = before.rfind(',').map_or(0, |i| i + 1);
    let start = after_comma + leading_spaces(&before[after_comma..]);
    let partial = before[start..].to_lowercase();
    if partial.trim().is_empty() {
        return None;
    }
    // Every other token's address — the one under the caret is the piece that
    // starts where the token does.
    let mut taken = Vec::new();
    let mut pos = 0;
    for piece in text.split(',') {
        if pos + leading_spaces(piece) != start {
            let addr = address_of(piece.trim()).to_lowercase();
            if !addr.is_empty() {
                taken.push(addr);
            }
        }
        pos += piece.len() + 1;
    }
    Some(RecipientCtx {
        start,
        partial,
        taken,
    })
}

fn leading_spaces(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

/// The address in a recipient token: the angle-bracketed part of
/// `Name <addr>`, else the token itself.
#[must_use]
pub fn address_of(token: &str) -> &str {
    match (token.rfind('<'), token.ends_with('>')) {
        (Some(i), true) => &token[i + 1..token.len() - 1],
        _ => token,
    }
}
