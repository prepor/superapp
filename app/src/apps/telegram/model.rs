//! Telegram's rows, its queries, and the spellings its panels use.
//!
//! The three lists page through rich-table sources over the store; the
//! transcript and the card read through registered queries, so a commit
//! under a panel reaches its next draw. The spellings — the hour today and
//! the weekday inside a week, the presence line, a count with its `k` — are
//! pure functions here, and tested as such.

use std::path::{Path, PathBuf};

use kernel::filter::Op;
use kernel::panel::{PanelId, Tag};
use kernel::richtable::{Dir, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, Values};
use kernel::store::{Q, Store, Val};
use kernel::time::civil_from_days;

use crate::shell::widgets::media::PlayerState;

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// A peer's id, which is its chat's id too.
pub type PeerId = i64;
/// A message's id.
pub type MsgId = i64;

// -- what a peer is -----------------------------------------------------------------

/// Who or what a chat is with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerKind {
    Person,
    Group,
    Channel,
}

impl PeerKind {
    /// The store's word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PeerKind::Person => "person",
            PeerKind::Group => "group",
            PeerKind::Channel => "channel",
        }
    }

    /// The kind a store word names; anything unknown reads as a person.
    #[must_use]
    pub fn of(s: &str) -> PeerKind {
        match s {
            "group" => PeerKind::Group,
            "channel" => PeerKind::Channel,
            _ => PeerKind::Person,
        }
    }
}

// -- rows ---------------------------------------------------------------------------

/// One row of the chat list: the chat, its flags, and its last message.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRow {
    pub peer: PeerId,
    pub topic: i64,
    pub is_forum: bool,
    pub kind: PeerKind,
    pub title: String,
    /// 0 for an unpinned chat, else its place among the pinned ones.
    pub pinned: i64,
    pub muted: bool,
    pub unread: i64,
    pub unread_mentions: i64,
    pub draft: Option<String>,
    /// Who is typing, in a word.
    pub typing: Option<String>,
    /// The last message's time; 0 where there is none.
    pub last: f64,
    pub last_text: String,
    pub last_media: Option<Media>,
    pub last_out: bool,
    pub last_state: Option<String>,
    /// The last message's writer, by first name; empty for a channel's post
    /// and a service line.
    pub last_from: String,
    pub last_service: bool,
}

impl ChatRow {
    pub fn key(&self) -> String {
        format!("{}:{}", self.peer, self.topic)
    }

    /// The second line: what the chat last said, in a word or two. A draft
    /// or somebody typing takes the line over.
    #[must_use]
    pub fn preview(&self, now: f64) -> String {
        if let Some(who) = &self.typing {
            return match self.kind {
                PeerKind::Person => "typing…".to_string(),
                _ => format!("{} is typing…", first_name(who)),
            };
        }
        if let Some(d) = self.draft.as_deref().filter(|d| !d.trim().is_empty()) {
            return format!("draft: {}", one_line(d));
        }
        if self.last == 0.0 {
            return String::new();
        }
        if self.last_service {
            return self.last_text.clone();
        }
        let what = media_or_text(self.last_media.as_ref(), &self.last_text, now);
        let who = if self.last_out {
            "me"
        } else if self.kind == PeerKind::Group {
            &self.last_from
        } else {
            ""
        };
        if who.is_empty() {
            what
        } else {
            format!("{}: {what}", first_name(who))
        }
    }

    /// The state of my last message, drawn before the time: `sent`,
    /// `read`, `failed`, nothing for anyone else's.
    #[must_use]
    pub fn state_mark(&self) -> &'static str {
        if !self.last_out {
            return "";
        }
        state_mark(self.last_state.as_deref())
    }
}

/// The word a message of mine wears for its state. Words, not ticks: the
/// bundled faces carry no check mark, and a word reads on any of them.
#[must_use]
pub fn state_mark(state: Option<&str>) -> &'static str {
    match state {
        Some("read") => "read",
        Some("failed") => "failed",
        Some("sending") => "sending…",
        _ => "sent",
    }
}

/// What one line says for a message: its text, with the media in a word
/// after it where it carries any; the media spelled out where there is no
/// text at all.
#[must_use]
pub fn media_or_text(media: Option<&Media>, text: &str, now: f64) -> String {
    let text = one_line(text);
    match media {
        Some(m) if text.is_empty() => m.line(now),
        Some(m) => format!("{text} · {}", m.word()),
        None => text,
    }
}

// -- media --------------------------------------------------------------------------

/// The latest byte counts for one download. An unknown total stays unknown;
/// TDLib's expected size is only an estimate until it reports the real size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub estimated: bool,
}

impl DownloadProgress {
    #[must_use]
    pub fn note(self) -> String {
        use kernel::caps::fmt_size;

        let downloaded = fmt_size(self.downloaded);
        match self.total {
            Some(total) => format!(
                "downloading · {downloaded} / {}{}",
                if self.estimated { "~" } else { "" },
                fmt_size(total)
            ),
            None => format!("downloading · {downloaded} · total unknown"),
        }
    }
}

/// What a message carries besides its text: one of nine kinds, and what is
/// known about it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Media {
    /// `photo`, `video`, `circle`, `sticker`, `voice`, `audio`, `file`,
    /// `location`, `live`.
    pub kind: String,
    /// A file's name and size, an audio track's title, a sticker's emoji.
    pub label: Option<String>,
    /// Where the bytes are: `demo:` a bundled picture this round.
    pub reference: Option<String>,
    /// What the file `reference` names is asked for by when its bytes are
    /// not on this device: TDLib's `remoteFile.id`, which outlives a
    /// session, where the `file.id` a download runs on is only this run's.
    /// A photo's largest size, a moving picture's poster. `None` for a demo
    /// line, and for anything that names no file at all.
    pub rid: Option<String>,
    pub w: Option<i64>,
    pub h: Option<i64>,
    /// A recording's length.
    pub secs: Option<i64>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// Until when a live location is shared.
    pub until: Option<f64>,
    /// A moving picture's clip, where the bytes will be once they are here:
    /// `reference` is the poster, which arrives with the line, and this is
    /// the file behind it, which nobody fetches until the viewer is opened
    /// on it. `None` for everything that is not a video, an animation or a
    /// video note.
    pub clip: Option<String>,
    /// What the clip is asked for by: TDLib's `remoteFile.id`, the id that
    /// outlives a session. The row keeps the durable one; the worker turns
    /// it back into this run's file id when the player asks.
    pub clip_rid: Option<String>,
}

impl Media {
    /// One of a kind, with nothing else known about it.
    #[must_use]
    pub fn of(kind: &str) -> Media {
        Media {
            kind: kind.to_string(),
            ..Media::default()
        }
    }

    /// The kind, as a list names it after a caption: *the garden today ·
    /// photo*.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self.kind.as_str() {
            "photo" => "photo",
            "video" => "video",
            "circle" => "video message",
            "sticker" => "sticker",
            "voice" => "voice",
            "audio" => "audio",
            "file" => "file",
            "location" => "location",
            "live" => "live location",
            _ => "media",
        }
    }

    /// The media spelled out on a line of its own: the word, and what is
    /// known — a picture's size, a recording's length, a file's name, a
    /// location's coordinates and, while it moves, how long it goes on.
    #[must_use]
    pub fn line(&self, now: f64) -> String {
        let word = self.word();
        let label = self.label.as_deref().filter(|l| !l.is_empty());
        match self.kind.as_str() {
            "photo" => match (self.w, self.h) {
                (Some(w), Some(h)) => format!("{word} {w}×{h}"),
                _ => word.to_string(),
            },
            "video" | "circle" | "voice" => match self.secs {
                Some(s) => format!("{word} {}", fmt_secs(s)),
                None => word.to_string(),
            },
            "audio" => match (label, self.secs) {
                (Some(l), Some(s)) => format!("{word} {l} · {}", fmt_secs(s)),
                (Some(l), None) => format!("{word} {l}"),
                (None, Some(s)) => format!("{word} {}", fmt_secs(s)),
                (None, None) => word.to_string(),
            },
            "location" | "live" => {
                let mut s = match (self.lat, self.lon) {
                    (Some(lat), Some(lon)) => format!("{word} {lat:.4}, {lon:.4}"),
                    _ => word.to_string(),
                };
                if self.kind == "live" {
                    if let Some(until) = self.until {
                        s.push_str(&format!(" · {}", live_left(until, now)));
                    }
                }
                s
            }
            _ => match label {
                Some(l) => format!("{word} {l}"),
                None => word.to_string(),
            },
        }
    }

    /// The bytes of the picture, or of a video's poster: a bundled demo
    /// image, or a downloaded blob read from the cache beside `store_dir`.
    /// Only a photo, a video or a circle has one; every other kind draws as a
    /// line. `None` where the reference names nothing yet on this device — a
    /// `tg:` file not yet downloaded — so the box stays a label until a redraw
    /// finds the bytes. See [`media_bytes`].
    #[must_use]
    pub fn picture_bytes(&self, store_dir: Option<&Path>) -> Option<Vec<u8>> {
        if !self.has_picture() {
            return None;
        }
        media_bytes(store_dir, self.reference.as_deref()?)
    }

    /// Whether the kind draws as a picture at all — a photo, or a moving
    /// picture's poster. A file, a sound, a place, a sticker have none to
    /// wait for.
    #[must_use]
    pub fn has_picture(&self) -> bool {
        matches!(self.kind.as_str(), "photo" | "video" | "circle")
    }

    /// Whether the row draws it as a picture, a line, or an emoji drawn
    /// large.
    #[must_use]
    pub fn is_sticker(&self) -> bool {
        self.kind == "sticker"
    }
}

