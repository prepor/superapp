//! Mail's rows, its queries, and the writes a verb composes into an action.
//!
//! Mailbox panels page through a rich table; everything else reads through a
//! registered [`Q`], so caching and provenance stay accurate. The `_tx`
//! functions are transaction-level pieces: an action runs them inside its one
//! transaction, and an [`Intent`](kernel::history::Intent) puts them back.

use std::rc::Rc;

use kernel::filter::Op;
use kernel::panel::{PanelId, Tag};
use kernel::richtable::{
    Dir, Sql, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, TextIndex, Values,
};
use kernel::store::{Q, Store, Val};
use kernel::time::fmt_date_long;

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
    /// The HTML reading, narrowed at ingest ([`html::sanitize`](super::html))
    /// to what the `Html` widget draws. A reader shows this where there is
    /// one and [`Self::body`] otherwise; a reply quotes the text either way.
    pub html: Option<String>,
    /// An optional status line; `true` marks it as an error.
    pub status: Option<(String, bool)>,
    /// The TO line: who the letter was addressed to, as addresses. A letter
    /// that arrived is addressed to the account it arrived at, and one in
    /// Sent to the person it went to — the second is the whole reason this
    /// is read off the letter rather than off the account. A letter whose
    /// bytes named nobody falls back to the account, which is what a letter
    /// that arrived says anyway.
    pub to: String,
    /// Passed on — the `$Forwarded` keyword, as this app or another client
    /// set it. The row draws a muted mark by the date.
    pub forwarded: bool,
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

// -- the mailbox roles ---------------------------------------------------------

/// Which mailbox a list panel is over. Five, and they are the five folders a
/// mail can be filed into and read back out of — the trash included, because
/// a delete you cannot see is a delete you cannot take back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Inbox,
    Archive,
    Sent,
    Spam,
    Trash,
}

/// Every role a mail list can show, in the order the launcher offers them:
/// the inbox first, then where mail goes when it leaves it.
pub const ROLES: [Role; 5] = [
    Role::Inbox,
    Role::Archive,
    Role::Sent,
    Role::Spam,
    Role::Trash,
];

impl Role {
    pub const INBOX: Tag = Tag("inbox");
    pub const ARCHIVE: Tag = Tag("archive");
    pub const SENT: Tag = Tag("sent");
    pub const SPAM: Tag = Tag("spam");
    pub const TRASH: Tag = Tag("trash");

    #[must_use]
    pub fn tag(self) -> Tag {
        match self {
            Role::Inbox => Role::INBOX,
            Role::Archive => Role::ARCHIVE,
            Role::Sent => Role::SENT,
            Role::Spam => Role::SPAM,
            Role::Trash => Role::TRASH,
        }
    }

    /// The word the `folder.role` column holds — the store's spelling, the
    /// panel's tag, and the panel's title, which are all one word by design:
    /// a mailbox is called what its folder is.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.tag().as_str()
    }

    /// The role a `folder.role` string names, or `None` for one no list shows
    /// — a folder role a later build learns to file, and `NULL`, which is
    /// every folder this app does not mirror. Not `FromStr`: this reads the
    /// store's own column, not a person's typing, and there is no error to
    /// report — only "no panel for that".
    #[must_use]
    pub fn named(s: &str) -> Option<Role> {
        ROLES.into_iter().find(|r| r.as_str() == s)
    }

    /// The identity of this mailbox's panel, unfiltered.
    #[must_use]
    pub fn id(self) -> PanelId {
        PanelId::bare(self.tag())
    }

    /// The same, narrowed to one correspondent: `("inbox", ["vera@kovac.io"])`,
    /// which is what a contact card's *messages from …* opens.
    ///
    /// The argument at position nought is a **sender**, and that is its whole
    /// meaning — the filter the panel comes up under is
    /// [`filter_expr`](Role::filter_expr) of it, in the list's own grammar,
    /// so what a person sees in the field is a filter they can edit into
    /// another one rather than a bare word that happens to match.
    #[must_use]
    pub fn filtered(self, sender: &str) -> PanelId {
        if sender.trim().is_empty() {
            return self.id();
        }
        PanelId::new(self.tag(), [sender])
    }

    /// The role a tag names; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<Role> {
        ROLES.into_iter().find(|r| r.tag() == id.tag)
    }

    /// The sender a mailbox panel was narrowed to, if it carries one. The
    /// field is seeded from it and is the person's from then on: the
    /// argument is the panel's identity, never a running copy of what has
    /// been typed.
    #[must_use]
    pub fn sender_of(id: &PanelId) -> Option<&str> {
        Role::of(id).and_then(|_| id.arg(0)).filter(|f| !f.is_empty())
    }

    /// What that sender is in the list's own filter grammar. Quoted where it
    /// has a space in it, which a display name may.
    #[must_use]
    pub fn filter_expr(sender: &str) -> String {
        if sender.contains(' ') {
            format!("@from:\"{sender}\"")
        } else {
            format!("@from:{sender}")
        }
    }
}

