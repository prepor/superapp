//! Attachment descriptions, on-demand IMAP downloads, and the local file cache.
//!
//! SQLite holds the reading and each file's MIME description and section.
//! File bodies live on the server and in the kernel's bounded blob cache,
//! shared with Telegram. Neither downloads nor cache paths are replicated.
//!
//! Ingest writes messages and their attachment rows in the same transaction;
//! replication carries that whole commit. Lists read those rows directly.

use std::path::PathBuf;
use std::rc::Rc;

use kernel::caps::{fmt_size, FileKind};
use kernel::store::{Store, Val, Q};

use super::model::{self, MailId};

/// One part's description, derived from the stored content snapshot.
/// [`part`] retrieves its bytes from the cache or the server.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub message: MailId,
    /// Its original MIME parser index, retained across content conversion.
    pub at: u32,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// Its Content-ID, for a part the reading refers to; empty otherwise.
    pub cid: String,
}

impl Attachment {
    /// What the message row's link says: the name, and how big it is.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, fmt_size(self.size))
    }

    /// What it is, as the card's kind line words it.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        FileKind::of_name(&self.name)
    }

    /// The identity of the card over it: `("attachment", [mail, at])`. The
    /// letter and its place in it, because a row's own id is derived and
    /// local to a device.
    #[must_use]
    pub fn panel(&self) -> kernel::panel::PanelId {
        super::panels::Card::id(self.message, self.at)
    }
}

static Q_ATTACHMENTS: Q = Q {
    id: "attachments",
    sql: "SELECT message, part, name, mime, size, cid
          FROM attachment WHERE message = ?1 ORDER BY part",
    describe: "the parts one letter carries, in the order they arrived",
};

static Q_ATTACHMENT: Q = Q {
    id: "attachment",
    sql: "SELECT message, part, name, mime, size, cid
          FROM attachment WHERE message = ?1 AND part = ?2",
    describe: "one part of a letter, by the letter and its place in it",
};

static Q_CARRIERS: Q = Q {
    id: "thread carriers",
    sql: "SELECT DISTINCT a.message FROM attachment a
          JOIN message m ON m.id = a.message
          WHERE m.thread = (SELECT thread FROM message WHERE id = ?1)",
    describe: "which mails of a conversation carry anything",
};

fn attachment_row(r: &rusqlite::Row) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        message: r.get(0)?,
        at: r.get::<_, i64>(1)? as u32,
        name: r.get(2)?,
        mime: r.get(3)?,
        size: r.get::<_, i64>(4)? as u64,
        cid: r.get(5)?,
    })
}

/// The parts one letter carries.
#[must_use]
pub fn attachments(store: &Store, id: MailId) -> Rc<Vec<Attachment>> {
    store.rows(&Q_ATTACHMENTS, &[Val::I(id)], attachment_row)
}

/// One part, by the letter and its place in it — the identity an
/// `attachment` panel persists.
#[must_use]
pub fn attachment(store: &Store, mail: MailId, at: u32) -> Option<Attachment> {
    store
        .rows(
            &Q_ATTACHMENT,
            &[Val::I(mail), Val::I(i64::from(at))],
            attachment_row,
        )
        .first()
        .cloned()
}

/// Which mails of a conversation carry anything — what the reader's height
/// wish adds a line for.
#[must_use]
pub fn thread_carriers(store: &Store, id: MailId) -> std::collections::BTreeSet<MailId> {
    store
        .rows(&Q_CARRIERS, &[Val::I(id)], |r| r.get::<_, MailId>(0))
        .iter()
        .copied()
        .collect()
}

/// One part's decoded bytes. Cache and network I/O belong on a worker;
/// neither a preview nor an open performs this on the live UI thread.
pub fn part(world: &kernel::effect::World, a: &Attachment) -> Result<Vec<u8>, String> {
    let raw = model::raw(world.store(), a.message).ok_or("message is no longer stored")?;
    let content = super::content::Content::read(&raw)?;
    let remote = content
        .parts
        .iter()
        .find(|p| p.part.at == a.at)
        .ok_or("attachment is no longer in the message")?;
    download(world, a.message, remote)
}

