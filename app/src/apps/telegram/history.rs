//! User commands in the shell's history. The tree records the desired state;
//! Telegram acknowledgements serialize each command, its undo and its redo.
//! Nothing here rewinds the message projection or automatically retries a send.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex};

use kernel::effect::World;
use kernel::history::Intent;
use kernel::session::{Action, Session};
use kernel::store::Store;
use serde_json::{json, Value};
use rusqlite::OptionalExtension;

use super::operations::{Receipt, Status};
use super::{model, panels, requests, runtime};

mod deletion;
mod reaction;

/// The caller owns refusal feedback, including offline/demo behavior. Session
/// transaction failures have already been reported and must not toast twice.
#[derive(Debug)]
pub(super) enum Refusal {
    Offline,
    Failed(String),
    Reported,
}

impl Refusal {
    pub(super) fn reason(&self) -> &str {
        match self {
            Self::Offline => "Telegram is not connected",
            Self::Failed(reason) => reason,
            Self::Reported => "the action could not be recorded; nothing was sent",
        }
    }

    pub(super) fn notify(self, s: &mut Session, what: &str) {
        match self {
            Self::Offline => s.notify(super::draft_toast(what), false),
            Self::Failed(reason) => s.notify(format!("{what} failed: {reason}"), true),
            Self::Reported => {}
        }
    }
}

pub(super) fn command(s: &mut Session, request: &str) -> Result<u64, Refusal> {
    submit(s, &[request.to_string()], None).map(|ids| ids[0])
}

pub(super) fn batch(s: &mut Session, requests: &[String], label: String) -> Result<Vec<u64>, Refusal> {
    submit(s, requests, Some(label))
}

