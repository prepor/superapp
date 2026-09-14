//! Durable local requests; subprocess work is never performed by a panel draw.
use super::{git, harness, model};
use kernel::{
    app::{Mode, Retirement, Wake, Worker},
    effect::{Job, World},
    layout::SlotId,
    nav::Nav,
    panel::{PanelId, Tag},
    session::{Edit, Session},
    store::{Store, Q},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    ApproveTool {
        call_id: i64,
    },
    RefuseTool {
        call_id: i64,
    },
    AddProject {
        path: String,
    },
    NewWorkspace {
        project_id: i64,
    },
    NewChat {
        workspace_id: i64,
    },
    CloseChat {
        chat_id: i64,
    },
    ReopenChat {
        chat_id: i64,
    },
    ArchiveWorkspace {
        workspace_id: i64,
    },
    RestoreWorkspace {
        workspace_id: i64,
    },
    Send {
        chat_id: i64,
        text: String,
        mode: String,
    },
    SaveDraft {
        chat_id: i64,
        text: String,
    },
    SetProvider {
        chat_id: i64,
        provider: String,
    },
    SetModel {
        chat_id: i64,
        model: String,
    },
    Stop {
        chat_id: i64,
    },
    TouchChat {
        chat_id: i64,
        #[serde(default)]
        viewed_version: Option<i64>,
    },
    Refresh {
        workspace_id: i64,
    },
    MarkFile {
        change_id: i64,
        reviewed: bool,
    },
    CreatePr {
        workspace_id: i64,
        draft: bool,
    },
    AiReview {
        workspace_id: i64,
    },
    FixErrors {
        workspace_id: i64,
    },
    Push {
        workspace_id: i64,
    },
    Merge {
        workspace_id: i64,
        method: String,
        auto: bool,
    },
    CancelAutoMerge {
        workspace_id: i64,
    },
    PostComment {
        workspace_id: i64,
        path: Option<String>,
        text: String,
        #[serde(default)]
        snapshot_id: Option<i64>,
    },
    SaveCommentDraft {
        workspace_id: i64,
        path: Option<String>,
        text: String,
    },
    PromoteTerminal {
        workspace_id: i64,
    },
    Login {
        provider: String,
    },
    SaveDefaults {
        provider: String,
        model: String,
    },
}
pub struct RuntimeMode(pub Mode);
#[derive(Default)]
pub struct Live {
    cancels: Mutex<HashMap<i64, harness::CancelToken>>,
    captures: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}
/// Keep the cancellation entry scoped to the accepted turn, including unwinds.
struct LiveRun {
    live: Arc<Live>,
    chat: i64,
    cancel: harness::CancelToken,
}
impl LiveRun {
    fn new(store: &Store, chat: i64) -> Self {
        let live = store.local::<Live>();
        let cancel = harness::CancelToken::new();
        live.cancels.lock().unwrap().insert(chat, cancel.clone());
        Self { live, chat, cancel }
    }
}
impl Drop for LiveRun {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.live.cancels.lock().unwrap().remove(&self.chat);
    }
}

#[cfg(test)]
#[derive(Default)]
struct TestHarness {
    executable: Mutex<Option<PathBuf>>,
    connection: Mutex<Option<super::bridge::RunConnection>>,
}
fn db_error(text: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(text.into())))
}
fn valid_provider(provider: &str) -> rusqlite::Result<()> {
    harness::Provider::parse(provider)
        .map(|_| ())
        .map_err(db_error)
}
fn id(kind: &'static str, arg: i64) -> PanelId {
    PanelId::new(Tag(kind), [arg.to_string()])
}

/// Prefer a chat in the originating workspace's joined chain. Independent chat
/// panels only win through last_used, never by belonging to another workspace.
pub fn joined_chat(s: &Session, from: SlotId, workspace: i64) -> Option<i64> {
    let hubs = s.showing(&id("workshop_workspace", workspace));
    let chats: Vec<_> = model::chats(s.store(), workspace)
        .iter()
        .flat_map(|chat| {
            s.showing(&id("workshop_chat", chat.id))
                .into_iter()
                .map(move |slot| (slot, chat.id))
        })
        .collect();
    let mut ancestor = Some(from);
    let mut hub = None;
    while let Some(slot) = ancestor {
        if let Some((_, chat)) = chats.iter().find(|(s, _)| *s == slot) {
            return Some(*chat);
        }
        if hubs.contains(&slot) {
            hub = Some(slot);
            break;
        }
        ancestor = s.join_parent_of(slot);
    }
    let mut child = hub.and_then(|slot| s.joined_child(slot));
    while let Some(slot) = child {
        if let Some((_, chat)) = chats.iter().find(|(s, _)| *s == slot) {
            return Some(*chat);
        }
        child = s.joined_child(slot);
    }
    None
}
fn command_workspace(c: &Command) -> Option<i64> {
    match c {
        Command::NewChat { workspace_id }
        | Command::Refresh { workspace_id }
        | Command::CreatePr { workspace_id, .. }
        | Command::AiReview { workspace_id }
        | Command::FixErrors { workspace_id }
        | Command::Push { workspace_id }
        | Command::Merge { workspace_id, .. }
        | Command::CancelAutoMerge { workspace_id }
        | Command::PostComment { workspace_id, .. }
        | Command::SaveCommentDraft { workspace_id, .. }
        | Command::PromoteTerminal { workspace_id } => Some(*workspace_id),
        _ => None,
    }
}

