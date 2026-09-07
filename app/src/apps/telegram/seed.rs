//! The demo world: the people mail's demo already knows and a few more,
//! ten chats between them, and enough lines in the group to carry every
//! kind of message a transcript draws.
//!
//! It is the fixture the library and the suites draw, and what a person
//! sees until an account signs in. Dates sit around the virtual epoch
//! ([`kernel::time::virtual_epoch`], the first of September 2026 at noon),
//! so a headless run draws the same `11:52`, `mon` and `25.08` every time.

use kernel::store::Store;
use kernel::time::ts;

use super::model::PeerId;

// -- who ------------------------------------------------------------------------

/// The one peer that is me: saved messages.
pub const SELF: PeerId = 1;
pub const VERA: PeerId = 2;
pub const ELENA: PeerId = 3;
pub const MAX: PeerId = 4;
pub const ANNA: PeerId = 5;
pub const IVAN: PeerId = 6;
pub const OLGA: PeerId = 7;
pub const IRINA: PeerId = 8;
pub const SERGEY: PeerId = 9;
pub const STELAXIS: PeerId = 10;
pub const HIKE: PeerId = 11;
pub const FAMILY: PeerId = 12;
pub const RUST_WEEKLY: PeerId = 13;
pub const DEV: PeerId = 14;
pub const OLD_FLAT: PeerId = 15;

/// One peer of the demo world.
struct SeedPeer {
    id: PeerId,
    kind: &'static str,
    name: &'static str,
    username: Option<&'static str>,
    about: Option<&'static str>,
    phone: Option<&'static str>,
    status: Option<&'static str>,
    /// A group's or a channel's count: the members table holds the members
    /// a demo group lists, and the count says the same number.
    members: Option<i64>,
    online: Option<i64>,
    admin: bool,
    contact: bool,
}

const fn person(
    id: PeerId,
    name: &'static str,
    username: &'static str,
    status: &'static str,
    contact: bool,
) -> SeedPeer {
    SeedPeer {
        id,
        kind: "person",
        name,
        username: Some(username),
        about: None,
        phone: None,
        status: Some(status),
        members: None,
        online: None,
        admin: false,
        contact,
    }
}

fn peers() -> Vec<SeedPeer> {
    vec![
        SeedPeer {
            id: SELF,
            kind: "person",
            name: "Andrey Rudenko",
            username: Some("prepor"),
            about: None,
            phone: Some("+7 926 000 00 01"),
            status: Some("online"),
            members: None,
            online: None,
            admin: false,
            contact: false,
        },
        SeedPeer {
            about: Some("designer at stelaxis · mountains on weekends"),
            phone: Some("+7 926 123 45 67"),
            ..person(VERA, "Vera Kovac", "vera", "online", true)
        },
        SeedPeer {
            about: Some("runs. reads. writes about both."),
            phone: Some("+7 916 555 01 02"),
            ..person(ELENA, "Elena Petrova", "elena_p", "recently", true)
        },
        SeedPeer {
            about: Some("infra, budgets, and the occasional hike"),
            phone: Some("+7 903 777 00 11"),
            ..person(MAX, "Max Ivanov", "maxiv", "week", true)
        },
        person(ANNA, "Anna Schmidt", "anna_s", "month", false),
        person(IVAN, "Ivan Petrov", "ivanp", "online", true),
        person(OLGA, "Olga Novak", "olga", "recently", true),
        SeedPeer {
            phone: Some("+7 926 000 00 02"),
            ..person(IRINA, "Irina Rudenko", "irina_r", "online", true)
        },
        SeedPeer {
            phone: Some("+7 926 000 00 03"),
            ..person(SERGEY, "Sergey Rudenko", "sergey_r", "long", true)
        },
        SeedPeer {
            id: STELAXIS,
            kind: "group",
            name: "stelaxis",
            username: None,
            about: Some("the product, the design system, and everything around it"),
            phone: None,
            status: None,
            members: Some(7),
            online: Some(3),
            admin: true,
            contact: false,
        },
        SeedPeer {
            id: HIKE,
            kind: "group",
            name: "Hiking Saturday",
            username: None,
            about: Some("7:30 at the trailhead, rain or shine"),
            phone: None,
            status: None,
            members: Some(5),
            online: Some(2),
            admin: false,
            contact: false,
        },
        SeedPeer {
            id: FAMILY,
            kind: "group",
            name: "Family",
            username: None,
            about: None,
            phone: None,
            status: None,
            members: Some(3),
            online: Some(1),
            admin: false,
            contact: false,
        },
        SeedPeer {
            id: RUST_WEEKLY,
            kind: "channel",
            name: "Rust Weekly",
            username: Some("rustweekly"),
            about: Some("the week in rust, every tuesday"),
            phone: None,
            status: None,
            members: Some(12_400),
            online: None,
            admin: false,
            contact: false,
        },
        SeedPeer {
            id: DEV,
            kind: "channel",
            name: "superapp dev",
            username: Some("superapp_dev"),
            about: Some("build notes for the one app"),
            phone: None,
            status: None,
            members: Some(38),
            online: None,
            admin: true,
            contact: false,
        },
        SeedPeer {
            id: OLD_FLAT,
            kind: "group",
            name: "Old flat",
            username: None,
            about: None,
            phone: None,
            status: None,
            members: Some(3),
            online: Some(0),
            admin: false,
            contact: false,
        },
    ]
}

