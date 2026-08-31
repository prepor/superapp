//! The fake mail behind the demo panels, plus its runtime flags.
//!
//! Static content lives in [`mails`]; what mutates at runtime (read, archived)
//! lives in [`MailState`], owned by the shell.

use std::collections::HashSet;

/// A mail's identity — an index into the static data.
pub type MailId = &'static str;

/// The local account.
pub const ME: &str = "me@prepor.dev";

/// One fake mail.
#[derive(Debug, Clone)]
pub struct Mail {
    /// Identity.
    pub id: MailId,
    /// Sender display name.
    pub from_name: &'static str,
    /// Sender address.
    pub from_email: &'static str,
    /// Subject line.
    pub subject: &'static str,
    /// Display date.
    pub date: &'static str,
    /// Body paragraphs.
    pub body: &'static [&'static str],
    /// An optional status line; `true` marks it as an error (the one place
    /// colour is allowed).
    pub status: Option<(&'static str, bool)>,
}

/// The hand-written mail, newest first.
static BASE: &[Mail] = &[
    Mail {
        id: "m1",
        from_name: "Vera Kovac",
        from_email: "vera@kovac.io",
        subject: "Q3 infra budget draft",
        date: "aug 31 09:14",
        body: &[
            "Draft for Q3 infra spend is ready. Main deltas: the old staging cluster goes away and CI runners move to the new box.",
            "Can you sanity-check the numbers before Thursday? Especially egress — I suspect the CDN line is stale.",
        ],
        status: None,
    },
    Mail {
        id: "m2",
        from_name: "GitHub",
        from_email: "notifications@github.com",
        subject: "[stelaxis] CI failed on main",
        date: "aug 31 08:02",
        body: &[
            "Workflow main #4128 failed on push 9f3c2a1.",
            "Failed steps: mix test (2 failures), credo --strict (1 warning). Full logs are attached to the run.",
        ],
        status: Some(("ci: FAILED — build (2m 14s), tests (41s)", true)),
    },
    Mail {
        id: "m3",
        from_name: "Max Ivanov",
        from_email: "max@ivanov.dev",
        subject: "Re: superapp panel model",
        date: "aug 30 22:47",
        body: &[
            "Read your note on panels. The joined/replace rule feels like the right default — it is the preview-pane pattern, but generalized to everything.",
            "One question though: what happens to a half-written draft if a joined compose panel gets replaced by the next link? Feels like some panels need a way to resist replacement.",
        ],
        status: None,
    },
    Mail {
        id: "m4",
        from_name: "Elena Petrova",
        from_email: "elena.p@gmail.com",
        subject: "Sat hike — early start?",
        date: "aug 30 18:20",
        body: &[
            "Weather looks fine for Saturday. Early start (7:30) or lazy start (10:00)?",
            "There is a new trail variant, ~14 km, one café stop. Bring the good thermos.",
        ],
        status: None,
    },
    Mail {
        id: "m5",
        from_name: "RSS Digest",
        from_email: "digest@rss.local",
        subject: "weekly: 14 unread items in 3 feeds",
        date: "aug 30 07:00",
        body: &[
            "Unread this week: niri release notes (2), simonwillison.net (9), lobste.rs top (3).",
            "This digest is itself a candidate for an rss/feed panel, by the way.",
        ],
        status: None,
    },
    Mail {
        id: "m6",
        from_name: "Calendar",
        from_email: "calendar@local",
        subject: "invite: dentist — tue 10:00",
        date: "aug 29 16:41",
        body: &[
            "Dentist, Tuesday 10:00–10:45. Reminder set for 30 minutes before.",
            "Reply yes to confirm, or propose a new time.",
        ],
        status: None,
    },
    Mail {
        id: "m7",
        from_name: "Hetzner",
        from_email: "billing@hetzner.com",
        subject: "invoice 2026-08 — €46.20",
        date: "aug 29 11:05",
        body: &[
            "Invoice 2026-08 for €46.20 is available. Auto-charge on Sep 3.",
            "Usage: 2× CX32, 1× volume 100 GB, egress 214 GB.",
        ],
        status: None,
    },
    Mail {
        id: "m8",
        from_name: "Dmitry Orlov",
        from_email: "dorlov@fastmail.com",
        subject: "that airport book",
        date: "aug 28 20:33",
        body: &[
            "Found it — the airport design book you mentioned at dinner. Ordering a copy tomorrow.",
            "Borrowing rights claimed for after you finish, obviously.",
        ],
        status: None,
    },
];