#[derive(Debug, PartialEq, Eq)]
struct Location {
    account: i64,
    email: String,
    host: String,
    folder: String,
    validity: u32,
    uid: u32,
}

static Q_LOCATION: Q = Q {
    id: "mail image source",
    sql: "SELECT a.id, a.email, COALESCE(a.imap_host, ''), f.name, f.uidvalidity, s.uid
          FROM message m JOIN account a ON a.id = m.account
          JOIN server_msg s ON s.message = m.id JOIN folder f ON f.id = s.folder
          WHERE m.id = ?1",
    describe: "the server identity of a letter's images and attachments",
};

fn location_row(r: &rusqlite::Row) -> rusqlite::Result<Location> {
    Ok(Location {
        account: r.get(0)?,
        email: r.get(1)?,
        host: r.get(2)?,
        folder: r.get(3)?,
        validity: r.get(4)?,
        uid: r.get(5)?,
    })
}

fn location(store: &Store, mail: MailId) -> Result<Location, String> {
    // Downloads need a fresh read before and after network I/O, including
    // commits the worker's store has not polled yet. Draws use rows below.
    store
        .conn()
        .query_row(Q_LOCATION.sql, [mail], location_row)
        .map_err(|_| {
            "message location is unavailable; sync before downloading this attachment".into()
        })
}

impl Location {
    fn key(&self, section: &str) -> String {
        // Length-delimited fields avoid collisions between accounts, folder
        // names and parts. UIDVALIDITY protects against server UID reuse.
        let identity = serde_json::to_vec(&(
            &self.host,
            &self.email,
            &self.folder,
            self.validity,
            self.uid,
            section,
        ))
        .unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, &identity);
        use std::fmt::Write as _;
        let mut key = "mail:".to_string();
        for byte in digest.as_ref() {
            let _ = write!(key, "{byte:02x}");
        }
        key
    }
}

/// A process-local image namespace that changes when the server identity
/// changes, and also distinguishes independent stores with the same row ids.
pub fn image_scope(store: &Store, mail: MailId) -> String {
    // Hash only when the query cache is filled. Both the join and its
    // derived identity stay cached until a source table changes.
    let identities = store.rows(&Q_LOCATION, &[Val::I(mail)], |r| Ok(location_row(r)?.key("")));
    let identity = identities.first().map_or("", String::as_str);
    format!(
        "m{mail}-{:p}-{identity}",
        std::sync::Arc::as_ptr(&store.db())
    )
}

pub fn download(
    world: &kernel::effect::World,
    mail: MailId,
    remote: &super::content::RemotePart,
) -> Result<Vec<u8>, String> {
    use kernel::caps::Blobs;
    let loc = location(world.store(), mail)?;
    let key = loc.key(&remote.section);
    if let Some(path) = world
        .with_cap::<dyn Blobs, _>(|b| b.get(&key))
        .ok()
        .flatten()
    {
        if let Ok(bytes) = std::fs::read(path) {
            return Ok(bytes);
        }
        let _ = world.with_cap::<dyn Blobs, _>(|b| b.remove(&key));
    }
    // The bundled demo is available even in a library mount with no network
    // capabilities. Its fixtures are compiled assets, never database blobs.
    if loc.email == super::seed::ADDRESS && loc.host.is_empty() {
        if let Some(m) = model::mail(world.store(), mail) {
            if let Some((_, bytes)) = super::seed::parts_of(&m.head.subject)
                .iter()
                .find(|(name, _)| *name == remote.part.name)
            {
                return Ok(bytes.to_vec());
            }
        }
    }
    super::sync::connect(world, loc.account)?;
    let encoded = world.run(&FetchPart {
        loc: &loc,
        section: &remote.section,
    })?;
    let bytes = super::content::decode(&remote.headers, &encoded)?;
    if location(world.store(), mail)? != loc {
        return Err("message moved while downloading; try again".into());
    }
    world.with_cap::<dyn Blobs, _>(|b| b.put(&key, &bytes))??;
    Ok(bytes)
}

struct FetchPart<'a> {
    loc: &'a Location,
    section: &'a str,
}

