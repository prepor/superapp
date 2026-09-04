//! What the files app lets an agent do, by name.
//!
//! Each one is the verb's own code path over a path instead of over a
//! cursor: the disk is written through [`ops`], the node claims the same
//! intent the button's does, and undo puts things back the same way. No
//! verb here is an `rm` — what a delete takes goes to the trash, and undo
//! moves it back out.
//!
//! The order is the verb's too. The disk is written first, because nothing
//! watches one and the first honest look is the write itself; then the
//! lease is asked again ([`Session::give_back`]) and the node recorded, so
//! a change with no node behind it is a change nobody can undo. Every
//! listing on screen re-reads afterwards, exactly as it does after a click.

use kernel::caps::WriteFile;
use kernel::effect::World;
use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::panel::PanelId;
use kernel::session::{Action, Session};
use kernel::time::fmt_date_long;
use kernel::tool::Tool;
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

/// How big a file `files.write` will write over. What was there is held on
/// the history node so undo can put it back, and a node is memory: past
/// this the honest answer is that the write could not be taken back.
const MAX_REWRITE: usize = 1024 * 1024;

/// The files app's tools: the two that read, then the six that write.
#[must_use]
pub fn all() -> Vec<Tool> {
    vec![
        Tool::new(
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
            false,
            list,
        ),
        Tool::new(
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
            false,
            read,
        ),
        Tool::new(
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
            true,
            rename,
        ),
        Tool::new(
            "files.move",
            "Move a file or a directory into another directory. It keeps its \
             name; a name the destination already has is refused rather than \
             written over.",
            into("the directory to move it into"),
            true,
            |s, input| here(s, input, Op::Move),
        ),
        Tool::new(
            "files.copy",
            "Copy a file, or a directory with everything under it, into \
             another directory. Copying into its own directory makes \
             “name copy.ext” beside it.",
            into("the directory to copy it into"),
            true,
            |s, input| here(s, input, Op::Copy),
        ),
        Tool::new(
            "files.trash",
            "Put a file or a directory in the trash. Nothing here is ever \
             removed outright, and cmd+z moves it back out.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
            true,
            trash,
        ),
        Tool::new(
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
            true,
            mkdir,
        ),
        Tool::new(
            "files.write",
            "Write text to a file, making it if it is not there and writing \
             over it if it is. What was there is kept so cmd+z puts it back; \
             a file over a megabyte is refused for that reason.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "text": {"type": "string", "description": "the whole of the file's new contents"}
                },
                "required": ["path", "text"],
                "additionalProperties": false
            }),
            true,
            write,
        ),
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
fn list(s: &mut Session, input: &Value) -> Result<Value, String> {
    let dir = text(input, "dir")?;
    let entries = list_in(s.world(), dir)?;
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
fn read(s: &mut Session, input: &Value) -> Result<Value, String> {
    let path = text(input, "path")?;
    let bytes = read_in(s.world(), path, MAX_TEXT + 1)?;
    let truncated = bytes.len() > MAX_TEXT;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_TEXT)]).into_owned();
    Ok(json!({"path": path, "text": text, "truncated": truncated}))
}

// -- the disk, written ------------------------------------------------------------------

/// `rename`: one path under a new name, in the directory it is already in —
/// the listing's own verb, over a path. Every panel on the old name follows
/// it, because a panel is on the thing and not on the spelling.
fn rename(s: &mut Session, input: &Value) -> Result<Value, String> {
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
    let world = s.world().clone();
    if stat_in(&world, &path).is_none() {
        return Err(format!("“{was}” is no longer there"));
    }
    if stat_in(&world, &to).is_some() {
        return Err(format!("“{name}” is already here"));
    }
    ready(s)?;
    ops::move_in(&world, &path, &to)?;
    // Read back the moment after the write: what undo will compare against
    // before it moves anything back.
    let intent: Box<dyn Intent> = Box::new(ops::Renamed::new(Done::of(&world, &path, &to)));
    let moves = renamings(s, &path, &to);
    node(
        s,
        Action::new("rename", format!("rename “{was}” to “{name}”"))
            .claiming(vec![intent])
            .moving(move |wm| {
                for (slot, id) in moves {
                    wm.replace(slot, id);
                }
            }),
    )?;
    Ok(json!({"path": to}))
}

