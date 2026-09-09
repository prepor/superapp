//! What the files app lets an agent do, by name.
//!
//! Each one is the verb's own code path over a path instead of over a
//! cursor: the disk is written through [`ops`], the node claims the same
//! intent the button's does, and undo puts things back the same way. No
//! verb here is an `rm` — what a delete takes goes to the trash, and undo
//! moves it back out.
//!
//! Preparation owns the request but does not mutate the disk. Acceptance
//! starts a native command on the shared blocking pool; the session owns its
//! completion, undo record and lease-loss compensation even if the agent
//! stops meanwhile. Native writes and SQLite tool bookkeeping are separate
//! commits, so an accepted native operation is never automatically replayed.

use kernel::caps::WriteFile;
use kernel::effect::World;
use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::panel::PanelId;
use kernel::session::{Action, Session};
use kernel::time::fmt_date_long;
use kernel::tool::{Command, CommandComplete, Prepare, Prepared, Read, Tool};
use serde_json::{json, Value};

use super::model::{basename, is_root, join, list_in, parent, read_in, real_path, stat_in};
use super::ops::{self, Done};
use super::panels::{self, Card, Dir};
use super::{Clipboard, Op, FILES};

/// How much of a file one call reads back — the same ceiling the kernel's
/// `sql.query` keeps.
const MAX_TEXT: usize = 64 * 1024;

/// How many entries one listing answers. A directory with more than this in
/// it is a question for `files.list` on a subdirectory, or for the panel.
const MAX_ENTRIES: usize = 500;

/// How big a file `files.write` will write over, and how long a text it
/// will put there. Both are held on the history node so undo can put the
/// file back and can tell that it is still the one this wrote, and a node
/// is memory: past this the honest answer is that the write could not be
/// taken back.
const MAX_REWRITE: usize = 1024 * 1024;

/// The files app's tools: the two that read, then the six that write.
#[must_use]
pub fn all() -> Vec<Tool> {
    vec![
        Tool::reading(
            "files.list",
            "List a directory: what is in it, which entries are directories, \
             how big each one is and when it last changed. Paths are written \
             the way the person sees them, starting at `~`.",
            json!({
                "type": "object",
                "properties": {"dir": {"type": "string", "description": "the directory, e.g. `~/Downloads`"}},
                "required": ["dir"],
                "additionalProperties": false
            }),
            |input| background_read(input, list),
        ),
        Tool::reading(
            "files.read",
            "Read a file as text. The first 64 KiB come back; a picture or an \
             archive comes back as whatever its bytes look like, so ask only \
             for files that are text.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
            |input| background_read(input, read),
        ),
        Tool::preparing(
            "files.rename",
            "Give a file or a directory another name, where it already is. \
             The new name is a name, not a path — use files.move to put \
             something somewhere else. Undoable with cmd+z.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "name": {"type": "string", "description": "the new name, one segment"}
                },
                "required": ["path", "name"],
                "additionalProperties": false
            }),
            |input| prepare_command(input, Operation::Rename),
        ),
        Tool::preparing(
            "files.move",
            "Move a file or a directory into another directory. It keeps its \
             name; a name the destination already has is refused rather than \
             written over.",
            into("the directory to move it into"),
            |input| prepare_command(input, Operation::Move),
        ),
        Tool::preparing(
            "files.copy",
            "Copy a file, or a directory with everything under it, into \
             another directory. Copying into its own directory makes \
             “name copy.ext” beside it.",
            into("the directory to copy it into"),
            |input| prepare_command(input, Operation::Copy),
        ),
        Tool::preparing(
            "files.trash",
            "Put a file or a directory in the trash. Nothing here is ever \
             removed outright, and cmd+z moves it back out.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
            |input| prepare_command(input, Operation::Trash),
        )
        // What a person means by *delete*: the file leaves the listing, and
        // undo is the only way back. That is a thing to be asked about.
        .asking(),
        Tool::preparing(
            "files.mkdir",
            "Make one directory, where nothing is yet. A name the parent \
             already has is refused.",
            json!({
                "type": "object",
                "properties": {
                    "dir": {"type": "string", "description": "the directory to make it in"},
                    "name": {"type": "string", "description": "the new directory's name, one segment"}
                },
                "required": ["dir", "name"],
                "additionalProperties": false
            }),
            |input| prepare_command(input, Operation::Mkdir),
        ),
        Tool::preparing(
            "files.write",
            "Write text to a file, making it if it is not there and writing \
             over it if it is. What was there is kept so cmd+z puts it back, \
             and so is what this writes; a file over a megabyte, or a text \
             over a megabyte, is refused for that reason.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "text": {"type": "string", "description": "the whole of the file's new contents"}
                },
                "required": ["path", "text"],
                "additionalProperties": false
            }),
            |input| prepare_command(input, Operation::Write),
        )
        // Writing over a file is the one verb here that destroys what was
        // there; what undo holds for it is memory, and the person's word
        // comes first.
        .asking(),
    ]
}

