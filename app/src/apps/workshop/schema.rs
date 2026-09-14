use kernel::app::{Schema, Step};

// No replicated() declaration: every Workshop row is local to this database.
pub static SCHEMA: Schema = Schema {
    app: "workshop",
    // A store remembers how many rungs it climbed, so a rung keeps its place
    // for good and the ladder only ever grows at the end: the transcript's
    // own sweep sits after the table it sweeps, on fresh and old stores alike.
    steps: &[
        Step::Sql(V1),
        Step::Always(recover),
        Step::Sql(V2),
        Step::Sql(V3),
        Step::Sql(V4),
        Step::Always(recover_items),
        Step::Sql(V5),
    ],
};
const V1: &str = r#"
CREATE TABLE workshop_project (
 id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
 base_ref TEXT NOT NULL DEFAULT 'origin/main', status TEXT NOT NULL DEFAULT 'pending', error TEXT NOT NULL DEFAULT ''
);
CREATE TABLE workshop_workspace (
 id INTEGER PRIMARY KEY AUTOINCREMENT, project_id INTEGER NOT NULL REFERENCES workshop_project(id),
 label TEXT NOT NULL, path TEXT NOT NULL UNIQUE, branch TEXT NOT NULL, base_ref TEXT NOT NULL,
 status TEXT NOT NULL DEFAULT 'preparing', error TEXT NOT NULL DEFAULT '', activity REAL NOT NULL,
 git_error TEXT NOT NULL DEFAULT '', github_error TEXT NOT NULL DEFAULT '', snapshot_id INTEGER, pr_json TEXT NOT NULL DEFAULT '', pr_checked REAL NOT NULL DEFAULT 0,
 refresh_requested INTEGER NOT NULL DEFAULT 1, UNIQUE(project_id,label)
);
CREATE INDEX workshop_workspace_activity ON workshop_workspace(activity DESC,id DESC);
CREATE TABLE workshop_chat (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER NOT NULL REFERENCES workshop_workspace(id),
 ordinal INTEGER NOT NULL, provider TEXT NOT NULL DEFAULT 'codex', model TEXT NOT NULL DEFAULT 'default',
 status TEXT NOT NULL DEFAULT 'ready', draft TEXT NOT NULL DEFAULT '', last_used REAL NOT NULL,
 unread INTEGER NOT NULL DEFAULT 0, session_id TEXT, error TEXT NOT NULL DEFAULT '',
 UNIQUE(workspace_id,ordinal)
);
CREATE TABLE workshop_message (
 id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL REFERENCES workshop_chat(id),
 run_id INTEGER, role TEXT NOT NULL, body TEXT NOT NULL, step_id INTEGER, created REAL NOT NULL
);
CREATE INDEX workshop_message_chat ON workshop_message(chat_id,id);
CREATE TABLE workshop_run (
 id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL REFERENCES workshop_chat(id),
 provider TEXT NOT NULL, model TEXT NOT NULL, prompt TEXT NOT NULL, mode TEXT NOT NULL DEFAULT 'work',
 status TEXT NOT NULL DEFAULT 'pending', error TEXT NOT NULL DEFAULT '', created REAL NOT NULL,
 before_id INTEGER, after_id INTEGER, message_id INTEGER, usage TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX workshop_run_live ON workshop_run(status,chat_id,id);
CREATE TABLE workshop_snapshot (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER NOT NULL REFERENCES workshop_workspace(id),
 head TEXT NOT NULL, base_oid TEXT NOT NULL, tree_oid TEXT NOT NULL, created REAL NOT NULL, json TEXT NOT NULL,
 historical INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE workshop_change (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER NOT NULL REFERENCES workshop_workspace(id),
 snapshot_id INTEGER NOT NULL REFERENCES workshop_snapshot(id), path TEXT NOT NULL, old_path TEXT,
 patch TEXT NOT NULL, added INTEGER NOT NULL, deleted INTEGER NOT NULL, reviewed INTEGER NOT NULL DEFAULT 0,
 needs_recheck INTEGER NOT NULL DEFAULT 0, binary INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL,
 fingerprint TEXT NOT NULL, UNIQUE(snapshot_id,path)
);
CREATE INDEX workshop_change_snapshot ON workshop_change(snapshot_id,id);
CREATE TABLE workshop_review (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER NOT NULL, change_id INTEGER NOT NULL,
 snapshot_id INTEGER NOT NULL, path TEXT NOT NULL, fingerprint TEXT NOT NULL,
 actor TEXT NOT NULL, reviewed INTEGER NOT NULL, created REAL NOT NULL
);
CREATE INDEX workshop_review_file ON workshop_review(workspace_id,path,id DESC);
CREATE TABLE workshop_step (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER NOT NULL, chat_id INTEGER NOT NULL,
 before_id INTEGER NOT NULL, after_id INTEGER NOT NULL, diff_id INTEGER NOT NULL,
 status TEXT NOT NULL, created REAL NOT NULL
);
CREATE TABLE workshop_job (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER, kind TEXT NOT NULL, payload TEXT NOT NULL,
 status TEXT NOT NULL DEFAULT 'pending', result TEXT NOT NULL DEFAULT '', error TEXT NOT NULL DEFAULT '', created REAL NOT NULL
);
CREATE INDEX workshop_job_live ON workshop_job(status,id);
CREATE TABLE workshop_comment_draft (
 workspace_id INTEGER NOT NULL, path TEXT NOT NULL DEFAULT '', body TEXT NOT NULL, modified REAL NOT NULL, PRIMARY KEY(workspace_id,path)
);
CREATE TABLE workshop_tool_call (
 id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL, run_id INTEGER NOT NULL,
 name TEXT NOT NULL, arguments TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
 result TEXT NOT NULL DEFAULT '', error TEXT NOT NULL DEFAULT '', created REAL NOT NULL
);
CREATE INDEX workshop_tool_call_chat ON workshop_tool_call(chat_id,id);
CREATE TABLE workshop_setting (key TEXT PRIMARY KEY NOT NULL,value TEXT NOT NULL);
INSERT INTO workshop_setting VALUES('provider','codex'),('model','default');
CREATE TABLE workshop_terminal (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id INTEGER NOT NULL, session_key TEXT NOT NULL UNIQUE,
 embedded INTEGER NOT NULL DEFAULT 1, created REAL NOT NULL
);
"#;
const V2: &str = r#"
ALTER TABLE workshop_workspace ADD COLUMN archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1));
ALTER TABLE workshop_chat ADD COLUMN closed INTEGER NOT NULL DEFAULT 0 CHECK(closed IN (0,1));
"#;
const V3: &str = "ALTER TABLE workshop_chat ADD COLUMN unread_version INTEGER NOT NULL DEFAULT 0;";
// The structured transcript of a run: text segments, tool calls, todo lists,
// subagents and background tasks, each upserted in place as the harness
// streams. `key` is the provider's own item/tool-use identity; `parent` is the
// tool use that spawned a subagent, so nested work stays under its card.
const V4: &str = r#"
CREATE TABLE workshop_item (
 id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL REFERENCES workshop_chat(id),
 run_id INTEGER NOT NULL, key TEXT NOT NULL, parent TEXT NOT NULL DEFAULT '',
 kind TEXT NOT NULL, name TEXT NOT NULL DEFAULT '', title TEXT NOT NULL DEFAULT '',
 input TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '', meta TEXT NOT NULL DEFAULT '',
 status TEXT NOT NULL DEFAULT 'running', created REAL NOT NULL, updated REAL NOT NULL,
 UNIQUE(run_id,key)
);
CREATE INDEX workshop_item_chat ON workshop_item(chat_id,id);
"#;
// Where a chat was last read: the key of the transcript row that stood at
// the top of the view, and how far into it. Empty is the tail, which is
// what a chat that has never been scrolled shows — so the column's default
// is the behaviour every existing store already has.
const V5: &str = r#"
ALTER TABLE workshop_chat ADD COLUMN anchor_key TEXT NOT NULL DEFAULT '';
ALTER TABLE workshop_chat ADD COLUMN anchor_scroll REAL NOT NULL DEFAULT 0;
"#;
fn recover(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    // A process/session may be resumed by a new explicit send; an interrupted
    // external operation must never be replayed at startup (especially comments).
    c.execute_batch("UPDATE workshop_tool_call SET status='interrupted',error='The originating agent process ended with the app.' WHERE status IN ('pending','approved','running');
      UPDATE workshop_run SET status='interrupted',error='App closed while this run was active. Send a message to resume.' WHERE status='running';
      UPDATE workshop_chat SET status='interrupted',error='The previous agent process ended with the app.' WHERE status='running';
      UPDATE workshop_job SET status='interrupted',error='App closed during this operation. Check its result before trying again.' WHERE status='running';")
}

/// A card left running or in the background when the app closed has no
/// result coming: say so, rather than draw it live forever.
fn recover_items(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE workshop_item SET status='interrupted' WHERE status IN ('running','background')",
        [],
    )?;
    Ok(())
}

