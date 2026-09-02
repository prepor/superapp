//! The mail domain over the store: typed queries, titles, the demo seed,
//! and the local mutations (read flags, archive).
//!
//! Everything panels show comes through the registered [`Q`] queries — that
//! is the reactive contract (see [`crate::store`]) and, later, the panel
//! context an agent receives. The inbox is a rich table over [`THREADS`]: its
//! filter is the shared grammar ([`crate::filter`]), whose bare text is one
//! substring over sender + subject — the shell's original semantics; the
//! launcher's word-AND lives in [`crate::launcher`].

use std::collections::BTreeSet;
use std::rc::Rc;

use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use crate::core::{Kind, MailId, Seed};
use crate::effect::{Creds, Ctx, Deferred, Effect, MailFlag, Outgoing, Registry, UidSet, World};
use crate::filter::Op;
use crate::history::Intent;
use crate::richtable::{
    Completion, Dir, SqlSource, SqlSpec, Suggestion, Table, TagDef, TagSql, TagType, Values,
    MAX_SUGGESTIONS,
};
use crate::store::{Q, Store, Val};

/// One list row: what the inbox and the launcher show.
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

/// One inbox row (CR-007): a conversation, as far as the inbox is
/// concerned — every message of it counts, and it is a row while at least
/// one of them sits in the inbox.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadHead {
    /// The anchor: the lowest member's id. The row's identity.
    pub thread: i64,
    /// The mail the row opens: the oldest unread inbox message, else the
    /// newest one.
    pub target: MailId,
    /// Who wrote in it, newest speaker first, `me` for the account's own
    /// address — first names once there are two of them.
    pub who: Vec<String>,
    /// Its subject, reply prefixes stripped, from the oldest message.
    pub topic: String,
    /// The latest inbox message's date: the order.
    pub last: f64,
    /// Any inbox message unread.
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
    /// The role of the folder it sits in: `inbox`, `archive`, `sent`.
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

static Q_ALL: Q = Q {
    id: "all_mail",
    sql: "SELECT id, from_name, from_email, subject, date, unread
          FROM message ORDER BY date DESC, id DESC",
    describe: "every mail, archived included, newest first",
};

static Q_MAIL: Q = Q {
    id: "mail",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err, a.email, m.html, m.forwarded
          FROM message m JOIN account a ON a.id = m.account
          WHERE m.id = ?1",
    describe: "one mail, both bodies included, with its account's address",
};

static Q_SENDERS: Q = Q {
    id: "senders",
    sql: "SELECT from_email, from_name, MAX(date) AS last
          FROM message GROUP BY from_email ORDER BY last DESC",
    describe: "distinct senders, most recently heard from first",
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

/// Decodes one grouped row of [`THREADS_SPEC`]: the participants arrive
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

/// The inbox as a rich table (CR-006) of **threads** (CR-007): the fixed
/// parts of its query, which the builder completes with the filter, the
/// page and the rank. The rows are inbox messages grouped by conversation;
/// what a row shows is aggregates over them, or over the whole conversation
/// (participants, count, topic — trash left out), read by subquery. The
/// account join is for `@account:` and for `me`.
static THREADS_SPEC: SqlSpec = SqlSpec {
    id: "inbox table",
    describe: "the inbox as conversations under the panel's filter, latest first, one page at a time",
    select: "m.thread AS thread,
             MAX(m.date) AS last,
             MAX(m.unread) AS unread,
             (SELECT t.id FROM message t JOIN folder tf ON tf.id = t.folder
               WHERE t.thread = m.thread AND tf.role = 'inbox'
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
               WHERE t.thread = m.thread AND tf.role IS NOT 'trash') AS n",
    from: "message m JOIN folder f ON m.folder = f.id JOIN account a ON a.id = m.account",
    base: "f.role = 'inbox'",
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
};

const DATE_OPS: &[Op] = &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte];

