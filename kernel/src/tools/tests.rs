//! The kernel's tools, driven over a fake session with no window in sight.
//!
//! The test app owns two tables on purpose: one keyed, which `sql.write`
//! may write and undo, and one with no primary key, which the session
//! extension would record nothing for and which the guard therefore
//! refuses.

use std::any::Any;

use serde_json::{json, Value};

use crate::app::{App, Mode, Root, Schema, Step};
use crate::layout::SlotId;
use crate::nav::Nav;
use crate::panel::{Opening, Panel, PanelId, PanelKind, Tag};
use crate::session::{Action, Session};
use crate::store::Store;
use crate::tool::Tool;

use super::MAX_ROWS;

// -- a build with one app, two tables and two panel kinds ------------------------

static SCHEMA: Schema = Schema {
    app: "note",
    steps: &[Step::Sql(
        "CREATE TABLE note(id INTEGER PRIMARY KEY, body TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX idx_note_done ON note(done);
         CREATE TABLE note_scrap(body TEXT NOT NULL);",
    )],
};

const LIST: Tag = Tag("notes");
const CARD: Tag = Tag("note");

fn list_id() -> PanelId {
    PanelId::bare(LIST)
}

fn card_id(n: i64) -> PanelId {
    PanelId::new(CARD, [n.to_string()])
}

struct Card(PanelId);

impl Panel for Card {
    fn id(&self) -> &PanelId {
        &self.0
    }
    fn title(&self) -> String {
        if self.0.tag == LIST {
            "notes".into()
        } else {
            format!("note {}", self.0.arg(0).unwrap_or("?"))
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

struct ListKind;
impl PanelKind for ListKind {
    fn tag(&self) -> Tag {
        LIST
    }
    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Card(id.clone()))
    }
}

struct CardKind;
impl PanelKind for CardKind {
    fn tag(&self) -> Tag {
        CARD
    }
    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Card(id.clone()))
    }
}

static LIST_KIND: ListKind = ListKind;
static CARD_KIND: CardKind = CardKind;
static KINDS: &[&dyn PanelKind] = &[&LIST_KIND, &CARD_KIND];

struct Notes;

impl App for Notes {
    fn id(&self) -> &'static str {
        "note"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }
    fn schema(&self) -> Option<&'static Schema> {
        Some(&SCHEMA)
    }
    fn describe(&self) -> Option<&'static str> {
        Some("note: one row a note. `body` is its text, `done` is 0 or 1.")
    }
    fn seed(&self, store: &Store, _mode: Mode) -> rusqlite::Result<()> {
        store.write(|c| {
            c.execute("INSERT INTO note(id, body) VALUES(1, 'buy milk')", [])?;
            c.execute("INSERT INTO note(id, body) VALUES(2, 'call max')", [])?;
            Ok(())
        })
    }
    fn roots(&self) -> Vec<Root> {
        vec![Root::new(list_id(), "notes", "notes")]
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

static NOTES: Notes = Notes;
static APPS: &[&dyn App] = &[&NOTES];

// -- the harness -------------------------------------------------------------------

fn session() -> Session {
    Session::fake(APPS)
}

/// One tool by name, out of the registry this build offers — so a test runs
/// exactly what a chat would.
fn tool(s: &Session, name: &str) -> Tool {
    s.apps()
        .tool(name)
        .unwrap_or_else(|| panic!("no tool {name}"))
        .clone()
}

/// One call, its arguments read against the tool's own schema first,
/// exactly as a run does it.
fn call(s: &mut Session, name: &str, input: Value) -> Result<Value, String> {
    let t = tool(s, name);
    t.check(&input)?;
    let out = (t.run)(s, &input);
    s.settle();
    out
}

/// Opens a root panel, as the launcher would.
fn open_root(s: &mut Session, id: PanelId) -> SlotId {
    let show = id.clone();
    s.act(
        Action::new("open", format!("open “{id}”")).moving(move |wm| {
            wm.open(show, None, false);
        }),
    );
    s.settle();
    s.focus().expect("the new slot has focus")
}

/// One note's text, or `None` when the row has gone.
fn body(s: &Session, id: i64) -> Option<String> {
    s.store()
        .conn()
        .query_row("SELECT body FROM note WHERE id = ?1", [id], |r| r.get(0))
        .ok()
}

fn done(s: &Session, id: i64) -> i64 {
    s.store()
        .conn()
        .query_row("SELECT done FROM note WHERE id = ?1", [id], |r| r.get(0))
        .unwrap_or(-1)
}

fn count(s: &Session, table: &str) -> i64 {
    s.store()
        .conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or(-1)
}

/// The newest history node, as the overlay would draw it.
fn head(s: &Session) -> Option<crate::history::Row> {
    let (mut rows, _) = s.history().rows();
    rows.pop()
}

// -- sql.query -----------------------------------------------------------------------

#[test]
fn a_query_answers_columns_and_rows() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.query",
        json!({"sql": "SELECT id, body FROM note ORDER BY id"}),
    )
    .expect("the query ran");
    assert_eq!(out["columns"], json!(["id", "body"]));
    assert_eq!(out["rows"], json!([[1, "buy milk"], [2, "call max"]]));
    assert_eq!(out["truncated"], json!(false));
}

