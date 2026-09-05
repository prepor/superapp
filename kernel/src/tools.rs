//! The kernel's own tools: the store, read and written, and the workspace.
//!
//! Every build has these, whatever apps it was given, so
//! [`Apps::new`](crate::app::Apps::new) chains them ahead of the apps' — the
//! way it chains the bucket's problem source. They are the direct access to
//! SQLite the plan promises, plus the workspace as context: what is open,
//! what the tables are, and a way to put a panel beside the chat.
//!
//! Two rules hold over the writing one. **The kernel's tables are refused by
//! name**: the workspace, the effect queue and the replication log are the
//! shell's own bookkeeping, and a model that rewrites them has broken the
//! window it is talking through. And **a write is undoable or it does not
//! happen**: the session extension records a transaction's changeset, its
//! inverse is the history node's claim, and a table it would record nothing
//! for — one with no primary key — is refused rather than written and lied
//! about afterwards.
//!
//! The apps' tools are preferred wherever one exists (`mail.archive` files
//! the letter *and* tells the server; an `UPDATE` on `message` reaches no
//! server at all). The system prompt says so; this module offers the writer
//! anyway, because it was asked for and because `cmd+z` covers its rows.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::session::{invert_strm, ConflictAction, ConflictType};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::effect::World;
use crate::history::Intent;
use crate::layout::SlotId;
use crate::nav::Nav;
use crate::panel::PanelId;
use crate::session::{Action, Session};
use crate::tool::Tool;

/// How many rows one question is worth answering. Past this the answer is
/// another `LIMIT`, not a bigger reply.
const MAX_ROWS: usize = 200;

/// How much JSON one answer may carry. A model's context is the scarce
/// thing here, and a wide table reaches this long before it reaches
/// [`MAX_ROWS`].
const MAX_JSON: usize = 64 * 1024;

/// The kernel's own tools, in the order a request lists them.
#[must_use]
pub fn all() -> Vec<Tool> {
    vec![
        Tool::new(
            "sql.query",
            "Run one read-only SQL statement against the app's SQLite store and \
             get its rows back. This is the way to answer any question about \
             the data that no other tool answers directly — counts, joins, \
             filters, anything. Call sql.schema first if you do not know the \
             tables. At most 200 rows and 64 KiB come back; add LIMIT and \
             narrow the columns rather than asking for everything.",
            json!({
                "type": "object",
                "properties": {
                    "sql": {"type": "string", "description": "one SELECT statement; ? placeholders bind to params"},
                    "params": {"type": "array", "description": "values for the ? placeholders, in order"}
                },
                "required": ["sql"],
                "additionalProperties": false
            }),
            false,
            query,
        ),
        Tool::new(
            "sql.write",
            "Run one INSERT, UPDATE or DELETE — or a batch of them — in a single \
             transaction. The whole call is one undoable action: the person \
             takes it back with cmd+z. Prefer an app's own tool wherever one \
             exists (mail.archive, files.rename): those keep the app's promises \
             to the outside world, and a hand-written row does not. The kernel's \
             own tables are refused, and so is a table with no primary key.",
            json!({
                "type": "object",
                "properties": {
                    "sql": {"type": "string", "description": "one statement; ? placeholders bind to params"},
                    "params": {"type": "array", "description": "values for the ? placeholders of `sql`, in order"},
                    "statements": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "several statements, run in order in one transaction; binds nothing"
                    }
                },
                "additionalProperties": false
            }),
            true,
            write,
        ),
        Tool::new(
            "sql.schema",
            "The store's data dictionary: every table and index this build has, \
             with the SQL that made it, and each app's description of its own \
             data in its own words. Call this before writing a query against \
             tables you have not seen yet.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            false,
            schema,
        ),
        Tool::new(
            "panels.list",
            "What the person has open right now: every panel on every \
             workspace, with its tag and arguments, its title, which panel it \
             is joined to, and which one has focus. Call this to find out what \
             is on screen before opening anything or referring to “this”.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            false,
            list,
        ),
        Tool::new(
            "panels.context",
            "One open panel as the person sees it: what it is about in its \
             app's words, the queries its last draw ran with their rows read \
             again now, and its recent effects — the same text a panel chip \
             carries into a chat. Call this with a slot from panels.list to \
             look at what the person is looking at without asking them to \
             paste it.",
            json!({
                "type": "object",
                "properties": {
                    "slot": {"type": "integer", "description": "the panel's slot, as panels.list spells it"}
                },
                "required": ["slot"],
                "additionalProperties": false
            }),
            false,
            context,
        ),
        Tool::new(
            "panels.open",
            "Open a panel so the person can look at what you are talking about. \
             It joins the end of the chain of panels hanging off the one that \
             has focus, so what is already open stays open; a panel already \
             open there is answered, not opened twice. Focus stays where it is. \
             Use the tags panels.list shows; a mailbox takes no arguments, a \
             message or a directory takes one.",
            json!({
                "type": "object",
                "properties": {
                    "tag": {"type": "string", "description": "the panel kind, as panels.list spells it"},
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "the identity's arguments, as panels.list spells them"
                    }
                },
                "required": ["tag"],
                "additionalProperties": false
            }),
            false,
            open,
        ),
    ]
}