/// The bytes to draw for a media `reference`, or `None` where none are on
/// this device yet.
///
/// Two worlds meet here, both additive to nothing else. A `demo:` reference
/// reads one of the pictures this build bundles, exactly as the round-one
/// world always has — it never touches disk and ignores `store_dir`. A
/// `tg:<unique id>` reference is a downloaded blob: the worker ingested the
/// finished download into the [cache](kernel::caps::Blobs) beside the store,
/// which names each file by the SHA-256 of its key, so the file sits at
/// `<store_dir>/blobs/<file_name(reference)>`. If it is there, its bytes are
/// read and returned; if the download has not landed, `None` — the widget
/// keeps its label, and the next redraw after the bytes arrive resolves them.
/// Anything else, and a `tg:` reference with no store dir to look under, is
/// `None`.
#[must_use]
pub fn media_bytes(store_dir: Option<&Path>, reference: &str) -> Option<Vec<u8>> {
    if reference.starts_with("tg:") {
        let path = store_dir?
            .join("blobs")
            .join(kernel::caps::file_name(reference));
        return std::fs::read(path).ok();
    }
    super::seed::demo_bytes(reference).map(<[u8]>::to_vec)
}

/// The cached file for a media `reference`, as a path — what a player is
/// pointed at, where [`media_bytes`] hands a drawing widget the bytes.
///
/// A clip is not read into memory to be played: the platform's player wants
/// a file, so this answers where the blob cache put one. The same naming
/// [`media_bytes`] uses — `<store_dir>/blobs/<file_name(reference)>` — and
/// the same `None` where the download has not landed, which is what a draw
/// re-asks every frame until it has. A `demo:` reference is bundled in the
/// binary and has no path, so it answers `None`.
#[must_use]
pub fn media_path(store_dir: Option<&Path>, reference: &str) -> Option<PathBuf> {
    if !reference.starts_with("tg:") {
        return None;
    }
    let path = store_dir?
        .join("blobs")
        .join(kernel::caps::file_name(reference));
    path.is_file().then_some(path)
}

/// The cached clip as a path the platform's player will open. The cache
/// names a blob by its hash and nothing else, and AVFoundation tells a
/// file's container by its extension — a bare hash it refuses to prepare,
/// silently, and the widget retries forever (2026-09-07: every clip
/// downloaded and none played; makepad's own in-memory path writes a temp
/// file *with* the sniffed extension for this very reason). So the clip is
/// reached through a link beside the cache, `blobs-play/<hash>.<ext>`, the
/// extension read off the bytes. `None` while the blob is not there; a
/// link left over from an evicted blob is remade when the blob is.
#[must_use]
pub fn playable_path(store_dir: Option<&Path>, reference: &str) -> Option<PathBuf> {
    // The real path: a relative `--db` would make a relative link, which
    // points nowhere from the player's working directory.
    let blob = std::fs::canonicalize(media_path(store_dir, reference)?).ok()?;
    let ext = container_extension(&blob);
    let dir = store_dir?.join("blobs-play");
    let link = dir.join(format!("{}.{ext}", kernel::caps::file_name(reference)));
    if std::fs::symlink_metadata(&link).is_err() {
        std::fs::create_dir_all(&dir).ok()?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&blob, &link).ok()?;
        #[cfg(not(unix))]
        std::fs::copy(&blob, &link).ok()?;
    }
    Some(link)
}

/// The container a clip's first bytes say it is, as its file extension —
/// what the player goes by. MP4 and its kin carry `ftyp` at offset four;
/// WebM an EBML header; anything unrecognised is called mp4, which is what
/// Telegram sends.
fn container_extension(path: &Path) -> &'static str {
    let mut head = [0u8; 12];
    let n = std::fs::File::open(path)
        .and_then(|mut f| {
            use std::io::Read as _;
            f.read(&mut head)
        })
        .unwrap_or(0);
    let head = &head[..n];
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        "webm"
    } else if head.starts_with(b"OggS") {
        "ogg"
    } else if head.starts_with(b"RIFF") {
        "avi"
    } else {
        "mp4"
    }
}

/// A length as a player spells it: `0:42`, `3:41`, `1:02:05`.
#[must_use]
pub fn fmt_secs(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// How much longer a live location is shared: `42 min left`, `2 h left`,
/// `ended`.
#[must_use]
pub fn live_left(until: f64, now: f64) -> String {
    let left = ((until - now) / 60.0).ceil() as i64;
    if left <= 0 {
        "ended".to_string()
    } else if left >= 60 {
        format!("{} h left", left / 60)
    } else {
        format!("{left} min left")
    }
}

/// Reads the media block that starts at column `at`, or `None` where the
/// message carries nothing.
fn media_from_row(r: &rusqlite::Row, at: usize) -> rusqlite::Result<Option<Media>> {
    let Some(kind) = r.get::<_, Option<String>>(at)? else {
        return Ok(None);
    };
    Ok(Some(Media {
        kind,
        label: r.get(at + 1)?,
        reference: r.get(at + 2)?,
        rid: r.get(at + 3)?,
        w: r.get(at + 4)?,
        h: r.get(at + 5)?,
        secs: r.get(at + 6)?,
        lat: r.get(at + 7)?,
        lon: r.get(at + 8)?,
        until: r.get(at + 9)?,
        clip: r.get(at + 10)?,
        clip_rid: r.get(at + 11)?,
    }))
}

/// The first line of a text, for a row that has one line to spend.
#[must_use]
pub fn one_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// A name's first word.
#[must_use]
pub fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

/// One message as the transcript draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub content_type: Option<String>,
    pub topic: i64,
    pub id: MsgId,
    pub chat: PeerId,
    pub sender: Option<PeerId>,
    pub sender_name: String,
    pub date: f64,
    pub text: String,
    /// `None` means metadata is unavailable; an empty received list is known.
    pub entities: Option<Vec<super::text::Entity>>,
    pub out: bool,
    pub state: Option<String>,
    pub edited: bool,
    pub reply_to: Option<MsgId>,
    pub unread_mention: bool,
    /// Who wrote what it answers, and what they wrote.
    pub reply_name: String,
    pub reply_text: String,
    pub fwd_from: Option<String>,
    pub media: Option<Media>,
    pub views: Option<i64>,
    pub comments: Option<i64>,
    pub reactions: Option<String>,
    pub service: bool,
}

impl Msg {
    /// The writer's name as the header draws it: `me` for mine.
    #[must_use]
    pub fn writer(&self) -> &str {
        if self.out {
            "me"
        } else {
            &self.sender_name
        }
    }
}

/// One row of a messages list: where and who, then the line.
#[derive(Debug, Clone, PartialEq)]
pub struct MsgHit {
    /// The row's key in the store, which is what a mark holds: a message id
    /// is one only within its chat, and this list sweeps across chats.
    pub seq: i64,
    pub id: MsgId,
    pub chat: PeerId,
    pub topic: i64,
    pub chat_title: String,
    pub sender_name: String,
    pub date: f64,
    pub text: String,
    pub media: Option<Media>,
    pub out: bool,
}

impl MsgHit {
    /// The line itself with the media in a word, or the media where there
    /// is no text.
    #[must_use]
    pub fn line(&self, now: f64) -> String {
        media_or_text(self.media.as_ref(), &self.text, now)
    }

    /// Who wrote it: `me` for mine.
    #[must_use]
    pub fn writer(&self) -> &str {
        if self.out {
            "me"
        } else {
            &self.sender_name
        }
    }
}

/// One person as the people lists draw them.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub id: PeerId,
    pub name: String,
    pub username: Option<String>,
    pub status: Option<String>,
    pub admin: bool,
    /// The group this row is a member of — `None` on the contacts list,
    /// which is what tells the row where it opens.
    pub group: Option<PeerId>,
}

impl Person {
    /// The muted line under the name: the presence, and the username.
    #[must_use]
    pub fn detail(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.push(presence(self.status.as_deref()));
        if let Some(u) = &self.username {
            parts.push(format!("@{u}"));
        }
        if self.admin {
            parts.push("admin".to_string());
        }
        parts.join(" · ")
    }
}

/// A peer's card, with the chat's flags where there is a chat.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerCard {
    pub id: PeerId,
    pub kind: PeerKind,
    pub name: String,
    pub username: Option<String>,
    pub about: Option<String>,
    pub phone: Option<String>,
    pub status: Option<String>,
    pub members: Option<i64>,
    pub online: Option<i64>,
    /// I administer it — and so may post in it, when it is a channel.
    pub admin: bool,
    pub is_contact: bool,
    pub is_self: bool,
    /// On Telegram's main block list (the stories-only list is separate).
    pub blocked: bool,
    /// The chat's, where I have one.
    pub muted: bool,
    pub pinned: i64,
    pub archived: bool,
    pub unread: i64,
    pub unread_mentions: i64,
    pub last_read: Option<MsgId>,
    pub draft: Option<String>,
    pub typing: Option<String>,
    /// The chat has a place in the main list: it is one of mine, not one the
    /// engine merely learned of through a forward or a mention. A peer with
    /// no chat at all reads `false`, which is the same thing — nothing here
    /// to leave, and something here to join.
    pub in_main: bool,
    pub is_forum: bool,
}

