use kernel::caps::{real_path, Disk};
use kernel::effect::{Ctx, Effect, World};
use kernel::history::Intent;
use kernel::richtable::{Dir, SqlSource, SqlSpec};
use kernel::session::{Action, Session};
use kernel::store::{Store, Val, Q};
use rusqlite::params;
use std::rc::Rc;

pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: i64,
    pub title: String,
    pub modified: f64,
}

pub static NOTES: SqlSource<Note, i64> = SqlSource {
    spec: &SqlSpec {
        id: "notes",
        describe: "notes, most recently edited first",
        select: "id,title,modified",
        from: "notes_note",
        base: "deleted=0",
        text: &["title", "body"],
        index: None,
        tags: &[],
        order: &[("modified", Dir::Desc), ("id", Dir::Desc)],
        group: None,
        key: "id",
        deps: &[],
    },
    tags: &[],
    map: |r| {
        Ok(Note {
            id: r.get(0)?,
            title: r.get(1)?,
            modified: r.get(2)?,
        })
    },
    key: |r| r.id,
    rank: |r| vec![Val::F(r.modified), Val::I(r.id)],
    suggest: |_, _, _| Vec::new(),
};

static BODY: Q = Q {
    id: "note text",
    describe: "the editable Markdown source of this note",
    sql: "SELECT body FROM notes_note WHERE id=? AND deleted=0",
};
pub fn body(store: &Store, id: i64) -> Option<String> {
    note_source(store, id).first().cloned()
}
pub fn note_source(store: &Store, id: i64) -> Rc<Vec<String>> {
    store.rows(&BODY, &[Val::I(id)], |r| r.get::<_, String>(0))
}

pub fn title(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let title = line.trim_start_matches('#').trim();
    if title.is_empty() {
        "Untitled".into()
    } else {
        title.chars().take(100).collect()
    }
}

pub fn create(s: &mut Session) -> Option<i64> {
    let now = s.now();
    let id = s.act(Action::writing("notes.new", "new note", move |c| {
        c.execute(
            "INSERT INTO notes_note(created,modified) VALUES(?1,?1)",
            [now],
        )?;
        Ok(c.last_insert_rowid())
    }))?;
    s.claim(Box::new(Deleted {
        ids: vec![id],
        before: true,
        after: false,
    }));
    Some(id)
}

pub fn edit(store: &Store, id: i64, body: String, now: f64) -> Result<(), String> {
    store
        .write(move |c| {
            let changed = c.execute(
                "UPDATE notes_note SET title=?1,body=?2,modified=?3 WHERE id=?4 AND deleted=0",
                params![title(&body), body, now, id],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
}

pub fn delete(s: &mut Session, ids: Vec<i64>) -> bool {
    if ids.is_empty() {
        return false;
    }
    let write_ids = ids.clone();
    s.act(
        Action::writing(
            "notes.delete",
            format!("delete {} notes", ids.len()),
            move |c| {
                for id in write_ids {
                    c.execute("UPDATE notes_note SET deleted=1 WHERE id=?", [id])?;
                }
                Ok(())
            },
        )
        .claiming(vec![Box::new(Deleted {
            ids,
            before: false,
            after: true,
        })]),
    )
    .is_some()
}

struct Deleted {
    ids: Vec<i64>,
    before: bool,
    after: bool,
}
impl Deleted {
    fn set(&self, world: &World, value: bool) -> Result<(), String> {
        let ids = self.ids.clone();
        world
            .store()
            .write(move |c| {
                for id in ids {
                    c.execute(
                        "UPDATE notes_note SET deleted=?1 WHERE id=?2",
                        params![value, id],
                    )?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}
impl Intent for Deleted {
    fn describe(&self) -> String {
        "note visibility".into()
    }
    fn reverse(&self, world: &World) -> Result<(), String> {
        self.set(world, self.before)
    }
    fn reapply(&self, world: &World) -> Result<(), String> {
        self.set(world, self.after)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub original: String,
    pub body: String,
}
static DRAFT: Q = Q {
    id: "file draft",
    describe: "unsaved file source and the original used to detect conflicts",
    sql: "SELECT original,body FROM notes_draft WHERE path=?",
};
pub fn draft(store: &Store, path: &str) -> Option<Draft> {
    draft_source(store, path).first().cloned()
}
pub fn draft_source(store: &Store, path: &str) -> Rc<Vec<Draft>> {
    store.rows(&DRAFT, &[Val::S(path.into())], |r| {
        Ok(Draft {
            original: r.get(0)?,
            body: r.get(1)?,
        })
    })
}

pub fn save_draft(store: &Store, path: String, draft: Draft, now: f64) -> Result<(), String> {
    store.write(move |c| {
        if draft.body == draft.original {
            c.execute("DELETE FROM notes_draft WHERE path=?", [path])?;
        } else {
            c.execute("INSERT INTO notes_draft(path,original,body,modified) VALUES(?1,?2,?3,?4)
                ON CONFLICT(path) DO UPDATE SET original=excluded.original,body=excluded.body,modified=excluded.modified",
                params![path,draft.original,draft.body,now])?;
        }
        Ok(())
    }).map_err(|e| e.to_string())
}

/// Bounded, complete UTF-8 reads. A preview must never become a truncated edit.
pub struct ReadFile(pub String);
impl Effect for ReadFile {
    const KIND: &'static str = "notes.read_file";
    type Reply = String;
    fn describe(&self) -> String {
        format!("read {} for editing", self.0)
    }
    fn writes(&self) -> bool {
        false
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<String, String> {
        let bytes = cx
            .cap::<dyn Disk>()?
            .read_file(&real_path(&self.0), MAX_FILE_BYTES + 1)?;
        decode(bytes)
    }
}
fn decode(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("this editor opens files up to 2 MiB".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "this file is not UTF-8 text")?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("this file contains binary data".into());
    }
    Ok(text)
}

pub struct SaveFile {
    pub path: String,
    pub original: String,
    pub body: String,
}
impl Effect for SaveFile {
    const KIND: &'static str = "notes.save_file";
    type Reply = ();
    fn describe(&self) -> String {
        format!("save {}", self.path)
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Disk>()?.replace_file(
            &real_path(&self.path),
            self.original.as_bytes(),
            self.body.as_bytes(),
        )
    }
}