// -- what a refusal reads as ----------------------------------------------------

/// Why an [`Session::act`] that answered `None` refused, in words a model
/// can act on. The lease is the one refusal a caller cannot see coming, and
/// it is the same sentence the person's own verb gets.
///
/// Every writing tool ends its `act` with this, so a call that could not be
/// made says why rather than answering nothing.
#[must_use]
pub fn refused(s: &Session) -> String {
    if s.writable() {
        "the store refused the write".to_string()
    } else {
        "another device holds the lease — nothing was written".to_string()
    }
}

// -- sql.query -------------------------------------------------------------------

/// One statement on the store's reader, which is `query_only` by
/// construction — a write attempt fails in SQLite's own words, and there is
/// nothing here to police.
fn query(s: &mut Session, input: &Value) -> Result<Value, String> {
    let sql = text(input, "sql")?;
    let params = params_of(input)?;
    let conn = s.store().conn();
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let n = stmt.column_count();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(params.iter()))
        .map_err(|e| e.to_string())?;
    let mut out: Vec<Value> = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        if out.len() >= MAX_ROWS {
            truncated = true;
            break;
        }
        let mut cells: Vec<Value> = Vec::with_capacity(n);
        for i in 0..n {
            cells.push(json_of(row.get_ref(i).map_err(|e| e.to_string())?));
        }
        let cell = Value::Array(cells);
        bytes += cell.to_string().len() + 1;
        if bytes > MAX_JSON && !out.is_empty() {
            truncated = true;
            break;
        }
        out.push(cell);
    }
    Ok(json!({"columns": columns, "rows": out, "truncated": truncated}))
}

/// One SQLite value as JSON. A blob is named rather than encoded: what a
/// model can do with a megabyte of base64 is nothing, and the size is the
/// part of it that answers a question.
fn json_of(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(format!("<blob {} bytes>", b.len())),
    }
}

// -- sql.write --------------------------------------------------------------------