/// The inbox filter's tags: what `@` offers. Each reads against the inbox
/// messages, and a conversation matches when any of them does.
static INBOX_TAGS: &[TagDef] = &[
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

/// Values for the inbox's dynamic tags, under what has been typed: senders
/// (most recently heard from first, by name or address) and accounts. Each
/// is one cached query; the match is a substring, so `kov` finds Vera.
fn suggest_inbox(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    match tag {
        "from" => senders(store)
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

/// The inbox's datasource: what the inbox panel's rich table runs on.
pub static THREADS: SqlSource<ThreadHead, i64> = SqlSource {
    spec: &THREADS_SPEC,
    tags: INBOX_TAGS,
    map: thread_head_row,
    key: |t| t.thread,
    rank: |t| vec![Val::F(t.last), Val::I(t.thread)],
    suggest: suggest_inbox,
};

/// Rows per page of the inbox table — what one scroll's worth of draws
/// fetches; the count is separate, so this is a batch size, not a limit.
pub const INBOX_PAGE: usize = 50;

/// The whole inbox under a filter, materialized — for tests and the odd
/// one-shot read; panels page through [`THREADS`] instead.
pub fn inbox_filtered(store: &Store, filter: &str) -> Vec<ThreadHead> {
    let mut t = Table::new(&THREADS, INBOX_PAGE);
    t.set_filter(filter);
    let n = t.len(store);
    t.rows(store, 0, n)
}

/// Every mail, archived included (the launcher's corpus).
pub fn all(store: &Store) -> Rc<Vec<MailHead>> {
    store.rows(&Q_ALL, &[], head_row)
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

/// Distinct senders, most recent first.
pub fn senders(store: &Store) -> Rc<Vec<Sender>> {
    store.rows(&Q_SENDERS, &[], sender_row)
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

/// Removes an account and everything it brought.
pub fn remove_account_tx(c: &rusqlite::Connection, id: i64) -> rusqlite::Result<()> {
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

/// Which of a conversation's mails sit in the inbox — what filing it moves.
pub fn thread_inbox(store: &Store, id: MailId) -> Vec<MailId> {
    store
        .rows(&Q_THREAD_MEMBERS, &[Val::I(id)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(2)?))
        })
        .iter()
        .filter(|(_, role)| role == "inbox")
        .map(|(id, _)| *id)
        .collect()
}

/// The inbox row a mail's conversation makes — the same aggregates the
/// table shows, for one thread — or `None` while none of it is in the inbox.
pub fn thread_head(store: &Store, id: MailId) -> Option<ThreadHead> {
    let sql = format!(
        "SELECT {} FROM {} WHERE {} AND m.thread = (SELECT thread FROM message WHERE id = ?1)
         GROUP BY m.thread",
        THREADS_SPEC.select, THREADS_SPEC.from, THREADS_SPEC.base
    );
    store
        .rows_sql(
            "thread head",
            "one conversation's inbox row",
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
/// folded), the status line if it has one, and the spacing and rule around
/// them — about four lines beyond the text. An estimate, like the chrome
/// allowance it feeds: the wish only has to land on the right grid row.
#[must_use]
pub fn thread_lines(msgs: &[ThreadMail], open: &BTreeSet<MailId>, cols: usize) -> usize {
    let lines: f64 = msgs
        .iter()
        .map(|t| {
            if open.contains(&t.mail.head.id) {
                4.0 + wrapped_lines(&own_text(&t.mail), cols) as f64
                    + if t.mail.status.is_some() { 1.0 } else { 0.0 }
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
        Kind::Inbox { filter: Some(f) } => format!("inbox · {f}"),
        Kind::Inbox { filter: None } => "inbox".into(),
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
        Kind::Bucket => "device sync".into(),
    }
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

/// Moves a mail out of the inbox into one of its account's role folders.
/// Intent only — the push pass makes the server agree (see [`mark_read_tx`]),
/// and it is generic over the move, so trash rides the same path archive
/// already proved.
///
/// The `EXISTS` guard is load-bearing: an account whose server advertises no
/// such folder (see [`crate::sync`]'s role detection) would otherwise get a
/// `NULL` from the subquery, and a mail with a null folder falls out of the
/// inbox query *and* out of the push set's join — vanishing silently, with
/// nothing to sync it back. Returns whether the mail actually moved, so the
/// caller can say so.
fn file_tx(c: &rusqlite::Connection, id: MailId, role: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE message SET folder =
           (SELECT f.id FROM folder f
            WHERE f.account = message.account AND f.role = ?2)
         WHERE id = ?1
           AND EXISTS (SELECT 1 FROM folder f
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
            subject: format!("Re: {}", m.head.subject),
            body: String::new(),
        }),
        Seed::Forward(id) => mail(store, id).map_or_else(Draft::default, |m| Draft {
            to: String::new(),
            subject: format!("Fwd: {}", m.head.subject),
            body: forwarded(&m),
        }),
    }
}

/// A forward's body: room to write at the top, then the mail under the
/// header block every client recognises — who wrote it, about what, when,
/// to whom. The letter is the plain reading, which an HTML mail keeps for
/// exactly this.
#[must_use]
pub fn forwarded(m: &MailFull) -> String {
    // A sender without a name is stored under their address as the name
    // (see [`crate::sync::parse_mail`]); written out, that is the address
    // once, not twice.
    let (name, email) = (&m.head.from_name, &m.head.from_email);
    let from = if name.is_empty() || name == email {
        email.clone()
    } else {
        format!("{name} <{email}>")
    };
    format!(
        "\n\nBegin forwarded message:\n\nFrom: {from}\nSubject: {}\nDate: {}\nTo: {}\n\n{}",
        m.head.subject,
        fmt_date_long(m.head.date),
        m.to,
        m.body.trim_end()
    )
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
    Ok(())
}

/// Discard: the draft goes with the panel (both revert on undo).
pub fn discard_draft_tx(c: &rusqlite::Connection, panel: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM draft WHERE panel=?1", [panel])?;
    Ok(())
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
        let d = load_outgoing(cx.db, self.outbox)?;
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
}

fn load_outgoing(db: &Connection, outbox: i64) -> Result<Outgo, String> {
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
                },
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

/// Discarding a compose takes its text with it.
pub struct Discarded {
    pub panel: i64,
    pub draft: Draft,
    pub seed: Seed,
}

impl Intent for Discarded {
    fn describe(&self) -> String {
        format!("panel:{} draft discarded", self.panel)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (now, panel, seed, draft) = (w.now(), self.panel, self.seed, self.draft.clone());
        w.store()
            .write(move |c| upsert_draft_tx(c, panel, seed, &draft, now))
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
        let id = self.id;
        w.store()
            .write(move |c| remove_account_tx(c, id))
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
    /// The folder's role: `inbox`, `archive` or `sent`.
    folder: &'a str,
    /// Message-ID and what it references — the threading headers (CR-007);
    /// empty for a mail that stands alone.
    mid: &'a str,
    refs: &'a [&'a str],
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
        folder("Trash", "trash")?;
        let folder_of = |role: &str| match role {
            "archive" => archive,
            "sent" => sent,
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
            insert(m)?;
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
        let mut t = Table::new(&THREADS, INBOX_PAGE);
        assert_eq!(t.len(&s), 69);
        let last = t.row(&s, 68).expect("the oldest");
        assert_eq!(t.index_of(&s, &last), Some(68));
        t.set_filter("digest");
        assert_eq!(t.len(&s), 61);
        assert_eq!(t.index_of(&s, &last), Some(60));
        let (sug_from, sug_acct) = (
            (THREADS.suggest)(&s, "from", "kov"),
            (THREADS.suggest)(&s, "account", ""),
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
            (re.to.as_str(), re.subject.as_str(), re.body.as_str()),
            ("vera@kovac.io", "Re: Q3 infra budget draft", "")
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
        let letter = mail(&s, 1).expect("vera").body;
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
        let mut t = Table::new(&THREADS, INBOX_PAGE);
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
        assert_eq!(Some(head.clone()), thread_head(&s, head.target));
        assert_eq!(t.by_key(&s, &-1), None, "no such thread");

        // The inbox knows inbox threads: filed away, the row is gone —
        // and so is the key from `keys`.
        t.set_filter("");
        for id in thread_inbox(&s, head.target) {
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
        assert_eq!(title(&s, &Kind::Inbox { filter: Some("x".into()) }), "inbox · x");

        s.write(|c| mark_read_tx(c, 1)).unwrap();
        assert!(!inbox(&s)[0].unread);
        assert!(s.write(|c| archive_tx(c, 1)).unwrap(), "archive moved it");
        assert_eq!(inbox(&s).len(), 69);
        assert_ne!(inbox(&s)[0].id, 1);
        assert_eq!(all(&s).len(), 76, "archived mail stays in the corpus");
        let (name, n) = contact(&s, "vera@kovac.io");
        assert_eq!((name.as_str(), n), ("Vera Kovac", 1));
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
        assert_eq!(all(&s).len(), 76, "deleted mail stays in the corpus");

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
        let long = all(&s)
            .iter()
            .find(|m| m.subject.starts_with("long version"))
            .and_then(|m| mail(&s, m.id))
            .expect("the long letter");
        assert!(reading_lines(&long, 60) > 4 * reading_lines(&vera, 60));
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
        let row = thread_head(&s, 3).expect("in the inbox");
        assert_eq!(row.who, vec!["Max", "me"]);
        assert_eq!(row.who_line(), "Max, me · 3");
        assert_eq!(row.topic, "superapp panel model");
        assert_eq!(row.target, 71, "newest inbox mail: none unread");
        assert!(!row.unread);
        assert_eq!(thread_inbox(&s, 70), vec![3, 71]);

        let gh = thread(&s, 2);
        assert_eq!(gh.len(), 6);
        assert_eq!(gh[5].mail.head.id, 2, "the inbox mail is the newest");
        assert_eq!(gh[0].mail.status.as_ref().map(|s| s.1), Some(true), "one red run");
        let row = thread_head(&s, 2).expect("in the inbox");
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
        let t = thread(&s, 71);
        assert_eq!(thread_lines(&t, &open, 1000), 8, "1.5 + 1.5 + (4 + 1)");
    }

    /// The civil-date maths round-trips.
    #[test]
    fn dates_round_trip() {
        assert_eq!(fmt_date(ts(2026, 8, 31, 9, 14)), "aug 31 09:14");
        assert_eq!(fmt_date(ts(2026, 1, 1, 0, 0)), "jan 01 00:00");
        assert_eq!(fmt_date(ts(2025, 12, 31, 23, 59)), "dec 31 23:59");
    }
}
