//! The demo world: one account, five folders, and the letters in them.
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
    ("Spam", "spam"),
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
    pub from_name: String,
    pub from_email: String,
    pub subject: String,
    pub date: f64,
    pub unread: bool,
    /// Paragraphs, `\n\n`-separated.
    pub body: String,
    /// The HTML reading, when the demo sender sent one. Written here as it
    /// arrived and narrowed on the way in, exactly as a synced letter's is —
    /// the seed exercises the real path rather than a tidied version of it.
    pub html: Option<&'static str>,
    /// An optional status line; `true` marks it as an error.
    pub status: Option<(&'static str, bool)>,
    /// The role of the folder it sits in.
    pub folder: &'static str,
    /// Message-ID and what it references — the threading headers; empty for
    /// a mail that stands alone.
    pub mid: String,
    pub refs: Vec<String>,
    /// Already passed on, so the mark has somewhere to show.
    pub forwarded: bool,
}

/// The shape most of the seed is written in: everything static, and the
/// owned fields built from it.
struct Static {
    from_name: &'static str,
    from_email: &'static str,
    subject: &'static str,
    date: f64,
    unread: bool,
    body: &'static str,
    html: Option<&'static str>,
    status: Option<(&'static str, bool)>,
    folder: &'static str,
    mid: &'static str,
    refs: &'static [&'static str],
    forwarded: bool,
}

impl From<Static> for SeedMail {
    fn from(s: Static) -> SeedMail {
        SeedMail {
            from_name: s.from_name.into(),
            from_email: s.from_email.into(),
            subject: s.subject.into(),
            date: s.date,
            unread: s.unread,
            body: s.body.into(),
            html: s.html,
            status: s.status,
            folder: s.folder,
            mid: s.mid.into(),
            refs: s.refs.iter().map(|r| (*r).to_string()).collect(),
            forwarded: s.forwarded,
        }
    }
}

/// The demo world's letters, in the order they are filed: the hand-written
/// inbox first, then the generated tail that makes it overflow, then the
/// conversation, the junk, and the CI runs.
#[must_use]
pub fn mails() -> Vec<SeedMail> {
    let mut v = base_mails();
    v.extend(filler_mails());
    v.extend(thread_mails());
    v.extend(spam_mails());
    v.extend(ci_mails());
    v
}

/// The hand-written demo mail, newest first — ids land as 1..=9 in a fresh
/// store, which the tests and the suites rely on.
fn base_mails() -> Vec<SeedMail> {
    [
        Static {
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
            forwarded: false,
        },
        Static {
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
            html: Some(GITHUB_HTML),
            status: Some(("ci: FAILED — build (2m 14s), tests (41s)", true)),
            folder: "inbox",
            mid: "ci-4128@github.com",
            refs: &["stelaxis-ci@github.com"],
            forwarded: false,
        },
        Static {
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
            forwarded: false,
        },
        Static {
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
            forwarded: false,
        },
        Static {
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
            forwarded: false,
        },
        Static {
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
            forwarded: false,
        },
        Static {
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
            // One mail already passed on, so the mark has somewhere to show.
            forwarded: true,
        },
        Static {
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
            forwarded: false,
        },
        // The one long letter in the demo world: it does not fit a message
        // panel's three rows, so the panel asks for more and opens tall.
        Static {
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
            html: None,
            status: None,
            folder: "inbox",
            mid: "",
            refs: &[],
            forwarded: false,
        },
    ]
    .into_iter()
    .map(SeedMail::from)
    .collect()
}

