//! The mail domain over the store: typed queries, titles, the demo seed,
//! and the local mutations (read flags, archive).
//!
//! Everything panels show comes through the registered [`Q`] queries — that
//! is the reactive contract (see [`crate::store`]) and, later, the panel
//! context an agent receives. A mailbox — inbox, archive, sent, spam — is a
//! rich table over the [`threads`] source of its folder role: its
//! filter is the shared grammar ([`crate::filter`]), whose bare text is one
//! substring over sender + subject — the shell's original semantics; the
//! launcher's word-AND lives in [`crate::launcher`].

use std::collections::BTreeSet;
use std::rc::Rc;

use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use crate::core::{Kind, MailId, Role, Seed};
use crate::effect::{Creds, Ctx, Deferred, Effect, MailFlag, Outgoing, Registry, UidSet, World};
use crate::filter::Op;
use crate::history::Intent;
use crate::richtable::{
    Completion, Dir, SqlSource, SqlSpec, Suggestion, Table, TagDef, TagSql, TagType, Values,
    MAX_SUGGESTIONS,
};
use crate::store::{Q, Store, Val};

/// One list row: what a mailbox and the launcher show.
#[derive(Debug, Clone, PartialEq)]
pub struct MailHead {
    pub id: MailId,
    pub from_name: String,
    pub from_email: String,
    pub subject: String,
    pub date: f64,
    pub unread: bool,
}

/// One whole mail: the message panel's content.
#[derive(Debug, Clone)]
pub struct MailFull {
    pub head: MailHead,
    /// Paragraphs, `\n\n`-separated in the store.
    pub body: String,
    /// The HTML reading, narrowed at ingest to what the panel draws. When
    /// present the panel shows this instead of [`Self::body`] — the letter
    /// keeps its lists, emphasis and links.
    pub html: Option<String>,
    /// An optional status line; `true` marks it as an error.
    pub status: Option<(String, bool)>,
    /// The receiving account's address (the TO line).
    pub to: String,
    /// Passed on — the `$Forwarded` keyword, as the app or another client
    /// set it. The row draws a mark by the date.
    pub forwarded: bool,
}

/// A distinct sender: the launcher's contact entries.
#[derive(Debug, Clone)]
pub struct Sender {
    pub email: String,
    pub name: String,
}

/// One row of a mailbox (CR-007): a conversation, as far as *that folder*
/// is concerned — every message of it counts towards what the row shows,
/// and it is a row while at least one of them sits in the folder. So one
/// conversation can be a row in two mailboxes at once, the same length and
/// the same participants in both.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadHead {
    /// The anchor: the lowest member's id. The row's identity.
    pub thread: i64,
    /// The mail the row opens: the folder's oldest unread message of the
    /// conversation, else its newest one.
    pub target: MailId,
    /// Who wrote in it, newest speaker first, `me` for the account's own
    /// address — first names once there are two of them.
    pub who: Vec<String>,
    /// Its subject, reply prefixes stripped, from the oldest message.
    pub topic: String,
    /// The date of the folder's latest message of it: the order.
    pub last: f64,
    /// Any of the folder's messages of it unread.
    pub unread: bool,
    /// How many messages the whole conversation has, trash left out.
    pub n: i64,
}

impl ThreadHead {
    /// The row's first line: the participants, and the count past one.
    #[must_use]
    pub fn who_line(&self) -> String {
        let who = self.who.join(", ");
        if self.n > 1 {
            format!("{who} · {}", self.n)
        } else {
            who
        }
    }
}

/// One message of a conversation, as the thread panel draws it.
#[derive(Debug, Clone)]
pub struct ThreadMail {
    pub mail: MailFull,
    /// The role of the folder it sits in: `inbox`, `archive`, `sent`,
    /// `spam`.
    pub role: String,
    pub message_id: String,
}

static Q_INBOX: Q = Q {
    id: "inbox",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread
          FROM message m JOIN folder f ON m.folder = f.id
          WHERE f.role = 'inbox'
          ORDER BY m.date DESC, m.id DESC",
    describe: "every mail in the inbox folders, newest first",
};

/// Every mail, archived and deleted included. See [`head_row`] for why the
/// column list stops where it does.
static Q_CORPUS: Q = Q {
    id: "all_mail",
    sql: "SELECT id, from_name, from_email, subject, date, unread
          FROM message ORDER BY date DESC, id DESC",
    describe: "every mail's headers, newest first",
};

static Q_MAIL: Q = Q {
    id: "mail",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err, a.email, m.html, m.forwarded
          FROM message m JOIN account a ON a.id = m.account
          WHERE m.id = ?1",
    describe: "one mail, both bodies included, with its account's address",
};

/// Distinct senders on one side of the spam line — `?1` picks which.
///
/// Two lists rather than one, because a spammer is not a correspondent: the
/// launcher's contacts and a compose's TO may never offer one, and the spam
/// list's own `@from:` may offer nothing else. The join is what tells them
/// apart, and `folder` is a handful of rows.
static Q_SENDERS: Q = Q {
    id: "senders",
    sql: "SELECT m.from_email, m.from_name, MAX(m.date) AS last
          FROM message m JOIN folder f ON f.id = m.folder
          WHERE (COALESCE(f.role, '') = 'spam') = ?1
          GROUP BY m.from_email ORDER BY last DESC",
    describe: "distinct senders on one side of the spam line, most recently heard from first",
};

static Q_CONTACT: Q = Q {
    id: "contact",
    sql: "SELECT from_name, COUNT(*) FROM message WHERE from_email = ?1",
    describe: "a sender's display name and how many mails they sent",
};

static Q_ME: Q = Q {
    id: "me",
    sql: "SELECT email FROM account ORDER BY id LIMIT 1",
    describe: "the local account's address",
};

static Q_ACCOUNTS: Q = Q {
    id: "accounts",
    sql: "SELECT id, label, email, imap_host, smtp_host, status, synced, auth
          FROM account ORDER BY id",
    describe: "every account with its connection config, auth and sync status",
};

static Q_THREAD: Q = Q {
    id: "thread",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err, a.email, m.html, m.forwarded,
                 COALESCE(f.role, ''), COALESCE(m.message_id, '')
          FROM message m JOIN account a ON a.id = m.account
                         JOIN folder f ON f.id = m.folder
          WHERE m.thread = (SELECT thread FROM message WHERE id = ?1)
            AND f.role IS NOT 'trash'
          ORDER BY m.date, m.id",
    describe: "the conversation a mail belongs to, oldest first, trash left out",
};

static Q_THREAD_TOPIC: Q = Q {
    id: "thread topic",
    sql: "SELECT COALESCE(t.topic, t.subject) FROM message t
          WHERE t.thread = (SELECT thread FROM message WHERE id = ?1)
          ORDER BY t.date, t.id LIMIT 1",
    describe: "a conversation's subject, reply prefixes stripped, off its oldest mail",
};

static Q_THREAD_MEMBERS: Q = Q {
    id: "thread members",
    sql: "SELECT m.id, m.unread, COALESCE(f.role, '')
          FROM message m JOIN folder f ON f.id = m.folder
          WHERE m.thread = (SELECT thread FROM message WHERE id = ?1)
            AND f.role IS NOT 'trash'
          ORDER BY m.date, m.id",
    describe: "every mail of a conversation with its read flag and folder role",
};

static Q_THREAD_OF: Q = Q {
    id: "thread of",
    sql: "SELECT thread FROM message WHERE id = ?1",
    describe: "which conversation a mail belongs to",
};

/// A list row's worth of a mail, and **the last column any list may read**.
///
/// A `message` row carries the letter it arrived as: `raw` is a blob of a
/// hundred kilobytes sitting at column 11, and SQLite decodes a record left
/// to right. So a query that asks for `thread`, `topic` or `forwarded` walks
/// the overflow chain of every mail it touches — over a real mailbox that is
/// two hundred megabytes to answer one keystroke, and it is exactly what the
/// launcher used to do. `unread` is column 7; everything here is before the
/// letter, and a scan of the whole mailbox costs a millisecond.
///
/// What a conversation is called comes from [`topic_of`] the subject rather
/// than from the `topic` column, for the same reason — and it is the same
/// string, by that function's definition.
fn head_row(r: &rusqlite::Row) -> rusqlite::Result<MailHead> {
    Ok(MailHead {
        id: r.get(0)?,
        from_name: r.get(1)?,
        from_email: r.get(2)?,
        subject: r.get(3)?,
        date: r.get(4)?,
        unread: r.get(5)?,
    })
}

fn full_row(r: &rusqlite::Row) -> rusqlite::Result<MailFull> {
    let status: Option<String> = r.get(7)?;
    let err: bool = r.get(8)?;
    Ok(MailFull {
        head: head_row(r)?,
        body: r.get(6)?,
        html: r.get(10)?,
        status: status.map(|s| (s, err)),
        to: r.get(9)?,
        forwarded: r.get(11)?,
    })
}

fn thread_row(r: &rusqlite::Row) -> rusqlite::Result<ThreadMail> {
    Ok(ThreadMail {
        mail: full_row(r)?,
        role: r.get(12)?,
        message_id: r.get(13)?,
    })
}

/// Decodes one grouped row of a mailbox spec: the participants arrive
/// newest speaker first, one per sender, separated by the unit separator
/// (a name may carry a comma; none carries that).
fn thread_head_row(r: &rusqlite::Row) -> rusqlite::Result<ThreadHead> {
    let who: Option<String> = r.get(4)?;
    let mut names: Vec<String> = Vec::new();
    for n in who.as_deref().unwrap_or("").split('\u{1f}') {
        let n = n.trim();
        if !n.is_empty() && !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    }
    if names.len() > 1 {
        for n in &mut names {
            if let Some(first) = n.split_whitespace().next() {
                *n = first.to_string();
            }
        }
    }
    Ok(ThreadHead {
        thread: r.get(0)?,
        last: r.get(1)?,
        unread: r.get::<_, i64>(2)? != 0,
        target: r.get(3)?,
        who: names,
        topic: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        n: r.get(6)?,
    })
}

fn sender_row(r: &rusqlite::Row) -> rusqlite::Result<Sender> {
    Ok(Sender {
        email: r.get(0)?,
        name: r.get(1)?,
    })
}

/// The inbox, newest first.
pub fn inbox(store: &Store) -> Rc<Vec<MailHead>> {
    store.rows(&Q_INBOX, &[], head_row)
}

/// A mailbox as a rich table (CR-006) of **threads** (CR-007): the fixed
/// parts of its query, which the builder completes with the filter, the
/// page and the rank. The rows are the folder's messages grouped by
/// conversation; what a row shows is aggregates over them, or over the
/// whole conversation (participants, count, topic — trash left out), read
/// by subquery. The account join is for `@account:` and for `me`.
///
/// The role is a literal rather than a bound parameter: a [`SqlSpec`] is
/// static text, which is what lets the same builder, the same rank and the
/// same page cache serve four lists without a string being formatted per
/// keystroke. `concat!` writes the four out at compile time.
macro_rules! mailbox_spec {
    ($role:literal) => {
        SqlSpec {
            id: concat!($role, " table"),
            describe: concat!(
                "the ",
                $role,
                " as conversations under the panel's filter, latest first, one page at a time"
            ),
            select: concat!(
                "m.thread AS thread,
             MAX(m.date) AS last,
             MAX(m.unread) AS unread,
             (SELECT t.id FROM message t JOIN folder tf ON tf.id = t.folder
               WHERE t.thread = m.thread AND tf.role = '",
                $role,
                "'
               ORDER BY t.unread DESC,
                        CASE WHEN t.unread THEN t.date ELSE -t.date END, t.id
               LIMIT 1) AS target,
             (SELECT GROUP_CONCAT(
                 CASE WHEN t.from_email = ta.email THEN 'me'
                      WHEN t.from_name = '' THEN t.from_email
                      ELSE t.from_name END, char(31) ORDER BY t.date DESC)
               FROM message t JOIN folder tf ON tf.id = t.folder
                              JOIN account ta ON ta.id = t.account
               WHERE t.thread = m.thread AND tf.role IS NOT 'trash') AS who,
             (SELECT COALESCE(t.topic, t.subject) FROM message t
               WHERE t.thread = m.thread ORDER BY t.date, t.id LIMIT 1) AS topic,
             (SELECT COUNT(DISTINCT COALESCE(NULLIF(t.message_id, ''), 'id:' || t.id))
               FROM message t JOIN folder tf ON tf.id = t.folder
               WHERE t.thread = m.thread AND tf.role IS NOT 'trash') AS n"
            ),
            from: "message m JOIN folder f ON m.folder = f.id JOIN account a ON a.id = m.account",
            base: concat!("f.role = '", $role, "'"),
            text: &["m.from_name", "m.from_email", "m.subject"],
            tags: &[
                ("unread", TagSql::Where("m.unread = 1")),
                ("html", TagSql::Where("m.html IS NOT NULL")),
                ("from", TagSql::Col("(m.from_name || ' ' || m.from_email)")),
                ("subject", TagSql::Col("m.subject")),
                ("date", TagSql::Col("m.date")),
                ("account", TagSql::Col("a.email")),
            ],
            order: &[("last", Dir::Desc), ("thread", Dir::Desc)],
            group: Some("m.thread"),
            key: "thread",
        }
    };
}

static INBOX_SPEC: SqlSpec = mailbox_spec!("inbox");
static ARCHIVE_SPEC: SqlSpec = mailbox_spec!("archive");
static SENT_SPEC: SqlSpec = mailbox_spec!("sent");
static SPAM_SPEC: SqlSpec = mailbox_spec!("spam");

/// The spec one role's list runs on.
fn spec_of(role: Role) -> &'static SqlSpec {
    match role {
        Role::Inbox => &INBOX_SPEC,
        Role::Archive => &ARCHIVE_SPEC,
        Role::Sent => &SENT_SPEC,
        Role::Spam => &SPAM_SPEC,
    }
}

const DATE_OPS: &[Op] = &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte];

/// A mailbox filter's tags: what `@` offers. Each reads against the
/// folder's messages, and a conversation matches when any of them does.
/// One table for all four lists — the grammar of a mail list does not
/// change with the folder it is over.
static MAILBOX_TAGS: &[TagDef] = &[
    TagDef {
        name: "unread",
        kind: TagType::Bool,
        ops: &[],
        describe: "not read yet",
        values: Values::None,
    },
    TagDef {
        name: "html",
        kind: TagType::Bool,
        ops: &[],
        describe: "sent with an html body",
        values: Values::None,
    },
    TagDef {
        name: "from",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "sender name or address",
        values: Values::Dynamic,
    },
    TagDef {
        name: "subject",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "words of the subject",
        values: Values::None,
    },
    TagDef {
        name: "date",
        kind: TagType::Date,
        ops: DATE_OPS,
        describe: "a day, 30.08.2026",
        values: Values::None,
    },
    TagDef {
        name: "account",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "the receiving address",
        values: Values::Dynamic,
    },
];