// -- queries -------------------------------------------------------------------

static Q_MAIL: Q = Q {
    id: "mail",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err,
                 COALESCE(NULLIF(m.to_addr, ''), a.email), m.html, m.forwarded
          FROM message m JOIN account a ON a.id = m.account
          WHERE m.id = ?1",
    describe: "one mail, both readings included, with the line it was addressed to",
};

static Q_MAIL_HEAD: Q = Q {
    id: "mail heading",
    sql: "SELECT id,from_name,from_email,subject,date,unread FROM message WHERE id=?1",
    describe: "one mail's heading without loading its bodies",
};

static Q_THREAD: Q = Q {
    id: "thread",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err,
                 COALESCE(NULLIF(m.to_addr, ''), a.email), m.html, m.forwarded,
                 COALESCE(f.role, ''), COALESCE(m.message_id, '')
          FROM message m JOIN account a ON a.id = m.account
                         JOIN folder f ON f.id = m.folder
          WHERE m.thread = (SELECT thread FROM message WHERE id = ?1)
            AND f.role IS NOT 'trash'
          ORDER BY m.date, m.id",
    describe: "the conversation a mail belongs to, oldest first, trash left out",
};

/// The same conversation with the trash in it — what a letter *read out of
/// the trash* is part of. A deleted letter is not gone, it is filed
/// somewhere a list now shows, so a reader opened on one draws the whole
/// conversation: the deleted letters beside the ones still filed.
static Q_THREAD_ALL: Q = Q {
    id: "thread with trash",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err, a.email, m.html, m.forwarded,
                 COALESCE(f.role, ''), COALESCE(m.message_id, '')
          FROM message m JOIN account a ON a.id = m.account
                         JOIN folder f ON f.id = m.folder
          WHERE m.thread = (SELECT thread FROM message WHERE id = ?1)
          ORDER BY m.date, m.id",
    describe: "the conversation a mail belongs to, oldest first, the trash included",
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

/// The same, with the trash in it — asked when the mail in hand is itself
/// deleted, which is the only time its siblings in the trash are what a verb
/// or a read mark is about.
static Q_THREAD_MEMBERS_ALL: Q = Q {
    id: "thread members with trash",
    sql: "SELECT m.id, m.unread, COALESCE(f.role, '')
          FROM message m JOIN folder f ON f.id = m.folder
          WHERE m.thread = (SELECT thread FROM message WHERE id = ?1)
          ORDER BY m.date, m.id",
    describe: "every mail of a conversation, the trash included, with its read flag and role",
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

/// One mail by id.
#[must_use]
pub fn mail(store: &Store, id: MailId) -> Option<MailFull> {
    store.rows(&Q_MAIL, &[Val::I(id)], full_row).first().cloned()
}

/// Small display metadata used by compose titles and attachment cards.
pub fn display_head(store: &Store, id: MailId) -> Option<MailHead> {
    store.snapshot_rows(&Q_MAIL_HEAD, &[Val::I(id)], head_row).first().cloned()
}

/// The stored content snapshot: MIME reading and remote file descriptors.
/// `None` for a letter the seed wrote by hand. Read off the connection;
/// drawings use the derived columns and leave content decoding to workers.
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
/// nothing the app offers to write to or to open a card for came out of the
/// junk folder.
#[must_use]
pub fn senders(store: &Store) -> Rc<Vec<Sender>> {
    store.rows(&Q_SENDERS, &[Val::I(0)], sender_row)
}

/// Display-only sender snapshots for recipient and mailbox autocomplete.
#[must_use]
pub fn display_senders(store: &Store, spam: bool) -> Rc<Vec<Sender>> {
    store.snapshot_rows(&Q_SENDERS, &[Val::I(i64::from(spam))], sender_row)
}

// -- the mailbox as a rich table ------------------------------------------------

/// A mailbox as a rich table of **threads**: the fixed parts of its query,
/// which the builder completes with the filter, the page and the rank. The
/// rows are the folder's messages grouped by conversation; what a row shows
/// is aggregates over them, or over the whole conversation (participants,
/// count, topic — trash left out), read by subquery.
///
/// `$who` and `$text` are what differ by mailbox: three lists are of letters
/// that came, so a row names who wrote them and free text matches the sender;
/// Sent is the list of letters that left, so both are about the other end —
/// a list searches what it shows.
///
/// The role is a literal rather than a bound parameter: a [`SqlSpec`] is
/// static text, which is what lets the same builder, the same rank and the
/// same page cache serve five lists without a string being formatted per
/// keystroke. `concat!` writes the five out at compile time.
///
/// `$of` is which side of the trash a row's count is over. Four lists leave
/// the trash out, so how long a conversation is is what is left of it; the
/// trash counts the deleted letters alone, because a row there is what was
/// thrown away and would otherwise say *0*.
macro_rules! mailbox_spec {
    ($role:literal, $who:expr, $text:expr, $of:literal) => {
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
                 ",
                $who,
                " AS who,
                 (SELECT COALESCE(t.topic, t.subject) FROM message t
                   WHERE t.thread = m.thread ORDER BY t.date, t.id LIMIT 1) AS topic,
                 (SELECT COUNT(DISTINCT COALESCE(NULLIF(t.message_id, ''), 'id:' || t.id))
                   FROM message t JOIN folder tf ON tf.id = t.folder
                   WHERE t.thread = m.thread AND tf.role ",
                $of,
                " 'trash') AS n"
            ),
            from: "message m JOIN folder f ON m.folder = f.id JOIN account a ON a.id = m.account",
            base: concat!("f.role = '", $role, "'"),
            text: $text,
            // The letters themselves, through the index that already holds
            // their words. A `LIKE` over `m.body` would read every body in
            // the folder per keystroke — a mailbox of twenty thousand
            // letters is a couple of hundred megabytes, and it showed:
            // a fifth of a second a count, and a count and a page run for
            // every letter typed. This is the same words asked of the same
            // index the launcher asks ([`fts_match`]), and it costs what an
            // index costs.
            index: Some(TextIndex {
                build: |text| Some(Sql {
                    sql: "m.id IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?)".into(),
                    params: vec![Val::S(fts_match(text)?)],
                }),
            }),
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
            deps: &[],
        }
    };
}

/// A mailbox of letters that **arrived**: a row names everyone who wrote in
/// the conversation, newest speaker first, `me` for the account's own
/// address.
///
/// `$of` is which side of the trash that name list counts, and it is the
/// same word the row's own count uses: everywhere but the trash it is the
/// whole conversation, trash aside — a reply of mine is part of who is in it
/// — and in the trash it is the deleted letters alone, which is what a row
/// there stands for.
macro_rules! arrived_spec {
    ($role:literal, $of:literal) => {
        mailbox_spec!(
            $role,
            concat!(
                "(SELECT GROUP_CONCAT(
                      CASE WHEN t.from_email = ta.email THEN 'me'
                           WHEN t.from_name = '' THEN t.from_email
                           ELSE t.from_name END, char(31) ORDER BY t.date DESC)
                    FROM message t JOIN folder tf ON tf.id = t.folder
                                   JOIN account ta ON ta.id = t.account
                   WHERE t.thread = m.thread AND tf.role ",
                $of,
                " 'trash')"
            ),
            &["m.from_name", "m.from_email", "m.subject"],
            $of
        )
    };
}