/// One statement or a batch, in one transaction, as one undoable action.
///
/// The whole of the safety is in three places: the authorizer below, which
/// stands only for the length of the closure; the changeset the store hands
/// back, which is what the node claims; and `cmd+z`.
fn write(s: &mut Session, input: &Value) -> Result<Value, String> {
    let (statements, params) = batch(input)?;
    if !s.writable() {
        return Err("another device holds the lease — nothing was written".to_string());
    }
    let label = cut(&statements[0], 60);
    let deny = Guard::of(s.store().conn());
    // What the closure could not do, in words — our own sentence for a
    // refusal, SQLite's for everything else. The `act` below only answers
    // whether it worked.
    let said: Arc<Mutex<Option<String>>> = Arc::default();
    let told = said.clone();
    let changes = s.act(Action::writing("sql.write", label, move |tx| {
        let refusal: Arc<Mutex<Option<String>>> = Arc::default();
        let seen = refusal.clone();
        // The guard comes first: this is the writer's own connection, which
        // every other action in the process writes through, so the
        // authorizer comes off however the closure ends.
        let _guard = Standing(tx);
        tx.authorizer(Some(move |ctx: AuthContext<'_>| {
            match deny.refusal(&ctx.action) {
                Some(why) => {
                    *seen.lock().expect("the authorizer's word") = Some(why);
                    Authorization::Deny
                }
                None => Authorization::Allow,
            }
        }))?;
        let mut n = 0i64;
        let mut wrong = None;
        for sql in &statements {
            let ran = if params.is_empty() {
                tx.execute(sql, [])
            } else {
                tx.execute(sql, rusqlite::params_from_iter(params.iter()))
            };
            match ran {
                Ok(k) => n += k as i64,
                Err(e) => {
                    // The authorizer's own sentence where it has one: SQLite
                    // says "not authorized" and nothing about which table or
                    // why, and that is the whole of what the model needs.
                    wrong = Some(
                        refusal
                            .lock()
                            .expect("the authorizer's word")
                            .take()
                            .unwrap_or_else(|| e.to_string()),
                    );
                    break;
                }
            }
        }
        match wrong {
            None => Ok(n),
            Some(why) => {
                *told.lock().expect("the refusal") = Some(why.clone());
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                    Some(why),
                ))
            }
        }
    }));
    let Some(changes) = changes else {
        return Err(said
            .lock()
            .expect("the refusal")
            .take()
            .unwrap_or_else(|| refused(s)));
    };
    // The rows this transaction moved, as the session extension recorded
    // them. Empty where nothing replicated changed — a statement that
    // matched no row — and then there is nothing to give back either.
    if let Some(changeset) = s.take_changeset() {
        s.claim(Box::new(Rows { changeset, changes }));
    }
    Ok(json!({"changes": changes}))
}

/// The authorizer, taken off the connection whatever happens to the closure
/// that installed it. The writer is one connection for the whole process,
/// and one left standing would refuse the next action anybody files.
struct Standing<'c>(&'c Connection);

impl Drop for Standing<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    }
}

/// The statements a call asks for, and the values bound to the first —
/// `sql` with its `params`, or `statements` on its own.
fn batch(input: &Value) -> Result<(Vec<String>, Vec<SqlValue>), String> {
    match (input.get("sql"), input.get("statements")) {
        (Some(_), Some(_)) => Err("give either `sql` or `statements`, not both".to_string()),
        (None, None) => Err("give `sql`, or `statements` for a batch".to_string()),
        (Some(_), None) => Ok((vec![text(input, "sql")?.to_string()], params_of(input)?)),
        (None, Some(list)) => {
            if input.get("params").is_some() {
                return Err(
                    "`params` binds to `sql`; a batch of `statements` binds nothing".to_string(),
                );
            }
            let list = list
                .as_array()
                .ok_or_else(|| "`statements` must be an array".to_string())?;
            let mut out = Vec::with_capacity(list.len());
            for (i, v) in list.iter().enumerate() {
                out.push(
                    v.as_str()
                        .ok_or_else(|| format!("`statements[{i}]` must be a string"))?
                        .to_string(),
                );
            }
            if out.is_empty() {
                return Err("`statements` is empty".to_string());
            }
            Ok((out, Vec::new()))
        }
    }
}