/// The generated tail: the inbox genuinely overflows, so in-panel scrolling
/// has something to do. All of it older than the hand-written nine, so the
/// top of the list is what it was.
fn filler_mails() -> Vec<SeedMail> {
    const SENDERS: [(&str, &str); 4] = [
        ("RSS Digest", "digest@rss.local"),
        ("GitHub", "notifications@github.com"),
        ("Hetzner", "billing@hetzner.com"),
        ("Calendar", "calendar@local"),
    ];
    (0..60u32)
        .map(|i| {
            let (name, email) = SENDERS[(i as usize) % SENDERS.len()];
            let n = 60 - i;
            SeedMail {
                from_name: name.into(),
                from_email: email.into(),
                subject: format!("archive digest #{n:02}"),
                date: ts(2026, 8, 27 - i / 6, 8 + (i % 12), (i * 7) % 60),
                unread: false,
                body: format!(
                    "Archive item #{n:02} from {name} — generated filler so the inbox \
                     overflows and in-panel scrolling is honest.\n\nNothing to see here \
                     beyond the scrollbar."
                ),
                html: None,
                status: None,
                folder: "inbox",
                mid: String::new(),
                refs: Vec::new(),
                forwarded: false,
            }
        })
        .collect()
}

/// The demo world's conversation: the note Max was replying to (mine, in
/// Sent) and his second reply, which folds into his first one's inbox row.
fn thread_mails() -> Vec<SeedMail> {
    [
        Static {
            from_name: "Andrey Rudenko",
            from_email: ADDRESS,
            subject: "superapp panel model",
            date: ts(2026, 8, 29, 14, 2),
            unread: false,
            body: "Wrote up the panel model: joined panels, replace in place, the chain closing behind a replacement. Curious what you make of the join rule in particular.\n\nThe draft is in the shared folder — comments welcome before Monday.",
            html: None,
            status: None,
            folder: "sent",
            mid: "pm-0@prepor.dev",
            refs: &[],
            forwarded: false,
        },
        Static {
            from_name: "Max Ivanov",
            from_email: "max@ivanov.dev",
            subject: "Re: superapp panel model",
            date: ts(2026, 8, 31, 7, 30),
            unread: false,
            body: "One more thought after sleeping on it: the join rule is also what keeps a preview honest. The panel beside the list is always the list's, never a stray.\n\nOn Sun, 30 Aug 2026 at 22:47, Max Ivanov wrote:\n> Read your note on panels. The joined/replace rule feels like the right default — it is the preview-pane pattern, but generalized to everything.\n>\n> One question though: what happens to a half-written draft if a joined compose panel gets replaced by the next link?",
            // The other HTML sender, and the one that matters to the reader:
            // a person's composer, which writes the quoted tail as the
            // `<blockquote>` under an attribution line. The fold reads it by
            // the same rule it reads a `>` block, so a conversation folds
            // both readings the one way.
            html: Some(MAX_HTML),
            status: None,
            folder: "inbox",
            mid: "pm-2@ivanov.dev",
            refs: &["pm-0@prepor.dev", "pm-1@ivanov.dev"],
            forwarded: false,
        },
    ]
    .into_iter()
    .map(SeedMail::from)
    .collect()
}

/// What the spam folder holds — the demo world's junk, so the panel that
/// shows it has something to show and the filter something to sift. None of
/// it threads with anything: that is rather the point of it.
fn spam_mails() -> Vec<SeedMail> {
    [
        Static {
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
            forwarded: false,
        },
        Static {
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
            forwarded: false,
        },
        Static {
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
            forwarded: false,
        },
    ]
    .into_iter()
    .map(SeedMail::from)
    .collect()
}

/// The five earlier runs of the CI workflow the GitHub mail continues —
/// `(run, day, hour, minute, failed)` — archived, so the inbox rows stay
/// where they were; the thread they make is six long. None of them names the
/// GitHub mail: every run references the same issue mail that never arrived,
/// which is the third threading lookup's case. Two failed; the oldest carries
/// the red status line, so a collapsed row has one to show.
const CI_RUNS: [(u32, u32, u32, u32, bool); 5] = [
    (4116, 28, 6, 10, true),
    (4119, 28, 18, 30, false),
    (4121, 29, 8, 45, false),
    (4124, 30, 7, 15, true),
    (4126, 30, 21, 0, false),
];

