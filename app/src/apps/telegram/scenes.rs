//! Telegram's entries for the panels library.
//!
//! A row is a fixture on the app's own row template, populated through the
//! very function the live list calls; a panel is a stage solo on one
//! identity, over a store of its own with the demo world in it, replaying a
//! script to reach its state. Both are built out of telegram's own types, so
//! a change that would break one of these scenes breaks the build instead
//! of the picture.

use kernel::scene::Scene;
use kernel::time::{ts, virtual_epoch};
use makepad_widgets::{live_id, LiveId};

use crate::shell::app_ui::Setup;
use crate::shell::catalog::{panel, widget, workspace_on};
use crate::shell::widgets::media::PlayerState;
use crate::shell::widgets::table::RowSpec;

use super::model::{self, Carried, ChatRow, Media, Msg, PeerKind, Person};
use super::panels::{Chat, Chats, Contacts, Line, Members, Messages, Peer, Place, Row, Viewer};
use super::seed::{DEV, ELENA, FAMILY, HIKE, IVAN, MAX, RUST_WEEKLY, SELF, STELAXIS, VERA};
use super::widgets::chats::ChatsRows;
use super::widgets::people::PeopleRows;

/// Telegram's scenes, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![
        chat_row(),
        chats(),
        topics(),
        message_row(),
        reactions(),
        media(),
        chat(),
        attach(),
        line(),
        viewer(),
        messages(),
        people(),
        peer(),
    ]
}

