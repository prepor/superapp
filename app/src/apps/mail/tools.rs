//! What mail lets an agent do, by name.
//!
//! Each one is the verb's own code path over ids instead of over a cursor:
//! the filing goes through [`super::panels::mailbox::move_tx`] and claims the same intents the
//! mailbox's own *archive* and *put back* claim, the marking goes
//! through [`model::mark_read_tx`] and claims the same [`MarkRead`] the
//! reader's open does, and the send files the outbox row the compose
//! sheet's *send* files. So a tool and a button cannot disagree, and `cmd+z`
//! takes either of them back the same way.
//!
//! A conversation is named by any of its letters — `mail.search` answers the
//! anchor, and a reader's argument is a letter of it — because every query
//! here resolves the thread from the id it is handed.
//!
//! One rule the agent does not get to break: **it never sends what nobody
//! read**. `mail.draft` opens a compose panel with the letter in it, for the
//! person to look at; `mail.send` takes only a draft that already exists.

use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::session::{Edit, Session};
use kernel::store::Store;
use kernel::time::fmt_date_long;
use kernel::tool::{Prepare, Prepared, Read, Tool};
use serde_json::{json, Value};

use super::effects::{outbox_entity, MarkRead, Sent};
use super::model::{self, Draft, MailId, Role, Seed};
use super::panels::mailbox::{movable, moved, To};
use super::panels::Compose;
use super::reading;

/// How much of a conversation one call is worth — the same ceiling the
/// kernel's `sql.query` keeps, for the same reason.
const MAX_TEXT: usize = 64 * 1024;

/// How many letters one search is worth showing; the index ranks them, so
/// past this the answer is another word, not another page.
const MAX_HITS: i64 = 100;

/// The default when a search says nothing about how many it wants.
const HITS: i64 = 20;

