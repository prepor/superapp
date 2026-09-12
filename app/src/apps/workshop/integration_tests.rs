//! Acceptance-path tests: real Session/store/tools with fake outside capabilities.
use super::{git, model, runtime, snapshots, tools, WORKSHOP};
use kernel::{
    app::App,
    nav::Nav,
    panel::{PanelId, Tag},
    session::{Action, Session},
    tool::Prepared,
};
use serde_json::{json, Value};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

static APPS: &[&dyn App] = &[&WORKSHOP];
fn session() -> Session {
    Session::fake(APPS)
}
fn command(
    s: &mut Session,
    command: runtime::Command,
    preferred: Option<i64>,
    actor: &str,
) -> Result<Value, String> {
    let reply = Rc::new(RefCell::new(None));
    let output = reply.clone();
    // Suppress worker scheduling to inspect the accepted queued operation itself.
    let edit = runtime::command_edit(
        command,
        200.0,
        PathBuf::from("/sample/workshop-tests"),
        preferred,
        actor.into(),
    )
    .wake_if(|_| false);
    s.act_async_result(edit, move |_, result| {
        *output.borrow_mut() = Some(result.map_err(|error| error.to_string()))
    });
    let result = reply.borrow_mut().take().expect("fake write completed");
    result
}
fn tool(s: &mut Session, name: &str, input: Value) -> Result<Value, String> {
    let tool = tools::all()
        .into_iter()
        .find(|tool| tool.name == name)
        .unwrap();
    tool.check(&input)?;
    if let Some(stage) = tool.stager {
        let prepare = stage(s, &input)?;
        let prepared = kernel::runtime::block_on(prepare(s.world()))?;
        let prepared = match prepared {
            Prepared::Edit(edit) => Prepared::Edit(edit.wake_if(|_| false)),
            other => other,
        };
        let reply = Rc::new(RefCell::new(None));
        let output = reply.clone();
        prepared.commit(s, move |_, result| *output.borrow_mut() = Some(result));
        let result = reply
            .borrow_mut()
            .take()
            .expect("fake prepared write completed");
        result
    } else {
        (tool.run)(s, &input)
    }
}
fn panel(tag: &'static str, id: i64) -> PanelId {
    PanelId::new(Tag(tag), [id.to_string()])
}
fn open(s: &mut Session, id: PanelId) -> u64 {
    s.act(Action::new("test.open", "Open").moving(move |wm| {
        wm.open(id, None, false);
    }));
    s.settle();
    s.focus().unwrap()
}

#[test]
fn pr_prompt_uses_preferred_then_recent_then_default_without_crossing_workspace() {
    let mut s = session();
    s.store()
        .write(|c| {
            c.execute("UPDATE workshop_chat SET last_used=500 WHERE id=2", [])?;
            Ok(())
        })
        .unwrap();
    let joined = command(
        &mut s,
        runtime::Command::CreatePr {
            workspace_id: 1,
            draft: false,
        },
        Some(1),
        "human",
    )
    .unwrap();
    assert_eq!(joined["chat_id"], 1);
    let recent = command(
        &mut s,
        runtime::Command::CreatePr {
            workspace_id: 1,
            draft: true,
        },
        None,
        "human",
    )
    .unwrap();
    assert_eq!(recent["chat_id"], 2);
    let isolated = command(
        &mut s,
        runtime::Command::CreatePr {
            workspace_id: 2,
            draft: true,
        },
        Some(1),
        "human",
    )
    .unwrap();
    let chat = isolated["chat_id"].as_i64().unwrap();
    assert_eq!(model::chat(s.store(), chat).unwrap().workspace_id, 2);
    assert_eq!(model::chat(s.store(), chat).unwrap().provider, "codex");
    let messages = model::messages(s.store(), chat);
    assert!(messages
        .last()
        .unwrap()
        .body
        .contains("a draft pull request for branch workshop/oslo"));
    assert_eq!(
        model::workspace(s.store(), 2).unwrap().pr_json,
        "null",
        "queueing a prompt must not invent a PR"
    );
}