#[test]
fn a_query_binds_its_parameters_and_names_every_kind_of_value() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.query",
        json!({
            "sql": "SELECT ?1, ?2, ?3, ?4, x'0102030405'",
            "params": ["a", 3, 1.5, null]
        }),
    )
    .expect("the query ran");
    assert_eq!(
        out["rows"],
        json!([["a", 3, 1.5, null, "<blob 5 bytes>"]]),
        "text, integer, real, null — and a blob named rather than encoded"
    );
}

#[test]
fn a_query_stops_at_two_hundred_rows_and_says_so() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.query",
        json!({"sql": "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 500) \
                       SELECT i FROM n"}),
    )
    .expect("the query ran");
    assert_eq!(out["rows"].as_array().expect("rows").len(), MAX_ROWS);
    assert_eq!(out["truncated"], json!(true));
}

#[test]
fn a_query_stops_at_sixty_four_kilobytes_and_says_so() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.query",
        json!({"sql": "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 100) \
                       SELECT hex(zeroblob(1000)) FROM n"}),
    )
    .expect("the query ran");
    let rows = out["rows"].as_array().expect("rows").len();
    assert!(
        rows > 0 && rows < 100,
        "cut on size long before the row count: {rows}"
    );
    assert_eq!(out["truncated"], json!(true));
}

#[test]
fn a_query_may_not_write_and_sqlite_says_why() {
    let mut s = session();
    let e = call(
        &mut s,
        "sql.query",
        json!({"sql": "DELETE FROM note WHERE id = 1"}),
    )
    .expect_err("the reader is query-only");
    assert!(
        e.to_lowercase().contains("readonly") || e.to_lowercase().contains("read-only"),
        "SQLite's own words: {e}"
    );
    assert!(body(&s, 1).is_some(), "and nothing was written");
}

#[test]
fn a_query_takes_one_statement() {
    let mut s = session();
    let e = call(&mut s, "sql.query", json!({"sql": "SELECT 1; SELECT 2"}))
        .expect_err("two statements are not one");
    assert!(!e.is_empty(), "and it says so");
}

// -- sql.write -------------------------------------------------------------------------

#[test]
fn a_write_inserts_a_row_and_cmd_z_takes_it_back() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.write",
        json!({
            "sql": "INSERT INTO note(id, body) VALUES(?1, ?2)",
            "params": [7, "water the plants"]
        }),
    )
    .expect("the write ran");
    assert_eq!(out, json!({"changes": 1}));
    assert_eq!(body(&s, 7).as_deref(), Some("water the plants"));

    // The node is labelled by the statement, which is what the history
    // overlay shows and what the card will say.
    let node = head(&s).expect("a node");
    assert_eq!(node.kind, "sql.write");
    assert_eq!(node.label, "INSERT INTO note(id, body) VALUES(?1, ?2)");

    assert!(s.undo(), "one node back");
    assert_eq!(body(&s, 7), None, "the row is gone");
    assert!(s.redo(), "and forward again");
    assert_eq!(body(&s, 7).as_deref(), Some("water the plants"));
}

#[test]
fn an_update_of_two_rows_is_one_undo_that_restores_both() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.write",
        json!({"sql": "UPDATE note SET done = 1"}),
    )
    .expect("the write ran");
    assert_eq!(out, json!({"changes": 2}));
    assert_eq!((done(&s, 1), done(&s, 2)), (1, 1));
    assert!(s.undo());
    assert_eq!((done(&s, 1), done(&s, 2)), (0, 0), "both rows, one node");
}

#[test]
fn a_batch_runs_in_one_transaction_and_is_one_undo() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.write",
        json!({"statements": [
            "INSERT INTO note(id, body) VALUES(8, 'one')",
            "INSERT INTO note(id, body) VALUES(9, 'two')",
            "UPDATE note SET done = 1 WHERE id = 1"
        ]}),
    )
    .expect("the batch ran");
    assert_eq!(out, json!({"changes": 3}));
    assert!(s.undo());
    assert_eq!((body(&s, 8), body(&s, 9), done(&s, 1)), (None, None, 0));
}

