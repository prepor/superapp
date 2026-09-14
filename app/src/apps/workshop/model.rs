use kernel::filter::Op;
use kernel::richtable::{Dir, SqlSource, SqlSpec, TagDef, TagSql, TagType, Values};
use kernel::store::{Store, Val, Q};
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectRow {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub base_ref: String,
    pub status: String,
    pub error: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: i64,
    pub project_id: i64,
    pub label: String,
    pub project: String,
    pub path: String,
    pub branch: String,
    pub base_ref: String,
    pub status: String,
    pub error: String,
    pub activity: f64,
    pub unread: bool,
    pub snapshot_id: Option<i64>,
    pub pr_json: String,
    pub archived: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatRow {
    pub id: i64,
    pub workspace_id: i64,
    pub ordinal: i64,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub draft: String,
    pub last_used: f64,
    pub unread: bool,
    pub unread_version: i64,
    pub session_id: Option<String>,
    pub error: String,
    pub closed: bool,
    /// Where this chat was last read: the key of the transcript row that
    /// stood at the top of the view, empty for the tail, and how far into
    /// that row the view began.
    pub anchor_key: String,
    pub anchor_scroll: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: i64,
    pub chat_id: i64,
    pub run_id: Option<i64>,
    pub role: String,
    pub body: String,
    pub step_id: Option<i64>,
    pub created: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChangeRow {
    pub id: i64,
    pub workspace_id: i64,
    pub snapshot_id: i64,
    pub path: String,
    pub old_path: Option<String>,
    pub patch: String,
    pub added: i64,
    pub deleted: i64,
    pub reviewed: bool,
    pub needs_recheck: bool,
    pub binary: bool,
    pub status: String,
    pub fingerprint: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRow {
    pub id: i64,
    pub workspace_id: i64,
    pub head: String,
    pub base_oid: String,
    pub tree_oid: String,
    pub created: f64,
    pub json: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepRow {
    pub id: i64,
    pub workspace_id: i64,
    pub chat_id: i64,
    pub before_id: i64,
    pub after_id: i64,
    pub diff_id: i64,
    pub has_changes: bool,
    pub status: String,
    pub created: f64,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Progress {
    pub reviewed: i64,
    pub total: i64,
    pub recheck: i64,
    pub other: i64,
}

fn project_row(r: &Row<'_>) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: r.get(0)?,
        name: r.get(1)?,
        path: r.get(2)?,
        base_ref: r.get(3)?,
        status: r.get(4)?,
        error: r.get(5)?,
    })
}
fn workspace_row(r: &Row<'_>) -> rusqlite::Result<WorkspaceRow> {
    Ok(WorkspaceRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        label: r.get(2)?,
        project: r.get(3)?,
        path: r.get(4)?,
        branch: r.get(5)?,
        base_ref: r.get(6)?,
        status: r.get(7)?,
        error: r.get(8)?,
        activity: r.get(9)?,
        unread: r.get(10)?,
        snapshot_id: r.get(11)?,
        pr_json: r.get(12)?,
        archived: r.get(13)?,
    })
}
fn chat_row(r: &Row<'_>) -> rusqlite::Result<ChatRow> {
    Ok(ChatRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        ordinal: r.get(2)?,
        provider: r.get(3)?,
        model: r.get(4)?,
        status: r.get(5)?,
        draft: r.get(6)?,
        last_used: r.get(7)?,
        unread: r.get(8)?,
        session_id: r.get(9)?,
        error: r.get(10)?,
        closed: r.get(11)?,
        unread_version: r.get(12)?,
        anchor_key: r.get(13)?,
        anchor_scroll: r.get(14)?,
    })
}
fn change_row(r: &Row<'_>) -> rusqlite::Result<ChangeRow> {
    Ok(ChangeRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        snapshot_id: r.get(2)?,
        path: r.get(3)?,
        old_path: r.get(4)?,
        patch: r.get(5)?,
        added: r.get(6)?,
        deleted: r.get(7)?,
        reviewed: r.get(8)?,
        needs_recheck: r.get(9)?,
        binary: r.get(10)?,
        status: r.get(11)?,
        fingerprint: r.get(12)?,
    })
}
fn snapshot_row(r: &Row<'_>) -> rusqlite::Result<SnapshotRow> {
    Ok(SnapshotRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        head: r.get(2)?,
        base_oid: r.get(3)?,
        tree_oid: r.get(4)?,
        created: r.get(5)?,
        json: r.get(6)?,
    })
}
fn step_row(r: &Row<'_>) -> rusqlite::Result<StepRow> {
    Ok(StepRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        chat_id: r.get(2)?,
        before_id: r.get(3)?,
        after_id: r.get(4)?,
        diff_id: r.get(5)?,
        has_changes: r.get(8)?,
        status: r.get(6)?,
        created: r.get(7)?,
    })
}
const WORKSPACE_FROM: &str = "workshop_workspace w JOIN workshop_project p ON p.id=w.project_id";
const WORKSPACE_SELECT:&str="w.id,w.project_id,w.label,p.name,w.path,w.branch,w.base_ref,w.status,trim(w.error||' '||w.git_error||' '||w.github_error),w.activity,EXISTS(SELECT 1 FROM workshop_chat c WHERE c.workspace_id=w.id AND c.closed=0 AND (c.unread=1 OR EXISTS(SELECT 1 FROM workshop_tool_call t WHERE t.chat_id=c.id AND t.status='pending'))),w.snapshot_id,w.pr_json,w.archived";
const CHANGE_SELECT:&str="id,workspace_id,snapshot_id,path,old_path,patch,added,deleted,reviewed,needs_recheck,binary,status,fingerprint";
const WORKSPACE_TAGS: &[TagDef] = &[
    TagDef {
        name: "project_id",
        kind: TagType::Number,
        ops: &[Op::Eq],
        describe: "local repository ID",
        values: Values::None,
    },
    TagDef {
        name: "archived",
        kind: TagType::Bool,
        ops: &[],
        describe: "archived workspaces",
        values: Values::None,
    },
    TagDef {
        name: "project",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "project name",
        values: Values::Dynamic,
    },
    TagDef {
        name: "unread",
        kind: TagType::Bool,
        ops: &[],
        describe: "new agent result",
        values: Values::None,
    },
];
const CHANGE_TAGS: &[TagDef] = &[
    TagDef {
        name: "workspace",
        kind: TagType::Number,
        ops: &[Op::Eq],
        describe: "workspace ID",
        values: Values::None,
    },
    TagDef {
        name: "snapshot",
        kind: TagType::Number,
        ops: &[Op::Eq],
        describe: "comparison snapshot",
        values: Values::None,
    },
    TagDef {
        name: "unreviewed",
        kind: TagType::Bool,
        ops: &[],
        describe: "files still to review",
        values: Values::None,
    },
    TagDef {
        name: "changed",
        kind: TagType::Bool,
        ops: &[],
        describe: "reviewed files that changed",
        values: Values::None,
    },
];
pub static PROJECTS: SqlSource<ProjectRow, i64> = SqlSource {
    spec: &SqlSpec {
        id: "workshop projects",
        describe: "local repositories",
        select: "id,name,path,base_ref,status,error",
        from: "workshop_project",
        base: "1",
        text: &["name", "path"],
        index: None,
        tags: &[],
        order: &[("name", Dir::Asc), ("id", Dir::Asc)],
        group: None,
        key: "id",
        deps: &[],
    },
    tags: &[],
    map: project_row,
    key: |r| r.id,
    rank: |r| vec![Val::S(r.name.clone()), Val::I(r.id)],
    suggest: |_, _, _| vec![],
};
pub static WORKSPACES:SqlSource<WorkspaceRow,i64>=SqlSource{
 spec:&SqlSpec{id:"workshop workspaces",describe:"local workspaces by activity",select:WORKSPACE_SELECT,from:WORKSPACE_FROM,base:"1",text:&["w.label","p.name","w.branch"],index:None,tags:&[("project_id",TagSql::Col("p.id")),("archived",TagSql::Where("w.archived=1")),("project",TagSql::Col("p.name")),("unread",TagSql::Where("EXISTS(SELECT 1 FROM workshop_chat c WHERE c.workspace_id=w.id AND c.closed=0 AND (c.unread=1 OR EXISTS(SELECT 1 FROM workshop_tool_call t WHERE t.chat_id=c.id AND t.status='pending')))"))],order:&[("w.activity",Dir::Desc),("w.id",Dir::Desc)],group:None,key:"w.id",deps:&["workshop_chat","workshop_tool_call"]},tags:WORKSPACE_TAGS,map:workspace_row,key:|r|r.id,rank:|r|vec![Val::F(r.activity),Val::I(r.id)],suggest:|store,tag,_|if tag=="project"{projects(store).iter().map(|p|kernel::richtable::Suggestion { label: p.name.clone(), value: project_filter_value(p), describe: p.path.clone() }).collect()}else{vec![]}};