static INBOX_SPEC: SqlSpec = arrived_spec!("inbox", "IS NOT");
static ARCHIVE_SPEC: SqlSpec = arrived_spec!("archive", "IS NOT");
static SPAM_SPEC: SqlSpec = arrived_spec!("spam", "IS NOT");
static TRASH_SPEC: SqlSpec = arrived_spec!("trash", "IS");

/// Sent is the mailbox of letters that **left**, and a row names who they
/// went to: their sender is always me, so the TO line is the only thing on
/// them worth a name. The conversation's own sent letters, newest first —
/// what the person wrote, not what came back.
///
/// A `To` line is itself a list, so its commas become the separator the
/// concatenation already uses and each address arrives as a name of its own:
/// two letters to one person are one name on the row, as two letters *from*
/// one are.
static SENT_SPEC: SqlSpec = mailbox_spec!(
    "sent",
    "(SELECT GROUP_CONCAT(REPLACE(t.to_addr, ', ', char(31)), char(31)
                          ORDER BY t.date DESC)
        FROM message t JOIN folder tf ON tf.id = t.folder
       WHERE t.thread = m.thread AND tf.role = 'sent' AND t.to_addr != '')",
    // …and free text over the same line, because a list is searched by what
    // it shows: the sender of everything here is the account itself.
    &["m.to_addr", "m.subject"],
    "IS NOT"
);

