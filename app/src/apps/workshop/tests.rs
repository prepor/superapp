use super::*;
use kernel::{
    app::{App, Apps, Mode},
    nav::Nav,
    panel::{PanelId, Tag},
    session::Session,
    store::Store,
};
use serde_json::{json, Value};
static APPS: &[&dyn App] = &[&WORKSHOP];
#[path = "integration_tests.rs"]
mod integration;
fn apply(s: &mut Session, command: runtime::Command) -> Value {
    let out = std::rc::Rc::new(std::cell::RefCell::new(None));
    let done = out.clone();
    s.act_async(
        runtime::command_edit(
            command,
            s.now(),
            "/sample/store".into(),
            None,
            "human".into(),
        )
        .wake_if(|_| false),
        move |_, value| *done.borrow_mut() = value,
    );
    let value = out.borrow_mut().take().expect("fake commits synchronously");
    value
}
#[test]
fn real_install_is_empty_and_no_workshop_rows_replicate() {
    let apps = Apps::new(APPS);
    assert!(WORKSHOP.replicated().is_empty());
    assert!(apps
        .replicated()
        .iter()
        .all(|r| !r.table.starts_with("workshop_")));
    let store = Store::open(
        None,
        &apps.schemas(),
        kernel::sync::Device::fake().replicating(apps.replicated()),
    )
    .unwrap();
    apps.seed(&store, Mode::Real).unwrap();
    assert!(model::projects(&store).is_empty());
    apps.seed(&store, Mode::Fake).unwrap();
    assert!(!model::workspaces(&store).is_empty());
    let count: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM sync_op WHERE tbl LIKE 'workshop_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn creation_is_immediate_untitled_and_uses_defaults() {
    let mut s = Session::fake(APPS);
    apply(
        &mut s,
        runtime::Command::SaveDefaults {
            provider: "claude".into(),
            model: "sonnet".into(),
        },
    );
    let created = apply(&mut s, runtime::Command::NewWorkspace { project_id: 1 });
    let wid = created["workspace_id"].as_i64().unwrap();
    let cid = created["chat_id"].as_i64().unwrap();
    let workspace = model::workspace(s.store(), wid).unwrap();
    assert_eq!(workspace.label, "basel");
    assert_eq!(workspace.status, "preparing");
    let chat = model::chat(s.store(), cid).unwrap();
    assert_eq!(chat.provider, "claude");
    assert_eq!(chat.model, "sonnet");
    assert!(model::messages(s.store(), cid).is_empty());
    let second = apply(&mut s, runtime::Command::NewWorkspace { project_id: 1 });
    assert_ne!(
        model::workspace(s.store(), second["workspace_id"].as_i64().unwrap())
            .unwrap()
            .label,
        workspace.label
    );
    let columns: Vec<String> = s
        .store()
        .conn()
        .prepare("SELECT name FROM pragma_table_info('workshop_chat')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!columns.iter().any(|c| c == "title"));
}
#[test]
fn provider_switch_preserves_started_chats_and_changes_empty_ones() {
    let mut s = Session::fake(APPS);
    let original = model::chat(s.store(), 1).unwrap();
    let next = apply(
        &mut s,
        runtime::Command::SetProvider {
            chat_id: 1,
            provider: "claude".into(),
        },
    )["chat_id"]
        .as_i64()
        .unwrap();
    assert_ne!(next, 1);
    assert_eq!(
        model::chat(s.store(), 1).unwrap().provider,
        original.provider
    );
    assert_eq!(model::messages(s.store(), 1).len(), 2);
    let same = apply(
        &mut s,
        runtime::Command::SetProvider {
            chat_id: next,
            provider: "codex".into(),
        },
    )["chat_id"]
        .as_i64()
        .unwrap();
    assert_eq!(same, next);
    assert_eq!(model::chat(s.store(), same).unwrap().provider, "codex");
}
#[test]
fn review_is_whole_file_and_agent_marks_do_not_claim_human_coverage() {
    let mut s = Session::fake(APPS);
    let snapshot = model::latest_snapshot(s.store(), 1).unwrap();
    let p = model::progress(s.store(), snapshot.id);
    assert_eq!((p.reviewed, p.total), (46, 67));
    let file = model::changes(s.store(), snapshot.id)
        .iter()
        .find(|c| c.added == 23)
        .unwrap()
        .clone();
    apply(
        &mut s,
        runtime::Command::MarkFile {
            change_id: file.id,
            reviewed: false,
        },
    );
    assert_eq!(model::progress(s.store(), snapshot.id).reviewed, 23);
    s.store()
        .write(move |c| model::mark_tx(c, file.id, true, "agent", 120.0))
        .unwrap();
    assert_eq!(model::progress(s.store(), snapshot.id).reviewed, 23);
    apply(
        &mut s,
        runtime::Command::MarkFile {
            change_id: file.id,
            reviewed: true,
        },
    );
    assert_eq!(model::progress(s.store(), snapshot.id).reviewed, 46);
}
#[test]
fn separate_file_comment_drafts_do_not_overwrite_each_other() {
    let mut s = Session::fake(APPS);
    for (path, text) in [
        (None, "general"),
        (Some("a.rs"), "first"),
        (Some("b.rs"), "second"),
    ] {
        apply(
            &mut s,
            runtime::Command::SaveCommentDraft {
                workspace_id: 1,
                path: path.map(str::to_owned),
                text: text.into(),
            },
        );
    }
    assert_eq!(model::comment_draft(s.store(), 1, None), "general");
    assert_eq!(model::comment_draft(s.store(), 1, Some("a.rs")), "first");
    assert_eq!(model::comment_draft(s.store(), 1, Some("b.rs")), "second");
}
#[test]
fn pr_prompt_stays_in_its_workspace_and_uses_recent_chat() {
    let mut s = Session::fake(APPS);
    apply(
        &mut s,
        runtime::Command::TouchChat {
            chat_id: 2,
            viewed_version: None,
        },
    );
    let result = apply(
        &mut s,
        runtime::Command::CreatePr {
            workspace_id: 1,
            draft: true,
        },
    );
    assert_eq!(result["chat_id"], 2);
    let body = model::messages(s.store(), 2).last().unwrap().body.clone();
    assert!(body.contains("draft pull request"));
    let result = apply(
        &mut s,
        runtime::Command::CreatePr {
            workspace_id: 2,
            draft: false,
        },
    );
    let chat = model::chat(s.store(), result["chat_id"].as_i64().unwrap()).unwrap();
    assert_eq!(chat.workspace_id, 2);
    assert_eq!(chat.provider, "codex");
}
#[test]
fn review_verbs_have_no_filters_next_or_partial_scopes() {
    let mut s = Session::fake(APPS);
    s.nav(Nav::Open {
        from: 0,
        id: PanelId::new(Tag("workshop_review"), ["1"]),
        fresh: true,
    });
    s.settle();
    let p = s.panel(s.focus().unwrap()).unwrap();
    let p = p.borrow();
    for verb in p.verbs() {
        assert!(!verb.id.contains("filter"));
        assert!(!verb.id.contains("next"));
        assert!(!verb.id.contains("chunk"));
    }
}
#[test]
fn tools_have_strict_schemas_and_external_writes_are_explicit() {
    let tools = WORKSHOP.tools();
    assert!(tools.len() >= 20);
    for t in &tools {
        assert_eq!(t.input["additionalProperties"], false, "{}", t.name);
        assert!(!t.description.is_empty());
    }
    for name in [
        "workshop.github.merge",
        "workshop.github.comment",
        "workshop.chats.send",
        "workshop.git.push",
    ] {
        assert!(tools.iter().find(|t| t.name == name).unwrap().asks);
    }
    assert!(tools
        .iter()
        .find(|t| t.name == "workshop.chats.start")
        .unwrap()
        .check(&json!({"workspace_id":1,"title":"should not exist"}))
        .is_err());
}

#[test]
fn raw_sql_cannot_rewrite_review_or_external_operation_state() {
    let mut s = Session::fake(APPS);
    let tool = s.apps().tool("sql.write").unwrap().clone();
    let prepared = kernel::runtime::block_on(tool.preparer.unwrap()(
        &json!({"sql":"UPDATE workshop_change SET reviewed=1"}),
    )(s.world()))
    .unwrap();
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let out = result.clone();
    prepared.commit(&mut s, move |_, value| *out.borrow_mut() = Some(value));
    let error = result.borrow_mut().take().unwrap().unwrap_err();
    assert!(error.contains("app's actions"), "{error}");
    let snap = model::latest_snapshot(s.store(), 1).unwrap();
    assert_eq!(model::progress(s.store(), snap.id).reviewed, 46);
}

#[test]
fn pending_approval_surfaces_attention_even_after_chat_was_read() {
    let mut s = Session::fake(APPS);
    s.store().write(|c| {
        c.execute("UPDATE workshop_chat SET unread=0", [])?;
        c.execute("UPDATE workshop_chat SET status='running' WHERE id=1", [])?;
        c.execute("INSERT INTO workshop_run(chat_id,provider,model,prompt,mode,status,created) VALUES(1,'codex','default','test','work','running',1)", [])?;
        let run = c.last_insert_rowid();
        c.execute("INSERT INTO workshop_tool_call(chat_id,run_id,name,arguments,status,created) VALUES(1,?,'workshop.git.push','{}','pending',1)",[run])?;
        Ok(())
    }).unwrap();
    assert_eq!(model::chat(s.store(), 1).unwrap().status, "waiting");
    assert!(model::workspace(s.store(), 1).unwrap().unread);
    let id = model::tool_calls(s.store(), 1)[0].id;
    apply(&mut s, runtime::Command::RefuseTool { call_id: id });
    assert_eq!(model::chat(s.store(), 1).unwrap().status, "running");
    assert!(!model::workspace(s.store(), 1).unwrap().unread);
}

#[test]
fn a_failed_repository_can_be_retried_without_creating_a_duplicate() {
    let mut s = Session::fake(APPS);
    s.store()
        .write(|c| {
            c.execute(
                "UPDATE workshop_project SET status='failed',error='Git not installed' WHERE id=1",
                [],
            )
        })
        .unwrap();
    let retried = apply(
        &mut s,
        runtime::Command::AddProject {
            path: "/sample/projects/superapp".into(),
        },
    );
    assert_eq!(retried["project_id"], 1);
    assert!(retried["job_id"].is_i64());
    let projects = model::projects(s.store());
    assert_eq!(projects.len(), 2);
    let project = projects.iter().find(|p| p.id == 1).unwrap();
    assert_eq!(project.status, "pending");
    assert!(project.error.is_empty());
}

#[test]
fn branch_names_come_from_the_answer_and_read_as_titles() {
    assert_eq!(model::branch_slug("`fix-the-login-timeout`\n"), Some("fix-the-login-timeout".into()));
    assert_eq!(model::branch_slug("\"Fix the Login timeout!\"\nmore words"), Some("fix-the-login-timeout".into()));
    assert_eq!(model::branch_slug("workshop/fix-login"), Some("fix-login".into()));
    assert_eq!(model::branch_slug("  prepor/Telegram_scroll jump  "), Some("telegram-scroll-jump".into()));
    assert_eq!(model::branch_slug(model::NO_NAME), None);
    assert_eq!(model::branch_slug("`<no-name>`"), None);
    assert_eq!(model::branch_slug("   \n---\n"), None);
    let long = model::branch_slug(&"word-".repeat(30)).unwrap();
    assert!(long.len() <= 60 && !long.ends_with('-'), "{long}");
    assert_eq!(model::humanize("fix-the-login-timeout"), "Fix the login timeout");
    assert_eq!(model::humanize("sync_op-truncation"), "Sync op truncation");
    assert_eq!(model::describe_branch("basel", "workshop/basel"), "");
    assert_eq!(model::describe_branch("basel", "workshop/fix-login"), "fix-login");
    assert_eq!(model::describe_branch("basel", "main"), "");
    assert!(model::naming_prompt("Fix it").contains(model::NO_NAME));
    let s = Session::fake(APPS);
    let mut w = model::workspace(s.store(), 2).unwrap();
    assert_eq!((w.branch.as_str(), model::workspace_title(&w).as_str()), ("workshop/oslo", "oslo"));
    w.branch = "workshop/fix-login".into();
    assert_eq!(model::workspace_title(&w), "Fix login");
    w.pr_json = json!({"number":5,"title":"  Fix login timeouts "}).to_string();
    assert_eq!(model::workspace_title(&w), "Fix login timeouts");
}

#[test]
fn the_first_message_queues_one_naming_job_that_renames_the_placeholder_branch() {
    let mut s = Session::fake(APPS);
    let jobs = |s: &Session, workspace: i64| -> Vec<(i64, String)> {
        let conn = s.store().conn();
        let mut stmt = conn
            .prepare("SELECT id,payload FROM workshop_job WHERE kind='name_branch' AND workspace_id=? ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([workspace], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    // zurich has a pull request and a named branch: a send queues nothing.
    apply(&mut s, runtime::Command::Send { chat_id: 1, text: "Add tests".into(), mode: "work".into() });
    assert!(jobs(&s, 1).is_empty());
    // oslo sits on its placeholder with no PR: the first message names it.
    let chat = apply(&mut s, runtime::Command::NewChat { workspace_id: 2 })["chat_id"].as_i64().unwrap();
    let reply = apply(&mut s, runtime::Command::Send { chat_id: chat, text: "Fix the login timeout".into(), mode: "work".into() });
    assert!(reply["naming_job"].is_i64());
    let queued = jobs(&s, 2);
    assert_eq!(queued.len(), 1);
    let payload: Value = serde_json::from_str(&queued[0].1).unwrap();
    assert_eq!(payload["message"], "Fix the login timeout");
    assert_eq!(payload["placeholder"], "workshop/oslo");
    assert_eq!(payload["provider"], "codex");
    // A second message while one is pending does not queue another.
    apply(&mut s, runtime::Command::Send { chat_id: chat, text: "Also the logout".into(), mode: "work".into() });
    assert_eq!(jobs(&s, 2).len(), 1);
    // The fixture's answer names the branch; the table and hub read it as a title.
    let workspace = model::workspace(s.store(), 2).unwrap();
    let result = runtime::fake_operation("name_branch", &payload, Some(&workspace));
    assert_eq!(result.as_ref().unwrap()["branch"], "workshop/fix-the-login-timeout");
    let outcome = runtime::OperationOutcome {
        job: queued[0].0,
        workspace: Some(2),
        kind: "name_branch".into(),
        request: payload.clone(),
        result,
        now: 300.0,
    };
    s.store().write(move |c| runtime::record_operation(c, &outcome)).unwrap();
    let named = model::workspace(s.store(), 2).unwrap();
    assert_eq!(named.branch, "workshop/fix-the-login-timeout");
    assert_eq!(model::workspace_title(&named), "Fix the login timeout");
    assert_eq!(named.activity, workspace.activity, "naming is not activity");
    // Once named, later messages leave the branch alone; a stale answer
    // for a branch that moved on is dropped rather than applied.
    apply(&mut s, runtime::Command::Send { chat_id: chat, text: "One more".into(), mode: "work".into() });
    assert_eq!(jobs(&s, 2).len(), 1);
    let stale = runtime::OperationOutcome {
        job: queued[0].0,
        workspace: Some(2),
        kind: "name_branch".into(),
        request: payload,
        result: Ok(json!({"branch":"workshop/something-else","from":"workshop/oslo"})),
        now: 301.0,
    };
    s.store().write(move |c| runtime::record_operation(c, &stale)).unwrap();
    assert_eq!(model::workspace(s.store(), 2).unwrap().branch, "workshop/fix-the-login-timeout");
    // A failed naming call is the job's failure, never the workspace's.
    let failed = runtime::OperationOutcome {
        job: queued[0].0,
        workspace: Some(2),
        kind: "name_branch".into(),
        request: json!({}),
        result: Err("The naming call did not answer in time".into()),
        now: 302.0,
    };
    s.store().write(move |c| runtime::record_operation(c, &failed)).unwrap();
    assert_eq!(model::workspace(s.store(), 2).unwrap().error, "");
}

#[test]
fn streamed_events_land_as_items_that_update_in_place() {
    let s = Session::fake(APPS);
    let (run, message) = s
        .store()
        .write(|c| {
            c.execute("INSERT INTO workshop_run(chat_id,provider,model,prompt,mode,status,created) VALUES(1,'claude','default','go','work','running',1)", [])?;
            let run = c.last_insert_rowid();
            c.execute("INSERT INTO workshop_message(chat_id,run_id,role,body,created) VALUES(1,?1,'Claude Code','',1)", [run])?;
            Ok((run, c.last_insert_rowid()))
        })
        .unwrap();
    let event = |kind: &str, text: &str, data: Value, item: Option<harness::Item>| harness::HarnessEvent {
        kind: kind.into(),
        text: text.into(),
        session_id: None,
        model: None,
        data,
        item,
    };
    let mut pending = runtime::Pending::default();
    pending.absorb(event("assistant_delta", "Hel", json!({"id":"msg_1"}), None));
    pending.absorb(event("assistant_delta", "lo", json!({"id":"msg_1"}), None));
    pending.absorb(event(
        "item",
        "",
        Value::Null,
        Some(harness::Item {
            key: "toolu_1".into(),
            kind: "tool".into(),
            name: "Bash".into(),
            title: "cargo test".into(),
            input: Some(json!({"command":"cargo test"})),
            status: "running".into(),
            ..Default::default()
        }),
    ));
    pending.absorb(event(
        "item",
        "",
        Value::Null,
        Some(harness::Item {
            key: "toolu_1".into(),
            body: Some("12 passed".into()),
            meta: Some(json!({"exit_code":0})),
            status: "done".into(),
            ..Default::default()
        }),
    ));
    assert!(pending.dirty);
    let batch = pending.take(5.0);
    assert!(!pending.dirty);
    s.store().write(move |c| batch.write(c, 1, run, message)).unwrap();
    let items = model::items(s.store(), 1);
    let streamed: Vec<_> = items.iter().filter(|i| i.run_id == run).collect();
    assert_eq!(streamed.len(), 2);
    assert_eq!((streamed[0].kind.as_str(), streamed[0].body.as_str(), streamed[0].status.as_str()), ("text", "Hello", "running"));
    assert_eq!((streamed[1].name.as_str(), streamed[1].title.as_str(), streamed[1].body.as_str(), streamed[1].status.as_str()), ("Bash", "cargo test", "12 passed", "done"));
    assert!(streamed[1].meta.contains("exit_code"));
    assert_eq!(model::messages(s.store(), 1).iter().find(|m| m.id == message).unwrap().body, "Hello");
    // The completed message replaces the streamed words; a later tick merges
    // meta into what an earlier one wrote.
    pending.absorb(event("assistant", "Hello there", json!({"id":"msg_1"}), None));
    pending.absorb(event(
        "item",
        "",
        Value::Null,
        Some(harness::Item {
            key: "toolu_1".into(),
            meta: Some(json!({"background":true})),
            ..Default::default()
        }),
    ));
    let batch = pending.take(6.0);
    s.store().write(move |c| batch.write(c, 1, run, message)).unwrap();
    let items = model::items(s.store(), 1);
    let text = items.iter().find(|i| i.key == "msg_1").unwrap();
    assert_eq!((text.body.as_str(), text.status.as_str()), ("Hello there", "done"));
    let tool = items.iter().find(|i| i.key == "toolu_1").unwrap();
    let meta: Value = serde_json::from_str(&tool.meta).unwrap();
    assert_eq!((meta["exit_code"].as_i64(), meta["background"].as_bool()), (Some(0), Some(true)));
    assert_eq!(tool.status, "done");
}