/// The schema of the two verbs that take a path and a destination.
fn into(what: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "dir": {"type": "string", "description": what}
        },
        "required": ["path", "dir"],
        "additionalProperties": false
    })
}

// -- reading -------------------------------------------------------------------------

/// One directory, through the world's disk — the same read a listing does.
fn list(world: &World, input: &Value) -> Result<Value, String> {
    let dir = text(input, "dir")?;
    let entries = list_in(world, dir)?;
    let truncated = entries.len() > MAX_ENTRIES;
    let rows: Vec<Value> = entries
        .iter()
        .take(MAX_ENTRIES)
        .map(|e| {
            json!({
                "name": e.name,
                "is_dir": e.is_dir,
                "size": e.size,
                "modified": fmt_date_long(e.modified),
            })
        })
        .collect();
    Ok(json!({"dir": dir, "entries": rows, "truncated": truncated}))
}

/// One file as text. The bytes are read as they are and turned into a
/// string as far as they go: a model asking for a picture should learn that
/// it asked for a picture, not get an error that says nothing.
fn read(world: &World, input: &Value) -> Result<Value, String> {
    let path = text(input, "path")?;
    let bytes = read_in(world, path, MAX_TEXT + 1)?;
    let truncated = bytes.len() > MAX_TEXT;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_TEXT)]).into_owned();
    Ok(json!({"path": path, "text": text, "truncated": truncated}))
}

fn background_read(input: &Value, read: fn(&World, &Value) -> Result<Value, String>) -> Read {
    let input = input.clone();
    Box::new(move |world| Box::pin(async move {
        if let Some(factory) = world.factory() {
            kernel::runtime::spawn_blocking(move || {
                let world = factory.build().map_err(|error| error.to_string())?;
                read(&world, &input)
            }).await.map_err(|error| error.to_string())?
        } else { read(world, &input) }
    }))
}

#[derive(Clone, Copy)]
enum Operation { Rename, Move, Copy, Trash, Mkdir, Write }

struct NativeCommand { operation: Operation, input: Value }

fn prepare_command(input: &Value, operation: Operation) -> Prepare {
    let input = input.clone();
    Box::new(move |_| Box::pin(async move {
        // Preparing carries intent only. Native mutations start after the
        // caller accepts this command and rechecks cancellation.
        Ok(Prepared::Command(Box::new(NativeCommand { operation, input })))
    }))
}

enum LayoutChange { None, Rename { from: String, to: String }, Trash(String) }

struct Applied {
    kind: &'static str,
    label: String,
    intent: Box<dyn Intent>,
    layout: LayoutChange,
    reply: Value,
}

impl NativeCommand {
    fn apply(self, world: &World) -> Result<Applied, String> {
        ready_world(world)?;
        match self.operation {
            Operation::Rename => rename(world, &self.input),
            Operation::Move => here(world, &self.input, Op::Move),
            Operation::Copy => here(world, &self.input, Op::Copy),
            Operation::Trash => trash(world, &self.input),
            Operation::Mkdir => mkdir(world, &self.input),
            Operation::Write => write(world, &self.input),
        }
    }
}