fn ci_mails() -> Vec<SeedMail> {
    CI_RUNS
        .into_iter()
        .map(|(run, day, hour, minute, failed)| {
            let outcome = if failed { "failed" } else { "passed" };
            SeedMail {
                from_name: "GitHub".into(),
                from_email: "notifications@github.com".into(),
                subject: format!("[stelaxis] CI {outcome} on main"),
                date: ts(2026, 8, day, hour, minute),
                unread: false,
                body: format!(
                    "Workflow main #{run} {outcome} on push {:07x}.\n\nFull logs are \
                     attached to the run.",
                    run.wrapping_mul(2_654_435)
                ),
                html: None,
                status: (failed && run == 4116).then_some(("ci: FAILED — tests (1m 02s)", true)),
                folder: "archive",
                mid: format!("ci-{run}@github.com"),
                refs: vec!["stelaxis-ci@github.com".into()],
                forwarded: false,
            }
        })
        .collect()
}

// -- what the demo letters carry ------------------------------------------------

/// The letter the GitHub notification arrived as.
const GITHUB_HTML: &str = "<html><head><style>.hd{background:#24292f;color:#fff}</style></head>\
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
     </body></html>";

/// The letter Max's second reply arrived as, from a composer that writes
/// HTML. Its quote is the `<blockquote>` under an attribution line, which is
/// the shape every composer writes one in.
const MAX_HTML: &str = "<div dir=\"ltr\"><p>One more thought after sleeping on it: the join \
     rule is also what keeps a preview honest. The panel beside the list is \
     always the list&#39;s, never a stray.</p>\
     <p>On Sun, 30 Aug 2026 at 22:47, Max Ivanov \
     &lt;<a href=\"mailto:max@ivanov.dev\">max@ivanov.dev</a>&gt; wrote:</p>\
     <blockquote><p>Read your note on panels. The joined/replace rule feels \
     like the right default &mdash; it is the preview-pane pattern, but \
     <i>generalized to everything</i>.</p>\
     <p>One question though: what happens to a half-written draft if a joined \
     compose panel gets replaced by the next link?</p></blockquote></div>";

/// One part of a seeded letter: what it is called, and its bytes.
type SeedPart = (&'static str, &'static [u8]);

/// The demo letters that carry something, by subject. A seeded mail normally
/// has no `raw` at all; these two get one — built as a real `multipart/mixed`
/// and walked by the ingest path's own parser — so the message row, the card
/// and the browser meet real MIME rather than a fixture. One is text, so the
/// card previews it; one is not, so `open` has something to hand to the OS.
pub const SEED_PARTS: &[(&str, &[SeedPart])] = &[
    (
        "Q3 infra budget draft",
        &[("q3-budget.csv", CSV.as_bytes())],
    ),
    ("invoice 2026-08 — €46.20", &[("invoice-2026-08.pdf", PDF)]),
];

const CSV: &str = "line,aug,sep,delta\n\
                   staging cluster,1840,0,-1840\n\
                   ci runners,320,910,+590\n\
                   object store,210,224,+14\n\
                   egress,640,?,?\n\
                   ,,,\n\
                   total,3010,1134+egress,\n";

/// The smallest thing that is honestly a PDF: one empty page. Enough for the
/// card to say `pdf`, and for `open` to hand the OS something a viewer will
/// actually show.
const PDF: &[u8] = b"%PDF-1.4\n\
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]>>endobj\n\
trailer<</Root 1 0 R>>\n%%EOF\n";

/// What this letter carries, if the seed gave it anything.
#[must_use]
pub fn parts_of(subject: &str) -> &'static [SeedPart] {
    SEED_PARTS
        .iter()
        .find(|(s, _)| *s == subject)
        .map_or(&[][..], |(_, p)| *p)
}

// -- the wire form -------------------------------------------------------------

