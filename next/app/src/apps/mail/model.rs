//! Mail's rows, its queries, and the writes a verb composes into an action.
//!
//! Mailbox panels page through a rich table; everything else reads through a
//! registered [`Q`], so caching and provenance stay accurate. The `_tx`
//! functions are transaction-level pieces: an action runs them inside its one
//! transaction, and an [`Intent`](kernel::history::Intent) puts them back.

use std::collections::BTreeSet;
use std::rc::Rc;

use kernel::caps::Secrets;
use kernel::filter::Op;
use kernel::panel::{PanelId, Tag};
use kernel::richtable::{
    Dir, SqlSource, SqlSpec, Suggestion, Table, TagDef, TagSql, TagType, Values,
};
use kernel::store::{Q, Store, Val};
use kernel::time::fmt_date_long;

use super::caps::Creds;

/// A mail's row id. The one argument a `message` panel carries.
pub type MailId = i64;

// -- rows ----------------------------------------------------------------------

/// One list row's worth of a mail: what the launcher and a card's head show.
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
#[derive(Debug, Clone, PartialEq)]
pub struct MailFull {
    pub head: MailHead,
    /// Paragraphs, `\n\n`-separated in the store.
    pub body: String,
    /// An optional status line; `true` marks it as an error.
    pub status: Option<(String, bool)>,
    /// The receiving account's address (the TO line).
    pub to: String,
}

/// A distinct sender: what a filter's `@from:` completes against.
#[derive(Debug, Clone, PartialEq)]
pub struct Sender {
    pub email: String,
    pub name: String,
}

/// One row of a mailbox: a conversation, as far as *that folder* is
/// concerned — every message of it counts towards what the row shows, and it
/// is a row while at least one of them sits in the folder. So one
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
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMail {
    pub mail: MailFull,
    /// The role of the folder it sits in: `inbox`, `archive`, `sent`.
    pub role: String,
    pub message_id: String,
}

/// A compose panel's persisted draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// One account row.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: i64,
    pub email: String,
    pub imap_host: String,
    pub smtp_host: String,
    pub status: Option<String>,
    pub synced: Option<f64>,
}

// -- the mailbox roles ---------------------------------------------------------

/// Which mailbox a list panel is over. The prototype shows two of the four;
/// the folders behind the other two exist, because a mail has to be able to
/// go there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Inbox,
    Archive,
}

impl Role {
    pub const INBOX: Tag = Tag("inbox");
    pub const ARCHIVE: Tag = Tag("archive");

    #[must_use]
    pub fn tag(self) -> Tag {
        match self {
            Role::Inbox => Role::INBOX,
            Role::Archive => Role::ARCHIVE,
        }
    }

    /// The word the `folder.role` column holds.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Inbox => "inbox",
            Role::Archive => "archive",
        }
    }

    /// The identity of this mailbox's panel.
    #[must_use]
    pub fn id(self) -> PanelId {
        PanelId::bare(self.tag())
    }

    /// The role a tag names; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<Role> {
        match id.tag {
            Role::INBOX => Some(Role::Inbox),
            Role::ARCHIVE => Some(Role::Archive),
            _ => None,
        }
    }
}

// -- queries -------------------------------------------------------------------

static Q_MAIL: Q = Q {
    id: "mail",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err, a.email
          FROM message m JOIN account a ON a.id = m.account
          WHERE m.id = ?1",
    describe: "one mail, its body included, with its account's address",
};

static Q_THREAD: Q = Q {
    id: "thread",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err, a.email,
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

static Q_SENDERS: Q = Q {
    id: "senders",
    sql: "SELECT m.from_email, m.from_name, MAX(m.date) AS last
          FROM message m
          GROUP BY m.from_email ORDER BY last DESC",
    describe: "distinct senders, most recently heard from first",
};

static Q_ACCOUNTS: Q = Q {
    id: "accounts",
    sql: "SELECT id, email, COALESCE(imap_host, ''), COALESCE(smtp_host, ''), status, synced
          FROM account ORDER BY id",
    describe: "every account with its hosts and its sync status",
};

/// A list row's worth of a mail, and the last column any list may read: a
/// scan of the whole mailbox has to stop before anything wide.
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
        status: status.map(|s| (s, err)),
        to: r.get(9)?,
    })
}

fn thread_row(r: &rusqlite::Row) -> rusqlite::Result<ThreadMail> {
    Ok(ThreadMail {
        mail: full_row(r)?,
        role: r.get(10)?,
        message_id: r.get(11)?,
    })
}