impl Command for NativeCommand {
    fn commit(self: Box<Self>, s: &mut Session, complete: CommandComplete) {
        if let Err(error) = ready(s) { complete(s, Err(error)); return; }
        s.prepare_work(move |world| Box::pin(async move {
            if let Some(factory) = world.factory() {
                kernel::runtime::spawn_blocking(move || {
                    let world = factory.build().map_err(|error| error.to_string())?;
                    self.apply(&world)
                }).await.map_err(|error| error.to_string())?
            } else { self.apply(world) }
        }), move |s, result| {
            match result {
                Ok(applied) => s.after_history(move |s| applied.finish(s, complete)),
                Err(error) => complete(s, Err(error)),
            }
        });
    }
}

impl Applied {
    fn finish(self, s: &mut Session, complete: CommandComplete) {
        let Applied { kind, label, intent, layout, reply } = self;
        super::run::accept(s, intent, move |s, accepted| {
            match accepted {
                Ok(intent) => {
                    let reply = Applied { kind, label, intent, layout, reply }.record(s);
                    complete(s, Ok(reply));
                }
                Err(error) => { panels::refresh(s, None); complete(s, Err(error)); }
            }
        });
    }

    fn record(self, s: &mut Session) -> Value {
        let mut action = Action::new(self.kind, self.label).claiming(vec![self.intent]);
        match self.layout {
            LayoutChange::None => {},
            LayoutChange::Rename { from, to } => {
                let moves = renamings(s, &from, &to);
                action = action.moving(move |wm| { for (slot, id) in moves { wm.replace(slot, id); } });
            }
            LayoutChange::Trash(path) => {
                let closing = showing(s, &path);
                action = action.moving(move |wm| { for slot in closing { wm.close(slot); } });
            }
        }
        s.act_done(action);
        panels::refresh(s, None);
        self.reply
    }
}

fn ready_world(world: &World) -> Result<(), String> {
    if world.store().is_writable() { Ok(()) }
    else { Err("read-only — another device holds the lease".into()) }
}

// -- the disk, written ------------------------------------------------------------------

/// `rename`: one path under a new name, in the directory it is already in —
/// the listing's own verb, over a path. Every panel on the old name follows
/// it, because a panel is on the thing and not on the spelling.
fn rename(world: &World, input: &Value) -> Result<Applied, String> {
    let path = text(input, "path")?.to_string();
    let name = text(input, "name")?.trim().to_string();
    let was = basename(&path).to_string();
    if name.is_empty() {
        return Err("a name is not nothing".to_string());
    }
    if name == was {
        return Err(format!("“{was}” is its name already"));
    }
    if is_root(&path) {
        return Err(format!("“{path}” is a root"));
    }
    ops::check_name(&name)?;
    let dir = parent(&path).ok_or_else(|| format!("“{path}” is a root"))?;
    let to = join(dir, &name);
    if stat_in(world, &path).is_none() {
        return Err(format!("“{was}” is no longer there"));
    }
    if stat_in(world, &to).is_some() {
        return Err(format!("“{name}” is already here"));
    }
    ready_world(world)?;
    ops::move_in(world, &path, &to)?;
    // Read back the moment after the write: what undo will compare against
    // before it moves anything back.
    let intent: Box<dyn Intent> = Box::new(ops::Renamed::new(Done::of(world, &path, &to)));
    Ok(Applied { kind: "rename", label: format!("rename “{was}” to “{name}”"), intent,
        layout: LayoutChange::Rename { from: path, to: to.clone() }, reply: json!({"path": to}) })
}