fn topics() -> Scene<Setup> {
    use super::{panels::Topics, seed::BERLIN};
    Scene::new("telegram topics", (480.0, 650.0))
        .note("Selected topics appear as independent conversations in the chat list.")
        .node("choose", panel(|_| Topics::id(BERLIN), ""))
        .node("selected", panel(|_| Topics::id(BERLIN), "click \"Meetups\"\nclick \"Cycling\"\nwait 500"))
        .node("filter", panel(|_| Topics::id(BERLIN), "click \"filter topics\"\ntype \"meet\"\nwait 500"))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn reactions() -> Scene<Setup> {
    let fixture = |card: bool, long: bool, script: &str| panel(move |store| {
        let id = model::history(store, RUST_WEEKLY).last().expect("a post").id;
        let text = if long {
            format!("Long reaction post\n{}", "A message extending below the panel.\n".repeat(45))
        } else {
            "All reaction counts stay visible at this width.".to_string()
        };
        store.write(move |c| {
            c.execute("UPDATE tg_message SET text = ?3, reactions = ?4 WHERE chat = ?1 AND id = ?2",
                rusqlite::params![RUST_WEEKLY, id, text,
                    "👍 12 · ❤️ 34 · 🔥 56 · 😂 7 · 😮 8 · 🙏 9 · 🎉 10 · 👏 11 · 🤔 12 · 🤯 13 · 😢 14 · 💯 15 · 🦄 16 · 🌚 17 · ⭐ 30 · custom emoji 4"])?;
            Ok(())
        }).expect("reaction fixture");
        if card { Line::id(RUST_WEEKLY, id) } else { Chat::at(RUST_WEEKLY, id) }
    }, script);
    let page = "wait 600\nkey cmd+j\nwait 300\nclick \"more\"\nwait 300\nclick \"🎉\"\nwait 300\nkey cmd+j\nwait 300\nclick \"more\"\nwait 300";
    Scene::new("reactions", (380.0, 360.0))
        .node("long chat", fixture(false, true, page))
        .about("A clipped message must leave the reaction bar clickable.")
        .node("long card", fixture(true, true, page))
        .about("The same picker remains clickable below a long message card.")
        .node("counts", fixture(true, false, ""))
        .sized((300.0, 360.0))
        .about("All counts wrap, including stars and custom emoji.")
}

/// A time today, against the virtual epoch the canvas runs on.
fn today(h: u32, min: u32) -> f64 {
    ts(2026, 9, 1, h, min)
}

fn chat_fixture(title: &str, kind: PeerKind, text: &str) -> ChatRow {
    ChatRow {
        peer: 2,
        topic: 0,
        is_forum: false,
        kind,
        title: title.to_string(),
        pinned: 0,
        muted: false,
        unread: 0,
        unread_mentions: 0,
        draft: None,
        typing: None,
        last: today(11, 52),
        last_text: text.to_string(),
        last_media: None,
        last_out: false,
        last_state: None,
        last_from: "Max Ivanov".to_string(),
        last_service: false,
    }
}

fn vera() -> ChatRow {
    chat_fixture("Vera Kovac", PeerKind::Person, "then we start earlier. 7:00?")
}

fn msg_fixture(name: &str, text: &str, at: f64) -> Msg {
    Msg {
        content_type: None,
        topic: 0,
        id: 1,
        chat: 2,
        sender: Some(2),
        sender_name: name.to_string(),
        date: at,
        text: text.to_string(),
        entities: None,
        out: false,
        state: None,
        edited: false,
        reply_to: None,
        reply_chat: None,
        unread_mention: false,
        reply_name: String::new(),
        reply_text: String::new(),
        fwd_from: None,
        media: None,
        views: None,
        comments: None,
        reactions: None,
        service: false,
    }
}

fn mine(text: &str, state: &str) -> Msg {
    Msg {
        out: true,
        state: Some(state.to_string()),
        ..msg_fixture("Andrey Rudenko", text, today(11, 52))
    }
}

// ---------------------------------------------------------------------------
// The scenes
// ---------------------------------------------------------------------------

/// One chat as the list shows it, in each of its states. Populated by
/// [`ChatsRows::populate`] — the function the live table calls — so the row
/// cannot drift from the list it belongs to.
fn chat_row() -> Scene<Setup> {
    let row = |r: ChatRow, selected: bool, marked: bool| {
        widget(live_id!(telegram_chat_row_tpl), move |cx, w| {
            ChatsRows::populate(cx, w, &r, selected, marked, kernel::time::virtual_epoch());
        })
    };
    Scene::new("chat row", (520.0, 52.0))
        .note("One chat as the list shows it: the title and when it last spoke, then what it said and how much of it is unread.")
        .note("Bold is unread. The ordinary count is outlined for a muted chat; a separate @ count highlights unread replies and mentions.")
        .node("read", row(vera(), false, false))
        .node(
            "unread",
            row(
                ChatRow {
                    unread: 2,
                    last_text: "also bringing a friend, ok?".into(),
                    ..chat_fixture("Elena Petrova", PeerKind::Person, "")
                },
                false,
                false,
            ),
        )
        .about("the whole title bold, and the count")
        .node(
            "group",
            row(
                ChatRow {
                    unread: 8,
                    unread_mentions: 1,
                    muted: true,
                    pinned: 2,
                    last_text: "meeting moved to 15:00".into(),
                    last_from: "Ivan Petrov".into(),
                    ..chat_fixture("stelaxis", PeerKind::Group, "")
                },
                false,
                false,
            ),
        )
        .about("a group says who spoke; pinned, muted, and one of them mentions me")
        .node(
            "mine, read",
            row(
                ChatRow {
                    last_out: true,
                    last_state: Some("read".into()),
                    ..vera()
                },
                false,
                false,
            ),
        )
        .about("my last line, and the other side has read it")
        .node(
            "mine, sent",
            row(
                ChatRow {
                    last_out: true,
                    last_state: Some("sent".into()),
                    last: ts(2026, 8, 29, 21, 10),
                    last_text: "the egress line is stale, I'll redo it".into(),
                    ..chat_fixture("Max Ivanov", PeerKind::Person, "")
                },
                false,
                false,
            ),
        )
        .about("one tick: sent, not read yet — and a day inside the week is its weekday")
        .node(
            "failed",
            row(
                ChatRow {
                    kind: PeerKind::Group,
                    last_out: true,
                    last_state: Some("failed".into()),
                    last: ts(2026, 8, 31, 19, 20),
                    last_text: "and the thermos".into(),
                    ..chat_fixture("Hiking Saturday", PeerKind::Group, "")
                },
                false,
                false,
            ),
        )
        .about("a line that never left, in the one colour errors get")
        .node(
            "draft",
            row(
                ChatRow {
                    kind: PeerKind::Group,
                    draft: Some("I'll bring the thermos and".into()),
                    last: ts(2026, 8, 31, 19, 20),
                    ..chat_fixture("Hiking Saturday", PeerKind::Group, "and the thermos")
                },
                false,
                false,
            ),
        )
        .about("the composer holds something unsent")
        .node(
            "typing",
            row(
                ChatRow {
                    unread: 1,
                    typing: Some("Irina".into()),
                    last: today(9, 30),
                    ..chat_fixture("Family", PeerKind::Group, "call me when you're free")
                },
                false,
                false,
            ),
        )
        .about("somebody is typing: the line says so instead")
        .node(
            "channel",
            row(
                ChatRow {
                    unread: 3,
                    muted: true,
                    last: today(8, 16),
                    last_text: "And a reminder: the RustConf CFP closes on friday.".into(),
                    ..chat_fixture("Rust Weekly", PeerKind::Channel, "")
                },
                false,
                false,
            ),
        )
        .about("a channel's post names nobody; muted, so the count is outlined")
        .node(
            "older",
            row(
                ChatRow {
                    last: ts(2026, 8, 12, 15, 20),
                    last_out: true,
                    last_state: Some("read".into()),
                    last_text: "sure, send the doc over".into(),
                    ..chat_fixture("Anna Schmidt", PeerKind::Person, "")
                },
                false,
                false,
            ),
        )
        .about("past a week, the day")
        .node("cursor", row(vera(), true, false))
        .about("the wash under the cursor; focus stays in the list")
        .node("marked", row(vera(), false, true))
        .about("a dark bar marks the row without changing its size")
        .node(
            "narrow",
            row(
                ChatRow {
                    unread: 8,
                    unread_mentions: 1,
                    pinned: 2,
                    last_text: "meeting moved to 15:00".into(),
                    last_from: "Ivan Petrov".into(),
                    ..chat_fixture("stelaxis", PeerKind::Group, "")
                },
                false,
                false,
            ),
        )
        .sized((320.0, 52.0))
        .about("the phone's width")
        .edge("read", "unread", "a line arrives")
        .edge("read", "cursor", "↓ / click")
        .edge("read", "marked", "space")
        .edge("read", "draft", "type, leave")
}

/// The list itself, live: the walk, the filter, the marks, the archive.
fn chats() -> Scene<Setup> {
    let list = |script: &str| panel(|_| Chats::id(), script);
    Scene::new("chats", (520.0, 640.0))
        .note("The chat list: pinned chats first, then by the last line's time; the filter above, the bar at the foot.")
        .note("Live — enter a node and walk it: a row previews the chat, space marks, / filters.")
        .node("fresh", list(""))
        .about("two pinned, then the rest; the counts at the right")
        .node("cursor", list("key down 3\nwait 500"))
        .about("the walk previews; the list keeps the keyboard")
        .node("unread", list("key /\nwait 300\ntype \"@unread\"\nwait 500"))
        .about("what has something unread in it")
        .node("folder", list("key /\nwait 300\ntype \"@folder:work\"\nwait 500"))
        .about("a folder is a filter: the chats it holds")
        .node(
            "marked",
            list("key down\nwait 300\nkey space\nwait 300\nkey shift+down 2\nwait 500"),
        )
        .about("space marks the cursor's row, shift+↓ the two under it; the bar grows the batch verbs")
        .node("archive", panel(|_| Chats::archive(), ""))
        .about("what was put away — the same rows, the other list")
        .node(
            "joined",
            workspace_on(|_| Chats::id(), "key down\nwait 700"),
        )
        .sized((1200.0, 700.0))
        .about("not solo: the chat the walk previews, joined to the right of the list that drives it")
        .edge("fresh", "cursor", "↓ ×3")
        .edge("fresh", "unread", "/ @unread")
        .edge("fresh", "folder", "/ @folder:work")
        .edge("cursor", "marked", "space")
        .edge("cursor", "joined", "the same walk, in a workspace")
}

/// One line of a transcript, in each of its states. Populated by the
/// function the live transcript calls.
fn message_row() -> Scene<Setup> {
    let row = |r: Row, selected: bool, marked: bool| {
        widget(live_id!(telegram_msg_row_tpl), move |cx, w| {
            super::widgets::chat::populate(
                cx, w, &r, (selected, marked), None,
                &super::widgets::RenderContext { now: virtual_epoch(), store_dir: None },
            );
        })
    };
    let line = |m: Msg| Row::Message { msg: m, run: false };
    let vera = |text: &str| msg_fixture("Vera Kovac", text, today(11, 50));
    Scene::new("message row", (560.0, 72.0))
        .note("One line of a chat: the writer and the time on a header, the line under it, and whatever it carries about itself.")
        .note("Nothing is aligned by who wrote it — a transcript in one face reads by names, as a log does.")
        .node("theirs", row(line(vera("the forecast changed, rain after 2pm")), false, false))
        .node("links", row(line(Msg {
            entities: Some(vec![super::text::Entity {
                offset: 3, length: 13,
                kind: super::text::EntityKind::TextUrl { url: "https://example.org/project".into() },
            }, super::text::Entity {
                offset: 19, length: 33,
                kind: super::text::EntityKind::Url,
            }]),
            ..vera("👋 project notes — https://example.org/notes?a=1&b=2")
        }), false, false))
        .sized((560.0, 100.0))
        .node("mine, read", row(line(mine("then we start earlier. 7:00?", "read")), false, false))
        .about("`me`, quiet; two ticks, read")
        .node("mine, sent", row(line(mine("the egress line is stale, I'll redo it", "sent")), false, false))
        .about("one tick: it left, nobody has read it")
        .node("failed", row(line(mine("and the thermos", "failed")), false, false))
        .about("it never left: the one colour errors get")
        .node(
            "run",
            row(
                Row::Message {
                    msg: vera("I'll bring the map"),
                    run: true,
                },
                false,
                false,
            ),
        )
        .sized((560.0, 40.0))
        .about("the second line of a writer's run: no header at all")
        .node(
            "edited",
            row(
                line(Msg {
                    edited: true,
                    ..vera("the forecast changed, rain after 2pm")
                }),
                false,
                false,
            ),
        )
        .about("said so, muted, before the time")
        .node(
            "forwarded",
            row(
                line(Msg {
                    fwd_from: Some("Elena Petrova".into()),
                    ..msg_fixture("Max Ivanov", "Q3 infra budget draft is ready for review", today(9, 0))
                }),
                false,
                false,
            ),
        )
        .about("whom it came from, under the header")
        .node(
            "reply",
            row(
                line(Msg {
                    reply_to: Some(9),
                    reply_name: "Anna Schmidt".into(),
                    reply_text: "the cover display clips the header".into(),
                    ..vera("I saw it — the inset is 28 dp, it wants 34")
                }),
                false,
                false,
            ),
        )
        .about("the line it answers, quoted in one line")
        .node(
            "reactions",
            row(
                line(Msg {
                    reactions: Some("👍 3 · 🔥 1".into()),
                    ..vera("new palette, what do you think")
                }),
                false,
                false,
            ),
        )
        .about("the reactions under the line")
        .node(
            "post",
            row(
                line(Msg {
                    sender: None,
                    sender_name: "Rust Weekly".into(),
                    views: Some(4_300),
                    comments: Some(8),
                    reactions: Some("❤️ 34 · 🦀 12".into()),
                    ..msg_fixture(
                        "Rust Weekly",
                        "Issue 612: the 2027 edition survey, cargo-script lands in nightly.",
                        today(8, 15),
                    )
                }),
                false,
                false,
            ),
        )
        .about("a channel post: its views by the time, its comments under it")
        .node(
            "service",
            row(
                Row::Service(Msg {
                    service: true,
                    sender: None,
                    ..msg_fixture("", "Ivan Petrov joined the group", ts(2026, 8, 25, 10, 0))
                }),
                false,
                false,
            ),
        )
        .sized((560.0, 30.0))
        .about("a line nobody wrote, muted")
        .node("day", row(Row::Day("YESTERDAY".into()), false, false))
        .sized((560.0, 30.0))
        .about("a day's caption over a hairline")
        .node("unread", row(Row::Unread, false, false))
        .sized((560.0, 30.0))
        .about("where the reading starts: a caption over a dark rule")
        .node("cursor", row(line(vera("the forecast changed, rain after 2pm")), true, false))
        .about("the wash under the cursor")
        .node("marked", row(line(vera("the forecast changed, rain after 2pm")), false, true))
        .about("a dark bar, for the batch verbs on the bar")
        .edge("theirs", "cursor", "↓ / click")
        .edge("theirs", "marked", "space")
        .edge("theirs", "run", "the same writer, within five minutes")
}

/// What a line carries, drawn: the nine kinds of media, through the kit.
fn media() -> Scene<Setup> {
    let row = |r: Row| {
        widget(live_id!(telegram_msg_row_tpl), move |cx, w| {
            let player = match &r {
                Row::Message { msg, .. } => msg.media.as_ref().and_then(|md| md.secs).map(|s| PlayerState {
                    playing: false,
                    position: 0.0,
                    length: s as f64,
                }),
                _ => None,
            };
            super::widgets::chat::populate(
                cx, w, &r, (false, false), player,
                &super::widgets::RenderContext { now: virtual_epoch(), store_dir: None },
            );
        })
    };
    let playing = |r: Row, position: f64| {
        widget(live_id!(telegram_msg_row_tpl), move |cx, w| {
            let player = match &r {
                Row::Message { msg, .. } => msg.media.as_ref().and_then(|md| md.secs).map(|s| PlayerState {
                    playing: true,
                    position,
                    length: s as f64,
                }),
                _ => None,
            };
            super::widgets::chat::populate(
                cx, w, &r, (false, false), player,
                &super::widgets::RenderContext { now: virtual_epoch(), store_dir: None },
            );
        })
    };
    let with = |m: Media, name: &str, text: &str, at: f64| {
        Row::Message {
            msg: Msg {
                media: Some(m),
                ..msg_fixture(name, text, at)
            },
            run: false,
        }
    };
    let m = |kind: &str| Media::of(kind);
    Scene::new("media", (560.0, 300.0))
        .note("What a line carries: a photo and a video's poster are drawn, a sticker is its emoji, and everything else is spelled on a line — the word, and what is known.")
        .note("Sending each of them is the plan's fourth phase: a file held by the files app through attach, a paste, a recording, a location the device answers with.")
        .node(
            "photo",
            row(with(
                Media {
                    reference: Some("demo:palette".into()),
                    w: Some(480),
                    h: Some(300),
                    ..m("photo")
                },
                "Vera Kovac",
                "new palette, what do you think",
                today(11, 40),
            )),
        )
        .about("the picture, at most 320 wide, over its caption")
        .node(
            "photo, no bytes",
            row(with(
                Media {
                    w: Some(1280),
                    h: Some(960),
                    ..m("photo")
                },
                "Vera Kovac",
                "",
                today(11, 40),
            )),
        )
        .sized((560.0, 48.0))
        .about("the bytes not here yet: the word and the size, where the picture will be")
        .node(
            "video",
            row(with(
                Media {
                    reference: Some("demo:palette".into()),
                    w: Some(480),
                    h: Some(300),
                    secs: Some(14),
                    ..m("video")
                },
                "Elena Petrova",
                "the fold in motion",
                today(11, 41),
            )),
        )
        .about("a poster, and the player under it: play, the progress, the time")
        .node(
            "video message",
            row(with(
                Media {
                    reference: Some("demo:garden".into()),
                    w: Some(480),
                    h: Some(360),
                    secs: Some(8),
                    ..m("circle")
                },
                "Vera Kovac",
                "",
                today(11, 49),
            )),
        )
        .about("a round video message: its poster, square here, and its length")
        .node(
            "sticker",
            row(with(
                Media {
                    label: Some("🙈".into()),
                    ..m("sticker")
                },
                "Olga Novak",
                "",
                today(11, 26),
            )),
        )
        .sized((560.0, 80.0))
        .about("its emoji, drawn large, until stickers are drawn")
        .node(
            "voice",
            row(with(
                Media {
                    secs: Some(42),
                    ..m("voice")
                },
                "Max Ivanov",
                "",
                today(11, 30),
            )),
        )
        .sized((560.0, 64.0))
        .about("a voice note: the player, at rest")
        .node(
            "voice, playing",
            playing(
                with(
                    Media {
                        secs: Some(42),
                        ..m("voice")
                    },
                    "Max Ivanov",
                    "",
                    today(11, 30),
                ),
                17.0,
            ),
        )
        .sized((560.0, 64.0))
        .about("running: pause, and the hairline filled to where it stands")
        .node(
            "audio",
            row(with(
                Media {
                    label: Some("Dry Cleaning — Scratchcard Lanyard".into()),
                    secs: Some(221),
                    ..m("audio")
                },
                "Andrey Rudenko",
                "",
                ts(2026, 8, 20, 8, 31),
            )),
        )
        .sized((560.0, 64.0))
        .about("a track by its title and length, and its player")
        .node(
            "file",
            row(with(
                Media {
                    label: Some("report-q3.pdf · 2.1 MB".into()),
                    ..m("file")
                },
                "Max Ivanov",
                "the numbers",
                today(9, 3),
            )),
        )
        .about("a file by its name and size, under the text it came with")
        .node(
            "location",
            row(with(
                Media {
                    lat: Some(47.0472),
                    lon: Some(8.3164),
                    ..m("location")
                },
                "Ivan Petrov",
                "",
                ts(2026, 8, 29, 20, 6),
            )),
        )
        .sized((560.0, 220.0))
        .about("a place on the map, the pin at its centre; a press opens the line's card, whose bar opens it in Maps or a browser")
        .node(
            "live location",
            row(with(
                Media {
                    lat: Some(55.7512),
                    lon: Some(37.6184),
                    until: Some(today(12, 42)),
                    ..m("live")
                },
                "Sergey Rudenko",
                "",
                today(9, 29),
            )),
        )
        .sized((560.0, 220.0))
        .about("a place that moves, and how long it goes on being shared")
        .edge("photo", "photo, no bytes", "before the bytes land")
        .edge("video", "video message", "round")
        .edge("voice", "voice, playing", "play")
        .edge("location", "live location", "shared as it moves")
}

/// What goes with the next message: one row, and the panel joined to its
/// chat — empty, carrying two files, recording, and the place.
fn attach() -> Scene<Setup> {
    let row = |path: &'static str, selected: bool| {
        widget(live_id!(telegram_attach_row_tpl), move |cx, w| {
            super::widgets::attach::populate(cx, w, &Carried { path: path.to_string() }, selected);
        })
    };
    let vera = |script: &str| workspace_on(|_| Chat::id(VERA), script);
    Scene::new("attach", (1200.0, 700.0))
        .note("`attach` on the chat's bar opens what goes with the next message, joined to the chat: the files the composer will send, in the order they will go, and the ways to make more of it — `browse` the files app and `add` what it holds, `voice`, `video`, `place`.")
        .note("One thing at a time, as the clients have it: files go together with the text; a voice note, a video message and a place each go on their own, at once — the list gives way while a recording runs and comes back when it has gone.")
        .note("Live — enter a node: arrows walk the rows, cmd+r removes one, cmd+e and cmd+a trade it with its neighbours; cmd+o starts a voice note and the clock runs, enter sends it (a toast this round), esc throws it away.")
        .node("file", row("~/Downloads/report-q3.pdf", false))
        .sized((520.0, 52.0))
        .about("one row: the name, and under it what it goes as and where it is")
        .node("photo", row("~/Downloads/screenshot-2026-08-30.png", true))
        .sized((520.0, 52.0))
        .about("a picture goes as a photo; the cursor's wash")
        .node("empty", vera("key cmd+h\nwait 700"))
        .about("nothing yet: `browse` is the link to the files panel, `add` comes beside it while the files app holds something — which on this canvas another node's copy may leave it holding — and the recordings and the place follow")
        .node("carrying", vera(CARRYING))
        .about("two files marked in Downloads, copied, and added: the rows here, the CARRIES line on the composer, and `attach 2` on the chat's bar")
        .node("voice", vera("key cmd+h\nwait 700\nkey cmd+o\nwait 3400"))
        .about("a voice note under way: what and how long, the level, and the keys on the line — `send` and `discard` on the bar")
        .node("voice over files", vera(&format!("{CARRYING}\nkey cmd+o\nwait 2000")))
        .about("a voice note while two files wait: the list gives way to the strip, the note goes on its own, and the files come back for the text")
        .node("video message", vera("key cmd+h\nwait 700\nkey cmd+v\nwait 2200"))
        .about("a video message under way: the camera's picture — faked — over the same strip")
        .node("place", panel(|_| Place::id(VERA), ""))
        .sized((520.0, 300.0))
        .about("where you are, on the map: `send` once, or `live 1 h`")
        .edge("empty", "carrying", "browse, copy, add")
        .edge("empty", "voice", "cmd+o")
        .edge("carrying", "voice over files", "cmd+o")
        .edge("voice", "video message", "the other recording")
        .edge("empty", "place", "cmd+p")
}