/// What a write may touch, decided once — before the transaction, off the
/// reader — and then asked of every statement SQLite prepares.
///
/// Three refusals. **The kernel's own tables**, by name: `meta`,
/// `workspace`, `ws_col`, `panel`, `wm`, `effect`, replication's log, and
/// SQLite's own catalogue. **A table with no primary key**, because the
/// session extension silently records nothing for one and the undo would
/// lie. And **a table this store did not have when the call began**, for
/// the same reason: what a `CREATE TABLE` in the same batch makes is
/// outside the set the writer's session is attached to.
///
/// The shape of a table is refused outright, whosever it is: a table's name
/// and columns belong to the app's schema ladder, which is a commit and a
/// migration, not a call.
struct Guard {
    /// Every ordinary table `main` had, and whether it has a primary key.
    tables: Arc<HashMap<String, bool>>,
}

impl Guard {
    /// Reads the store's shape off a reader.
    fn of(conn: &Connection) -> Guard {
        Guard {
            tables: Arc::new(tables_of(conn)),
        }
    }

    /// Why this action is refused, or `None` to let it through. Every read
    /// is let through: a write may read whatever it likes to decide what to
    /// write.
    fn refusal(&self, action: &AuthAction<'_>) -> Option<String> {
        let table = match action {
            AuthAction::Insert { table_name }
            | AuthAction::Delete { table_name }
            | AuthAction::Update { table_name, .. } => *table_name,
            AuthAction::DropTable { table_name }
            | AuthAction::DropVtable { table_name, .. }
            | AuthAction::CreateTable { table_name }
            | AuthAction::CreateVtable { table_name, .. }
            | AuthAction::AlterTable { table_name, .. } => {
                return Some(format!(
                    "“{table_name}” is not a tool's to make, alter or drop: \
                     a table's name and shape are its app's schema ladder's"
                ))
            }
            _ => return None,
        };
        let name = table.to_ascii_lowercase();
        if name.starts_with("sqlite_") {
            return Some(format!(
                "“{table}” is SQLite's own catalogue — the tables here are made by \
                 the apps' schema ladders"
            ));
        }
        if kernel_table(&name) {
            return Some(format!(
                "“{table}” is the kernel's own: the workspace, the effect queue and \
                 the replication log are not an agent's to write"
            ));
        }
        match self.tables.get(&name) {
            Some(true) => None,
            Some(false) => Some(format!(
                "“{table}” has no primary key, so the changeset records nothing for it \
                 and cmd+z could not put its rows back"
            )),
            None => Some(format!(
                "“{table}” is not a table this store had when the call began, \
                 so nothing written to it could be undone"
            )),
        }
    }
}

/// The kernel's own tables, by name. `repl*` is replication's whole
/// namespace, `sqlite_*` is asked separately because its sentence is
/// another one.
fn kernel_table(lower: &str) -> bool {
    matches!(
        lower,
        "meta" | "workspace" | "ws_col" | "panel" | "wm" | "effect"
    ) || lower.starts_with("repl")
        || lower.starts_with("sqlite_")
}

/// Whether a changeset's table is one an agent's undo may touch — the
/// filter both halves of [`Rows`] apply with. A transaction's changeset
/// carries the layout save that rode along in it; the node's own snapshot
/// is what puts the layout back, so those rows are left out of the apply.
fn not_kernel(table: &str) -> bool {
    !kernel_table(&table.to_ascii_lowercase())
}

/// Every ordinary table of `main`, and whether it has a primary key. Views,
/// virtual tables and the shadow tables under them are left out: a
/// changeset may not carry one, so a write to one could not be undone.
fn tables_of(conn: &Connection) -> HashMap<String, bool> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare("PRAGMA table_list") else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    }) else {
        return out;
    };
    let names: Vec<String> = rows
        .filter_map(Result::ok)
        .filter(|(schema, _, kind)| schema == "main" && kind == "table")
        .map(|(_, name, _)| name)
        .collect();
    for name in names {
        let keyed = conn
            .prepare(&format!("PRAGMA table_info(\"{name}\")"))
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, i64>(5))
                    .map(|rows| rows.filter_map(Result::ok).any(|pk| pk > 0))
            })
            .unwrap_or(false);
        out.insert(name.to_ascii_lowercase(), keyed);
    }
    out
}

