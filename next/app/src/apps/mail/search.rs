//! The launcher's mail source: the letters a query's words reach.
//!
//! `LIKE` over the sender and the subject, every word required — no index.
//! The prototype's mailbox is a few dozen letters, and a full-text index is
//! a schema, a back-fill and a query language for a scan that costs
//! microseconds here.

use kernel::search::{Abandoned, Hit, Provider};
use kernel::store::Store;
use kernel::time::fmt_date;

use super::model::MailId;
use super::panels::Message;

/// How many letters one question is worth showing.
const LIMIT: usize = 50;

/// The mail world as a search source.
///
/// Two rules it keeps. **Poll first**: a provider's store never hears about a
/// commit by itself, so it would answer with yesterday's mail forever. And the
/// query goes **round the cache**: its parameter is the person's typing, and
/// the result cache is keyed on parameters, so every keystroke would leave an
/// entry behind that nothing ever reads again.
pub struct MailSearch;

impl Provider for MailSearch {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn search(&self, store: &Store, query: &str, _abandoned: &Abandoned) -> Vec<Hit> {
        store.poll_external();
        let terms = kernel::search::terms(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut sql = String::from(
            "SELECT m.id, m.from_name, m.from_email, m.subject, m.date
             FROM message m JOIN folder f ON f.id = m.folder
             WHERE f.role IS NOT 'trash'",
        );
        let mut params: Vec<String> = Vec::new();
        for t in &terms {
            sql.push_str(
                " AND (m.from_name LIKE ? ESCAPE '\\'
                    OR m.from_email LIKE ? ESCAPE '\\'
                    OR m.subject LIKE ? ESCAPE '\\')",
            );
            let pat = format!("%{}%", escape_like(t));
            params.extend([pat.clone(), pat.clone(), pat]);
        }
        sql.push_str(" ORDER BY m.date DESC, m.id DESC LIMIT ?");
        let Ok(mut stmt) = store.conn().prepare(&sql) else {
            return Vec::new();
        };
        let bound: Vec<rusqlite::types::Value> = params
            .into_iter()
            .map(rusqlite::types::Value::Text)
            .chain(Some(rusqlite::types::Value::Integer(LIMIT as i64)))
            .collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(bound.iter()), |r| {
            Ok((
                r.get::<_, MailId>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
            ))
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.filter_map(Result::ok)
            .map(|(id, name, email, subject, date)| {
                let who = if name.is_empty() { &email } else { &name };
                Hit::found(subject, format!("{who} · {}", fmt_date(date)), Message::id(id))
            })
            .collect()
    }
}

/// A word as a `LIKE` pattern's literal: the two wildcards and the escape
/// itself are spelled out, so a query with an underscore in it means one.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
