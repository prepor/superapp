use kernel::app::{Schema, Step};

// No replicated() declaration: every Workshop row is local to this database.
pub static SCHEMA: Schema = Schema {
    app: "workshop",
    steps: &[Step::Sql(V1), Step::Always(recover)],
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
fn recover(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    // A process/session may be resumed by a new explicit send; an interrupted
    // external operation must never be replayed at startup (especially comments).
    c.execute_batch("UPDATE workshop_tool_call SET status='interrupted',error='The originating agent process ended with the app.' WHERE status IN ('pending','approved','running');
      UPDATE workshop_run SET status='interrupted',error='App closed while this run was active. Send a message to resume.' WHERE status='running';
      UPDATE workshop_chat SET status='interrupted',error='The previous agent process ended with the app.' WHERE status='running';
      UPDATE workshop_job SET status='interrupted',error='App closed during this operation. Check its result before trying again.' WHERE status='running';")
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
];