/// `copy here` / `move here` for one path: the plan the clipboard's verb
/// makes, performed and claimed the same way. One path, so the plan has one
/// step or one refusal, and the refusal is the sentence.
fn here(s: &mut Session, input: &Value, op: Op) -> Result<Value, String> {
    let path = text(input, "path")?.to_string();
    let dir = text(input, "dir")?.to_string();
    let world = s.world().clone();
    if !super::model::is_dir_in(&world, &dir) {
        return Err(format!("“{dir}” is not a directory"));
    }
    let clip = Clipboard {
        verb: op,
        paths: vec![path.clone()],
    };
    let mut plan = ops::plan_here(&world, &clip, &dir);
    let Some(step) = plan.steps.pop() else {
        return Err(plan
            .refused
            .pop()
            .unwrap_or_else(|| format!("there is nothing to {} there", op.verb())));
    };
    ready(s)?;
    match op {
        Op::Copy => ops::copy_in(&world, &step.from, &step.to)?,
        Op::Move => ops::move_in(&world, &step.from, &step.to)?,
    }
    let done = vec![Done::of(&world, &step.from, &step.to)];
    let intent: Box<dyn Intent> = match op {
        Op::Copy => Box::new(ops::Copied::new(done)),
        Op::Move => Box::new(ops::Moved::new(done)),
    };
    let here = basename(&dir).to_string();
    let what = basename(&path).to_string();
    node(
        s,
        Action::new(op.verb(), format!("{} “{what}” into {here}", op.verb()))
            .claiming(vec![intent]),
    )?;
    Ok(json!({"path": step.to}))
}

/// `delete`: to the trash, and the panels that were showing it go with it.
fn trash(s: &mut Session, input: &Value) -> Result<Value, String> {
    let path = text(input, "path")?.to_string();
    if is_root(&path) {
        return Err(format!("“{path}” is a root"));
    }
    let world = s.world().clone();
    if stat_in(&world, &path).is_none() {
        return Err(format!("“{}” is no longer there", basename(&path)));
    }
    ready(s)?;
    let landed = ops::trash_in(&world, &path)?;
    let intent: Box<dyn Intent> =
        Box::new(ops::Deleted::new(vec![Done::of(&world, &path, &landed)]));
    let what = basename(&path).to_string();
    let closing = showing(s, &path);
    node(
        s,
        Action::new("delete", format!("delete “{what}”"))
            .claiming(vec![intent])
            .moving(move |wm| {
                for slot in closing {
                    wm.close(slot);
                }
            }),
    )?;
    Ok(json!({"trashed": landed}))
}

/// `new dir`: one directory, where nothing is yet.
fn mkdir(s: &mut Session, input: &Value) -> Result<Value, String> {
    let dir = text(input, "dir")?.to_string();
    let name = text(input, "name")?.trim().to_string();
    if name.is_empty() {
        return Err("a name is not nothing".to_string());
    }
    ops::check_name(&name)?;
    let path = join(&dir, &name);
    let world = s.world().clone();
    ready(s)?;
    ops::make_dir_in(&world, &path)?;
    let intent: Box<dyn Intent> = Box::new(ops::MadeDir::of(&world, &path));
    let here = basename(&dir).to_string();
    node(
        s,
        Action::new("new dir", format!("new dir “{name}/” in {here}")).claiming(vec![intent]),
    )?;
    Ok(json!({"path": path}))
}

/// A file written whole, with what was there kept so undo can put it back.
/// The one verb here no button makes yet: a card reads a file and does not
/// edit one.
fn write(s: &mut Session, input: &Value) -> Result<Value, String> {
    let path = text(input, "path")?.to_string();
    let body = text(input, "text")?.to_string();
    if is_root(&path) {
        return Err(format!("“{path}” is a root"));
    }
    let world = s.world().clone();
    let was = match stat_in(&world, &path) {
        None => None,
        Some(e) if e.is_dir => return Err(format!("“{path}” is a directory")),
        Some(e) if e.size as usize > MAX_REWRITE => {
            return Err(format!(
                "“{}” is too big to write over: what is there would have to be kept \
                 in memory for cmd+z",
                basename(&path)
            ))
        }
        Some(_) => Some(read_in(&world, &path, MAX_REWRITE)?),
    };
    ready(s)?;
    put(&world, &path, body.as_bytes())?;
    let intent: Box<dyn Intent> = Box::new(Wrote {
        path: path.clone(),
        was,
        wrote: body.into_bytes(),
    });
    let what = basename(&path).to_string();
    node(
        s,
        Action::new("write", format!("write “{what}”")).claiming(vec![intent]),
    )?;
    Ok(json!({"path": path}))
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
    if s.writable() {
        Ok(())
    } else {
        Err("read-only — another device holds the lease".to_string())
    }
}

/// The node, and the listings after it. The lease is asked again, because
/// it may have turned over between the disk write and here, and then the
/// claim is given back rather than recorded.
fn node(s: &mut Session, action: Action<()>) -> Result<(), String> {
    if let Some(intent) = action.intents.first() {
        if let Some(why) = s.give_back(intent.as_ref()) {
            panels::refresh(s, None);
            return Err(why);
        }
    }
    s.act_done(action);
    panels::refresh(s, None);
    Ok(())
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