#[test]
fn a_batch_that_fails_half_way_writes_nothing() {
    let mut s = session();
    let e = call(
        &mut s,
        "sql.write",
        json!({"statements": [
            "INSERT INTO note(id, body) VALUES(8, 'one')",
            "INSERT INTO note(id, body) VALUES(1, 'clashes')"
        ]}),
    )
    .expect_err("the second statement clashes");
    assert!(e.contains("UNIQUE") || e.contains("constraint"), "{e}");
    assert_eq!(body(&s, 8), None, "the first went back with it");
}

#[test]
fn the_kernels_own_tables_are_refused_by_name() {
    let mut s = session();
    let before = count(&s, "meta");
    let e = call(
        &mut s,
        "sql.write",
        json!({"sql": "INSERT INTO meta(key, value) VALUES('mine', 1)"}),
    )
    .expect_err("meta is the kernel's");
    assert!(e.contains("meta") && e.contains("the kernel's own"), "{e}");
    assert_eq!(count(&s, "meta"), before, "and nothing was written");
    assert!(head(&s).is_none(), "a refused write leaves no node");
}

#[test]
fn every_kernel_table_is_refused_the_same_way() {
    let mut s = session();
    for sql in [
        "UPDATE panel SET kind = 'x'",
        "DELETE FROM workspace",
        "UPDATE ws_col SET idx = 1",
        "DELETE FROM wm",
        "DELETE FROM effect",
        "DELETE FROM repl_log",
        "UPDATE repl SET holding = 0",
    ] {
        match call(&mut s, "sql.write", json!({ "sql": sql })) {
            Ok(v) => panic!("{sql} was allowed: {v}"),
            Err(e) => assert!(
                e.contains("the kernel's own"),
                "{sql} refused for the wrong reason: {e}"
            ),
        }
    }
}

#[test]
fn a_table_with_no_primary_key_is_refused_because_undo_would_lie() {
    let mut s = session();
    let e = call(
        &mut s,
        "sql.write",
        json!({"sql": "INSERT INTO note_scrap(body) VALUES('nowhere')"}),
    )
    .expect_err("nothing would be recorded for it");
    assert!(
        e.contains("note_scrap") && e.contains("no primary key"),
        "{e}"
    );
    assert_eq!(count(&s, "note_scrap"), 0, "and nothing was written");
}

#[test]
fn a_tables_shape_is_the_schema_ladders_and_not_a_tools() {
    let mut s = session();
    for sql in [
        "CREATE TABLE mine(id INTEGER PRIMARY KEY)",
        "DROP TABLE note",
        "ALTER TABLE note ADD COLUMN extra TEXT",
    ] {
        match call(&mut s, "sql.write", json!({ "sql": sql })) {
            Ok(v) => panic!("{sql} was allowed: {v}"),
            Err(e) => assert!(
                e.contains("not a tool's") || e.contains("SQLite's own catalogue"),
                "{sql}: {e}"
            ),
        }
    }
    assert!(body(&s, 1).is_some(), "and the tables stand as they were");
    assert!(
        s.store()
            .conn()
            .query_row("SELECT 1 FROM sqlite_master WHERE name = 'mine'", [], |r| r
                .get::<_, i64>(0))
            .is_err(),
        "nothing was made"
    );
}

#[test]
fn transaction_control_is_the_sessions_and_not_a_tools() {
    let mut s = session();
    let e = call(
        &mut s,
        "sql.write",
        json!({"statements": [
            "UPDATE note SET done = 1 WHERE id = 1",
            "COMMIT",
            "UPDATE note SET body = 'after' WHERE id = 2"
        ]}),
    )
    .expect_err("a commit inside the session's own transaction");
    assert!(e.contains("a call is one transaction"), "{e}");
    assert_eq!(
        done(&s, 1),
        0,
        "and the statement before it went back with it"
    );
    assert!(head(&s).is_none(), "a refused write leaves no node");
}

#[test]
fn a_savepoint_is_refused_the_same_way() {
    let mut s = session();
    for sql in ["SAVEPOINT x", "BEGIN", "ROLLBACK", "END", "RELEASE x"] {
        match call(&mut s, "sql.write", json!({ "sql": sql })) {
            Ok(v) => panic!("{sql} was allowed: {v}"),
            Err(e) => assert!(
                e.contains("a call is one transaction"),
                "{sql} refused for the wrong reason: {e}"
            ),
        }
    }
}