impl PeerCard {
    /// The line under a title: what the client's title bar says under the
    /// name. `typing…` while somebody is.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.blocked {
            return "blocked".to_string();
        }
        if let Some(who) = &self.typing {
            return match self.kind {
                PeerKind::Person => "typing…".to_string(),
                _ => format!("{} is typing…", first_name(who)),
            };
        }
        if self.is_self {
            return "saved messages".to_string();
        }
        match self.kind {
            PeerKind::Person => presence(self.status.as_deref()),
            PeerKind::Group => {
                let n = self.members.unwrap_or(0);
                match self.online {
                    Some(o) if o > 0 => format!("{} members, {o} online", fmt_count(n)),
                    _ => format!("{} members", fmt_count(n)),
                }
            }
            PeerKind::Channel => format!("{} subscribers", fmt_count(self.members.unwrap_or(0))),
        }
    }

    /// The card's one line saying what the peer is.
    #[must_use]
    pub fn kind_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        match self.kind {
            PeerKind::Person => {
                if let Some(u) = &self.username {
                    parts.push(format!("@{u}"));
                }
                parts.push(self.status_line());
            }
            PeerKind::Group | PeerKind::Channel => {
                parts.push(self.kind.as_str().to_string());
                parts.push(self.status_line());
                if let Some(u) = &self.username {
                    parts.push(format!("@{u}"));
                }
            }
        }
        parts.join(" · ")
    }

    /// Whether the composer stands: unblock a person before writing to them.
    #[must_use]
    pub fn can_post(&self) -> bool {
        !self.blocked && (self.kind != PeerKind::Channel || self.admin)
    }

    /// The composer's empty text, with the key that reaches it — the way
    /// the filter's says `( / )`.
    #[must_use]
    pub fn placeholder(&self) -> &'static str {
        if self.kind == PeerKind::Channel {
            "broadcast…  ( enter )"
        } else {
            "write a message…  ( enter )"
        }
    }
}

// -- spellings ------------------------------------------------------------------------

/// Whether a chat's people are known at all: the presence line for a
/// person's status word.
#[must_use]
pub fn presence(status: Option<&str>) -> String {
    match status {
        Some("online") => "online",
        Some("recently") => "last seen recently",
        Some("week") => "last seen within a week",
        Some("month") => "last seen within a month",
        Some("long") => "last seen a long time ago",
        _ => "last seen hidden",
    }
    .to_string()
}

/// A count as a list spells it: `999`, `1.2k`, `12.4k`.
#[must_use]
pub fn fmt_count(n: i64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n as f64 / 1000.0;
    let s = format!("{k:.1}k");
    s.replace(".0k", "k")
}

const WEEKDAYS: [&str; 7] = ["thu", "fri", "sat", "sun", "mon", "tue", "wed"];

/// The day a timestamp falls on, in days since the epoch.
fn day_of(ts: f64) -> i64 {
    (ts as i64).div_euclid(86_400)
}

/// The hour of a timestamp, `HH:MM`.
#[must_use]
pub fn fmt_hour(ts: f64) -> String {
    let rem = (ts as i64).rem_euclid(86_400);
    format!("{:02}:{:02}", rem / 3_600, (rem % 3_600) / 60)
}

/// The chat list's time: the hour today, the weekday inside a week, the
/// day past that.
#[must_use]
pub fn when(ts: f64, now: f64) -> String {
    if ts == 0.0 {
        return String::new();
    }
    let (d, today) = (day_of(ts), day_of(now));
    if d == today {
        return fmt_hour(ts);
    }
    if today - d < 7 && d < today {
        return WEEKDAYS[d.rem_euclid(7) as usize].to_string();
    }
    let (_, m, day) = civil_from_days(d);
    format!("{day:02}.{m:02}")
}

/// The transcript's day caption: `TODAY`, `YESTERDAY`, else `30 AUG`.
#[must_use]
pub fn day_caption(ts: f64, now: f64) -> String {
    let (d, today) = (day_of(ts), day_of(now));
    if d == today {
        return "TODAY".to_string();
    }
    if d == today - 1 {
        return "YESTERDAY".to_string();
    }
    let (_, m, day) = civil_from_days(d);
    format!("{day} {}", MONTHS[(m - 1) as usize].to_uppercase())
}

/// Whether two timestamps fall on one day.
#[must_use]
pub fn same_day(a: f64, b: f64) -> bool {
    day_of(a) == day_of(b)
}

/// How long two messages by one writer may be apart and still share a
/// header.
pub const RUN_GAP: f64 = 5.0 * 60.0;

// -- the chat list --------------------------------------------------------------------

const DATE_OPS: &[Op] = &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte];

/// The chat list's tags.
static CHATS_TAGS: &[TagDef] = &[
    TagDef {
        name: "replies",
        kind: TagType::Bool,
        ops: &[],
        describe: "unread replies and mentions in groups",
        values: Values::None,
    },
    TagDef {
        name: "unread",
        kind: TagType::Bool,
        ops: &[],
        describe: "something in it not read yet",
        values: Values::None,
    },
    TagDef {
        name: "muted",
        kind: TagType::Bool,
        ops: &[],
        describe: "notifications off",
        values: Values::None,
    },
    TagDef {
        name: "pinned",
        kind: TagType::Bool,
        ops: &[],
        describe: "held at the top",
        values: Values::None,
    },
    TagDef {
        name: "kind",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "person, group or channel",
        values: Values::Static(&[
            ("person", "person"),
            ("group", "group"),
            ("channel", "channel"),
        ]),
    },
    TagDef {
        name: "folder",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "one of the saved folders",
        values: Values::Dynamic,
    },
];

/// The chat list, as one query: every chat with its peer and its last line.
/// Pinned chats first in their order, then by the last line's time.
macro_rules! chats_spec {
    ($id:literal, $base:literal) => {
        SqlSpec {
            id: $id,
            describe: "the chats under the panel's filter, pinned first, then latest first",
            select: "c.peer AS peer, p.kind, c.title, c.pinned, c.muted, c.unread, c.mention,
                     c.draft, c.typing,
                     COALESCE(m.date, 0) AS last, COALESCE(m.text, ''),
                     COALESCE(m.out, 0), m.state, COALESCE(s.name, ''), COALESCE(m.service, 0),
                     m.media, m.media_label, m.media_ref, m.media_rid, m.media_w, m.media_h,
                     m.media_secs, m.media_lat, m.media_lon, m.media_until,
                     m.media_clip, m.media_clip_rid,
                     (c.pinned = 0) AS unpinned, c.topic, c.is_forum",
            // The last line is named by the row it is, not by its message id:
            // that number belongs to the chat it is in, and another chat's
            // line may wear it (V8).
            from: "tg_dialog c JOIN tg_peer p ON p.id = c.peer
                   LEFT JOIN tg_message m ON m.seq = (SELECT t.seq FROM tg_message t
                                                      WHERE t.chat = c.peer AND t.topic = c.topic
                                                      ORDER BY t.date DESC, t.id DESC LIMIT 1)
                   LEFT JOIN tg_peer s ON s.id = m.sender",
            base: $base,
            text: &["c.title", "m.text"],
            index: None,
            tags: &[
                ("unread", TagSql::Where("c.unread > 0 OR c.mention > 0")),
                ("replies", TagSql::Where("p.kind = 'group' AND c.mention > 0")),
                ("muted", TagSql::Where("c.muted = 1")),
                ("pinned", TagSql::Where("c.pinned > 0")),
                ("kind", TagSql::Col("p.kind")),
                (
                    "folder",
                    TagSql::Col(
                        "(SELECT GROUP_CONCAT(f.name, ' ') FROM tg_folder_chat fc
                          JOIN tg_folder f ON f.id = fc.folder WHERE fc.chat = c.peer)",
                    ),
                ),
            ],
            // Real columns, not the aliases above: a flat spec's rank and
            // key queries name them in a `WHERE`, where an alias is not in
            // scope.
            order: &[
                ("(c.pinned = 0)", Dir::Asc),
                ("c.pinned", Dir::Asc),
                ("COALESCE(m.date, 0)", Dir::Desc),
                ("c.peer", Dir::Desc),
                ("c.topic", Dir::Desc),
            ],
            group: None,
            key: "c.row_key",
            deps: &["tg_chat", "tg_peer", "tg_topic", "tg_message"],
        }
    };
}

// The list is the chats with a place in it: `in_main` is what the engine's
// positions say, and a chat it merely came to know — a channel a line was
// forwarded from, a group a reply was quoted out of — has none. The archive
// is its own place.
static CHATS_SPEC: SqlSpec = chats_spec!("chats", "c.archived = 0 AND c.in_main = 1 AND c.is_forum = 0");
static ARCHIVE_SPEC: SqlSpec = chats_spec!("archive", "c.archived = 1 AND c.is_forum = 0");
static FORUMS_SPEC: SqlSpec = chats_spec!("telegram forums", "c.is_forum = 1 AND (c.in_main = 1 OR c.archived = 1)");

fn chat_row(r: &rusqlite::Row) -> rusqlite::Result<ChatRow> {
    Ok(ChatRow {
        peer: r.get(0)?,
        topic: r.get(28)?,
        is_forum: r.get::<_, i64>(29)? != 0,
        kind: PeerKind::of(&r.get::<_, String>(1)?),
        title: r.get(2)?,
        pinned: r.get(3)?,
        muted: r.get::<_, i64>(4)? != 0,
        unread: r.get(5)?,
        unread_mentions: r.get(6)?,
        draft: r.get(7)?,
        typing: r.get(8)?,
        last: r.get(9)?,
        last_text: r.get(10)?,
        last_out: r.get::<_, i64>(11)? != 0,
        last_state: r.get(12)?,
        last_from: r.get(13)?,
        last_service: r.get::<_, i64>(14)? != 0,
        last_media: media_from_row(r, 15)?,
    })
}