/// Mail's tools, in the order a request lists them: the three that read, then
/// the five that file, then the two that mark, then writing a letter.
#[must_use]
pub fn all() -> Vec<Tool> {
    vec![
        Tool::reading(
            "mail.search",
            "Find letters by their words — sender, subject, body. This is the \
             way to turn “the mail from Vera about the budget” into ids you can \
             act on. Answers the conversation each letter belongs to, which is \
             what mail.archive, mail.thread and the rest take.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "the words to look for; the last one matches as a prefix"},
                    "limit": {"type": "integer", "description": "how many letters to answer, 100 at most"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            |input| read_on_worker(input, search),
        ),
        Tool::reading(
            "mail.thread",
            "Read a whole conversation: every letter of it, oldest first, with \
             who wrote it, when, and what it says. Call this before answering \
             any question about what a conversation is about — a subject line \
             is not the letter.",
            json!({
                "type": "object",
                "properties": {"thread": {"type": "integer", "description": "the conversation, as mail.search answers it"}},
                "required": ["thread"],
                "additionalProperties": false
            }),
            |input| read_on_worker(input, thread),
        ),
        Tool::reading(
            "mail.attachment",
            "Read a mail attachment's contents, downloading it on demand and reusing \
             the local cache. Use this to translate or summarize an attached PDF or \
             text file; an attachment preview contains metadata, not its contents. \
             Find mail and part in mail.thread, the attachment panel, or the attachment \
             table. Supports PDF text layers and UTF-8/UTF-16 text up to 32 MiB; \
             scanned PDFs need OCR. Returns at most 64 KiB of text; continue with \
             next_offset until truncated is false. Does not mark mail read.",
            json!({
                "type": "object",
                "properties": {
                    "mail": {"type": "integer", "description": "message.id of the letter carrying the file"},
                    "part": {"type": "integer", "description": "attachment.part (not attachment.id)"},
                    "offset": {"type": "integer", "description": "omit for the start; use next_offset from a previous read"}
                },
                "required": ["mail", "part"], "additionalProperties": false
            }),
            attachment,
        ),
        Tool::preparing(
            "mail.archive",
            "Archive a conversation out of the inbox — the same thing the \
             archive button does, and the person takes it back with cmd+z. The \
             conversation has to be in the inbox; from anywhere else there is \
             nothing to archive.",
            one("the conversation to archive"),
            |input| prepare_on_worker(input, |world, input| file(world, input, Role::Inbox, To::Role("archive"))),
        ),
        Tool::preparing(
            "mail.delete",
            "Put a conversation in the trash — the same thing the delete button \
             does, and undoable the same way. It moves the copies in whichever \
             mailbox holds it.",
            one("the conversation to delete"),
            |input| prepare_on_worker(input, delete),
        )
        // The trash is not the archive: what a person means by *delete* is
        // the one filing they would want to be asked about.
        .asking(),
        Tool::preparing(
            "mail.not_spam",
            "Take a conversation out of the junk and back into the inbox. It \
             has to be in the spam for there to be anything to do.",
            one("the conversation to take out of the spam"),
            |input| prepare_on_worker(input, |world, input| file(world, input, Role::Spam, To::Role("inbox"))),
        ),
        Tool::preparing(
            "mail.put_back",
            "Take a conversation out of the trash and back where it was \
             deleted from — the inbox for a letter that never passed through \
             this device's own delete. It has to be in the trash for there to \
             be anything to do.",
            one("the conversation to put back"),
            |input| prepare_on_worker(input, |world, input| file(world, input, Role::Trash, To::Back)),
        ),
        Tool::preparing(
            "mail.read",
            "Mark every unread letter of a conversation as read, exactly as \
             opening it does.",
            one("the conversation to mark read"),
            |input| prepare_on_worker(input, read),
        ),
        Tool::preparing(
            "mail.unread",
            "Mark a conversation unread again, so it stands out in the mailbox \
             as something to come back to.",
            one("the conversation to mark unread"),
            |input| prepare_on_worker(input, unread),
        ),
        Tool::staging(
            "mail.draft",
            "Write a letter and open it in a compose panel for the person to \
             read. It is not sent: the person looks at it, edits it if they \
             like, and either presses send or asks you to. Answers the slot the \
             sheet landed in, which is what mail.send takes.",
            json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "the recipient's address"},
                    "subject": {"type": "string"},
                    "body": {"type": "string"},
                    "re": {"type": "integer", "description": "the letter this answers, so it threads as a reply"}
                },
                "required": ["to", "subject", "body"],
                "additionalProperties": false
            }),
            true,
            draft,
        ),
        Tool::staging(
            "mail.send",
            "Send a draft that is already open — the one mail.draft answered \
             with. There is a short window in which cmd+z takes it back; after \
             that the letter has gone. Never send a letter the person has not \
             seen.",
            json!({
                "type": "object",
                "properties": {"slot": {"type": "integer", "description": "the compose panel's slot, as mail.draft answered it"}},
                "required": ["slot"],
                "additionalProperties": false
            }),
            true,
            send,
        )
        // A letter that has gone has left the machine: the undo window is
        // seconds, and the person's word comes before it.
        .asking(),
    ]
}

/// The schema of every tool that names one conversation and nothing else.
fn one(what: &str) -> Value {
    json!({
        "type": "object",
        "properties": {"thread": {"type": "integer", "description": what}},
        "required": ["thread"],
        "additionalProperties": false
    })
}

/// Native preparation/read work owns a factory so SQL and MIME conversion do
/// not monopolize either the UI or the local network executor.
fn prepare_on_worker(
    input: &Value,
    prepare: fn(&World, &Value) -> Result<Prepared, String>,
) -> Prepare {
    let input = input.clone();
    Box::new(move |world| {
        Box::pin(async move {
            if let Some(factory) = world.factory() {
                kernel::runtime::spawn_blocking(move || {
                    let world = factory.build().map_err(|e| e.to_string())?;
                    prepare(&world, &input)
                })
                .await
                .map_err(|e| e.to_string())?
            } else {
                prepare(world, &input)
            }
        })
    })
}

