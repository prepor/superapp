//! An inverted index for literal, Unicode case-insensitive substrings.
//!
//! FTS5's word tokenizer cannot find an infix, and its trigram tokenizer
//! cannot answer one- or two-character searches. Index character n-grams of
//! lengths 1–3 as ASCII tokens instead. Short queries are exact index lookups;
//! longer queries intersect their trigrams, then verify only the candidates.
//! No positions or message bodies are duplicated in this index.

use std::fmt::Write;
use std::rc::Rc;

use kernel::filter::Ast;
use kernel::richtable::{Datasource, Sql, SqlSource, Suggestion, TagDef};
use kernel::store::{Store, Val};
use rusqlite::functions::FunctionFlags;
use rusqlite::Connection;

use super::model::{self, MsgHit};

/// Broad matches should read the newest rows in date order, rather than
/// fetching and sorting tens of thousands of message records for one page.
pub(super) const BROAD: usize = 10_000;
pub(super) const RECENT_FROM: &str = "tg_message m INDEXED BY tg_message_date
    JOIN tg_peer p ON p.id = m.chat LEFT JOIN tg_peer s ON s.id = m.sender";
const COUNT_FROM: &str = "tg_message m INDEXED BY tg_message_search_meta
    JOIN tg_peer p ON p.id = m.chat LEFT JOIN tg_peer s ON s.id = m.sender";
const CHAT_FROM: &str = "tg_message m INDEXED BY tg_message_search_chat
    JOIN tg_peer p ON p.id = m.chat LEFT JOIN tg_peer s ON s.id = m.sender";

fn scoped(ast: Option<&Ast>) -> bool {
    match ast {
        Some(Ast::Op { tag, op: kernel::filter::Op::Eq, .. }) => tag == "chat",
        Some(Ast::And(parts)) => parts.iter().any(|a| scoped(Some(a))),
        _ => false,
    }
}

/// The n-gram posting list is the exact answer for up to three characters.
/// Count it directly, without loading message records or joining peer names.
pub(super) fn short_count(store: &Store, text: &str) -> Option<usize> {
    let q = predicate(text)?;
    if q.params.len() != 1 { return None; }
    Some(read_count(store, &Sql {
        sql: "SELECT COUNT(*) FROM tg_message_substr WHERE tg_message_substr MATCH ?".into(),
        params: q.params,
    }))
}

fn read_count(store: &Store, q: &Sql) -> usize {
    store.snapshot_rows_sql_deps("telegram search count", "matching cached messages", &q.sql, &q.params,
        &["tg_message"], |r| r.get::<_, i64>(0)).first().copied().unwrap_or(0).max(0) as usize
}

/// A required text clause with few candidates should drive the message
/// lookup, even inside a large chat. Stop counting at the broad threshold;
/// common terms must not enumerate their entire posting list just to plan.
fn selective(store: &Store, ast: Option<&Ast>) -> bool {
    match ast {
        Some(Ast::Text(text)) => predicate(text).is_some_and(|q| {
            read_count(store, &Sql {
                sql: "SELECT COUNT(*) FROM (SELECT rowid FROM tg_message_substr
                    WHERE tg_message_substr MATCH ? LIMIT ?)".into(),
                params: vec![q.params[0].clone(), Val::I(BROAD as i64)],
            }) < BROAD
        }),
        Some(Ast::And(parts)) => parts.iter().any(|a| selective(store, Some(a))),
        _ => false,
    }
}

/// SQL table behavior with cheap counts and an ordered scan for broad filters.
pub struct MessageSource {
    pub sql: &'static SqlSource<MsgHit, i64>,
    pub all: bool,
}

impl Datasource for MessageSource {
    type Row = MsgHit;
    type Key = i64;

    fn tags(&self) -> &'static [TagDef] { self.sql.tags() }
    fn key(&self, row: &MsgHit) -> i64 { self.sql.key(row) }
    fn key_text(&self, key: &i64) -> String { self.sql.key_text(key) }
    fn key_parse(&self, text: &str) -> Option<i64> { self.sql.key_parse(text) }

    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize> {
        if !self.all { return self.sql.count(store, ast); }
        match ast {
            None => return Some(read_count(store, &Sql {
                sql: "SELECT COUNT(*) FROM tg_message WHERE service = 0".into(), params: vec![],
            })),
            Some(Ast::Text(text)) => {
                if let Some(n) = short_count(store, text) { return Some(n); }
            }
            _ => {}
        }
        let mut spec = *self.sql.spec;
        spec.from = if scoped(ast) && !selective(store, ast) { CHAT_FROM } else { COUNT_FROM };
        Some(read_count(store, &spec.count(self.sql.tags, ast)))
    }