/// Decodes one grouped row of a mailbox spec: the participants arrive newest
/// speaker first, one per sender, separated by the unit separator (a name may
/// carry a comma; none carries that).
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

fn account_row(r: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        email: r.get(1)?,
        imap_host: r.get(2)?,
        smtp_host: r.get(3)?,
        status: r.get(4)?,
        synced: r.get(5)?,
    })
}

/// One mail by id.
#[must_use]
pub fn mail(store: &Store, id: MailId) -> Option<MailFull> {
    store.rows(&Q_MAIL, &[Val::I(id)], full_row).first().cloned()
}

/// The people who have written, most recent first.
#[must_use]
pub fn senders(store: &Store) -> Rc<Vec<Sender>> {
    store.rows(&Q_SENDERS, &[], sender_row)
}

/// Every account.
#[must_use]
pub fn accounts(store: &Store) -> Rc<Vec<Account>> {
    store.rows(&Q_ACCOUNTS, &[], account_row)
}

// -- the mailbox as a rich table ------------------------------------------------

/// A mailbox as a rich table of **threads**: the fixed parts of its query,
/// which the builder completes with the filter, the page and the rank. The
/// rows are the folder's messages grouped by conversation; what a row shows
/// is aggregates over them, or over the whole conversation (participants,
/// count, topic — trash left out), read by subquery.
///
/// The role is a literal rather than a bound parameter: a [`SqlSpec`] is
/// static text, which is what lets the same builder, the same rank and the
/// same page cache serve both lists without a string being formatted per
/// keystroke.
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
                ("from", TagSql::Col("(m.from_name || ' ' || m.from_email)")),
                ("subject", TagSql::Col("m.subject")),
                ("date", TagSql::Col("m.date")),
                ("account", TagSql::Col("a.email")),
            ],
            order: &[("last", Dir::Desc), ("thread", Dir::Desc)],
            group: Some("m.thread"),
            key: "thread",
            deps: &[],
        }
    };
}

static INBOX_SPEC: SqlSpec = mailbox_spec!("inbox");
static ARCHIVE_SPEC: SqlSpec = mailbox_spec!("archive");

const DATE_OPS: &[Op] = &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte];