/// Values for a mailbox's dynamic tags, under what has been typed: senders
/// (most recently heard from first, by name or address) and accounts. Each
/// is one cached query; the match is a substring, so `kov` finds Vera.
///
/// `spam` picks which side of the line the senders come from — a list
/// completes against the people who wrote *to it*.
fn suggest_mailbox(store: &Store, spam: bool, tag: &str, typed: &str) -> Vec<Suggestion> {
    match tag {
        "from" => (if spam { spam_senders(store) } else { senders(store) })
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(typed) || s.email.to_lowercase().contains(typed)
            })
            .map(|s| {
                if s.name.is_empty() {
                    Suggestion::value(s.email.clone())
                } else {
                    Suggestion::labeled(s.name.clone(), s.email.clone())
                }
            })
            .collect(),
        "account" => accounts(store)
            .iter()
            .filter(|a| a.email.to_lowercase().contains(typed))
            .map(|a| Suggestion::value(a.email.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The `@from:` completion of a list over correspondents, and of the one
/// over spam. Named functions rather than a role passed along: the field
/// is a plain `fn` pointer, and these are the only two answers.
fn suggest_correspondents(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    suggest_mailbox(store, false, tag, typed)
}

fn suggest_spam(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    suggest_mailbox(store, true, tag, typed)
}

/// One role's datasource: what that mailbox panel's rich table runs on.
/// Four values of one shape — everything but the spec is shared, because
/// a row of the sent folder is decoded, keyed and ranked exactly like a row
/// of the inbox.
macro_rules! mailbox_source {
    ($spec:expr, $suggest:expr) => {
        SqlSource {
            spec: $spec,
            tags: MAILBOX_TAGS,
            map: thread_head_row,
            key: |t| t.thread,
            rank: |t| vec![Val::F(t.last), Val::I(t.thread)],
            suggest: $suggest,
        }
    };
}

static INBOX: SqlSource<ThreadHead, i64> = mailbox_source!(&INBOX_SPEC, suggest_correspondents);
static ARCHIVE: SqlSource<ThreadHead, i64> = mailbox_source!(&ARCHIVE_SPEC, suggest_correspondents);
static SENT: SqlSource<ThreadHead, i64> = mailbox_source!(&SENT_SPEC, suggest_correspondents);
static SPAM: SqlSource<ThreadHead, i64> = mailbox_source!(&SPAM_SPEC, suggest_spam);

/// The datasource a mailbox panel of this role pages through.
#[must_use]
pub fn threads(role: Role) -> &'static SqlSource<ThreadHead, i64> {
    match role {
        Role::Inbox => &INBOX,
        Role::Archive => &ARCHIVE,
        Role::Sent => &SENT,
        Role::Spam => &SPAM,
    }
}

/// Rows per page of a mailbox table — what one scroll's worth of draws
/// fetches; the count is separate, so this is a batch size, not a limit.
pub const MAILBOX_PAGE: usize = 50;

/// One whole mailbox under a filter, materialized — for tests and the odd
/// one-shot read; panels page through [`threads`] instead.
pub fn mailbox_filtered(store: &Store, role: Role, filter: &str) -> Vec<ThreadHead> {
    let mut t = Table::new(threads(role), MAILBOX_PAGE);
    t.set_filter(filter);
    let n = t.len(store);
    t.rows(store, 0, n)
}

/// The inbox under a filter — [`mailbox_filtered`] on the role every test
/// that predates the other three means.
#[cfg(test)]
fn inbox_filtered(store: &Store, filter: &str) -> Vec<ThreadHead> {
    mailbox_filtered(store, Role::Inbox, filter)
}

/// Every mail, archived included, headers only, in one flat scan.
pub fn corpus(store: &Store) -> Rc<Vec<MailHead>> {
    store.rows(&Q_CORPUS, &[], head_row)
}

/// One mail by id.
pub fn mail(store: &Store, id: MailId) -> Option<MailFull> {
    store.rows(&Q_MAIL, &[Val::I(id)], full_row).first().cloned()
}

/// The mail as it arrived, for what its reading refers to but does not
/// hold: the images it carries as parts. `None` for the demo seed, which
/// has no raw. Read straight off the connection rather than through the
/// query cache — a blob with attachments is megabytes, and a panel wants
/// it once.
#[must_use]
pub fn raw(store: &Store, id: MailId) -> Option<Vec<u8>> {
    store
        .conn()
        .query_row("SELECT raw FROM message WHERE id = ?1", [id], |r| {
            r.get::<_, Option<Vec<u8>>>(0)
        })
        .ok()
        .flatten()
}

/// The people who have written, most recent first — spam left out, so
/// nothing the app offers to write to or to open a card for came out of
/// the junk folder.
pub fn senders(store: &Store) -> Rc<Vec<Sender>> {
    store.rows(&Q_SENDERS, &[Val::I(0)], sender_row)
}

/// The senders of the spam folder — what the spam list's own `@from:`
/// completes against, and the one place they are offered.
fn spam_senders(store: &Store) -> Rc<Vec<Sender>> {
    store.rows(&Q_SENDERS, &[Val::I(1)], sender_row)
}

// -- the launcher's mail provider --------------------------------------------

/// How many letters one question is worth showing. The index ranks them, so
/// the hundred best are the hundred a person would ever look at; past that
/// the answer is "type another word", not "scroll".
const FTS_LIMIT: i64 = 100;

/// The letters a query matches, best first. The index answers with rowids
/// and a rank; the join is what turns them into rows to show, and it reads
/// no further into `message` than [`head_row`] allows.
static Q_FTS: Q = Q {
    id: "mail search",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread
          FROM message_fts JOIN message m ON m.id = message_fts.rowid
          WHERE message_fts MATCH ?1
          ORDER BY message_fts.rank
          LIMIT ?2",
    describe: "the letters a query matches, best first, out of the FTS5 index",
};

/// The query as FTS5 reads it: every word its own quoted prefix term, all of
/// them required.
///
/// The quoting is the point. A person types `vera@kovac.io` or `re: q3` or a
/// bare `*`, and none of it may be mistaken for the match language — so the
/// words are cut out on non-alphanumeric boundaries (which is also how
/// `unicode61` tokenizes, Cyrillic included) and put back quoted, where no
/// operator can survive. The trailing `*` is what makes it type-ahead:
/// "ver kov" finds Vera Kovac on the fourth keystroke.
///
/// `None` when there is no word in it at all — the empty launcher asks
/// nothing of the mail world.
#[must_use]
pub fn fts_match(query: &str) -> Option<String> {
    let mut out = String::new();
    // [`crate::search::terms`] is where the cutting lives, so the index is
    // asked for exactly what the in-memory sources match on.
    for w in crate::search::terms(query) {
        if !out.is_empty() {
            out.push_str(" AND ");
        }
        out.push('"');
        out.push_str(&w); // no quote can be in it: it was cut on non-alphanumerics
        out.push_str("\"*");
    }
    (!out.is_empty()).then_some(out)
}

/// The mail world as a search source (CR-006): the people who wrote, then
/// the letters, best match first.
///
/// Runs on its own thread with its own reader. Two rules it keeps:
///
/// - **Poll first.** A worker's store never hears about a commit by itself,
///   so the cached reads below would answer with yesterday's senders
///   forever. [`Store::poll_external`] is what the UI thread does with the
///   same problem.
/// - **The index query goes round the cache.** Its parameter is the
///   person's typing, and the result cache is keyed on parameters: every
///   keystroke would leave an entry behind that nothing ever reads again.
pub struct Provider;

impl crate::search::Provider for Provider {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn search(
        &self,
        store: &Store,
        query: &str,
        abandoned: &crate::search::Abandoned,
    ) -> Vec<crate::search::Hit> {
        let Some(m) = fts_match(query) else {
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
/// enough (one row a correspondent) to sift in memory, and not a thing FTS5
/// indexes: a contact is a fact about the mailbox, not a document in it.
fn matching_senders(store: &Store, query: &str) -> Vec<crate::search::Hit> {
    let terms = crate::search::terms(query);
    senders(store)
        .iter()
        .filter_map(|s| {
            // The name as of their latest letter, the address when they
            // signed none.
            let label = if s.name.is_empty() { &s.email } else { &s.name };
            crate::search::matches(&terms, &[label, &s.email, "contact"]).then(|| {
                crate::search::Hit::found(
                    label,
                    &s.email,
                    Kind::Contact {
                        email: s.email.clone(),
                    },
                )
            })
        })
        .collect()
}

/// The letters, as the index ranks them.
fn matching_mail(store: &Store, m: &str) -> Vec<crate::search::Hit> {
    let mut stmt = match store.conn().prepare_cached(Q_FTS.sql) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("search: preparing the mail index failed: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(rusqlite::params![m, FTS_LIMIT], head_row);
    let rows = match rows {
        Ok(rows) => rows,
        // A malformed match string is the one error a person can cause from
        // the keyboard, and the answer to it is no rows, not a crash.
        Err(e) => {
            eprintln!("search: the mail index refused {m:?}: {e}");
            return Vec::new();
        }
    };
    rows.filter_map(Result::ok)
        .map(|c| {
            crate::search::Hit::found(
                topic_of(&c.subject),
                &c.from_name,
                Kind::Message { id: c.id },
            )
        })
        .collect()
}

/// A sender's `(name, mail count)`; the name falls back to the address.
pub fn contact(store: &Store, email: &str) -> (String, i64) {
    store
        .rows(&Q_CONTACT, &[Val::S(email.to_string())], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        })
        .first()
        .map(|(name, n)| {
            (
                name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| email.to_string()),
                *n,
            )
        })
        .unwrap_or_else(|| (email.to_string(), 0))
}

/// One account row, as settings shows it.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: i64,
    pub label: String,
    pub email: String,
    pub imap_host: Option<String>,
    pub smtp_host: Option<String>,
    pub status: Option<String>,
    pub synced: Option<f64>,
    /// How it authenticates: `NULL`/`password` for an app password,
    /// `google` for an OAuth grant. See [`crate::oauth`].
    pub auth: Option<String>,
}

fn account_row(r: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        label: r.get(1)?,
        email: r.get(2)?,
        imap_host: r.get(3)?,
        smtp_host: r.get(4)?,
        status: r.get(5)?,
        synced: r.get(6)?,
        auth: r.get(7)?,
    })
}

/// Every account.
pub fn accounts(store: &Store) -> Rc<Vec<Account>> {
    store.rows(&Q_ACCOUNTS, &[], account_row)
}

/// Creates an account (the add-account form's action, and the end of a
/// Gmail sign-in). Folders arrive with the first sync; the secret — a
/// password or a refresh token — goes to the keychain, never here.
pub fn add_account_tx(
    c: &rusqlite::Connection,
    email: &str,
    imap_host: &str,
    smtp_host: &str,
    auth: &str,
) -> rusqlite::Result<i64> {
    c.execute(
        "INSERT INTO account(label, email, imap_host, smtp_host, auth) VALUES(?1,?1,?2,?3,?4)",
        rusqlite::params![email, imap_host, smtp_host, auth],
    )?;
    Ok(c.last_insert_rowid())
}

/// Removes an account and everything it brought, `now` being the world's
/// clock (the queue it retires is timestamped with it).
pub fn remove_account_tx(c: &rusqlite::Connection, id: i64, now: f64) -> rusqlite::Result<()> {
    // Its queued work goes with it. Only this account's worker may claim
    // these ([`crate::effect::Scope`]), and that worker retires with the
    // row — so a job left behind would wait for a thread that is never
    // coming back. Obsolete rather than deleted: the log keeps what was
    // asked for.
    c.execute(
        "UPDATE effect SET status='obsolete', updated=?2
         WHERE entity=?1 AND status IN ('pending','processing')",
        rusqlite::params![format!("account:{id}"), now],
    )?;
    // What was derived from its letters goes with them (CR-010): a
    // `message` rowid is reused, and a scan row left behind would tell the
    // next letter to take that id that it had already been walked.
    for t in ["attachment", "attachment_scan"] {
        c.execute(
            &format!("DELETE FROM {t} WHERE message IN (SELECT id FROM message WHERE account=?1)"),
            [id],
        )?;
    }
    c.execute("DELETE FROM message WHERE account=?1", [id])?;
    c.execute("DELETE FROM folder WHERE account=?1", [id])?;
    c.execute("DELETE FROM account WHERE id=?1", [id])?;
    Ok(())
}

/// The local account's address.
pub fn me(store: &Store) -> String {
    store
        .rows(&Q_ME, &[], |r| r.get::<_, String>(0))
        .first()
        .cloned()
        .unwrap_or_default()
}

// -- threads (CR-007) ---------------------------------------------------------

/// The subject with its reply and forward prefixes stripped — what a
/// conversation is called, whichever of its mails you read it off.
#[must_use]
pub fn topic_of(subject: &str) -> String {
    const PREFIXES: &[&str] = &[
        "re", "fw", "fwd", "aw", "wg", "sv", "vs", "tr", "antw", "ref", "res", "rif", "odp",
        "ynt",
    ];
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let Some(colon) = lower.find(':') else { break };
        // `re[2]:` and `re (2):` count as `re`.
        let head = lower[..colon]
            .split(['[', '('])
            .next()
            .unwrap_or("")
            .trim();
        if !PREFIXES.contains(&head) {
            break;
        }
        s = s[colon + 1..].trim_start();
    }
    let s = s.trim();
    if s.is_empty() {
        subject.trim().to_string()
    } else {
        s.to_string()
    }
}

/// Decides which conversation a mail belongs to and records it, in the
/// transaction that stored the mail. Three lookups over the account, and
/// their union merges into one thread:
///
/// 1. **my references name them** — mails whose id is in my `References`;
/// 2. **they name me** — mails whose references carry my id (the parent
///    arrived late: Sent syncs after Inbox, the window, a move);
/// 3. **we name the same missing mail** — mails whose references share an
///    id with mine (two GitHub comments under an issue mail never received).
///
/// Plus a mail already here under my own id (my reply, in Sent and back
/// through a list). Every thread found merges into the lowest anchor; none
/// found, and the mail anchors itself.
pub fn thread_tx(
    c: &rusqlite::Connection,
    account: i64,
    id: MailId,
    message_id: &str,
    refs: &[String],
) -> rusqlite::Result<()> {
    c.execute("DELETE FROM reference WHERE message = ?1", [id])?;
    for r in refs {
        if !r.is_empty() {
            c.execute(
                "INSERT INTO reference(message, mid) VALUES(?1, ?2)",
                rusqlite::params![id, r],
            )?;
        }
    }
    let found: Vec<i64> = c
        .prepare(
            "SELECT DISTINCT m.thread FROM message m
             WHERE m.account = ?1 AND m.id != ?2 AND m.thread IS NOT NULL AND (
                   (?3 != '' AND m.message_id = ?3)
                OR m.message_id IN (SELECT mid FROM reference WHERE message = ?2)
                OR (?3 != '' AND EXISTS (SELECT 1 FROM reference r
                                          WHERE r.message = m.id AND r.mid = ?3))
                OR EXISTS (SELECT 1 FROM reference r JOIN reference mine ON mine.mid = r.mid
                            WHERE r.message = m.id AND mine.message = ?2))",
        )?
        .query_map(rusqlite::params![account, id, message_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let anchor = found.iter().copied().chain(std::iter::once(id)).min().unwrap_or(id);
    c.execute(
        "UPDATE message SET thread = ?1 WHERE id = ?2",
        rusqlite::params![anchor, id],
    )?;
    if !found.is_empty() {
        let list = found.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        c.execute(
            &format!("UPDATE message SET thread = ?1 WHERE account = ?2 AND thread IN ({list})"),
            rusqlite::params![anchor, account],
        )?;
    }
    Ok(())
}

/// The conversation `id` belongs to, oldest first. A mail present twice in
/// the account — my own reply, in Sent and back through a list — is one
/// message here: the copy outside Sent wins.
pub fn thread(store: &Store, id: MailId) -> Vec<ThreadMail> {
    let rows = store.rows(&Q_THREAD, &[Val::I(id)], thread_row);
    let mut out: Vec<ThreadMail> = Vec::with_capacity(rows.len());
    for m in rows.iter() {
        if !m.message_id.is_empty() {
            if let Some(i) = out.iter().position(|o| o.message_id == m.message_id) {
                if out[i].role == "sent" && m.role != "sent" {
                    out[i] = m.clone();
                }
                continue;
            }
        }
        out.push(m.clone());
    }
    out
}

/// A conversation's subject, off its oldest mail, reply prefixes stripped.
pub fn thread_topic(store: &Store, id: MailId) -> Option<String> {
    store
        .rows(&Q_THREAD_TOPIC, &[Val::I(id)], |r| r.get::<_, String>(0))
        .first()
        .cloned()
}

/// The anchor of the conversation a mail belongs to.
pub fn thread_of(store: &Store, id: MailId) -> Option<i64> {
    store
        .rows(&Q_THREAD_OF, &[Val::I(id)], |r| r.get::<_, Option<i64>>(0))
        .first()
        .cloned()
        .flatten()
}

/// Which of a conversation's mails are unread — what opening it marks.
pub fn thread_unread(store: &Store, id: MailId) -> Vec<MailId> {
    store
        .rows(&Q_THREAD_MEMBERS, &[Val::I(id)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
        })
        .iter()
        .filter(|(_, unread)| *unread)
        .map(|(id, _)| *id)
        .collect()
}

/// Which folder a mail sits in now — read before filing it, so undo puts
/// it back exactly there rather than guessing "the inbox".
#[must_use]
pub fn folder_of(store: &Store, id: MailId) -> i64 {
    store
        .conn()
        .query_row("SELECT folder FROM message WHERE id = ?1", [id], |r| r.get(0))
        .unwrap_or(0)
}

/// Which folder role a mail sits under — `inbox`, `archive`, `sent`,
/// `spam`, `trash` — or `None` for a mail whose folder plays none.
#[must_use]
pub fn role_of(store: &Store, id: MailId) -> Option<Role> {
    store
        .conn()
        .query_row(
            "SELECT f.role FROM message m JOIN folder f ON f.id = m.folder WHERE m.id = ?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .and_then(|r| Role::named(&r))
}

/// Which of a conversation's mails share this one's folder role — what
/// filing the row moves.
///
/// The row is the thread, and the mailbox it was read in decides which of
/// its mails the verb takes: archiving from the inbox takes the inbox
/// copies, deleting from the archive takes the archived ones. That mailbox
/// is not passed in — it is what the mail under the cursor is already
/// filed as, which is the same answer and cannot disagree with the list.
/// A mail in no listed role (a trashed one, reached from a reader) is
/// alone: the set is just itself.
#[must_use]
pub fn thread_siblings(store: &Store, id: MailId) -> Vec<MailId> {
    let Some(role) = role_of(store, id) else {
        return vec![id];
    };
    store
        .rows(&Q_THREAD_MEMBERS, &[Val::I(id)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(2)?))
        })
        .iter()
        .filter(|(_, r)| r == role.as_str())
        .map(|(id, _)| *id)
        .collect()
}

/// The row a mail's conversation makes in a mailbox of this role — the same
/// aggregates the table shows, for one thread — or `None` while none of it
/// sits in that folder.
pub fn thread_head(store: &Store, role: Role, id: MailId) -> Option<ThreadHead> {
    let spec = spec_of(role);
    let sql = format!(
        "SELECT {} FROM {} WHERE {} AND m.thread = (SELECT thread FROM message WHERE id = ?1)
         GROUP BY m.thread",
        spec.select, spec.from, spec.base
    );
    store
        .rows_sql(
            "thread head",
            "one conversation's row in a mailbox",
            &sql,
            &[Val::I(id)],
            thread_head_row,
        )
        .first()
        .cloned()
}

/// A plain-text letter split into what its author wrote and the quoted
/// tail they wrote it over — the `On … wrote:` line and the `>` block under
/// it, when that is how the text ends. A letter that is all quote stays
/// whole.
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
pub fn split_quote_html(html: &str) -> (String, Option<String>) {
    let Some(at) = html.find("<blockquote") else {
        return (html.to_string(), None);
    };
    let head = &html[..at];
    let wrote = |from: usize| crate::html::plain(&head[from..]).trim_end().ends_with("wrote:");
    let on = |from: usize, to: usize| {
        crate::html::plain(&head[from..to]).trim_start().starts_with("On ")
    };
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
            let end = if head[..last].ends_with("<br>") { last - 4 } else { last };
            let prev = line_start(end);
            if prev < last && on(prev, last) {
                cut = prev;
            }
        }
    }
    let mut own = html[..cut].trim_end().to_string();
    while own.ends_with("<br>") {
        own.truncate(own.len() - 4);
    }
    if crate::html::plain(&own).trim().is_empty() {
        return (html.to_string(), None);
    }
    (own, Some(html[cut..].to_string()))
}

/// What the author wrote, as plain text — the reading a collapsed line
/// previews and the height wish measures.
#[must_use]
pub fn own_text(m: &MailFull) -> String {
    match &m.html {
        Some(h) => crate::html::plain(&split_quote_html(h).0),
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

/// How many lines a conversation reads as, wrapped at `cols`. A closed
/// message is its one row, which with its inset stands half a line taller
/// than a line of text; an open one is that row, its own text (the quote
/// folded), the status line if it has one, the line its parts are listed
/// on if it carries any (CR-010), and the spacing and rule around them —
/// about four lines beyond the text. An estimate, like the chrome
/// allowance it feeds: the wish only has to land on the right grid row.
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
                    + if carries.contains(&t.mail.head.id) { 1.0 } else { 0.0 }
            } else {
                1.5
            }
        })
        .sum();
    (lines.ceil() as usize).max(1)
}

/// How many lines the letter reads as when wrapped at `cols` columns — how
/// *long* a mail is, which is what the message panel's height wish is made
/// of (the shell turns lines into grid rows).
///
/// The reading measured is the one the panel draws: the HTML when the
/// sender sent one, the plain text otherwise. Wrapping is counted by
/// character, so a real word wrap breaks a line or two earlier than this
/// says; the wish rounds up to whole rows and swallows the difference.
#[must_use]
pub fn reading_lines(m: &MailFull, cols: usize) -> usize {
    let text = match &m.html {
        Some(h) => crate::html::plain(h),
        None => m.body.clone(),
    };
    let cols = cols.max(1);
    text.lines()
        .map(|l| l.chars().count().div_ceil(cols).max(1))
        .sum::<usize>()
        .max(1)
}

/// A job's panel title: its verb and whose work it was, which is what the
/// log's row leads with too. Read off the row rather than decoded — a title
/// is drawn on every frame of a tab strip, and the sentence lives one panel
/// over anyway.
fn job_title(store: &Store, id: i64) -> String {
    store
        .conn()
        .query_row(
            "SELECT kind, entity FROM effect WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .map(|(kind, entity)| match entity {
            Some(e) if !e.is_empty() => format!("{kind} · {e}"),
            _ => kind,
        })
        .unwrap_or_else(|_| format!("job #{id}"))
}