/// `@folder:` completes against the folders there are.
fn suggest_chats(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    match tag {
        "folder" => folders(store)
            .iter()
            .filter(|f| f.to_lowercase().contains(typed))
            .map(|f| Suggestion::value(f.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

macro_rules! chats_source {
    ($spec:expr) => {
        SqlSource {
            spec: $spec,
            tags: CHATS_TAGS,
            map: chat_row,
            key: ChatRow::key,
            rank: |c| {
                vec![
                    Val::I(i64::from(c.pinned == 0)),
                    Val::I(c.pinned),
                    Val::F(c.last),
                    Val::I(c.peer),
                    Val::I(c.topic),
                ]
            },
            suggest: suggest_chats,
        }
    };
}

static CHATS: SqlSource<ChatRow, String> = chats_source!(&CHATS_SPEC);
static ARCHIVE: SqlSource<ChatRow, String> = chats_source!(&ARCHIVE_SPEC);
pub static FORUMS: SqlSource<ChatRow, String> = chats_source!(&FORUMS_SPEC);

/// The datasource a chat list pages through: the active chats, or the
/// archived ones.
#[must_use]
pub fn chats(archive: bool) -> &'static SqlSource<ChatRow, String> {
    if archive {
        &ARCHIVE
    } else {
        &CHATS
    }
}

/// Rows per page of any of the app's lists.
pub const PAGE: usize = 50;

// -- the messages list ------------------------------------------------------------------

static MESSAGES_TAGS: &[TagDef] = &[
    TagDef {
        name: "from",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "who wrote it",
        values: Values::Dynamic,
    },
    TagDef {
        name: "chat",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "the chat it is in",
        values: Values::Dynamic,
    },
    TagDef {
        name: "date",
        kind: TagType::Date,
        ops: DATE_OPS,
        describe: "when it was written",
        values: Values::None,
    },
    TagDef {
        name: "media",
        kind: TagType::Bool,
        ops: &[],
        describe: "carries a photo, a file, a voice note or a sticker",
        values: Values::None,
    },
];

// The row's own key orders and identifies it, not the message id: a mark, a
// cursor and the fetch a hidden mark makes all name one row, and a message id
// names one only inside its chat (V8).
static MESSAGES_SPEC: SqlSpec = SqlSpec {
    id: "messages",
    describe: "the messages under the panel's filter, latest first, service lines left out",
    select: "m.seq AS seq, m.id, m.chat, p.name, COALESCE(s.name, ''), m.date AS date,
             m.text, m.out,
             m.media, m.media_label, m.media_ref, m.media_rid, m.media_w, m.media_h,
             m.media_secs, m.media_lat, m.media_lon, m.media_until,
             m.media_clip, m.media_clip_rid, m.topic",
    from: "tg_message m JOIN tg_peer p ON p.id = m.chat LEFT JOIN tg_peer s ON s.id = m.sender",
    base: "m.service = 0",
    text: &["m.text"],
    index: None,
    tags: &[
        ("from", TagSql::Col("COALESCE(s.name, '')")),
        ("chat", TagSql::Col("p.name")),
        ("date", TagSql::Col("m.date")),
        ("media", TagSql::Where("m.media IS NOT NULL")),
    ],
    order: &[("m.date", Dir::Desc), ("m.seq", Dir::Desc)],
    group: None,
    key: "m.seq",
    deps: &[],
};

pub(crate) fn msg_hit_row(r: &rusqlite::Row) -> rusqlite::Result<MsgHit> {
    Ok(MsgHit {
        seq: r.get(0)?,
        id: r.get(1)?,
        chat: r.get(2)?,
        chat_title: r.get(3)?,
        sender_name: r.get(4)?,
        date: r.get(5)?,
        text: r.get(6)?,
        out: r.get::<_, i64>(7)? != 0,
        media: media_from_row(r, 8)?,
        topic: r.get(20)?,
    })
}

/// `@from:` completes against the people, `@chat:` against the chats.
fn suggest_messages(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    match tag {
        "from" => people_names(store)
            .iter()
            .filter(|n| n.to_lowercase().contains(typed))
            .map(|n| Suggestion::value(n.clone()))
            .collect(),
        "chat" => chat_titles(store)
            .iter()
            .filter(|n| n.to_lowercase().contains(typed))
            .map(|n| Suggestion::value(n.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The datasource a messages panel pages through, everywhere or in one
/// chat — the same source, narrowed by the panel's seeded `@chat:`.
pub static MESSAGES: SqlSource<MsgHit, i64> = SqlSource {
    spec: &MESSAGES_SPEC,
    tags: MESSAGES_TAGS,
    map: msg_hit_row,
    key: |m| m.seq,
    rank: |m| vec![Val::F(m.date), Val::I(m.seq)],
    suggest: suggest_messages,
};

static REPLIES_SPEC: SqlSpec = SqlSpec {
    id: "telegram replies",
    describe: "unread replies and mentions in joined groups, including the archive",
    from: "tg_message m JOIN tg_peer p ON p.id = m.chat
           JOIN tg_chat c ON c.peer = m.chat LEFT JOIN tg_peer s ON s.id = m.sender",
    base: "m.service = 0 AND m.out = 0 AND m.unread_mention = 1
           AND p.kind = 'group' AND (c.in_main = 1 OR c.archived = 1)",
    ..MESSAGES_SPEC
};

pub static REPLIES: SqlSource<MsgHit, i64> = SqlSource {
    spec: &REPLIES_SPEC,
    ..MESSAGES
};

static Q_REPLY_CHATS: Q = Q {
    id: "tg reply chats",
    sql: "SELECT c.peer, c.mention FROM tg_chat c JOIN tg_peer p ON p.id = c.peer
          WHERE p.kind = 'group' AND (c.in_main = 1 OR c.archived = 1) AND c.mention > 0",
    describe: "groups with unread replies or mentions, whether muted or archived",
};

pub fn reply_chats(store: &Store) -> std::rc::Rc<Vec<(PeerId, i64)>> {
    store.rows(&Q_REPLY_CHATS, &[], |r| Ok((r.get(0)?, r.get(1)?)))
}

pub fn reply_count(store: &Store) -> i64 {
    reply_chats(store).iter().map(|(_, n)| n).sum()
}

// -- the people -------------------------------------------------------------------------

static CONTACTS_TAGS: &[TagDef] = &[TagDef {
    name: "online",
    kind: TagType::Bool,
    ops: &[],
    describe: "online right now",
    values: Values::None,
}];

static MEMBERS_TAGS: &[TagDef] = &[
    TagDef {
        name: "online",
        kind: TagType::Bool,
        ops: &[],
        describe: "online right now",
        values: Values::None,
    },
    TagDef {
        name: "admin",
        kind: TagType::Bool,
        ops: &[],
        describe: "administers the group",
        values: Values::None,
    },
    TagDef {
        name: "group",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "the group the list is of",
        values: Values::None,
    },
];

static CONTACTS_SPEC: SqlSpec = SqlSpec {
    id: "contacts",
    describe: "the people in the address book, by name",
    select: "p.id AS id, p.name AS name, p.username, p.status, 0, NULL",
    from: "tg_peer p",
    base: "p.kind = 'person' AND p.is_contact = 1 AND p.is_self = 0",
    text: &["p.name", "p.username"],
    index: None,
    tags: &[("online", TagSql::Where("p.status = 'online'"))],
    order: &[("p.name", Dir::Asc), ("p.id", Dir::Asc)],
    group: None,
    key: "p.id",
    deps: &[],
};

static MEMBERS_SPEC: SqlSpec = SqlSpec {
    id: "members",
    describe: "a group's members, by name",
    select: "p.id AS id, p.name AS name, p.username, p.status, mb.admin, mb.chat",
    from: "tg_member mb JOIN tg_peer p ON p.id = mb.peer JOIN tg_peer g ON g.id = mb.chat",
    base: "",
    text: &["p.name", "p.username"],
    index: None,
    tags: &[
        ("online", TagSql::Where("p.status = 'online'")),
        ("admin", TagSql::Where("mb.admin = 1")),
        ("group", TagSql::Col("g.name")),
    ],
    order: &[("p.name", Dir::Asc), ("p.id", Dir::Asc)],
    group: None,
    key: "p.id",
    deps: &[],
};

fn person_row(r: &rusqlite::Row) -> rusqlite::Result<Person> {
    Ok(Person {
        id: r.get(0)?,
        name: r.get(1)?,
        username: r.get(2)?,
        status: r.get(3)?,
        admin: r.get::<_, i64>(4)? != 0,
        group: r.get(5)?,
    })
}

fn suggest_nothing(_store: &Store, _tag: &str, _typed: &str) -> Vec<Suggestion> {
    Vec::new()
}

macro_rules! people_source {
    ($spec:expr, $tags:expr) => {
        SqlSource {
            spec: $spec,
            tags: $tags,
            map: person_row,
            key: |p| p.id,
            rank: |p| vec![Val::S(p.name.clone()), Val::I(p.id)],
            suggest: suggest_nothing,
        }
    };
}

/// The address book.
pub static CONTACTS: SqlSource<Person, i64> = people_source!(&CONTACTS_SPEC, CONTACTS_TAGS);
/// A group's members, narrowed by the panel's seeded `@group:`.
pub static MEMBERS: SqlSource<Person, i64> = people_source!(&MEMBERS_SPEC, MEMBERS_TAGS);

// -- queries ------------------------------------------------------------------------------

static Q_PEER: Q = Q {
    id: "tg peer",
    sql: "SELECT p.id, p.kind, p.name, p.username, p.about, p.phone, p.status, p.members,
                 p.online, p.admin, p.is_contact, p.is_self,
                 COALESCE(c.muted, 0), COALESCE(c.pinned, 0), COALESCE(c.archived, 0),
                 COALESCE(c.unread, 0), c.last_read, c.draft, c.typing, COALESCE(c.mention, 0),
                 COALESCE(c.in_main, 0), p.blocked, p.is_forum
          FROM tg_peer p LEFT JOIN tg_chat c ON c.peer = p.id
          WHERE p.id = ?1",
    describe: "one peer, with the flags of the chat I have with it",
};

fn peer_card_row(r: &rusqlite::Row) -> rusqlite::Result<PeerCard> {
    Ok(PeerCard {
        id: r.get(0)?,
        kind: PeerKind::of(&r.get::<_, String>(1)?),
        name: r.get(2)?,
        username: r.get(3)?,
        about: r.get(4)?,
        phone: r.get(5)?,
        status: r.get(6)?,
        members: r.get(7)?,
        online: r.get(8)?,
        admin: r.get::<_, i64>(9)? != 0,
        is_contact: r.get::<_, i64>(10)? != 0,
        is_self: r.get::<_, i64>(11)? != 0,
        muted: r.get::<_, i64>(12)? != 0,
        pinned: r.get(13)?,
        archived: r.get::<_, i64>(14)? != 0,
        unread: r.get(15)?,
        last_read: r.get(16)?,
        draft: r.get(17)?,
        typing: r.get(18)?,
        unread_mentions: r.get(19)?,
        in_main: r.get::<_, i64>(20)? != 0,
        blocked: r.get::<_, i64>(21)? != 0,
        is_forum: r.get::<_, i64>(22)? != 0,
    })
}

/// One peer's card, with its chat's flags. `None` for an id the store does
/// not have.
#[must_use]
pub fn peer(store: &Store, id: PeerId) -> Option<PeerCard> {
    store
        .rows(&Q_PEER, &[Val::I(id)], peer_card_row)
        .first()
        .cloned()
}

static Q_HISTORY: Q = Q {
    id: "tg history",
    // A reply answers a line of the *same chat*: the id it names means
    // nothing outside it, and joining on the id alone quoted whichever chat's
    // line happened to wear that number (V8).
    sql: "SELECT m.id, m.chat, m.sender, COALESCE(s.name, ''), m.date, m.text, m.out, m.state,
                 m.edited, m.reply_to, COALESCE(rs.name, ''), COALESCE(r.text, '') AS reply_text,
                 m.fwd_from, m.views, m.comments,
                 CASE WHEN rx.known THEN rx.counts ELSE m.reactions END, m.service,
                 COALESCE(r.out, 0), r.media,
                 m.media, m.media_label, m.media_ref, m.media_rid, m.media_w, m.media_h,
                 m.media_secs, m.media_lat, m.media_lon, m.media_until,
                 m.media_clip, m.media_clip_rid, m.entities, m.entities_known, m.unread_mention, m.content_type, m.topic
          FROM tg_message m
          LEFT JOIN tg_message_reaction rx ON rx.chat = m.chat AND rx.message = m.id
          LEFT JOIN tg_peer s ON s.id = m.sender
          LEFT JOIN tg_message r ON r.chat = m.chat AND r.id = m.reply_to
          LEFT JOIN tg_peer rs ON rs.id = r.sender
          WHERE m.chat = ?1 AND (?2 = 0 OR m.topic = ?2)
          ORDER BY m.date, m.id",
    describe: "one chat's lines, oldest first, each with what it answers",
};

fn msg_row(r: &rusqlite::Row) -> rusqlite::Result<Msg> {
    // The quoted line's writer is `me` where it was mine, and the quote is
    // its media where it had no text — the same spellings the line itself
    // gets.
    let reply_name = if r.get::<_, i64>(17)? != 0 {
        "me".to_string()
    } else {
        r.get(10)?
    };
    let reply_media = r.get::<_, Option<String>>(18)?.map(|k| Media::of(&k));
    let reply_text = media_or_text(reply_media.as_ref(), &r.get::<_, String>(11)?, 0.0);
    Ok(Msg {
        content_type: r.get(34)?,
        topic: r.get(35)?,
        id: r.get(0)?,
        chat: r.get(1)?,
        sender: r.get(2)?,
        sender_name: r.get(3)?,
        date: r.get(4)?,
        text: r.get(5)?,
        entities: if r.get::<_, bool>(32)? {
            Some(serde_json::from_str(&r.get::<_, String>(31)?).unwrap_or_default())
        } else {
            None
        },
        out: r.get::<_, i64>(6)? != 0,
        state: r.get(7)?,
        edited: r.get::<_, i64>(8)? != 0,
        reply_to: r.get(9)?,
        unread_mention: r.get(33)?,
        reply_name,
        reply_text,
        fwd_from: r.get(12)?,
        views: r.get(13)?,
        comments: r.get(14)?,
        reactions: r.get(15)?,
        service: r.get::<_, i64>(16)? != 0,
        media: media_from_row(r, 19)?,
    })
}

/// One chat's lines, oldest first.
#[must_use]
pub fn history(store: &Store, chat: PeerId) -> std::rc::Rc<Vec<Msg>> {
    history_in(store, chat, 0)
}

pub fn history_in(store: &Store, chat: PeerId, topic: i64) -> std::rc::Rc<Vec<Msg>> {
    store.rows(&Q_HISTORY, &[Val::I(chat), Val::I(topic)], msg_row)
}

pub(super) fn read_history(conn: &rusqlite::Connection, chat: PeerId, topic: i64) -> rusqlite::Result<Vec<Msg>> {
    let mut statement = conn.prepare_cached(Q_HISTORY.sql)?;
    let rows = statement.query_map([chat, topic], msg_row)?;
    rows.collect()
}

pub(super) fn trace_history(store: &Store, chat: PeerId, topic: i64, rows: usize) {
    store.trace_rows(&Q_HISTORY, &[Val::I(chat), Val::I(topic)], rows);
}

static Q_FIRST_UNREAD: Q = Q {
    id: "tg first unread",
    sql: "SELECT id FROM tg_message WHERE chat = ?1 AND (?2 = 0 OR topic = ?2)
          AND id > ?3 AND out = 0 AND service = 0 ORDER BY date, id LIMIT 1",
    describe: "the unread divider, without loading the transcript",
};

pub fn first_unread_in(store: &Store, chat: PeerId, topic: i64, last_read: MsgId) -> Option<MsgId> {
    store.rows(&Q_FIRST_UNREAD, &[Val::I(chat), Val::I(topic), Val::I(last_read)], |r| r.get(0))
        .first().copied()
}

static Q_FOLDERS: Q = Q {
    id: "tg folders",
    sql: "SELECT name FROM tg_folder ORDER BY id",
    describe: "the saved folders, in their order",
};

/// The folders' names.
#[must_use]
pub fn folders(store: &Store) -> std::rc::Rc<Vec<String>> {
    store.rows(&Q_FOLDERS, &[], |r| r.get(0))
}

static Q_PEOPLE_NAMES: Q = Q {
    id: "tg people names",
    sql: "SELECT name FROM tg_peer WHERE kind = 'person' AND is_self = 0 ORDER BY name",
    describe: "every person's name, for a completion",
};

fn people_names(store: &Store) -> std::rc::Rc<Vec<String>> {
    store.rows(&Q_PEOPLE_NAMES, &[], |r| r.get(0))
}

static Q_CHAT_TITLES: Q = Q {
    id: "tg chat titles",
    sql: "SELECT p.name FROM tg_chat c JOIN tg_peer p ON p.id = c.peer ORDER BY p.name",
    describe: "every chat's title, for a completion",
};

fn chat_titles(store: &Store) -> std::rc::Rc<Vec<String>> {
    store.rows(&Q_CHAT_TITLES, &[], |r| r.get(0))
}

/// What a peer's name is in a list's filter grammar: quoted where it has a
/// space in it.
#[must_use]
pub fn filter_value(name: &str) -> String {
    if name.contains(' ') {
        format!("\"{name}\"")
    } else {
        name.to_string()
    }
}

// -- writes ---------------------------------------------------------------------------------

/// Makes sure a chat row stands for a peer — a person out of the address
/// book has none until something is written to them.
///
/// # Errors
///
/// If the store refuses the write.
pub fn ensure_chat_tx(c: &rusqlite::Connection, peer: PeerId) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO tg_chat(peer) VALUES(?1)",
        rusqlite::params![peer],
    )?;
    Ok(())
}

/// Writes a chat's draft: what the composer holds, which the list shows.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_draft_tx(c: &rusqlite::Connection, peer: PeerId, draft: &str) -> rusqlite::Result<()> {
    ensure_chat_tx(c, peer)?;
    let d = (!draft.trim().is_empty()).then_some(draft);
    c.execute(
        "UPDATE tg_chat SET draft = ?2 WHERE peer = ?1",
        rusqlite::params![peer, d],
    )?;
    Ok(())
}

/// Advances the ordinary inbox to the exact line named in its read receipt.
/// Only cached incoming messages crossed by that boundary are subtracted
/// from the count; uncached unread messages wait for the server's count.
/// The remaining cached messages are a lower bound, including new arrivals.
/// Replies and mentions keep their separate acknowledgment when viewed.
///
/// # Errors
///
/// If the store refuses the write.
pub fn mark_read_tx(c: &rusqlite::Connection, peer: PeerId, through: MsgId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_chat SET unread = MAX(
            (SELECT COUNT(*) FROM tg_message
             WHERE chat = ?1 AND id > ?2 AND out = 0 AND service = 0),
            unread - (SELECT COUNT(*) FROM tg_message
                      WHERE chat = ?1 AND id > COALESCE(tg_chat.last_read, 0)
                        AND id <= ?2 AND out = 0 AND service = 0)),
            last_read = ?2
         WHERE peer = ?1 AND ?2 > COALESCE(last_read, 0)",
        rusqlite::params![peer, through],
    )?;
    Ok(())
}

// -- playing, recording, carrying ----------------------------------------------------

/// A player over one line's recording, ticked against the clock: where it
/// stands is what it was at when it last started plus the time since. The
/// panel instance owns one; the shell's kit draws its [`PlayerState`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Player {
    pub msg: MsgId,
    pub length: f64,
    /// Where it stood when it last started, or was paused.
    pub offset: f64,
    /// When it last started, while it runs.
    pub started: Option<f64>,
}

impl Player {
    /// A player at the start of a line's recording, not running.
    #[must_use]
    pub fn over(msg: MsgId, length: f64) -> Player {
        Player {
            msg,
            length,
            offset: 0.0,
            started: None,
        }
    }

    /// Where it stands now.
    #[must_use]
    pub fn state(&self, now: f64) -> PlayerState {
        let position = match self.started {
            Some(t) => (self.offset + (now - t)).min(self.length),
            None => self.offset,
        };
        PlayerState {
            playing: self.started.is_some() && position < self.length,
            position,
            length: self.length,
        }
    }

    /// Play or pause. Played again at its end, it starts over.
    pub fn toggle(&mut self, now: f64) {
        let st = self.state(now);
        if st.playing {
            self.offset = st.position;
            self.started = None;
        } else {
            self.offset = if st.position >= self.length { 0.0 } else { st.position };
            self.started = Some(now);
        }
    }

    /// Move along the timeline, preserving whether it is currently running.
    pub fn seek(&mut self, position: f64, now: f64) {
        if !position.is_finite() {
            return;
        }
        let playing = self.state(now).playing;
        self.offset = position.clamp(0.0, self.length.max(0.0));
        self.started = playing.then_some(now);
    }
}

/// What the attach panel is recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecKind {
    Voice,
    Video,
}

impl RecKind {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            RecKind::Voice => "voice",
            RecKind::Video => "video message",
        }
    }
}