/// A mailbox filter's tags: what `@` offers. Each reads against the folder's
/// messages, and a conversation matches when any of them does. One table for
/// both lists — the grammar of a mail list does not change with the folder it
/// is over.
static MAILBOX_TAGS: &[TagDef] = &[
    TagDef {
        name: "unread",
        kind: TagType::Bool,
        ops: &[],
        describe: "not read yet",
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
/// (most recently heard from first, by name or address) and accounts. Each is
/// one cached query; the match is a substring, so `kov` finds Vera.
fn suggest_mailbox(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
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

/// One role's datasource: what that mailbox panel's rich table runs on.
/// Everything but the spec is shared, because a row of the archive is
/// decoded, keyed and ranked exactly like a row of the inbox.
macro_rules! mailbox_source {
    ($spec:expr) => {
        SqlSource {
            spec: $spec,
            tags: MAILBOX_TAGS,
            map: thread_head_row,
            key: |t| t.thread,
            rank: |t| vec![Val::F(t.last), Val::I(t.thread)],
            suggest: suggest_mailbox,
        }
    };
}

static INBOX: SqlSource<ThreadHead, i64> = mailbox_source!(&INBOX_SPEC);
static ARCHIVE: SqlSource<ThreadHead, i64> = mailbox_source!(&ARCHIVE_SPEC);

/// The datasource a mailbox panel of this role pages through.
#[must_use]
pub fn threads(role: Role) -> &'static SqlSource<ThreadHead, i64> {
    match role {
        Role::Inbox => &INBOX,
        Role::Archive => &ARCHIVE,
    }
}

/// Rows per page of a mailbox table — what one scroll's worth of draws
/// fetches; the count is separate, so this is a batch size, not a limit.
pub const MAILBOX_PAGE: usize = 50;

/// One whole mailbox under a filter, materialized — for tests and the odd
/// one-shot read; panels page through [`threads`] instead.
#[must_use]
pub fn mailbox_filtered(store: &Store, role: Role, filter: &str) -> Vec<ThreadHead> {
    let mut t = Table::new(threads(role), MAILBOX_PAGE);
    t.set_filter(filter);
    let n = t.len(store);
    t.rows(store, 0, n)
}

// -- threads --------------------------------------------------------------------

/// The subject with its reply and forward prefixes stripped — what a
/// conversation is called, whichever of its mails you read it off.
#[must_use]
pub fn topic_of(subject: &str) -> String {
    const PREFIXES: &[&str] = &[
        "re", "fw", "fwd", "aw", "wg", "sv", "vs", "tr", "antw", "ref", "res", "rif", "odp", "ynt",
    ];
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let Some(colon) = lower.find(':') else { break };
        // `re[2]:` and `re (2):` count as `re`.
        let head = lower[..colon].split(['[', '(']).next().unwrap_or("").trim();
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
/// 3. **we name the same missing mail** — mails whose references share an id
///    with mine (two comments under an issue mail never received).
///
/// Plus a mail already here under my own id (my reply, in Sent and back
/// through a list). Every thread found merges into the lowest anchor; none
/// found, and the mail anchors itself.
///
/// # Errors
///
/// If the store refuses a write.
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
    let anchor = found
        .iter()
        .copied()
        .chain(std::iter::once(id))
        .min()
        .unwrap_or(id);
    c.execute(
        "UPDATE message SET thread = ?1 WHERE id = ?2",
        rusqlite::params![anchor, id],
    )?;
    if !found.is_empty() {
        let list = found
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
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
#[must_use]
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
#[must_use]
pub fn thread_topic(store: &Store, id: MailId) -> Option<String> {
    store
        .rows(&Q_THREAD_TOPIC, &[Val::I(id)], |r| r.get::<_, String>(0))
        .first()
        .cloned()
}

/// The anchor of the conversation a mail belongs to.
#[must_use]
pub fn thread_of(store: &Store, id: MailId) -> Option<i64> {
    store
        .rows(&Q_THREAD_OF, &[Val::I(id)], |r| r.get::<_, Option<i64>>(0))
        .first()
        .copied()
        .flatten()
}

/// Which of a conversation's mails are unread — what opening it marks.
#[must_use]
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

/// Which folder a mail sits in now — read before filing it, so undo puts it
/// back exactly there rather than guessing "the inbox".
#[must_use]
pub fn folder_of(store: &Store, id: MailId) -> i64 {
    store
        .conn()
        .query_row("SELECT folder FROM message WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap_or(0)
}

/// Which folder role a mail sits under — `inbox`, `archive`, `sent`,
/// `trash` — or `None` for a mail whose folder plays none.
#[must_use]
pub fn role_word_of(store: &Store, id: MailId) -> Option<String> {
    store
        .conn()
        .query_row(
            "SELECT f.role FROM message m JOIN folder f ON f.id = m.folder WHERE m.id = ?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
}

/// Which of a conversation's mails share this one's folder role — what
/// filing the row moves.
///
/// The row is the thread, and the mailbox it was read in decides which of its
/// mails the verb takes: archiving from the inbox takes the inbox copies,
/// deleting from the archive takes the archived ones. That mailbox is not
/// passed in — it is what the mail under the cursor is already filed as,
/// which is the same answer and cannot disagree with the list. A mail in no
/// listed role is alone: the set is just itself.
#[must_use]
pub fn thread_siblings(store: &Store, id: MailId) -> Vec<MailId> {
    let Some(role) = role_word_of(store, id) else {
        return vec![id];
    };
    store
        .rows(&Q_THREAD_MEMBERS, &[Val::I(id)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(2)?))
        })
        .iter()
        .filter(|(_, r)| *r == role)
        .map(|(id, _)| *id)
        .collect()
}

/// The row a mail's conversation makes in a mailbox of this role — the same
/// aggregates the table shows, for one thread — or `None` while none of it
/// sits in that folder.
#[must_use]
pub fn thread_head(store: &Store, role: Role, id: MailId) -> Option<ThreadHead> {
    let spec = match role {
        Role::Inbox => &INBOX_SPEC,
        Role::Archive => &ARCHIVE_SPEC,
    };
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

// -- how long a letter reads -----------------------------------------------------

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

/// What the author wrote — the reading a collapsed line previews and the
/// height wish measures.
#[must_use]
pub fn own_text(m: &MailFull) -> String {
    split_quote(&m.body).0
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

// -- local mutations -------------------------------------------------------------

/// Marks a mail read (opening it does this). A no-change update touches no
/// row — so it records nothing, and undoing the open of an already-read mail
/// correctly leaves it read. This writes **intent** only; the sync pass
/// pushes wherever intent and `server_msg` disagree.
///
/// # Errors
///
/// If the store refuses the write.
pub fn mark_read_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE message SET unread = 0 WHERE id = ?1 AND unread = 1",
        [id],
    )?;
    Ok(())
}

/// Moves a mail into one of its account's role folders. Intent only — the
/// push pass makes the server agree — and it is generic over the move, so
/// trash rides the same path archive already proved.
///
/// The `EXISTS` guard is load-bearing: an account whose server advertises no
/// such folder would otherwise get a `NULL` from the subquery, and a mail
/// with a null folder falls out of the mailbox queries *and* out of the push
/// set's join — vanishing silently, with nothing to sync it back.
///
/// The second guard is the honest no-op: filing a mail into the folder it
/// already sits in changes nothing, so it must *say* it changed nothing.
/// Answers whether the mail actually moved.
///
/// # Errors
///
/// If the store refuses the write.
pub fn file_tx(c: &rusqlite::Connection, id: MailId, role: &str) -> rusqlite::Result<bool> {
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

/// Whether this mail's account has the folder a triage would move it to. The
/// pre-flight for [`file_tx`]: the verb asks first so it can say *why*
/// nothing happened rather than record an empty action.
#[must_use]
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
/// action that moves nothing.
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

// -- drafts and the send window ---------------------------------------------------

/// What a compose panel started from: nothing, a mail it answers, or a mail
/// it passes on. The spelling of a `compose` panel's arguments is decided
/// here, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Seed {
    #[default]
    Blank,
    Reply(MailId),
    Forward(MailId),
}

impl Seed {
    /// The arguments a `compose` panel carries for this seed.
    #[must_use]
    pub fn args(self) -> Vec<String> {
        match self {
            Seed::Blank => Vec::new(),
            Seed::Reply(id) => vec!["reply".into(), id.to_string()],
            Seed::Forward(id) => vec!["forward".into(), id.to_string()],
        }
    }

    /// The seed a `compose` panel's arguments name. Anything unreadable is a
    /// blank sheet, which is the one seed that cannot be wrong.
    #[must_use]
    pub fn of_args(args: &[String]) -> Seed {
        let id = || args.get(1).and_then(|a| a.parse::<MailId>().ok());
        match (args.first().map(String::as_str), id()) {
            (Some("reply"), Some(id)) => Seed::Reply(id),
            (Some("forward"), Some(id)) => Seed::Forward(id),
            _ => Seed::Blank,
        }
    }

    /// The mail it answers, for the `In-Reply-To` header.
    #[must_use]
    pub fn in_reply_to(self) -> Option<MailId> {
        match self {
            Seed::Reply(id) => Some(id),
            _ => None,
        }
    }

    /// The mail it passes on.
    #[must_use]
    pub fn forwards(self) -> Option<MailId> {
        match self {
            Seed::Forward(id) => Some(id),
            _ => None,
        }
    }

    /// The mail it came from, either way.
    #[must_use]
    pub fn source(self) -> Option<MailId> {
        match self {
            Seed::Blank => None,
            Seed::Reply(id) | Seed::Forward(id) => Some(id),
        }
    }
}

/// The draft a fresh compose starts from, by its seed: a reply answers its
/// mail, a forward passes it on. Text the panel persisted wins over this —
/// the panel asks only when there is none.
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

/// A reply's body: room to write at the top, then the letter it answers under
/// the attribution line every client writes — `On <date>, <who> wrote:` —
/// with each of its lines behind a `>`. That is the shape [`split_quote`]
/// folds away on the way back in, so what this app sends is what it knows how
/// to read.
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
    if quote.is_empty() {
        head
    } else {
        format!("{head}\n{quote}")
    }
}

/// A forward's body: room to write at the top, then the mail under the header
/// block every client recognises — who wrote it, about what, when, to whom.
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

/// Who wrote a letter, as a reply's attribution and a forward's header block
/// name them: `Name <addr>`. A sender without a name is stored under their
/// address as the name; written out, that is the address once, not twice.
fn writer(m: &MailFull) -> String {
    let (name, email) = (&m.head.from_name, &m.head.from_email);
    if name.is_empty() || name == email {
        email.clone()
    } else {
        format!("{name} <{email}>")
    }
}

/// What a draft row answers and what it passes on — the seed it was saved
/// under, as `(re_message, fwd_message)`.
type DraftSeed = (Option<MailId>, Option<MailId>);

/// A slot's draft, if the row is `seed`'s own: what it answers and what it
/// passes on must match. A panel replaced in place keeps its slot, so a row a
/// reply left is not the forward's draft — that one seeds afresh.
#[must_use]
pub fn draft_for(store: &Store, slot: i64, seed: Seed) -> Option<Draft> {
    store
        .conn()
        .query_row(
            "SELECT to_addr, subject, body, re_message, fwd_message FROM draft WHERE panel = ?1",
            [slot],
            |r| {
                Ok((
                    Draft {
                        to: r.get(0)?,
                        subject: r.get(1)?,
                        body: r.get(2)?,
                    },
                    (r.get::<_, Option<MailId>>(3)?, r.get::<_, Option<MailId>>(4)?),
                ))
            },
        )
        .ok()
        .filter(|(_, s): &(Draft, DraftSeed)| *s == (seed.in_reply_to(), seed.forwards()))
        .map(|(d, _)| d)
}

/// The transaction-level draft upsert — also part of the send action, so the
/// row carries the final text. The seed's mail is recorded as what the draft
/// answers or what it passes on: the send reads its threading headers off
/// either.
///
/// # Errors
///
/// If the store refuses the write.
pub fn upsert_draft_tx(
    c: &rusqlite::Connection,
    slot: i64,
    seed: Seed,
    d: &Draft,
    now: f64,
) -> rusqlite::Result<()> {
    let account: Option<i64> = seed
        .source()
        .and_then(|id| {
            c.query_row("SELECT account FROM message WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .ok()
        })
        .or_else(|| {
            c.query_row(
                "SELECT id FROM account WHERE COALESCE(smtp_host,'') != '' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok()
        })
        .or_else(|| {
            c.query_row("SELECT id FROM account ORDER BY id LIMIT 1", [], |r| r.get(0))
                .ok()
        });
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
            slot,
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

/// Discard: the draft goes with the panel (both revert on undo).
///
/// # Errors
///
/// If the store refuses the write.
pub fn discard_draft_tx(c: &rusqlite::Connection, slot: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM draft WHERE panel = ?1", [slot])?;
    Ok(())
}

/// Files the outbox row for a send action — the row id is the slot id, so the
/// undo entity (`outbox:{slot}`) exists before the row does.
///
/// # Errors
///
/// If the store refuses the write.
pub fn file_send_tx(c: &rusqlite::Connection, slot: i64, send_after: f64) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO outbox(id, account, send_after, status, error)
         SELECT panel, COALESCE(account, 1), ?2, 'pending', NULL FROM draft WHERE panel = ?1",
        rusqlite::params![slot, send_after],
    )?;
    // A send filed again — a retry from the problems panel, or a redo after a
    // failure — must not be failed on sight by the job that failed *last*
    // time: the outbox pass derives a row's failure from any failed submit
    // for it, so those stand down first.
    c.execute(
        "UPDATE effect SET status = 'obsolete'
         WHERE kind = 'submit' AND status = 'failed' AND payload ->> 'outbox' = ?1",
        [slot],
    )?;
    Ok(())
}

/// Reopens a failed send as a draft on slot `new`: the draft row moves under
/// the new slot's id (a compose reads its draft by its own slot) and the
/// failed outbox row goes, so the problem is gone with it.
///
/// # Errors
///
/// If the store refuses the write.
pub fn reopen_send_tx(
    c: &rusqlite::Connection,
    old: i64,
    new: i64,
    now: f64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO draft(panel, account, re_message, fwd_message,
                                      to_addr, subject, body, updated)
         SELECT ?2, account, re_message, fwd_message, to_addr, subject, body, ?3
           FROM draft WHERE panel = ?1",
        rusqlite::params![old, new, now],
    )?;
    c.execute("DELETE FROM draft WHERE panel = ?1", [old])?;
    c.execute(
        "DELETE FROM outbox WHERE id = ?1 AND status = 'failed'",
        [old],
    )?;
    Ok(())
}

/// How long a filed send waits before the sender may take it, in seconds.
/// Mail's own knob, read from the environment — argv belongs to the shell.
#[must_use]
pub fn send_delay() -> f64 {
    std::env::var("SUPERAPP_SEND_DELAY")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|d| *d >= 0.0)
        .unwrap_or(10.0)
}

/// The credentials one account's session opens with. Both sites that open one
/// — the sync pass and [`Submit`](super::effects::Submit) — come through
/// here, so neither can drift.
///
/// # Errors
///
/// If the keychain has no password for the address.
pub fn creds_for(
    secrets: &mut dyn Secrets,
    email: &str,
    host: &str,
) -> Result<Creds, String> {
    let pass = secrets
        .get(email)
        .ok_or("no password in the keychain")?;
    Ok(Creds::password(host, email, pass))
}
