//! The mail domain over the store: typed queries, titles, the demo seed,
//! and the local mutations (read flags, archive).
//!
//! Everything panels show comes through the registered [`Q`] queries — that
//! is the reactive contract (see [`crate::store`]) and, later, the panel
//! context an agent receives. Filtering keeps the shell's semantics: the
//! typed filter is one lowercase substring over sender + subject; the
//! launcher's word-AND lives in [`crate::launcher`].

use std::rc::Rc;

use crate::core::{Kind, MailId};
use crate::store::{Q, Store, Val};

/// One list row: what the inbox and the launcher show.
#[derive(Debug, Clone)]
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
    /// An optional status line; `true` marks it as an error.
    pub status: Option<(String, bool)>,
    /// The receiving account's address (the TO line).
    pub to: String,
}

/// A distinct sender: the launcher's contact entries.
#[derive(Debug, Clone)]
pub struct Sender {
    pub email: String,
    pub name: String,
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
                 m.body, m.status, m.status_err, a.email
          FROM message m JOIN account a ON a.id = m.account
          WHERE m.id = ?1",
    describe: "one mail, body included, with its account's address",
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
    sql: "SELECT id, label, email, imap_host, smtp_host, status, synced
          FROM account ORDER BY id",
    describe: "every account with its connection config and sync status",
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
        status: status.map(|s| (s, err)),
        to: r.get(9)?,
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

/// The inbox under the typed filter: one lowercase substring over
/// sender name + address + subject (the shell's historical semantics).
pub fn inbox_filtered(store: &Store, filter: &str) -> Vec<MailHead> {
    let q = filter.trim().to_lowercase();
    inbox(store)
        .iter()
        .filter(|m| {
            q.is_empty()
                || format!("{} {} {}", m.from_name, m.from_email, m.subject)
                    .to_lowercase()
                    .contains(&q)
        })
        .cloned()
        .collect()
}

/// Every mail, archived included (the launcher's corpus).
pub fn all(store: &Store) -> Rc<Vec<MailHead>> {
    store.rows(&Q_ALL, &[], head_row)
}

/// One mail by id.
pub fn mail(store: &Store, id: MailId) -> Option<MailFull> {
    store.rows(&Q_MAIL, &[Val::I(id)], full_row).first().cloned()
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
    })
}

/// Every account.
pub fn accounts(store: &Store) -> Rc<Vec<Account>> {
    store.rows(&Q_ACCOUNTS, &[], account_row)
}