impl kernel::effect::Effect for FetchPart<'_> {
    const KIND: &'static str = "mail-part";
    type Reply = Vec<u8>;
    fn describe(&self) -> String {
        format!(
            "download part {} of uid {} in {}",
            self.section, self.loc.uid, self.loc.folder
        )
    }
    fn writes(&self) -> bool {
        false
    }
    fn perform(&self, cx: &mut kernel::effect::Ctx<'_>) -> Result<Vec<u8>, String> {
        cx.cap::<dyn super::caps::Imap>()?.part(
            self.loc.account,
            &self.loc.folder,
            self.loc.validity,
            self.loc.uid,
            self.section,
        )
    }
}

/// Rebuild the requesting world's capabilities on the picture reader thread.
/// Fakes share the exact same server, so tests can take a download offline.
#[derive(Clone)]
pub struct Reader {
    pub env: kernel::app::Env,
    pub mode: kernel::app::Mode,
    pub fake: Option<super::caps::FakeServers>,
}

impl Reader {
    pub fn world(
        &self,
        db: std::sync::Arc<kernel::store::Db>,
    ) -> Result<kernel::effect::World, String> {
        static APPS: &[&dyn kernel::app::App] = &[&super::MAIL];
        let store = Store::with_db(db).map_err(|e| e.to_string())?;
        let w = kernel::app::world_for(APPS, store, self.mode, &self.env);
        if let Some(fake) = &self.fake {
            let fake = fake.for_world();
            w.caps(|caps| {
                caps.insert::<dyn super::caps::Imap>(Box::new(fake.clone()));
                caps.insert::<dyn super::caps::OAuth>(Box::new(fake.clone()));
                caps.insert::<super::caps::FakeServers>(Box::new(fake));
            });
        }
        Ok(w)
    }
}

/// Records what a letter carries in the transaction that stores the letter,
/// so its attachment list is available on the first read. Part indices are
/// the stable identities used by attachment panels.
///
/// # Errors
///
/// If the store refuses the write.
pub fn attach_tx(
    c: &rusqlite::Connection,
    message: MailId,
    parts: &[super::sync::Part],
) -> rusqlite::Result<()> {
    for p in parts {
        c.execute(
            "INSERT INTO attachment(message, part, name, mime, size, cid)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(message, part) DO UPDATE SET
               name = excluded.name, mime = excluded.mime,
               size = excluded.size, cid = excluded.cid",
            rusqlite::params![
                message,
                i64::from(p.at),
                p.name,
                p.mime,
                p.size as i64,
                p.cid
            ],
        )?;
    }
    // Interpolated rather than bound: the values are part indices this build
    // just read off a MIME walk, and `NOT IN ()` is not a thing — a letter
    // that carries nothing has every row of its own to drop.
    let kept: Vec<String> = parts.iter().map(|p| p.at.to_string()).collect();
    let sql = if kept.is_empty() {
        "DELETE FROM attachment WHERE message = ?1".to_string()
    } else {
        format!(
            "DELETE FROM attachment WHERE message = ?1 AND part NOT IN ({})",
            kept.join(",")
        )
    };
    c.execute(&sql, [message])?;
    Ok(())
}

// -- where a part lands ------------------------------------------------------

/// Where a letter's part lands when it is opened: the app's own scratch
/// directory, **a folder per part**, so nothing can be overwritten by
/// anything — not another letter's part, and not this letter's second
/// `image.png`, which is a shape mail actually arrives in. The folder carries
/// the disambiguation so the file keeps the name the sender gave it, which is
/// the name the viewer will put in its title bar. An ordinary directory
/// either way, so a files panel can walk to it afterwards.
#[must_use]
pub fn scratch(mail: MailId, at: u32, name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("superapp-parts")
        .join(format!("mail-{mail}"))
        .join(format!("part-{at}"))
        // A part's filename comes off the wire: the last segment of it is
        // all that may reach the disk, and never `..`.
        .join(safe_name(name))
}

/// A filename from outside as a single, harmless segment: no separators, no
/// climbing, never empty.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if last.is_empty() || last == "." || last == ".." {
        "part".to_string()
    } else {
        last.to_string()
    }
}