/// What an agent's `sql.write` claimed of the store: the rows the
/// transaction moved, as the session extension recorded them.
///
/// Every other claim in this tree is an app's own sentence about its own
/// rows — *mail:7 archived*, *renamed “a” to “b”* — because the app knows
/// what it did. A tool that ran a person's `UPDATE` knows only which rows
/// moved, so the inverse of the changeset is the whole of what it can
/// promise: undo applies it, redo applies the original, and both go through
/// [`Store::write`](crate::store::Store::write), so the reversal replicates
/// to the other device and invalidates the queries that drew the rows.
struct Rows {
    /// The changeset the write recorded, forward.
    changeset: Vec<u8>,
    /// How many rows the statements said they changed — the sentence, not
    /// the mechanism.
    changes: i64,
}

impl Intent for Rows {
    fn describe(&self) -> String {
        if self.changes == 1 {
            "1 row written by sql.write".to_string()
        } else {
            format!("{} rows written by sql.write", self.changes)
        }
    }

    /// A rehearsal, always rolled back: if putting the rows back would
    /// fight what is there now, the node expires and says so rather than
    /// writing over somebody else's change.
    fn blocked(&self, w: &World) -> Option<String> {
        let inverse = match invert(&self.changeset) {
            Ok(cs) => cs,
            Err(e) => return Some(e),
        };
        rehearse(w, inverse).err()
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        apply(w, invert(&self.changeset)?)
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        apply(w, self.changeset.clone())
    }
}

/// A changeset the other way round.
fn invert(cs: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(cs.len());
    let mut input = cs;
    invert_strm(&mut input, &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A changeset applied through the one door every write goes through, so it
/// is captured, replicated and invalidates the queries that read those
/// tables.
///
/// The conflict policy is *replace* where a row is there and differs, which
/// is what a person means by undo, and *omit* where the row has gone —
/// there is nothing to take back. Anything else (a constraint, a foreign
/// key) aborts the whole apply, and the walk says the claim would not go
/// back. [`Rows::blocked`] rehearses first, so in the ordinary case a
/// conflict is refused before anything is written.
fn apply(w: &World, cs: Vec<u8>) -> Result<(), String> {
    w.store()
        .write(move |tx| {
            let mut input = &cs[..];
            tx.apply_strm(
                &mut input,
                Some(not_kernel as fn(&str) -> bool),
                on_conflict,
            )
        })
        .map_err(|e| e.to_string())
}

/// The same apply, in a transaction that is always rolled back: a dry
/// check, so a node that could not be undone cleanly expires instead of
/// half-applying.
fn rehearse(w: &World, cs: Vec<u8>) -> Result<(), String> {
    let clash = Arc::new(AtomicBool::new(false));
    let said: Arc<Mutex<Option<String>>> = Arc::default();
    let (hit, told) = (clash.clone(), said.clone());
    let _ = w.store().write(move |tx| {
        let mut input = &cs[..];
        if let Err(e) = tx.apply_strm(
            &mut input,
            Some(not_kernel as fn(&str) -> bool),
            move |_ty, _item| {
                hit.store(true, Ordering::Relaxed);
                ConflictAction::SQLITE_CHANGESET_ABORT
            },
        ) {
            *told.lock().expect("the rehearsal's word") = Some(e.to_string());
        }
        // It landed or it did not; either way this was a rehearsal, and an
        // error is how a transaction goes back.
        Err::<(), _>(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("a rehearsal is always rolled back".to_string()),
        ))
    });
    if clash.load(Ordering::Relaxed) {
        return Err("the rows have changed since".to_string());
    }
    // A store that would not even rehearse — the lease has moved — is not
    // this node's problem: the reversal will say so in the gate's words.
    let wrong = said.lock().expect("the rehearsal's word").take();
    wrong.map_or(Ok(()), Err)
}

/// Replace what is there and differs; skip what has gone; stop for anything
/// else. See [`apply`].
fn on_conflict(kind: ConflictType, _item: rusqlite::session::ChangesetItem) -> ConflictAction {
    match kind {
        ConflictType::SQLITE_CHANGESET_DATA | ConflictType::SQLITE_CHANGESET_CONFLICT => {
            ConflictAction::SQLITE_CHANGESET_REPLACE
        }
        ConflictType::SQLITE_CHANGESET_NOTFOUND => ConflictAction::SQLITE_CHANGESET_OMIT,
        _ => ConflictAction::SQLITE_CHANGESET_ABORT,
    }
}

// -- sql.schema -------------------------------------------------------------------

/// The data dictionary: what SQLite has, and what the apps say about it.
fn schema(s: &mut Session, _input: &Value) -> Result<Value, String> {
    let conn = s.store().conn();
    let shadows: HashSet<String> = shadow_tables(conn);
    let mut tables: Vec<Value> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT name, tbl_name, sql FROM sqlite_master
         WHERE type IN ('table','index') AND sql IS NOT NULL
         ORDER BY tbl_name, type DESC, name",
    ) {
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows.filter_map(Result::ok) {
            let (name, owner, sql) = row;
            let lower = name.to_ascii_lowercase();
            if kernel_table(&lower)
                || kernel_table(&owner.to_ascii_lowercase())
                || shadows.contains(&lower)
            {
                continue;
            }
            tables.push(json!({"name": name, "sql": sql}));
        }
    }
    let apps: Vec<Value> = s
        .apps()
        .list()
        .iter()
        .map(|a| json!({"id": a.id(), "describe": a.describe()}))
        .collect();
    Ok(json!({"tables": tables, "apps": apps}))
}