type Description = (&'static str, String, Undo);

fn describe(conn: &rusqlite::Connection, v: &Value) -> rusqlite::Result<Option<Description>> {
    let (kind, label, undo) = match v["@type"].as_str().unwrap_or("") {
        "sendMessage" => {
            let text = v["input_message_content"]["text"]["text"].as_str().unwrap_or("");
            let label = if text.is_empty() { "send attachment".into() }
                else { format!("send “{}”", text.chars().take(40).collect::<String>()) };
            ("send", label, Undo::Send)
        }
        "forwardMessages" => ("forward", "forward messages".into(), Undo::Send),
        "editMessageText" | "editMessageCaption" => ("edit", "edit message".into(), Undo::Edit),
        "deleteMessages" if deletion::eligible(conn, v)? =>
            ("delete", "delete messages (undo resends copies)".into(), Undo::Delete(None)),
        "deleteMessages" => ("delete", "delete messages (cannot undo)".into(), Undo::Impossible),
        "addMessageReaction" => ("react", format!("react {}", v["reaction_type"]["emoji"].as_str().unwrap_or("")),
            Undo::Reaction(reaction::Snapshot::default())),
        "setChatNotificationSettings" => ("mute", "change chat notifications".into(), Undo::Chat),
        "toggleChatIsPinned" => ("pin", "change chat pin".into(), Undo::Chat),
        "addChatToList" => ("archive", "move chat".into(), Undo::Chat),
        _ => return Ok(None),
    };
    let peer: Option<String> = if let Some(chat) = v["chat_id"].as_i64() {
        conn.query_row("SELECT name FROM tg_peer WHERE id = ?1", [chat], |row| row.get(0)).optional()?
    } else { None };
    let label = peer.map_or(label.clone(), |peer| format!("{label} · {peer}"));
    Ok(Some((kind, label, undo)))
}

enum Undo {
    Send,
    Edit,
    Reaction(reaction::Snapshot),
    Chat,
    Delete(Option<deletion::Saved>),
    Commands(Vec<Value>),
    Impossible,
}

#[derive(Default)]
struct Journal {
    entries: Mutex<Vec<Arc<Mutex<Change>>>>,
    next: AtomicU64,
    pumping: Mutex<()>,
    preparing: Mutex<HashSet<(i64, i64)>>,
}

/// A gesture reserves its exact targets until its metadata has been read and
/// the UI can file the command. Closing its panel does not cancel the gesture.
struct Preparing {
    journal: Arc<Journal>,
    keys: HashSet<(i64, i64)>,
}

impl Drop for Preparing {
    fn drop(&mut self) {
        let mut pending = self.journal.preparing.lock().unwrap();
        for key in &self.keys { pending.remove(key); }
    }
}

/// Deletion needs metadata for every selected message. Read one consistent
/// projection snapshot in the bounded reader pool, then recheck admission
/// before creating any operation, history node or wire side effect.
pub(super) fn delete(s: &mut Session, chat: i64, messages: Vec<i64>,
    complete: impl FnOnce(&mut Session, Result<u64, Refusal>) + 'static) {
    if let Err(error) = admit(s) { complete(s, Err(error)); return; }
    let request = requests::delete_messages(chat, &messages, true);
    let value: Value = serde_json::from_str(&request).expect("delete request JSON");
    let journal = s.store().local::<Journal>();
    let keys = targets(&value, &[]).into_iter().collect::<HashSet<_>>();
    if busy(s.store(), &journal, &keys) {
        complete(s, Err(Refusal::Failed("wait for the previous Telegram change to finish".into())));
        return;
    }
    journal.preparing.lock().unwrap().extend(keys.iter().copied());
    let preparing = Preparing { journal, keys };
    let asynchronous = s.store().ui_attached() || s.world().factory().is_some();
    s.prepare_work(move |world| Box::pin(async move {
        let descriptions = if asynchronous {
            let checked = value.clone();
            world.store().db().read_async(move |conn| describe(conn, &checked)).await
        } else {
            describe(world.store().conn(), &value)
        }.map_err(|error| error.to_string())?;
        Ok((request, value, descriptions))
    }), move |s, prepared| {
        drop(preparing);
        let result = prepared.map_err(Refusal::Failed).and_then(|(request, value, description)| {
            submit_prepared(s, &[request], vec![value], vec![description], None).map(|ids| ids[0])
        });
        complete(s, result);
    });
}

fn admit(s: &Session) -> Result<(), Refusal> {
    if !panels::live(s.store()) { return Err(Refusal::Offline); }
    let rt = runtime::of(s.store());
    if let Some(error) = rt.connection_error() { return Err(Refusal::Failed(error)); }
    if !rt.can_send() { return Err(Refusal::Failed("Telegram is not connected".into())); }
    if s.history_busy() { return Err(Refusal::Failed("wait for the undo operation to finish".into())); }
    if !s.writable() || !s.store().is_writable() {
        return Err(Refusal::Failed("another device holds the lease — nothing was sent".into()));
    }
    Ok(())
}

fn busy(store: &Store, journal: &Journal, keys: &HashSet<(i64, i64)>) -> bool {
    journal.preparing.lock().unwrap().iter().any(|key| keys.contains(key))
        || journal.entries.lock().unwrap().iter().any(|entry| {
            let change = entry.lock().unwrap();
            change.pending() && change.targets(store).iter().any(|key| keys.contains(key))
        })
}

#[derive(Default)]
struct Aliases(Mutex<HashMap<(i64, i64), (i64, i64)>>);

struct Flight {
    receipt: Arc<Mutex<Receipt>>,
    applied: bool,
    remaining: VecDeque<Value>,
}

struct Change {
    label: String,
    initial: Value,
    undo: Undo,
    prepared: bool,
    snapshot: Option<(u64, Arc<Mutex<Receipt>>)>,
    desired: bool,
    applied: bool,
    flight: Option<Flight>,
    messages: Vec<(i64, i64)>,
    failed: Option<String>,
    order: u64,
}

/// Only deliberate user commands enter here. Reads, typing and projection
/// updates keep using the ordinary queue and create no history nodes.
fn submit(s: &mut Session, requests: &[String], label: Option<String>) -> Result<Vec<u64>, Refusal> {
    if requests.is_empty() { return Ok(Vec::new()); }
    if !panels::live(s.store()) { return Err(Refusal::Offline); }
    let rt = runtime::of(s.store());
    if let Some(error) = rt.connection_error() {
        return Err(Refusal::Failed(error));
    }
    if !rt.can_send() { return Err(Refusal::Failed("Telegram is not connected".into())); }
    let values: Vec<Value> = requests.iter().map(|request| serde_json::from_str(request))
        .collect::<Result<_, _>>().map_err(|e| Refusal::Failed(e.to_string()))?;
    let descriptions = values.iter().map(|v| describe(s.store().conn(), v))
        .collect::<rusqlite::Result<_>>().map_err(|error| Refusal::Failed(error.to_string()))?;
    submit_prepared(s, requests, values, descriptions, label)
}

fn submit_prepared(s: &mut Session, requests: &[String], values: Vec<Value>,
    descriptions: Vec<Option<Description>>, label: Option<String>) -> Result<Vec<u64>, Refusal> {
    if descriptions.iter().all(Option::is_none) {
        // Reads and other ordinary requests still bypass the undo tree.
        return requests.iter().map(|request| panels::queue(s.store(), request)
            .ok_or_else(|| Refusal::Failed("Telegram is not connected".into()))).collect();
    }
    admit(s)?;
    let rt = runtime::of(s.store());
    // Live account projection progresses receipts; a UI gesture must not
    // wait behind attachment backup work owned by that projection.
    if !s.store().ui_attached() { pump(s.store()); }
    let journal = s.store().local::<Journal>();
    let keys = values.iter().flat_map(|v| targets(v, &[])).collect();
    if busy(s.store(), &journal, &keys) {
        return Err(Refusal::Failed("wait for the previous Telegram change to finish".into()));
    }
    let (kind, first_label, _) = descriptions.iter().flatten().next().unwrap();
    let kind = *kind;
    let label = label.unwrap_or_else(|| first_label.clone());
    let mut tracked = Vec::new();
    let mut changes = Vec::new();
    for (request, description) in requests.iter().zip(descriptions) {
        let request = rt.operations.track(request);
        let initial: Value = serde_json::from_str(&request).unwrap();
        let id = initial["@extra"]["operation"].as_u64().unwrap();
        if let Some((_, label, undo)) = description {
            let receipt = rt.operations.watch(id).unwrap();
            let prepared = !matches!(undo, Undo::Edit | Undo::Reaction(_) | Undo::Chat | Undo::Delete(_));
            changes.push(Arc::new(Mutex::new(Change {
                label, initial, undo, prepared, snapshot: None,
                desired: true, applied: false, messages: Vec::new(), failed: None,
                order: journal.next.fetch_add(1, Ordering::Relaxed),
                flight: Some(Flight { receipt, applied: true, remaining: VecDeque::new() }),
            })));
        }
        tracked.push((id, request));
    }
    // One gesture is one transaction and one node. Each request still owns
    // its snapshot and receipt, so a quick undo waits for acknowledgements.
    let independent_sends = values.iter().all(|v| v["@type"] == "sendMessage");
    let remote = Remote { label: label.clone(), changes: changes.clone(), independent_sends };
    if s.act(Action::new(kind, label).claiming(vec![Box::new(remote)])).is_none() {
        for (id, _) in tracked {
            rt.operations.fail(s.store(), id, "The action could not be recorded; nothing was sent", false);
            rt.operations.forget_payload(id);
        }
        return Err(Refusal::Reported);
    }
    journal.entries.lock().unwrap().extend(changes);
    if !rt.send_batch(tracked.iter().map(|(_, request)| request.as_str())) {
        for (id, _) in &tracked {
            rt.operations.fail(s.store(), *id, "Telegram is disconnected; the request was not sent", false);
            rt.operations.forget_payload(*id);
        }
        return Err(Refusal::Failed("Telegram is disconnected; the request could not be queued".into()));
    }
    Ok(tracked.into_iter().map(|(id, _)| id).collect())
}

/// Take a full TDLib snapshot before edits and reactions. The projection only
/// retains renderable text entities and reaction counts; it cannot reconstruct
/// arbitrary formatting or tell whose reaction would be removed by undo.
pub(super) fn before_send(store: &Store, request: &Value) -> Option<String> {
    let id = request["@extra"]["operation"].as_u64()?;
    let journal = store.local::<Journal>();
    let entries = journal.entries.lock().unwrap();
    for entry in entries.iter() {
        let mut change = entry.lock().unwrap();
        if change.initial["@extra"]["operation"] != id || change.prepared { continue; }
        if change.snapshot.as_ref().is_some_and(|(_, receipt)| receipt.lock().unwrap().status == Status::Pending) {
            return Some(String::new());
        }
        let mut snapshot = json!({"@type": "getMessage", "chat_id": request["chat_id"],
            "message_id": request["message_id"], "@extra": format!("undo_snapshot:{id}")});
        if matches!(change.undo, Undo::Chat) {
            snapshot["@type"] = json!("getChat");
            snapshot.as_object_mut().unwrap().remove("message_id");
        }
        if matches!(change.undo, Undo::Delete(_)) {
            snapshot["@type"] = json!("getMessages");
            snapshot.as_object_mut().unwrap().remove("message_id");
            snapshot["message_ids"] = request["message_ids"].clone();
            // A failed preparation needs a fresh delete gesture and snapshot,
            // never a retry of a deletion whose backup is incomplete.
            runtime::of(store).operations.forget_payload(id);
        }
        let rt = runtime::of(store);
        let tracked = rt.operations.track(&snapshot.to_string());
        let tracked_value: Value = serde_json::from_str(&tracked).unwrap();
        let snapshot_id = tracked_value["@extra"]["operation"].as_u64().unwrap();
        change.snapshot = Some((snapshot_id, rt.operations.watch(snapshot_id).unwrap()));
        return Some(tracked);
    }
    None
}

/// A snapshot is private to the command: it must not re-project stale message
/// bodies. Deletion needs a complete backup; a failed backup cancels deletion.
/// Other missing snapshots leave their actions recorded as irreversible.
pub(super) fn snapshot(w: &World, reply: &Value) -> Option<Option<String>> {
    let store = w.store();
    let context = reply["@extra"]["context"].as_str().or_else(|| reply["@extra"].as_str())?;
    let id: u64 = context.strip_prefix("undo_snapshot:")?.parse().ok()?;
    let journal = store.local::<Journal>();
    let entries = journal.entries.lock().unwrap().clone();
    for entry in entries.iter() {
        let mut change = entry.lock().unwrap();
        if change.initial["@extra"]["operation"] != id || change.prepared { continue; }
        if change.snapshot.as_ref().is_none_or(|(id, _)| reply["@extra"]["operation"] != *id) {
            return Some(None);
        }
        if matches!(change.undo, Undo::Delete(_)) {
            let status = change.snapshot.as_ref().unwrap().1.lock().unwrap().status.clone();
            if let Status::Failed { error, .. } = status {
                let rt = runtime::of(store);
                rt.operations.fail(store, id, &format!("{error}; nothing was deleted"), false);
                rt.operations.forget_payload(id);
                return Some(None);
            }
            drop(change);
            return Some(prepare_delete(w, entry, reply));
        }
        let initial = change.initial.clone();
        let inverse = if let Undo::Reaction(snapshot) = &mut change.undo {
            match snapshot.prepare(store, &initial, reply) {
                reaction::Preparation::Read(request) => {
                    let rt = runtime::of(store);
                    if !rt.operations.pending(id) { return Some(None); }
                    let tracked = rt.operations.track(&request);
                    let value: Value = serde_json::from_str(&tracked).unwrap();
                    let snapshot_id = value["@extra"]["operation"].as_u64().unwrap();
                    change.snapshot = Some((snapshot_id, rt.operations.watch(snapshot_id).unwrap()));
                    return Some(Some(tracked));
                }
                reaction::Preparation::Ready(inverse) => inverse,
            }
        } else { inverse(&change, reply) };
        change.prepared = true;
        change.undo = inverse.map_or(Undo::Impossible, Undo::Commands);
        return Some(runtime::of(store).operations.pending(id).then(|| change.initial.to_string()));
    }
    Some(None)
}

/// A confirmation that crosses a metadata update cannot establish absence.
pub(super) fn reactions_changed(store: &Store, chat: Option<i64>) {
    let journal = store.local::<Journal>();
    for entry in journal.entries.lock().unwrap().iter() {
        let mut change = entry.lock().unwrap();
        if chat.is_some_and(|chat| change.initial["chat_id"] != chat) { continue; }
        if let Undo::Reaction(snapshot) = &mut change.undo { snapshot.invalidate(); }
    }
}

fn prepare_delete(w: &World, entry: &Arc<Mutex<Change>>, reply: &Value) -> Option<String> {
    let store = w.store();
    let rt = runtime::of(store);
    let mut change = entry.lock().unwrap();
    let id = change.initial["@extra"]["operation"].as_u64().unwrap();
    if !rt.operations.pending(id) { return None; }
    let initial = change.initial.clone();
    let Undo::Delete(mut saved) = std::mem::replace(&mut change.undo, Undo::Delete(None)) else { unreachable!() };
    // Copying large attachments must not hold a lock the UI's undo path needs.
    drop(change);
    let result = (|| {
        if !store.is_writable() { return Err("another device holds the lease".into()); }
        if let Some(saved) = &mut saved { saved.downloaded(w, reply)?; }
        else { saved = Some(deletion::Saved::capture(&initial, reply)?); }
        saved.as_mut().unwrap().next_file(w)
    })();
    let mut change = entry.lock().unwrap();
    change.undo = Undo::Delete(saved);
    match result {
        Ok(file) if store.is_writable() && rt.operations.pending(id) => {
            if reply["@type"] == "file" {
                rt.operations.backed_up_file(reply["@extra"]["operation"].as_u64().unwrap());
            }
            if let Some(file) = file {
                let request = rt.operations.track(&requests::download_media(file, &format!("undo_snapshot:{id}")));
                let value: Value = serde_json::from_str(&request).unwrap();
                let snapshot_id = value["@extra"]["operation"].as_u64().unwrap();
                change.snapshot = Some((snapshot_id, rt.operations.watch(snapshot_id).unwrap()));
                return Some(request);
            }
            change.prepared = true;
            rt.operations.preparing_delete(id, false);
            Some(change.initial.to_string())
        }
        result => {
            let reason = result.err().unwrap_or_else(|| "The deletion is no longer available".into());
            let error = format!("{reason}; nothing was deleted");
            rt.operations.fail(store, id, &error, false);
            rt.operations.forget_payload(id);
            if reply["@type"] == "file" {
                rt.operations.fail(store, reply["@extra"]["operation"].as_u64().unwrap(), &error, false);
            }
            None
        }
    }
}

/// An attachment download has its own progress/timeout. Keep the deletion
/// pending while that backup runs; a failed backup must never delete anything.
pub(super) fn preparing(store: &Store) {
    let journal = store.local::<Journal>();
    let rt = runtime::of(store);
    for entry in journal.entries.lock().unwrap().iter() {
        let change = entry.lock().unwrap();
        if change.prepared || !matches!(change.undo, Undo::Delete(_)) { continue; }
        let id = change.initial["@extra"]["operation"].as_u64().unwrap();
        if let Some((_, snapshot)) = &change.snapshot {
            let status = snapshot.lock().unwrap().status.clone();
            match status {
                Status::Pending => rt.operations.preparing_delete(id, true),
                Status::Failed { error, .. } => {
                    rt.operations.fail(store, id, &format!("{error}; nothing was deleted"), false);
                    rt.operations.forget_payload(id);
                }
                Status::Done => {}
            }
        }
    }
}

fn inverse(change: &Change, message: &Value) -> Option<Vec<Value>> {
    let request = &change.initial;
    let mut base = request.clone();
    base.as_object_mut()?.remove("@extra");
    if matches!(change.undo, Undo::Chat) {
        if message["@type"] != "chat" || message["id"] != request["chat_id"] { return None; }
        match request["@type"].as_str()? {
            "setChatNotificationSettings" => {
                message["notification_settings"].as_object()?;
                base["notification_settings"] = message["notification_settings"].clone();
            }
            "toggleChatIsPinned" => {
                let position = message["positions"].as_array()?.iter()
                    .find(|p| p["list"] == request["chat_list"])?;
                base["is_pinned"] = json!(position["is_pinned"].as_bool()?);
            }
            "addChatToList" => {
                let position = message["positions"].as_array()?.iter().find(|p|
                    matches!(p["list"]["@type"].as_str(), Some("chatListMain" | "chatListArchive"))
                        && p["order"].as_str().is_none_or(|order| order != "0"))?;
                base["chat_list"] = position["list"].clone();
            }
            _ => return None,
        }
        return Some(vec![base]);
    }
    if message["@type"] != "message" || message["chat_id"] != request["chat_id"]
        || message["id"] != request["message_id"] { return None; }
    match change.undo {
        Undo::Edit => {
            let caption = request["@type"] == "editMessageCaption";
            let text = &message["content"][if caption { "caption" } else { "text" }];
            text["text"].as_str()?;
            if caption { base["caption"] = text.clone(); }
            else { base["input_message_content"]["text"] = text.clone(); }
            Some(vec![base])
        }
        _ => None,
    }
}

impl Change {
    fn targets(&self, store: &Store) -> Vec<(i64, i64)> {
        let mut sent = self.messages.clone();
        if let Some(flight) = &self.flight {
            sent.extend(&flight.receipt.lock().unwrap().messages);
        }
        let mut keys = targets(&self.initial, &sent);
        if let Undo::Delete(Some(saved)) = &self.undo {
            keys.extend(sent);
            for copy in &saved.copies { keys.extend(targets(&copy.request, &[])); }
        }
        let aliases = store.local::<Aliases>();
        let aliases = aliases.0.lock().unwrap();
        keys.into_iter().map(|key| resolve_key(&aliases, key)).collect()
    }

    fn pending(&self) -> bool {
        self.flight.as_ref().is_some_and(|flight| flight.receipt.lock().unwrap().status == Status::Pending)
    }

    fn needs_progress(&self) -> bool {
        self.flight.as_ref().map_or(self.failed.is_none() && self.desired != self.applied, |flight|
            match flight.receipt.lock().unwrap().status {
                Status::Pending => false,
                Status::Done => true,
                Status::Failed { .. } => self.failed.is_none(),
            })
    }

    fn unavailable(&self) -> Option<String> {
        self.failed.clone().or_else(|| self.flight.as_ref().and_then(|flight| {
            match &flight.receipt.lock().unwrap().status {
                Status::Failed { error, .. } => Some(error.clone()),
                _ => None,
            }
        })).or_else(|| matches!(self.undo, Undo::Impossible)
            .then(|| "Telegram cannot restore this action".into()))
    }

    fn advance(&mut self, store: &Store) {
        if let Some(flight) = &self.flight {
            let receipt = flight.receipt.lock().unwrap().clone();
            match receipt.status {
                Status::Pending => { self.failed = None; return; }
                Status::Failed { error, .. } => {
                    self.failed = Some(error);
                    return;
                }
                Status::Done => { self.failed = None; }
            }
            let mut flight = self.flight.take().unwrap();
            if flight.applied && matches!(self.undo, Undo::Send) {
                // Resending creates new Telegram ids. Later edits/reactions
                // on this branch must follow those identities when redone.
                let aliases = store.local::<Aliases>();
                let mut aliases = aliases.0.lock().unwrap();
                for (old, new) in self.messages.iter().zip(&receipt.messages) {
                    let old = resolve_key(&aliases, *old);
                    if old != *new { aliases.insert(old, *new); }
                }
                self.messages = receipt.messages.clone();
            }
            if !flight.applied {
                if let Undo::Delete(Some(saved)) = &self.undo {
                    let original = saved.copies[saved.copies.len() - flight.remaining.len() - 1].original;
                    if let [new] = receipt.messages.as_slice() {
                        let aliases = store.local::<Aliases>();
                        let mut aliases = aliases.0.lock().unwrap();
                        let old = resolve_key(&aliases, original);
                        if old != *new { aliases.insert(old, *new); }
                    } else {
                        self.failed = Some("Telegram did not confirm the restored message".into());
                        return;
                    }
                }
            }
            if let Some(next) = flight.remaining.pop_front() {
                self.start(store, next, flight.applied, flight.remaining);
                return;
            }
            self.applied = flight.applied;
        }
        if self.desired == self.applied || self.failed.is_some() { return; }
        let mut commands = if self.desired {
            let mut request = self.initial.clone();
            request.as_object_mut().unwrap().remove("@extra");
            // Redo sends a fresh message, but must not discard a newer draft.
            if request["input_message_content"].get("clear_draft").is_some() {
                request["input_message_content"]["clear_draft"] = json!(false);
            }
            VecDeque::from([request])
        } else {
            match &self.undo {
                Undo::Send if !self.messages.is_empty() => {
                    let chat = self.messages[0].0;
                    let ids: Vec<_> = self.messages.iter().map(|(_, id)| *id).collect();
                    VecDeque::from([serde_json::from_str(&requests::delete_messages(chat, &ids, true)).unwrap()])
                }
                Undo::Commands(commands) => commands.clone().into(),
                Undo::Delete(Some(saved)) => saved.copies.iter().map(|copy| copy.request.clone()).collect(),
                _ => {
                    let error = "Telegram cannot undo this action; its previous state is unavailable";
                    runtime::of(store).operations.report(store, &format!("undo {}", self.label), error);
                    self.failed = Some(error.into());
                    return;
                }
            }
        };
        if let Some(next) = commands.pop_front() { self.start(store, next, self.desired, commands); }
        else { self.applied = self.desired; }
    }

    fn start(&mut self, store: &Store, request: Value, applied: bool, remaining: VecDeque<Value>) {
        let rt = runtime::of(store);
        let tracked = rt.operations.track(&resolve(store, request).to_string());
        let value: Value = serde_json::from_str(&tracked).unwrap();
        let id = value["@extra"]["operation"].as_u64().unwrap();
        self.flight = Some(Flight { receipt: rt.operations.watch(id).unwrap(), applied, remaining });
        if !store.is_writable() {
            rt.operations.fail(store, id, "another device holds the lease; the history change was not sent", false);
        } else if !rt.send(&tracked) {
            rt.operations.fail(store, id, "Telegram is disconnected; the history change was not sent", false);
        }
    }
}

/// Called by the worker after replies, so an undo requested before delivery
/// waits for the final server id. Retain pending work even if history is pruned.
pub(super) fn pump(store: &Store) {
    let journal = store.local::<Journal>();
    let _pumping = journal.pumping.lock().unwrap();
    let mut entries = journal.entries.lock().unwrap().clone();
    entries.sort_by_key(|entry| entry.lock().unwrap().order);
    let mut active: Vec<_> = entries.iter().filter_map(|entry| {
        let change = entry.lock().unwrap();
        change.pending().then(|| (Arc::as_ptr(entry), change.targets(store)))
    }).collect();
    for entry in &entries {
        if !entry.lock().unwrap().needs_progress() { continue; }
        let keys = entry.lock().unwrap().targets(store);
        // Different history nodes can change the same message. Complete
        // their undo requests in the order of the user's history walk.
        let busy = active.iter().any(|(other, targets)| *other != Arc::as_ptr(entry)
            && targets.iter().any(|key| keys.contains(key)));
        if !busy {
            let mut change = entry.lock().unwrap();
            change.advance(store);
            if change.pending() { active.push((Arc::as_ptr(entry), change.targets(store))); }
        }
    }
    drop(entries);
    journal.entries.lock().unwrap().retain(|entry| {
        let change = entry.lock().unwrap();
        Arc::strong_count(entry) > 1 || (change.failed.is_none()
            && (change.flight.is_some() || change.desired != change.applied))
    });
}

struct Remote {
    label: String,
    changes: Vec<Arc<Mutex<Change>>>,
    independent_sends: bool,
}

impl Intent for Remote {
    fn describe(&self) -> String { self.label.clone() }

    fn blocked(&self, _w: &World) -> Option<String> { self.unavailable() }

    fn reverse(&self, w: &World) -> Result<(), String> { self.set(w, false) }

    fn reapply(&self, w: &World) -> Result<(), String> { self.set(w, true) }
}

impl Remote {
    fn unavailable(&self) -> Option<String> {
        self.changes.iter().find_map(|change| {
            let change = change.lock().unwrap();
            // An unconfirmed message consumes its own gesture's undo step,
            // whether it was sent alone or with other messages. Keep its
            // failed flight: redo cannot retry it, and a late confirmation
            // still follows undo without blocking delivered siblings.
            if self.independent_sends && !change.applied
                && change.flight.as_ref().is_some_and(|flight| flight.applied)
            {
                None
            } else {
                change.unavailable()
            }
        })
    }

    fn set(&self, w: &World, desired: bool) -> Result<(), String> {
        if !w.store().is_writable() { return Err("another device holds the lease".into()); }
        // Check the whole gesture before changing any member, including on
        // redo: a failed deletion must not let a sibling resend on its own.
        if let Some(error) = self.unavailable() { return Err(error); }
        let journal = w.store().local::<Journal>();
        for change in &self.changes {
            let mut change = change.lock().unwrap();
            change.desired = desired;
            change.order = journal.next.fetch_add(1, Ordering::Relaxed);
        }
        pump(w.store());
        self.unavailable().map_or(Ok(()), Err)
    }
}

fn targets(request: &Value, sent: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let Some(chat) = request["chat_id"].as_i64() else { return Vec::new(); };
    if matches!(request["@type"].as_str(), Some("sendMessage" | "forwardMessages")) {
        let mut keys = sent.to_vec();
        if let Some(reply) = request["reply_to"]["message_id"].as_i64() {
            keys.push((request["reply_to"]["chat_id"].as_i64().unwrap_or(chat), reply));
        }
        if let Some(from) = request["from_chat_id"].as_i64() {
            keys.extend(request["message_ids"].as_array().into_iter().flatten()
                .filter_map(|id| id.as_i64().map(|id| (from, id))));
        }
        return keys;
    }
    if let Some(ids) = request["message_ids"].as_array() {
        return ids.iter().filter_map(|id| id.as_i64().map(|id| (chat, id))).collect();
    }
    vec![(chat, request["message_id"].as_i64().unwrap_or(0))]
}

fn resolve_key(aliases: &HashMap<(i64, i64), (i64, i64)>, mut key: (i64, i64)) -> (i64, i64) {
    // The map only grows from old server ids to new ones; never back again.
    while let Some(next) = aliases.get(&key) { key = *next; }
    key
}

fn resolve(store: &Store, mut request: Value) -> Value {
    let Some(chat) = request["chat_id"].as_i64() else { return request; };
    let aliases = store.local::<Aliases>();
    let aliases = aliases.0.lock().unwrap();
    for field in ["message_id", "reply_to_message_id"] {
        if let Some(id) = request[field].as_i64() { request[field] = json!(resolve_key(&aliases, (chat, id)).1); }
    }
    if let Some(id) = request["reply_to"]["message_id"].as_i64() {
        let from = request["reply_to"]["chat_id"].as_i64().unwrap_or(chat);
        request["reply_to"]["message_id"] = json!(resolve_key(&aliases, (from, id)).1);
    }
    let from = request["from_chat_id"].as_i64().unwrap_or(chat);
    if let Some(ids) = request.get_mut("message_ids").and_then(Value::as_array_mut) {
        for id in ids {
            if let Some(value) = id.as_i64() { *id = json!(resolve_key(&aliases, (from, value)).1); }
        }
    }
    request
}