#[test]
fn joined_chat_follows_only_the_originating_workspace_chain() {
    let mut s = session();
    let hub = open(&mut s, panel("workshop_workspace", 1));
    s.nav_within(Nav::Open {
        from: hub,
        id: panel("workshop_chat", 1),
        fresh: false,
    });
    s.settle();
    let chat = s.joined_child(hub).unwrap();
    assert_eq!(runtime::joined_chat(&s, hub, 1), Some(1));
    assert_eq!(runtime::joined_chat(&s, chat, 1), Some(1));
    assert_eq!(runtime::joined_chat(&s, chat, 2), None);
    let independent = open(&mut s, panel("workshop_workspace", 2));
    assert_eq!(runtime::joined_chat(&s, independent, 1), None);
}

#[test]
fn workspace_creation_is_immediate_and_uses_configured_default_chat() {
    let mut s = session();
    command(
        &mut s,
        runtime::Command::SaveDefaults {
            provider: "claude".into(),
            model: "sonnet".into(),
        },
        None,
        "human",
    )
    .unwrap();
    let created = command(
        &mut s,
        runtime::Command::NewWorkspace { project_id: 1 },
        None,
        "human",
    )
    .unwrap();
    let wid = created["workspace_id"].as_i64().unwrap();
    let cid = created["chat_id"].as_i64().unwrap();
    let workspace = model::workspace(s.store(), wid).unwrap();
    let chat = model::chat(s.store(), cid).unwrap();
    assert_eq!(workspace.status, "preparing");
    assert_eq!(created["open_workspace"], wid);
    assert_eq!(created["open_chat"], cid);
    assert!(!workspace.label.is_empty());
    assert_eq!(chat.workspace_id, wid);
    assert_eq!(chat.provider, "claude");
    assert_eq!(chat.model, "sonnet");
    assert!(model::messages(s.store(), cid).is_empty());
    let another = command(
        &mut s,
        runtime::Command::NewWorkspace { project_id: 1 },
        None,
        "human",
    )
    .unwrap();
    assert_ne!(
        model::workspace(s.store(), another["workspace_id"].as_i64().unwrap())
            .unwrap()
            .label,
        workspace.label
    );
}