#[test]
fn a_syntax_error_comes_back_in_sqlites_own_words() {
    let mut s = session();
    let e = call(
        &mut s,
        "sql.write",
        json!({"sql": "UPDAT note SET done = 1"}),
    )
    .expect_err("that is not SQL");
    assert!(e.contains("syntax error"), "{e}");
    assert_eq!(done(&s, 1), 0, "and nothing was written");
    assert!(head(&s).is_none(), "and no node was recorded");
}

#[test]
fn a_write_asks_for_one_of_sql_or_statements() {
    let mut s = session();
    assert_eq!(
        call(&mut s, "sql.write", json!({})).expect_err("neither"),
        "give `sql`, or `statements` for a batch"
    );
    assert_eq!(
        call(
            &mut s,
            "sql.write",
            json!({"sql": "DELETE FROM note", "statements": ["DELETE FROM note"]})
        )
        .expect_err("both"),
        "give either `sql` or `statements`, not both"
    );
    assert_eq!(
        call(
            &mut s,
            "sql.write",
            json!({"statements": ["DELETE FROM note WHERE id = ?1"], "params": [1]})
        )
        .expect_err("a batch binds nothing"),
        "`params` binds to `sql`; a batch of `statements` binds nothing"
    );
    assert_eq!(count(&s, "note"), 2, "and nothing ran");
}

#[test]
fn a_write_that_moved_no_row_answers_zero() {
    let mut s = session();
    let out = call(
        &mut s,
        "sql.write",
        json!({"sql": "UPDATE note SET done = 1 WHERE id = 99"}),
    )
    .expect("the statement ran");
    assert_eq!(out, json!({"changes": 0}));
    assert!(s.undo(), "the node is there — something was attempted");
    assert_eq!((done(&s, 1), done(&s, 2)), (0, 0), "and nothing moved back");
}

#[test]
fn a_reversal_that_would_fight_a_later_change_expires_instead() {
    let mut s = session();
    call(
        &mut s,
        "sql.write",
        json!({"sql": "UPDATE note SET body = 'archived' WHERE id = 1"}),
    )
    .expect("the write ran");
    // Somebody else moves the same row afterwards — a sync pass, another
    // device, the person's own verb.
    s.store()
        .write(|c| {
            c.execute("UPDATE note SET body = 'somebody else' WHERE id = 1", [])
                .map(|_| ())
        })
        .expect("the second write");

    assert!(
        !s.undo(),
        "the claim will not go back, and there is no older node"
    );
    assert_eq!(
        body(&s, 1).as_deref(),
        Some("somebody else"),
        "and it did not write over what is there now"
    );
    assert_eq!(
        head(&s).expect("the node").state,
        "expired",
        "expired, not half-applied"
    );
}

// -- sql.schema -----------------------------------------------------------------------

#[test]
fn the_schema_lists_the_apps_tables_and_what_the_app_says_about_them() {
    let mut s = session();
    let out = call(&mut s, "sql.schema", json!({})).expect("the dictionary");
    let tables = out["tables"].as_array().expect("tables").clone();
    let names: Vec<String> = tables
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(names.contains(&"note".to_string()), "{names:?}");
    assert!(names.contains(&"note_scrap".to_string()), "{names:?}");
    assert!(
        names.contains(&"idx_note_done".to_string()),
        "indexes too: {names:?}"
    );
    for kernel in [
        "meta",
        "panel",
        "workspace",
        "ws_col",
        "wm",
        "effect",
        "repl_log",
    ] {
        assert!(
            !names.contains(&kernel.to_string()),
            "{kernel} in {names:?}"
        );
    }
    let note = tables
        .iter()
        .find(|t| t["name"] == json!("note"))
        .expect("the note table");
    assert!(note["sql"].as_str().expect("its sql").contains("body TEXT"));
    assert_eq!(
        out["apps"],
        json!([{
            "id": "note",
            "describe": "note: one row a note. `body` is its text, `done` is 0 or 1."
        }])
    );
}

// -- panels.list ------------------------------------------------------------------------

#[test]
fn the_list_says_what_is_open_where_and_what_is_joined_to_what() {
    let mut s = session();
    let list = open_root(&mut s, list_id());
    s.nav(Nav::Preview {
        from: list,
        id: card_id(2),
    });
    s.settle();
    let card = s
        .showing(&card_id(2))
        .first()
        .copied()
        .expect("the preview");

    let out = call(&mut s, "panels.list", json!({})).expect("the workspace");
    let panels = out["panels"].as_array().expect("panels").clone();
    assert_eq!(panels.len(), 2);
    assert_eq!(
        panels[0],
        json!({
            "slot": list, "tag": "notes", "args": [], "title": "notes",
            "workspace": 1, "focused": true, "joined_to": null
        })
    );
    assert_eq!(
        panels[1],
        json!({
            "slot": card, "tag": "note", "args": ["2"], "title": "note 2",
            "workspace": 1, "focused": false, "joined_to": list
        }),
        "a preview keeps focus behind and joins to the panel it came from"
    );
}