fn read_on_worker(input: &Value, read: fn(&World, &Value) -> Result<Value, String>) -> Read {
    let input = input.clone();
    Box::new(move |world| {
        Box::pin(async move {
            if let Some(factory) = world.factory() {
                kernel::runtime::spawn_blocking(move || {
                    let world = factory.build().map_err(|e| e.to_string())?;
                    read(&world, &input)
                })
                .await
                .map_err(|e| e.to_string())?
            } else {
                read(world, &input)
            }
        })
    })
}

fn changed() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
        Some("the conversation changed before this edit; read it again".into()),
    )
}

// -- reading -----------------------------------------------------------------------

/// The FTS5 index the launcher's mail source reads, cut by the same
/// [`model::fts_match`], answering the columns an agent acts on rather than
/// the label a hit draws. The trash is left out, as it is everywhere else: a
/// deleted letter is no longer part of the conversation it was in.
fn search(s: &World, input: &Value) -> Result<Value, String> {
    let query = text(input, "query")?;
    let limit = int(input, "limit")?.unwrap_or(HITS).clamp(1, MAX_HITS);
    let Some(m) = model::fts_match(query) else {
        return Ok(json!({"letters": []}));
    };
    let store = s.store();
    store.poll_external();
    let mut stmt = store
        .conn()
        .prepare_cached(
            "SELECT COALESCE(m.thread, m.id), m.id, m.from_name, m.from_email, m.subject, m.date
             FROM message_fts JOIN message m ON m.id = message_fts.rowid
                              JOIN folder f ON f.id = m.folder
             WHERE message_fts MATCH ?1 AND f.role IS NOT 'trash'
             ORDER BY message_fts.rank
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    // A malformed match string is the one error a person can cause from the
    // keyboard, and SQLite's word for it is the word to pass on.
    let rows = stmt
        .query_map(rusqlite::params![m, limit], |r| {
            Ok(json!({
                "thread": r.get::<_, i64>(0)?,
                "mail": r.get::<_, MailId>(1)?,
                "from": who(&r.get::<_, String>(2)?, &r.get::<_, String>(3)?),
                "subject": r.get::<_, String>(4)?,
                "date": fmt_date_long(r.get::<_, f64>(5)?),
            }))
        })
        .map_err(|e| e.to_string())?;
    let letters: Vec<Value> = rows.filter_map(Result::ok).collect();
    Ok(json!({"letters": letters}))
}

/// One conversation as text: every letter of it, oldest first, in the
/// reading a person sees — the HTML narrowed to words where the sender sent
/// one, the plain text otherwise, and the quoted tail folded away, because
/// the letter above it is already here.
fn thread(s: &World, input: &Value) -> Result<Value, String> {
    let id = int(input, "thread")?.ok_or("`thread` must be an integer")?;
    let store = s.store();
    let msgs = model::thread(store, id);
    if msgs.is_empty() {
        return Err(format!("no conversation at {id}"));
    }
    let mut letters: Vec<Value> = Vec::with_capacity(msgs.len());
    let mut bytes = 0usize;
    let mut truncated = false;
    for m in &msgs {
        let text = reading::own_text(&m.mail);
        bytes += text.len();
        if bytes > MAX_TEXT && !letters.is_empty() {
            truncated = true;
            break;
        }
        letters.push(json!({
            "mail": m.mail.head.id,
            "from": who(&m.mail.head.from_name, &m.mail.head.from_email),
            "to": m.mail.to,
            "date": fmt_date_long(m.mail.head.date),
            "mailbox": m.role,
            "unread": m.mail.head.unread,
            "text": text,
            "attachments": super::parts::attachments(store, m.mail.head.id).iter().map(|a| json!({
                "mail": a.message, "part": a.at, "name": a.name, "mime": a.mime, "size": a.size,
            })).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({
        "thread": id,
        "topic": model::thread_topic(store, id),
        "letters": letters,
        "truncated": truncated,
    }))
}

fn attachment(input: &Value) -> kernel::tool::Read {
    let input = input.clone();
    Box::new(move |world| {
        Box::pin(async move {
            let mail = int(&input, "mail")?.ok_or("`mail` must be an integer")?;
            let part = int(&input, "part")?
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("`part` must be a nonnegative 32-bit integer")?;
            let offset = crate::reader::document::offset(&input)?;
            let a = super::parts::attachment(world.store(), mail, part)
                .ok_or_else(|| format!("no attachment at mail {mail}, part {part}"))?;
            crate::reader::document::check_size(a.size)?;
            let bytes = super::parts::part(world, &a).await?;
            let size = bytes.len();
            let (name, mime) = (a.name.clone(), a.mime.clone());
            let mut out = kernel::runtime::spawn_blocking(move || {
                crate::reader::document::read(&bytes, &name, &mime, offset)
            })
            .await
            .map_err(|e| e.to_string())??;
            out["mail"] = json!(mail);
            out["part"] = json!(part);
            out["name"] = json!(a.name);
            out["mime"] = json!(a.mime);
            out["size"] = json!(size);
            Ok(out)
        })
    })
}

// -- filing --------------------------------------------------------------------------

/// *archive* and *not spam*: the copies of one conversation that sit in
/// `from`, moved into `role`, as one undoable action claiming one [`Filed`]
/// per letter — the mailbox verb's data half, over an id instead of over the
/// marks.
///
/// Which letters move is the mailbox's own rule: a conversation can be a row
/// in two mailboxes at once, and archiving from the inbox takes the inbox
/// copies. A tool has no cursor to read that off, so the verb that only
/// makes sense from one mailbox names it here.
fn file(s: &World, input: &Value, from: Role, to: To) -> Result<Prepared, String> {
    let thread = int(input, "thread")?.ok_or("`thread` must be an integer")?;
    let moving = folder_mails(s.store(), from, thread);
    if moving.is_empty() {
        return Err(format!(
            "that conversation is not in the {} — nothing to {}",
            from.as_str(),
            to.word()
        ));
    }
    filed(s, thread, &moving, to)
}

/// *delete*: the trash is where mail goes from anywhere, so this takes the
/// copies in whichever mailbox holds the conversation — the mailboxes asked
/// in the order the launcher lists them, the trash left out because that is
/// where they are going.
fn delete(s: &World, input: &Value) -> Result<Prepared, String> {
    let thread = int(input, "thread")?.ok_or("`thread` must be an integer")?;
    let moving = model::ROLES
        .into_iter()
        .filter(|role| *role != Role::Trash)
        .map(|role| folder_mails(s.store(), role, thread))
        .find(|m| !m.is_empty())
        .unwrap_or_default();
    if moving.is_empty() {
        return Err(format!("no conversation at {thread} in any mailbox"));
    }
    filed(s, thread, &moving, To::Role("trash"))
}

/// The action itself, once the letters are known: one node, labelled by what
/// the conversation is called, claiming the reversal of every move.
fn filed(s: &World, thread: i64, letters: &[MailId], to: To) -> Result<Prepared, String> {
    let store = s.store().clone();
    let moving: Vec<(MailId, i64)> = letters
        .iter()
        .filter(|id| movable(&store, **id, to))
        .map(|id| (*id, model::folder_of(&store, *id)))
        .collect();
    if moving.is_empty() {
        return Err(format!(
            "there is nowhere to {} that conversation to",
            to.word()
        ));
    }
    let intents = moving
        .iter()
        .map(|(mail, from_folder)| moved(&store, *mail, *from_folder, to))
        .collect();
    let ids: Vec<MailId> = moving.iter().map(|(id, _)| *id).collect();
    let n = ids.len();
    let title = topic(&store, thread);
    let expected: Vec<_> = ids
        .iter()
        .map(|id| {
            (
                *id,
                model::folder_of(&store, *id),
                model::trashed_from(&store, *id),
                model::put_back_target(&store, *id),
            )
        })
        .collect();
    Ok(Prepared::Edit(Edit::writing("file", format!("{} “{title}”", to.word()), move |tx| {
        for (id, folder, trashed, back) in &expected {
            let current: (i64, Option<i64>) = tx.query_row(
                "SELECT folder,(SELECT folder FROM trashed WHERE message=m.id) FROM message m WHERE id=?",
                [id], |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if current != (*folder, *trashed) || (to == To::Back && model::put_back_target_in(tx, *id) != *back) {
                return Err(changed());
            }
        }
        for id in &ids {
            let changed = match to { To::Role(role) => model::file_tx(tx, *id, role)?, To::Back => model::put_back_tx(tx, *id)? };
            if !changed { return Err(self::changed()); }
        }
        Ok(json!({"thread": thread, "letters": n, "mailbox": mailbox_of(to)}))
    }).claiming(intents)))
}

/// What the tool answers with as the mailbox a conversation landed in. A put
/// back has no one answer — each letter went where it came from — so it says
/// what it did instead.
fn mailbox_of(to: To) -> &'static str {
    match to {
        To::Role(role) => role,
        To::Back => "put back",
    }
}

/// The mails of one conversation that sit in a mailbox of this role — what
/// filing the row moves, decided exactly as
/// [`Mailbox`](super::panels::Mailbox)'s batch verb decides it: the row's
/// own letter, and then whichever of its siblings share that letter's
/// folder.
fn folder_mails(store: &Store, role: Role, thread: i64) -> Vec<MailId> {
    let Some(head) = model::thread_head(store, role, thread) else {
        return Vec::new();
    };
    model::thread_siblings(store, head.target)
}

// -- marking -------------------------------------------------------------------------

/// Every unread letter of a conversation marked read, one [`MarkRead`] each
/// — what opening the reader claims, without opening it.
fn read(s: &World, input: &Value) -> Result<Prepared, String> {
    let thread = int(input, "thread")?.ok_or("`thread` must be an integer")?;
    let store = s.store().clone();
    let unread = model::thread_unread(&store, thread);
    if unread.is_empty() {
        return Err("that conversation is read already".to_string());
    }
    let n = unread.len();
    let intents = unread
        .iter()
        .map(|m| Box::new(MarkRead { mail: *m }) as Box<dyn kernel::history::Intent>)
        .collect();
    let marks = unread.clone();
    let title = topic(&store, thread);
    Ok(Prepared::Edit(
        Edit::writing("read", format!("read “{title}”"), move |tx| {
            for mail in &marks {
                let unread: bool =
                    tx.query_row("SELECT unread FROM message WHERE id=?", [mail], |r| {
                        r.get(0)
                    })?;
                if !unread {
                    return Err(changed());
                }
            }
            for mail in &marks {
                model::mark_read_tx(tx, *mail)?;
            }
            Ok(json!({"thread": thread, "letters": n}))
        })
        .claiming(intents),
    ))
}

/// The other way: a conversation put back where a person would find it.
fn unread(s: &World, input: &Value) -> Result<Prepared, String> {
    let thread = int(input, "thread")?.ok_or("`thread` must be an integer")?;
    let store = s.store().clone();
    let letters: Vec<MailId> = model::thread(&store, thread)
        .into_iter()
        .filter(|m| !m.mail.head.unread)
        .map(|m| m.mail.head.id)
        .collect();
    if letters.is_empty() {
        return Err("that conversation is unread already".to_string());
    }
    let n = letters.len();
    let intents = letters
        .iter()
        .map(|m| Box::new(MarkUnread { mail: *m }) as Box<dyn kernel::history::Intent>)
        .collect();
    let marks = letters.clone();
    let title = topic(&store, thread);
    Ok(Prepared::Edit(
        Edit::writing("read", format!("unread “{title}”"), move |tx| {
            for mail in &marks {
                let unread: bool =
                    tx.query_row("SELECT unread FROM message WHERE id=?", [mail], |r| {
                        r.get(0)
                    })?;
                if unread {
                    return Err(changed());
                }
            }
            for mail in &marks {
                mark_unread_tx(tx, *mail)?;
            }
            Ok(json!({"thread": thread, "letters": n}))
        })
        .claiming(intents),
    ))
}

/// The mirror of [`model::mark_read_tx`]. Intent only, like its twin: the
/// sync pass pushes wherever intent and the server disagree.
fn mark_unread_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE message SET unread = 1 WHERE id = ?1 AND unread = 0",
        [id],
    )?;
    Ok(())
}

/// What `mail.unread` claimed: a letter put back to unread. The mirror of
/// [`MarkRead`], and the only claim in this app that no button makes —
/// nothing in the surface marks a letter unread yet.
struct MarkUnread {
    mail: MailId,
}

impl kernel::history::Intent for MarkUnread {
    fn describe(&self) -> String {
        format!("mail:{} unread", self.mail)
    }
    fn reverse(&self, w: &kernel::effect::World) -> Result<(), String> {
        let mail = self.mail;
        w.store()
            .write(move |c| model::mark_read_tx(c, mail))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &kernel::effect::World) -> Result<(), String> {
        let mail = self.mail;
        w.store()
            .write(move |c| mark_unread_tx(c, mail))
            .map_err(|e| e.to_string())
    }
}

// -- writing a letter -------------------------------------------------------------------

/// A compose sheet, opened beside the panel that has focus with the letter
/// already in it. The panel is the point: the person reads what the agent
/// wrote before anything leaves.
///
/// The draft row is written the way a keystroke writes it — straight through
/// the store, not as an action — because typing is the future editor's local
/// undo, not the workspace's. What is undoable here is the panel.
fn draft(s: &mut Session, input: &Value) -> Result<Prepare, String> {
    let draft = Draft {
        to: text(input, "to")?.to_string(),
        subject: text(input, "subject")?.to_string(),
        body: text(input, "body")?.to_string(),
    };
    let seed = match int(input, "re")? {
        Some(id) => {
            let exists: bool = s
                .store()
                .conn()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM message WHERE id=?)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !exists {
                return Err(format!("no letter at {id} to answer"));
            }
            Seed::Reply(id)
        }
        None => Seed::Blank,
    };
    let from = s
        .focus()
        .ok_or("no panel has focus, so there is nowhere to put the sheet")?;
    let id = Compose::id(seed);
    let before: Vec<SlotId> = s.showing(&id);
    s.nav(Nav::Preview {
        from,
        id: id.clone(),
    });
    let slot = s
        .showing(&id)
        .into_iter()
        .find(|slot| !before.contains(slot))
        .ok_or("the compose sheet did not open")?;
    let key = i64_of(slot);
    Ok(Box::new(move |world| {
        Box::pin(async move {
            let now = world.now();
            let edit = Edit::writing("draft", "write mail draft", move |tx| {
                // The sheet was opened before preparation. Typing in that new
                // sheet takes precedence over the agent's initial draft.
                if model::draft_any_in(tx, key)?.is_some() {
                    return Err(changed());
                }
                if let Some(source) = seed.source() {
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM message WHERE id=?)",
                        [source],
                        |r| r.get(0),
                    )?;
                    if !exists {
                        return Err(changed());
                    }
                }
                model::upsert_draft_tx(tx, key, seed, &draft, now)?;
                Ok(json!({"slot": slot}))
            })
            .record_if(|_| false);
            Ok(Prepared::Edit(edit))
        })
    }))
}