/// A recording under way in the attach panel: what, and since when.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recording {
    pub kind: RecKind,
    pub since: f64,
}

impl Recording {
    #[must_use]
    pub fn elapsed(&self, now: f64) -> f64 {
        (now - self.since).max(0.0)
    }

    /// The line the strip says: *recording voice 0:03*.
    #[must_use]
    pub fn line(&self, now: f64) -> String {
        format!(
            "recording {} {}",
            self.kind.word(),
            fmt_secs(self.elapsed(now).floor() as i64)
        )
    }

    /// The line under it: the keys, on the control as the placeholder's
    /// are.
    #[must_use]
    pub const fn keys() -> &'static str {
        "enter sends, esc discards"
    }
}

/// A file the composer carries, by its path on this machine: what a send
/// reads as the message leaves, as a letter does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    pub path: String,
}

impl Carried {
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// What it will be sent as, by its name: a picture as a photo, a
    /// moving one as a video, a sound as audio, anything else as a file.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        let ext = self
            .name()
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" => "photo",
            "gif" => "animation",
            "mp4" | "mov" | "m4v" | "webm" => "video",
            "mp3" | "m4a" | "ogg" | "opus" | "wav" | "flac" => "audio",
            _ => "file",
        }
    }

    /// *report-q3.pdf · file*.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} · {}", self.name(), self.kind())
    }

    /// Where it is: the path without the name, `~/Downloads`.
    #[must_use]
    pub fn dir(&self) -> &str {
        match self.path.rsplit_once('/') {
            Some(("", _)) => "/",
            Some((d, _)) => d,
            None => "",
        }
    }

    /// The row's second line: *file · ~/Downloads*.
    #[must_use]
    pub fn detail(&self) -> String {
        format!("{} · {}", self.kind(), self.dir())
    }
}