/// The spec one role's list runs on.
fn spec_of(role: Role) -> &'static SqlSpec {
    match role {
        Role::Inbox => &INBOX_SPEC,
        Role::Archive => &ARCHIVE_SPEC,
        Role::Sent => &SENT_SPEC,
        Role::Spam => &SPAM_SPEC,
        Role::Trash => &TRASH_SPEC,
    }
}

const DATE_OPS: &[Op] = &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte];

/// A mailbox filter's tags: what `@` offers. Each reads against the folder's
/// messages, and a conversation matches when any of them does. One table for
/// all four lists — the grammar of a mail list does not change with the
/// folder it is over.
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
/// (most recently heard from first, by name or address) and accounts. Each is
/// one cached query; the match is a substring, so `kov` finds Vera.
///
/// `spam` picks which side of the line the senders come from — a list
/// completes against the people who wrote *to it*.
fn suggest_mailbox(store: &Store, spam: bool, tag: &str, typed: &str) -> Vec<Suggestion> {
    match tag {
        "from" => display_senders(store, spam)
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(typed) || s.email.to_lowercase().contains(typed)
            })
            .take(kernel::richtable::MAX_SUGGESTIONS)
            .map(|s| {
                if s.name.is_empty() {
                    Suggestion::value(s.email.clone())
                } else {
                    Suggestion::labeled(s.name.clone(), s.email.clone())
                }
            })
            .collect(),
        "account" => super::accounts::accounts(store)
            .iter()
            .filter(|a| a.email.to_lowercase().contains(typed))
            .map(|a| Suggestion::value(a.email.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The `@from:` completion of a list over correspondents, and of the one over
/// spam. Named functions rather than a role passed along: the field is a
/// plain `fn` pointer, and these are the only two answers.
fn suggest_correspondents(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    suggest_mailbox(store, false, tag, typed)
}

fn suggest_spam(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    suggest_mailbox(store, true, tag, typed)
}

/// One role's datasource: what that mailbox panel's rich table runs on. Five
/// values of one shape — everything but the spec is shared, because a row of
/// the sent folder is decoded, keyed and ranked exactly like a row of the
/// inbox.
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
static TRASH: SqlSource<ThreadHead, i64> = mailbox_source!(&TRASH_SPEC, suggest_correspondents);

/// The datasource a mailbox panel of this role pages through.
#[must_use]
pub fn threads(role: Role) -> &'static SqlSource<ThreadHead, i64> {
    match role {
        Role::Inbox => &INBOX,
        Role::Archive => &ARCHIVE,
        Role::Sent => &SENT,
        Role::Spam => &SPAM,
        Role::Trash => &TRASH,
    }
}

/// Rows per page of a mailbox table — what one scroll's worth of draws
/// fetches; the count is separate, so this is a batch size, not a limit.
pub const MAILBOX_PAGE: usize = 50;

/// One whole mailbox under a filter, materialized — what a test reads;
/// panels page through [`threads`] instead.
#[cfg(test)]
#[must_use]
pub fn mailbox_filtered(store: &Store, role: Role, filter: &str) -> Vec<ThreadHead> {
    let mut t = kernel::richtable::Table::new(threads(role), MAILBOX_PAGE);
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
            // A header that names one id twice is one row: the pair is the
            // table's key.
            c.execute(
                "INSERT OR IGNORE INTO reference(message, mid) VALUES(?1, ?2)",
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
    let q = if is_trashed(store, id) {
        &Q_THREAD_ALL
    } else {
        &Q_THREAD
    };
    let rows = store.rows(q, &[Val::I(id)], thread_row);
    let mut out: Vec<ThreadMail> = Vec::with_capacity(rows.len());
    let mut positions: std::collections::HashMap<String, usize> = std::collections::HashMap::with_capacity(rows.len());
    for m in rows.iter() {
        if !m.message_id.is_empty() {
            if let Some(&i) = positions.get(&m.message_id) {
                if stands_for(&m.role, &out[i].role) {
                    out[i] = m.clone();
                }
                continue;
            }
            positions.insert(m.message_id.clone(), out.len());
        }
        out.push(m.clone());
    }
    out
}

/// Which of two copies of one letter — the same `Message-ID` in two folders
/// — the conversation shows, and so which one a verb over the conversation
/// acts on.
///
/// A deleted copy never stands for a filed one, whichever arrives first: I
/// deleted the copy that came back through the list, not the letter. Between
/// the rest the copy outside Sent wins, so my own reply reads in the folder
/// it came back to. Only a conversation read out of the trash has a deleted
/// copy to weigh at all.
fn stands_for(role: &str, over: &str) -> bool {
    fn rank(role: &str) -> u8 {
        match role {
            "trash" => 0,
            "sent" => 1,
            _ => 2,
        }
    }
    rank(role) > rank(over)
}

/// A conversation's subject, off its oldest mail, reply prefixes stripped.
#[must_use]
pub fn thread_topic(store: &Store, id: MailId) -> Option<String> {
    store
        .rows(&Q_THREAD_TOPIC, &[Val::I(id)], |r| r.get::<_, String>(0))
        .first()
        .cloned()
}

/// Which of a conversation's mails are unread — what opening it marks.
#[must_use]
pub fn thread_unread(store: &Store, id: MailId) -> Vec<MailId> {
    store
        .rows(members_q(store, id), &[Val::I(id)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
        })
        .iter()
        .filter(|(_, unread)| *unread)
        .map(|(id, _)| *id)
        .collect()
}

/// Original unread flags captured inside the same transaction that marks them.
pub fn thread_unread_in(conn: &rusqlite::Connection, id: MailId) -> rusqlite::Result<Vec<MailId>> {
    conn.prepare_cached(
        "SELECT m.id FROM message m JOIN folder f ON f.id=m.folder
         WHERE m.thread=(SELECT thread FROM message WHERE id=?1) AND m.unread!=0
           AND (f.role IS NOT 'trash' OR EXISTS(
             SELECT 1 FROM message a JOIN folder af ON af.id=a.folder WHERE a.id=?1 AND af.role='trash'))
         ORDER BY m.date,m.id",
    )?.query_map([id], |row| row.get(0))?.collect()
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

/// Which folder role a mail sits under, as the store spells it — `inbox`,
/// `archive`, `sent`, `spam`, `trash` — or `None` for a mail whose folder
/// plays none.
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

/// The same, as a mailbox: `None` for a mail no list shows — one in a folder
/// this build files nothing into.
#[must_use]
pub fn role_of(store: &Store, id: MailId) -> Option<Role> {
    role_word_of(store, id).as_deref().and_then(Role::named)
}

/// Whether this mail is in the trash. What it decides is how its
/// conversation reads: everywhere else the trash is left out, and a letter
/// already in it is read with its deleted siblings.
#[must_use]
pub fn is_trashed(store: &Store, id: MailId) -> bool {
    role_word_of(store, id).as_deref() == Some(Role::TRASH.as_str())
}

/// Which members query a conversation is asked with — see [`is_trashed`].
fn members_q(store: &Store, id: MailId) -> &'static Q {
    if is_trashed(store, id) {
        &Q_THREAD_MEMBERS_ALL
    } else {
        &Q_THREAD_MEMBERS
    }
}

/// Which of a conversation's mails share this one's folder role — what
/// filing the row moves.
///
/// The row is the thread, and the mailbox it was read in decides which of its
/// mails the verb takes: archiving from the inbox takes the inbox copies,
/// deleting from the archive takes the archived ones. That mailbox is not
/// passed in — it is what the mail under the cursor is already filed as,
/// which is the same answer and cannot disagree with the list. A mail in no
/// listed role — one in a folder this build files nothing into — is alone:
/// the set is just itself.
#[must_use]
pub fn thread_siblings(store: &Store, id: MailId) -> Vec<MailId> {
    let Some(role) = role_of(store, id) else {
        return vec![id];
    };
    store
        .rows(members_q(store, id), &[Val::I(id)], |r| {
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
#[must_use]
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
    // Where it stood, read before the move: a delete is the one filing whose
    // origin has to outlive the session that made it.
    let was: Option<i64> = c
        .query_row("SELECT folder FROM message WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .ok();
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
    if n > 0 {
        match (role == Role::TRASH.as_str(), was) {
            (true, Some(was)) => {
                c.execute(
                    "INSERT OR REPLACE INTO trashed(message, folder) VALUES(?1, ?2)",
                    rusqlite::params![id, was],
                )?;
            }
            // Out of the trash by any other road — archived from a reader,
            // undone, moved by a tool. The letter is filed again, so where
            // it was deleted from is no longer a fact about it.
            _ => {
                c.execute("DELETE FROM trashed WHERE message = ?1", [id])?;
            }
        }
    }
    Ok(n > 0)
}

/// Where a deleted letter was filed before it was deleted, if that is
/// written down. `None` for one that arrived in the trash from the server —
/// deleted on another device, mirrored here — which never passed through a
/// filing of ours.
#[must_use]
pub fn trashed_from(store: &Store, id: MailId) -> Option<i64> {
    store
        .conn()
        .query_row("SELECT folder FROM trashed WHERE message = ?1", [id], |r| {
            r.get(0)
        })
        .ok()
}

/// The folder a *put back* sends a letter to: where it was deleted from,
/// else the account's inbox. Both halves are joined against `folder`, so a
/// row pointing at a folder a resync dropped falls through to the inbox
/// rather than moving the letter nowhere.
const Q_PUT_BACK_TARGET: &str = "SELECT COALESCE(
     (SELECT t.folder FROM trashed t JOIN folder f ON f.id = t.folder
       WHERE t.message = ?1),
     (SELECT f.id FROM folder f JOIN message m ON m.account = f.account
       WHERE m.id = ?1 AND f.role = 'inbox'))";

/// Where a *put back* would send this letter — [`Q_PUT_BACK_TARGET`].
/// `None` when there is nowhere: the account has no inbox row either, which
/// is a mailbox this app never mirrored.
#[must_use]
pub fn put_back_target(store: &Store, id: MailId) -> Option<i64> {
    put_back_target_in(store.conn(), id)
}

pub(crate) fn put_back_target_in(c: &rusqlite::Connection, id: MailId) -> Option<i64> {
    c.query_row(Q_PUT_BACK_TARGET, [id], |r| r.get::<_, Option<i64>>(0))
        .ok()
        .flatten()
}

/// Puts a letter back where it was deleted from, and the row that remembered
/// it goes with the move. Answers whether anything moved.
///
/// # Errors
///
/// If the store refuses the write.
pub fn put_back_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<bool> {
    let Some(to) = put_back_target_in(c, id) else {
        return Ok(false);
    };
    let n = c.execute(
        "UPDATE message SET folder = ?2 WHERE id = ?1 AND folder IS NOT ?2",
        rusqlite::params![id, to],
    )?;
    c.execute("DELETE FROM trashed WHERE message = ?1", [id])?;
    Ok(n > 0)
}

/// Puts a letter back in the trash and writes down where it came from — what
/// undoing a *put back* does, and nothing else: the row it restores is the
/// one the put back consumed.
///
/// # Errors
///
/// If the store refuses the write.
pub fn retrash_tx(
    c: &rusqlite::Connection,
    id: MailId,
    trash: i64,
    from: i64,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE message SET folder = ?2 WHERE id = ?1",
        rusqlite::params![id, trash],
    )?;
    c.execute(
        "INSERT OR REPLACE INTO trashed(message, folder) VALUES(?1, ?2)",
        rusqlite::params![id, from],
    )?;
    Ok(())
}

/// What a letter's `trashed` row should be once a move has been reversed:
/// the one it had before the move, or none at all. The folder half of undo,
/// which writes `message.folder` back directly rather than filing it.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_trashed_tx(
    c: &rusqlite::Connection,
    id: MailId,
    was: Option<i64>,
) -> rusqlite::Result<()> {
    match was {
        Some(folder) => c.execute(
            "INSERT OR REPLACE INTO trashed(message, folder) VALUES(?1, ?2)",
            rusqlite::params![id, folder],
        ),
        None => c.execute("DELETE FROM trashed WHERE message = ?1", [id]),
    }?;
    Ok(())
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

/// Who a reply is addressed to: whoever wrote the letter — unless that is an
/// account of this store's own, in which case answering means writing to the
/// people it went to rather than to oneself. A reply out of Sent is a second
/// letter to the same person, which is what every client makes of one.
///
/// The whole TO line, as the letter wrote it: a letter to three people is
/// answered to three people, and the field takes the list it takes.
#[must_use]
fn reply_to(store: &Store, m: &MailFull) -> String {
    if super::accounts::account_for(store, &m.head.from_email).is_some() {
        return m.to.clone();
    }
    m.head.from_email.clone()
}

/// The draft a fresh compose starts from, by its seed: a reply answers its
/// mail, a forward passes it on. Text the panel persisted wins over this —
/// the panel asks only when there is none.
#[must_use]
pub fn seed_draft(store: &Store, seed: Seed) -> Draft {
    match seed {
        Seed::Blank => Draft::default(),
        Seed::Reply(id) => mail(store, id).map_or_else(Draft::default, |m| Draft {
            to: reply_to(store, &m),
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
/// with each of its lines behind a `>`. That is the shape
/// [`split_quote`](super::reading::split_quote) folds away on the way back
/// in, so what this app sends is what it knows how to read.
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

/// A slot's draft whatever it answers, with the seed the row itself names —
/// what a reopened send adopts, since the letter that failed already knows
/// which mail it was written against.
#[must_use]
#[cfg(test)]
pub fn draft_any(store: &Store, slot: i64) -> Option<(Draft, Seed)> {
    draft_any_in(store.conn(), slot).ok().flatten()
}

pub fn draft_any_in(conn: &rusqlite::Connection, slot: i64) -> rusqlite::Result<Option<(Draft, Seed)>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
            "SELECT to_addr, subject, body, re_message, fwd_message FROM draft WHERE panel = ?1",
            [slot],
            |r| {
                let d = Draft {
                    to: r.get(0)?,
                    subject: r.get(1)?,
                    body: r.get(2)?,
                };
                let seed = match (
                    r.get::<_, Option<MailId>>(3)?,
                    r.get::<_, Option<MailId>>(4)?,
                ) {
                    (Some(id), _) => Seed::Reply(id),
                    (None, Some(id)) => Seed::Forward(id),
                    (None, None) => Seed::Blank,
                };
                Ok((d, seed))
            },
        )
        .optional()
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
                "SELECT id FROM account WHERE mail_enabled=1 AND COALESCE(smtp_host,'') != '' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok()
        })
        .or_else(|| {
            c.query_row("SELECT id FROM account WHERE mail_enabled=1 ORDER BY id LIMIT 1", [], |r| r.get(0))
                .ok()
        });
    // A compose retargeted in place keeps its slot, so the files a reply left
    // are not the forward's: `carry::files` already holds them to the seed,
    // and this is where they go for good.
    let was: Option<DraftSeed> = c
        .query_row(
            "SELECT re_message, fwd_message FROM draft WHERE panel = ?1",
            [slot],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    if was.is_some_and(|s| s != (seed.in_reply_to(), seed.forwards())) {
        super::carry::discard_tx(c, slot)?;
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

/// Discard: the draft goes with the panel, and what it was going to carry
/// goes with the draft (all of it reverts on undo).
///
/// # Errors
///
/// If the store refuses the write.
pub fn discard_draft_tx(c: &rusqlite::Connection, slot: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM draft WHERE panel = ?1", [slot])?;
    super::carry::discard_tx(c, slot)
}

/// Files the outbox row for a send action — the row id is the slot id, so the
/// undo entity (`outbox:{slot}`) exists before the row does.
///
/// # Errors
///
/// If the store refuses the write.
pub fn file_send_tx(c: &rusqlite::Connection, slot: i64, send_after: f64) -> rusqlite::Result<()> {
    if !c.prepare("SELECT 1 FROM draft d JOIN account a ON a.id=d.account WHERE d.panel=? AND a.mail_enabled=1")?.exists([slot])? {
        return Err(rusqlite::Error::InvalidParameterName("enable Mail for this account in Accounts before sending".into()));
    }
    c.execute(
        "INSERT OR REPLACE INTO outbox(id, account, send_after, status, error)
         SELECT panel, account, ?2, 'pending', NULL FROM draft WHERE panel = ?1",
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

/// The executor finishes a submit after the sender's reconciliation pass.
/// Its terminal result is authoritative even while the outbox says sending.
fn failed_send_tx(c: &rusqlite::Connection, slot: i64) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbox o WHERE o.id = ?1
           AND (o.status = 'failed' OR (o.status = 'sending' AND
             (SELECT e.status FROM effect e
              WHERE e.kind = 'submit' AND e.status != 'obsolete'
                AND e.payload ->> 'outbox' = o.id
              ORDER BY e.id DESC LIMIT 1) = 'failed')))",
        [slot],
        |r| r.get(0),
    )
}

/// Refiles a failed send, checking the current state in the same transaction
/// so a stale retry button cannot replace an already active send.
///
/// # Errors
///
/// If the store refuses the write or the account can no longer send mail.
pub fn retry_send_tx(c: &rusqlite::Connection, slot: i64, send_after: f64) -> rusqlite::Result<bool> {
    if !failed_send_tx(c, slot)? {
        return Ok(false);
    }
    file_send_tx(c, slot, send_after)?;
    Ok(true)
}

/// Reopens a failed send as a draft on slot `new`: the draft row moves under
/// the new slot's id (a compose reads its draft by its own slot) and the
/// failed outbox row goes, so the problem is gone with it. Reversed by
/// [`Reopened`](super::effects::Reopened).
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
    if !failed_send_tx(c, old)? {
        // Restoring an already adopted compose is harmless. A stale problem
        // action must not take the draft away from a send now in progress.
        if c.prepare("SELECT 1 FROM outbox WHERE id = ?1")?.exists([old])? {
            return Err(rusqlite::Error::InvalidParameterName(
                "this send is no longer failed".into(),
            ));
        }
        return Ok(());
    }
    move_draft_tx(c, old, new, now)?;
    c.execute("DELETE FROM outbox WHERE id = ?1", [old])?;
    Ok(())
}

/// Re-keys a draft from one slot to another, keeping its text and its
/// account.
///
/// # Errors
///
/// If the store refuses the write.
pub fn move_draft_tx(
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
    // What it was going to carry moves with it — a reopened send keeps its
    // files, or the letter that goes out the second time is not the letter
    // that failed.
    c.execute(
        "UPDATE OR REPLACE draft_attachment SET panel = ?2 WHERE panel = ?1",
        rusqlite::params![from, to],
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

/// The query as FTS5 reads it: every word its own quoted prefix term, all of
/// them required.
///
/// The quoting is the point. A person types `vera@kovac.io` or `re: q3` or a
/// bare `*`, and none of it may be mistaken for the match language — so the
/// words are cut out on non-alphanumeric boundaries (which is also how
/// `unicode61` tokenizes, Cyrillic included) and put back quoted, where no
/// operator can survive. The trailing `*` is what makes it type-ahead: "ver
/// kov" finds Vera Kovac on the fourth keystroke.
///
/// `None` when there is no word in it at all — the empty launcher asks
/// nothing of the mail world.
#[must_use]
pub fn fts_match(query: &str) -> Option<String> {
    let mut out = String::new();
    // The kernel's own cutting, so the index is asked for exactly what the
    // in-memory sources match on.
    for w in kernel::search::terms(query) {
        if !out.is_empty() {
            out.push_str(" AND ");
        }
        out.push('"');
        out.push_str(&w); // no quote can be in it: it was cut on non-alphanumerics
        out.push_str("\"*");
    }
    (!out.is_empty()).then_some(out)
}
