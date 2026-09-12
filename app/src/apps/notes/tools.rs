//! Notes and file drafts as agent tools. All writes stay in the store.

use super::{file_text, model, ops, panels::Editor};
use kernel::caps::{display_path, real_path};
use kernel::effect::World;
use kernel::session::Edit;
use kernel::tool::{Prepare, Prepared, Tool};
use serde_json::{json, Value};

const PAGE_BYTES: usize = 64 * 1024;

pub fn all() -> Vec<Tool> {
    let body = json!({"type": "string", "description": "Complete Markdown source, at most 2 MiB; never send only a read page."});
    let id = json!({"type": "integer", "description": "An active notes_note ID, discoverable with sql.query."});
    let path = json!({"type": "string", "description": "Existing UTF-8 text file, absolute path or ~/path."});
    let revision = json!({"type": "string", "description": "Revision returned by the most recent read or write of this same note/path."});
    let offset = json!({"type": "integer", "description": "UTF-8 byte offset, initially 0; use next_offset to read subsequent pages. All pages must have the same revision."});
    vec![
        Tool::preparing("notes.create",
            "Create an autosaved note independent of files. Its first nonempty line supplies the title. Returns its ID, revision and editor panel. Undoable with cmd+z.",
            schema(json!({"body": body}), &["body"]), |i| preparing(i, create)),
        Tool::reading("notes.read",
            "Read an active note's Markdown source and revision, up to 64 KiB per page. Follow next_offset until null before replacing the whole body with notes.update.",
            schema(json!({"id": id, "offset": offset}), &["id"]), |input| reading(input, read)),
        Tool::preparing("notes.update",
            "Replace a note's complete Markdown source using its latest revision; refuses stale edits. Updates its derived title and timestamp, refreshes open editors, and is one undo step.",
            schema(json!({"id": id, "body": body, "revision": revision}), &["id", "body", "revision"]), |i| preparing(i, update)),
        Tool::reading("notes.read_draft",
            "Read a file's current draft, or the file if no draft exists, without creating a draft. Returns LF text without a BOM, exists, and a revision for create_draft/update_draft. Pages are at most 64 KiB; read all pages at the same revision before replacing text.",
            schema(json!({"path": path, "offset": offset}), &["path"]), |input| reading(input, read_draft)),
        Tool::preparing("notes.create_draft",
            "Create a database draft for an existing text file using the revision from read_draft (exists=false). Refuses if the file changed or a draft appeared. Preserves the original, BOM and line endings. Undoable; only explicit Save in the editor writes to disk.",
            schema(json!({"path": path, "body": body, "revision": revision}), &["path", "body", "revision"]), |i| preparing(i, |w, i| write_draft(w, i, true))),
        Tool::preparing("notes.update_draft",
            "Replace an existing draft's complete text using its latest revision. Keeps the original for Save conflict checks. Returning to the original removes the draft. One undo step; never writes to disk.",
            schema(json!({"path": path, "body": body, "revision": revision}), &["path", "body", "revision"]), |i| preparing(i, |w, i| write_draft(w, i, false))),
    ]
}

fn preparing(input: &Value, prepare: fn(&World, &Value) -> Result<Prepared, String>) -> Prepare {
    let input = input.clone();
    Box::new(move |world| Box::pin(async move {
        if let Some(factory) = world.factory() {
            kernel::runtime::spawn_blocking(move || {
                let world = factory.build().map_err(|error| error.to_string())?;
                prepare(&world, &input)
            }).await.map_err(|error| error.to_string())?
        } else {
            prepare(world, &input)
        }
    }))
}

fn reading(input: &Value, read: fn(&World, &Value) -> Result<Value, String>) -> kernel::tool::Read {
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

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn text<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be text"))
}
fn id(input: &Value) -> Result<i64, String> {
    input
        .get("id")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or_else(|| "id must be a positive note ID".into())
}
fn path(input: &Value) -> Result<String, String> {
    let path = text(input, "path")?;
    if !(path.starts_with('/') || path.starts_with("~/")) || path.contains('\0') {
        return Err("path must be absolute or start with ~/".into());
    }
    // The same key as Editor::file, so absolute and ~/ spellings meet.
    Ok(display_path(&real_path(path)))
}
fn bounded(body: &str) -> Result<(), String> {
    if body.len() > model::MAX_FILE_BYTES {
        return Err("note tools edit text up to 2 MiB (including the text kept for undo)".into());
    }
    Ok(())
}
fn body(input: &Value) -> Result<String, String> {
    let body = text(input, "body")?;
    bounded(body)?;
    if body
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("body must be text without binary control characters".into());
    }
    Ok(body.replace("\r\n", "\n"))
}
fn check_revision(input: &Value, current: &str) -> Result<(), String> {
    if text(input, "revision")? != current {
        return Err(ops::CHANGED.into());
    }
    Ok(())
}