#[test]
fn provider_switch_preserves_started_conversation_and_queued_run_provenance() {
    let mut s = session();
    let queued = command(
        &mut s,
        runtime::Command::Send {
            chat_id: 1,
            text: "queued work".into(),
            mode: "work".into(),
        },
        None,
        "human",
    )
    .unwrap();
    command(
        &mut s,
        runtime::Command::SaveDraft {
            chat_id: 1,
            text: "unsent original".into(),
        },
        None,
        "human",
    )
    .unwrap();
    let changed = command(
        &mut s,
        runtime::Command::SetProvider {
            chat_id: 1,
            provider: "claude".into(),
        },
        None,
        "human",
    )
    .unwrap();
    let next = changed["chat_id"].as_i64().unwrap();
    assert_ne!(next, 1);
    let original = model::chat(s.store(), 1).unwrap();
    assert_eq!(original.provider, "codex");
    assert_eq!(original.draft, "unsent original");
    assert_eq!(model::chat(s.store(), next).unwrap().provider, "claude");
    assert!(model::messages(s.store(), next).is_empty());
    let provenance: (String, String) = s
        .store()
        .conn()
        .query_row(
            "SELECT provider,prompt FROM workshop_run WHERE id=?",
            [queued["run_id"].as_i64().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(provenance, ("codex".into(), "queued work".into()));
}

#[test]
fn empty_provider_switch_preserves_draft_and_resets_opaque_session() {
    let mut s = session();
    s.store().write(|c|{c.execute("UPDATE workshop_chat SET draft='unsent',session_id='old-provider-session' WHERE id=2",[])?;Ok(())}).unwrap();
    let changed = command(
        &mut s,
        runtime::Command::SetProvider {
            chat_id: 2,
            provider: "codex".into(),
        },
        None,
        "human",
    )
    .unwrap();
    assert_eq!(changed["chat_id"], 2);
    let chat = model::chat(s.store(), 2).unwrap();
    assert_eq!(chat.provider, "codex");
    assert_eq!(chat.model, "default");
    assert_eq!(chat.draft, "unsent");
    assert_eq!(chat.session_id, None);
    assert!(command(
        &mut s,
        runtime::Command::SetProvider {
            chat_id: 2,
            provider: "unknown".into()
        },
        None,
        "human"
    )
    .is_err());
}

#[test]
fn agent_tool_marks_never_count_as_human_review_and_reject_extra_scope() {
    let mut s = session();
    let snapshot = model::latest_snapshot(s.store(), 1).unwrap().id;
    let file = model::changes(s.store(), snapshot)
        .iter()
        .find(|file| !file.reviewed)
        .unwrap()
        .clone();
    let before = model::progress(s.store(), snapshot);
    tool(
        &mut s,
        "workshop.review.mark_file",
        json!({"change_id":file.id,"reviewed":true}),
    )
    .unwrap();
    assert_eq!(model::progress(s.store(), snapshot), before);
    let actor: String = s
        .store()
        .conn()
        .query_row(
            "SELECT actor FROM workshop_review WHERE change_id=? ORDER BY id DESC LIMIT 1",
            [file.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(actor, "agent");
    assert!(tool(
        &mut s,
        "workshop.review.mark_file",
        json!({"change_id":file.id,"reviewed":true,"line":1})
    )
    .is_err());
    command(
        &mut s,
        runtime::Command::MarkFile {
            change_id: file.id,
            reviewed: true,
        },
        None,
        "human",
    )
    .unwrap();
    assert_eq!(
        model::progress(s.store(), snapshot).reviewed,
        before.reviewed + file.added + file.deleted
    );
}

#[test]
fn historical_ambiguous_mark_cannot_change_current_personal_coverage() {
    let mut s = session();
    let prior = model::latest_snapshot(s.store(), 1).unwrap();
    let mut snapshot: git::Snapshot = serde_json::from_str(&prior.json).unwrap();
    let mut first = snapshot.files[0].clone();
    first.path = "first.rs".into();
    first.fingerprint = "duplicate-fingerprint".into();
    let mut second = first.clone();
    second.path = "second.rs".into();
    snapshot.files = vec![first.clone(), second];
    snapshot.tree_oid = "historical-tree".into();
    let historical = s
        .store()
        .write(move |c| snapshots::put(c, 1, snapshot, 210.0, true))
        .unwrap();
    let mut current: git::Snapshot = serde_json::from_str(&prior.json).unwrap();
    current.files = vec![first];
    current.tree_oid = "current-tree".into();
    let current = s
        .store()
        .write(move |c| snapshots::put(c, 1, current, 220.0, false))
        .unwrap();
    let source = model::changes(s.store(), historical)[0].id;
    command(
        &mut s,
        runtime::Command::MarkFile {
            change_id: source,
            reviewed: true,
        },
        None,
        "human",
    )
    .unwrap();
    assert_eq!(model::progress(s.store(), current).reviewed, 0);
    assert_eq!(model::progress(s.store(), historical).reviewed, 23);
}

#[test]
fn github_jobs_keep_expected_head_and_personal_review_is_not_a_merge_gate() {
    let mut s = session();
    let snapshot = model::latest_snapshot(s.store(), 1).unwrap().id;
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE workshop_change SET reviewed=0 WHERE snapshot_id=?",
                [snapshot],
            )?;
            Ok(())
        })
        .unwrap();
    let merged = command(
        &mut s,
        runtime::Command::Merge {
            workspace_id: 1,
            method: "squash".into(),
            auto: false,
        },
        None,
        "human",
    )
    .unwrap();
    let job = merged["job_id"].as_i64().unwrap();
    s.store().write(|c|{c.execute("UPDATE workshop_workspace SET pr_json=json_set(pr_json,'$.head','new-head') WHERE id=1",[])?;Ok(())}).unwrap();
    let payload: String = s
        .store()
        .conn()
        .query_row("SELECT payload FROM workshop_job WHERE id=?", [job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["head"],
        "sample-head"
    );
    assert_eq!(model::progress(s.store(), snapshot).reviewed, 0);
}

#[test]
fn per_file_comment_drafts_survive_missing_pr_and_do_not_become_threads() {
    let mut s = session();
    for (path, text) in [
        (Some("a.rs".into()), "first"),
        (Some("b.rs".into()), "second"),
        (None, "general"),
    ] {
        command(
            &mut s,
            runtime::Command::SaveCommentDraft {
                workspace_id: 2,
                path,
                text: text.into(),
            },
            None,
            "human",
        )
        .unwrap();
    }
    assert!(command(
        &mut s,
        runtime::Command::PostComment {
            workspace_id: 2,
            path: Some("a.rs".into()),
            text: "first".into()
        },
        None,
        "human"
    )
    .is_err());
    assert_eq!(model::comment_draft(s.store(), 2, Some("a.rs")), "first");
    assert_eq!(model::comment_draft(s.store(), 2, Some("b.rs")), "second");
    assert_eq!(model::comment_draft(s.store(), 2, None), "general");
    assert_eq!(
        s.store()
            .conn()
            .query_row(
                "SELECT count(*) FROM workshop_job WHERE kind='comment' AND workspace_id=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn canonical_repository_alias_finishes_as_a_visible_duplicate_error() {
    let mut s = session();
    let accepted = command(
        &mut s,
        runtime::Command::AddProject {
            path: "/sample/projects/superapp/alias".into(),
        },
        None,
        "human",
    )
    .unwrap();
    let project = accepted["project_id"].as_i64().unwrap();
    let job = accepted["job_id"].as_i64().unwrap();
    let outcome = runtime::OperationOutcome {
        job,
        workspace: None,
        kind: "add_project".into(),
        request: json!({"project_id":project}),
        result: Ok(
            json!({"project_id":project,"name":"superapp","path":"/sample/projects/superapp","base_ref":"origin/main"}),
        ),
        now: 220.0,
    };
    s.store()
        .write(move |c| runtime::record_operation(c, &outcome))
        .unwrap();
    let (status, error): (String, String) = s
        .store()
        .conn()
        .query_row(
            "SELECT status,error FROM workshop_job WHERE id=?",
            [job],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert!(error.contains("already registered as project 1"));
    assert_eq!(
        model::projects(s.store())
            .iter()
            .find(|p| p.id == project)
            .unwrap()
            .status,
        "failed"
    );
    assert_eq!(
        model::projects(s.store())
            .iter()
            .find(|p| p.id == 1)
            .unwrap()
            .status,
        "ready"
    );
}

#[test]
fn recording_failure_retains_external_result_and_never_requeues_the_operation() {
    let mut s = session();
    let accepted = command(
        &mut s,
        runtime::Command::NewWorkspace { project_id: 1 },
        None,
        "human",
    )
    .unwrap();
    let workspace = accepted["workspace_id"].as_i64().unwrap();
    let job = accepted["job_id"].as_i64().unwrap();
    let row = model::workspace(s.store(), workspace).unwrap();
    s.store().write(move|c|{c.execute("UPDATE workshop_job SET status='running' WHERE id=?",[job])?;c.execute_batch("CREATE TRIGGER fail_workspace_path_update BEFORE UPDATE OF path ON workshop_workspace BEGIN SELECT RAISE(ABORT,'fixture path update failure'); END;")?;Ok(())}).unwrap();
    let outcome = runtime::OperationOutcome {
        job,
        workspace: Some(workspace),
        kind: "create_workspace".into(),
        request: json!({}),
        result: Ok(json!({"path":row.path,"branch":row.branch,"base_ref":row.base_ref})),
        now: 220.0,
    };
    let recording = outcome.clone();
    let error = s
        .store()
        .write(move |c| runtime::record_operation(c, &recording))
        .unwrap_err()
        .to_string();
    assert!(error.contains("fixture path update failure"));
    assert_eq!(
        s.store()
            .conn()
            .query_row("SELECT status FROM workshop_job WHERE id=?", [job], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "running",
        "the failed transaction rolls back its premature done mark"
    );
    s.store()
        .write(move |c| runtime::record_operation_failure(c, &outcome, &error))
        .unwrap();
    let (status, result, error): (String, String, String) = s
        .store()
        .conn()
        .query_row(
            "SELECT status,result,error FROM workshop_job WHERE id=?",
            [job],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "interrupted");
    assert!(serde_json::from_str::<Value>(&result).unwrap()["external_result"]["path"].is_string());
    assert!(error.contains("Check its external result"));
    assert_eq!(
        model::workspace(s.store(), workspace).unwrap().status,
        "failed"
    );
}