/// Where the device says I am, this round: the trailhead. The fourth phase
/// asks a `Location` capability, whose fake answers the same.
pub const HERE: (f64, f64) = (47.0472, 8.3164);

/// Writes a line's text and whether it counts as edited: the edit verb, and
/// its undo, which puts the old text and the old flag back.
///
/// # Errors
///
/// If the store refuses the write.
pub fn edit_tx(
    c: &rusqlite::Connection,
    chat: PeerId,
    msg: MsgId,
    text: &str,
    edited: bool,
    entities: Option<&[super::text::Entity]>,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_message SET text = ?3, edited = ?4, entities = ?5, entities_known = ?6
         WHERE chat = ?1 AND id = ?2",
        rusqlite::params![chat, msg, text, edited,
            serde_json::to_string(entities.unwrap_or_default()).expect("text entities serialize"),
            entities.is_some()],
    )?;
    Ok(())
}

/// One line copied whole, column by column, before it goes: what undo puts
/// back exactly, whatever the table's shape — all but the row key, which is
/// the table's to hand out.
#[derive(Debug, Clone)]
pub struct LineCopy {
    cols: Vec<String>,
    vals: Vec<rusqlite::types::Value>,
}

/// Copies lines out, whole but for `seq`.
///
/// The row key is left behind because it is a number the table mints, and one
/// freed by a delete may be minted again for the next line that lands; a
/// restore carrying it would then land on that line rather than beside it.
/// What identifies the line is its chat and its id, and those are copied.
///
/// # Errors
///
/// If the store refuses the read.
pub fn copy_lines_tx(
    c: &rusqlite::Connection,
    chat: PeerId,
    ids: &[MsgId],
) -> rusqlite::Result<Vec<LineCopy>> {
    let mut st = c.prepare("SELECT * FROM tg_message WHERE chat = ?1 AND id = ?2")?;
    let named: Vec<String> = st.column_names().iter().map(ToString::to_string).collect();
    let kept: Vec<usize> = (0..named.len()).filter(|&i| named[i] != "seq").collect();
    let cols: Vec<String> = kept.iter().map(|&i| named[i].clone()).collect();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let mut rows = st.query(rusqlite::params![chat, id])?;
        if let Some(r) = rows.next()? {
            let vals = kept
                .iter()
                .map(|&i| r.get::<_, rusqlite::types::Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            out.push(LineCopy {
                cols: cols.clone(),
                vals,
            });
        }
    }
    Ok(out)
}

/// Deletes lines; answers how many went.
///
/// # Errors
///
/// If the store refuses the write.
pub fn delete_lines_tx(c: &rusqlite::Connection, chat: PeerId, ids: &[MsgId]) -> rusqlite::Result<usize> {
    let mut n = 0;
    for id in ids {
        n += c.execute(
            "DELETE FROM tg_message WHERE chat = ?1 AND id = ?2",
            rusqlite::params![chat, id],
        )?;
    }
    Ok(n)
}

/// Puts copied lines back as they were, each on a fresh row key. `OR
/// REPLACE` on the chat and the id, so a line that came back over the wire
/// while the undo waited is written once, not twice.
///
/// # Errors
///
/// If the store refuses the write.
pub fn restore_lines_tx(c: &rusqlite::Connection, rows: &[LineCopy]) -> rusqlite::Result<()> {
    for r in rows {
        let sql = format!(
            "INSERT OR REPLACE INTO tg_message ({}) VALUES ({})",
            r.cols.join(", "),
            vec!["?"; r.cols.len()].join(", ")
        );
        c.execute(&sql, rusqlite::params_from_iter(r.vals.iter()))?;
    }
    Ok(())
}

static Q_MEDIA_IDS: Q = Q {
    id: "tg media ids",
    sql: "SELECT id FROM tg_message
          WHERE chat = ?1 AND media IS NOT NULL AND service = 0
            AND topic = COALESCE((SELECT topic FROM tg_message WHERE chat = ?1 AND id = ?2), 0)
          ORDER BY date, id",
    describe: "the lines of a chat that carry media, oldest first, for the viewer's walk",
};

/// The lines of a chat that carry media, oldest first.
#[must_use]
pub fn media_ids(store: &Store, chat: PeerId, around: MsgId) -> std::rc::Rc<Vec<MsgId>> {
    store.rows(&Q_MEDIA_IDS, &[Val::I(chat), Val::I(around)], |r| r.get(0))
}

static Q_MESSAGE_TOPIC: Q = Q {
    id: "telegram message topic",
    sql: "SELECT topic FROM tg_message WHERE chat = ?1 AND id = ?2",
    describe: "the conversation a message belongs to",
};

pub fn message_topic(store: &Store, chat: PeerId, id: MsgId) -> i64 {
    store.rows(&Q_MESSAGE_TOPIC, &[Val::I(chat), Val::I(id)], |r| r.get(0)).first().copied().unwrap_or(0)
}

/// One line, by id.
#[must_use]
pub fn line(store: &Store, chat: PeerId, id: MsgId) -> Option<Msg> {
    let sql = Q_HISTORY.sql.replace(
        "WHERE m.chat = ?1 AND (?2 = 0 OR m.topic = ?2)",
        "WHERE m.chat = ?1 AND m.id = ?2",
    );
    store.rows_sql("tg line", "one message and its reply", &sql, &[Val::I(chat), Val::I(id)], msg_row)
        .first().cloned()
}

