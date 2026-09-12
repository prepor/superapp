//! Agents call the same command transactions as native buttons. Personal review
//! is distinct: tool marks are attributed to the agent, not to the person.
use super::{
    model,
    runtime::{self, Command},
};
use kernel::{
    session::Session,
    tool::{Prepare, Prepared, Tool},
};
use serde_json::{json, Value};
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn integer() -> Value {
    json!({"type":"integer"})
}
fn text() -> Value {
    json!({"type":"string"})
}
fn boolean() -> Value {
    json!({"type":"boolean"})
}
fn stage(s: &mut Session, v: &Value, kind: &str) -> Result<Prepare, String> {
    let mut v = v.clone();
    v["command"] = json!(kind);
    match kind {
        "send" => {
            if v.get("mode").is_none() {
                v["mode"] = json!("work");
            }
        }
        "create_pr" => {
            if v.get("draft").is_none() {
                v["draft"] = json!(false);
            }
        }
        "merge" => {
            if v.get("auto").is_none() {
                v["auto"] = json!(false);
            }
            if v.get("method").is_none() {
                v["method"] = json!("squash");
            }
        }
        "post_comment" | "save_comment_draft" if v.get("path").is_none() => {
            v["path"] = Value::Null;
        }
        _ => {}
    }
    let command: Command = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    let caller =
        super::bridge::caller(s.store()).and_then(|(chat, _)| model::chat(s.store(), chat));
    let from = caller
        .as_ref()
        .and_then(|chat| {
            s.showing(&super::panels::Detail::chat(chat.id))
                .first()
                .copied()
        })
        .or_else(|| s.focus())
        .unwrap_or(0);
    let preferred = v["workspace_id"].as_i64().and_then(|id| {
        caller
            .as_ref()
            .filter(|chat| chat.workspace_id == id)
            .map(|chat| chat.id)
            .or_else(|| runtime::joined_chat(s, from, id))
    });
    let now = s.now();
    let root = s
        .db_dir()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| "/sample/workshop".into());
    Ok(Box::new(move |_| {
        Box::pin(async move {
            Ok(Prepared::Edit(
                runtime::command_edit(command, now, root, preferred, "agent".into()).on_commit(
                    move |reply| {
                        let reply = reply.clone();
                        Box::new(move |s| runtime::navigate(s, from, &reply))
                    },
                ),
            ))
        })
    }))
}
macro_rules! stager {
    ($name:ident,$kind:literal) => {
        fn $name(s: &mut Session, v: &Value) -> Result<Prepare, String> {
            stage(s, v, $kind)
        }
    };
}
stager!(add_project, "add_project");
stager!(new_workspace, "new_workspace");
stager!(new_chat, "new_chat");
stager!(send, "send");
stager!(stop, "stop");
stager!(set_provider, "set_provider");
stager!(set_model, "set_model");
stager!(refresh, "refresh");
stager!(mark, "mark_file");
stager!(create_pr, "create_pr");
stager!(push, "push");
stager!(merge, "merge");
stager!(cancel_auto, "cancel_auto_merge");
stager!(comment, "post_comment");
stager!(ai_review, "ai_review");
stager!(fix, "fix_errors");
stager!(save_defaults, "save_defaults");
stager!(save_draft, "save_draft");
stager!(save_comment_draft, "save_comment_draft");
pub fn all() -> Vec<Tool> {
    let mut tools=vec![
 Tool::new("workshop.projects.list","List local repository records. Workshop data is never device-synced.",schema(json!({}),&[]),false,|s,_|Ok(json!(model::projects(s.store()).as_ref()))),
 Tool::staging("workshop.projects.add","Register a local Git repository. Validation runs in background; inspect workshop_project.status/error or job_id.",schema(json!({"path":text()}),&["path"]),true,add_project),
 Tool::new("workshop.workspaces.list","List workspaces across projects by meaningful recent activity.",schema(json!({}),&[]),false,|s,_|Ok(json!(model::workspaces(s.store()).as_ref()))),
 Tool::staging("workshop.workspaces.create","Create an isolated local worktree with generated city label and initial default chat. Opens both immediately; job_id tracks preparation.",schema(json!({"project_id":integer()}),&["project_id"]),true,new_workspace),
 Tool::new("workshop.chats.list","List untitled chats and their provider/model/status in one workspace.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),false,|s,v|Ok(json!(model::chats(s.store(),v["workspace_id"].as_i64().unwrap()).as_ref()))),
 Tool::new("workshop.chats.read","Read a chat transcript and provider session metadata.",schema(json!({"chat_id":integer()}),&["chat_id"]),false,|s,v|{let id=v["chat_id"].as_i64().unwrap();Ok(json!({"chat":model::chat(s.store(),id),"messages":model::messages(s.store(),id).as_ref()}))}),
 Tool::staging("workshop.chats.start","Start an untitled chat with the configured default provider/model.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),true,new_chat),
 Tool::staging("workshop.chats.send","Queue a prompt for the local subscription harness. work permits sandboxed workspace edits/commands; plan is read-only. Runs in a chat are serialized; different chats may run concurrently.",schema(json!({"chat_id":integer(),"text":text(),"mode":{"type":"string","enum":["work","plan"]}}),&["chat_id","text"]),true,send).asking(),
 Tool::staging("workshop.chats.stop","Stop the current process and cancel queued messages in this chat; retain transcript and step changes.",schema(json!({"chat_id":integer()}),&["chat_id"]),true,stop),
 Tool::staging("workshop.chats.set_provider","Change an empty chat in place; after a user message, opens a NEW chat, retaining original transcript/session.",schema(json!({"chat_id":integer(),"provider":{"type":"string","enum":["codex","claude"]}}),&["chat_id","provider"]),true,set_provider),
 Tool::staging("workshop.chats.set_model","Choose a provider model ID or default in the same idle conversation.",schema(json!({"chat_id":integer(),"model":text()}),&["chat_id","model"]),true,set_model),
 Tool::staging("workshop.workspace.refresh","Refresh local changes and GitHub observations asynchronously.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),true,refresh),
 Tool::new("workshop.review.progress","Read changed-line-weighted human coverage and whole-file statuses for a snapshot. Context lines are excluded.",schema(json!({"snapshot_id":integer()}),&["snapshot_id"]),false,|s,v|{let id=v["snapshot_id"].as_i64().unwrap();Ok(json!({"progress":model::progress(s.store(),id),"files":model::changes(s.store(),id).as_ref()}))}),
 Tool::new("workshop.steps.list","List immutable before/after agent intervals. Overlapping writers share the interval; this is not exclusive authorship.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),false,|s,v|Ok(json!(model::steps(s.store(),v["workspace_id"].as_i64().unwrap()).as_ref()))),
 Tool::new("workshop.steps.diff","Read a stored comparison and its complete file diffs, including historical steps after rebase.",schema(json!({"snapshot_id":integer()}),&["snapshot_id"]),false,|s,v|{let id=v["snapshot_id"].as_i64().unwrap();Ok(json!({"snapshot":model::snapshot(s.store(),id),"files":model::changes(s.store(),id).as_ref()}))}),
 Tool::staging("workshop.review.mark_file","Record an AGENT review of the whole displayed file. Does not count as personal human coverage. Use change_id from a pinned comparison; partial marks do not exist.",schema(json!({"change_id":integer(),"reviewed":boolean()}),&["change_id","reviewed"]),true,mark),
 Tool::staging("workshop.github.create_pr","Send a PR creation prompt to the joined, most recently used, or new default chat in this workspace. The harness prepares title/body and creates the PR; this returns run_id.",schema(json!({"workspace_id":integer(),"draft":boolean()}),&["workspace_id"]),true,create_pr).asking(),
 Tool::staging("workshop.git.push","Push the reviewed expected local commit through Git. Never stages/commits unrelated working-tree changes; inspect job result for dirty state.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),true,push).asking(),
 Tool::staging("workshop.github.merge","Merge or enable GitHub auto-merge with an expected-head guard and repository rules. Personal review percentage is informative, never a merge gate.",schema(json!({"workspace_id":integer(),"method":{"type":"string","enum":["squash","merge","rebase"]},"auto":boolean()}),&["workspace_id"]),true,merge).asking(),
 Tool::staging("workshop.github.cancel_auto_merge","Cancel GitHub auto-merge for the observed PR head.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),true,cancel_auto).asking(),
 Tool::staging("workshop.github.comment","Publish to GitHub directly. path selects a complete published file; omit for general PR comment. Create a draft PR first if none exists. No internal threads.",schema(json!({"workspace_id":integer(),"path":{"type":["string","null"]},"text":text()}),&["workspace_id","text"]),true,comment).asking(),
 Tool::staging("workshop.ai_review","Ask an agent to review the workspace diff without marking human coverage.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),true,ai_review).asking(),
 Tool::staging("workshop.fix_errors","Send current checks/conflict information to a workspace chat for repair.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),true,fix).asking(),
];
    tools.extend([
        Tool::new("workshop.settings.read", "Read the default provider/model and cached local provider availability and models.", schema(json!({}), &[]), false, |s, _| {
            let (provider, model) = model::settings(s.store());
            Ok(json!({"provider":provider,"model":model,"providers":(["codex","claude"].map(|provider|json!({"provider":provider,"status":model::provider_status(s.store(),provider),"models":model::models(s.store(),provider).as_ref()})))}))
        }),
        Tool::staging("workshop.settings.save", "Set the default provider/model for future chats.", schema(json!({"provider":{"type":"string","enum":["codex","claude"]},"model":text()}), &["provider","model"]), true, save_defaults),
        Tool::staging("workshop.chats.save_draft", "Save unsent text in a chat without starting a harness run.", schema(json!({"chat_id":integer(),"text":text()}), &["chat_id","text"]), true, save_draft),
        Tool::staging("workshop.github.save_comment_draft", "Save an unsent GitHub comment. Omit path for a general PR comment. Drafts are never internal discussion threads.", schema(json!({"workspace_id":integer(),"path":{"type":["string","null"]},"text":text()}), &["workspace_id","text"]), true, save_comment_draft),
    ]);
    tools.push(
        Tool::new(
            "workshop.providers.login",
            "Open the local provider subscription sign-in flow in a terminal panel.",
            schema(
                json!({"provider":{"type":"string","enum":["codex","claude"]}}),
                &["provider"],
            ),
            true,
            |s, v| {
                super::terminal::try_login(
                    s,
                    s.focus().unwrap_or(0),
                    v["provider"].as_str().unwrap(),
                )?;
                Ok(json!({"opened":true}))
            },
        )
        .asking(),
    );
    tools.extend(terminal_tools());
    tools
}

fn terminal_tools() -> Vec<Tool> {
    vec![
 Tool::new("workshop.terminal.list","List this workspace's live terminal session keys and placement. Processes are device-local.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),false,|s,v|Ok(json!(super::terminal::list(s.world(),v["workspace_id"].as_i64().unwrap())))),
 Tool::new("workshop.terminal.open_panel","Open the current embedded terminal in an independent panel and immediately start a fresh embedded session. There is no dock-back operation.",schema(json!({"workspace_id":integer()}),&["workspace_id"]),true,|s,v|{let workspace=v["workspace_id"].as_i64().unwrap();super::terminal::try_promote(s,s.focus().unwrap_or(0),workspace)?;Ok(json!(super::terminal::list(s.world(),workspace)))}),
 Tool::new("workshop.terminal.input","Send literal input to one live terminal. Include carriage return to execute a command. This can run shell commands; it is not a clipboard write.",schema(json!({"session_key":text(),"text":text()}),&["session_key","text"]),true,|s,v|{super::terminal::input(s.world(),v["session_key"].as_str().unwrap(),v["text"].as_str().unwrap())?;Ok(json!({"sent":true}))}).asking(),
 Tool::reading("workshop.terminal.read","Read bounded scrollback from one live terminal without changing its view or clipboard.",schema(json!({"session_key":text()}),&["session_key"]),|v|{let key=v["session_key"].as_str().unwrap().to_string();Box::new(move|w|Box::pin(async move{Ok(json!({"text":super::terminal::read(w,&key).await?}))}))}),
 Tool::new("workshop.terminal.close","Explicitly stop a live terminal session. Closing a Workspace view does not stop its independent terminals.",schema(json!({"session_key":text()}),&["session_key"]),true,|s,v|Ok(json!({"closed":super::terminal::close(s.world(),v["session_key"].as_str().unwrap())}))).asking(),
]
}