/// `copy here` / `move here` for one path: the plan the clipboard's verb
/// makes, performed and claimed the same way. One path, so the plan has one
/// step or one refusal, and the refusal is the sentence.
fn here(world: &World, input: &Value, op: Op) -> Result<Applied, String> {
    let path = text(input, "path")?.to_string();
    let dir = text(input, "dir")?.to_string();
    if !super::model::is_dir_in(world, &dir) {
        return Err(format!("“{dir}” is not a directory"));
    }
    let clip = Clipboard {
        verb: op,
        paths: vec![path.clone()],
    };
    let mut plan = ops::plan_here(world, &clip, &dir);
    let Some(step) = plan.steps.pop() else {
        return Err(plan
            .refused
            .pop()
            .unwrap_or_else(|| format!("there is nothing to {} there", op.verb())));
    };
    ready_world(world)?;
    match op {
        Op::Copy => ops::copy_in(world, &step.from, &step.to)?,
        Op::Move => ops::move_in(world, &step.from, &step.to)?,
    }
    let done = vec![Done::of(world, &step.from, &step.to)];
    let intent: Box<dyn Intent> = match op {
        Op::Copy => Box::new(ops::Copied::new(done)),
        Op::Move => Box::new(ops::Moved::new(done)),
    };
    let here = basename(&dir).to_string();
    let what = basename(&path).to_string();
    Ok(Applied { kind: op.verb(), label: format!("{} “{what}” into {here}", op.verb()), intent,
        layout: LayoutChange::None, reply: json!({"path": step.to}) })
}

/// `delete`: to the trash, and the panels that were showing it go with it.
fn trash(world: &World, input: &Value) -> Result<Applied, String> {
    let path = text(input, "path")?.to_string();
    if is_root(&path) {
        return Err(format!("“{path}” is a root"));
    }
    if stat_in(world, &path).is_none() {
        return Err(format!("“{}” is no longer there", basename(&path)));
    }
    ready_world(world)?;
    let landed = ops::trash_in(world, &path)?;
    let intent: Box<dyn Intent> =
        Box::new(ops::Deleted::new(vec![Done::of(world, &path, &landed)]));
    let what = basename(&path).to_string();
    Ok(Applied { kind: "delete", label: format!("delete “{what}”"), intent,
        layout: LayoutChange::Trash(path), reply: json!({"trashed": landed}) })
}

/// `new dir`: one directory, where nothing is yet.
fn mkdir(world: &World, input: &Value) -> Result<Applied, String> {
    let dir = text(input, "dir")?.to_string();
    let name = text(input, "name")?.trim().to_string();
    if name.is_empty() {
        return Err("a name is not nothing".to_string());
    }
    ops::check_name(&name)?;
    let path = join(&dir, &name);
    ready_world(world)?;
    ops::make_dir_in(world, &path)?;
    // What the disk has at the path the moment after the write — the one
    // reading that is certainly about the directory this made.
    let made = Done::of(world, &path, &path);
    let intent: Box<dyn Intent> = Box::new(ops::MadeDir::made(&made));
    let here = basename(&dir).to_string();
    Ok(Applied { kind: "new dir", label: format!("new dir “{name}/” in {here}"), intent,
        layout: LayoutChange::None, reply: json!({"path": path}) })
}