/// The Telegram conversation tag. The agent app owns the bare `chat` tag;
/// every Telegram list and search result opens through this identity.
pub const CHAT_TAG: Tag = Tag("telegram-chat");

/// The identity of one chat, optionally opened at a message.
#[must_use]
pub fn chat_id(peer: PeerId, at: Option<MsgId>) -> PanelId {
    match at {
        Some(m) => PanelId::new(CHAT_TAG, [peer.to_string(), m.to_string()]),
        None => PanelId::new(CHAT_TAG, [peer.to_string()]),
    }
}

// -- the verbs about a chat ------------------------------------------------------------
//
// The local half of mute, pin, archive and leave: the flip a card or a list
// writes the moment it is pressed, so the bar reads the new word on this draw
// rather than on the engine's answer, which confirms it a moment later. None
// of them is undoable — a flip that the wire will restate is not a thing to
// give back — and a chat with no row of its own gets one first, since a peer
// out of the address book has none until something is written to it.

static Q_NEWEST_ORDINARY_LINE: Q = Q {
    id: "tg newest ordinary line",
    sql: "SELECT MAX(id) FROM tg_message WHERE chat = ?1 AND topic = ?2 AND unread_mention = 0",
    describe: "the newest line a chat can read without acknowledging an unread mention",
};

/// The newest line that can advance a chat's read position without
/// acknowledging an unread reply or mention, or `None` when none is cached.
#[must_use]
#[cfg(test)]
pub fn newest_ordinary_line(store: &Store, chat: PeerId) -> Option<MsgId> {
    newest_ordinary_line_in(store, chat, 0)
}

pub fn newest_ordinary_line_in(store: &Store, chat: PeerId, topic: i64) -> Option<MsgId> {
    store
        .rows(&Q_NEWEST_ORDINARY_LINE, &[Val::I(chat), Val::I(topic)], |r| {
            r.get::<_, Option<MsgId>>(0)
        })
        .first()
        .copied()
        .flatten()
}

/// Mutes a chat, or lets it speak again.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_muted_tx(c: &rusqlite::Connection, peer: PeerId, muted: bool) -> rusqlite::Result<()> {
    ensure_chat_tx(c, peer)?;
    c.execute(
        "UPDATE tg_chat SET muted = ?2 WHERE peer = ?1",
        rusqlite::params![peer, muted],
    )?;
    Ok(())
}

/// Pins a chat to the top of its list, or lets it down. The column is a rank
/// among the pinned, which only the whole list can settle; one chat's own
/// verb can say no more than *pinned*, and the list orders itself when the
/// engine sends the positions.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_pinned_tx(c: &rusqlite::Connection, peer: PeerId, pinned: bool) -> rusqlite::Result<()> {
    ensure_chat_tx(c, peer)?;
    c.execute(
        "UPDATE tg_chat SET pinned = ?2 WHERE peer = ?1",
        rusqlite::params![peer, i64::from(pinned)],
    )?;
    Ok(())
}

/// Puts a chat in the archive, or brings it back.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_archived_tx(
    c: &rusqlite::Connection,
    peer: PeerId,
    archived: bool,
) -> rusqlite::Result<()> {
    ensure_chat_tx(c, peer)?;
    c.execute(
        "UPDATE tg_chat SET archived = ?2 WHERE peer = ?1",
        rusqlite::params![peer, archived],
    )?;
    Ok(())
}

/// Leaves a group or a channel, locally: its transcript, its membership, its
/// place in every folder and the chat row itself all go, in one write, and
/// the delete trigger takes the lines out of the index with them.
///
/// The **peer stays**. Leaving is not forgetting: the group is still a name a
/// forwarded line may point at, a member list may hold, and a search may
/// find; what ends is the conversation. It is also what lets the card the
/// verb was pressed on go on drawing — the card reads the peer and left-joins
/// the chat, so a chat that is gone is simply a card with no flags.
///
/// Undo-free on purpose: the wire has been told, and what comes back from it
/// is the chat's absence.
///
/// # Errors
///
/// If the store refuses the write.
pub fn leave_chat_tx(c: &rusqlite::Connection, peer: PeerId) -> rusqlite::Result<()> {
    c.execute("DELETE FROM tg_message WHERE chat = ?1", [peer])?;
    c.execute("DELETE FROM tg_member WHERE chat = ?1", [peer])?;
    c.execute("DELETE FROM tg_folder_chat WHERE chat = ?1", [peer])?;
    c.execute("DELETE FROM tg_chat WHERE peer = ?1", [peer])?;
    Ok(())
}

/// Changes a person's block without touching their contact or conversation.
pub fn set_blocked_tx(c: &rusqlite::Connection, peer: PeerId, blocked: bool) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_peer SET blocked = ?2 WHERE id = ?1 AND id > 0 AND kind = 'person' AND is_self = 0",
        rusqlite::params![peer, blocked],
    )?;
    Ok(())
}

/// Removes a contact while keeping their identity, messages and memberships.
pub fn delete_contact_tx(c: &rusqlite::Connection, peer: PeerId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_peer SET is_contact = 0 WHERE id = ?1 AND kind = 'person' AND is_self = 0",
        [peer],
    )?;
    Ok(())
}

/// Clears a chat's lines and keeps the chat — the list row stays, empty,
/// as the client's *clear history* leaves it.
///
/// # Errors
///
/// If the store refuses the write.
pub fn clear_history_tx(c: &rusqlite::Connection, peer: PeerId) -> rusqlite::Result<()> {
    c.execute("DELETE FROM tg_message WHERE chat = ?1", [peer])?;
    c.execute(
        "UPDATE tg_chat SET unread = 0, mention = 0, last_read = NULL WHERE peer = ?1",
        [peer],
    )?;
    Ok(())
}

static Q_SELF: Q = Q {
    id: "tg self",
    sql: "SELECT id FROM tg_peer WHERE is_self = 1 AND id <> ?1 ORDER BY id LIMIT 1",
    describe: "the signed-in account's own peer",
};