/// `(group, member, admin)`.
fn members() -> Vec<(PeerId, PeerId, bool)> {
    vec![
        (STELAXIS, SELF, true),
        (STELAXIS, VERA, true),
        (STELAXIS, MAX, false),
        (STELAXIS, ELENA, false),
        (STELAXIS, ANNA, false),
        (STELAXIS, IVAN, false),
        (STELAXIS, OLGA, false),
        (HIKE, SELF, false),
        (HIKE, VERA, true),
        (HIKE, ELENA, false),
        (HIKE, MAX, false),
        (HIKE, IVAN, false),
        (FAMILY, SELF, false),
        (FAMILY, IRINA, true),
        (FAMILY, SERGEY, false),
        (OLD_FLAT, SELF, false),
        (OLD_FLAT, SERGEY, false),
        (OLD_FLAT, ANNA, false),
    ]
}

/// `(folder, name, chats)`.
fn folders() -> Vec<(i64, &'static str, Vec<PeerId>)> {
    vec![
        (1, "work", vec![STELAXIS, DEV, MAX, RUST_WEEKLY]),
        (2, "personal", vec![VERA, ELENA, FAMILY, HIKE]),
    ]
}

// -- the chats --------------------------------------------------------------------

/// One conversation of the demo world, with its lines.
struct SeedChat {
    peer: PeerId,
    pinned: i64,
    muted: bool,
    archived: bool,
    draft: Option<&'static str>,
    typing: Option<&'static str>,
    /// The index into `lines` of the last message I read; `None` reads
    /// everything.
    last_read: Option<usize>,
    mention: bool,
    lines: Vec<Line>,
}

/// One message, as the seed spells it.
#[derive(Default)]
struct Line {
    sender: Option<PeerId>,
    at: f64,
    text: &'static str,
    out: bool,
    state: Option<&'static str>,
    edited: bool,
    /// The index into the chat's lines of what it answers.
    reply_to: Option<usize>,
    fwd_from: Option<&'static str>,
    media: Option<&'static str>,
    media_label: Option<&'static str>,
    media_ref: Option<&'static str>,
    media_size: Option<(i64, i64)>,
    media_secs: Option<i64>,
    media_at: Option<(f64, f64)>,
    media_until: Option<f64>,
    views: Option<i64>,
    comments: Option<i64>,
    reactions: Option<&'static str>,
    service: bool,
}

/// A time this year.
fn t(mo: u32, d: u32, h: u32, min: u32) -> f64 {
    ts(2026, mo, d, h, min)
}

/// Somebody else's line.
fn from(who: PeerId, at: f64, text: &'static str) -> Line {
    Line {
        sender: Some(who),
        at,
        text,
        ..Line::default()
    }
}

/// Mine, and read by the other side.
fn me(at: f64, text: &'static str) -> Line {
    Line {
        sender: Some(SELF),
        at,
        text,
        out: true,
        state: Some("read"),
        ..Line::default()
    }
}

/// A line about the chat.
fn service(at: f64, text: &'static str) -> Line {
    Line {
        at,
        text,
        service: true,
        ..Line::default()
    }
}

/// The trailhead, for the two locations the hike shares.
const TRAILHEAD: (f64, f64) = (47.0472, 8.3164);

/// A channel's own post.
fn post(at: f64, text: &'static str, views: i64, comments: i64) -> Line {
    Line {
        at,
        text,
        views: Some(views),
        comments: Some(comments),
        ..Line::default()
    }
}