/// The way to two carried files, on the demo disk: the attach panel, the
/// files panel off it, Downloads entered, two rows marked and copied, `add
/// 2` back on the attach panel, and the files panels closed again so the
/// chat and the attach panel stand together.
const CARRYING: &str = "key cmd+h
wait 700
key cmd+b
wait 800
click \"Downloads/\"
wait 600
key enter
wait 800
click \"report-q3.pdf\"
wait 500
key space
wait 300
click \"screenshot-2026-08-30.png\"
wait 500
key space
wait 300
key cmd+p
wait 500
key cmd+left 2
wait 500
key cmd+d
wait 700
key cmd+right
wait 300
key cmd+w
wait 700";

/// One line as a card: whole, with its media at the card's width and the
/// verbs on one line on its bar.
fn line() -> Scene<Setup> {
    let at = |find: &'static str| {
        panel(
            move |s| {
                let hist = model::history(s, STELAXIS);
                let m = hist.iter().find(|m| m.text.contains(find) || m.media.as_ref().is_some_and(|md| md.kind == find)).expect("a seeded line");
                Line::id(STELAXIS, m.id)
            },
            "",
        )
    };
    Scene::new("line", (520.0, 420.0))
        .note("One line, whole: reached by `line` from the chat, over the line under the cursor, and joined to it — so `reply` here lands on the chat's composer.")
        .note("The verbs on one line live here and not on the chat's bar: edit, forward, delete, pin; play over a recording; open, to the viewer; and a place's two ways out.")
        .node("text", at("inset is 28"))
        .about("a reply, whole, with what it answers")
        .node("photo", at("photo"))
        .about("the picture at the card's width; a press on it, or `open`, is the viewer")
        .node("video", at("video"))
        .about("the poster and the player")
        .node("voice", at("voice"))
        .sized((520.0, 220.0))
        .about("the player alone")
        .node(
            "place",
            panel(
                |s| {
                    let hist = model::history(s, HIKE);
                    let m = hist.iter().find(|m| m.media.as_ref().is_some_and(|md| md.kind == "location")).expect("a seeded place");
                    Line::id(HIKE, m.id)
                },
                "",
            ),
        )
        .about("a place: the map, and `maps` and `browser` on the bar")
        .node(
            "joined",
            workspace_on(|_| Chat::id(STELAXIS), "key up\nwait 300\nkey cmd+n\nwait 800"),
        )
        .sized((1200.0, 700.0))
        .about("from the chat: ↑ puts the cursor on the last line, cmd+n opens its card beside it")
        .edge("text", "photo", "another line")
        .edge("photo", "joined", "as opened from the chat")
}

