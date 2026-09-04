//! The launcher's mail source: the people who wrote, then the letters a
//! query's words reach.
//!
//! The letters come out of the FTS5 index, best match first; the people come
//! out of the sender list, which is one row a correspondent and not a thing
//! an index is for — a contact is a fact about the mailbox, not a document in
//! it.

use kernel::search::{Abandoned, Hit, Provider};
use kernel::store::{Q, Store};

use super::model::{self, MailId};
use super::panels::{Contact, Message};

/// How many letters one question is worth showing. The index ranks them, so
/// the hundred best are the hundred a person would ever look at; past that the
/// answer is "type another word", not "scroll".
const FTS_LIMIT: i64 = 100;

/// The letters a query matches, best first. The index answers with rowids and
/// a rank; the join is what turns them into rows to show, and it reads no
/// further into `message` than a list may — everything it asks for sits
/// before the letter's own bytes.
static Q_FTS: Q = Q {
    id: "mail search",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject
          FROM message_fts JOIN message m ON m.id = message_fts.rowid
          WHERE message_fts MATCH ?1
          ORDER BY message_fts.rank
          LIMIT ?2",
    describe: "the letters a query matches, best first, out of the FTS5 index",
};

/// The mail world as a search source.
///
/// Two rules it keeps. **Poll first**: a provider's store never hears about a
/// commit by itself, so it would answer with yesterday's mail forever. And the
/// index query goes **round the cache**: its parameter is the person's typing,
/// and the result cache is keyed on parameters, so every keystroke would leave
/// an entry behind that nothing ever reads again.
pub struct MailSearch;

impl Provider for MailSearch {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn search(&self, store: &Store, query: &str, abandoned: &Abandoned) -> Vec<Hit> {
        let Some(m) = model::fts_match(query) else {
            return Vec::new();
        };
        store.poll_external();
        let mut hits = matching_senders(store, query);
        if abandoned.yes() {
            return hits;
        }
        hits.extend(matching_mail(store, &m));
        hits
    }
}

/// The people whose name or address carries every word of the query. Small
/// enough (one row a correspondent) to sift in memory — and spam is not in the
/// list, so nothing the launcher offers a card for came out of the junk.
fn matching_senders(store: &Store, query: &str) -> Vec<Hit> {
    let terms = kernel::search::terms(query);
    model::senders(store)
        .iter()
        .filter_map(|s| {
            // The name as of their latest letter, the address when they
            // signed none.
            let label = if s.name.is_empty() { &s.email } else { &s.name };
            kernel::search::matches(&terms, &[label, &s.email, "contact"])
                .then(|| Hit::found(label, &s.email, Contact::id(&s.email)))
        })
        .collect()
}

/// The letters, as the index ranks them.
fn matching_mail(store: &Store, m: &str) -> Vec<Hit> {
    let mut stmt = match store.conn().prepare_cached(Q_FTS.sql) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("search: preparing the mail index failed: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(rusqlite::params![m, FTS_LIMIT], |r| {
        Ok((
            r.get::<_, MailId>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    });
    // A malformed match string is the one error a person can cause from the
    // keyboard, and the answer to it is no rows, not a crash.
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("search: the mail index refused {m:?}: {e}");
            return Vec::new();
        }
    };
    rows.filter_map(Result::ok)
        .map(|(id, name, email, subject)| {
            let who = if name.is_empty() { &email } else { &name };
            Hit::found(model::topic_of(&subject), who, Message::id(id))
        })
        .collect()
}