/// The send: the draft as it stands, an outbox row that comes due after the
/// window, and the sheet closed behind it — the compose panel's own verb,
/// by slot instead of by button. One action, so one undo takes the letter
/// back and the panel with it, until the sender has taken the row
/// ([`Sent::blocked`](super::effects::Sent)).
fn send(s: &mut Session, input: &Value) -> Result<Prepare, String> {
    let slot = int(input, "slot")?.ok_or("`slot` must be an integer")?;
    let slot = SlotId::try_from(slot).map_err(|_| format!("no compose panel at slot {slot}"))?;
    let captured = if let Some(panel) = s.panel(slot) {
        let mut panel = panel.borrow_mut();
        let title = panel.title();
        let compose = panel
            .as_any()
            .downcast_mut::<Compose>()
            .ok_or("there is no draft in that slot — write one with mail.draft first")?;
        let (draft, seed, hold) = compose.hold_send()?;
        Some((draft, seed, title, hold))
    } else {
        None
    };
    Ok(Box::new(move |world| {
        Box::pin(async move {
            let key = i64_of(slot);
            // Staging flushed any latest coalesced UI text. A prepared send may
            // read only after that accepted save has passed through the writer.
            let stored = if world.factory().is_some() {
                world
                    .store()
                    .flush_async()
                    .await
                    .map_err(|e| e.to_string())?;
                world
                    .store()
                    .db()
                    .read_async(move |conn| model::draft_any_in(conn, key))
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                model::draft_any_in(world.store().conn(), key).map_err(|e| e.to_string())?
            };
            let (draft, seed) =
                stored.ok_or("there is no draft in that slot — write one with mail.draft first")?;
            if draft.to.trim().is_empty() {
                return Err("no recipient".into());
            }
            let (title, hold) = if let Some((shown, source, title, hold)) = captured {
                if shown != draft || source != seed {
                    return Err("the draft changed before sending; review it again".into());
                }
                (title, Some(hold))
            } else {
                (title_of(world.store(), seed), None)
            };
            let delay = model::send_delay();
            let now = world.now();
            let clock = world.factory().map(|factory| factory.clock());
            Ok(Prepared::Edit(
                Edit::writing("send", format!("send “{title}”"), move |tx| {
                    if model::draft_any_in(tx, key)? != Some((draft.clone(), seed)) {
                        return Err(changed());
                    }
                    model::file_send_tx(
                        tx,
                        key,
                        clock.as_ref().map_or(now, |clock| clock.read()) + delay,
                    )?;
                    Ok(json!({"outbox": key, "sending_in": delay}))
                })
                .about(outbox_entity(key))
                .claiming(vec![Box::new(Sent { slot: key, delay })])
                .on_commit(move |_| {
                    Box::new(move |session| {
                        // The hold identifies this exact sheet even if navigation
                        // replaced another compose into the same slot meanwhile.
                        let close = hold.as_ref().is_some_and(|hold| {
                            session.panel(slot).is_some_and(|panel| {
                                panel
                                    .borrow_mut()
                                    .as_any()
                                    .downcast_mut::<Compose>()
                                    .is_some_and(|compose| compose.holds(hold))
                            })
                        });
                        if close {
                            session.nav_within(Nav::Close {
                                slot,
                                label: Some(title),
                            });
                        }
                        drop(hold);
                    })
                }),
            ))
        })
    }))
}