pub static CHANGES: SqlSource<ChangeRow, i64> = SqlSource {
    spec: &SqlSpec {
        id: "workshop changes",
        describe: "whole files in a displayed comparison",
        select: CHANGE_SELECT,
        from: "workshop_change",
        base: "1",
        text: &["path"],
        index: None,
        tags: &[
            ("workspace", TagSql::Col("workspace_id")),
            ("snapshot", TagSql::Col("snapshot_id")),
            ("unreviewed", TagSql::Where("reviewed=0")),
            ("changed", TagSql::Where("needs_recheck=1 AND reviewed=0")),
        ],
        order: &[("path", Dir::Asc), ("id", Dir::Asc)],
        group: None,
        key: "id",
        deps: &[],
    },
    tags: CHANGE_TAGS,
    map: change_row,
    key: |r| r.id,
    rank: |r| vec![Val::S(r.path.clone()), Val::I(r.id)],
    suggest: |_, _, _| vec![],
};

static PROJECT_LIST: Q = Q {
    id: "workshop project list",
    describe: "local repositories",
    sql: "SELECT id,name,path,base_ref,status,error FROM workshop_project ORDER BY name,id",
};
static WORKSPACE_LIST:Q=Q{id:"workshop workspace list",describe:"local workspaces",sql:"SELECT w.id,w.project_id,w.label,p.name,w.path,w.branch,w.base_ref,w.status,trim(w.error||' '||w.git_error||' '||w.github_error),w.activity,EXISTS(SELECT 1 FROM workshop_chat c WHERE c.workspace_id=w.id AND c.closed=0 AND (c.unread=1 OR EXISTS(SELECT 1 FROM workshop_tool_call t WHERE t.chat_id=c.id AND t.status='pending'))),w.snapshot_id,w.pr_json,w.archived FROM workshop_workspace w JOIN workshop_project p ON p.id=w.project_id ORDER BY w.activity DESC,w.id DESC"};
static CHAT_LIST:Q=Q{id:"workshop chats",describe:"untitled chats in this workspace",sql:"SELECT id,workspace_id,ordinal,provider,model,CASE WHEN status='running' AND EXISTS(SELECT 1 FROM workshop_tool_call t WHERE t.chat_id=workshop_chat.id AND t.status='pending') THEN 'waiting' ELSE status END,draft,last_used,unread,session_id,error,closed,unread_version,anchor_key,anchor_scroll FROM workshop_chat WHERE workspace_id=? AND closed=0 ORDER BY ordinal"};
static CLOSED_CHAT_LIST:Q=Q{id:"workshop closed chats",describe:"untitled chats in this workspace",sql:"SELECT id,workspace_id,ordinal,provider,model,CASE WHEN status='running' AND EXISTS(SELECT 1 FROM workshop_tool_call t WHERE t.chat_id=workshop_chat.id AND t.status='pending') THEN 'waiting' ELSE status END,draft,last_used,unread,session_id,error,closed,unread_version,anchor_key,anchor_scroll FROM workshop_chat WHERE workspace_id=? AND closed=1 ORDER BY ordinal"};
static CHAT:Q=Q{id:"workshop chat",describe:"local provider session",sql:"SELECT id,workspace_id,ordinal,provider,model,CASE WHEN status='running' AND EXISTS(SELECT 1 FROM workshop_tool_call t WHERE t.chat_id=workshop_chat.id AND t.status='pending') THEN 'waiting' ELSE status END,draft,last_used,unread,session_id,error,closed,unread_version,anchor_key,anchor_scroll FROM workshop_chat WHERE id=?"};
static MESSAGES:Q=Q{id:"workshop messages",describe:"local chat transcript",sql:"SELECT id,chat_id,role,body,step_id,created,run_id FROM workshop_message WHERE chat_id=? ORDER BY id"};
static SNAPSHOT:Q=Q{id:"workshop snapshot",describe:"immutable comparison",sql:"SELECT id,workspace_id,head,base_oid,tree_oid,created,json FROM workshop_snapshot WHERE id=?"};
static CHANGE_LIST:Q=Q{id:"workshop file changes",describe:"files in this comparison",sql:"SELECT id,workspace_id,snapshot_id,path,old_path,patch,added,deleted,reviewed,needs_recheck,binary,status,fingerprint FROM workshop_change WHERE snapshot_id=? ORDER BY path"};
static CHANGE:Q=Q{id:"workshop file",describe:"complete changed file",sql:"SELECT id,workspace_id,snapshot_id,path,old_path,patch,added,deleted,reviewed,needs_recheck,binary,status,fingerprint FROM workshop_change WHERE id=?"};
static STEPS:Q=Q{id:"workshop steps",describe:"immutable intervals between agent turn boundaries; writers may overlap",sql:"SELECT id,workspace_id,chat_id,before_id,after_id,diff_id,status,created,EXISTS(SELECT 1 FROM workshop_change WHERE snapshot_id=workshop_step.diff_id) FROM workshop_step WHERE workspace_id=? ORDER BY id DESC"};
static STEP:Q=Q{id:"workshop step",describe:"one agent interval",sql:"SELECT id,workspace_id,chat_id,before_id,after_id,diff_id,status,created,EXISTS(SELECT 1 FROM workshop_change WHERE snapshot_id=workshop_step.diff_id) FROM workshop_step WHERE id=?"};
static SETTINGS: Q = Q {
    id: "workshop defaults",
    describe: "new chat provider and model",
    sql: "SELECT key,value FROM workshop_setting",
};
static COMMENT: Q = Q {
    id: "workshop unsent comment",
    describe: "unsent GitHub text, never an internal thread",
    sql: "SELECT body FROM workshop_comment_draft WHERE workspace_id=? AND path=?",
};
pub fn projects(s: &Store) -> Rc<Vec<ProjectRow>> {
    s.rows(&PROJECT_LIST, &[], project_row)
}
/// A generated filter retains a readable name, while the final suffix carries
/// identity even when repositories share a name or their display name changes.
pub fn project_filter_value(project: &ProjectRow) -> String {
    format!("{} #{}", project.name, project.id)
}
pub fn project_filter_id(value: &str) -> Option<i64> {
    value
        .rsplit_once(" #")?
        .1
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
}
pub fn project_matches(s: &Store, value: &str) -> Vec<i64> {
    if let Some(id) = project_filter_id(value) {
        return projects(s)
            .iter()
            .filter(|p| p.id == id)
            .map(|p| p.id)
            .collect();
    }
    static MATCHING: Q = Q {
        id: "workshop project filter matches",
        describe: "repositories matching a manually typed project-name filter",
        sql: "SELECT id FROM workshop_project WHERE casefold(name) LIKE casefold(?) ESCAPE '\\' ORDER BY id",
    };
    let pattern = format!(
        "%{}%",
        value
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    s.rows(&MATCHING, &[Val::S(pattern)], |r| r.get(0))
        .as_ref()
        .clone()
}
pub fn workspaces(s: &Store) -> Rc<Vec<WorkspaceRow>> {
    s.rows(&WORKSPACE_LIST, &[], workspace_row)
}
pub fn workspace(s: &Store, id: i64) -> Option<WorkspaceRow> {
    workspaces(s).iter().find(|r| r.id == id).cloned()
}
pub fn chats(s: &Store, id: i64) -> Rc<Vec<ChatRow>> {
    s.rows(&CHAT_LIST, &[Val::I(id)], chat_row)
}
pub fn closed_chats(s: &Store, id: i64) -> Rc<Vec<ChatRow>> {
    s.rows(&CLOSED_CHAT_LIST, &[Val::I(id)], chat_row)
}
pub fn chat(s: &Store, id: i64) -> Option<ChatRow> {
    s.rows(&CHAT, &[Val::I(id)], chat_row).first().cloned()
}
pub fn messages(s: &Store, id: i64) -> Rc<Vec<MessageRow>> {
    s.rows(&MESSAGES, &[Val::I(id)], |r| {
        Ok(MessageRow {
            id: r.get(0)?,
            chat_id: r.get(1)?,
            run_id: r.get(6)?,
            role: r.get(2)?,
            body: r.get(3)?,
            step_id: r.get(4)?,
            created: r.get(5)?,
        })
    })
}
pub fn snapshot(s: &Store, id: i64) -> Option<SnapshotRow> {
    s.rows(&SNAPSHOT, &[Val::I(id)], snapshot_row)
        .first()
        .cloned()
}
pub fn latest_snapshot(s: &Store, id: i64) -> Option<SnapshotRow> {
    snapshot(s, workspace(s, id)?.snapshot_id?)
}
pub fn changes(s: &Store, id: i64) -> Rc<Vec<ChangeRow>> {
    s.rows(&CHANGE_LIST, &[Val::I(id)], change_row)
}
pub fn change(s: &Store, id: i64) -> Option<ChangeRow> {
    s.rows(&CHANGE, &[Val::I(id)], change_row).first().cloned()
}
pub fn steps(s: &Store, id: i64) -> Rc<Vec<StepRow>> {
    s.rows(&STEPS, &[Val::I(id)], step_row)
}
pub fn step(s: &Store, id: i64) -> Option<StepRow> {
    s.rows(&STEP, &[Val::I(id)], step_row).first().cloned()
}
pub fn progress(s: &Store, id: i64) -> Progress {
    let mut p = Progress::default();
    for f in changes(s, id).iter() {
        let n = f.added + f.deleted;
        p.total += n;
        if f.reviewed {
            p.reviewed += n;
        } else if f.needs_recheck {
            p.recheck += n;
        }
        if n == 0 && !f.reviewed {
            p.other += 1;
        }
    }
    p
}
pub fn settings(s: &Store) -> (String, String) {
    let v = s.rows(&SETTINGS, &[], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    });
    let get = |key, default: &str| {
        v.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or(default.into())
    };
    (get("provider", "codex"), get("model", "default"))
}
pub fn models(s: &Store, provider: &str) -> Rc<Vec<String>> {
    let values = s.rows(&SETTINGS, &[], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    });
    Rc::new(
        values
            .iter()
            .find(|(key, _)| key == &format!("models:{provider}"))
            .and_then(|(_, value)| serde_json::from_str::<Vec<String>>(value).ok())
            .unwrap_or_else(|| vec!["default".into()]),
    )
}
pub fn provider_status(s: &Store, provider: &str) -> String {
    let values = s.rows(&SETTINGS, &[], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    });
    values
        .iter()
        .find(|(key, _)| key == &format!("provider_status:{provider}"))
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| "checking local CLI…".into())
}
pub fn comment_draft(s: &Store, id: i64, path: Option<&str>) -> String {
    s.rows(
        &COMMENT,
        &[Val::I(id), Val::S(path.unwrap_or("").into())],
        |r| r.get::<_, String>(0),
    )
    .first()
    .cloned()
    .unwrap_or_default()
}
pub fn chat_conn(c: &Connection, id: i64) -> rusqlite::Result<ChatRow> {
    c.query_row(CHAT.sql, [id], chat_row)
}
pub fn workspace_conn(c: &Connection, id: i64) -> rusqlite::Result<WorkspaceRow> {
    let sql = format!("SELECT {WORKSPACE_SELECT} FROM {WORKSPACE_FROM} WHERE w.id=?");
    c.query_row(&sql, [id], workspace_row)
}
pub fn change_conn(c: &Connection, id: i64) -> rusqlite::Result<ChangeRow> {
    c.query_row(CHANGE.sql, [id], change_row)
}
pub fn snapshot_conn(c: &Connection, id: i64) -> rusqlite::Result<SnapshotRow> {
    c.query_row(SNAPSHOT.sql, [id], snapshot_row)
}
pub fn active_workspace_conn(c: &Connection, id: i64) -> rusqlite::Result<WorkspaceRow> {
    let workspace = workspace_conn(c, id)?;
    if workspace.archived {
        return Err(inactive("Restore this workspace before starting work."));
    }
    Ok(workspace)
}
pub fn active_chat_conn(c: &Connection, id: i64) -> rusqlite::Result<ChatRow> {
    let chat = chat_conn(c, id)?;
    if chat.closed {
        return Err(inactive("Reopen this chat before starting work."));
    }
    active_workspace_conn(c, chat.workspace_id)?;
    Ok(chat)
}
fn inactive(message: &str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(message)))
}
pub fn new_chat_tx(
    c: &Connection,
    workspace: i64,
    provider: Option<&str>,
    model: Option<&str>,
    now: f64,
) -> rusqlite::Result<i64> {
    active_workspace_conn(c, workspace)?;
    let default = |key: &str| {
        c.query_row(
            "SELECT value FROM workshop_setting WHERE key=?",
            [key],
            |r| r.get::<_, String>(0),
        )
    };
    let provider = provider.map(str::to_owned).unwrap_or(default("provider")?);
    let model = model.map(str::to_owned).unwrap_or(default("model")?);
    c.execute("INSERT INTO workshop_chat(workspace_id,ordinal,provider,model,last_used) SELECT ?1,COALESCE(MAX(ordinal),0)+1,?2,?3,?4 FROM workshop_chat WHERE workspace_id=?1",params![workspace,provider,model,now])?;
    Ok(c.last_insert_rowid())
}
pub fn queue_tx(
    c: &Connection,
    workspace: Option<i64>,
    kind: &str,
    payload: serde_json::Value,
    now: f64,
) -> rusqlite::Result<i64> {
    if let Some(workspace) = workspace {
        active_workspace_conn(c, workspace)?;
    }
    c.execute(
        "INSERT INTO workshop_job(workspace_id,kind,payload,created) VALUES(?1,?2,?3,?4)",
        params![workspace, kind, payload.to_string(), now],
    )?;
    Ok(c.last_insert_rowid())
}
pub fn send_tx(
    c: &Connection,
    chat: i64,
    text: &str,
    mode: &str,
    now: f64,
) -> rusqlite::Result<i64> {
    let row = active_chat_conn(c, chat)?;
    c.execute("INSERT INTO workshop_run(chat_id,provider,model,prompt,mode,created) VALUES(?1,?2,?3,?4,?5,?6)",params![chat,row.provider,row.model,text,mode,now])?;
    let run = c.last_insert_rowid();
    c.execute(
        "INSERT INTO workshop_message(chat_id,run_id,role,body,created) VALUES(?1,?2,'You',?3,?4)",
        params![chat, run, text, now],
    )?;
    c.execute("UPDATE workshop_chat SET draft='',last_used=?2,status=CASE WHEN status='running' THEN status ELSE 'pending' END,error='' WHERE id=?1",params![chat,now])?;
    c.execute(
        "UPDATE workshop_workspace SET activity=?2 WHERE id=?1",
        params![row.workspace_id, now],
    )?;
    Ok(run)
}
pub fn mark_tx(
    c: &Connection,
    id: i64,
    reviewed: bool,
    actor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let file = change_conn(c, id)?;
    c.execute("INSERT INTO workshop_review(workspace_id,change_id,snapshot_id,path,fingerprint,actor,reviewed,created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![file.workspace_id,id,file.snapshot_id,file.path,file.fingerprint,actor,reviewed,now])?;
    // Agent coverage is attributed but can never masquerade as personal review.
    if actor == "human" {
        c.execute("UPDATE workshop_change SET reviewed=?2,needs_recheck=CASE WHEN ?2 THEN 0 ELSE needs_recheck END WHERE id=?1",params![id,reviewed])?;
        let current: Option<i64> = c.query_row(
            "SELECT snapshot_id FROM workshop_workspace WHERE id=?",
            [file.workspace_id],
            |r| r.get(0),
        )?;
        if let Some(current) = current.filter(|current| *current != file.snapshot_id) {
            let matching: i64 = c.query_row(
                "SELECT count(*) FROM workshop_change WHERE snapshot_id=? AND fingerprint=?",
                params![current, file.fingerprint],
                |r| r.get(0),
            )?;
            let source: i64 = c.query_row(
                "SELECT count(*) FROM workshop_change WHERE snapshot_id=? AND fingerprint=?",
                params![file.snapshot_id, file.fingerprint],
                |r| r.get(0),
            )?;
            let source_snapshot = snapshot_conn(c, file.snapshot_id)?;
            let current_snapshot = snapshot_conn(c, current)?;
            let source_conflicts: serde_json::Value =
                serde_json::from_str(&source_snapshot.json).unwrap_or_default();
            let current_conflicts: serde_json::Value =
                serde_json::from_str(&current_snapshot.json).unwrap_or_default();
            let conflict = |v: &serde_json::Value| {
                v["conflicts"]
                    .as_array()
                    .is_some_and(|paths| !paths.is_empty())
            };
            if matching == 1
                && source == 1
                && !conflict(&source_conflicts)
                && !conflict(&current_conflicts)
            {
                c.execute("UPDATE workshop_change SET reviewed=?3,needs_recheck=CASE WHEN ?3 THEN 0 ELSE needs_recheck END WHERE snapshot_id=?1 AND fingerprint=?2",params![current,file.fingerprint,reviewed])?;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCallRow {
    pub id: i64,
    pub chat_id: i64,
    pub run_id: i64,
    pub name: String,
    pub arguments: String,
    pub status: String,
    pub result: String,
    pub error: String,
    pub created: f64,
}
static TOOL_CALLS:Q=Q{id:"workshop tool calls",describe:"app tool results and requests waiting for the person's approval",sql:"SELECT id,chat_id,run_id,name,arguments,status,result,error,created FROM workshop_tool_call WHERE chat_id=? ORDER BY id"};
pub fn tool_calls(s: &Store, id: i64) -> Rc<Vec<ToolCallRow>> {
    s.rows(&TOOL_CALLS, &[Val::I(id)], |r| {
        Ok(ToolCallRow {
            id: r.get(0)?,
            chat_id: r.get(1)?,
            run_id: r.get(2)?,
            name: r.get(3)?,
            arguments: r.get(4)?,
            status: r.get(5)?,
            result: r.get(6)?,
            error: r.get(7)?,
            created: r.get(8)?,
        })
    })
}

/// One line of a run's structured transcript. `kind` is text, reasoning,
/// tool, todo, agent, task, denied or error; `status` is running, background,
/// done, failed, denied, stopped or interrupted. `parent` names the tool use
/// that spawned the subagent an item belongs to, or is empty on the main thread.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ItemRow {
    pub id: i64,
    pub chat_id: i64,
    pub run_id: i64,
    pub key: String,
    pub parent: String,
    pub kind: String,
    pub name: String,
    pub title: String,
    pub input: String,
    pub body: String,
    pub meta: String,
    pub status: String,
    pub created: f64,
    pub updated: f64,
}
static ITEMS:Q=Q{id:"workshop items",describe:"structured transcript: text, tool calls, todo lists, subagents and background tasks",sql:"SELECT id,chat_id,run_id,key,parent,kind,name,title,input,body,meta,status,created,updated FROM workshop_item WHERE chat_id=? ORDER BY id"};
pub fn items(s: &Store, chat: i64) -> Rc<Vec<ItemRow>> {
    s.rows(&ITEMS, &[Val::I(chat)], |r| {
        Ok(ItemRow {
            id: r.get(0)?,
            chat_id: r.get(1)?,
            run_id: r.get(2)?,
            key: r.get(3)?,
            parent: r.get(4)?,
            kind: r.get(5)?,
            name: r.get(6)?,
            title: r.get(7)?,
            input: r.get(8)?,
            body: r.get(9)?,
            meta: r.get(10)?,
            status: r.get(11)?,
            created: r.get(12)?,
            updated: r.get(13)?,
        })
    })
}
/// The newest transcript line of every open chat in a workspace, for the hub's
/// chat rows: (chat id, role, body).
static PREVIEWS:Q=Q{id:"workshop chat previews",describe:"the last message of each chat in a workspace",sql:"SELECT m.chat_id,m.role,m.body FROM workshop_message m WHERE m.id IN (SELECT max(id) FROM workshop_message WHERE chat_id IN (SELECT id FROM workshop_chat WHERE workspace_id=?) GROUP BY chat_id)"};
pub fn chat_previews(s: &Store, workspace: i64) -> Rc<Vec<(i64, String, String)>> {
    s.rows(&PREVIEWS, &[Val::I(workspace)], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
}
/// Every Workshop branch carries this prefix: the placeholder a workspace
/// starts on, and the name it is given after its first message.
pub const BRANCH_PREFIX: &str = "workshop/";
/// What the naming call answers when a message says nothing to name.
pub const NO_NAME: &str = "<no-name>";
/// The branch a new workspace starts on, before it is named after the work.
pub fn placeholder_branch(label: &str) -> String {
    format!("{BRANCH_PREFIX}{label}")
}
/// What a workspace's branch says about the work: the name after the
/// `workshop/` prefix. Empty while the branch is still the placeholder, or
/// is something Workshop did not name.
pub fn describe_branch(label: &str, branch: &str) -> String {
    if branch == placeholder_branch(label) {
        return String::new();
    }
    branch
        .strip_prefix(BRANCH_PREFIX)
        .unwrap_or_default()
        .to_owned()
}
/// The question the naming call asks, over the person's first message. Its
/// rules follow Conductor's: concrete words, short, hyphenated, no prefix,
/// and a sentinel when there is nothing to name.
pub fn naming_prompt(message: &str) -> String {
    let message: String = message.chars().take(4000).collect();
    format!(
        "You are generating a short git branch name for a coding task.\n\
Return only the name. Do not include backticks, explanations, quotes, markdown, or `git branch -m`.\n\
If the message truly does not contain enough information to derive a name (for example, it is a greeting or otherwise contentless), respond with exactly the string {NO_NAME} and nothing else.\n\n\
Requirements:\n\
- Base the name on the user's message\n\
- Use concrete, specific language; avoid abstract nouns\n\
- Keep it concise (under 30 characters when possible)\n\
- Use lowercase words separated by hyphens\n\
- Do not include any prefix\n\n\
User message:\n{message}"
    )
}
/// The name in an answer, as a branch slug: quotes and code fences off, the
/// first line only, any path prefix the model added dropped, lowercase words
/// joined by hyphens, at most sixty characters. `None` for the sentinel or an
/// answer with no letters in it.
pub fn branch_slug(answer: &str) -> Option<String> {
    let line = answer
        .lines()
        .map(|l| l.trim().trim_matches('`').trim_matches(['"', '\'']).trim())
        .find(|l| !l.is_empty())?;
    if line.replace(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '<' | '>' | '-'), "")
        == NO_NAME
    {
        return None;
    }
    let line = line.rsplit('/').next().unwrap_or(line);
    let mut slug = String::new();
    let mut gap = false;
    for c in line.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    if slug.len() > 60 {
        let cut = slug[..60].rfind('-').unwrap_or(60);
        slug.truncate(cut);
    }
    (!slug.is_empty()).then_some(slug)
}
/// A slug as a title: hyphens and underscores to spaces, one capital.
pub fn humanize(slug: &str) -> String {
    let words = slug.replace(['-', '_'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
pub fn pr_value(workspace: &WorkspaceRow) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(&workspace.pr_json)
        .ok()
        .filter(|v| v.get("number").and_then(|n| n.as_u64()).is_some())
}
pub fn pr_number(workspace: &WorkspaceRow) -> Option<u64> {
    pr_value(workspace).and_then(|v| v["number"].as_u64())
}
/// What a workspace is called, as Conductor calls its own: the pull request's
/// title once there is one, else the branch name read as words, else the
/// city label it started with.
pub fn workspace_title(workspace: &WorkspaceRow) -> String {
    if let Some(title) = pr_value(workspace)
        .and_then(|v| v["title"].as_str().map(str::trim).map(str::to_owned))
        .filter(|t| !t.is_empty())
    {
        return title;
    }
    let described = describe_branch(&workspace.label, &workspace.branch);
    if described.is_empty() {
        workspace.label.clone()
    } else {
        humanize(&described)
    }
}