/// The viewer: one line's media as large as the grid allows.
fn viewer() -> Scene<Setup> {
    let over = |find: &'static str| {
        panel(
            move |s| {
                let hist = model::history(s, STELAXIS);
                let m = hist.iter().find(|m| m.media.as_ref().is_some_and(|md| md.kind == find)).expect("a seeded line");
                Viewer::id(STELAXIS, m.id)
            },
            "",
        )
    };
    Scene::new("viewer", (1440.0, 900.0))
        .note("Full screen, in a grammar of columns: a panel that asks for the whole grid. It is still a panel — joined to what opened it, closed with cmd+w, undone with cmd+z.")
        .note("`previous` and `next` walk the chat's media in place; `play` runs a recording; `open` hands the bytes to the system.")
        .node("photo", over("photo"))
        .about("the picture fitted to the panel")
        .node("video", over("video"))
        .about("the poster, and the player under it")
        .node("voice", over("voice"))
        .about("a sound has no face: the word, and the player")
        .node(
            "next",
            panel(
                |s| {
                    let hist = model::history(s, STELAXIS);
                    let m = hist.iter().find(|m| m.media.as_ref().is_some_and(|md| md.kind == "photo")).expect("a seeded line");
                    Viewer::id(STELAXIS, m.id)
                },
                "click \"next\"\nwait 700",
            ),
        )
        .about("the next of the chat's media, in the same panel")
        .edge("photo", "next", "next")
        .edge("photo", "video", "another kind")
}