/// What the sheet is called: the panel's own title while it is open, and the
/// same sentence derived from the seed once it has closed.
fn title_of(store: &Store, seed: Seed) -> String {
    match seed {
        Seed::Blank => "new mail".to_string(),
        Seed::Reply(id) => model::mail(store, id)
            .map_or_else(|| "new mail".into(), |m| format!("re: {}", m.head.subject)),
        Seed::Forward(id) => model::mail(store, id)
            .map_or_else(|| "new mail".into(), |m| format!("fwd: {}", m.head.subject)),
    }
}

// -- the small readings ---------------------------------------------------------------

/// What a conversation is called, for a history node. The subject of its
/// oldest letter, reply prefixes stripped — what the mailbox row says.
fn topic(store: &Store, thread: i64) -> String {
    model::thread_topic(store, thread).unwrap_or_else(|| "the conversation".to_string())
}

/// Who wrote: the name as of their letter, the address when they signed
/// none.
fn who(name: &str, email: &str) -> String {
    if name.is_empty() {
        email.to_string()
    } else {
        format!("{name} <{email}>")
    }
}

/// A slot id as the draft table keys by it — the same reading the compose
/// panel does, since the two must agree on which row is whose.
fn i64_of(slot: SlotId) -> i64 {
    i64::try_from(slot).unwrap_or(i64::MAX)
}

fn text<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn int(input: &Value, key: &str) -> Result<Option<i64>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}
