//! The demo world: one account, four folders, and the letters in them.
//!
//! The same list fills two things — the store, through [`seed_if_empty`], and
//! the fake server, through [`FakeServers::demo`](super::caps::FakeServers::demo)
//! — in the same order, so the uids agree and the first sync pass is the
//! no-op a mirror of an agreeing server should be.

use kernel::store::Store;
use kernel::time::{civil_from_days, ts, virtual_epoch};

use super::model::{thread_tx, topic_of};

/// The demo account's row id in a fresh store.
pub const ACCOUNT: i64 = 1;
/// Whose mailbox this is.
pub const ADDRESS: &str = "me@prepor.dev";
/// What it signs in with. In the keychain, never in the store.
pub const PASSWORD: &str = "demo-app-password";
pub const IMAP_HOST: &str = "imap.demo";
pub const SMTP_HOST: &str = "smtp.demo";

/// The folders the account has, `(name, role)`. The names are the server's,
/// because that is what a folder row is keyed by.
pub const FOLDERS: &[(&str, &str)] = &[
    ("INBOX", "inbox"),
    ("Archive", "archive"),
    ("Sent", "sent"),
    ("Trash", "trash"),
];

/// The folder name a role is played by.
#[must_use]
pub fn folder_name(role: &str) -> &'static str {
    FOLDERS
        .iter()
        .find(|(_, r)| *r == role)
        .map_or("INBOX", |(n, _)| *n)
}

/// One demo letter.
pub struct SeedMail {
    pub from_name: &'static str,
    pub from_email: &'static str,
    pub subject: &'static str,
    pub date: f64,
    pub unread: bool,
    /// Paragraphs, `\n\n`-separated. Plain text only: the prototype draws no
    /// HTML.
    pub body: &'static str,
    /// An optional status line; `true` marks it as an error.
    pub status: Option<(&'static str, bool)>,
    /// The role of the folder it sits in.
    pub folder: &'static str,
    /// Message-ID and what it references — the threading headers; empty for
    /// a mail that stands alone.
    pub mid: &'static str,
    pub refs: &'static [&'static str],
}

/// The demo world's letters, in the order they are filed: the hand-written
/// inbox first, then the conversation.
#[must_use]
pub fn mails() -> Vec<SeedMail> {
    let mut v = base_mails();
    v.extend(thread_mails());
    v
}

/// The hand-written demo mail, newest first — ids land as 1..=9 in a fresh
/// store, which the tests rely on.
fn base_mails() -> Vec<SeedMail> {
    vec![
        SeedMail {
            from_name: "Vera Kovac",
            from_email: "vera@kovac.io",
            subject: "Q3 infra budget draft",
            date: ts(2026, 8, 31, 9, 14),
            unread: true,
            body: "Draft for Q3 infra spend is ready. Main deltas: the old staging cluster goes away and CI runners move to the new box.\n\nCan you sanity-check the numbers before Thursday? Especially egress — I suspect the CDN line is stale.",
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
                   Anyway — this letter is its own test case. If it opens in three rows you have proven my point; if it opens tall, yours.",
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
        },
    ]
}

/// The demo world's conversation, appended after the filler so the first
/// nine ids stay what every test expects: the note Max was replying to
/// (mine, in Sent) and his second reply, which folds into his first one's
/// inbox row.
fn thread_mails() -> Vec<SeedMail> {
    vec![
        SeedMail {
            from_name: "Andrey Rudenko",
            from_email: ADDRESS,
            subject: "superapp panel model",
            date: ts(2026, 8, 29, 14, 2),
            unread: false,
            body: "Wrote up the panel model: joined panels, replace in place, the chain closing behind a replacement. Curious what you make of the join rule in particular.\n\nThe draft is in the shared folder — comments welcome before Monday.",
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
            status: None,
            folder: "inbox",
            mid: "pm-2@ivanov.dev",
            refs: &["pm-0@prepor.dev", "pm-1@ivanov.dev"],
        },
    ]
}

// -- the wire form -------------------------------------------------------------