/// A file written whole, with what was there kept so undo can put it back.
/// The one verb here no button makes yet: a card reads a file and does not
/// edit one.
///
/// Both sides of the write are held to [`MAX_REWRITE`], and for the one
/// reason: the node keeps what was there *and* what this put there, and
/// [`Wrote::blocked`] compares the second against the disk by reading back
/// at most that much. A longer text would be a claim that could never
/// match itself — undo would refuse the file it had just written, saying it
/// had changed since.
fn write(world: &World, input: &Value) -> Result<Applied, String> {
    let path = text(input, "path")?.to_string();
    let body = text(input, "text")?.to_string();
    if is_root(&path) {
        return Err(format!("“{path}” is a root"));
    }
    if body.len() > MAX_REWRITE {
        return Err(format!(
            "“{}” is too much to write: what this would put there would have to be \
             kept in memory for cmd+z",
            basename(&path)
        ));
    }
    let was = match stat_in(world, &path) {
        None => None,
        Some(e) if e.is_dir => return Err(format!("“{path}” is a directory")),
        Some(e) if e.size as usize > MAX_REWRITE => {
            return Err(format!(
                "“{}” is too big to write over: what is there would have to be kept \
                 in memory for cmd+z",
                basename(&path)
            ))
        }
        Some(_) => Some(read_in(world, &path, MAX_REWRITE)?),
    };
    ready_world(world)?;
    put(world, &path, body.as_bytes())?;
    let intent: Box<dyn Intent> = Box::new(Wrote {
        path: path.clone(),
        was,
        wrote: body.into_bytes(),
    });
    let what = basename(&path).to_string();
    Ok(Applied { kind: "write", label: format!("write “{what}”"), intent,
        layout: LayoutChange::None, reply: json!({"path": path}) })
}

/// What `files.write` claimed of the disk: a file's contents, and what was
/// there before it. Compared by the bytes rather than by the object's id,
/// because writing over a file leaves the same object wearing new contents
/// — which is exactly the thing undo has to be sure of.
struct Wrote {
    path: String,
    /// What was at the path before, or `None` where nothing was.
    was: Option<Vec<u8>>,
    /// What this write put there.
    wrote: Vec<u8>,
}

impl Intent for Wrote {
    fn describe(&self) -> String {
        format!("wrote “{}”", basename(&self.path))
    }

    /// Undo may decline; it may not guess. A file whose contents are no
    /// longer what this write left is somebody else's now.
    fn blocked(&self, w: &World) -> Option<String> {
        let name = basename(&self.path);
        match read_in(w, &self.path, MAX_REWRITE) {
            Ok(now) if now == self.wrote => None,
            Ok(_) => Some(format!("“{name}” has changed since")),
            Err(_) => Some(format!("“{name}” is no longer there")),
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        match &self.was {
            Some(bytes) => put(w, &self.path, bytes),
            // Nothing was there, so putting it back is taking it away — and
            // taking away here means the trash, as it does everywhere else.
            None => ops::trash_in(w, &self.path).map(|_| ()),
        }
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        put(w, &self.path, &self.wrote)
    }
}

/// The disk write itself, with the app's own counter bumped so every open
/// listing re-reads — what [`ops`] does for the verbs it owns.
fn put(w: &World, path: &str, bytes: &[u8]) -> Result<(), String> {
    let r = w.run(&WriteFile {
        path: &real_path(path),
        bytes,
    });
    FILES.touched();
    r
}

// -- what every writing tool does around its verb -------------------------------------

/// The write gate, asked before any disk is: a change nobody can undo is
/// not a change this app makes.
fn ready(s: &Session) -> Result<(), String> {
    if s.writable() && s.store().is_writable() {
        Ok(())
    } else {
        Err("read-only — another device holds the lease".to_string())
    }
}

/// Every slot showing this path, as a listing or as a card — what a delete
/// closes.
fn showing(s: &Session, path: &str) -> Vec<SlotId> {
    let mut out = s.showing(&Dir::id(path));
    out.extend(s.showing(&Card::id(path)));
    out
}

/// The same slots, pointed at the new name: a panel is on the thing, not on
/// the spelling.
fn renamings(s: &Session, from: &str, to: &str) -> Vec<(SlotId, PanelId)> {
    let mut out: Vec<(SlotId, PanelId)> = s
        .showing(&Dir::id(from))
        .into_iter()
        .map(|slot| (slot, Dir::id(to)))
        .collect();
    out.extend(
        s.showing(&Card::id(from))
            .into_iter()
            .map(|slot| (slot, Card::id(to))),
    );
    out
}

/// A string argument. Paths are written the way the panels write them —
/// `~/Downloads/report.pdf` — and [`real_path`] is what turns one into a
/// path on the machine, here as everywhere else in this app.
fn text<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

#[cfg(test)]
mod native_tests;