/// A conversation, live: a person, a group, the two kinds of channel, the
/// reply line, the composer.
fn chat() -> Scene<Setup> {
    Scene::new("chat", (640.0, 640.0))
        .note("One conversation: the status line under the header, the transcript, and the composer at the foot.")
        .note("Live — enter a node: esc puts the keyboard on the lines and the arrows walk them; over the cursor's line cmd+r replies, and over one of mine cmd+e edits it in the composer and cmd+d deletes it, both real and undone by cmd+z; enter sends; cmd+h opens what goes with the next message, joined.")
        .node("person", panel(|_| Chat::id(VERA), ""))
        .about("a person: online, the runs, a reply, an edit, my two ticks")
        .node("group", panel(|_| Chat::id(STELAXIS), ""))
        .about("a group: names, a service line, a forward, a file, a photo, a video, a sticker, a voice note, and the unread line where the reading starts")
        .node("channel", panel(|_| Chat::id(RUST_WEEKLY), ""))
        .about("a channel I read: views and comments on every post, and no composer")
        .node("own channel", panel(|_| Chat::id(DEV), ""))
        .about("a channel I run: the composer says broadcast")
        .node(
            "reply",
            panel(|_| Chat::id(VERA), "key esc\nwait 200\nkey up\nwait 300\nkey cmd+r\nwait 500"),
        )
        .about("esc puts the keyboard on the lines, ↑ the cursor on the last one, and cmd+r: the reply line stands above the composer, and the caret is back in it")
        .node(
            "edit",
            panel(|_| Chat::id(VERA), "key esc\nwait 200\nkey up\nwait 300\nkey cmd+e\nwait 500"),
        )
        .about("the cursor on my own line, and cmd+e: the composer takes its text under a line saying which; enter writes it, esc lets it go, cmd+z gives the old text back")
        .node(
            "typed",
            panel(
                |_| Chat::id(ELENA),
                "click \"write a message\"\nwait 300\ntype \"8 is fine, see you there\"\nwait 500",
            ),
        )
        .about("what is typed lands in the composer and nowhere else — and the list shows it as a draft")
        .node("draft", panel(|_| Chat::id(HIKE), ""))
        .about("a chat that already holds a draft comes up with it in the composer")
        .node("saved", panel(|_| Chat::id(SELF), ""))
        .about("the chat with oneself")
        .node("empty", panel(|_| Chat::id(IVAN), ""))
        .sized((640.0, 240.0))
        .about("a person who has never written: nothing yet, and the composer to start")
        .node(
            "joined",
            workspace_on(|_| Chats::id(), "key down 2\nwait 700"),
        )
        .sized((1200.0, 700.0))
        .about("previewed from the list: the chat joined to the right, the list keeping the keyboard")
        .edge("person", "reply", "esc, ↑, cmd+r")
        .edge("reply", "edit", "cmd+e instead")
        .edge("person", "typed", "the composer")
        .edge("group", "channel", "another kind of peer")
        .edge("channel", "own channel", "one I run")
}