pub const PROTECTED: &[&str] = &[
    "workshop_project",
    "workshop_workspace",
    "workshop_chat",
    "workshop_message",
    "workshop_run",
    "workshop_snapshot",
    "workshop_change",
    "workshop_review",
    "workshop_step",
    "workshop_job",
    "workshop_comment_draft",
    "workshop_setting",
    "workshop_terminal",
    "workshop_tool_call",
    "workshop_item",
];

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The ladder as main shipped it, which every existing store has climbed.
    static SHIPPED: Schema = Schema {
        app: "workshop",
        steps: &[
            Step::Sql(V1),
            Step::Always(recover),
            Step::Sql(V2),
            Step::Sql(V3),
        ],
    };
    fn store() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY);")
            .unwrap();
        c
    }

    #[test]
    fn a_shipped_store_climbs_to_the_transcript_table_and_a_fresh_one_starts_there() {
        let c = store();
        SHIPPED.apply(&c).unwrap();
        assert_eq!(SHIPPED.progress(&c).unwrap(), 4);
        c.execute_batch(
            "INSERT INTO workshop_project(id,name,path) VALUES(1,'p','/p');
             INSERT INTO workshop_workspace(id,project_id,label,path,branch,base_ref,activity) VALUES(1,1,'basel','/w','workshop/basel','main',1);
             INSERT INTO workshop_chat(id,workspace_id,ordinal,last_used) VALUES(1,1,1,1);",
        )
        .unwrap();
        SCHEMA.apply(&c).unwrap();
        assert_eq!(SCHEMA.progress(&c).unwrap(), 7);
        c.execute("INSERT INTO workshop_item(chat_id,run_id,key,kind,status,created,updated) VALUES(1,1,'k','tool','running',1,1)", []).unwrap();
        // The next open sweeps what was left running.
        SCHEMA.apply(&c).unwrap();
        let status: String = c
            .query_row("SELECT status FROM workshop_item", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "interrupted");
        let fresh = store();
        SCHEMA.apply(&fresh).unwrap();
        assert_eq!(SCHEMA.progress(&fresh).unwrap(), 7);
        let items: i64 = fresh
            .query_row("SELECT count(*) FROM workshop_item", [], |r| r.get(0))
            .unwrap();
        assert_eq!(items, 0);
    }
}