/// Every mail, newest first: the hand-written set plus a generated archive
/// tail, so the inbox genuinely overflows and scrolling has something to do.
#[must_use]
pub fn mails() -> &'static [Mail] {
    static ALL: std::sync::OnceLock<Vec<Mail>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<Mail> = BASE.to_vec();
        let senders: [(&str, &str); 4] = [
            ("RSS Digest", "digest@rss.local"),
            ("GitHub", "notifications@github.com"),
            ("Hetzner", "billing@hetzner.com"),
            ("Calendar", "calendar@local"),
        ];
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        for i in 0..60u32 {
            let (name, email) = senders[(i as usize) % senders.len()];
            let n = 60 - i;
            let day = 27 - i / 6;
            let body: &'static [&'static str] = Box::leak(
                vec![
                    leak(format!(
                        "Archive item #{n:02} from {name} — generated filler so the inbox overflows and in-panel scrolling is honest."
                    )),
                    "Nothing to see here beyond the scrollbar.",
                ]
                .into_boxed_slice(),
            );
            v.push(Mail {
                id: leak(format!("gen{i}")),
                from_name: name,
                from_email: email,
                subject: leak(format!("archive digest #{n:02}")),
                date: leak(format!("aug {day:02} {:02}:{:02}", 8 + (i % 12), (i * 7) % 60)),
                body,
                status: None,
            });
        }
        v
    })
}

/// A mail by id.
#[must_use]
pub fn mail(id: MailId) -> Option<&'static Mail> {
    mails().iter().find(|m| m.id == id)
}

/// Runtime mail flags: what the demo mutates.
#[derive(Debug, Default)]
pub struct MailState {
    unread: HashSet<MailId>,
    archived: HashSet<MailId>,
}

impl MailState {
    /// The boot state: the two newest mails are unread.
    #[must_use]
    pub fn new() -> Self {
        let mut s = Self::default();
        s.unread.insert("m1");
        s.unread.insert("m2");
        s
    }

    /// Whether a mail is unread.
    #[must_use]
    pub fn is_unread(&self, id: MailId) -> bool {
        self.unread.contains(id)
    }

    /// Marks a mail read (opening it does this).
    pub fn mark_read(&mut self, id: MailId) {
        self.unread.remove(id);
    }

    /// Archives a mail: it leaves the inbox.
    pub fn archive(&mut self, id: MailId) {
        self.archived.insert(id);
    }

    /// The inbox: every non-archived mail, newest first.
    pub fn inbox(&self) -> impl Iterator<Item = &'static Mail> + '_ {
        mails().iter().filter(|m| !self.archived.contains(m.id))
    }

    /// The inbox filtered by a case-insensitive substring over sender+subject.
    #[must_use]
    pub fn inbox_filtered(&self, filter: &str) -> Vec<&'static Mail> {
        let q = filter.trim().to_lowercase();
        self.inbox()
            .filter(|m| {
                q.is_empty()
                    || format!("{} {} {}", m.from_name, m.from_email, m.subject)
                        .to_lowercase()
                        .contains(&q)
            })
            .collect()
    }

    /// The neighbours of a mail in the inbox: `(newer, older)`.
    #[must_use]
    pub fn neighbours(&self, id: MailId) -> (Option<MailId>, Option<MailId>) {
        let list: Vec<&Mail> = self.inbox().collect();
        let Some(i) = list.iter().position(|m| m.id == id) else {
            return (None, None);
        };
        (
            i.checked_sub(1).map(|j| list[j].id),
            list.get(i + 1).map(|m| m.id),
        )
    }

    /// How many mails a sender has in the mailbox (archived included).
    #[must_use]
    pub fn count_from(&self, email: &str) -> usize {
        mails().iter().filter(|m| m.from_email == email).count()
    }
}