pub fn dispatch(s: &mut Session, from: SlotId, command: Command) {
    // The terminal service owns actual PTYs; this branch never re-creates a moved session.
    if let Command::PromoteTerminal { workspace_id } = command {
        super::terminal::promote(s, from, workspace_id);
        return;
    }
    if let Command::Login { ref provider } = command {
        super::terminal::login(s, from, provider);
        return;
    }
    let preferred = command_workspace(&command).and_then(|wid| joined_chat(s, from, wid));
    let root = s
        .db_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/sample/workshop"));
    let edit =
        command_edit(command, s.now(), root, preferred, "human".into()).on_commit(move |reply| {
            let reply = reply.clone();
            Box::new(move |s| navigate(s, from, &reply))
        });
    s.act_async(edit, |_, _| {});
}
pub fn navigate(s: &mut Session, from: SlotId, reply: &Value) {
    // Both UI commands and agent tools reach here only after their durable
    // cancellation commits. A failed transaction never stops a live process.
    if let Some(chats) = reply["stop_chats"].as_array() {
        for chat in chats.iter().filter_map(Value::as_i64) {
            if let Some(cancel) = s.store().local::<Live>().cancels.lock().unwrap().get(&chat) {
                cancel.cancel();
            }
        }
    }
    if let Some(runs) = reply["stop_runs"].as_array() {
        for run in runs.iter().filter_map(Value::as_i64) {
            super::bridge::unregister_run(s.store(), run);
        }
    }
    if let Some(chats) = reply["close_chats"].as_array() {
        for chat in chats.iter().filter_map(Value::as_i64) {
            for slot in s.showing(&id("workshop_chat", chat)) {
                s.nav_within(Nav::Close { slot, label: None });
            }
        }
    }
    if let Some(workspace) = reply["archived_workspace"].as_i64() {
        super::terminal::close_embedded(s.world(), workspace);
    }

    if let Some(workspace) = reply["open_workspace"].as_i64() {
        let panel = id("workshop_workspace", workspace);
        let showing = s.showing(&panel);
        let existing = if showing.contains(&from) {
            Some(from)
        } else {
            showing.first().copied()
        };
        let slot = if let Some(slot) = existing {
            s.nav_within(Nav::Focus(slot));
            slot
        } else {
            s.nav_within(Nav::Open {
                from,
                id: panel.clone(),
                fresh: false,
            });
            s.showing(&panel).first().copied().unwrap_or(from)
        };
        if let Some(chat) = reply["open_chat"].as_i64() {
            s.nav_within(Nav::Open {
                from: slot,
                id: id("workshop_chat", chat),
                fresh: false,
            });
        }
    } else if let Some(chat) = reply["open_chat"].as_i64() {
        let panel = id("workshop_chat", chat);
        if let Some(slot) = s.showing(&panel).first().copied() {
            s.nav_within(Nav::Focus(slot));
            return;
        }
        let workspace = model::chat(s.store(), chat).map(|c| c.workspace_id);
        let hub =
            workspace.and_then(|wid| s.showing(&id("workshop_workspace", wid)).first().copied());
        let from = hub.unwrap_or_else(|| {
            let samechat = s
                .panel(from)
                .and_then(|p| {
                    p.try_borrow()
                        .ok()
                        .map(|p| p.id().tag == Tag("workshop_chat"))
                })
                .unwrap_or(false);
            if samechat {
                s.join_parent_of(from).unwrap_or(from)
            } else {
                from
            }
        });
        s.nav_within(Nav::Open {
            from,
            id: panel,
            fresh: false,
        });
    } else if reply["open_projects"].as_bool() == Some(true) {
        s.nav_within(Nav::Open {
            from,
            id: PanelId::bare(Tag("workshop_projects")),
            fresh: false,
        });
    }
}
fn resolve_chat(
    c: &Connection,
    workspace: i64,
    preferred: Option<i64>,
    now: f64,
) -> rusqlite::Result<i64> {
    model::active_workspace_conn(c, workspace)?;
    if let Some(chat) = preferred {
        if model::active_chat_conn(c, chat).is_ok_and(|c| c.workspace_id == workspace) {
            return Ok(chat);
        }
    }
    let last=c.query_row("SELECT id FROM workshop_chat WHERE workspace_id=? AND closed=0 ORDER BY last_used DESC,id DESC LIMIT 1",[workspace],|r|r.get(0)).optional()?;
    match last {
        Some(id) => Ok(id),
        None => model::new_chat_tx(c, workspace, None, None, now),
    }
}
pub fn command_edit(
    command: Command,
    now: f64,
    root: PathBuf,
    preferred: Option<i64>,
    actor: String,
) -> Edit<Value> {
    let bookkeeping = matches!(
        command,
        Command::SaveDraft { .. } | Command::SaveCommentDraft { .. } | Command::TouchChat { .. }
    );
    // Restoring a workspace/chat is explicit. Generic undo must never restore
    // cancelled pending runs or external operations for automatic replay.
    let record = !bookkeeping
        && !matches!(
            command,
            Command::Stop { .. }
                | Command::CloseChat { .. }
                | Command::ReopenChat { .. }
                | Command::ArchiveWorkspace { .. }
                | Command::RestoreWorkspace { .. }
        );
    Edit::writing("workshop.action","Workshop",move |c|{
 if let Some(workspace) = command_workspace(&command) {
  if !matches!(command, Command::SaveCommentDraft{..}) { model::active_workspace_conn(c, workspace)?; }
 }
 let result=match command {
  Command::ApproveTool{call_id}=>{let changed=c.execute("UPDATE workshop_tool_call SET status='approved' WHERE id=? AND status='pending' AND run_id IN (SELECT id FROM workshop_run WHERE status='running')",[call_id])?;if changed!=1{return Err(db_error("This request is no longer waiting for approval."));}json!({"call_id":call_id})},
  Command::RefuseTool{call_id}=>{c.execute("UPDATE workshop_tool_call SET status='refused',error='Refused by the person.' WHERE id=? AND status='pending'",[call_id])?;json!({"call_id":call_id})},
  Command::AddProject{path}=>{
   let path=if let Some(rest)=path.trim().strip_prefix("~/"){std::env::var("HOME").map(|home|format!("{home}/{rest}")).unwrap_or(path)}else{path.trim().to_owned()};
   if path.is_empty(){return Err(db_error("Choose a repository folder."));}
   let name=Path::new(&path).file_name().unwrap_or_default().to_string_lossy().into_owned();
   let existing:Option<(i64,String)>=c.query_row("SELECT id,status FROM workshop_project WHERE path=?",[&path],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
   let project=if let Some((project,status))=existing {
    if status!="failed"{return Ok(json!({"project_id":project,"open_projects":true}));}
    c.execute("UPDATE workshop_project SET status='pending',error='' WHERE id=?",[project])?;project
   }else{c.execute("INSERT INTO workshop_project(name,path) VALUES(?1,?2)",params![name,path])?;c.last_insert_rowid()};
   let job=model::queue_tx(c,None,"add_project",json!({"project_id":project,"path":path}),now)?;
   json!({"project_id":project,"job_id":job,"open_projects":true})
  },
  Command::NewWorkspace{project_id}=>{
   let (repo,base,status):(String,String,String)=c.query_row("SELECT path,base_ref,status FROM workshop_project WHERE id=?",[project_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
   if status!="ready"{return Err(db_error("The repository is not ready yet."));}
   let cities=["basel","lucerne","lausanne","geneva","lugano","sion","thun","aarau","zurich","bern"];
   let mut n=0;let label=loop{let city=cities[n%cities.len()];let label=if n<cities.len(){city.to_string()}else{format!("{city}-v{}",n/cities.len()+1)};let count:i64=c.query_row("SELECT count(*) FROM workshop_workspace WHERE label=?",[&label],|r|r.get(0))?;if count==0{break label;}n+=1;};
   let directory=root.join("workshop").join("worktrees").join(project_id.to_string());let path=directory.join(&label).to_string_lossy().into_owned();
   c.execute("INSERT INTO workshop_workspace(project_id,label,path,branch,base_ref,activity) VALUES(?1,?2,?3,?4,?5,?6)",params![project_id,label,path,format!("workshop/{label}"),base,now])?;let workspace=c.last_insert_rowid();let chat=model::new_chat_tx(c,workspace,None,None,now)?;
   let job=model::queue_tx(c,Some(workspace),"create_workspace",json!({"repository":repo,"root":directory,"label":label,"base_ref":base}),now)?;
   json!({"workspace_id":workspace,"chat_id":chat,"job_id":job,"open_workspace":workspace,"open_chat":chat})
  },
  Command::NewChat{workspace_id}=>{model::workspace_conn(c,workspace_id)?;let chat=model::new_chat_tx(c,workspace_id,None,None,now)?;json!({"chat_id":chat,"open_chat":chat})},
  Command::Send{chat_id,text,mode}=>{if text.trim().is_empty(){return Err(db_error("Enter a message."));}
if !["work","plan"].contains(&mode.as_str()){return Err(db_error("Mode must be work or plan."));}let run=model::send_tx(c,chat_id,&text,&mode,now)?;let naming=queue_name_branch(c,chat_id,run,&text,now)?;json!({"run_id":run,"chat_id":chat_id,"naming_job":naming})},
  Command::SaveDraft{chat_id,text}=>{c.execute("UPDATE workshop_chat SET draft=?2,last_used=?3 WHERE id=?1",params![chat_id,text,now])?;json!({"chat_id":chat_id})},
  Command::TouchChat{chat_id,viewed_version}=>{c.execute("UPDATE workshop_chat SET unread=CASE WHEN unread_version=?3 THEN 0 ELSE unread END,last_used=?2 WHERE id=?1",params![chat_id,now,viewed_version])?;json!({"chat_id":chat_id})},
  Command::SetProvider{chat_id,provider}=>{
   valid_provider(&provider)?;let chat=model::active_chat_conn(c,chat_id)?;if matches!(chat.status.as_str(),"running"|"waiting"){return Err(db_error("Stop the running agent before changing provider."));}
   let started:i64=c.query_row("SELECT count(*) FROM workshop_message WHERE chat_id=? AND role='You'",[chat_id],|r|r.get(0))?;
   if chat.provider==provider{json!({"chat_id":chat_id})}else if started>0{let next=model::new_chat_tx(c,chat.workspace_id,Some(&provider),Some("default"),now)?;json!({"chat_id":next,"open_chat":next})}else{c.execute("UPDATE workshop_chat SET provider=?2,model='default',session_id=NULL WHERE id=?1",params![chat_id,provider])?;json!({"chat_id":chat_id})}
  },
  Command::SetModel{chat_id,model}=>{model::active_chat_conn(c,chat_id)?;if model.is_empty()||model.starts_with('-'){return Err(db_error("Enter a provider model ID or default."));}let changed=c.execute("UPDATE workshop_chat SET model=?2 WHERE id=?1 AND status NOT IN ('running','pending')",params![chat_id,model])?;if changed!=1{return Err(db_error("Choose a model when this chat is idle."));}json!({"chat_id":chat_id})},
  Command::Stop{chat_id}=>{model::chat_conn(c,chat_id)?;let runs=stop_chat_tx(c,chat_id,"Stopped by the person.")?;json!({"chat_id":chat_id,"stop_chats":[chat_id],"stop_runs":runs})},
  Command::CloseChat{chat_id}=>{model::chat_conn(c,chat_id)?;let runs=stop_chat_tx(c,chat_id,"Chat closed.")?;c.execute("UPDATE workshop_chat SET closed=1,unread=0 WHERE id=?",[chat_id])?;json!({"chat_id":chat_id,"closed":true,"stop_chats":[chat_id],"stop_runs":runs,"close_chats":[chat_id]})},
  Command::ReopenChat{chat_id}=>{let chat=model::chat_conn(c,chat_id)?;model::active_workspace_conn(c,chat.workspace_id)?;c.execute("UPDATE workshop_chat SET closed=0,last_used=?2 WHERE id=?1",params![chat_id,now])?;json!({"chat_id":chat_id,"closed":false,"open_chat":chat_id})},
  Command::ArchiveWorkspace{workspace_id}=>{
   model::workspace_conn(c,workspace_id)?;
   let chats=c.prepare("SELECT id FROM workshop_chat WHERE workspace_id=?")?.query_map([workspace_id],|r|r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
   let mut runs=Vec::new();for chat in &chats {runs.extend(stop_chat_tx(c,*chat,"Workspace archived.")?);}
   c.execute("UPDATE workshop_workspace SET archived=1,refresh_requested=0 WHERE id=?",[workspace_id])?;
   c.execute("UPDATE workshop_chat SET unread=0 WHERE workspace_id=?",[workspace_id])?;
   c.execute("UPDATE workshop_job SET status='stopped',error='Workspace archived before this operation started.' WHERE workspace_id=? AND status='pending'",[workspace_id])?;
   json!({"workspace_id":workspace_id,"archived":true,"archived_workspace":workspace_id,"stop_chats":chats,"stop_runs":runs,"close_chats":chats})
  },
  Command::RestoreWorkspace{workspace_id}=>{
   model::workspace_conn(c,workspace_id)?;
   c.execute("UPDATE workshop_workspace SET archived=0,refresh_requested=1,activity=?2 WHERE id=?1",params![workspace_id,now])?;
   // Only unfinished worktree preparation resumes with this explicit restore;
   // cancelled prompts and GitHub publications are never replayed.
   c.execute("UPDATE workshop_job SET status='pending',error='' WHERE workspace_id=? AND kind='create_workspace' AND status='stopped'",[workspace_id])?;
   json!({"workspace_id":workspace_id,"archived":false,"open_workspace":workspace_id})
  },
  Command::Refresh{workspace_id}=>{c.execute("UPDATE workshop_workspace SET refresh_requested=1 WHERE id=?",[workspace_id])?;json!({"workspace_id":workspace_id})},
  Command::MarkFile{change_id,reviewed}=>{model::mark_tx(c,change_id,reviewed,&actor,now)?;json!({"change_id":change_id,"actor":actor,"reviewed":reviewed})},
  Command::CreatePr{workspace_id,draft}=>{
   let workspace=model::workspace_conn(c,workspace_id)?;let chat=resolve_chat(c,workspace_id,preferred,now)?;
   let prompt=format!("Create {}pull request for branch {} against {}. Inspect the changes and repository PR template, run relevant checks, commit and push as needed, then create the PR on GitHub using gh. Report its URL.{}",if draft{"a draft "}else{"a "},workspace.branch,workspace.base_ref,if draft{" Keep this PR in draft so I can post review comments before it is ready."}else{""});
   let run=model::send_tx(c,chat,&prompt,"work",now)?;json!({"run_id":run,"chat_id":chat,"open_chat":chat})
  },
  Command::AiReview{workspace_id}=>{
   let workspace=model::workspace_conn(c,workspace_id)?;let chat=resolve_chat(c,workspace_id,preferred,now)?;
   let prompt=format!("Review the complete workspace diff against {}. Find correctness issues and missing tests. Give findings with file:line references. Do not edit files or set personal human review marks.",workspace.base_ref);
   let run=model::send_tx(c,chat,&prompt,"plan",now)?;json!({"run_id":run,"open_chat":chat})
  },
  Command::FixErrors{workspace_id}=>{
   let workspace=model::workspace_conn(c,workspace_id)?;let chat=resolve_chat(c,workspace_id,preferred,now)?;
   let prompt=format!("Inspect and fix failing GitHub checks or merge/rebase conflicts in this workspace. Run relevant validation and explain what changed. Current GitHub state: {}. Current local error: {}",workspace.pr_json,workspace.error);
   let run=model::send_tx(c,chat,&prompt,"work",now)?;json!({"run_id":run,"open_chat":chat})
  },
  Command::Push{workspace_id}=>{let workspace=model::workspace_conn(c,workspace_id)?;let snapshot=model::snapshot_conn(c,workspace.snapshot_id.ok_or_else(||db_error("Refresh changes first."))?)?;let job=model::queue_tx(c,Some(workspace_id),"push",json!({"head":snapshot.head}),now)?;json!({"job_id":job})},
  Command::Merge{workspace_id,method,auto}=>{if !["squash","merge","rebase"].contains(&method.as_str()){return Err(db_error("Unknown merge method."));}let workspace=model::workspace_conn(c,workspace_id)?;let pr:git::GitHubPullRequest=serde_json::from_str(&workspace.pr_json).map_err(|_|db_error("Refresh the pull request first."))?;let job=model::queue_tx(c,Some(workspace_id),"merge",json!({"head":pr.head,"number":pr.number,"method":method,"auto":auto}),now)?;json!({"job_id":job})},
  Command::CancelAutoMerge{workspace_id}=>{let workspace=model::workspace_conn(c,workspace_id)?;let pr:git::GitHubPullRequest=serde_json::from_str(&workspace.pr_json).map_err(|_|db_error("Refresh the pull request first."))?;let job=model::queue_tx(c,Some(workspace_id),"cancel_auto",json!({"head":pr.head,"number":pr.number}),now)?;json!({"job_id":job})},
  Command::SaveCommentDraft{workspace_id,path,text}=>{c.execute("INSERT INTO workshop_comment_draft(workspace_id,path,body,modified) VALUES(?1,?2,?3,?4) ON CONFLICT(workspace_id,path) DO UPDATE SET body=excluded.body,modified=excluded.modified",params![workspace_id,path.unwrap_or_default(),text,now])?;json!({"workspace_id":workspace_id})},
  Command::PostComment{workspace_id,path,text,snapshot_id}=>{
   if text.trim().is_empty(){return Err(db_error("Enter a comment."));}let workspace=model::workspace_conn(c,workspace_id)?;let pr:git::GitHubPullRequest=serde_json::from_str(&workspace.pr_json).map_err(|_|db_error("Create a draft PR before posting this comment."))?;
   let snapshot_id=snapshot_id.or(workspace.snapshot_id);
   let request=json!({"head":pr.head,"number":pr.number,"path":path,"text":text,"snapshot_id":snapshot_id});
   comment_snapshot(c,&request,Some(&workspace)).map_err(db_error)?;
   let job=model::queue_tx(c,Some(workspace_id),"comment",request,now)?;json!({"job_id":job})
  },
  Command::SaveDefaults{provider,model}=>{valid_provider(&provider)?;if model.is_empty()||model.starts_with('-'){return Err(db_error("Enter a model ID or default."));}c.execute("UPDATE workshop_setting SET value=? WHERE key='provider'",[provider])?;c.execute("UPDATE workshop_setting SET value=? WHERE key='model'",[model])?;json!({"saved":true})},
  Command::PromoteTerminal{..}|Command::Login{..}=>return Err(db_error("Open this action in the terminal panel.")),
 };Ok(result)
 }).record_if(move |_|record).wake_if(move |_|!bookkeeping)
}

fn stop_chat_tx(c: &Connection, chat: i64, reason: &str) -> rusqlite::Result<Vec<i64>> {
    let runs = c
        .prepare("SELECT id FROM workshop_run WHERE chat_id=? AND status IN ('pending','running')")?
        .query_map([chat], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    c.execute("UPDATE workshop_run SET status='stopped' WHERE chat_id=? AND status IN ('pending','running')", [chat])?;
    c.execute("UPDATE workshop_tool_call SET status='interrupted',error=?2 WHERE chat_id=?1 AND status IN ('pending','approved','running')", params![chat, reason])?;
    c.execute("UPDATE workshop_chat SET status=CASE WHEN status IN ('running','pending','waiting') THEN 'stopped' ELSE status END WHERE id=?", [chat])?;
    Ok(runs)
}

static JOBS: Q = Q {
    id: "workshop pending operations",
    describe: "accepted local work",
    sql: "SELECT id FROM workshop_job WHERE status IN ('pending','running') LIMIT 1",
};
static ACTIVE_CHATS: Q = Q {
    id: "workshop live chats",
    describe: "one worker per conversation",
    sql: "SELECT DISTINCT r.chat_id FROM workshop_run r JOIN workshop_chat c ON c.id=r.chat_id JOIN workshop_workspace w ON w.id=c.workspace_id WHERE r.status IN ('pending','running') AND c.closed=0 AND w.archived=0",
};
static ACTIVE_WORKSPACES: Q = Q {
    id: "workshop watches",
    describe: "local worktrees",
    sql: "SELECT id FROM workshop_workspace WHERE status='ready' AND archived=0",
};
pub fn workers(s: &Store) -> Vec<Box<dyn Worker>> {
    let mut workers: Vec<Box<dyn Worker>> = vec![];
    workers.push(Box::new(Providers { last: 0.0 }));
    if !s.rows(&JOBS, &[], |r| r.get::<_, i64>(0)).is_empty() {
        workers.push(Box::new(Operations));
    }
    for id in s.rows(&ACTIVE_CHATS, &[], |r| r.get::<_, i64>(0)).iter() {
        workers.push(Box::new(ChatWorker {
            chat: *id,
            retirement: Retirement::default(),
        }));
    }
    for id in s
        .rows(&ACTIVE_WORKSPACES, &[], |r| r.get::<_, i64>(0))
        .iter()
    {
        workers.push(Box::new(Watch {
            workspace: *id,
            pr_at: 0.0,
        }));
    }
    workers
}
fn mode(w: &World) -> Mode {
    w.with_cap::<RuntimeMode, _>(|m| m.0).unwrap_or(Mode::Deny)
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}
struct Operations;
#[async_trait::async_trait(?Send)]
impl Worker for Operations {
    fn name(&self) -> String {
        "workshop-operations".into()
    }
    fn claims(&self, _: &Job) -> bool {
        false
    }
    async fn pass(&mut self, w: &World) -> Wake {
        let job=w.store().conn().query_row("SELECT id,workspace_id,kind,payload FROM workshop_job WHERE status='pending' AND (workspace_id IS NULL OR workspace_id IN (SELECT id FROM workshop_workspace WHERE archived=0)) ORDER BY id LIMIT 1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional().ok().flatten();
        let Some((job, wid, kind, payload)) = job else {
            return Wake::OnKick;
        };
        if !matches!(
            w.store()
                .write_async(move |c| c.execute(
                    "UPDATE workshop_job SET status='running' WHERE id=? AND status='pending' AND (workspace_id IS NULL OR workspace_id IN (SELECT id FROM workshop_workspace WHERE archived=0))",
                    [job]
                ))
                .await,
            Ok(1)
        ) {
            return Wake::OnKick;
        }
        let value: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let workspace = wid.and_then(|id| model::workspace(w.store(), id));
        let snapshot = if kind == "comment" {
            comment_snapshot(w.store().conn(), &value, workspace.as_ref())
        } else {
            Ok(None)
        };
        let result = match snapshot {
            Err(error) => Err(error),
            Ok(_) if mode(w) == Mode::Fake => fake_operation(&kind, &value, workspace.as_ref()),
            Ok(_) if mode(w) == Mode::Deny => Err("Local commands are unavailable in this view.".into()),
            Ok(_) if kind == "name_branch" => name_branch_operation(&value, workspace.as_ref()).await,
            Ok(snapshot) => {
                let kind = kind.clone();
                let value = value.clone();
                blocking(move || operation(&kind, &value, workspace.as_ref(), snapshot.as_ref())).await
            }
        };
        let now = w.now();
        let completed = OperationOutcome {
            job,
            workspace: wid,
            kind,
            request: value,
            result,
            now,
        };
        let recording = completed.clone();
        if let Err(error) = w
            .store()
            .write_async(move |c| record_operation(c, &recording))
            .await
        {
            // The external action has already run. Never put it back into the
            // pending queue when recording its outcome fails.
            let error = error.to_string();
            if let Err(error) = w
                .store()
                .write_async(move |c| record_operation_failure(c, &completed, &error))
                .await
            {
                eprintln!("Workshop could not record operation {job}; its external outcome needs checking: {error}");
            }
        }
        Wake::After(Duration::ZERO)
    }
}

#[derive(Clone)]
pub(super) struct OperationOutcome {
    pub job: i64,
    pub workspace: Option<i64>,
    pub kind: String,
    pub request: Value,
    pub result: Result<Value, String>,
    pub now: f64,
}

/// Called in the writer transaction: a successful observation and all rows
/// describing it become visible together, or none of those rows change.
pub(super) fn record_operation(c: &Connection, outcome: &OperationOutcome) -> rusqlite::Result<()> {
    let OperationOutcome {
        job,
        workspace,
        kind,
        request,
        result,
        now,
    } = outcome;
    let mut result = result.clone();
    if kind == "add_project" {
        if let Ok(value) = &result {
            let duplicate: Option<i64> = c
                .query_row(
                    "SELECT id FROM workshop_project WHERE path=?1 AND id!=?2",
                    params![value["path"].as_str(), value["project_id"].as_i64()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = duplicate {
                result=Err(format!("This repository is already registered as project {id}. Open that project instead."));
            }
        }
    }
    match result {
        Ok(value) => {
            c.execute(
                "UPDATE workshop_job SET status='done',result=?2,error='' WHERE id=?1",
                params![job, value.to_string()],
            )?;
            if kind == "add_project" {
                c.execute("UPDATE workshop_project SET path=?2,name=?3,base_ref=?4,status='ready',error='' WHERE id=?1",params![value["project_id"].as_i64(),value["path"].as_str(),value["name"].as_str(),value["base_ref"].as_str()])?;
            }
            if let (Some(workspace), "name_branch") = (workspace, kind.as_str()) {
                if let (Some(branch), Some(from)) = (value["branch"].as_str(), value["from"].as_str()) {
                    c.execute(
                        "UPDATE workshop_workspace SET branch=?2 WHERE id=?1 AND branch=?3 AND archived=0",
                        params![workspace, branch, from],
                    )?;
                }
            } else if let Some(workspace) = workspace {
                if kind == "create_workspace" {
                    c.execute("UPDATE workshop_workspace SET path=?2,branch=?3,base_ref=?4,status='ready',error='',refresh_requested=CASE WHEN archived=0 THEN 1 ELSE 0 END WHERE id=?1",params![workspace,value["path"].as_str(),value["branch"].as_str(),value["base_ref"].as_str()])?;
                } else {
                    c.execute("UPDATE workshop_workspace SET error='',refresh_requested=1,activity=?2 WHERE id=?1 AND archived=0",params![workspace,now])?;
                }
                if kind == "comment" {
                    c.execute("DELETE FROM workshop_comment_draft WHERE workspace_id=?1 AND body=?2 AND path=COALESCE(?3,'')",params![workspace,value["body"].as_str(),value["path"].as_str()])?;
                }
            }
        }
        Err(error) => {
            c.execute(
                "UPDATE workshop_job SET status='failed',error=?2 WHERE id=?1",
                params![job, error],
            )?;
            if let (Some(workspace), false) = (workspace, kind == "name_branch") {
                c.execute("UPDATE workshop_workspace SET error=?2,status=CASE WHEN status='preparing' THEN 'failed' ELSE status END WHERE id=?1",params![workspace,error])?;
            }
            if kind == "add_project" {
                c.execute(
                    "UPDATE workshop_project SET status='failed',error=?2 WHERE id=?1",
                    params![request["project_id"].as_i64(), error],
                )?;
            }
        }
    }
    Ok(())
}

/// Independent recovery transaction retains the returned outside result and
/// flags uncertainty. Interrupted jobs are never selected for another attempt.
pub(super) fn record_operation_failure(
    c: &Connection,
    outcome: &OperationOutcome,
    error: &str,
) -> rusqlite::Result<()> {
    let detail = match &outcome.result {
        Ok(value) => json!({"external_result":value}),
        Err(error) => json!({"external_error":error}),
    };
    let message=format!("The operation ended, but its result could not be saved: {error}. Check its external result before trying again.");
    c.execute(
        "UPDATE workshop_job SET status='interrupted',result=?2,error=?3 WHERE id=?1",
        params![outcome.job, detail.to_string(), message],
    )?;
    if let Some(workspace) = outcome.workspace {
        c.execute("UPDATE workshop_workspace SET error=?2,status=CASE WHEN status='preparing' THEN 'failed' ELSE status END WHERE id=?1",params![workspace,message])?;
    }
    if outcome.kind == "add_project" {
        c.execute(
            "UPDATE workshop_project SET status='failed',error=?2 WHERE id=?1",
            params![outcome.request["project_id"].as_i64(), message],
        )?;
    }
    Ok(())
}

fn comment_error(error: String) -> String {
    let post = error.starts_with("Post to GitHub:")
        || error.starts_with("Read published GitHub comment:")
        || error.starts_with("GitHub accepted the request");
    let lower = error.to_ascii_lowercase();
    let uncertain = error.starts_with("Read published GitHub comment:")
        || error.starts_with("GitHub accepted the request")
        || [
            "timed out",
            "timeout",
            "connection",
            "eof",
            "broken pipe",
            "reset by peer",
            "context deadline",
            "http 5",
        ]
        .iter()
        .any(|part| lower.contains(part));
    if post && uncertain {
        format!(
            "{error}. GitHub may have received this comment; check the PR before posting it again."
        )
    } else {
        error
    }
}

/// File comments describe the immutable comparison accepted with the request.
/// Reading a newer worktree here would silently change the reviewed file.
pub(super) fn comment_snapshot(
    c: &Connection,
    request: &Value,
    workspace: Option<&model::WorkspaceRow>,
) -> Result<Option<git::Snapshot>, String> {
    let Some(path) = request["path"].as_str() else {
        return Ok(None);
    };
    let workspace = workspace.ok_or("Workspace no longer exists")?;
    let id = request["snapshot_id"].as_i64()
        .ok_or("This file comment has no saved comparison; refresh the file before posting.")?;
    let stored = model::snapshot_conn(c, id)
        .map_err(|_| "The comment's saved comparison is unavailable; refresh the file before posting.")?;
    if stored.workspace_id != workspace.id {
        return Err("The comment's saved comparison belongs to another workspace.".into());
    }
    let snapshot: git::Snapshot = serde_json::from_str(&stored.json)
        .map_err(|_| "The comment's saved comparison is unreadable; refresh the file before posting.")?;
    if !snapshot.files.iter().any(|file| file.path == path) {
        return Err("This file is not in the comment's saved comparison; reopen its diff before posting.".into());
    }
    Ok(Some(snapshot))
}

fn operation(
    kind: &str,
    v: &Value,
    workspace: Option<&model::WorkspaceRow>,
    comment: Option<&git::Snapshot>,
) -> Result<Value, String> {
    let text = |key| v[key].as_str().ok_or_else(|| format!("Missing {key}"));
    if kind == "add_project" {
        let project = git::discover_repository(text("path")?)?;
        let mut value = serde_json::to_value(project).unwrap();
        value["project_id"] = v["project_id"].clone();
        return Ok(value);
    }
    if kind == "create_workspace" {
        return serde_json::to_value(git::create_worktree_named(
            Path::new(text("repository")?),
            Path::new(text("root")?),
            text("label")?,
            text("base_ref")?,
        )?)
        .map_err(|e| e.to_string());
    }
    let workspace = workspace.ok_or("Workspace no longer exists")?;
    let number = v["number"].as_u64().unwrap_or(0);
    let path = Path::new(&workspace.path);
    match kind {
        "push" => serde_json::to_value(git::push(path, text("head")?)?).map_err(|e| e.to_string()),
        "merge" => {
            let method = match text("method")? {
                "merge" => git::MergeMethod::Merge,
                "rebase" => git::MergeMethod::Rebase,
                _ => git::MergeMethod::Squash,
            };
            Ok(
                json!({"result":git::merge_pull_request(path,number,text("head")?,method,v["auto"].as_bool().unwrap_or(false))?}),
            )
        }
        "cancel_auto" => {
            git::cancel_auto_merge(path, number, text("head")?)?;
            Ok(json!({"auto_merge":false}))
        }
        "comment" => {
            let file = v["path"]
                .as_str()
                .map(|path| {
                    comment.ok_or("The comment's saved comparison is unavailable; reopen its diff before posting.")?
                        .files
                        .iter()
                        .find(|f| f.path == path)
                        .ok_or("This file is not in the comment's saved comparison; reopen its diff before posting.")
                })
                .transpose()?;
            serde_json::to_value(
                git::post_comment(path, number, text("head")?, text("text")?, file)
                    .map_err(comment_error)?,
            )
            .map_err(|e| e.to_string())
        }
        _ => Err(format!("Unknown operation {kind}")),
    }
}
pub(super) fn fake_operation(
    kind: &str,
    v: &Value,
    workspace: Option<&model::WorkspaceRow>,
) -> Result<Value, String> {
    match kind {
        "name_branch" => {
            let slug = model::branch_slug(v["message"].as_str().unwrap_or_default())
                .unwrap_or_else(|| "task".into());
            Ok(json!({"branch":format!("{}{slug}", model::BRANCH_PREFIX),"from":v["placeholder"]}))
        }
        "add_project" => Ok(
            json!({"project_id":v["project_id"],"name":Path::new(v["path"].as_str().unwrap_or("repository")).file_name().unwrap_or_default().to_string_lossy(),"path":v["path"],"base_ref":"origin/main"}),
        ),
        "create_workspace" => Ok(
            json!({"path":workspace.unwrap().path,"branch":format!("workshop/{}",v["label"].as_str().unwrap_or("basel")),"base_ref":v["base_ref"]}),
        ),
        "comment" => Ok(
            json!({"id":1,"url":"https://github.com/example/project/pull/1#issuecomment-1","body":v["text"],"path":v["path"]}),
        ),
        _ => Ok(json!({"sample":true})),
    }
}

struct Watch {
    workspace: i64,
    pr_at: f64,
}
#[async_trait::async_trait(?Send)]
impl Worker for Watch {
    fn name(&self) -> String {
        format!("workshop-watch-{}", self.workspace)
    }
    fn claims(&self, _: &Job) -> bool {
        false
    }
    async fn pass(&mut self, w: &World) -> Wake {
        let Some(workspace) = model::workspace(w.store(), self.workspace) else {
            return Wake::OnKick;
        };
        if workspace.archived {
            return Wake::OnKick;
        }
        let now = w.now();
        let id = self.workspace;
        if mode(w) != Mode::Real {
            return Wake::OnKick;
        }
        let result = capture(w, &workspace).await;
        match result {
            Ok(_) => {
                let _ = w
                    .store()
                    .write_async(move |c| {
                        c.execute(
                            "UPDATE workshop_workspace SET git_error='' WHERE id=? AND archived=0",
                            [id],
                        )
                    })
                    .await;
            }
            Err(error) => {
                let _ = w
                    .store()
                    .write_async(move |c| {
                        c.execute(
                            "UPDATE workshop_workspace SET git_error=?2 WHERE id=?1 AND archived=0",
                            params![id, error],
                        )
                    })
                    .await;
            }
        }
        let requested: i64 = w
            .store()
            .conn()
            .query_row(
                "SELECT refresh_requested FROM workshop_workspace WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if model::workspace(w.store(), id).is_none_or(|w| w.archived) {
            return Wake::OnKick;
        }
        if requested != 0 || now - self.pr_at >= 45.0 || self.pr_at == 0.0 {
            let path = workspace.path.clone();
            let result = blocking(move || git::pull_request(path)).await;
            self.pr_at = now;
            let _=w.store().write_async(move|c|{match result{Ok(pr)=>{c.execute("UPDATE workshop_workspace SET pr_json=?2,pr_checked=?3,refresh_requested=0,github_error='' WHERE id=?1 AND archived=0",params![id,serde_json::to_string(&pr).unwrap(),now])?;},Err(error)=>{c.execute("UPDATE workshop_workspace SET github_error=?2,refresh_requested=0 WHERE id=?1 AND archived=0",params![id,error])?;}}Ok(())}).await;
        }
        Wake::After(Duration::from_secs(3))
    }
}
struct ChatWorker {
    chat: i64,
    retirement: Retirement,
}
#[async_trait::async_trait(?Send)]
impl Worker for ChatWorker {
    fn name(&self) -> String {
        format!("workshop-chat-{}", self.chat)
    }
    fn claims(&self, _: &Job) -> bool {
        false
    }
    fn retiring(&mut self, retirement: Retirement) {
        self.retirement = retirement;
    }
    async fn pass(&mut self, w: &World) -> Wake {
        if self.retirement.requested() {
            return Wake::OnKick;
        }
        let chat_id = self.chat;
        let Some(chat) = model::chat(w.store(), chat_id) else {
            return Wake::OnKick;
        };
        let Some(workspace) = model::workspace(w.store(), chat.workspace_id) else {
            return Wake::OnKick;
        };
        if chat.closed || workspace.archived {
            return Wake::OnKick;
        }
        if workspace.status == "preparing" {
            return Wake::After(Duration::from_millis(250));
        }
        let run=w.store().conn().query_row("SELECT id,prompt,mode,provider,model FROM workshop_run WHERE chat_id=? AND status='pending' ORDER BY id LIMIT 1",[chat_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?))).optional().ok().flatten();
        let Some((run, prompt, run_mode, provider, requested_model)) = run else {
            return Wake::OnKick;
        };
        if !matches!(
            w.store()
                .write_async(move |c| {
                    let count = c.execute(
                        "UPDATE workshop_run SET status='running' WHERE id=? AND status='pending' AND chat_id IN (SELECT c.id FROM workshop_chat c JOIN workshop_workspace w ON w.id=c.workspace_id WHERE c.closed=0 AND w.archived=0)",
                        [run],
                    )?;
                    if count == 1 {
                        c.execute(
                            "UPDATE workshop_chat SET status='running',error='' WHERE id=?",
                            [chat_id],
                        )?;
                    }
                    Ok(count)
                })
                .await,
            Ok(1)
        ) {
            return Wake::OnKick;
        }
        let active = LiveRun::new(w.store(), chat_id);
        let cancel = active.cancel.clone();
        let turn = async {
            if workspace.status != "ready" {
                Err("The workspace is not ready. Resolve its preparation error first.".into())
            } else if mode(w) == Mode::Fake {
                fake_run(w, run, &chat, &prompt).await
            } else if mode(w) == Mode::Deny {
                Err("Agents cannot run in this view.".into())
            } else {
                run_agent(
                    w,
                    run,
                    &chat,
                    &workspace,
                    AgentTurn {
                        prompt,
                        mode: run_mode,
                        provider,
                        model: requested_model,
                    },
                    cancel.clone(),
                )
                .await
            }
        };
        tokio::pin!(turn);
        let result = tokio::select! {
            biased;
            // Retirement may already have arrived while claiming the run.
            // Revoke approval waits immediately, then let accepted writes and
            // the provider's final events finish through their usual path.
            () = self.retirement.wait() => {
                cancel.cancel();
                super::bridge::unregister_run(w.store(), run);
                let _ = w.store().write_async(move |c| {
                    c.execute("UPDATE workshop_run SET status='stopped' WHERE id=? AND status='running'", [run])?;
                    c.execute("UPDATE workshop_tool_call SET status='interrupted',error='The agent stopped because its worker retired.' WHERE run_id=? AND status IN ('pending','approved','running')", [run])?;
                    Ok(())
                }).await;
                turn.await
            },
            result = &mut turn => result,
        };
        let now = w.now();
        let wid = workspace.id;
        let stopped = cancel.is_cancelled();
        let _ = w
            .store()
            .write_async(move |c| {
                if stopped {
                    // Keep final outcome and approval cleanup atomic even if
                    // the earlier retirement bookkeeping had a transient error.
                    c.execute("UPDATE workshop_run SET status='stopped' WHERE id=? AND status='running'", [run])?;
                    c.execute("UPDATE workshop_tool_call SET status='interrupted',error='The agent stopped.' WHERE run_id=? AND status IN ('pending','approved','running')", [run])?;
                }
                finish_run(c, run, chat_id, wid, result, now)
            })
            .await;
        Wake::After(Duration::ZERO)
    }
}
/// Recording a late provider result can enrich history, but cannot reopen a
/// closed chat, restore an archive or turn cancelled requests into new work.
pub(super) fn finish_run(
    c: &Connection,
    run: i64,
    chat_id: i64,
    workspace: i64,
    result: Result<(), String>,
    now: f64,
) -> rusqlite::Result<()> {
    let stopped: bool = c.query_row(
        "SELECT status='stopped' FROM workshop_run WHERE id=?",
        [run],
        |r| r.get(0),
    )?;
    let (status, error) = if stopped {
        ("stopped", result.err().unwrap_or_default())
    } else {
        match result {
            Ok(()) => ("done", String::new()),
            Err(error) => ("failed", error),
        }
    };
    c.execute(
        "UPDATE workshop_run SET status=?2,error=?3 WHERE id=?1",
        params![run, status, error],
    )?;
    let pending: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workshop_run WHERE chat_id=? AND status='pending')",
        [chat_id],
        |r| r.get(0),
    )?;
    c.execute(
        "UPDATE workshop_chat SET status=?2,error=?3,unread=1,unread_version=unread_version+1 WHERE id=?1 AND closed=0 AND workspace_id IN (SELECT id FROM workshop_workspace WHERE archived=0)",
        params![chat_id, if pending { "pending" } else if status == "done" { "ready" } else { status }, error],
    )?;
    c.execute(
        "UPDATE workshop_workspace SET activity=?2,refresh_requested=1 WHERE id=?1 AND archived=0",
        params![workspace, now],
    )?;
    Ok(())
}

async fn capture(
    w: &World,
    workspace: &model::WorkspaceRow,
) -> Result<(i64, git::Snapshot), String> {
    let lock = w
        .store()
        .local::<Live>()
        .captures
        .lock()
        .unwrap()
        .entry(workspace.id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = lock.lock().await;
    let path = workspace.path.clone();
    let base = workspace.base_ref.clone();
    let (snapshot, branch) = blocking(move || {
        let snapshot = git::capture_snapshot(&path, &base)?;
        let branch = git::current_branch(&path)?;
        Ok((snapshot, branch))
    })
    .await?;
    let copy = snapshot.clone();
    let id = workspace.id;
    let now = w.now();
    let stored = w
        .store()
        .write_async(move |c| {
            let archived = model::workspace_conn(c, id)?.archived;
            let snapshot = super::snapshots::put(c, id, copy, now, archived)?;
            c.execute(
                "UPDATE workshop_workspace SET branch=?2 WHERE id=?1 AND branch!=?2",
                params![id, branch],
            )?;
            Ok(snapshot)
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok((stored, snapshot))
}
struct AgentTurn {
    prompt: String,
    mode: String,
    provider: String,
    model: String,
}
async fn run_agent(
    w: &World,
    run: i64,
    chat: &model::ChatRow,
    workspace: &model::WorkspaceRow,
    turn: AgentTurn,
    cancel: harness::CancelToken,
) -> Result<(), String> {
    let AgentTurn {
        prompt,
        mode,
        provider,
        model,
    } = turn;
    if cancel.is_cancelled() {
        return Ok(());
    }
    let (before_id, before) = capture(w, workspace).await?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    let chat_id = chat.id;
    let now = w.now();
    let role = if provider == "claude" {
        "Claude Code"
    } else {
        "Codex"
    };
    let message=w.store().write_async(move|c|{c.execute("INSERT INTO workshop_message(chat_id,run_id,role,body,created) VALUES(?1,?2,?3,'',?4)",params![chat_id,run,role,now])?;let id=c.last_insert_rowid();c.execute("UPDATE workshop_run SET before_id=?2,message_id=?3 WHERE id=?1",params![run,before_id,id])?;Ok(id)}).await.map_err(|e|e.to_string())?;
    // Hold the guard before the async registration: retirement can arrive
    // while its server is starting, before a token exists to revoke.
    let _tool_guard = super::bridge::guard_run(w.store(), run);
    if cancel.is_cancelled() {
        return Ok(());
    }
    let connection = super::bridge::register_run(w, chat_id, run).await?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    #[cfg(test)]
    let test_executable = {
        let test = w.store().local::<TestHarness>();
        let executable = test.executable.lock().unwrap().clone();
        if executable.is_some() {
            *test.connection.lock().unwrap() = Some(connection.clone());
        }
        executable
    };
    let request = harness::RunRequest {
        mcp: Some(harness::McpConfig {
            url: connection.url,
            bearer_token: connection.bearer_token,
        }),
        provider: harness::Provider::parse(&provider)?,
        model,
        cwd: PathBuf::from(&workspace.path),
        prompt,
        resume_session_id: chat.session_id.clone(),
        permission_mode: if mode == "plan" {
            harness::PermissionMode::ReadOnly
        } else {
            harness::PermissionMode::WorkspaceWrite
        },
    };
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let emit = move |event| {
        let _ = send.send(event);
    };
    let provider_cancel = cancel.clone();
    let mut future = Box::pin(async move {
        #[cfg(test)]
        if let Some(executable) = test_executable {
            return harness::run_with_executable(request, executable, provider_cancel, emit).await;
        }
        harness::run(request, provider_cancel, emit).await
    });
    // Streamed events land in one transaction per tick, never one per token:
    // every commit redraws the whole app, so the writer sets the frame rate.
    let mut pending = Pending::default();
    let mut outcome = None;
    let mut stop_check = tokio::time::interval(Duration::from_millis(150));
    let mut flush = tokio::time::interval(FLUSH_EVERY);
    let mut stopping = false;
    let teardown = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(teardown);
    loop {
        tokio::select! {
         result=&mut future,if outcome.is_none()=>{outcome=Some(result);},
         ()=cancel.cancelled(),if !stopping && outcome.is_none()=>{
          stopping=true;
          super::bridge::unregister_run(w.store(),run);
          teardown.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(5));
         },
         ()=&mut teardown,if stopping && outcome.is_none()=>{
          outcome=Some(Err("The stopped provider did not finish shutting down.".into()));
         },
         event=receive.recv()=>{if let Some(event)=event{
          pending.absorb(event);
         }else if outcome.is_some(){break;}},
         _=flush.tick(),if pending.dirty=>{
          let batch=pending.take(w.now());
          if let Err(error)=w.store().write_async(move|c|batch.write(c,chat_id,run,message)).await{eprintln!("workshop: could not save streamed transcript for run {run}: {error}");}
         },
         _=stop_check.tick()=>{let stopped:bool=w.store().conn().query_row("SELECT status='stopped' FROM workshop_run WHERE id=?",[run],|r|r.get(0)).unwrap_or(true);if stopped{cancel.cancel();}}
        }
        if outcome.is_some() && receive.is_empty() {
            break;
        }
    }
    // A stalled native wait cannot retain the provider or inherited pipes.
    // Its drop guard kills the process group; accepted events above and the
    // snapshot/outcome writes below still finish before the worker is joined.
    drop(future);
    super::bridge::unregister_run(w.store(), run);
    let result = outcome.unwrap_or_else(|| Err("The provider stream ended unexpectedly.".into()));
    let run_status = match &result {
        _ if cancel.is_cancelled() => "stopped",
        Ok(_) => "done",
        Err(_) => "failed",
    };
    // What the provider left running has no result coming: say so on its card.
    let now = w.now();
    let batch = pending.take(now);
    let unfinished = if run_status == "stopped" { "stopped" } else { "interrupted" };
    let _ = w
        .store()
        .write_async(move |c| {
            batch.write(c, chat_id, run, message)?;
            c.execute("UPDATE workshop_item SET status=?2,updated=?3 WHERE run_id=?1 AND status IN ('running','background')", params![run, unfinished, now])?;
            c.execute(
                "UPDATE workshop_item SET status='done' WHERE run_id=?1 AND kind='text' AND status='running'",
                [run],
            )?;
            Ok(())
        })
        .await;
    let (after_id, after) = capture(w, workspace)
        .await
        .map_err(|e| format!("The turn ended, but its changes could not be captured: {e}"))?;
    let path = workspace.path.clone();
    let diff = blocking(move || git::compare_snapshots(path, &before, &after))
        .await
        .map_err(|e| format!("Could not compare the turn's before/after snapshots: {e}"))?;
    let wid = workspace.id;
    let status = run_status.to_string();
    let now = w.now();
    w.store().write_async(move|c|{let diff_id=super::snapshots::put(c,wid,diff,now,true)?;c.execute("INSERT INTO workshop_step(workspace_id,chat_id,before_id,after_id,diff_id,status,created) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![wid,chat_id,before_id,after_id,diff_id,status,now])?;let step=c.last_insert_rowid();c.execute("UPDATE workshop_message SET step_id=?2 WHERE id=?1",params![message,step])?;c.execute("UPDATE workshop_run SET after_id=?2 WHERE id=?1",params![run,after_id])?;Ok(())}).await.map_err(|e|format!("Could not save the turn's change preview: {e}"))?;
    match result {
        Ok(outcome) => {
            let session = outcome.session_id;
            let usage = outcome.usage.to_string();
            let cancelled = outcome.cancelled;
            let _=w.store().write_async(move|c|{if let Some(session)=session{c.execute("UPDATE workshop_chat SET session_id=?2 WHERE id=?1",params![chat_id,session])?;}c.execute("UPDATE workshop_run SET usage=?2,status=CASE WHEN ?3 THEN 'stopped' ELSE status END WHERE id=?1",params![run,usage,cancelled])?;Ok(())}).await;
            Ok(())
        }
        Err(error) => Err(error),
    }
}
/// How often a streaming run commits what it has received. Every commit
/// redraws the panels that read the transcript, so this is the frame rate a
/// long answer draws at, not a latency the person can feel.
const FLUSH_EVERY: Duration = Duration::from_millis(120);

/// What a run has received from the harness and not yet written. Text is
/// kept by message: the finished text of a message and the words streaming
/// after it, so a Claude message that says something, calls a tool and goes
/// on speaking stays one segment.
#[derive(Default)]
pub(super) struct Pending {
    session: Option<String>,
    model: Option<String>,
    text: Vec<(String, String, String)>,
    items: Vec<harness::Item>,
    pub(super) dirty: bool,
}
impl Pending {
    pub(super) fn absorb(&mut self, event: harness::HarnessEvent) {
        if let Some(session) = event.session_id {
            self.session = Some(session);
        }
        if let Some(model) = event.model {
            self.model = Some(model);
        }
        match event.kind.as_str() {
            "assistant_delta" | "assistant" => {
                let key = event.data["id"].as_str().unwrap_or("assistant").to_owned();
                let done = event.kind == "assistant";
                let at = match self.text.iter().position(|(k, _, _)| *k == key) {
                    Some(at) => at,
                    None => {
                        self.text.push((key.clone(), String::new(), String::new()));
                        self.text.len() - 1
                    }
                };
                let entry = &mut self.text[at];
                if done {
                    entry.1 = event.text;
                    entry.2.clear();
                } else {
                    entry.2.push_str(&event.text);
                }
                let body = match (entry.1.is_empty(), entry.2.is_empty()) {
                    (_, true) => entry.1.clone(),
                    (true, false) => entry.2.clone(),
                    (false, false) => format!("{}\n\n{}", entry.1, entry.2),
                };
                let mut item = harness::Item {
                    key,
                    kind: "text".into(),
                    body: Some(body),
                    status: if done { "done" } else { "running" }.into(),
                    ..Default::default()
                };
                item.parent.clear();
                self.merge(item);
            }
            "item" => {
                if let Some(item) = event.item {
                    self.merge(item);
                }
            }
            "error" => {
                let item = harness::Item {
                    key: format!("error-{}", self.items.len() + 1),
                    kind: "error".into(),
                    body: Some(event.text),
                    status: "failed".into(),
                    ..Default::default()
                };
                self.merge(item);
            }
            _ => return,
        }
        self.dirty = true;
    }
    /// A later event for the same key lands on the earlier one: fields it
    /// says replace, fields it leaves out stay.
    fn merge(&mut self, item: harness::Item) {
        if let Some(into) = self.items.iter_mut().find(|i| i.key == item.key) {
            if !item.parent.is_empty() {
                into.parent = item.parent;
            }
            if !item.kind.is_empty() {
                into.kind = item.kind;
            }
            if !item.name.is_empty() {
                into.name = item.name;
            }
            if !item.title.is_empty() {
                into.title = item.title;
            }
            if item.input.is_some() {
                into.input = item.input;
            }
            if item.body.is_some() {
                into.body = item.body;
            }
            if let Some(meta) = item.meta {
                into.meta = Some(match into.meta.take() {
                    Some(mut have) => {
                        json_merge(&mut have, meta);
                        have
                    }
                    None => meta,
                });
            }
            if !item.status.is_empty() {
                into.status = item.status;
            }
        } else {
            self.items.push(item);
        }
    }
    pub(super) fn take(&mut self, now: f64) -> Batch {
        self.dirty = false;
        Batch {
            now,
            session: self.session.take(),
            model: self.model.take(),
            body: (!self.text.is_empty()).then(|| {
                self.text
                    .iter()
                    .map(|(_, complete, live)| match (complete.is_empty(), live.is_empty()) {
                        (_, true) => complete.clone(),
                        (true, false) => live.clone(),
                        (false, false) => format!("{complete}\n\n{live}"),
                    })
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }),
            items: std::mem::take(&mut self.items),
        }
    }
}
fn json_merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(into), Value::Object(from)) => {
            for (key, value) in from {
                match into.get_mut(&key) {
                    Some(have) if have.is_object() && value.is_object() => json_merge(have, value),
                    _ => {
                        into.insert(key, value);
                    }
                }
            }
        }
        (into, from) => *into = from,
    }
}
/// One tick's writes, in one transaction.
pub(super) struct Batch {
    now: f64,
    session: Option<String>,
    model: Option<String>,
    body: Option<String>,
    items: Vec<harness::Item>,
}
impl Batch {
    pub(super) fn write(self, c: &Connection, chat: i64, run: i64, message: i64) -> rusqlite::Result<()> {
        if let Some(session) = self.session {
            c.execute(
                "UPDATE workshop_chat SET session_id=?2 WHERE id=?1",
                params![chat, session],
            )?;
        }
        if let Some(model) = self.model {
            c.execute(
                "UPDATE workshop_run SET model=?2 WHERE id=?1",
                params![run, model],
            )?;
        }
        if let Some(body) = self.body {
            c.execute(
                "UPDATE workshop_message SET body=?2 WHERE id=?1",
                params![message, body],
            )?;
        }
        for item in self.items {
            upsert_item(c, chat, run, &item, self.now)?;
        }
        Ok(())
    }
}
/// An item lands where its key already is, or at the end of the transcript.
pub(super) fn upsert_item(
    c: &Connection,
    chat: i64,
    run: i64,
    item: &harness::Item,
    now: f64,
) -> rusqlite::Result<()> {
    let input = item.input.as_ref().map(Value::to_string);
    let meta = item.meta.as_ref().map(Value::to_string);
    c.execute(
        "INSERT INTO workshop_item(chat_id,run_id,key,parent,kind,name,title,input,body,meta,status,created,updated) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,COALESCE(?8,''),COALESCE(?9,''),COALESCE(?10,''),CASE WHEN ?11='' THEN 'running' ELSE ?11 END,?12,?12) \
         ON CONFLICT(run_id,key) DO UPDATE SET \
           parent=CASE WHEN excluded.parent!='' THEN excluded.parent ELSE workshop_item.parent END, \
           kind=CASE WHEN excluded.kind!='' THEN excluded.kind ELSE workshop_item.kind END, \
           name=CASE WHEN excluded.name!='' THEN excluded.name ELSE workshop_item.name END, \
           title=CASE WHEN excluded.title!='' THEN excluded.title ELSE workshop_item.title END, \
           input=COALESCE(?8,workshop_item.input), body=COALESCE(?9,workshop_item.body), \
           meta=CASE WHEN ?10 IS NULL THEN workshop_item.meta WHEN workshop_item.meta='' THEN ?10 ELSE json_patch(workshop_item.meta,?10) END, \
           status=CASE WHEN ?11!='' THEN ?11 ELSE workshop_item.status END, updated=?12",
        params![
            chat,
            run,
            item.key,
            item.parent,
            item.kind,
            item.name,
            item.title,
            input,
            item.body,
            meta,
            item.status,
            now
        ],
    )?;
    Ok(())
}
/// A message sent while the workspace is still on its placeholder branch,
/// with no pull request, queues one naming job: a separate one-shot call
/// that reads the message and names the branch, the way Conductor names its
/// workspaces in the background. One at a time per workspace; a message that
/// yielded no name lets the next one try.
fn queue_name_branch(
    c: &Connection,
    chat: i64,
    run: i64,
    text: &str,
    now: f64,
) -> rusqlite::Result<Option<i64>> {
    let chat = model::chat_conn(c, chat)?;
    let workspace = model::workspace_conn(c, chat.workspace_id)?;
    if workspace.branch != model::placeholder_branch(&workspace.label)
        || model::pr_number(&workspace).is_some()
    {
        return Ok(None);
    }
    let live: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workshop_job WHERE workspace_id=?1 AND kind='name_branch' AND status IN ('pending','running'))",
        [workspace.id],
        |r| r.get(0),
    )?;
    if live {
        return Ok(None);
    }
    let payload = json!({
        "chat_id": chat.id, "run_id": run, "message": text,
        "provider": chat.provider, "model": chat.model, "placeholder": workspace.branch
    });
    model::queue_tx(c, Some(workspace.id), "name_branch", payload, now).map(Some)
}
/// The naming call, in earnest: the same harness as the chat, one question,
/// read-only, no app tools. Guards repeat the queue's, against the worktree
/// itself, so a branch the person or an agent already renamed is left alone.
async fn name_branch_operation(
    v: &Value,
    workspace: Option<&model::WorkspaceRow>,
) -> Result<Value, String> {
    let workspace = workspace.ok_or("Workspace no longer exists")?;
    let placeholder = model::placeholder_branch(&workspace.label);
    if workspace.status != "ready" {
        return Ok(json!({"skipped":"the workspace is not ready"}));
    }
    if model::pr_number(workspace).is_some() {
        return Ok(json!({"skipped":"a pull request exists"}));
    }
    let path = PathBuf::from(&workspace.path);
    let checked = path.clone();
    let current = blocking(move || git::current_branch(&checked)).await?;
    if current != placeholder {
        return Ok(json!({"skipped":"the branch is already named","branch":current}));
    }
    let provider = harness::Provider::parse(v["provider"].as_str().unwrap_or("codex"))?;
    let message = v["message"].as_str().unwrap_or_default();
    if message.trim().is_empty() {
        return Ok(json!({"skipped":"an empty message"}));
    }
    let request = harness::RunRequest {
        provider,
        model: v["model"].as_str().unwrap_or("default").to_owned(),
        cwd: path.clone(),
        prompt: model::naming_prompt(message),
        resume_session_id: None,
        permission_mode: harness::PermissionMode::ReadOnly,
        mcp: None,
    };
    let cancel = harness::CancelToken::new();
    let answer = match tokio::time::timeout(
        Duration::from_secs(120),
        harness::ask(request, cancel.clone()),
    )
    .await
    {
        Ok(answer) => answer?,
        Err(_) => {
            cancel.cancel();
            return Err("The naming call did not answer in time".into());
        }
    };
    let Some(slug) = model::branch_slug(&answer) else {
        return Ok(json!({"skipped":"no name suggested","answer":answer.chars().take(200).collect::<String>()}));
    };
    let wanted = format!("{}{slug}", model::BRANCH_PREFIX);
    if wanted == placeholder {
        return Ok(json!({"skipped":"the suggestion is the placeholder"}));
    }
    // Another workspace may already hold this name: the next free -vN.
    let repo = path.clone();
    let base = wanted.clone();
    let name = blocking(move || {
        let mut candidate = base.clone();
        let mut n = 2;
        while git::branch_exists(&repo, &candidate) {
            if n > 50 {
                return Err(format!("Too many branches named {base}"));
            }
            candidate = format!("{base}-v{n}");
            n += 1;
        }
        Ok(candidate)
    })
    .await?;
    let from = placeholder.clone();
    let renamed = blocking(move || git::rename_branch(&path, &from, &name)).await?;
    Ok(json!({"branch":renamed,"from":placeholder}))
}
async fn fake_run(w: &World, run: i64, chat: &model::ChatRow, prompt: &str) -> Result<(), String> {
    let id = chat.id;
    let wid = chat.workspace_id;
    let provider = chat.provider.clone();
    let prompt = prompt.to_string();
    let now = w.now();
    w.store().write_async(move|c|{let active:bool=c.query_row("SELECT status='running' FROM workshop_run WHERE id=?",[run],|r|r.get(0))?;if !active{return Ok(());}
c.execute("INSERT INTO workshop_message(chat_id,run_id,role,body,created) VALUES(?1,?2,?3,?4,?5)",params![id,run,if provider=="claude"{"Claude Code"}else{"Codex"},format!("Sample reply: {prompt}"),now])?;
let sample_call=harness::Item{key:"sample-shell".into(),kind:"tool".into(),name:"shell".into(),title:"git status --short".into(),input:Some(json!({"command":"git status --short"})),body:Some(" M src/review/anchors.rs".into()),status:"done".into(),..Default::default()};
upsert_item(c,id,run,&sample_call,now)?;
let sample_text=harness::Item{key:"sample-text".into(),kind:"text".into(),body:Some(format!("Sample reply: {prompt}")),status:"done".into(),..Default::default()};
upsert_item(c,id,run,&sample_text,now+0.001)?;
if prompt.starts_with("Create ")&&prompt.contains("pull request"){
 let pr=json!({"number":148,"url":"https://github.com/example/project/pull/148","title":"Workspace changes","state":"OPEN","draft":prompt.contains("draft"),"head":"sample-head","head_branch":"workshop/zurich","base_branch":"main","mergeable":"MERGEABLE","merge_state":"CLEAN","review_decision":"APPROVED","auto_merge":false,"checks":[],"observed_at":0});c.execute("UPDATE workshop_workspace SET pr_json=?2 WHERE id=?1",params![wid,pr.to_string()])?;}Ok(())}).await.map_err(|e|e.to_string())
}

static PROVIDER_CACHE: Q = Q {
    id: "workshop provider discovery",
    describe: "local CLI availability",
    sql: "SELECT count(*) FROM workshop_setting WHERE key LIKE 'provider_status:%'",
};
struct Providers {
    last: f64,
}

#[async_trait::async_trait(?Send)]
impl Worker for Providers {
    fn name(&self) -> String {
        "workshop-providers".into()
    }
    fn claims(&self, _: &Job) -> bool {
        false
    }
    async fn pass(&mut self, w: &World) -> Wake {
        let now = w.now();
        if mode(w) != Mode::Real
            && w.store()
                .rows(&PROVIDER_CACHE, &[], |r| r.get::<_, i64>(0))
                .first()
                .copied()
                .unwrap_or(0)
                >= 2
        {
            return Wake::OnKick;
        }
        if self.last > 0.0 && now - self.last < 60.0 {
            return Wake::After(Duration::from_secs(30));
        }
        self.last = now;
        for provider in [harness::Provider::Codex, harness::Provider::Claude] {
            let (models, status) = if mode(w) == Mode::Real {
                let info = blocking(move || Ok(harness::provider_info(provider))).await;
                match info {
                    Ok(info) => {
                        let status = if info.executable.is_some() {
                            harness::authentication_status(provider)
                                .await
                                .unwrap_or_else(|e| e)
                        } else {
                            format!(
                                "{} CLI not found. Install it or use Conductor's bundled CLI.",
                                provider.as_str()
                            )
                        };
                        (info.models, status)
                    }
                    Err(error) => (vec!["default".into()], error),
                }
            } else {
                (
                    vec!["default".into()],
                    "sample provider · no process started".into(),
                )
            };
            let key = provider.as_str();
            let _=w.store().write_async(move|c|{for (key,value) in [(format!("models:{key}"),serde_json::to_string(&models).unwrap()),(format!("provider_status:{key}"),status)]{c.execute("INSERT INTO workshop_setting(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;}Ok(())}).await;
        }
        if mode(w) == Mode::Real {
            Wake::After(Duration::from_secs(60))
        } else {
            Wake::OnKick
        }
    }
}

#[cfg(all(test, unix))]
#[path = "shutdown_tests.rs"]
mod shutdown_tests;