fn note(s: &World, id: i64) -> Result<model::NoteText, String> {
    model::note_text(s.store().conn(), id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "note does not exist or is deleted".into())
}
fn note_reply(id: i64, note: &model::NoteText) -> Value {
    json!({"id": id, "title": note.title, "modified": note.modified,
        "revision": ops::revision(&(id, note)),
        "panel": {"tag": "editor", "args": Editor::note(id).args}})
}
fn create(world: &World, input: &Value) -> Result<Prepared, String> {
    let after = model::NoteText::new(body(input)?, world.now());
    Ok(Prepared::Edit(Edit::writing("notes.new", "new note", move |tx| {
        tx.execute(
            "INSERT INTO notes_note(title,body,created,modified) VALUES(?1,?2,?3,?3)",
            rusqlite::params![after.title, after.body, after.modified],
        )?;
        Ok(note_reply(tx.last_insert_rowid(), &after))
    }).claiming_with(|reply| vec![model::creation(reply["id"].as_i64().expect("created note ID"))])))
}
fn read(s: &World, input: &Value) -> Result<Value, String> {
    let id = id(input)?;
    let note = note(s, id)?;
    page(note_reply(id, &note), &note.body, input)
}
fn update(s: &World, input: &Value) -> Result<Prepared, String> {
    let id = id(input)?;
    let body = body(input)?;
    let before = note(s, id)?;
    check_revision(input, &ops::revision(&(id, &before)))?;
    bounded(&before.body)?;
    if body == before.body {
        return Ok(Prepared::Reply(note_reply(id, &before)));
    }
    let after = model::NoteText::new(body, s.now());
    let reply = note_reply(id, &after);
    Ok(ops::Edit::Note {
        id,
        before,
        after,
    }
    .prepared(reply))
}

fn draft(s: &World, path: &str) -> Result<Option<model::StoredDraft>, String> {
    model::stored_draft(s.store().conn(), path).map_err(|e| e.to_string())
}
fn original(s: &World, path: &str, draft: Option<&model::StoredDraft>) -> Result<String, String> {
    match draft {
        Some(draft) => Ok(draft.text.original.clone()),
        None => s.run(&model::ReadFile(path.into())),
    }
}
fn draft_reply(path: &str, original: &str, draft: Option<&model::StoredDraft>) -> Value {
    json!({"path": path, "exists": draft.is_some(), "modified": draft.map(|d| d.modified),
        "revision": ops::revision(&(path, original, draft)),
        "panel": {"tag": "editor", "args": Editor::file(path).args}})
}
fn read_draft(s: &World, input: &Value) -> Result<Value, String> {
    let path = path(input)?;
    let draft = draft(s, &path)?;
    let original = original(s, &path, draft.as_ref())?;
    let body = file_text::editable(draft.as_ref().map_or(&original, |d| &d.text.body));
    page(draft_reply(&path, &original, draft.as_ref()), &body, input)
}
fn write_draft(s: &World, input: &Value, create: bool) -> Result<Prepared, String> {
    let path = path(input)?;
    let body = file_text::editable(&body(input)?);
    let before = draft(s, &path)?;
    if create && before.is_some() {
        return Err("a draft already exists; read it and use notes.update_draft".into());
    }
    if !create && before.is_none() {
        return Err("no draft exists; use notes.read_draft before notes.create_draft".into());
    }
    let original = original(s, &path, before.as_ref())?;
    check_revision(input, &ops::revision(&(&path, &original, &before)))?;
    bounded(&original)?;
    if let Some(before) = &before {
        bounded(&before.text.body)?;
    }
    let body = file_text::encode(&original, &body);
    bounded(&body)?;
    if body == before.as_ref().map_or(&original, |d| &d.text.body).as_str() {
        return Ok(Prepared::Reply(draft_reply(&path, &original, before.as_ref())));
    }
    let after = (body != original).then(|| model::StoredDraft {
        text: model::Draft {
            original: original.clone(),
            body,
        },
        modified: s.now(),
    });
    let reply = draft_reply(&path, &original, after.as_ref());
    Ok(ops::Edit::Draft {
        path,
        before,
        after,
    }
    .prepared(reply))
}

fn page(mut reply: Value, body: &str, input: &Value) -> Result<Value, String> {
    let offset = match input.get("offset") {
        None => 0,
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("offset must be a nonnegative byte offset")?,
    };
    if offset > body.len() || !body.is_char_boundary(offset) {
        return Err("offset must be a UTF-8 boundary within the body; use next_offset".into());
    }
    let mut end = offset.saturating_add(PAGE_BYTES).min(body.len());
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    reply["body"] = json!(&body[offset..end]);
    reply["offset"] = json!(offset);
    reply["total_bytes"] = json!(body.len());
    reply["next_offset"] = json!((end < body.len()).then_some(end));
    Ok(reply)
}