/// The messages a filter finds, everywhere and in one chat.
fn messages() -> Scene<Setup> {
    let all = |script: &str| panel(|_| Messages::id(), script);
    Scene::new("messages", (520.0, 640.0))
        .note("The messages a filter finds: where and who with the time, then the line. The cursor previews the chat opened at that line.")
        .note("Bare, it is every chat; opened about one, its field says so.")
        .node("everywhere", all(""))
        .about("every line in every chat, latest first")
        .node("found", all("key /\nwait 300\ntype \"map\"\nwait 500"))
        .about("a word narrows them")
        .node("by writer", all("key /\nwait 300\ntype \"@from:vera\"\nwait 500"))
        .about("one writer's lines, wherever they are")
        .node("in a chat", panel(|_| Messages::in_chat(STELAXIS), ""))
        .about("opened from a chat's bar: narrowed to it, and the title says which")
        .edge("everywhere", "found", "/ map")
        .edge("everywhere", "by writer", "/ @from:vera")
        .edge("everywhere", "in a chat", "search, on a chat's bar")
}

/// The people: one row in its states, the address book, a group's members.
fn people() -> Scene<Setup> {
    let row = |p: Person, selected: bool, marked: bool| {
        widget(live_id!(telegram_person_row_tpl), move |cx, w| {
            PeopleRows::populate(cx, w, &p, selected, marked, kernel::time::virtual_epoch());
        })
    };
    let person = |name: &str, username: &str, status: &str| Person {
        id: 2,
        name: name.to_string(),
        username: Some(username.to_string()),
        status: Some(status.to_string()),
        admin: false,
        group: None,
    };
    Scene::new("people", (520.0, 520.0))
        .note("The people: the name, and under it the presence and the username.")
        .note("The address book previews the chat with a person, which is how a new conversation starts; a group's members preview the member's card.")
        .node("online", row(person("Vera Kovac", "vera", "online"), false, false))
        .sized((520.0, 52.0))
        .node(
            "last seen",
            row(person("Max Ivanov", "maxiv", "week"), false, false),
        )
        .sized((520.0, 52.0))
        .about("the presence as the client spells it")
        .node(
            "admin",
            row(
                Person {
                    admin: true,
                    group: Some(STELAXIS),
                    ..person("Vera Kovac", "vera", "online")
                },
                false,
                false,
            ),
        )
        .sized((520.0, 52.0))
        .about("a member who administers the group says so")
        .node("cursor", row(person("Vera Kovac", "vera", "online"), true, false))
        .sized((520.0, 52.0))
        .node("contacts", panel(|_| Contacts::id(), ""))
        .about("the address book, by name")
        .node(
            "online only",
            panel(|_| Contacts::id(), "key /\nwait 300\ntype \"@online\"\nwait 500"),
        )
        .about("who is here right now")
        .node("members", panel(|_| Members::id(STELAXIS), ""))
        .about("one group's members, the field saying which")
        .node(
            "new chat",
            workspace_on(|_| Contacts::id(), "key down 2\nwait 700"),
        )
        .sized((1200.0, 700.0))
        .about("the walk previews the chat with the person under the cursor")
        .edge("online", "cursor", "↓ / click")
        .edge("contacts", "online only", "/ @online")
        .edge("contacts", "new chat", "the same walk, in a workspace")
}