fn chats() -> Vec<SeedChat> {
    vec![
        SeedChat {
            peer: VERA,
            pinned: 1,
            muted: false,
            archived: false,
            draft: None,
            typing: None,
            last_read: None,
            mention: false,
            lines: vec![
                from(VERA, t(8, 27, 10, 0), "lunch thursday?"),
                me(t(8, 27, 10, 5), "yes, 13:00 at the usual"),
                from(VERA, t(8, 31, 18, 20), "did you see the CI run? all green now"),
                me(t(8, 31, 18, 22), "🎉"),
                from(VERA, t(9, 1, 9, 12), "see you at 7:30 then?"),
                from(VERA, t(9, 1, 9, 12), "I'll bring the map"),
                Line {
                    reply_to: Some(4),
                    ..me(t(9, 1, 9, 14), "yes — the trailhead car park")
                },
                Line {
                    media: Some("circle"),
                    media_ref: Some("demo:garden"),
                    media_size: Some((480, 360)),
                    media_secs: Some(8),
                    ..from(VERA, t(9, 1, 11, 49), "")
                },
                Line {
                    edited: true,
                    ..from(VERA, t(9, 1, 11, 50), "the forecast changed, rain after 2pm")
                },
                me(t(9, 1, 11, 52), "then we start earlier. 7:00?"),
            ],
        },
        SeedChat {
            peer: STELAXIS,
            pinned: 2,
            muted: true,
            archived: false,
            draft: None,
            typing: None,
            last_read: Some(5),
            mention: true,
            lines: vec![
                service(t(8, 25, 10, 0), "Ivan Petrov joined the group"),
                from(MAX, t(8, 25, 10, 30), "welcome Ivan — the design doc is pinned"),
                service(t(8, 25, 10, 31), "Max Ivanov pinned a message"),
                Line {
                    media: Some("photo"),
                    media_ref: Some("demo:palette"),
                    media_size: Some((480, 300)),
                    reactions: Some("👍 3 · 🔥 1"),
                    ..from(VERA, t(8, 29, 16, 40), "new palette, what do you think")
                },
                from(ELENA, t(8, 29, 16, 45), "the second grey reads as disabled to me"),
                Line {
                    media: Some("video"),
                    media_ref: Some("demo:palette"),
                    media_size: Some((480, 300)),
                    media_secs: Some(14),
                    ..from(ELENA, t(8, 29, 16, 50), "the fold in motion")
                },
                me(t(8, 29, 17, 2), "the muted grey is too light on the fold"),
                Line {
                    fwd_from: Some("Elena Petrova"),
                    ..from(MAX, t(8, 31, 9, 0), "Q3 infra budget draft is ready for review")
                },
                Line {
                    media: Some("file"),
                    media_label: Some("report-q3.pdf · 2.1 MB"),
                    ..from(MAX, t(8, 31, 9, 3), "the numbers")
                },
                from(ANNA, t(9, 1, 11, 20), "@prepor can you look at the fold layout today?"),
                from(ANNA, t(9, 1, 11, 21), "the cover display clips the header"),
                Line {
                    reply_to: Some(9),
                    ..from(VERA, t(9, 1, 11, 25), "I saw it — the inset is 28 dp, it wants 34")
                },
                Line {
                    media: Some("sticker"),
                    media_label: Some("🙈"),
                    ..from(OLGA, t(9, 1, 11, 26), "")
                },
                Line {
                    media: Some("voice"),
                    media_secs: Some(42),
                    ..from(MAX, t(9, 1, 11, 30), "")
                },
                Line {
                    edited: true,
                    ..from(IVAN, t(9, 1, 11, 40), "meeting moved to 15:00")
                },
            ],
        },
        SeedChat {
            peer: ELENA,
            pinned: 0,
            muted: false,
            archived: false,
            draft: None,
            typing: None,
            last_read: Some(1),
            mention: false,
            lines: vec![
                from(ELENA, t(8, 30, 18, 20), "Sat hike — early start?"),
                me(t(8, 30, 18, 40), "7:30 at the trailhead?"),
                from(ELENA, t(9, 1, 10, 1), "can we make it 8? my train is late"),
                from(ELENA, t(9, 1, 10, 3), "also bringing a friend, ok?"),
            ],
        },
        SeedChat {
            peer: FAMILY,
            pinned: 0,
            muted: false,
            archived: false,
            draft: None,
            typing: Some("Irina"),
            last_read: Some(2),
            mention: false,
            lines: vec![
                Line {
                    media: Some("photo"),
                    media_ref: Some("demo:garden"),
                    media_size: Some((480, 360)),
                    ..from(IRINA, t(8, 30, 12, 0), "the garden today")
                },
                me(t(8, 30, 12, 30), "beautiful"),
                Line {
                    media: Some("voice"),
                    media_secs: Some(12),
                    ..from(SERGEY, t(9, 1, 9, 28), "")
                },
                Line {
                    media: Some("live"),
                    media_at: Some((55.7512, 37.6184)),
                    media_until: Some(t(9, 1, 12, 42)),
                    ..from(SERGEY, t(9, 1, 9, 29), "")
                },
                from(IRINA, t(9, 1, 9, 30), "call me when you're free"),
            ],
        },
        SeedChat {
            peer: RUST_WEEKLY,
            pinned: 0,
            muted: true,
            archived: false,
            draft: None,
            typing: None,
            last_read: Some(0),
            mention: false,
            lines: vec![
                Line {
                    reactions: Some("🦀 120"),
                    ..post(
                        t(8, 18, 9, 0),
                        "Rust 1.92 is out: precise capturing in traits, the new lint on unused generics, and cargo's lockfile v5.",
                        12_800,
                        41,
                    )
                },
                post(
                    t(8, 25, 9, 0),
                    "This week: async closures stabilised, a look at the 2027 edition survey, and three crates worth your evening.",
                    11_200,
                    23,
                ),
                Line {
                    reactions: Some("❤️ 34 · 🦀 12"),
                    ..post(
                        t(9, 1, 8, 15),
                        "Issue 612: the 2027 edition survey, cargo-script lands in nightly, and the borrow checker gets a new formulation.",
                        4_300,
                        8,
                    )
                },
                post(t(9, 1, 8, 16), "And a reminder: the RustConf CFP closes on friday.", 4_100, 0),
            ],
        },
        SeedChat {
            peer: HIKE,
            pinned: 0,
            muted: false,
            archived: false,
            draft: Some("I'll bring the thermos and"),
            typing: None,
            last_read: None,
            mention: false,
            lines: vec![
                from(VERA, t(8, 29, 20, 0), "who's in for saturday? 7:30 at the trailhead"),
                from(MAX, t(8, 29, 20, 5), "in"),
                from(IVAN, t(8, 29, 20, 5), "in, if the weather holds"),
                Line {
                    media: Some("location"),
                    media_at: Some(TRAILHEAD),
                    ..from(IVAN, t(8, 29, 20, 6), "")
                },
                me(t(8, 31, 19, 15), "I'll bring the map"),
                Line {
                    state: Some("failed"),
                    ..me(t(8, 31, 19, 20), "and the thermos")
                },
            ],
        },
        SeedChat {
            peer: DEV,
            pinned: 0,
            muted: false,
            archived: false,
            draft: None,
            typing: None,
            last_read: None,
            mention: false,
            lines: vec![
                Line {
                    out: true,
                    state: Some("read"),
                    ..post(t(8, 28, 12, 0), "files: a thing keeps its name until you give it another one — rename.", 35, 0)
                },
                Line {
                    out: true,
                    state: Some("read"),
                    ..post(
                        t(8, 31, 17, 5),
                        "mail: the server says a letter arrived — IMAP IDLE beside the interval.",
                        31,
                        2,
                    )
                },
            ],
        },
        SeedChat {
            peer: MAX,
            pinned: 0,
            muted: false,
            archived: false,
            draft: None,
            typing: None,
            last_read: None,
            mention: false,
            lines: vec![
                from(MAX, t(8, 29, 21, 0), "Q3 infra budget draft — do the numbers check out?"),
                Line {
                    state: Some("sent"),
                    ..me(t(8, 29, 21, 10), "the egress line is stale, I'll redo it")
                },
            ],
        },
        SeedChat {
            peer: SELF,
            pinned: 0,
            muted: false,
            archived: false,
            draft: None,
            typing: None,
            last_read: None,
            mention: false,
            lines: vec![
                me(t(8, 12, 10, 0), "book: that airport book — chapter 4, on queues"),
                Line {
                    media: Some("file"),
                    media_label: Some("ticket-lisbon.pdf · 340 KB"),
                    ..me(t(8, 20, 8, 30), "")
                },
                Line {
                    media: Some("audio"),
                    media_label: Some("Dry Cleaning — Scratchcard Lanyard"),
                    media_secs: Some(221),
                    ..me(t(8, 20, 8, 31), "")
                },
                me(t(8, 27, 13, 0), "idea: a chat is a transcript, not bubbles"),
            ],
        },
        SeedChat {
            peer: ANNA,
            pinned: 0,
            muted: false,
            archived: false,
            draft: None,
            typing: None,
            last_read: None,
            mention: false,
            lines: vec![
                from(ANNA, t(8, 12, 15, 0), "hi! Vera gave me your contact — about the fold review"),
                me(t(8, 12, 15, 20), "sure, send the doc over"),
            ],
        },
        SeedChat {
            peer: OLD_FLAT,
            pinned: 0,
            muted: false,
            archived: true,
            draft: None,
            typing: None,
            last_read: None,
            mention: false,
            lines: vec![
                from(SERGEY, t(8, 12, 9, 0), "the keys are with the neighbour"),
                service(t(8, 12, 9, 30), "Anna Schmidt left the group"),
            ],
        },
    ]
}