// -- panels.context ---------------------------------------------------------------------

#[test]
fn the_context_of_a_slot_is_the_panel_rendered_for_the_model() {
    let mut s = session();
    let list = open_root(&mut s, list_id());
    let out = call(&mut s, "panels.context", json!({"slot": list})).expect("the panel's text");
    assert_eq!(out["slot"], json!(list));
    assert_eq!(out["title"], json!("notes"));
    let text = out["context"].as_str().expect("the rendered panel");
    assert!(
        text.starts_with("<panel id=\"notes\" title=\"notes\" workspace=\"1\">"),
        "the header names the panel: {text}"
    );
    assert!(text.trim_end().ends_with("</panel>"), "a whole block: {text}");

    // A slot nobody has open is refused by number, in words.
    let err = call(&mut s, "panels.context", json!({"slot": 999})).expect_err("no such slot");
    assert_eq!(err, "no panel in slot 999");
}

// -- panels.open ------------------------------------------------------------------------

#[test]
fn opening_a_panel_previews_it_beside_the_focus() {
    let mut s = session();
    let list = open_root(&mut s, list_id());
    let out = call(&mut s, "panels.open", json!({"tag": "note", "args": ["1"]}))
        .expect("the panel opened");
    let slot = out["slot"].as_u64().expect("a slot");
    assert_ne!(slot, list);
    assert_eq!(s.focus(), Some(list), "focus stays where it was");
    assert_eq!(s.joined_child(list), Some(slot), "joined to the focus");
    assert_eq!(
        s.panel(slot).expect("the instance").borrow().title(),
        "note 1"
    );

    // A second open joins the end of the chain rather than replacing the
    // first: a model that opens two panels means both to stay.
    let out = call(&mut s, "panels.open", json!({"tag": "note", "args": ["2"]}))
        .expect("the second panel opened");
    let second = out["slot"].as_u64().expect("a slot");
    assert_eq!(s.focus(), Some(list), "focus still where it was");
    assert_eq!(s.joined_child(list), Some(slot), "the first is still there");
    assert_eq!(s.joined_child(slot), Some(second), "the second hangs off it");

    // What is already open in the chain is answered, not opened twice.
    let out = call(&mut s, "panels.open", json!({"tag": "note", "args": ["1"]}))
        .expect("the panel is open already");
    assert_eq!(out["slot"].as_u64(), Some(slot));
    assert_eq!(s.joined_child(slot), Some(second), "and nothing moved");
}

#[test]
fn opening_a_tag_no_app_owns_is_refused_in_words() {
    let mut s = session();
    open_root(&mut s, list_id());
    assert_eq!(
        call(&mut s, "panels.open", json!({"tag": "wombat"})).expect_err("no such kind"),
        "no panel kind `wombat` in this build"
    );
}

#[test]
fn opening_a_panel_with_nothing_focused_says_so() {
    let mut s = session();
    assert!(s.focus().is_none(), "an empty workspace");
    assert_eq!(
        call(&mut s, "panels.open", json!({"tag": "notes"})).expect_err("nowhere to put it"),
        "no panel has focus, so there is nothing to open beside"
    );
}

// -- the registry ------------------------------------------------------------------------

#[test]
fn the_kernels_tools_lead_the_list_and_read_their_arguments() {
    let s = session();
    let names: Vec<&str> = s.apps().tools().iter().map(|t| t.name).collect();
    assert_eq!(
        &names[..6],
        &[
            "sql.query",
            "sql.write",
            "sql.schema",
            "panels.list",
            "panels.context",
            "panels.open"
        ]
    );
    assert!(s.apps().tool("sql.write").expect("the writer").writes);
    assert!(!s.apps().tool("sql.query").expect("the reader").writes);
    // The schema is the other half of the contract, and it is read.
    assert_eq!(
        tool(&s, "sql.query").check(&json!({})),
        Err("missing `sql`".to_string())
    );
    assert_eq!(
        tool(&s, "panels.open").check(&json!({"tag": "note", "args": [1]})),
        Err("`args[0]` must be a string".to_string())
    );
    assert_eq!(
        tool(&s, "sql.write").check(&json!({"sql": "x", "oops": 1})),
        Err("unknown key `oops`".to_string())
    );
}