/// A timestamp as RFC 5322 writes one: `Mon, 31 Aug 2026 09:14:00 +0000`.
///
/// The real spelling, because the real parser reads it: what the fake server
/// hands back goes through `mail-parser`, the same walk a fetched letter
/// takes, so the demo world proves the ingest path rather than a tidier one.
#[must_use]
pub fn header_date(at: f64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = at as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    // Unix day zero was a Thursday, which is where the table starts.
    let wd = DAYS[days.rem_euclid(7) as usize];
    let mon = MONTHS[(m as usize).clamp(1, 12) - 1];
    format!("{wd}, {d:02} {mon} {y:04} {h:02}:{min:02}:{s:02} +0000")
}

/// The date a mail the fake submission server formats wears. It has no clock
/// of its own, so it wears the instant a scripted run believes it is.
#[must_use]
pub fn sent_date() -> String {
    header_date(virtual_epoch())
}

/// One seeded letter as the bytes it would have arrived as: its headers,
/// then its readings and whatever it carries, as a real `multipart/mixed`
/// the ingest path's own parser walks.
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
    let parts = parts_of(&m.subject);
    let text = m.body.replace('\n', "\r\n");
    if parts.is_empty() && m.html.is_none() {
        raw += &format!("\r\n{text}\r\n");
        return raw;
    }
    raw += "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"seed\"\r\n\r\n";
    raw += &format!("--seed\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{text}\r\n");
    if let Some(html) = m.html {
        raw += &format!(
            "--seed\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n"
        );
    }
    for (name, bytes) in parts {
        raw += &format!(
            "--seed\r\nContent-Type: {}\r\n\
             Content-Disposition: attachment; filename=\"{name}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            kernel::caps::mime_of(name),
            super::html::base64_encode(bytes)
        );
    }
    raw += "--seed--\r\n";
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
        let mut folders: Vec<(&str, i64)> = Vec::new();
        for (name, role) in FOLDERS {
            c.execute(
                "INSERT INTO folder(account, name, role, uidvalidity, uidnext)
                 VALUES(?1, ?2, ?3, 1, 1)",
                rusqlite::params![ACCOUNT, name, role],
            )?;
            folders.push((role, c.last_insert_rowid()));
        }
        for m in &mails() {
            let (_, fid) = *folders
                .iter()
                .find(|(role, _)| *role == m.folder)
                .expect("a seeded mail names a seeded folder");
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email, subject,
                                     date, unread, body, status, status_err,
                                     message_id, topic, forwarded, html)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
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
                    (!m.mid.is_empty()).then(|| m.mid.clone()),
                    topic_of(&m.subject),
                    m.forwarded,
                    // Narrowed on the way in, as a synced letter's is.
                    m.html.map(super::html::sanitize),
                ],
            )?;
            let id = c.last_insert_rowid();
            thread_tx(c, ACCOUNT, id, &m.mid, &m.refs)?;
            // The two that carry something: the letter they would have
            // arrived as goes into `raw`, and the parts come back out of it
            // through the ingest path's own walk — so the demo world proves
            // the real one rather than standing in for it.
            if !parts_of(&m.subject).is_empty() {
                let raw = rfc822(m);
                c.execute(
                    "UPDATE message SET raw = ?2 WHERE id = ?1",
                    rusqlite::params![id, raw.as_bytes()],
                )?;
                super::parts::attach_tx(c, id, &super::sync::parse_mail(raw.as_bytes()).attachments)?;
            }
            // The uid the fake server handed the same letter: one per
            // folder, in this order, from one.
            let uid: i64 =
                c.query_row("SELECT uidnext FROM folder WHERE id = ?1", [fid], |r| {
                    r.get(0)
                })?;
            c.execute(
                "INSERT INTO server_msg(message, folder, uid, seen, forwarded)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, fid, uid, !m.unread, m.forwarded],
            )?;
            c.execute(
                "UPDATE folder SET uidnext = uidnext + 1 WHERE id = ?1",
                [fid],
            )?;
        }
        Ok(())
    })
}