/// The panel's display title for a kind — what headers, tab strips, the
/// overlay and the launcher all show. Data-carrying kinds resolve through
/// the store (cached like everything else).
pub fn title(store: &Store, kind: &Kind) -> String {
    match kind {
        Kind::Help => "help".into(),
        Kind::About => "about".into(),
        Kind::Mailbox { role, filter: Some(f) } => format!("{} · {f}", role.as_str()),
        Kind::Mailbox { role, filter: None } => role.as_str().into(),
        Kind::Message { id } => thread_topic(store, *id).unwrap_or_else(|| "message".into()),
        Kind::Contact { email } => contact(store, email).0,
        Kind::Compose { seed: Seed::Blank } => "new mail".into(),
        Kind::Compose {
            seed: Seed::Reply(id),
        } => mail(store, *id)
            .map(|m| format!("re: {}", m.head.subject))
            .unwrap_or_else(|| "new mail".into()),
        Kind::Compose {
            seed: Seed::Forward(id),
        } => mail(store, *id)
            .map(|m| format!("fwd: {}", m.head.subject))
            .unwrap_or_else(|| "new mail".into()),
        Kind::Settings => "settings".into(),
        Kind::AddAccount => "add account".into(),
        Kind::Problems => "problems".into(),
        Kind::Effects => "effects".into(),
        Kind::Job { id } => job_title(store, *id),
        Kind::Files { dir } => crate::files::basename(dir).into(),
        Kind::File { path } => crate::files::basename(path).into(),
        Kind::Attachment { mail, at } => {
            attachment(store, *mail, *at).map_or_else(|| "attachment".into(), |a| a.name)
        }
        Kind::Bucket => "device sync".into(),
    }
}

// -- what a letter carries (CR-010) -------------------------------------------

/// One part of a letter, as the panels see it: the row [`crate::sync`]
/// derived from the mail's `raw`. The bytes are not here — [`part`] reads
/// them back out of the letter when a card asks.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub message: MailId,
    /// Which part of the letter it is (see [`crate::sync::part_bytes`]).
    pub at: u32,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// Its Content-ID, for a part the reading refers to; empty otherwise.
    pub cid: String,
}

impl Attachment {
    /// What the message row's link says: the name, and how big it is.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, crate::files::fmt_size(self.size))
    }

    /// The card this part draws as — the same card a file on the disk
    /// draws (CR-008's, reused whole), so one widget serves both.
    #[must_use]
    pub fn card(&self, from: &str, date: f64) -> crate::files::Card {
        crate::files::Card {
            kind: crate::files::FileKind::of_name(&self.name),
            name: self.name.clone(),
            size: self.size,
            when: format!("with {from}, {}", fmt_date(date)),
            detail: self.mime.clone(),
        }
    }
}

static Q_ATTACHMENTS: Q = Q {
    id: "attachments",
    sql: "SELECT message, part, name, mime, size, cid
          FROM attachment WHERE message = ?1 ORDER BY part",
    describe: "the parts one letter carries, in the order they arrived",
};

static Q_ATTACHMENT: Q = Q {
    id: "attachment",
    sql: "SELECT message, part, name, mime, size, cid
          FROM attachment WHERE message = ?1 AND part = ?2",
    describe: "one part of a letter, by the letter and its place in it",
};

fn attachment_row(r: &rusqlite::Row) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        message: r.get(0)?,
        at: r.get::<_, i64>(1)? as u32,
        name: r.get(2)?,
        mime: r.get(3)?,
        size: r.get::<_, i64>(4)? as u64,
        cid: r.get(5)?,
    })
}

/// The parts one letter carries.
pub fn attachments(store: &Store, id: MailId) -> Rc<Vec<Attachment>> {
    store.rows(&Q_ATTACHMENTS, &[Val::I(id)], attachment_row)
}

/// One part, by the letter and its place in it — the identity a
/// [`Kind::Attachment`] persists, since the row's own id is derived and
/// local to a device.
pub fn attachment(store: &Store, mail: MailId, at: u32) -> Option<Attachment> {
    store
        .rows(
            &Q_ATTACHMENT,
            &[Val::I(mail), Val::I(i64::from(at))],
            attachment_row,
        )
        .first()
        .cloned()
}

/// One part's bytes, out of the letter that carries them. Reads the whole
/// `raw` and walks its MIME, so it belongs on a thread and never in a draw
/// (see the book's *nothing heavy in a draw*); [`crate::panels`] asks for
/// it the way it asks for a letter's pictures.
#[must_use]
pub fn part(store: &Store, a: &Attachment) -> Option<Vec<u8>> {
    crate::sync::part_bytes(&raw(store, a.message)?, a.at)
}

/// Records what a letter carries, and marks the mail walked at this
/// build's version. One transaction with the ingest that stored the letter,
/// so no draw ever sees a mail without its parts.
///
/// An **upsert on `(message, part)`**, not a delete and a re-insert: a row's
/// id is what a `Kind::Attachment` panel persists, and a re-derive — the
/// walk's version changed, or a peer's snapshot landed — must not hand that
/// id to a different part of a different letter. Parts the letter no longer
/// has go afterwards, which is the only thing a re-derive may take away.
pub fn attach_tx(
    c: &rusqlite::Connection,
    message: MailId,
    parts: &[crate::sync::Part],
) -> rusqlite::Result<()> {
    for p in parts {
        c.execute(
            "INSERT INTO attachment(message, part, name, mime, size, cid)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(message, part) DO UPDATE SET
               name = excluded.name, mime = excluded.mime,
               size = excluded.size, cid = excluded.cid",
            rusqlite::params![message, i64::from(p.at), p.name, p.mime, p.size as i64, p.cid],
        )?;
    }
    // Interpolated rather than bound: the values are part indices this
    // build just read off a MIME walk, and `NOT IN ()` is not a thing —
    // a letter that carries nothing has every row of its own to drop.
    let kept: Vec<String> = parts.iter().map(|p| p.at.to_string()).collect();
    let sql = if kept.is_empty() {
        "DELETE FROM attachment WHERE message = ?1".to_string()
    } else {
        format!(
            "DELETE FROM attachment WHERE message = ?1 AND part NOT IN ({})",
            kept.join(",")
        )
    };
    c.execute(&sql, [message])?;
    mark_scanned_tx(c, message)
}

/// Notes that this mail's `raw` has been walked at the current version —
/// including the answer "there was nothing to walk", which is why it is its
/// own function.
pub fn mark_scanned_tx(c: &rusqlite::Connection, message: MailId) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO attachment_scan(message, version) VALUES(?1, ?2)
         ON CONFLICT(message) DO UPDATE SET version = excluded.version",
        rusqlite::params![message, crate::store::attach_version()],
    )?;
    Ok(())
}

// -- what a draft will carry (CR-010) ------------------------------------------

/// One file a compose panel will carry out. The **path**, not the bytes: an
/// attach costs a `stat`, the file stays where it is, and the send is what
/// reads it — so a draft that sits for a day carries the file as it is when
/// it leaves, and a file that has moved fails the send honestly rather than
/// going out stale.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftFile {
    pub id: i64,
    /// The display path (`~/Downloads/report-q3.pdf`).
    pub path: String,
    pub name: String,
    pub size: u64,
    /// Which install attached it (`repl.device`). A path is only a file on
    /// the machine it was picked on, and these rows replicate.
    pub device: String,
}

impl DraftFile {
    /// What the compose panel's line says.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, crate::files::fmt_size(self.size))
    }
}

static Q_DRAFT_FILES: Q = Q {
    id: "draft files",
    sql: "SELECT id, path, name, size, device FROM draft_attachment
          WHERE panel = ?1 ORDER BY id",
    describe: "the files one compose panel will carry out, in the order they were attached",
};

fn draft_file_row(r: &rusqlite::Row) -> rusqlite::Result<DraftFile> {
    Ok(DraftFile {
        id: r.get(0)?,
        path: r.get(1)?,
        name: r.get(2)?,
        size: r.get::<_, i64>(3)? as u64,
        device: r.get(4)?,
    })
}

/// What a compose panel will carry out, whatever it is showing now. Callers
/// that draw or send want [`draft_files_for`], which holds the rows to the
/// same seed rule the text is held to.
pub fn draft_files(store: &Store, panel: i64) -> Rc<Vec<DraftFile>> {
    store.rows(&Q_DRAFT_FILES, &[Val::I(panel)], draft_file_row)
}

/// A panel's files, if they are `seed`'s own — the rule [`draft_for`] holds
/// the text to (CR-010). A compose retargeted in place keeps its id, so the
/// files a reply left are not the forward's, and a draft row whose seed
/// disagrees hides them until [`upsert_draft_tx`] clears them for good. A
/// panel with no draft row yet has nothing to disagree: `attach` on a blank
/// compose writes the files before the first keystroke writes the text.
pub fn draft_files_for(store: &Store, panel: i64, seed: Seed) -> Rc<Vec<DraftFile>> {
    match draft_row(store, panel) {
        Some((_, (re, fwd))) if (re, fwd) != (seed.in_reply_to(), seed.forwards()) => {
            Rc::new(Vec::new())
        }
        _ => draft_files(store, panel),
    }
}

/// Attaches a set of paths to a draft, and answers with the ones it
/// actually added — what [`Attached`] must give back, and no more: a path
/// the draft already carried was not this action's to take away.
///
/// `device` is this install's id. The rows replicate, but a path does not
/// mean the same file on two machines, so the send refuses one attached
/// somewhere else rather than carrying out whatever sits at that path here
/// (see [`load_outgoing`]).
pub fn attach_files_tx(
    c: &rusqlite::Connection,
    panel: i64,
    files: &[DraftFile],
    now: f64,
) -> rusqlite::Result<Vec<DraftFile>> {
    let mut added = Vec::new();
    for f in files {
        // The same file twice is one attachment, in the place it already
        // has: a second `attach` of an overlapping set is not two copies
        // in the envelope, and not a reordering either.
        let held: bool = c
            .query_row(
                "SELECT 1 FROM draft_attachment WHERE panel = ?1 AND path = ?2",
                rusqlite::params![panel, f.path],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if held {
            continue;
        }
        c.execute(
            "INSERT INTO draft_attachment(panel, path, name, size, added, device)
             VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![panel, f.path, f.name, f.size as i64, now, f.device],
        )?;
        added.push(f.clone());
    }
    Ok(added)
}

/// Takes them off again — undo's half of an attach.
pub fn detach_files_tx(
    c: &rusqlite::Connection,
    panel: i64,
    paths: &[String],
) -> rusqlite::Result<()> {
    for p in paths {
        c.execute(
            "DELETE FROM draft_attachment WHERE panel = ?1 AND path = ?2",
            rusqlite::params![panel, p],
        )?;
    }
    Ok(())
}

/// A draft's files go with the draft: a discard, and a send once it has
/// left. Called wherever `draft` rows are.
pub fn discard_draft_files_tx(c: &rusqlite::Connection, panel: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM draft_attachment WHERE panel = ?1", [panel])?;
    Ok(())
}

// -- local mutations ---------------------------------------------------------
//
// Transaction-level pieces, composed into undoable actions by the shell
// (the session records them; undo inverts them). Phase 4 makes the server
// agree via the op queue.

/// Marks a mail read (opening it does this). A no-change update touches no
/// row — so it records nothing, and undoing the open of an already-read
/// mail correctly leaves it read. This writes **intent** only; the sync
/// worker pushes wherever intent and `server_msg` disagree.
pub fn mark_read_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE message SET unread = 0 WHERE id = ?1 AND unread = 1",
        [id],
    )?;
    Ok(())
}

/// Moves a mail into one of its account's role folders. Intent only — the
/// push pass makes the server agree (see [`mark_read_tx`]) — and it is
/// generic over the move, so trash rides the same path archive already
/// proved.
///
/// The `EXISTS` guard is load-bearing: an account whose server advertises no
/// such folder (see [`crate::sync`]'s role detection) would otherwise get a
/// `NULL` from the subquery, and a mail with a null folder falls out of the
/// mailbox queries *and* out of the push set's join — vanishing silently,
/// with nothing to sync it back.
///
/// The second guard is the honest no-op: filing a mail into the folder it
/// already sits in changes nothing, so it must *say* it changed nothing —
/// otherwise archiving from the archive would record an undo node and push
/// the server a MOVE onto itself. Returns whether the mail actually moved,
/// so the caller can say so.
fn file_tx(c: &rusqlite::Connection, id: MailId, role: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE message SET folder =
           (SELECT f.id FROM folder f
            WHERE f.account = message.account AND f.role = ?2)
         WHERE id = ?1
           AND EXISTS (SELECT 1 FROM folder f
                       WHERE f.account = message.account AND f.role = ?2)
           AND folder IS NOT (SELECT f.id FROM folder f
                              WHERE f.account = message.account AND f.role = ?2)",
        rusqlite::params![id, role],
    )?;
    Ok(n > 0)
}

/// Archives a mail: it moves to its account's archive folder.
pub fn archive_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<bool> {
    file_tx(c, id, "archive")
}

/// Deletes a mail: it moves to its account's trash folder. Recoverable by
/// undo like any other action, and by the server's own trash after that.
pub fn delete_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<bool> {
    file_tx(c, id, "trash")
}

/// Whether this mail's account has the folder a triage would move it to.
/// The pre-flight for [`archive_tx`] / [`delete_tx`]: the shell asks first so
/// it can say *why* nothing happened rather than record an empty action.
pub fn can_file(store: &Store, id: MailId, role: &str) -> bool {
    store
        .conn()
        .query_row(
            "SELECT 1 FROM message m JOIN folder f ON f.account = m.account
             WHERE m.id = ?1 AND f.role = ?2",
            rusqlite::params![id, role],
            |_| Ok(true),
        )
        .unwrap_or(false)
}

/// Whether this mail is in that folder already — the triage's other
/// pre-flight, and the other thing worth saying instead of recording an
/// action that moves nothing. Reachable from a message panel's `archive`
/// on a mail read out of the archive: the button is about the mail, and
/// the mail is where it is.
#[must_use]
pub fn already_filed(store: &Store, id: MailId, role: &str) -> bool {
    store
        .conn()
        .query_row(
            "SELECT 1 FROM message m JOIN folder f ON f.id = m.folder
             WHERE m.id = ?1 AND f.role = ?2",
            rusqlite::params![id, role],
            |_| Ok(true),
        )
        .unwrap_or(false)
}

// -- recipients: what the compose panel's TO field completes -----------------

/// The compose panel's TO field as a completion — the rich table's box
/// (CR-006) over the mail world rather than the filter grammar. The token
/// under the caret, comma-separated from its neighbours, is matched as a
/// substring against every sender the store has heard from, by name or
/// address: the `@from:` offer, landing in a different field. A pick lands
/// the bare address, which is what a reply prefills and what the send
/// pipeline reads.
pub struct Recipients;

/// What the caret is in the middle of typing in a recipient list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientCtx {
    /// Where the token starts: after the last comma before the caret and
    /// the spaces that follow it.
    pub start: usize,
    /// The token as typed up to the caret, lowercased.
    pub partial: String,
    /// The addresses the other tokens already hold, lowercased — offered
    /// no second time.
    pub taken: Vec<String>,
}

impl Completion for Recipients {
    type Ctx = RecipientCtx;