    fn page(&self, store: &Store, ast: Option<&Ast>, offset: usize, limit: usize) -> Rc<Vec<MsgHit>> {
        let mut spec = *self.sql.spec;
        if self.all {
            if scoped(ast) {
                if !selective(store, ast) { spec.from = CHAT_FROM; }
            }
            else if self.count(store, ast).is_some_and(|n| n >= BROAD) { spec.from = RECENT_FROM; }
        }
        let q = spec.page(self.sql.tags, ast, offset, limit);
        store.snapshot_rows_sql_deps(spec.id, spec.describe, &q.sql, &q.params, &[], model::msg_hit_row)
    }

    fn keys(&self, store: &Store, ast: Option<&Ast>) -> Option<Vec<i64>> { self.sql.keys(store, ast) }
    fn present(&self, store: &Store, ast: Option<&Ast>, keys: &[i64]) -> Vec<i64> { self.sql.present(store, ast, keys) }
    fn by_key(&self, store: &Store, key: &i64) -> Option<MsgHit> { self.sql.by_key(store, key) }
    fn poll_keys(&self, store: &Store, ast: Option<&Ast>) -> std::task::Poll<Option<Vec<i64>>> { self.sql.poll_keys(store, ast) }
    fn poll_present(&self, store: &Store, ast: Option<&Ast>, keys: &[i64]) -> std::task::Poll<Vec<i64>> { self.sql.poll_present(store, ast, keys) }
    fn poll_by_key(&self, store: &Store, key: &i64) -> std::task::Poll<Option<MsgHit>> { self.sql.poll_by_key(store, key) }
    fn index_of(&self, store: &Store, ast: Option<&Ast>, row: &MsgHit) -> Option<usize> { self.sql.index_of(store, ast, row) }
    fn suggest(&self, store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> { self.sql.suggest(store, tag, prefix) }
}

fn token(out: &mut String, chars: &[char]) {
    out.push('g');
    for ch in chars {
        write!(out, "{:x}x", *ch as u32).expect("write to a string");
    }
    out.push(' ');
}

fn grams(text: &str) -> String {
    let folded = caseless::default_case_fold_str(text);
    let chars: Vec<char> = folded.chars().collect();
    let mut out = String::new();
    for size in 1..=3.min(chars.len()) {
        let mut windows: Vec<_> = chars.windows(size).collect();
        windows.sort_unstable();
        windows.dedup();
        for window in windows {
            token(&mut out, window);
        }
    }
    out
}

/// Register on every writer, including connections used to upgrade old stores.
pub(super) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "tg_search_grams", 1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| Ok(grams(ctx.get_raw(0).as_str()?)),
    )
}

/// A predicate over `tg_message m`, shared by both search UIs. All user text
/// stays in parameters; punctuation and FTS operators remain literal text.
pub(super) fn predicate(text: &str) -> Option<Sql> {
    let folded = caseless::default_case_fold_str(text);
    let chars: Vec<char> = folded.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut query = String::new();
    let mut windows: Vec<_> = chars.windows(chars.len().min(3)).collect();
    windows.sort_unstable();
    windows.dedup();
    // Extra grams can only narrow candidates; the verification below is
    // authoritative. Bound the FTS expression even for a pasted paragraph.
    for window in windows.into_iter().take(64) {
        token(&mut query, window);
    }
    let mut sql = Sql {
        sql: "m.seq IN (SELECT rowid FROM tg_message_substr WHERE tg_message_substr MATCH ?)".into(),
        params: vec![Val::S(query)],
    };
    if chars.len() > 3 {
        // Posting lists do not preserve adjacency or repeated occurrences.
        // Verify the literal substring after narrowing to matching row ids.
        sql.sql.push_str(" AND casefold(m.text) LIKE casefold(?) ESCAPE '\\'");
        sql.params.push(Val::S(format!("%{}%",
            text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))));
    }
    Some(sql)
}