/// The tables an FTS index (or any other virtual table) keeps under itself.
/// They are real tables with real SQL and no meaning to anybody reading a
/// dictionary.
fn shadow_tables(conn: &Connection) -> HashSet<String> {
    let Ok(mut stmt) = conn.prepare("PRAGMA table_list") else {
        return HashSet::new();
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
    else {
        return HashSet::new();
    };
    rows.filter_map(Result::ok)
        .filter(|(_, kind)| kind == "shadow")
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect()
}

// -- panels.list ------------------------------------------------------------------

/// Every open slot, in slot order — the workspace as the model reads it.
fn list(s: &mut Session, _input: &Value) -> Result<Value, String> {
    let focus = s.focus();
    let panels: Vec<Value> = s
        .panels()
        .into_iter()
        .map(|(slot, inst)| {
            let id = inst.borrow().id().clone();
            json!({
                "slot": slot,
                "tag": id.tag.as_str(),
                "args": id.args,
                "title": inst.borrow().title(),
                // One-based, as the person's cmd+1 counts them.
                "workspace": s.ws().ws_of(slot).map(|k| k + 1),
                "focused": focus == Some(slot),
                "joined_to": s.join_parent_of(slot),
            })
        })
        .collect();
    Ok(json!({"panels": panels}))
}

// -- panels.context ---------------------------------------------------------------

/// One open panel rendered for the model — [`crate::context::render`] over
/// the slot's identity, its `about`, and the trace of its last draw, the
/// rows read again now. A slot nobody has open is refused by number.
fn context(s: &mut Session, input: &Value) -> Result<Value, String> {
    let slot = input
        .get("slot")
        .and_then(Value::as_i64)
        .ok_or("`slot` must be an integer")?;
    let slot = SlotId::try_from(slot).map_err(|_| format!("no panel in slot {slot}"))?;
    let cx = crate::context::of(s, slot).ok_or_else(|| format!("no panel in slot {slot}"))?;
    let store = s.store().clone();
    let effects = crate::context::recent_effects(&store, &cx.id, crate::context::EFFECTS);
    let text = crate::context::render(&store, &cx, &effects);
    Ok(json!({"slot": slot, "title": cx.title, "context": text}))
}

// -- panels.open ------------------------------------------------------------------

/// A panel at the end of the focused panel's joined chain, focus staying
/// where it is — the same [`Nav::Preview`] a cursor walk makes, so it lands
/// on the same kind of undoable node. The *end* of the chain, because a
/// preview from the focused slot would replace whatever is joined there,
/// and a model that opens two panels in a row means both to stay. An
/// identity already open in that chain is answered, not opened twice.
fn open(s: &mut Session, input: &Value) -> Result<Value, String> {
    // Asked of the tags the build owns rather than interned first: a tag is
    // a `&'static str`, so interning a name nobody owns would leak one per
    // spelling a model guessed at.
    let name = text(input, "tag")?;
    let tag = s
        .apps()
        .tags()
        .into_iter()
        .find(|t| t.as_str() == name)
        .ok_or_else(|| format!("no panel kind `{name}` in this build"))?;
    let args = strings(input, "args")?;
    let id = PanelId::new(tag, args);
    let focus = s
        .focus()
        .ok_or("no panel has focus, so there is nothing to open beside")?;
    // The chain hanging off the focus, focus first. Joins never loop, and
    // the bound is only there so a broken layout cannot spin this.
    let chain: Vec<SlotId> = std::iter::successors(Some(focus), |&x| s.joined_child(x))
        .take(64)
        .collect();
    let showing: HashSet<SlotId> = s.showing(&id).into_iter().collect();
    if let Some(open) = chain.iter().copied().find(|x| showing.contains(x)) {
        return Ok(json!({"slot": open}));
    }
    let from = chain.last().copied().unwrap_or(focus);
    // Which slot is the new one: this identity may well be open elsewhere,
    // and the answer is the slot that was not there a moment ago.
    let before = showing;
    s.nav(Nav::Preview {
        from,
        id: id.clone(),
    });
    let slot = s
        .showing(&id)
        .into_iter()
        .find(|slot| !before.contains(slot))
        .ok_or_else(|| format!("“{id}” did not open"))?;
    Ok(json!({"slot": slot}))
}

// -- reading the arguments ---------------------------------------------------------

/// A string argument. [`Tool::check`] has read the schema already; this is
/// what makes `run` honest on its own, since a test may call it directly.
fn text<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

/// An optional array of strings — a panel's arguments.
fn strings(input: &Value, key: &str) -> Result<Vec<String>, String> {
    let Some(v) = input.get(key) else {
        return Ok(Vec::new());
    };
    let list = v
        .as_array()
        .ok_or_else(|| format!("`{key}` must be an array of strings"))?;
    list.iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_str()
                .map(String::from)
                .ok_or_else(|| format!("`{key}[{i}]` must be a string"))
        })
        .collect()
}

/// The `params` of a query or a write, as SQLite binds them. A JSON array
/// or an object is not a value a column holds, and saying so is more use to
/// a model than a stringified `{}`.
fn params_of(input: &Value) -> Result<Vec<SqlValue>, String> {
    let Some(v) = input.get("params") else {
        return Ok(Vec::new());
    };
    let list = v
        .as_array()
        .ok_or_else(|| "`params` must be an array".to_string())?;
    let mut out = Vec::with_capacity(list.len());
    for (i, v) in list.iter().enumerate() {
        out.push(match v {
            Value::Null => SqlValue::Null,
            Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
            Value::String(s) => SqlValue::Text(s.clone()),
            Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => SqlValue::Integer(i),
                (None, Some(f)) => SqlValue::Real(f),
                _ => return Err(format!("`params[{i}]` is not a number SQLite holds")),
            },
            _ => {
                return Err(format!(
                    "`params[{i}]` must be a string, a number, a boolean, or null"
                ))
            }
        });
    }
    Ok(out)
}

/// A statement as a history label: one line, cut where a person stops
/// reading.
fn cut(sql: &str, at: usize) -> String {
    let one: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= at {
        return one;
    }
    let mut out: String = one.chars().take(at - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests;