    fn context(&self, text: &str, cursor: usize) -> Option<RecipientCtx> {
        recipient_context(text, cursor)
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

/// Classifies the caret in a recipient list: the token is what sits
/// between the last comma before the caret and the caret itself, less the
/// spaces after the comma. An empty token is `None` — typing is what opens
/// the offer, not landing in the field.
#[must_use]
pub fn recipient_context(text: &str, cursor: usize) -> Option<RecipientCtx> {
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
    // Every other token's address — the one under the caret is the piece
    // that starts where the token does.
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
fn address_of(token: &str) -> &str {
    match (token.rfind('<'), token.ends_with('>')) {
        (Some(i), true) => &token[i + 1..token.len() - 1],
        _ => token,
    }
}

// -- drafts and the send window ----------------------------------------------

/// A compose panel's persisted draft.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// The draft a fresh compose starts from, by its seed: a reply answers
/// its mail, a forward passes it on. Text the panel persisted wins over
/// this — the shell asks only when there is none.
#[must_use]
pub fn seed_draft(store: &Store, seed: Seed) -> Draft {
    match seed {
        Seed::Blank => Draft::default(),
        Seed::Reply(id) => mail(store, id).map_or_else(Draft::default, |m| Draft {
            to: m.head.from_email.clone(),
            subject: format!("Re: {}", topic_of(&m.head.subject)),
            body: quoted(&m),
        }),
        Seed::Forward(id) => mail(store, id).map_or_else(Draft::default, |m| Draft {
            to: String::new(),
            subject: format!("Fwd: {}", topic_of(&m.head.subject)),
            body: forwarded(&m),
        }),
    }
}

/// A reply's body: room to write at the top, then the letter it answers
/// under the attribution line every client writes — `On <date>, <who>
/// wrote:` — with each of its lines behind a `>`. That is the shape
/// [`split_quote`] folds away on the way back in, so what this app sends
/// is what it knows how to read.
///
/// The whole letter goes, its own quoted tail included: the chain is how
/// the conversation reaches someone who joins it late, and every reader
/// folds it.
#[must_use]
pub fn quoted(m: &MailFull) -> String {
    let quote = m
        .body
        .trim_end()
        .lines()
        // A `> ` before every line — and nothing but `>` before an empty
        // one, which is the shape a quote is written in everywhere.
        .map(|l| format!("> {l}").trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let head = format!(
        "\n\nOn {}, {} wrote:",
        fmt_date_long(m.head.date),
        writer(m)
    );
    // A letter with nothing to quote — an image alone — still says whose
    // it was, and leaves nothing hanging under the line.
    if quote.is_empty() {
        head
    } else {
        format!("{head}\n{quote}")
    }
}

/// A forward's body: room to write at the top, then the mail under the
/// header block every client recognises — who wrote it, about what, when,
/// to whom. The letter is the plain reading, which an HTML mail keeps for
/// exactly this.
#[must_use]
pub fn forwarded(m: &MailFull) -> String {
    format!(
        "\n\nBegin forwarded message:\n\nFrom: {}\nSubject: {}\nDate: {}\nTo: {}\n\n{}",
        writer(m),
        m.head.subject,
        fmt_date_long(m.head.date),
        m.to,
        m.body.trim_end()
    )
}

/// Who wrote a letter, as a reply's attribution and a forward's header
/// block name them: `Name <addr>`. A sender without a name is stored
/// under their address as the name (see [`crate::sync::parse_mail`]);
/// written out, that is the address once, not twice.
fn writer(m: &MailFull) -> String {
    let (name, email) = (&m.head.from_name, &m.head.from_email);
    if name.is_empty() || name == email {
        email.clone()
    } else {
        format!("{name} <{email}>")
    }
}

/// Loads a panel's draft, if any (boot restore, prefill).
pub fn draft(store: &Store, panel: i64) -> Option<Draft> {
    draft_row(store, panel).map(|(d, _)| d)
}

/// A panel's draft, if the row is `seed`'s own: what it answers and what
/// it passes on must match. A panel replaced in place keeps its id, so a
/// row a reply left is not the forward's draft — that one seeds afresh.
pub fn draft_for(store: &Store, panel: i64, seed: Seed) -> Option<Draft> {
    draft_row(store, panel)
        .filter(|(_, (re, fwd))| (*re, *fwd) == (seed.in_reply_to(), seed.forwards()))
        .map(|(d, _)| d)
}

/// What a draft row answers and what it passes on — the seed it was
/// saved under, as `(re_message, fwd_message)`.
type DraftSeed = (Option<MailId>, Option<MailId>);

/// The row: the text, and the seed it was saved under.
fn draft_row(store: &Store, panel: i64) -> Option<(Draft, DraftSeed)> {
    store
        .conn()
        .query_row(
            "SELECT to_addr, subject, body, re_message, fwd_message FROM draft WHERE panel=?1",
            [panel],
            |r| {
                Ok((
                    Draft {
                        to: r.get(0)?,
                        subject: r.get(1)?,
                        body: r.get(2)?,
                    },
                    (r.get(3)?, r.get(4)?),
                ))
            },
        )
        .ok()
}

/// Persists a compose panel's fields — plain typing upkeep, deliberately
/// **not** an action (text editing is the future editor's local undo).
/// The caller skips no-op saves; this just writes.
pub fn save_draft(store: &Store, panel: i64, seed: Seed, d: &Draft, now: f64) {
    let d = d.clone();
    let _ = store.write(move |c| upsert_draft_tx(c, panel, seed, &d, now));
}

/// The transaction-level draft upsert (also part of the send action, so
/// the recorded changeset carries the final content). The seed's mail is
/// recorded as what the draft answers or what it passes on — the send
/// reads its threading headers off either — and a row a panel already has
/// takes the seed along with the text: a compose retargeted in place is
/// a new draft under an old id.
pub fn upsert_draft_tx(
    c: &rusqlite::Connection,
    panel: i64,
    seed: Seed,
    d: &Draft,
    now: f64,
) -> rusqlite::Result<()> {
    let account: Option<i64> = seed
        .source()
        .and_then(|id| {
            c.query_row("SELECT account FROM message WHERE id=?1", [id], |r| r.get(0))
                .ok()
        })
        .or_else(|| {
            c.query_row(
                "SELECT id FROM account
                 WHERE COALESCE(smtp_host,'') != '' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok()
        })
        .or_else(|| c.query_row("SELECT id FROM account ORDER BY id LIMIT 1", [], |r| r.get(0)).ok());
    // A compose retargeted in place is a new draft under an old id, and
    // the files the old one named are not this one's to carry (CR-010) —
    // the same rule `draft_for` holds the text to, applied where the seed
    // actually changes.
    let was: Option<DraftSeed> = c
        .query_row(
            "SELECT re_message, fwd_message FROM draft WHERE panel = ?1",
            [panel],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    if was.is_some_and(|s| s != (seed.in_reply_to(), seed.forwards())) {
        discard_draft_files_tx(c, panel)?;
    }
    c.execute(
        "INSERT INTO draft(panel, account, re_message, fwd_message,
                           to_addr, subject, body, updated)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(panel) DO UPDATE SET
           account=excluded.account, re_message=excluded.re_message,
           fwd_message=excluded.fwd_message,
           to_addr=excluded.to_addr, subject=excluded.subject,
           body=excluded.body, updated=excluded.updated",
        rusqlite::params![
            panel,
            account,
            seed.in_reply_to(),
            seed.forwards(),
            d.to,
            d.subject,
            d.body,
            now
        ],
    )?;
    Ok(())
}

/// Files the outbox row for a send action — the row id is the panel id, so
/// the undo entity (`outbox:{panel}`) exists before the row does.
pub fn file_send_tx(
    c: &rusqlite::Connection,
    panel: i64,
    send_after: f64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO outbox(id, account, send_after, status, error)
         SELECT panel, COALESCE(account, 1), ?2, 'pending', NULL FROM draft WHERE panel=?1",
        rusqlite::params![panel, send_after],
    )?;
    // A send filed again — a retry from the problems panel, or a redo after
    // a failure — must not be failed on sight by the job that failed *last*
    // time: the outbox pass derives a row's failure from any failed submit
    // for it, so those stand down first.
    c.execute(
        "UPDATE effect SET status = 'obsolete'
         WHERE kind = 'submit' AND status = 'failed' AND payload ->> 'outbox' = ?1",
        [panel],
    )?;
    Ok(())
}

/// Reopens a failed send as a draft on panel `new`: the draft rows move
/// under the new panel's id (a compose reads its draft by its own id) and
/// the failed outbox row goes, so the problem is gone with it. Reversed by
/// [`Reopened`].
pub fn reopen_send_tx(
    c: &rusqlite::Connection,
    old: i64,
    new: i64,
    now: f64,
) -> rusqlite::Result<()> {
    move_draft_tx(c, old, new, now)?;
    c.execute(
        "DELETE FROM outbox WHERE id = ?1 AND status = 'failed'",
        [old],
    )?;
    Ok(())
}

/// Re-keys a draft from one panel to another, keeping its text and account.
fn move_draft_tx(
    c: &rusqlite::Connection,
    from: i64,
    to: i64,
    now: f64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO draft(panel, account, re_message, fwd_message,
                                      to_addr, subject, body, updated)
         SELECT ?2, account, re_message, fwd_message, to_addr, subject, body, ?3
           FROM draft WHERE panel = ?1",
        rusqlite::params![from, to, now],
    )?;
    c.execute("DELETE FROM draft WHERE panel = ?1", [from])?;
    // What it was going to carry moves with it (CR-010) — a reopened send
    // keeps its attachments, or the letter that goes out the second time is
    // not the letter that failed.
    c.execute(
        "UPDATE draft_attachment SET panel = ?2 WHERE panel = ?1",
        rusqlite::params![from, to],
    )?;
    Ok(())
}

/// Discard: the draft goes with the panel (both revert on undo).
pub fn discard_draft_tx(c: &rusqlite::Connection, panel: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM draft WHERE panel=?1", [panel])?;
    discard_draft_files_tx(c, panel)
}

/// Outbox rows that failed — surfaced as toasts and on settings.
pub fn outbox_failures(store: &Store) -> Vec<(i64, String)> {
    let Ok(mut stmt) = store
        .conn()
        .prepare("SELECT id, COALESCE(error,'send failed') FROM outbox WHERE status='failed' ORDER BY id")
    else {
        return Vec::new();
    };
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

// -- the mail domain's effects ------------------------------------------------
//
// Deferred, so each one is a row in the effect table: retryable, observable,
// and cancellable by undo while it is still `pending`. Every one of them
// revalidates before the round trip — if undo landed while the job waited,
// it goes `obsolete` rather than pushing stale intent at the server. That is
// what keeps CR-001 phase 4's property: undo costs no server traffic.

/// Make the server agree about which folder a mail lives in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Move {
    pub account: i64,
    pub message: i64,
    /// The folder row the mail now claims — `settle` records it as fact.
    pub to_folder: i64,
    pub from: String,
    pub to: String,
    pub uid: u32,
}

impl Effect for Move {
    const KIND: &'static str = "move";
    type Reply = Option<u32>;

    fn describe(&self) -> String {
        format!("move uid {} from {} to {}", self.uid, self.from, self.to)
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.move_uid(self.account, &self.from, &self.to, self.uid)
    }
}

impl Deferred for Move {
    /// Moving an already-moved uid fails harmlessly, and `still_wanted`
    /// catches the common case first.
    fn idempotent(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(format!("account:{}", self.account))
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        db.query_row(
            "SELECT 1 FROM message m JOIN server_msg s ON s.message = m.id
             WHERE m.id = ?1 AND m.folder = ?2 AND s.folder != ?2 AND s.uid = ?3",
            rusqlite::params![self.message, self.to_folder, self.uid],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn settle(&self, tx: &Transaction, reply: &Self::Reply) -> rusqlite::Result<()> {
        // No COPYUID means identity is lost until the next fetch adopts it
        // by Message-ID; the uid goes null rather than stale.
        tx.execute(
            "UPDATE server_msg SET folder = ?1, uid = ?2 WHERE message = ?3",
            rusqlite::params![self.to_folder, reply, self.message],
        )?;
        Ok(())
    }
}

/// Make the server agree about whether a mail has been read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seen {
    pub account: i64,
    pub message: i64,
    pub folder: String,
    pub uid: u32,
    pub seen: bool,
}

impl Effect for Seen {
    const KIND: &'static str = "seen";
    type Reply = ();

    fn describe(&self) -> String {
        format!(
            "mark uid {} in {} {}",
            self.uid,
            self.folder,
            if self.seen { "seen" } else { "unseen" }
        )
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out
            .store_flag(self.account, &self.folder, self.uid, MailFlag::Seen, self.seen)
    }
}

impl Deferred for Seen {
    fn idempotent(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(format!("account:{}", self.account))
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        db.query_row(
            "SELECT 1 FROM message m JOIN server_msg s ON s.message = m.id
             WHERE m.id = ?1 AND m.unread = ?2 AND s.seen != ?3",
            rusqlite::params![self.message, !self.seen, self.seen],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn settle(&self, tx: &Transaction, _reply: &()) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE server_msg SET seen = ?1 WHERE message = ?2",
            rusqlite::params![self.seen, self.message],
        )?;
        Ok(())
    }
}

/// Make the server agree that a mail was passed on — the `$Forwarded`
/// keyword, which is what every other client draws its arrow from. The
/// read flag's twin in every respect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forwarded {
    pub account: i64,
    pub message: i64,
    pub folder: String,
    pub uid: u32,
    pub on: bool,
}

impl Effect for Forwarded {
    const KIND: &'static str = "forwarded";
    type Reply = ();

    fn describe(&self) -> String {
        format!(
            "mark uid {} in {} {}",
            self.uid,
            self.folder,
            if self.on {
                "forwarded"
            } else {
                "not forwarded"
            }
        )
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.store_flag(
            self.account,
            &self.folder,
            self.uid,
            MailFlag::Forwarded,
            self.on,
        )
    }
}

impl Deferred for Forwarded {
    fn idempotent(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(format!("account:{}", self.account))
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        db.query_row(
            "SELECT 1 FROM message m JOIN server_msg s ON s.message = m.id
             WHERE m.id = ?1 AND m.forwarded = ?2 AND s.forwarded != ?2",
            rusqlite::params![self.message, self.on],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn settle(&self, tx: &Transaction, _reply: &()) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE server_msg SET forwarded = ?1 WHERE message = ?2",
            rusqlite::params![self.on, self.message],
        )?;
        Ok(())
    }
}

/// Hand a mail to the SMTP server, then file it into Sent. The one
/// genuinely irreversible effect in the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submit {
    /// The outbox row, which shares its compose panel's id. Referenced, not
    /// embedded: a payload that carried the body would go stale.
    pub outbox: i64,
}

impl Effect for Submit {
    const KIND: &'static str = "submit";
    /// `None` when the mail was also filed to Sent; `Some(why)` when it was
    /// sent but filing failed — best effort, exactly as before.
    type Reply = Option<String>;

    fn describe(&self) -> String {
        format!("submit outbox:{}", self.outbox)
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        let mut d = load_outgoing(cx.db, self.outbox)?;
        // The files the draft named are read *now*, through the outside,
        // rather than having been copied into the store when they were
        // attached (CR-010): what leaves is the file as it stands, and a
        // file that has since gone fails the send instead of sending a
        // stale copy of it.
        let here = crate::store::this_device(cx.db);
        for f in &d.files {
            // A path is a file on the machine it was picked on. These rows
            // replicate, so `~/Downloads/report-q3.pdf` over here is some
            // other file or none — refuse rather than carry out whatever
            // happens to sit there.
            if !f.device.is_empty() && !here.is_empty() && f.device != here {
                return Err(format!(
                    "“{}” was attached on another device — attach it again here",
                    f.name
                ));
            }
            // One byte past the cap is asked for, so a file that grew since
            // it was attached is *refused* rather than quietly truncated to
            // the limit and sent under its own name.
            let cap = crate::files::ATTACH_MAX as usize;
            let bytes = cx
                .out
                .read_file(&crate::files::real_path(&f.path), cap + 1)
                .map_err(|e| format!("cannot attach “{}”: {e}", f.name))?;
            if bytes.len() > cap {
                return Err(format!(
                    "“{}” is past {} now — attach it again or send it another way",
                    f.name,
                    crate::files::fmt_size(crate::files::ATTACH_MAX)
                ));
            }
            d.mail.attachments.push(crate::effect::Part {
                name: f.name.clone(),
                mime: crate::files::mime_of(&f.name).to_string(),
                bytes,
            });
        }
        let smtp = creds_for(cx.out, &d.email, &d.smtp, d.oauth)?;
        let raw = cx.out.submit(&smtp, &d.mail)?;
        // Gmail's SMTP files its own copy into Sent Mail, so appending one
        // would leave the human looking at the same letter twice. The
        // account's provider is what knows; a plain relay files nothing.
        if d.oauth && crate::oauth::GOOGLE.files_sent_itself {
            return Ok(None);
        }
        // The mail is gone; filing it is best effort and never fails a send.
        if d.imap.is_empty() {
            return Ok(Some("no imap host to file to Sent".into()));
        }
        // The same secret reaches both servers, so the token is not minted
        // twice — `Creds` is cheap, and the backend's cache is the point.
        let imap = Creds {
            host: d.imap,
            user: d.email,
            auth: smtp.auth,
        };
        let filed = cx
            .out
            .connect(d.account, &imap)
            .and_then(|()| cx.out.append(d.account, &d.sent, &raw));
        Ok(filed.err().map(|e| format!("sent; filing to Sent failed: {e}")))
    }
}

impl Deferred for Submit {
    /// Never. A second submission is a second mail, so a crash mid-send
    /// must ask a human rather than guess.
    fn idempotent(&self) -> bool {
        false
    }

    fn entity(&self) -> Option<String> {
        Some(format!("outbox:{}", self.outbox))
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        matches!(
            db.query_row(
                "SELECT status FROM outbox WHERE id = ?1",
                [self.outbox],
                |r| r.get::<_, String>(0)
            ),
            Ok(s) if s == "sending"
        )
    }

    fn settle(&self, tx: &Transaction, reply: &Self::Reply) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE outbox SET status = 'sent', error = ?2 WHERE id = ?1",
            rusqlite::params![self.outbox, reply],
        )?;
        // The mail a forward passed on is now forwarded — intent, which
        // the next push pass sets on the server as `$Forwarded`. Not an
        // action: it is a consequence of a send that has already left.
        tx.execute(
            "UPDATE message SET forwarded = 1
             WHERE id = (SELECT fwd_message FROM draft WHERE panel = ?1)",
            [self.outbox],
        )?;
        Ok(())
    }
}

/// Everything [`Submit`] needs, read from rows at execution time.
struct Outgo {
    account: i64,
    email: String,
    smtp: String,
    imap: String,
    sent: String,
    /// The account authenticates with a bearer token, not a password.
    oauth: bool,
    mail: Outgoing,
    /// What the draft named to carry out (CR-010) — paths; the bytes are
    /// read at the last moment, in [`Submit::perform`].
    files: Vec<DraftFile>,
}

fn load_outgoing(db: &Connection, outbox: i64) -> Result<Outgo, String> {
    let files: Vec<DraftFile> = db
        .prepare(
            "SELECT id, path, name, size, device FROM draft_attachment
             WHERE panel = ?1 ORDER BY id",
        )
        .and_then(|mut s| s.query_map([outbox], draft_file_row).and_then(|it| it.collect()))
        .map_err(|e| format!("outbox:{outbox} cannot read its attachments: {e}"))?;
    db.query_row(
        "SELECT o.account, a.email, COALESCE(a.smtp_host,''), COALESCE(a.imap_host,''),
                COALESCE((SELECT name FROM folder WHERE account=a.id AND role='sent'), 'Sent'),
                d.to_addr, d.subject, d.body,
                (SELECT message_id FROM message WHERE id = d.re_message),
                (SELECT message_id FROM message
                  WHERE id = COALESCE(d.re_message, d.fwd_message)),
                (SELECT GROUP_CONCAT(mid, ' ') FROM reference
                  WHERE message = COALESCE(d.re_message, d.fwd_message)),
                COALESCE(a.auth, '')
         FROM outbox o
         JOIN account a ON a.id = o.account
         JOIN draft d ON d.panel = o.id
         WHERE o.id = ?1",
        [outbox],
        |r| {
            // The chain: what the source itself referenced, then the
            // source — so a reply to a reply, or a forward of one, threads
            // for whoever already has the conversation (RFC 5322).
            let source: Option<String> = r.get::<_, Option<String>>(9)?.filter(|s| !s.is_empty());
            let mut references: Vec<String> = r
                .get::<_, Option<String>>(10)?
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            if let Some(mid) = source {
                if !references.contains(&mid) {
                    references.push(mid);
                }
            }
            Ok(Outgo {
                account: r.get(0)?,
                email: r.get(1)?,
                smtp: r.get(2)?,
                imap: r.get(3)?,
                sent: r.get(4)?,
                oauth: r.get::<_, String>(11)? == crate::oauth::GOOGLE.name,
                mail: Outgoing {
                    to: r.get(5)?,
                    subject: r.get(6)?,
                    body: r.get(7)?,
                    in_reply_to: r.get::<_, Option<String>>(8)?.filter(|s| !s.is_empty()),
                    references,
                    attachments: Vec::new(),
                },
                files: files.clone(),
            })
        },
    )
    .map_err(|e| format!("outbox:{outbox} is not sendable: {e}"))
    .and_then(|d| {
        if d.smtp.is_empty() {
            Err("account has no smtp host".to_string())
        } else {
            Ok(d)
        }
    })
}

/// The credentials one account's session opens with.
///
/// The account row's `auth` picks the mechanism, and the two secrets live
/// in different places for different lengths of time: an app password is
/// read straight out of the keychain, while a Gmail account's bearer token
/// is minted (or recalled from the process cache) by the backend. Both
/// sites that open a session — the sync worker and [`Submit`] — come
/// through here, so neither can drift.
///
/// # Errors
///
/// If the keychain has no password, or the OAuth grant is gone.
pub fn creds_for(
    out: &mut dyn crate::effect::Outside,
    email: &str,
    host: &str,
    oauth: bool,
) -> Result<Creds, String> {
    if oauth {
        Ok(Creds::bearer(host, email, out.access_token(email)?))
    } else {
        let pass = out
            .secret_get(email)
            .ok_or("no password in the keychain")?;
        Ok(Creds::password(host, email, pass))
    }
}

// -- the mail domain's intents ------------------------------------------------
//
// What an action claimed of the world, and how to give it back. In memory,
// on a history node, never serialized — what survives a restart is the row
// each one wrote.

/// Opening a mail marks it read.
pub struct MarkRead {
    pub mail: MailId,
}