/// Creates an account (the settings form's action). Folders arrive with the
/// first sync; the password goes to the keychain, never here.
pub fn add_account_tx(
    c: &rusqlite::Connection,
    email: &str,
    imap_host: &str,
    smtp_host: &str,
) -> rusqlite::Result<i64> {
    c.execute(
        "INSERT INTO account(label, email, imap_host, smtp_host) VALUES(?1,?1,?2,?3)",
        rusqlite::params![email, imap_host, smtp_host],
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

/// The inbox neighbours of a mail: `(newer, older)`.
pub fn neighbours(store: &Store, id: MailId) -> (Option<MailId>, Option<MailId>) {
    let list = inbox(store);
    let Some(i) = list.iter().position(|m| m.id == id) else {
        return (None, None);
    };
    (
        i.checked_sub(1).map(|j| list[j].id),
        list.get(i + 1).map(|m| m.id),
    )
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
        Kind::Message { id } => mail(store, *id)
            .map(|m| m.head.subject)
            .unwrap_or_else(|| "message".into()),
        Kind::Contact { email } => contact(store, email).0,
        Kind::Compose { re } => mail(store, *re)
            .map(|m| format!("re: {}", m.head.subject))
            .unwrap_or_else(|| "new mail".into()),
        Kind::Settings => "settings".into(),
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

/// Archives a mail: it moves to its account's archive folder. Intent only —
/// the push pass makes the server agree (see [`mark_read_tx`]).
pub fn archive_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE message SET folder =
           (SELECT f.id FROM folder f
            WHERE f.account = message.account AND f.role = 'archive')
         WHERE id = ?1",
        [id],
    )?;
    Ok(())
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

// -- the demo seed -----------------------------------------------------------

struct SeedMail {
    from_name: &'static str,
    from_email: &'static str,
    subject: &'static str,
    date: f64,
    unread: bool,
    body: &'static str,
    status: Option<(&'static str, bool)>,
}

/// The hand-written demo mail, newest first — ids land as 1..=8 in a fresh
/// store, which the tests and e2e suites rely on.
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
        },
        SeedMail {
            from_name: "GitHub",
            from_email: "notifications@github.com",
            subject: "[stelaxis] CI failed on main",
            date: ts(2026, 8, 31, 8, 2),
            unread: true,
            body: "Workflow main #4128 failed on push 9f3c2a1.\n\nFailed steps: mix test (2 failures), credo --strict (1 warning). Full logs are attached to the run.",
            status: Some(("ci: FAILED — build (2m 14s), tests (41s)", true)),
        },
        SeedMail {
            from_name: "Max Ivanov",
            from_email: "max@ivanov.dev",
            subject: "Re: superapp panel model",
            date: ts(2026, 8, 30, 22, 47),
            unread: false,
            body: "Read your note on panels. The joined/replace rule feels like the right default — it is the preview-pane pattern, but generalized to everything.\n\nOne question though: what happens to a half-written draft if a joined compose panel gets replaced by the next link? Feels like some panels need a way to resist replacement.",
            status: None,
        },
        SeedMail {
            from_name: "Elena Petrova",
            from_email: "elena.p@gmail.com",
            subject: "Sat hike — early start?",
            date: ts(2026, 8, 30, 18, 20),
            unread: false,
            body: "Weather looks fine for Saturday. Early start (7:30) or lazy start (10:00)?\n\nThere is a new trail variant, ~14 km, one café stop. Bring the good thermos.",
            status: None,
        },
        SeedMail {
            from_name: "RSS Digest",
            from_email: "digest@rss.local",
            subject: "weekly: 14 unread items in 3 feeds",
            date: ts(2026, 8, 30, 7, 0),
            unread: false,
            body: "Unread this week: niri release notes (2), simonwillison.net (9), lobste.rs top (3).\n\nThis digest is itself a candidate for an rss/feed panel, by the way.",
            status: None,
        },
        SeedMail {
            from_name: "Calendar",
            from_email: "calendar@local",
            subject: "invite: dentist — tue 10:00",
            date: ts(2026, 8, 29, 16, 41),
            unread: false,
            body: "Dentist, Tuesday 10:00–10:45. Reminder set for 30 minutes before.\n\nReply yes to confirm, or propose a new time.",
            status: None,
        },
        SeedMail {
            from_name: "Hetzner",
            from_email: "billing@hetzner.com",
            subject: "invoice 2026-08 — €46.20",
            date: ts(2026, 8, 29, 11, 5),
            unread: false,
            body: "Invoice 2026-08 for €46.20 is available. Auto-charge on Sep 3.\n\nUsage: 2× CX32, 1× volume 100 GB, egress 214 GB.",
            status: None,
        },
        SeedMail {
            from_name: "Dmitry Orlov",
            from_email: "dorlov@fastmail.com",
            subject: "that airport book",
            date: ts(2026, 8, 28, 20, 33),
            unread: false,
            body: "Found it — the airport design book you mentioned at dinner. Ordering a copy tomorrow.\n\nBorrowing rights claimed for after you finish, obviously.",
            status: None,
        },
    ]
}

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
        folder("Archive", "archive")?;
        folder("Sent", "sent")?;
        folder("Trash", "trash")?;

        let insert = |m: &SeedMail| -> rusqlite::Result<()> {
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email,
                                     subject, date, unread, body, status, status_err)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    acct,
                    inbox,
                    m.from_name,
                    m.from_email,
                    m.subject,
                    m.date,
                    m.unread,
                    m.body,
                    m.status.map(|(s, _)| s),
                    m.status.map(|(_, e)| e).unwrap_or(false),
                ],
            )?;
            Ok(())
        };
        for m in &base_mails() {
            insert(m)?;
        }
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
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email,
                                     subject, date, unread, body)
                 VALUES(?1,?2,?3,?4,?5,?6,0,?7)",
                rusqlite::params![
                    acct,
                    inbox,
                    name,
                    email,
                    format!("archive digest #{n:02}"),
                    ts(2026, 8, 27 - i / 6, 8 + (i % 12), (i * 7) % 60),
                    format!(
                        "Archive item #{n:02} from {name} — generated filler so the inbox overflows and in-panel scrolling is honest.\n\nNothing to see here beyond the scrollbar."
                    ),
                ],
            )?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let s = Store::open(None).expect("in-memory store");
        seed_if_empty(&s).expect("seed");
        s
    }

    /// The seed reproduces the demo world: 68 mails, m1/m2 unread, ids in
    /// insert order (m1 = 1), newest first.
    #[test]
    fn seed_reproduces_the_demo_world() {
        let s = store();
        let rows = inbox(&s);
        assert_eq!(rows.len(), 68);
        assert_eq!(rows[0].id, 1);
        assert_eq!(rows[0].subject, "Q3 infra budget draft");
        assert!(rows[0].unread && rows[1].unread && !rows[2].unread);
        assert_eq!(fmt_date(rows[0].date), "aug 31 09:14");
        assert_eq!(me(&s), "me@prepor.dev");
        // Seeding an already-seeded store is a no-op.
        seed_if_empty(&s).unwrap();
        assert_eq!(inbox(&s).len(), 68);
    }

    /// Filter semantics are the shell's: one substring, sender + subject.
    #[test]
    fn filter_and_neighbours_match_the_old_semantics() {
        let s = store();
        let hits = inbox_filtered(&s, "vera");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 1);
        assert!(inbox_filtered(&s, "GITHUB").len() > 10, "case-insensitive");
        let (newer, older) = neighbours(&s, 2);
        assert_eq!(newer, Some(1));
        assert_eq!(older, Some(3));
        assert_eq!(neighbours(&s, 1).0, None);
    }

    /// Archive moves a mail out of the inbox (and out of neighbours), the
    /// read flag clears once, titles resolve through the store.
    #[test]
    fn mutations_and_titles() {
        let s = store();
        assert_eq!(title(&s, &Kind::Message { id: 1 }), "Q3 infra budget draft");
        assert_eq!(
            title(&s, &Kind::Contact { email: "vera@kovac.io".into() }),
            "Vera Kovac"
        );
        assert_eq!(title(&s, &Kind::Compose { re: 1 }), "re: Q3 infra budget draft");
        assert_eq!(title(&s, &Kind::Inbox { filter: Some("x".into()) }), "inbox · x");

        s.write(|c| mark_read_tx(c, 1)).unwrap();
        assert!(!inbox(&s)[0].unread);
        s.write(|c| archive_tx(c, 1)).unwrap();
        assert_eq!(inbox(&s).len(), 67);
        assert_ne!(inbox(&s)[0].id, 1);
        assert_eq!(all(&s).len(), 68, "archived mail stays in the corpus");
        let (name, n) = contact(&s, "vera@kovac.io");
        assert_eq!((name.as_str(), n), ("Vera Kovac", 1));
    }

    /// The civil-date maths round-trips.
    #[test]
    fn dates_round_trip() {
        assert_eq!(fmt_date(ts(2026, 8, 31, 9, 14)), "aug 31 09:14");
        assert_eq!(fmt_date(ts(2026, 1, 1, 0, 0)), "jan 01 00:00");
        assert_eq!(fmt_date(ts(2025, 12, 31, 23, 59)), "dec 31 23:59");
    }
}