// -- the pictures ---------------------------------------------------------------------

/// The two pictures the demo world carries, bundled: a `media_ref` of
/// `demo:palette` or `demo:garden` reads one of these. Greyscale, drawn by a
/// script, so the repository carries a few kilobytes and no photograph.
#[must_use]
pub fn demo_bytes(media_ref: &str) -> Option<&'static [u8]> {
    match media_ref {
        "demo:palette" => Some(include_bytes!("../../../resources/telegram/palette.png")),
        "demo:garden" => Some(include_bytes!("../../../resources/telegram/garden.png")),
        _ => None,
    }
}

// -- the write -------------------------------------------------------------------

/// Fills an empty store with the demo world. Idempotent: a store with a
/// peer in it is left alone.
///
/// # Errors
///
/// If the store refuses the write.
pub fn seed_if_empty(store: &Store) -> rusqlite::Result<()> {
    let n: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM tg_peer", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    store.write(|c| {
        // The demo world numbers its own lines, in the order they are
        // written: a message id is the chat's, not the table's, so it is
        // said outright rather than taken from the row key (V8).
        let mut next_msg: i64 = 0;
        for p in &peers() {
            c.execute(
                "INSERT INTO tg_peer(id, kind, name, username, about, phone, status,
                                     members, online, admin, is_contact, is_self)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    p.id,
                    p.kind,
                    p.name,
                    p.username,
                    p.about,
                    p.phone,
                    p.status,
                    p.members,
                    p.online,
                    p.admin,
                    p.contact,
                    p.id == SELF
                ],
            )?;
        }
        for (chat, peer, admin) in members() {
            c.execute(
                "INSERT INTO tg_member(chat, peer, admin) VALUES(?1, ?2, ?3)",
                rusqlite::params![chat, peer, admin],
            )?;
        }
        for (id, name, chats) in folders() {
            c.execute(
                "INSERT INTO tg_folder(id, name) VALUES(?1, ?2)",
                rusqlite::params![id, name],
            )?;
            for chat in chats {
                c.execute(
                    "INSERT INTO tg_folder_chat(folder, chat) VALUES(?1, ?2)",
                    rusqlite::params![id, chat],
                )?;
            }
        }
        for chat in &chats() {
            c.execute(
                "INSERT INTO tg_chat(peer, pinned, muted, archived, draft, typing, mention)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    chat.peer,
                    chat.pinned,
                    chat.muted,
                    chat.archived,
                    chat.draft,
                    chat.typing,
                    chat.mention
                ],
            )?;
            let mut ids: Vec<i64> = Vec::with_capacity(chat.lines.len());
            for l in &chat.lines {
                let reply_to = l.reply_to.map(|i| ids[i]);
                next_msg += 1;
                c.execute(
                    "INSERT INTO tg_message(id, chat, sender, date, text, out, state, edited,
                                            reply_to, fwd_from, media, media_label, views,
                                            comments, reactions, service, media_ref,
                                            media_w, media_h, media_secs, media_lat,
                                            media_lon, media_until)
                     VALUES(?23, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                            ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                    rusqlite::params![
                        chat.peer,
                        l.sender,
                        l.at,
                        l.text,
                        l.out,
                        l.state,
                        l.edited,
                        reply_to,
                        l.fwd_from,
                        l.media,
                        l.media_label,
                        l.views,
                        l.comments,
                        l.reactions,
                        l.service,
                        l.media_ref,
                        l.media_size.map(|(w, _)| w),
                        l.media_size.map(|(_, h)| h),
                        l.media_secs,
                        l.media_at.map(|(lat, _)| lat),
                        l.media_at.map(|(_, lon)| lon),
                        l.media_until,
                        next_msg
                    ],
                )?;
                ids.push(next_msg);
            }
            // What I have read, and so how many I have not: the count is
            // derived from the line rather than written twice.
            let last_read = match chat.last_read {
                Some(i) => ids[i],
                None => *ids.last().unwrap_or(&0),
            };
            let unread = chat
                .lines
                .iter()
                .zip(&ids)
                .filter(|(l, id)| **id > last_read && !l.out && !l.service)
                .count() as i64;
            c.execute(
                "UPDATE tg_chat SET last_read = ?2, unread = ?3 WHERE peer = ?1",
                rusqlite::params![chat.peer, last_read, unread],
            )?;
        }
        Ok(())
    })
}