/// A peer's card, for each kind of peer.
fn peer() -> Scene<Setup> {
    Scene::new("peer", (480.0, 300.0))
        .note("Who or what a chat is with: the name, one line saying what it is, a phone number where there is one, and what is written about them under a rule.")
        .note("The ways off it are on the bar: the chat, its messages, and, for a group, who is in it.")
        .node("person", panel(|_| Peer::id(VERA), ""))
        .about("a contact: the username, the presence, the number, the bio")
        .node("confirm block", panel(|_| Peer::id(VERA), "click \"block user\"\nwait 300"))
        .about("blocking names its consequence before confirmation")
        .node("blocked", panel(|store| {
            store.write(|c| model::set_blocked_tx(c, VERA, true)).expect("blocked fixture");
            Peer::id(VERA)
        }, ""))
        .about("a blocked contact keeps its history and offers unblock")
        .node("blocked conversation", panel(|store| {
            store.write(|c| model::set_blocked_tx(c, VERA, true)).expect("blocked fixture");
            Chat::id(VERA)
        }, ""))
        .sized((480.0, 600.0))
        .about("the transcript stays readable; unblock restores the composer")
        .node("group", panel(|_| Peer::id(STELAXIS), ""))
        .about("a group: how many, how many online, the description — and members on the bar")
        .node("channel", panel(|_| Peer::id(RUST_WEEKLY), ""))
        .about("a channel: its subscribers and its handle")
        .node("stranger", panel(|_| Peer::id(MAX), ""))
        .about("a contact seen within the week")
        .node("family", panel(|_| Peer::id(FAMILY), ""))
        .sized((480.0, 220.0))
        .about("a group with nothing written about it says so")
        .node(
            "members",
            workspace_on(|_| Peer::id(STELAXIS), "click \"members\"\nwait 700"),
        )
        .sized((1200.0, 700.0))
        .about("the link followed: the members joined to the right of the card")
        .edge("group", "members", "members")
        .edge("person", "group", "another kind of peer")
}