impl Intent for MarkRead {
    fn describe(&self) -> String {
        format!("mail:{} read", self.mail)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let mail = self.mail;
        w.store()
            .write(move |c| {
                c.execute("UPDATE message SET unread = 1 WHERE id = ?1", [mail])
                    .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let mail = self.mail;
        w.store()
            .write(move |c| mark_read_tx(c, mail))
            .map_err(|e| e.to_string())
    }
}

/// Filing moves a mail out of the folder it was in — archive or trash, the
/// same move to a different role (CR-005). Reversing is a plain intent flip
/// — the push pass re-converges, so nothing compensates.
pub struct Filed {
    pub mail: MailId,
    /// Where it was, so undo can put it back exactly there.
    pub from_folder: i64,
    /// The role it went to: `archive` or `trash`.
    pub role: &'static str,
}

impl Intent for Filed {
    fn describe(&self) -> String {
        let verb = if self.role == "trash" { "deleted" } else { "archived" };
        format!("mail:{} {verb}", self.mail)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (mail, from_folder) = (self.mail, self.from_folder);
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE message SET folder = ?1 WHERE id = ?2",
                    rusqlite::params![from_folder, mail],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (mail, role) = (self.mail, self.role);
        w.store()
            .write(move |c| file_tx(c, mail, role).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}

/// A send, claimable back only until the sender takes it.
pub struct Sent {
    pub panel: i64,
    /// How long the window is, so redo files a fresh one.
    pub delay: f64,
}

impl Intent for Sent {
    fn describe(&self) -> String {
        format!("outbox:{} filed", self.panel)
    }

    /// The status guard is the whole race: `pending` means the executor has
    /// not taken it, and undo wins. Anything else means the mail is gone.
    fn blocked(&self, w: &World) -> Option<String> {
        match w.store().conn().query_row(
            "SELECT status FROM outbox WHERE id = ?1",
            [self.panel],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) if s == "pending" => None,
            Ok(s) if s == "failed" => None, // never left; still cancellable
            Ok(_) => Some("already sent".into()),
            Err(_) => None, // no row: nothing to give back, nothing to block
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let panel = self.panel;
        w.store()
            .write(move |c| {
                c.execute(
                    "DELETE FROM outbox WHERE id = ?1 AND status IN ('pending','failed')",
                    [panel],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (panel, after) = (self.panel, w.now() + self.delay);
        w.store()
            .write(move |c| file_send_tx(c, panel, after))
            .map_err(|e| e.to_string())
    }
}

/// A failed send reopened as a draft (the problems panel's *reopen*): the
/// draft moved from the outbox's id to a fresh compose panel, and the failed
/// row went. Giving it back moves the draft home and restores the row with
/// the error it carried.
pub struct Reopened {
    /// The failed outbox row — and the draft's old panel id.
    pub old: i64,
    /// The compose panel it reopened on. Minted while the action's layout
    /// change ran, so it is read rather than carried.
    pub new: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The failure the row carried, put back with it.
    pub error: String,
}

impl Reopened {
    fn new_id(&self) -> i64 {
        self.new.load(std::sync::atomic::Ordering::Relaxed) as i64
    }
}

impl Intent for Reopened {
    fn describe(&self) -> String {
        format!("outbox:{} reopened", self.old)
    }

    /// Once the reopened draft has *gone out* from its new panel, there is
    /// no failed send to put back — the walk steps past this node.
    fn blocked(&self, w: &World) -> Option<String> {
        match w.store().conn().query_row(
            "SELECT status FROM outbox WHERE id = ?1",
            [self.new_id()],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) if s == "pending" || s == "failed" => None,
            Ok(_) => Some("already sent".into()),
            Err(_) => None,
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (old, new, error, now) = (self.old, self.new_id(), self.error.clone(), w.now());
        w.store()
            .write(move |c| {
                move_draft_tx(c, new, old, now)?;
                c.execute(
                    "INSERT OR REPLACE INTO outbox(id, account, send_after, status, error)
                     SELECT panel, COALESCE(account, 1), 0, 'failed', ?2 FROM draft WHERE panel = ?1",
                    rusqlite::params![old, error],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (old, new, now) = (self.old, self.new_id(), w.now());
        w.store()
            .write(move |c| reopen_send_tx(c, old, new, now))
            .map_err(|e| e.to_string())
    }
}

/// A failed send filed again (the problems panel's *retry*): the row went
/// back to `pending` with a fresh window, and the submit job that failed
/// last time stood down. Giving it back puts the failure back — the row
/// and the job — so the draft stays reachable through the problems panel
/// rather than stranded behind a compose that no snapshot reopens.
pub struct Retried {
    pub outbox: i64,
    /// The failure the row carried, put back with it.
    pub error: String,
    /// How long the window is, so redo files a fresh one.
    pub delay: f64,
}

impl Intent for Retried {
    fn describe(&self) -> String {
        format!("outbox:{} retried", self.outbox)
    }

    /// As a send's: once the executor has taken the row, the mail is gone.
    fn blocked(&self, w: &World) -> Option<String> {
        match w.store().conn().query_row(
            "SELECT status FROM outbox WHERE id = ?1",
            [self.outbox],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) if s == "pending" || s == "failed" => None,
            Ok(_) => Some("already sent".into()),
            Err(_) => None,
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (outbox, error) = (self.outbox, self.error.clone());
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE outbox SET status = 'failed', error = ?2
                     WHERE id = ?1 AND status IN ('pending', 'failed')",
                    rusqlite::params![outbox, error],
                )?;
                // The job the retry stood down stands again, so the row
                // reads as it did: the attempts it took, the error it gave.
                c.execute(
                    "UPDATE effect SET status = 'failed'
                     WHERE id = (SELECT MAX(id) FROM effect
                                 WHERE kind = 'submit' AND status = 'obsolete'
                                   AND payload ->> 'outbox' = ?1)",
                    [outbox],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (outbox, after) = (self.outbox, w.now() + self.delay);
        w.store()
            .write(move |c| file_send_tx(c, outbox, after))
            .map_err(|e| e.to_string())
    }
}

/// Attaching files to a draft (CR-010). The draft holds paths, so giving it
/// back is taking those paths off again — matched by path rather than by
/// row id, which a redo mints afresh.
pub struct Attached {
    /// What the action **added** — never a path the draft already carried,
    /// which was not this action's to take away.
    pub files: Vec<DraftFile>,
    pub panel: i64,
}

impl Intent for Attached {
    fn describe(&self) -> String {
        format!("panel:{} carries {}", self.panel, crate::files::plural(self.files.len()))
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (panel, paths) = (
            self.panel,
            self.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        );
        w.store()
            .write(move |c| detach_files_tx(c, panel, &paths))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (panel, files, now) = (self.panel, self.files.clone(), w.now());
        w.store()
            .write(move |c| attach_files_tx(c, panel, &files, now).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}

/// Discarding a compose takes its text with it — and what it was going to
/// carry, which undo has to put back too.
pub struct Discarded {
    pub panel: i64,
    pub draft: Draft,
    pub seed: Seed,
    pub files: Vec<DraftFile>,
}

impl Intent for Discarded {
    fn describe(&self) -> String {
        format!("panel:{} draft discarded", self.panel)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (now, panel, seed, draft) = (w.now(), self.panel, self.seed, self.draft.clone());
        let files = self.files.clone();
        w.store()
            .write(move |c| {
                upsert_draft_tx(c, panel, seed, &draft, now)?;
                attach_files_tx(c, panel, &files, now).map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let panel = self.panel;
        w.store()
            .write(move |c| discard_draft_tx(c, panel))
            .map_err(|e| e.to_string())
    }
}

/// Adding an account. Reversible while it is still empty, which it is at
/// the moment it is added.
pub struct AccountAdded {
    pub id: i64,
    pub email: String,
    pub imap: String,
    pub smtp: String,
    /// The `account.auth` word, so a redo restores a Gmail account as a
    /// Gmail account rather than as one asking for a password.
    pub auth: String,
}

impl Intent for AccountAdded {
    fn describe(&self) -> String {
        format!("account:{} added", self.id)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (id, now) = (self.id, w.now());
        w.store()
            .write(move |c| remove_account_tx(c, id, now))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (id, email, imap, smtp, auth) = (
            self.id,
            self.email.clone(),
            self.imap.clone(),
            self.smtp.clone(),
            self.auth.clone(),
        );
        w.store()
            .write(move |c| {
                c.execute(
                    "INSERT INTO account(id, label, email, imap_host, smtp_host, auth)
                     VALUES(?1, ?2, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, email, imap, smtp, auth],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
}

/// Removing an account takes its mail with it, and no snapshot brings that
/// back. Stated honestly rather than half-restored: the node goes expired
/// and the walk steps past it.
pub struct AccountRemoved {
    pub email: String,
}

impl Intent for AccountRemoved {
    fn describe(&self) -> String {
        format!("account {} removed", self.email)
    }
    fn blocked(&self, _w: &World) -> Option<String> {
        Some("an account's mail cannot be restored".into())
    }
    fn reverse(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }
    fn reapply(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }
}

// -- the in-memory half -------------------------------------------------------
//
// Reads and session handling: nobody retries them, nobody waits on a row for
// them, and their answers are values the caller needs now. So they are
// `Effect` but not `Deferred` — performed at the call, written nowhere.

/// Open this account's mail session. Not `Deferred`, and therefore not
/// serializable — which is the point: it carries a password.
#[derive(Debug, Clone)]
pub struct Connect {
    pub account: i64,
    pub creds: Creds,
}

impl Effect for Connect {
    const KIND: &'static str = "connect";
    type Reply = ();
    fn describe(&self) -> String {
        format!("connect to {} as {}", self.creds.host, self.creds.user)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.connect(self.account, &self.creds)
    }
}

/// List the account's folders.
#[derive(Debug, Clone)]
pub struct Folders {
    pub account: i64,
}

impl Effect for Folders {
    const KIND: &'static str = "folders";
    type Reply = Vec<crate::effect::RemoteFolder>;
    fn describe(&self) -> String {
        "list folders".into()
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.folders(self.account)
    }
}

/// SELECT a folder for its uidvalidity/uidnext.
#[derive(Debug, Clone)]
pub struct Meta {
    pub account: i64,
    pub folder: String,
}

impl Effect for Meta {
    const KIND: &'static str = "meta";
    type Reply = crate::effect::FolderMeta;
    fn describe(&self) -> String {
        format!("select {}", self.folder)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.folder_meta(self.account, &self.folder)
    }
}

/// Fetch everything at or above a uid.
#[derive(Debug, Clone)]
pub struct Fetch {
    pub account: i64,
    pub folder: String,
    pub from: u32,
}

impl Effect for Fetch {
    const KIND: &'static str = "fetch";
    type Reply = Vec<crate::effect::RemoteMail>;
    fn describe(&self) -> String {
        format!("fetch {} from uid {}", self.folder, self.from)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.fetch(self.account, &self.folder, self.from)
    }
}

/// Search a folder's uids — all of them, the unseen, or the forwarded.
#[derive(Debug, Clone)]
pub struct Uids {
    pub account: i64,
    pub folder: String,
    pub which: UidSet,
}

impl Effect for Uids {
    const KIND: &'static str = "uids";
    type Reply = std::collections::HashSet<u32>;
    fn describe(&self) -> String {
        let which = match self.which {
            UidSet::All => "all",
            UidSet::Unseen => "unseen",
            UidSet::Forwarded => "forwarded",
        };
        format!("search {which} in {}", self.folder)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.uids(self.account, &self.folder, self.which)
    }
}

/// The mail domain's deferred effects. Each domain registers its own, so
/// adding one touches no central list.
pub fn register(reg: &mut Registry) {
    reg.register::<Move>();
    reg.register::<Seen>();
    reg.register::<Forwarded>();
    reg.register::<Submit>();
}

/// A registry with every domain's effects — what a real world and a test
/// world both start from.
#[must_use]
pub fn registry() -> Registry {
    let mut reg = Registry::new();
    register(&mut reg);
    reg
}

// -- dates -------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Days from civil date (Howard Hinnant's algorithm), epoch 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

/// Civil date from days since the epoch (the inverse of the above).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A timestamp from a civil date-time (naïve — the store's demo timezone).
#[must_use]
pub fn ts(y: i64, mo: u32, d: u32, h: u32, min: u32) -> f64 {
    (days_from_civil(y, mo, d) * 86_400 + i64::from(h) * 3_600 + i64::from(min) * 60) as f64
}

/// The list/date style the panels always used: `aug 31 09:14`.
#[must_use]
pub fn fmt_date(ts: f64) -> String {
    let secs = ts as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (_, m, d) = civil_from_days(days);
    let (h, min) = (rem / 3_600, (rem % 3_600) / 60);
    format!("{} {d:02} {h:02}:{min:02}", MONTHS[(m - 1) as usize])
}

/// The date written out for a letter's own text — a forwarded header —
/// with the year the list style leaves off: `31 Aug 2026 at 09:14`.
#[must_use]
pub fn fmt_date_long(ts: f64) -> String {
    let secs = ts as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let (h, min) = (rem / 3_600, (rem % 3_600) / 60);
    let mut mon = MONTHS[(m - 1) as usize].to_string();
    mon[..1].make_ascii_uppercase();
    format!("{d} {mon} {y} at {h:02}:{min:02}")
}

// -- the demo seed -----------------------------------------------------------

struct SeedMail<'a> {
    from_name: &'a str,
    from_email: &'a str,
    subject: &'a str,
    date: f64,
    unread: bool,
    body: &'a str,
    /// The HTML reading, when the demo sender sent one. Stored raw here
    /// and narrowed on the way in, exactly as a synced mail would be — the
    /// seed exercises the real path rather than a tidied version of it.
    html: Option<&'a str>,
    status: Option<(&'a str, bool)>,
    /// The folder's role: `inbox`, `archive`, `sent` or `spam`.
    folder: &'a str,
    /// Message-ID and what it references — the threading headers (CR-007);
    /// empty for a mail that stands alone.
    mid: &'a str,
    refs: &'a [&'a str],
}

/// The demo letters that carry something (CR-010), by subject. A seeded
/// mail normally has no `raw` at all; these two get one — built as a real
/// `multipart/mixed` and walked by the ingest path's own parser — so the
/// message row, the card and the browser meet real MIME rather than a
/// fixture. One is text, so the card previews it; one is not, so `open` has
/// something to hand to the OS.
type SeedPart = (&'static str, &'static [u8]);
const SEED_PARTS: &[(&str, &[SeedPart])] = &[
    (
        "Q3 infra budget draft",
        &[("q3-budget.csv", CSV.as_bytes())],
    ),
    (
        "invoice 2026-08 — €46.20",
        &[("invoice-2026-08.pdf", PDF)],
    ),
];

const CSV: &str = "line,aug,sep,delta\n\
                   staging cluster,1840,0,-1840\n\
                   ci runners,320,910,+590\n\
                   object store,210,224,+14\n\
                   egress,640,?,?\n\
                   ,,,\n\
                   total,3010,1134+egress,\n";

/// The smallest thing that is honestly a PDF: one empty page. Enough for
/// the card to say `pdf`, and for `open` to hand the OS something a viewer
/// will actually show.
const PDF: &[u8] = b"%PDF-1.4\n\
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]>>endobj\n\
trailer<</Root 1 0 R>>\n%%EOF\n";

/// One seeded letter as the bytes it would have arrived as: the plain text
/// it already has, then each part, base64'd. No `Message-ID` and no
/// `References` — the seed carries those itself, and a raw that disagreed
/// with the row would re-thread the mail somewhere else on the next
/// back-fill.
fn seed_raw(m: &SeedMail<'_>, parts: &[SeedPart]) -> Vec<u8> {
    let mut raw = format!(
        "From: {} <{}>\r\nTo: me@prepor.dev\r\nSubject: {}\r\n\
         MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"seed\"\r\n\r\n\
         --seed\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
        m.from_name, m.from_email, m.subject, m.body
    );
    for (name, bytes) in parts {
        raw += &format!(
            "--seed\r\nContent-Type: {}\r\n\
             Content-Disposition: attachment; filename=\"{name}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            crate::files::mime_of(name),
            crate::html::base64_encode(bytes)
        );
    }
    raw += "--seed--\r\n";
    raw.into_bytes()
}

/// The hand-written demo mail, newest first — ids land as 1..=9 in a fresh
/// store, which the tests and e2e suites rely on.
fn base_mails() -> Vec<SeedMail<'static>> {
    vec![
        SeedMail {
            from_name: "Vera Kovac",
            from_email: "vera@kovac.io",
            subject: "Q3 infra budget draft",
            date: ts(2026, 8, 31, 9, 14),
            unread: true,
            body: "Draft for Q3 infra spend is ready. Main deltas: the old staging cluster goes away and CI runners move to the new box.\n\nCan you sanity-check the numbers before Thursday? Especially egress — I suspect the CDN line is stale.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
        SeedMail {
            from_name: "GitHub",
            from_email: "notifications@github.com",
            subject: "[stelaxis] CI failed on main",
            date: ts(2026, 8, 31, 8, 2),
            unread: true,
            body: "Workflow main #4128 failed on push 9f3c2a1.\n\nFailed steps: mix test (2 failures), credo --strict (1 warning). Full logs are attached to the run.",
            // The one demo sender that writes HTML — and it writes it the
            // way real senders do: a stylesheet, tables holding the page
            // together, a pixel counting the open, a `javascript:` link.
            // What survives the narrowing is the letter.
            html: Some(
                "<html><head><style>.hd{background:#24292f;color:#fff}</style></head>\
                 <body><table width=\"100%\"><tr><td class=\"hd\">\
                 <div><b>Workflow failed</b> in \
                 <a href=\"https://github.com/x/stelaxis\">stelaxis</a></div>\
                 </td></tr><tr><td>\
                 <p>Run <b>main #4128</b> failed on push <code>9f3c2a1</code>.</p>\
                 <p>Failed steps: &#55357;&#56960;</p>\
                 <p><img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAGAAAAAUCAIAAAD9Sa+4AAAAOklEQVR42u3XMQ0AAAgDQfybBgFMEBaSewmXLo2QDkq1AM2BbBYQIECAAAECBAgQIECAnFVAf4CkZQX8qiSFOZw4FwAAAABJRU5ErkJggg==\" \
                 alt=\"the build badge\" width=\"96\" height=\"20\"></p>\
                 <ul><li>mix test &mdash; <b>2 failures</b></li>\
                 <li>credo --strict &mdash; <i>1 warning</i></li></ul>\
                 <p><i>This run was triggered by a push to </i><b><i>main</i></b>.</p>\
                 <p><a href=\"https://github.com/x/stelaxis/actions/runs/4128\">View the run</a> \
                 or <a href=\"javascript:unsub()\">unsubscribe</a>.</p>\
                 </td></tr></table>\
                 <img src=\"https://github.com/pixel.gif\" width=\"1\" height=\"1\">\
                 </body></html>",
            ),
            status: Some(("ci: FAILED — build (2m 14s), tests (41s)", true)),
            folder: "inbox",
            mid: "ci-4128@github.com",
            refs: &["stelaxis-ci@github.com"],
        },
        SeedMail {
            from_name: "Max Ivanov",
            from_email: "max@ivanov.dev",
            subject: "Re: superapp panel model",
            date: ts(2026, 8, 30, 22, 47),
            unread: false,
            body: "Read your note on panels. The joined/replace rule feels like the right default — it is the preview-pane pattern, but generalized to everything.\n\nOne question though: what happens to a half-written draft if a joined compose panel gets replaced by the next link? Feels like some panels need a way to resist replacement.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "pm-1@ivanov.dev",
            refs: &["pm-0@prepor.dev"],
        },
        SeedMail {
            from_name: "Elena Petrova",
            from_email: "elena.p@gmail.com",
            subject: "Sat hike — early start?",
            date: ts(2026, 8, 30, 18, 20),
            unread: false,
            body: "Weather looks fine for Saturday. Early start (7:30) or lazy start (10:00)?\n\nThere is a new trail variant, ~14 km, one café stop. Bring the good thermos.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
        SeedMail {
            from_name: "RSS Digest",
            from_email: "digest@rss.local",
            subject: "weekly: 14 unread items in 3 feeds",
            date: ts(2026, 8, 30, 7, 0),
            unread: false,
            body: "Unread this week: niri release notes (2), simonwillison.net (9), lobste.rs top (3).\n\nThis digest is itself a candidate for an rss/feed panel, by the way.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
        SeedMail {
            from_name: "Calendar",
            from_email: "calendar@local",
            subject: "invite: dentist — tue 10:00",
            date: ts(2026, 8, 29, 16, 41),
            unread: false,
            body: "Dentist, Tuesday 10:00–10:45. Reminder set for 30 minutes before.\n\nReply yes to confirm, or propose a new time.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
        SeedMail {
            from_name: "Hetzner",
            from_email: "billing@hetzner.com",
            subject: "invoice 2026-08 — €46.20",
            date: ts(2026, 8, 29, 11, 5),
            unread: false,
            body: "Invoice 2026-08 for €46.20 is available. Auto-charge on Sep 3.\n\nUsage: 2× CX32, 1× volume 100 GB, egress 214 GB.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
        SeedMail {
            from_name: "Dmitry Orlov",
            from_email: "dorlov@fastmail.com",
            subject: "that airport book",
            date: ts(2026, 8, 28, 20, 33),
            unread: false,
            body: "Found it — the airport design book you mentioned at dinner. Ordering a copy tomorrow.\n\nBorrowing rights claimed for after you finish, obviously.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
        // The one long letter in the demo world: it does not fit a message
        // panel's three rows, so the panel asks for more and opens tall.
        SeedMail {
            from_name: "Max Ivanov",
            from_email: "max@ivanov.dev",
            subject: "long version: what panels owe their content",
            date: ts(2026, 8, 28, 9, 12),
            unread: false,
            body: "You asked for the long version, so here it is — the argument I could not fit into two lines yesterday.\n\n\
                   What bothers me about every mail client I have used is that the reading pane is a fixed hole in the layout. A two-line \"ok, see you Thursday\" and a four-page release note are poured into the same box: one leaves most of it empty, the other is cut off a third of the way down. The box was sized for neither.\n\n\
                   Your panels already know better. A panel asks for grid units — a request, rather than a rectangle handed down. But the request is a constant per kind, which makes it a guess about the average letter, and the average letter does not exist.\n\n\
                   So let the kind's request be a floor rather than a promise. A short mail keeps its three rows — no reason to make a one-liner tall. A long one asks for as many rows as it needs, up to the whole column, and the grid clamps it there like it clamps everything else. Nothing new in the layout, just a better number going in.\n\n\
                   The nice consequence is that the layout tells you something before you read a word: a column where one panel is visibly taller is a column where one letter is long. That is real information, and you got it for free.\n\n\
                   Anyway — this letter is its own test case. If it opens in three rows you have proven my point; if it opens tall, yours.",
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
    ]
}

/// The demo world's conversations (CR-007), appended after the filler so
/// the first nine ids stay what every suite expects: the note Max was
/// replying to (mine, in Sent) and his second reply, which folds into his
/// first one's inbox row.
fn thread_mails() -> Vec<SeedMail<'static>> {
    vec![
        SeedMail {
            from_name: "Andrey Rudenko",
            from_email: "me@prepor.dev",
            subject: "superapp panel model",
            date: ts(2026, 8, 29, 14, 2),
            unread: false,
            body: "Wrote up the panel model: joined panels, replace in place, the chain closing behind a replacement. Curious what you make of the join rule in particular.\n\nThe draft is in the shared folder — comments welcome before Monday.",
            html: None,
            status: None,
            folder: "sent",
            mid: "pm-0@prepor.dev",
            refs: &[],
        },
        SeedMail {
            from_name: "Max Ivanov",
            from_email: "max@ivanov.dev",
            subject: "Re: superapp panel model",
            date: ts(2026, 8, 31, 7, 30),
            unread: false,
            body: "One more thought after sleeping on it: the join rule is also what keeps a preview honest. The panel beside the list is always the list's, never a stray.\n\nOn Sun, 30 Aug 2026 at 22:47, Max Ivanov wrote:\n> Read your note on panels. The joined/replace rule feels like the right default — it is the preview-pane pattern, but generalized to everything.\n>\n> One question though: what happens to a half-written draft if a joined compose panel gets replaced by the next link?",
            html: None,
            status: None,
            folder: "inbox",
            mid: "pm-2@ivanov.dev",
            refs: &["pm-0@prepor.dev", "pm-1@ivanov.dev"],
        },
    ]
}

/// What the spam folder holds — the demo world's junk, so the panel that
/// shows it has something to show and the filter something to sift. None of
/// it threads with anything: that is rather the point of it.
fn spam_mails() -> Vec<SeedMail<'static>> {
    vec![
        SeedMail {
            from_name: "Crypto Rewards",
            from_email: "no-reply@crypt0-rewards.biz",
            subject: "Your 4.2 BTC withdrawal is PENDING — confirm in 24h",
            date: ts(2026, 8, 30, 3, 41),
            unread: true,
            body: "Dear valued member,\n\nOur system shows an unclaimed balance of 4.2 BTC on your account. Confirm your wallet within 24 hours or the funds return to the pool.\n\nThis message was sent to you because you are a winner.",
            html: None,
            status: None,
            folder: "spam",
            mid: "",
            refs: &[],
        },
        SeedMail {
            from_name: "IT Helpdesk",
            from_email: "security@acount-verify.info",
            subject: "Mailbox quota exceeded — re-validate your password",
            date: ts(2026, 8, 29, 22, 8),
            unread: true,
            body: "Your mailbox has reached 99.8% of its quota and outgoing mail will be blocked.\n\nRe-validate your credentials on the portal below to restore full service. Failure to act will result in permanent deactivation.",
            html: None,
            status: None,
            folder: "spam",
            mid: "",
            refs: &[],
        },
        SeedMail {
            from_name: "Conference Board",
            from_email: "invites@global-summits.co",
            subject: "Invitation: keynote speaker, 14th Global Innovation Summit",
            date: ts(2026, 8, 26, 11, 20),
            unread: false,
            body: "Distinguished Professor,\n\nFollowing your remarkable contributions, the organising committee invites you to deliver a keynote at our summit in Dubai.\n\nRegistration fee of $1,890 applies to all speakers.",
            html: None,
            status: None,
            folder: "spam",
            mid: "",
            refs: &[],
        },
    ]
}

/// The five earlier runs of the CI workflow the GitHub mail continues —
/// `(run, day, hour, minute, failed)` — archived, so the inbox rows stay
/// where they were; the thread they make is six long. None of them names
/// the GitHub mail: every run references the same issue mail that never
/// arrived, which is the third threading lookup's case. Two failed; the
/// oldest carries the red status line, so a collapsed row has one to show.
const CI_RUNS: [(u32, u32, u32, u32, bool); 5] = [
    (4116, 28, 6, 10, true),
    (4119, 28, 18, 30, false),
    (4121, 29, 8, 45, false),
    (4124, 30, 7, 15, true),
    (4126, 30, 21, 0, false),
];

/// Seeds the demo account and mail into an empty store — the same content
/// the static module used to hold, so panels and e2e suites behave
/// identically. A store with any mail is left alone.
pub fn seed_if_empty(store: &Store) -> rusqlite::Result<()> {
    let n: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    store.write(|c| {
        c.execute(
            "INSERT INTO account(label, email) VALUES('demo', 'me@prepor.dev')",
            [],
        )?;
        let acct = c.last_insert_rowid();
        let folder = |name: &str, role: &str| -> rusqlite::Result<i64> {
            c.execute(
                "INSERT INTO folder(account, name, role) VALUES(?1, ?2, ?3)",
                rusqlite::params![acct, name, role],
            )?;
            Ok(c.last_insert_rowid())
        };
        let inbox = folder("Inbox", "inbox")?;
        let archive = folder("Archive", "archive")?;
        let sent = folder("Sent", "sent")?;
        let spam = folder("Spam", "spam")?;
        folder("Trash", "trash")?;
        let folder_of = |role: &str| match role {
            "archive" => archive,
            "sent" => sent,
            "spam" => spam,
            _ => inbox,
        };

        // Every row goes through here, so every row is threaded and carries
        // its topic — the seed walks the ingest path, not a tidier one.
        let insert = |m: &SeedMail<'_>| -> rusqlite::Result<i64> {
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email,
                                     subject, date, unread, body, html,
                                     status, status_err, message_id, topic)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                rusqlite::params![
                    acct,
                    folder_of(m.folder),
                    m.from_name,
                    m.from_email,
                    m.subject,
                    m.date,
                    m.unread,
                    m.body,
                    m.html.map(crate::html::sanitize),
                    m.status.map(|(s, _)| s),
                    m.status.map(|(_, e)| e).unwrap_or(false),
                    (!m.mid.is_empty()).then_some(m.mid),
                    topic_of(m.subject),
                ],
            )?;
            let id = c.last_insert_rowid();
            let refs: Vec<String> = m.refs.iter().map(|r| (*r).to_string()).collect();
            thread_tx(c, acct, id, m.mid, &refs)?;
            Ok(id)
        };
        for m in &base_mails() {
            let id = insert(m)?;
            // The two that carry something (CR-010): the letter they would
            // have arrived as goes into `raw`, and the parts come back out
            // of it through the ingest path's own walk — so the demo world
            // proves the real one rather than standing in for it.
            let Some((_, parts)) = SEED_PARTS.iter().find(|(s, _)| *s == m.subject) else {
                continue;
            };
            let raw = seed_raw(m, parts);
            c.execute(
                "UPDATE message SET raw = ?2 WHERE id = ?1",
                rusqlite::params![id, raw],
            )?;
            attach_tx(c, id, &crate::sync::parse_mail(&raw).attachments)?;
        }
        // One mail already passed on, so the mark has somewhere to show.
        c.execute(
            "UPDATE message SET forwarded = 1 WHERE subject LIKE 'invoice 2026-08%'",
            [],
        )?;
        // The generated archive tail: the inbox genuinely overflows, so
        // in-panel scrolling has something to do.
        let senders: [(&str, &str); 4] = [
            ("RSS Digest", "digest@rss.local"),
            ("GitHub", "notifications@github.com"),
            ("Hetzner", "billing@hetzner.com"),
            ("Calendar", "calendar@local"),
        ];
        for i in 0..60u32 {
            let (name, email) = senders[(i as usize) % senders.len()];
            let n = 60 - i;
            let subject = format!("archive digest #{n:02}");
            let body = format!(
                "Archive item #{n:02} from {name} — generated filler so the inbox overflows and in-panel scrolling is honest.\n\nNothing to see here beyond the scrollbar."
            );
            insert(&SeedMail {
                from_name: name,
                from_email: email,
                subject: &subject,
                date: ts(2026, 8, 27 - i / 6, 8 + (i % 12), (i * 7) % 60),
                unread: false,
                body: &body,
                html: None,
                status: None,
                folder: "inbox",
                mid: "",
                refs: &[],
            })?;
        }
        for m in &thread_mails() {
            insert(m)?;
        }
        for m in &spam_mails() {
            insert(m)?;
        }
        for (run, day, hour, minute, failed) in CI_RUNS {
            let mid = format!("ci-{run}@github.com");
            let outcome = if failed { "failed" } else { "passed" };
            let subject = format!("[stelaxis] CI {outcome} on main");
            let body = format!(
                "Workflow main #{run} {outcome} on push {:07x}.\n\nFull logs are attached to the run.",
                run.wrapping_mul(2_654_435)
            );
            insert(&SeedMail {
                from_name: "GitHub",
                from_email: "notifications@github.com",
                subject: &subject,
                date: ts(2026, 8, day, hour, minute),
                unread: false,
                body: &body,
                html: None,
                status: (failed && run == 4116).then_some(("ci: FAILED — tests (1m 02s)", true)),
                folder: "archive",
                mid: &mid,
                refs: &["stelaxis-ci@github.com"],
            })?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::richtable::Marks;

    fn store() -> Store {
        let s = Store::open(None).expect("in-memory store");
        seed_if_empty(&s).expect("seed");
        s
    }

    /// The demo world's two letters that carry something (CR-010): the
    /// rows come off a real `multipart/mixed` raw, the bytes come back out
    /// of it, and the card the panel draws is the file browser's own.
    #[test]
    fn the_seeded_letters_carry_their_parts() {
        let s = store();
        let of = |subject: &str| -> Rc<Vec<Attachment>> {
            let id = corpus(&s)
                .iter()
                .find(|m| m.subject == subject)
                .map(|m| m.id)
                .expect("the seeded letter");
            attachments(&s, id)
        };
        let budget = of("Q3 infra budget draft");
        assert_eq!(budget.len(), 1);
        assert_eq!(budget[0].name, "q3-budget.csv");
        assert_eq!(budget[0].mime, "text/csv");
        assert_eq!(budget[0].size, CSV.len() as u64);
        assert_eq!(budget[0].label(), "q3-budget.csv · 140 B");
        // The bytes are read back out of the letter, not kept beside it.
        assert_eq!(part(&s, &budget[0]).as_deref(), Some(CSV.as_bytes()));
        // The card is the browser's card: a kind word off the name, the
        // size, the letter it came with, and the media type under it.
        let card = budget[0].card("Vera Kovac", ts(2026, 8, 31, 9, 14));
        assert_eq!(card.kind, crate::files::FileKind::Text);
        assert_eq!(card.kind_line(), "text · 140 B");
        assert_eq!(card.when, "with Vera Kovac, aug 31 09:14");
        assert_eq!(card.detail, "text/csv");
        // …so the shared preview reads it as text, and would not read a
        // pdf at all.
        let read = |max: usize| part(&s, &budget[0]).map(|b| b.into_iter().take(max).collect());
        assert!(matches!(
            crate::files::preview_of(card.kind, &card.name, read),
            crate::files::Preview::Text(t) if t.starts_with("line,aug")
        ));

        let invoice = of("invoice 2026-08 — €46.20");
        assert_eq!(invoice.len(), 1);
        assert_eq!(invoice[0].name, "invoice-2026-08.pdf");
        assert!(part(&s, &invoice[0]).is_some_and(|b| b.starts_with(b"%PDF")));
        assert_eq!(
            invoice[0].card("Hetzner", 0.0).kind,
            crate::files::FileKind::Pdf
        );
        assert!(attachments(&s, 3).is_empty(), "the rest carry nothing");
        let (mail, at) = (budget[0].message, budget[0].at);
        assert_eq!(title(&s, &Kind::Attachment { mail, at }), "q3-budget.csv");
        assert_eq!(title(&s, &Kind::Attachment { mail, at: 99 }), "attachment");
        assert_eq!(attachment(&s, mail, at).as_ref(), Some(&budget[0]));
        assert_eq!(attachment(&s, mail, 99), None);

        // A panel is named `(mail, at)` and never by the row's id, because
        // these rows are derived and local while a panel replicates — an id
        // would name another device's letter. A re-derive is therefore a
        // no-op for identity, and a part the letter no longer has still
        // goes.
        let raw = raw(&s, mail).expect("the seeded raw");
        let parts = crate::sync::parse_mail(&raw).attachments;
        s.write(move |c| attach_tx(c, mail, &parts)).unwrap();
        assert_eq!(attachment(&s, mail, at).map(|a| a.name), Some("q3-budget.csv".into()));
        s.write(move |c| attach_tx(c, mail, &[])).unwrap();
        assert!(attachments(&s, mail).is_empty(), "a part that left, left");
    }

    /// The derived rows are keyed by a `message` rowid, and SQLite hands a
    /// fresh row the lowest free one — so a letter that goes must take its
    /// parts *and* its scan row, or the next letter to take that id is
    /// listed with someone else's attachments and never walked for its own.
    #[test]
    fn a_letter_that_goes_takes_its_derived_rows_with_it() {
        let s = store();
        let carrying = corpus(&s)
            .iter()
            .find(|m| m.subject == "Q3 infra budget draft")
            .map(|m| m.id)
            .expect("the seeded letter");
        let rows = |t: &str| -> i64 {
            s.conn()
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        };
        assert!(rows("attachment") > 0 && rows("attachment_scan") > 0);
        let now = 1.0;
        s.write(move |c| remove_account_tx(c, 1, now)).unwrap();
        assert_eq!((rows("attachment"), rows("attachment_scan")), (0, 0));
        assert!(attachments(&s, carrying).is_empty());

        // …and the sweep catches what a delete somewhere else missed — a
        // letter removed by an applied changeset runs no code of ours.
        s.write(move |c| {
            c.execute(
                "INSERT INTO attachment(message, part, name, mime, size, cid)
                 VALUES(4242, 0, 'ghost.pdf', 'application/pdf', 1, '')",
                [],
            )?;
            c.execute(
                "INSERT INTO attachment_scan(message, version) VALUES(4242, 1)",
                [],
            )
            .map(|_| ())
        })
        .unwrap();
        s.write(|c| crate::store::backfill_attachments(c)).unwrap();
        assert_eq!((rows("attachment"), rows("attachment_scan")), (0, 0));
    }

    /// A draft holds the *paths* it will carry (CR-010): attaching the same
    /// file twice is one part, detaching takes it off, and the whole thing
    /// goes with the draft when it is discarded.
    #[test]
    fn a_draft_holds_the_files_it_will_carry() {
        let s = store();
        let file = |path: &str, size: u64| DraftFile {
            id: 0,
            path: path.to_string(),
            name: crate::files::basename(path).to_string(),
            size,
            device: "this".into(),
        };
        let names = |panel: i64| -> Vec<String> {
            draft_files(&s, panel).iter().map(|f| f.name.clone()).collect()
        };
        let two = vec![
            file("~/Downloads/report-q3.pdf", 96 * 1024),
            file("~/Downloads/2026/notes.txt", 1124),
        ];
        let added = s.write(move |c| attach_files_tx(c, 7, &two, 1.0)).unwrap();
        assert_eq!(names(7), ["report-q3.pdf", "notes.txt"]);
        assert_eq!(added.len(), 2, "both were new");
        assert_eq!(draft_files(&s, 7)[0].label(), "report-q3.pdf · 96 KB");
        // The same file again is the same part, in the place it already
        // has — never two copies in one envelope, and never a reordering.
        // It is not *added*, either, so undoing that attach cannot take
        // away an attachment it did not make.
        let again = vec![file("~/Downloads/report-q3.pdf", 96 * 1024)];
        let added = s.write(move |c| attach_files_tx(c, 7, &again, 2.0)).unwrap();
        assert_eq!(names(7), ["report-q3.pdf", "notes.txt"]);
        assert!(added.is_empty(), "nothing was added, so nothing is undone");
        // Undo's half.
        s.write(|c| detach_files_tx(c, 7, &["~/Downloads/2026/notes.txt".to_string()]))
            .unwrap();
        assert_eq!(names(7), ["report-q3.pdf"]);
        // A compose retargeted in place keeps its id, and the files a reply
        // left are not the forward's: the seed rule the text is held to,
        // held here too — hidden at once, and cleared when the retargeted
        // draft is next written.
        let d = Draft::default();
        s.write(move |c| upsert_draft_tx(c, 7, Seed::Reply(1), &d, 4.0)).unwrap();
        assert_eq!(draft_files_for(&s, 7, Seed::Reply(1)).len(), 1);
        assert!(draft_files_for(&s, 7, Seed::Forward(1)).is_empty(), "not the forward's");
        assert!(draft_files_for(&s, 7, Seed::Blank).is_empty());
        let d = Draft::default();
        s.write(move |c| upsert_draft_tx(c, 7, Seed::Forward(1), &d, 5.0)).unwrap();
        assert!(names(7).is_empty(), "the retarget cleared them for good");
        // A panel with no draft row yet has nothing to disagree: `attach`
        // on a blank compose lands before the first keystroke does.
        let one = vec![file("~/notes.md", 2 * 1024)];
        s.write(move |c| attach_files_tx(c, 8, &one, 6.0)).unwrap();
        assert_eq!(draft_files_for(&s, 8, Seed::Blank).len(), 1);
        s.write(|c| discard_draft_files_tx(c, 8)).unwrap();
        let two = vec![
            file("~/Downloads/report-q3.pdf", 96 * 1024),
            file("~/Downloads/2026/notes.txt", 1124),
        ];
        s.write(move |c| attach_files_tx(c, 7, &two, 7.0)).unwrap();
        s.write(|c| detach_files_tx(c, 7, &["~/Downloads/2026/notes.txt".to_string()]))
            .unwrap();
        // A reopened send takes them along; a discard takes them with it.
        s.write(|c| move_draft_tx(c, 7, 42, 3.0)).unwrap();
        assert!(names(7).is_empty());
        assert_eq!(names(42), ["report-q3.pdf"]);
        s.write(|c| discard_draft_tx(c, 42)).unwrap();
        assert!(names(42).is_empty());
    }

    /// An attach is an action, so undo takes the files back off and redo
    /// puts them on — matched by path, because a redo mints fresh rows.
    #[test]
    fn an_attach_is_given_back_by_path() {
        let w = World::fake(registry());
        let files = vec![
            DraftFile {
                id: 0,
                path: "~/Downloads/report-q3.pdf".into(),
                name: "report-q3.pdf".into(),
                size: 96 * 1024,
                device: "this".into(),
            },
            DraftFile {
                id: 0,
                path: "~/notes.md".into(),
                name: "notes.md".into(),
                size: 2 * 1024,
                device: "this".into(),
            },
        ];
        let names = || -> Vec<String> {
            draft_files(w.store(), 7).iter().map(|f| f.name.clone()).collect()
        };
        let intent = Attached { panel: 7, files };
        intent.reapply(&w).unwrap();
        assert_eq!(names(), ["report-q3.pdf", "notes.md"]);
        assert!(intent.describe().contains("2 files"));
        intent.reverse(&w).unwrap();
        assert!(names().is_empty());
        // …and again, over rows whose ids are not the ones it recorded.
        intent.reapply(&w).unwrap();
        assert_eq!(names(), ["report-q3.pdf", "notes.md"]);
        intent.reverse(&w).unwrap();
        assert!(names().is_empty());
    }

    /// The seed reproduces the demo world: 69 mails, m1/m2 unread, ids in
    /// insert order (m1 = 1), newest first.
    #[test]
    fn seed_reproduces_the_demo_world() {
        let s = store();
        let rows = inbox(&s);
        assert_eq!(rows.len(), 70);
        assert_eq!(rows[0].id, 1);
        assert_eq!(rows[0].subject, "Q3 infra budget draft");
        assert!(rows[0].unread && rows[1].unread && !rows[2].unread);
        assert_eq!(fmt_date(rows[0].date), "aug 31 09:14");
        assert_eq!(me(&s), "me@prepor.dev");
        // Seeding an already-seeded store is a no-op.
        seed_if_empty(&s).unwrap();
        assert_eq!(inbox(&s).len(), 70);
    }

    /// The compose panel's TO field completes the token under the caret
    /// against the senders the store knows, by name or address; a pick
    /// lands the bare address over the token and nothing else moves.
    #[test]
    fn recipients_complete_the_token_under_the_caret() {
        let s = store();
        let r = Recipients;
        let ctx = |text: &str| r.context(text, text.len());
        let labels = |v: Vec<Suggestion>| v.into_iter().map(|s| s.label).collect::<Vec<_>>();
        // An empty token is nothing to complete: landing in the field, or
        // typing the comma for the next address, opens no box.
        assert_eq!(ctx(""), None);
        assert_eq!(ctx("vera@kovac.io, "), None);
        // Name or address, as a substring, the way `@from:` matches.
        let c = ctx("kov").unwrap();
        assert_eq!((c.start, c.partial.as_str()), (0, "kov"));
        assert_eq!(labels(r.offer(&s, &c)), vec!["Vera Kovac"]);
        assert_eq!(labels(r.offer(&s, &ctx("ELENA").unwrap())), vec!["Elena Petrova"]);
        let vera = &r.offer(&s, &c)[0];
        assert_eq!(
            (vera.value.as_str(), vera.describe.as_str()),
            ("vera@kovac.io", "vera@kovac.io")
        );
        assert_eq!(r.splice("kov", 3, &c, vera), ("vera@kovac.io".into(), 13));
        // A second recipient: the token starts after the comma and its
        // space, the first address is not offered again, and the splice
        // keeps it.
        let text = "vera@kovac.io, v";
        let c = ctx(text).unwrap();
        assert_eq!((c.start, c.partial.as_str()), (15, "v"));
        assert_eq!(c.taken, vec!["vera@kovac.io"]);
        let offer = r.offer(&s, &c);
        assert!(offer.iter().all(|s| s.value != "vera@kovac.io"), "{offer:?}");
        let max = offer.iter().find(|s| s.label == "Max Ivanov").expect("Ivanov has a v");
        assert_eq!(
            r.splice(text, text.len(), &c, max),
            ("vera@kovac.io, max@ivanov.dev".into(), 29)
        );
        // Typed out in full, an address needs no completing.
        assert!(r.offer(&s, &ctx("vera@kovac.io").unwrap()).is_empty());
        // The caret in the middle of the line completes the token it is in
        // and leaves the rest of the line alone.
        let text = "ele, max@ivanov.dev";
        let c = r.context(text, 3).unwrap();
        assert_eq!((c.start, c.partial.as_str()), (0, "ele"));
        assert_eq!(c.taken, vec!["max@ivanov.dev"]);
        let elena = &r.offer(&s, &c)[0];
        assert_eq!(
            r.splice(text, 3, &c, elena),
            ("elena.p@gmail.com, max@ivanov.dev".into(), 17)
        );
        // A `Name <addr>` token counts by its address.
        let c = ctx("Vera Kovac <vera@kovac.io>, ver").unwrap();
        assert_eq!(c.taken, vec!["vera@kovac.io"]);
        assert!(r.offer(&s, &c).is_empty());
    }

    /// The demo world carries one HTML sender, narrowed on the way in: the
    /// letter survives, the stylesheet, the layout table, the tracking
    /// pixel and the `javascript:` link do not.
    #[test]
    fn the_seeded_html_mail_is_narrowed() {
        let s = store();
        let h = mail(&s, 2).expect("the github mail").html.expect("html");
        assert!(h.contains("<ul><li>mix test"), "the list survives: {h}");
        assert!(h.contains(r#"<a href="https://github.com/x/stelaxis">"#));
        assert!(h.contains("test — <b>2"), "entities are decoded on the way in: {h}");
        // Including the ones makepad's own parser would die on: the seed
        // carries an emoji spelled as its UTF-16 surrogate pair, the way
        // real composers send them, so every run that draws this mail is a
        // check that the pair was put back together before the widget saw it.
        assert!(h.contains('🚀'), "the surrogate pair is repaired: {h}");
        assert!(!h.contains("&#55357;"), "no bare surrogate reaches the widget");
        assert!(!h.contains("background:#24292f"), "the stylesheet is gone");
        assert!(!h.contains("<table") && !h.contains("<td"), "layout is gone");
        assert!(!h.contains("pixel.gif"), "the tracking pixel is gone");
        assert!(h.contains("<img src=\"data:image/png;base64,"), "the badge stays: {h}");
        assert!(!h.contains("javascript:"), "the script link is defused");
        assert!(h.contains("unsubscribe"), "but its text is kept");
        // Its plain reading is still there for quoting a reply.
        assert!(mail(&s, 2).expect("mail").body.starts_with("Workflow main"));
        // And a text-only sender stays on the plain path.
        assert_eq!(mail(&s, 1).expect("vera").html, None);
    }

    /// Bare text keeps the shell's original semantics — one substring,
    /// sender + subject — and the tags reach the same rows, which are
    /// conversations now (CR-007): a thread matches when any of its inbox
    /// mails does, so Max's second reply, dated the 31st, brings his
    /// thread into `@date>=31.08.2026`.
    #[test]
    fn filter_matches_the_old_semantics() {
        let s = store();
        let hits = inbox_filtered(&s, "vera");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, 1);
        assert!(inbox_filtered(&s, "GITHUB").len() > 10, "case-insensitive");
        assert_eq!(inbox_filtered(&s, "@unread").len(), 2);
        assert_eq!(inbox_filtered(&s, "@not:unread").len(), 67);
        assert_eq!(inbox_filtered(&s, "@from:vera@kovac.io")[0].target, 1);
        assert_eq!(inbox_filtered(&s, "@html")[0].target, 2);
        assert_eq!(inbox_filtered(&s, "@date>=31.08.2026").len(), 3);
        assert_eq!(inbox_filtered(&s, "@date:30.08.2026").len(), 3);
        assert_eq!(inbox_filtered(&s, "(@unread @or hike) @not:html").len(), 2);
        assert!(inbox_filtered(&s, "@account:me@prepor.dev").len() == 69);
        // The table pages: the whole inbox is more than one page, and the
        // rank query finds any row without a walk.
        let mut t = Table::new(threads(Role::Inbox), MAILBOX_PAGE);
        assert_eq!(t.len(&s), 69);
        let last = t.row(&s, 68).expect("the oldest");
        assert_eq!(t.index_of(&s, &last), Some(68));
        t.set_filter("digest");
        assert_eq!(t.len(&s), 61);
        assert_eq!(t.index_of(&s, &last), Some(60));
        let (sug_from, sug_acct) = (
            (threads(Role::Inbox).suggest)(&s, "from", "kov"),
            (threads(Role::Inbox).suggest)(&s, "account", ""),
        );
        assert_eq!(sug_from[0].label, "Vera Kovac");
        assert_eq!(sug_from[0].value, "vera@kovac.io");
        assert_eq!(sug_acct[0].value, "me@prepor.dev");
    }

    /// A fresh compose starts from its seed: a reply answers its mail, a
    /// forward passes the letter on under the header block — and threads
    /// to nothing, since whoever receives it was not in the conversation.
    #[test]
    fn a_compose_starts_from_its_seed() {
        let s = store();
        assert_eq!(seed_draft(&s, Seed::Blank), Draft::default());

        let re = seed_draft(&s, Seed::Reply(1));
        assert_eq!(
            (re.to.as_str(), re.subject.as_str()),
            ("vera@kovac.io", "Re: Q3 infra budget draft")
        );
        assert!(
            re.body.starts_with(
                "\n\nOn 31 Aug 2026 at 09:14, Vera Kovac <vera@kovac.io> wrote:\n> Draft for Q3"
            ),
            "{}",
            re.body
        );
        // The whole letter, every line behind a `>` and the blank ones
        // bare — and nothing before the attribution but the room to write.
        let letter = mail(&s, 1).expect("vera").body;
        for line in re.body.lines().skip(3) {
            assert!(line == ">" || line.starts_with("> "), "{line:?}");
        }
        assert_eq!(
            re.body.lines().skip(3).count(),
            letter.trim_end().lines().count()
        );
        assert!(re.body.ends_with("the CDN line is stale."), "{}", re.body);

        // What it writes is what it reads: the app's own fold takes the
        // quote back off, attribution and all.
        let sent = format!("Numbers check out.{}", re.body);
        let (own, quote) = split_quote(&sent);
        assert_eq!(own, "Numbers check out.");
        assert!(quote.expect("a quote").starts_with("On 31 Aug 2026"));

        // A reply to a reply is still one `Re:` (mail 3 arrived as one).
        assert_eq!(
            seed_draft(&s, Seed::Reply(3)).subject,
            "Re: superapp panel model"
        );

        let fwd = seed_draft(&s, Seed::Forward(1));
        assert_eq!(fwd.to, "", "a forward wants a recipient");
        assert_eq!(fwd.subject, "Fwd: Q3 infra budget draft");
        assert!(
            fwd.body.starts_with(
                "\n\nBegin forwarded message:\n\nFrom: Vera Kovac <vera@kovac.io>\n\
                 Subject: Q3 infra budget draft\nDate: 31 Aug 2026 at 09:14\n\
                 To: me@prepor.dev\n\n"
            ),
            "{}",
            fwd.body
        );
        assert!(
            fwd.body.ends_with(letter.trim_end()),
            "the whole letter goes"
        );

        // An HTML letter forwards as its plain reading.
        let fwd = seed_draft(&s, Seed::Forward(2));
        assert!(fwd.body.contains("\n\nWorkflow main #4128"), "{}", fwd.body);
        assert!(!fwd.body.contains("<li>"), "no markup: {}", fwd.body);

        // Only a reply names a parent; both carry their source's chain.
        assert_eq!(Seed::Reply(1).in_reply_to(), Some(1));
        assert_eq!(Seed::Forward(1).in_reply_to(), None);
        assert_eq!(Seed::Blank.in_reply_to(), None);
        assert_eq!(Seed::Reply(1).source(), Some(1));
        assert_eq!(Seed::Forward(1).source(), Some(1));
        assert_eq!(Seed::Forward(1).forwards(), Some(1));
        assert_eq!(Seed::Reply(1).forwards(), None);

        // A mail the store does not have seeds nothing.
        assert_eq!(seed_draft(&s, Seed::Forward(9999)), Draft::default());
        assert_eq!(fmt_date_long(ts(2026, 1, 5, 7, 3)), "5 Jan 2026 at 07:03");

        // A sender stored under their address is written out once.
        let mut bare = mail(&s, 1).expect("vera");
        bare.head.from_name = bare.head.from_email.clone();
        assert!(forwarded(&bare).contains("\nFrom: vera@kovac.io\nSubject:"));
        bare.head.from_name.clear();
        assert!(forwarded(&bare).contains("\nFrom: vera@kovac.io\nSubject:"));
        assert!(quoted(&bare).starts_with("\n\nOn 31 Aug 2026 at 09:14, vera@kovac.io wrote:"));

        // Nothing to quote: the attribution stands alone rather than over
        // an empty line.
        bare.body.clear();
        assert!(quoted(&bare).ends_with("wrote:"), "{}", quoted(&bare));
    }

    /// A compose replaced in place keeps its panel id: the row its old
    /// seed left is not the new seed's draft, and the next save takes the
    /// seed along with the text — so the send threads and marks by what
    /// the panel shows, not by what it showed.
    #[test]
    fn a_retargeted_draft_follows_its_seed() {
        let s = store();
        let now = 1.0;
        let text = Draft {
            to: "x@y".into(),
            subject: "Re: Q3".into(),
            body: "hi".into(),
        };
        s.write(move |c| upsert_draft_tx(c, 7, Seed::Reply(1), &text, now))
            .unwrap();
        assert!(draft_for(&s, 7, Seed::Reply(1)).is_some());
        assert!(
            draft_for(&s, 7, Seed::Forward(1)).is_none(),
            "not the forward's"
        );
        assert!(draft_for(&s, 7, Seed::Blank).is_none());

        let text = Draft {
            to: String::new(),
            subject: "Fwd: Q3".into(),
            body: "fyi".into(),
        };
        s.write(move |c| upsert_draft_tx(c, 7, Seed::Forward(1), &text, now))
            .unwrap();
        let (re, fwd): (Option<i64>, Option<i64>) = s
            .conn()
            .query_row(
                "SELECT re_message, fwd_message FROM draft WHERE panel = 7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((re, fwd), (None, Some(1)), "the row is the forward's now");
        assert!(draft_for(&s, 7, Seed::Reply(1)).is_none());
        assert_eq!(
            draft_for(&s, 7, Seed::Forward(1)).map(|d| d.body),
            Some("fyi".into())
        );
    }

    /// Reopening a failed send re-keys the draft to a new panel, and the
    /// seed goes with the text: a forward that failed comes back a
    /// forward, not a blank sheet with its body in it.
    #[test]
    fn a_reopened_draft_keeps_what_it_forwards() {
        let s = store();
        let now = 1.0;
        let text = Draft {
            to: "x@y".into(),
            subject: "Fwd: Q3".into(),
            body: "fyi".into(),
        };
        s.write(move |c| upsert_draft_tx(c, 7, Seed::Forward(1), &text, now))
            .unwrap();
        s.write(move |c| reopen_send_tx(c, 7, 9, now + 1.0)).unwrap();
        assert!(draft_for(&s, 7, Seed::Forward(1)).is_none(), "moved off 7");
        assert_eq!(
            draft_for(&s, 9, Seed::Forward(1)).map(|d| d.body),
            Some("fyi".into()),
            "the new panel has the forward, seed and all"
        );
    }

    /// The marks' three questions on the inbox (CR-009), which are one
    /// query each on the thread source: every thread the filter matches
    /// (what `mark all` marks), which of a marked set it still shows, and
    /// the row for a thread it hides — read fresh by its key, base
    /// condition and all, so it is still an inbox row.
    #[test]
    fn threads_answer_for_a_marked_set() {
        let s = store();
        let mut t = Table::new(threads(Role::Inbox), MAILBOX_PAGE);
        let all = t.keys(&s).expect("the inbox can list its threads");
        assert_eq!(all.len(), 69, "every conversation, not just a page of them");
        assert_eq!(
            all,
            t.rows(&s, 0, 69).iter().map(|r| r.thread).collect::<Vec<_>>(),
            "in the table's own order"
        );
        assert_eq!(t.key(&t.row(&s, 0).expect("a row")), all[0]);

        // Under a filter: exactly the matching threads, by any member.
        t.set_filter("@from:vera@kovac.io");
        let hits = t.keys(&s).expect("keys");
        assert_eq!(hits.len(), 1);
        let (mine, other) = (hits[0], *all.iter().find(|k| **k != hits[0]).expect("another"));
        assert_eq!(thread_of(&s, 1), Some(mine), "Vera's conversation");

        // A mark the filter hides is sorted out, not dropped.
        let mut marks = Marks::new();
        marks.extend([mine, other]);
        assert_eq!(t.present(&s, &marks.keys()), vec![mine]);
        assert_eq!(t.split(&s, &marks), (vec![mine], vec![other]));

        // And it still has a row: by_key ignores the filter, and gives the
        // same aggregates the one-thread read does.
        let head = t.by_key(&s, &other).expect("the hidden mark's row");
        assert_eq!(head.thread, other);
        assert_eq!(Some(head.clone()), thread_head(&s, Role::Inbox, head.target));
        assert_eq!(t.by_key(&s, &-1), None, "no such thread");

        // The inbox knows inbox threads: filed away, the row is gone —
        // and so is the key from `keys`.
        t.set_filter("");
        for id in thread_siblings(&s, head.target) {
            assert!(s.write(move |c| archive_tx(c, id)).unwrap());
        }
        assert_eq!(t.by_key(&s, &other), None);
        assert_eq!(t.present(&s, &marks.keys()), vec![mine]);
        assert_eq!(t.keys(&s).map(|k| k.len()), Some(68));
    }

    /// Archive moves a mail out of the inbox, the
    /// read flag clears once, titles resolve through the store.
    #[test]
    fn mutations_and_titles() {
        let s = store();
        assert_eq!(title(&s, &Kind::Message { id: 1 }), "Q3 infra budget draft");
        assert_eq!(
            title(&s, &Kind::Contact { email: "vera@kovac.io".into() }),
            "Vera Kovac"
        );
        let compose = |seed| Kind::Compose { seed };
        assert_eq!(title(&s, &compose(Seed::Blank)), "new mail");
        assert_eq!(
            title(&s, &compose(Seed::Reply(1))),
            "re: Q3 infra budget draft"
        );
        assert_eq!(
            title(&s, &compose(Seed::Forward(1))),
            "fwd: Q3 infra budget draft"
        );
        let filtered = Kind::Mailbox { role: Role::Inbox, filter: Some("x".into()) };
        assert_eq!(title(&s, &filtered), "inbox · x");
        // A mailbox is titled what its folder is.
        for role in crate::core::ROLES {
            let k = Kind::Mailbox { role, filter: None };
            assert_eq!(title(&s, &k), role.as_str());
        }

        s.write(|c| mark_read_tx(c, 1)).unwrap();
        assert!(!inbox(&s)[0].unread);
        assert!(s.write(|c| archive_tx(c, 1)).unwrap(), "archive moved it");
        assert_eq!(inbox(&s).len(), 69);
        assert_ne!(inbox(&s)[0].id, 1);
        assert_eq!(corpus(&s).len(), 79, "archived mail stays in the corpus");
        let (name, n) = contact(&s, "vera@kovac.io");
        assert_eq!((name.as_str(), n), ("Vera Kovac", 1));
    }

    /// Filing a mail into the folder it already sits in moves nothing, and
    /// says so — otherwise `archive` on a mail read out of the archive (the
    /// button is about the mail, so it is still there) would record an undo
    /// node and push the server a MOVE onto itself.
    #[test]
    fn filing_a_mail_where_it_already_is_moves_nothing() {
        let s = store();
        assert!(!already_filed(&s, 1, "archive"));
        assert!(s.write(|c| archive_tx(c, 1)).unwrap(), "the first move lands");
        assert_eq!(role_of(&s, 1), Some(Role::Archive));

        assert!(already_filed(&s, 1, "archive"));
        assert!(can_file(&s, 1, "archive"), "the folder is there all the same");
        assert!(!s.write(|c| archive_tx(c, 1)).unwrap(), "the second moves nothing");
        assert_eq!(role_of(&s, 1), Some(Role::Archive));
        // And out of the archive is still a move.
        assert!(s.write(|c| delete_tx(c, 1)).unwrap());
        assert_eq!(role_of(&s, 1), None, "trash plays no mailbox role");
    }

    /// Delete is archive's twin on the trash folder, and both refuse to move
    /// a mail whose account has no such folder rather than stranding it with
    /// a null one (which would drop it from the inbox *and* from the push).
    #[test]
    fn delete_files_to_trash_and_needs_the_folder() {
        let s = store();
        assert!(can_file(&s, 2, "trash"));
        assert!(s.write(|c| delete_tx(c, 2)).unwrap(), "delete moved it");
        assert_eq!(inbox(&s).len(), 69);
        assert!(!inbox(&s).iter().any(|m| m.id == 2));
        assert_eq!(corpus(&s).len(), 79, "deleted mail stays in the corpus");

        // An account without the folder: the mail must stay exactly where it
        // is. A fresh store, because the folder can only be dropped while
        // nothing references it.
        let s = store();
        s.write(|c| c.execute("DELETE FROM folder WHERE role = 'trash'", []))
            .unwrap();
        assert!(!can_file(&s, 3, "trash"));
        assert!(!s.write(|c| delete_tx(c, 3)).unwrap(), "nothing to move to");
        assert!(inbox(&s).iter().any(|m| m.id == 3), "still in the inbox");
    }

    /// How long a letter is, counted in wrapped lines — the measure behind
    /// a message panel's height wish. Paragraph breaks count as the blank
    /// line they draw, and the reading measured is the one the panel shows.
    #[test]
    fn a_letters_length_is_counted_in_wrapped_lines() {
        let s = store();
        let vera = mail(&s, 1).expect("vera");
        // Two paragraphs, wide enough not to wrap: two lines and the blank
        // one between them.
        assert_eq!(reading_lines(&vera, 1000), 3);
        // The narrower the column, the more lines the same letter takes.
        assert!(reading_lines(&vera, 40) > reading_lines(&vera, 80));
        // The HTML sender is measured on its HTML — the reading the panel
        // draws — so its list is lines rather than one long blob.
        let gh = mail(&s, 2).expect("github");
        assert!(reading_lines(&gh, 1000) >= 5, "{}", reading_lines(&gh, 1000));
        // And the demo world's one long letter dwarfs them both.
        let long = corpus(&s)
            .iter()
            .find(|m| m.subject.starts_with("long version"))
            .and_then(|m| mail(&s, m.id))
            .expect("the long letter");
        assert!(reading_lines(&long, 60) > 4 * reading_lines(&vera, 60));
    }

    /// `raw` is a whole letter sitting in the middle of every `message`
    /// row, and SQLite decodes a record left to right: reading one column
    /// past it walks the overflow chain of every mail it touches. Every
    /// list read must stop before it — over a real mailbox the difference
    /// is a millisecond against thirty (see [`head_row`]).
    #[test]
    fn the_corpus_stops_before_the_letter() {
        let s = store();
        let cid = |name: &str| -> i64 {
            s.conn()
                .query_row(
                    "SELECT cid FROM pragma_table_info('message') WHERE name = ?1",
                    [name],
                    |r| r.get(0),
                )
                .expect("a column of message")
        };
        let raw = cid("raw");
        for col in ["id", "from_name", "from_email", "subject", "date", "unread"] {
            assert!(cid(col) < raw, "{col} sits past raw");
            for q in [&Q_CORPUS, &Q_FTS] {
                assert!(q.sql.contains(col), "{} lost {col}", q.id);
            }
        }
        for late in ["body", "html", "thread", "topic", "forwarded", "raw"] {
            assert!(cid(late) > cid("unread"));
            for q in [&Q_CORPUS, &Q_FTS] {
                assert!(!q.sql.contains(late), "{} reads {late}", q.id);
            }
        }
    }

    /// A trace records exactly what was read between begin and end — the
    /// provenance the panel context serializes.
    #[test]
    fn traces_record_panel_provenance() {
        let s = store();
        s.trace_begin(7);
        let _ = inbox(&s);
        let _ = mail(&s, 1);
        let _ = mail(&s, 1); // repeated reads dedupe
        s.trace_end();
        let _ = senders(&s); // outside the trace: not recorded
        let t = s.trace_of(7);
        assert_eq!(
            t.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec!["inbox", "mail"]
        );
        assert_eq!(t[0].rows, 70);
        assert_eq!(t[1].params, "1");
        assert!(t[0].describe.contains("inbox"));
    }

    /// A conversation is called what its subject says once the reply
    /// prefixes are off, in whichever language the client wrote them.
    #[test]
    fn topics_strip_reply_prefixes() {
        assert_eq!(topic_of("Re: superapp panel model"), "superapp panel model");
        assert_eq!(topic_of("RE: Re: Fwd: budget"), "budget");
        assert_eq!(topic_of("AW: Re[2]: budget"), "budget");
        assert_eq!(topic_of("[stelaxis] CI failed on main"), "[stelaxis] CI failed on main");
        assert_eq!(topic_of("invite: dentist — tue 10:00"), "invite: dentist — tue 10:00");
        assert_eq!(topic_of("Re:"), "Re:", "nothing left: the subject stands");
    }

    /// The three lookups, and the merge: a reply finds its parent, a late
    /// parent finds its replies, two orphans sharing a missing parent find
    /// each other — and when a bridge arrives, two threads become one.
    #[test]
    fn threading_joins_by_references_both_ways_and_merges() {
        let s = store();
        let put = |mid: &str, refs: &[&str]| -> i64 {
            let refs: Vec<String> = refs.iter().map(|r| (*r).to_string()).collect();
            let mid = mid.to_string();
            s.write(move |c| {
                c.execute(
                    "INSERT INTO message(account, folder, subject, date, message_id, topic)
                     VALUES(1, 1, ?1, 0, ?1, ?1)",
                    [mid.as_str()],
                )?;
                let id = c.last_insert_rowid();
                thread_tx(c, 1, id, &mid, &refs)?;
                Ok(id)
            })
            .unwrap()
        };
        // 1. a reply names its parent
        let a = put("a@x", &[]);
        let a1 = put("a1@x", &["a@x"]);
        assert_eq!(thread_of(&s, a1), Some(a));
        // 2. a parent arriving after its replies
        let b1 = put("b1@x", &["b@x"]);
        let b = put("b@x", &[]);
        assert_eq!(thread_of(&s, b), Some(b1), "anchored on the earliest id");
        // 3. two orphans under one missing parent
        let c1 = put("c1@x", &["c@x"]);
        let c2 = put("c2@x", &["c@x", "c1@x"]);
        assert_eq!(thread_of(&s, c2), Some(c1));
        // A bridge: a reply to a1 that also references b — one thread.
        let bridge = put("ab@x", &["a1@x", "b@x"]);
        assert_eq!(thread_of(&s, bridge), Some(a));
        assert_eq!(thread_of(&s, b), Some(a));
        assert_eq!(thread_of(&s, b1), Some(a));
        assert_eq!(thread(&s, b1).len(), 5);
        // Unrelated mail is untouched.
        assert_eq!(thread_of(&s, c1), Some(c1));
        assert_eq!(thread(&s, c1).len(), 2);
    }

    /// Four lists over one store: each shows the conversations its folder
    /// holds, and nothing another folder holds. The aggregates on a row
    /// still cover the *whole* conversation, whichever list it is read
    /// in — the GitHub thread is six mails long from the inbox and from
    /// the archive alike, and only the mail the row opens differs.
    #[test]
    fn each_mailbox_lists_its_own_folder() {
        let s = store();
        let n = |role| mailbox_filtered(&s, role, "").len();
        assert_eq!(n(Role::Inbox), 69);
        // The five CI runs are one conversation; the archive has no other.
        assert_eq!(n(Role::Archive), 1);
        assert_eq!(n(Role::Sent), 1);
        assert_eq!(n(Role::Spam), 3);

        // The same conversation, read in two mailboxes: one row each, the
        // same participants and count, a different mail to open.
        let inbox = thread_head(&s, Role::Inbox, 2).expect("the inbox row");
        let archived = thread_head(&s, Role::Archive, 2).expect("the archive row");
        assert_eq!((inbox.thread, inbox.n), (archived.thread, archived.n));
        assert_eq!(inbox.n, 6);
        assert_eq!(inbox.target, 2, "the unread inbox mail");
        assert_ne!(archived.target, 2, "an archived one");
        // And a mailbox it is not in has no row for it at all.
        assert_eq!(thread_head(&s, Role::Spam, 2), None);

        // The filter is the same grammar over each: the spam list sifts
        // its own rows and knows nothing of the inbox's.
        assert_eq!(mailbox_filtered(&s, Role::Spam, "@unread").len(), 2);
        assert_eq!(mailbox_filtered(&s, Role::Spam, "vera").len(), 0);
        assert_eq!(mailbox_filtered(&s, Role::Sent, "panel model")[0].who, ["Max", "me"]);

        // `@from:` completes against the senders of the list it is on: the
        // spam one offers spammers, and no other list offers them.
        let spam = (threads(Role::Spam).suggest)(&s, "from", "crypt");
        assert_eq!(spam.len(), 1);
        assert_eq!(spam[0].value, "no-reply@crypt0-rewards.biz");
        assert!((threads(Role::Inbox).suggest)(&s, "from", "crypt").is_empty());
        assert!(senders(&s).iter().all(|x| !x.email.contains("crypt0")));
    }

    /// The demo world threads: Max's two replies and my note make one
    /// conversation of three, and the GitHub mail continues five archived
    /// runs. The inbox row shows the whole conversation; the walk opens
    /// the newest inbox mail, or the oldest unread one.
    #[test]
    fn the_seed_threads_the_demo_world() {
        let s = store();
        let max = thread(&s, 3);
        assert_eq!(
            max.iter().map(|t| t.mail.head.id).collect::<Vec<_>>(),
            vec![70, 3, 71],
            "oldest first, my sent note included"
        );
        assert_eq!(max[0].role, "sent");
        let row = thread_head(&s, Role::Inbox, 3).expect("in the inbox");
        assert_eq!(row.who, vec!["Max", "me"]);
        assert_eq!(row.who_line(), "Max, me · 3");
        assert_eq!(row.topic, "superapp panel model");
        assert_eq!(row.target, 71, "newest inbox mail: none unread");
        assert!(!row.unread);
        // My own note is in Sent: its siblings there are itself alone,
        // while the conversation's inbox copies are Max's two replies.
        assert_eq!(thread_siblings(&s, 70), vec![70]);
        assert_eq!(thread_siblings(&s, 3), vec![3, 71]);

        let gh = thread(&s, 2);
        assert_eq!(gh.len(), 6);
        assert_eq!(gh[5].mail.head.id, 2, "the inbox mail is the newest");
        assert_eq!(gh[0].mail.status.as_ref().map(|s| s.1), Some(true), "one red run");
        let row = thread_head(&s, Role::Inbox, 2).expect("in the inbox");
        assert_eq!(row.who, vec!["GitHub"], "one participant keeps the full name");
        assert_eq!(row.who_line(), "GitHub · 6");
        assert_eq!(row.target, 2, "the unread one");
        assert!(row.unread);
        assert_eq!(thread_unread(&s, 2), vec![2]);

        // Rows: 70 inbox mails fold into 69 conversations, and the two
        // unread ones are the two bold rows.
        let rows = inbox_filtered(&s, "");
        assert_eq!(rows.len(), 69);
        assert_eq!(rows[0].target, 1);
        assert_eq!(rows[1].target, 2);
        assert_eq!(rows[2].target, 71, "Max's thread, by its latest inbox mail");
        assert_eq!(title(&s, &Kind::Message { id: 3 }), "superapp panel model");
        assert_eq!(title(&s, &Kind::Message { id: 71 }), "superapp panel model");
        // A lone mail is a row exactly as before.
        assert_eq!(rows[3].who_line(), "Elena Petrova");
    }

    /// The quoted tail folds: the attribution line and the `>` block under
    /// it, leaving what the author wrote. A letter without one stays whole,
    /// and so does one that is nothing but quote.
    #[test]
    fn quotes_fold_off_the_tail() {
        let (own, q) = split_quote("Agreed.\n\nOn Sun, Max wrote:\n> the note\n>\n> more");
        assert_eq!(own, "Agreed.");
        assert_eq!(q.as_deref(), Some("On Sun, Max wrote:\n> the note\n>\n> more"));
        let (own, q) = split_quote("Agreed.\n\nOn Sun, 30 Aug 2026,\nMax Ivanov wrote:\n> the note");
        assert_eq!(own, "Agreed.");
        assert!(q.unwrap().starts_with("On Sun"), "a wrapped attribution goes too");
        assert_eq!(split_quote("> only a quote").1, None);
        assert_eq!(split_quote("no quote at all").1, None);
        assert_eq!(split_quote("a > b in the middle\nthen more").1, None);
        let (own, q) = split_quote_html("<p>Agreed.</p><p>On Sun, Max wrote:</p><blockquote>the note</blockquote>");
        assert_eq!(own, "<p>Agreed.</p>");
        assert_eq!(q.as_deref(), Some("<p>On Sun, Max wrote:</p><blockquote>the note</blockquote>"));
        // Gmail and Apple Mail write the attribution in a `<div>` of its own,
        // which the narrowing turns into a `<br>`-separated line.
        let (own, q) = split_quote_html("Agreed.<br>On Sun, Max wrote:<blockquote>the note</blockquote>");
        assert_eq!(own, "Agreed.");
        assert_eq!(q.as_deref(), Some("On Sun, Max wrote:<blockquote>the note</blockquote>"));
        let (own, q) = split_quote_html("Agreed.<br>On Sun, 30 Aug 2026,<br>Max wrote:<blockquote>x</blockquote>");
        assert_eq!(own, "Agreed.");
        assert!(q.unwrap().starts_with("On Sun"), "a wrapped attribution goes too");
        // An attribution sharing its paragraph with the letter is not cut
        // out of it: the reading stays whole rather than losing its shape.
        assert_eq!(split_quote_html("<p>Agreed.<br>On Sun, Max wrote:</p><blockquote>x</blockquote>").1, None);
        let s = store();
        let m = mail(&s, 71).expect("max's second reply");
        assert_eq!(own_text(&m).lines().count(), 1, "one paragraph; the quote is gone");
        let open: BTreeSet<MailId> = [71].into_iter().collect();
        let none: BTreeSet<MailId> = BTreeSet::new();
        let t = thread(&s, 71);
        assert_eq!(thread_lines(&t, &open, &none, 1000), 8, "1.5 + 1.5 + (4 + 1)");
        // A letter that carries something is a line taller open (CR-010),
        // and not a pixel taller closed.
        assert_eq!(thread_lines(&t, &open, &open, 1000), 9);
        let shut: BTreeSet<MailId> = BTreeSet::new();
        assert_eq!(thread_lines(&t, &shut, &open, 1000), thread_lines(&t, &shut, &none, 1000));
    }

    /// The civil-date maths round-trips.
    #[test]
    fn dates_round_trip() {
        assert_eq!(fmt_date(ts(2026, 8, 31, 9, 14)), "aug 31 09:14");
        assert_eq!(fmt_date(ts(2026, 1, 1, 0, 0)), "jan 01 00:00");
        assert_eq!(fmt_date(ts(2025, 12, 31, 23, 59)), "dec 31 23:59");
    }
}