/// The account holder's own peer, where an account has signed in: whom the
/// engine's `my_id` marked ([`sync`](super::sync)). What *saved messages*
/// resolves to.
///
/// The demo world marks its own stand-in self, and that is not an answer —
/// nothing signed in named it — so a store of fixtures says `None` here and
/// the notes-to-self stay the conversation they always drew.
#[must_use]
pub fn self_peer(store: &Store) -> Option<PeerId> {
    store
        .rows(&Q_SELF, &[Val::I(super::seed::SELF)], |r| r.get(0))
        .first()
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::time::{ts, virtual_epoch};

    /// A cached clip is reached through a link that carries its container
    /// as an extension, beside the cache; the link is made once and points
    /// at the blob.
    #[test]
    fn a_clip_is_played_through_a_link_with_its_extension() {
        let dir = std::env::temp_dir().join(format!("superapp-tg-play-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("blobs")).unwrap();
        let key = "tg:clip_link_test";
        let blob = dir.join("blobs").join(kernel::caps::file_name(key));
        assert!(playable_path(Some(&dir), key).is_none(), "no blob, no link");
        std::fs::write(&blob, b"\0\0\0\x1cftypisom\0\0\x02\0isomiso2").unwrap();
        let link = playable_path(Some(&dir), key).expect("a link");
        assert_eq!(link.extension().and_then(|e| e.to_str()), Some("mp4"));
        assert_eq!(std::fs::read(&link).unwrap(), std::fs::read(&blob).unwrap(), "the link reads the blob");
        assert_eq!(playable_path(Some(&dir), key).as_deref(), Some(link.as_path()), "made once");
        let webm = dir.join("blobs").join(kernel::caps::file_name("tg:webm_test"));
        std::fs::write(&webm, [0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(
            playable_path(Some(&dir), "tg:webm_test").unwrap().extension().and_then(|e| e.to_str()),
            Some("webm")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_list_spells_time_by_how_far_it_is() {
        let now = virtual_epoch(); // tuesday, 1 sep 2026, noon
        assert_eq!(when(ts(2026, 9, 1, 11, 52), now), "11:52");
        assert_eq!(when(ts(2026, 8, 31, 19, 20), now), "mon");
        assert_eq!(when(ts(2026, 8, 27, 13, 0), now), "thu");
        assert_eq!(when(ts(2026, 8, 26, 13, 0), now), "wed");
        assert_eq!(when(ts(2026, 8, 25, 10, 0), now), "25.08");
        assert_eq!(when(0.0, now), "");
        assert_eq!(day_caption(ts(2026, 9, 1, 8, 0), now), "TODAY");
        assert_eq!(day_caption(ts(2026, 8, 31, 8, 0), now), "YESTERDAY");
        assert_eq!(day_caption(ts(2026, 8, 30, 8, 0), now), "30 AUG");
    }

    #[test]
    fn counts_and_presence_are_spelled_for_a_human() {
        assert_eq!(fmt_count(38), "38");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1k");
        assert_eq!(fmt_count(1_200), "1.2k");
        assert_eq!(fmt_count(12_400), "12.4k");
        assert_eq!(presence(Some("online")), "online");
        assert_eq!(presence(Some("week")), "last seen within a week");
        assert_eq!(presence(None), "last seen hidden");
        assert_eq!(state_mark(Some("read")), "read");
        assert_eq!(state_mark(Some("sent")), "sent");
        assert_eq!(state_mark(Some("failed")), "failed");
        assert_eq!(fmt_secs(42), "0:42");
        assert_eq!(fmt_secs(221), "3:41");
        assert_eq!(fmt_secs(3725), "1:02:05");
        let noon = virtual_epoch();
        assert_eq!(live_left(noon + 42.0 * 60.0, noon), "42 min left");
        assert_eq!(live_left(noon + 3.0 * 3600.0, noon), "3 h left");
        assert_eq!(live_left(noon - 1.0, noon), "ended");
    }

    fn row() -> ChatRow {
        ChatRow {
            peer: 2,
            topic: 0,
            is_forum: false,
            kind: PeerKind::Person,
            title: "Vera Kovac".into(),
            pinned: 0,
            muted: false,
            unread: 0,
            unread_mentions: 0,
            draft: None,
            typing: None,
            last: ts(2026, 9, 1, 11, 52),
            last_text: "then we start earlier. 7:00?".into(),
            last_media: None,
            last_out: false,
            last_state: None,
            last_from: "Vera Kovac".into(),
            last_service: false,
        }
    }

    #[test]
    fn the_preview_line_says_who_and_what() {
        assert_eq!(row().preview(virtual_epoch()), "then we start earlier. 7:00?");
        let mine = ChatRow {
            last_out: true,
            last_state: Some("read".into()),
            ..row()
        };
        assert_eq!(mine.preview(virtual_epoch()), "me: then we start earlier. 7:00?");
        assert_eq!(mine.state_mark(), "read");
        let group = ChatRow {
            kind: PeerKind::Group,
            last_from: "Max Ivanov".into(),
            last_media: Some(Media {
                secs: Some(42),
                ..Media::of("voice")
            }),
            last_text: String::new(),
            ..row()
        };
        assert_eq!(group.preview(virtual_epoch()), "Max: voice 0:42");
        let captioned = ChatRow {
            last_media: Some(Media {
                w: Some(480),
                h: Some(300),
                ..Media::of("photo")
            }),
            last_text: "new palette, what do you think".into(),
            ..row()
        };
        assert_eq!(captioned.preview(virtual_epoch()), "new palette, what do you think · photo");
        let draft = ChatRow {
            draft: Some("I'll bring the thermos and".into()),
            ..row()
        };
        assert_eq!(draft.preview(virtual_epoch()), "draft: I'll bring the thermos and");
        let typing = ChatRow {
            kind: PeerKind::Group,
            typing: Some("Irina".into()),
            ..row()
        };
        assert_eq!(typing.preview(virtual_epoch()), "Irina is typing…");
        let service = ChatRow {
            last_service: true,
            last_text: "Anna Schmidt left the group".into(),
            ..row()
        };
        assert_eq!(service.preview(virtual_epoch()), "Anna Schmidt left the group");
    }

    #[test]
    fn every_kind_of_media_is_spelled_on_a_line() {
        let noon = virtual_epoch();
        let m = |kind: &str| Media::of(kind);
        assert_eq!(Media { w: Some(480), h: Some(300), ..m("photo") }.line(noon), "photo 480×300");
        assert_eq!(m("photo").line(noon), "photo");
        assert_eq!(Media { secs: Some(14), ..m("video") }.line(noon), "video 0:14");
        assert_eq!(Media { secs: Some(8), ..m("circle") }.line(noon), "video message 0:08");
        assert_eq!(Media { label: Some("🙈".into()), ..m("sticker") }.line(noon), "sticker 🙈");
        assert_eq!(Media { secs: Some(42), ..m("voice") }.line(noon), "voice 0:42");
        assert_eq!(
            Media { label: Some("Scratchcard Lanyard".into()), secs: Some(221), ..m("audio") }.line(noon),
            "audio Scratchcard Lanyard · 3:41"
        );
        assert_eq!(
            Media { label: Some("report-q3.pdf · 2.1 MB".into()), ..m("file") }.line(noon),
            "file report-q3.pdf · 2.1 MB"
        );
        assert_eq!(
            Media { lat: Some(47.0472), lon: Some(8.3164), ..m("location") }.line(noon),
            "location 47.0472, 8.3164"
        );
        assert_eq!(
            Media { lat: Some(55.7512), lon: Some(37.6184), until: Some(noon + 42.0 * 60.0), ..m("live") }.line(noon),
            "live location 55.7512, 37.6184 · 42 min left"
        );
        assert_eq!(Media { reference: Some("demo:palette".into()), ..m("photo") }.picture_bytes(None).map(|b| b.len()), Some(630));
        assert!(Media { reference: Some("demo:palette".into()), ..m("file") }.picture_bytes(None).is_none());
        assert_eq!(media_or_text(Some(&m("circle")), "", noon), "video message");
        assert_eq!(media_or_text(Some(&m("live")), "here", noon), "here · live location");
    }

    #[test]
    fn media_bytes_reads_demo_bundled_and_downloaded_tg_alike() {
        // A demo reference reads a bundled picture, store dir or not — the
        // round-one path, untouched.
        assert_eq!(
            media_bytes(None, "demo:palette").map(|b| b.len()),
            Some(630)
        );
        assert!(media_bytes(None, "demo:nope").is_none());

        // A tg reference resolves to a file the worker's download landed in
        // the blob cache beside the store: plant one where the cache names it
        // and the resolver reads it back.
        let dir = std::env::temp_dir()
            .join(format!("superapp-tg-media-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let blobs = dir.join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        // A miss — not downloaded yet — is None, and the widget keeps its
        // label until it lands.
        assert!(media_bytes(Some(&dir), "tg:garden").is_none());
        std::fs::write(blobs.join(kernel::caps::file_name("tg:garden")), b"jpeg here").unwrap();
        assert_eq!(
            media_bytes(Some(&dir), "tg:garden").as_deref(),
            Some(b"jpeg here".as_slice()),
            "the cached download resolves"
        );
        // A tg reference with the store in memory has nowhere to look.
        assert!(media_bytes(None, "tg:garden").is_none());
        // The kind gate holds: only a photo, video or circle draws a picture.
        let cached = Media { reference: Some("tg:garden".into()), ..Media::of("photo") };
        assert_eq!(cached.picture_bytes(Some(&dir)).as_deref(), Some(b"jpeg here".as_slice()));
        assert!(Media { reference: Some("tg:garden".into()), ..Media::of("voice") }
            .picture_bytes(Some(&dir))
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a player is pointed at: the cached file's path, and nothing
    /// where the clip has not landed — which is what the viewer re-asks
    /// every draw while the download runs.
    #[test]
    fn media_path_names_the_cached_file_and_nothing_before_it_lands() {
        let dir = std::env::temp_dir()
            .join(format!("superapp-tg-clip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let blobs = dir.join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        assert!(media_path(Some(&dir), "tg:reef").is_none(), "not downloaded yet");
        let file = blobs.join(kernel::caps::file_name("tg:reef"));
        std::fs::write(&file, b"mp4 here").unwrap();
        assert_eq!(media_path(Some(&dir), "tg:reef"), Some(file));
        // Nowhere to look with the store in memory, and a bundled picture is
        // in the binary, not on disk.
        assert!(media_path(None, "tg:reef").is_none());
        assert!(media_path(Some(&dir), "demo:palette").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_player_runs_against_the_clock_and_starts_over_at_its_end() {
        let mut p = Player::over(7, 42.0);
        assert_eq!(p.state(100.0).position, 0.0);
        assert!(!p.state(100.0).playing);
        p.toggle(100.0);
        assert!(p.state(117.0).playing);
        assert_eq!(p.state(117.0).position, 17.0);
        p.toggle(117.0);
        assert!(!p.state(130.0).playing);
        assert_eq!(p.state(130.0).position, 17.0);
        p.toggle(130.0);
        let end = p.state(200.0);
        assert!(!end.playing && end.position == 42.0, "ran out");
        p.toggle(200.0);
        assert_eq!(p.state(201.0).position, 1.0, "played again, from the start");
    }

    #[test]
    fn a_recording_and_a_carried_file_say_what_they_are() {
        let r = Recording {
            kind: RecKind::Voice,
            since: 10.0,
        };
        assert_eq!(r.line(13.4), "recording voice 0:03");
        assert_eq!(Recording::keys(), "enter sends, esc discards");
        let v = Recording {
            kind: RecKind::Video,
            since: 10.0,
        };
        assert_eq!(v.elapsed(5.0), 0.0);
        assert!(v.line(70.0).starts_with("recording video message 1:00"));
        let c = |p: &str| Carried { path: p.to_string() };
        assert_eq!(c("~/Pictures/fold-cover.png").label(), "fold-cover.png · photo");
        assert_eq!(c("~/Downloads/clip.MOV").kind(), "video");
        assert_eq!(c("~/Music/track.mp3").kind(), "audio");
        assert_eq!(c("~/Downloads/report-q3.pdf").label(), "report-q3.pdf · file");
        assert_eq!(c("~/Downloads/report-q3.pdf").detail(), "file · ~/Downloads");
        assert_eq!(c("/etc/hosts").dir(), "/etc");
        assert_eq!(c("/hosts").dir(), "/");
        assert_eq!(c("notes").kind(), "file");
        assert_eq!(c("notes").dir(), "");
    }

    #[test]
    fn a_name_is_quoted_in_a_filter_only_when_it_has_to_be() {
        assert_eq!(filter_value("stelaxis"), "stelaxis");
        assert_eq!(filter_value("Hiking Saturday"), "\"Hiking Saturday\"");
    }
}
