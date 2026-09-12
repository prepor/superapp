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
    apply(&mut s, runtime::Command::TouchChat { chat_id: 2 });
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