/// A timestamp as a header writes it: `2026-08-31 09:14`.
///
/// Not RFC 5322's spelling. The prototype's parser reads what its own fake
/// server writes, and the kernel already knows how to read this one
/// ([`date_span`](kernel::richtable::date_span)); a real transport brings a
/// real date parser with it.
#[must_use]
pub fn header_date(at: f64) -> String {
    let secs = at as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let (h, min) = (rem / 3_600, (rem % 3_600) / 60);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}")
}

/// The date a mail the fake submission server formats wears. It has no clock
/// of its own, so it wears the instant a scripted run believes it is.
#[must_use]
pub fn sent_date() -> String {
    header_date(virtual_epoch())
}

/// One seeded letter as the bytes it would have arrived as.
#[must_use]
pub fn rfc822(m: &SeedMail) -> String {
    let mut raw = format!(
        "From: {} <{}>\r\nTo: {ADDRESS}\r\nSubject: {}\r\nDate: {}\r\n",
        m.from_name,
        m.from_email,
        m.subject,
        header_date(m.date)
    );
    if !m.mid.is_empty() {
        raw += &format!("Message-ID: <{}>\r\n", m.mid);
    }
    if !m.refs.is_empty() {
        let refs: Vec<String> = m.refs.iter().map(|r| format!("<{r}>")).collect();
        raw += &format!("References: {}\r\n", refs.join(" "));
    }
    raw += &format!("\r\n{}\r\n", m.body.replace('\n', "\r\n"));
    raw
}

// -- the store's half ----------------------------------------------------------

/// Seeds the demo account and mail into an empty store. A store with any
/// mail is left alone, so a crash between the seed and its record repeats
/// nothing.
///
/// Every row goes through the ingest path's own threading, and every one
/// gets a `server_msg` row with the uid the fake server gave it — so the
/// account is a mirror of a server rather than a pile of rows nothing can
/// push.
///
/// # Errors
///
/// If the store refuses the write.
pub fn seed_if_empty(store: &Store) -> rusqlite::Result<()> {
    let n: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    store.write(|c| {
        c.execute(
            "INSERT INTO account(id, label, email, imap_host, smtp_host)
             VALUES(?1, 'demo', ?2, ?3, ?4)",
            rusqlite::params![ACCOUNT, ADDRESS, IMAP_HOST, SMTP_HOST],
        )?;
        let mut folders: Vec<(&str, &str, i64)> = Vec::new();
        for (name, role) in FOLDERS {
            c.execute(
                "INSERT INTO folder(account, name, role, uidvalidity, uidnext)
                 VALUES(?1, ?2, ?3, 1, 1)",
                rusqlite::params![ACCOUNT, name, role],
            )?;
            folders.push((name, role, c.last_insert_rowid()));
        }
        for m in &mails() {
            let (_, _, fid) = *folders
                .iter()
                .find(|(_, role, _)| *role == m.folder)
                .expect("a seeded mail names a seeded folder");
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email, subject,
                                     date, unread, body, status, status_err,
                                     message_id, topic)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                rusqlite::params![
                    ACCOUNT,
                    fid,
                    m.from_name,
                    m.from_email,
                    m.subject,
                    m.date,
                    m.unread,
                    m.body,
                    m.status.map(|(s, _)| s),
                    m.status.is_some_and(|(_, e)| e),
                    (!m.mid.is_empty()).then_some(m.mid),
                    topic_of(m.subject),
                ],
            )?;
            let id = c.last_insert_rowid();
            let refs: Vec<String> = m.refs.iter().map(|r| (*r).to_string()).collect();
            thread_tx(c, ACCOUNT, id, m.mid, &refs)?;
            // The uid the fake server handed the same letter: one per
            // folder, in this order, from one.
            let uid: i64 = c.query_row(
                "SELECT uidnext FROM folder WHERE id = ?1",
                [fid],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO server_msg(message, folder, uid, seen) VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![id, fid, uid, !m.unread],
            )?;
            c.execute(
                "UPDATE folder SET uidnext = uidnext + 1 WHERE id = ?1",
                [fid],
            )?;
        }
        Ok(())
    })
}
