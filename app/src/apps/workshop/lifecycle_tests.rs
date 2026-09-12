//! Lifecycle keeps local work readable while preventing cancelled work replay.
use super::{model, runtime, schema, tools, WORKSHOP};
use kernel::{
    app::{App, Schema},
    nav::Nav,
    panel::{PanelId, Tag},
    session::Session,
    tool::Prepared,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc};

static APPS: &[&dyn App] = &[&WORKSHOP];
fn command(s: &mut Session, command: runtime::Command) -> Result<Value, String> {
    let reply = Rc::new(RefCell::new(None));
    let output = reply.clone();
    let edit = runtime::command_edit(command, 200.0, "/sample/store".into(), None, "human".into())
        .wake_if(|_| false);
    s.act_async_result(edit, move |_, result| {
        *output.borrow_mut() = Some(result.map_err(|error| error.to_string()));
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
        let result = reply.borrow_mut().take().expect("fake write completed");
        result
    } else {
        (tool.run)(s, &input)
    }
}
fn queued_work(s: &Session) -> (i64, i64, i64) {
    s.store().write(|c| {
        let running = model::send_tx(c, 1, "first request", "work", 120.0)?;
        let queued = model::send_tx(c, 1, "queued request", "work", 121.0)?;
        c.execute("UPDATE workshop_run SET status='running' WHERE id=?", [running])?;
        c.execute("UPDATE workshop_chat SET status='running',session_id='saved-session',draft='unsent text' WHERE id=1", [])?;
        for status in ["pending", "approved", "running"] {
            c.execute("INSERT INTO workshop_tool_call(chat_id,run_id,name,arguments,status,created) VALUES(1,?1,'sql.query','{}',?2,122)", params![running,status])?;
        }
        let job = model::queue_tx(c, Some(1), "comment", json!({"text":"retained publication"}), 123.0)?;
        Ok((running, queued, job))
    }).unwrap()
}
fn statuses(s: &Session, table: &str) -> Vec<String> {
    s.store()
        .conn()
        .prepare(&format!("SELECT status FROM {table} ORDER BY id"))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn migration_adds_open_defaults_to_existing_records_and_preserves_lifecycle_on_reopen() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value);")
        .unwrap();
    let old = Schema {
        app: "workshop",
        steps: &schema::SCHEMA.steps[..2],
    };
    old.apply(&c).unwrap();
    c.execute_batch("INSERT INTO workshop_project(id,name,path,status) VALUES(1,'repo','/repo','ready');
      INSERT INTO workshop_workspace(id,project_id,label,path,branch,base_ref,status,activity) VALUES(1,1,'basel','/worktree','topic','main','ready',12);
      INSERT INTO workshop_chat(id,workspace_id,ordinal,last_used,draft,session_id) VALUES(1,1,1,12,'keep my draft','provider-session');
      INSERT INTO workshop_message(chat_id,role,body,created) VALUES(1,'You','retained history',12);").unwrap();
    schema::SCHEMA.apply(&c).unwrap();
    assert!(!model::workspace_conn(&c, 1).unwrap().archived);
    let chat = model::chat_conn(&c, 1).unwrap();
    assert!(!chat.closed);
    assert_eq!(chat.draft, "keep my draft");
    assert_eq!(chat.session_id.as_deref(), Some("provider-session"));
    c.execute_batch("UPDATE workshop_workspace SET archived=1; UPDATE workshop_chat SET closed=1;")
        .unwrap();
    schema::SCHEMA.apply(&c).unwrap();
    assert!(model::workspace_conn(&c, 1).unwrap().archived);
    assert!(model::chat_conn(&c, 1).unwrap().closed);
    assert_eq!(
        c.query_row("SELECT body FROM workshop_message", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "retained history"
    );
}

#[test]
fn close_cancels_runs_and_approvals_but_reopening_preserves_session_draft_and_history() {
    let mut s = Session::fake(APPS);
    queued_work(&s);
    let messages = model::messages(s.store(), 1);
    let steps = model::steps(s.store(), 1);
    let reply = command(&mut s, runtime::Command::CloseChat { chat_id: 1 }).unwrap();
    assert_eq!(reply["stop_runs"].as_array().unwrap().len(), 2);
    assert_eq!(statuses(&s, "workshop_run"), ["stopped", "stopped"]);
    assert!(statuses(&s, "workshop_tool_call")
        .iter()
        .all(|s| s == "interrupted"));
    assert!(model::chats(s.store(), 1).iter().all(|c| c.id != 1));
    assert_eq!(model::closed_chats(s.store(), 1).len(), 1);
    let closed = model::chat(s.store(), 1).unwrap();
    assert!(closed.closed);
    assert!(!closed.unread);
    assert_eq!(closed.draft, "unsent text");
    assert_eq!(closed.session_id.as_deref(), Some("saved-session"));
    assert_eq!(model::messages(s.store(), 1), messages);
    assert_eq!(model::steps(s.store(), 1), steps);
    command(&mut s, runtime::Command::ReopenChat { chat_id: 1 }).unwrap();
    assert!(!model::chat(s.store(), 1).unwrap().closed);
    assert!(model::closed_chats(s.store(), 1).is_empty());
    assert_eq!(statuses(&s, "workshop_run"), ["stopped", "stopped"]);
    assert_eq!(model::messages(s.store(), 1), messages);
}

#[test]
fn archive_cancels_workspace_queues_and_restore_does_not_replay_them() {
    let mut s = Session::fake(APPS);
    queued_work(&s);
    s.store()
        .write(|c| model::send_tx(c, 2, "other chat", "work", 124.0))
        .unwrap();
    let original = model::workspace(s.store(), 1).unwrap();
    command(
        &mut s,
        runtime::Command::ArchiveWorkspace { workspace_id: 1 },
    )
    .unwrap();
    let archived = model::workspace(s.store(), 1).unwrap();
    assert!(archived.archived);
    assert!(!archived.unread);
    assert_eq!(archived.path, original.path);
    assert_eq!(archived.snapshot_id, original.snapshot_id);
    assert!(statuses(&s, "workshop_run").iter().all(|s| s == "stopped"));
    assert_eq!(statuses(&s, "workshop_job"), ["stopped"]);
    assert!(runtime::workers(s.store()).iter().all(|w| ![
        "workshop-watch-1",
        "workshop-chat-1",
        "workshop-chat-2"
    ]
    .contains(&w.name().as_str())));
    command(
        &mut s,
        runtime::Command::RestoreWorkspace { workspace_id: 1 },
    )
    .unwrap();
    assert!(!model::workspace(s.store(), 1).unwrap().archived);
    assert!(statuses(&s, "workshop_run").iter().all(|s| s == "stopped"));
    assert_eq!(statuses(&s, "workshop_job"), ["stopped"]);
    assert!(model::chats(s.store(), 1).iter().all(|c| !c.closed));
}

#[test]
fn late_provider_completion_cannot_reopen_closed_chat_or_restore_archived_workspace() {
    for archive in [false, true] {
        let mut s = Session::fake(APPS);
        let (run, _, _) = queued_work(&s);
        let action = if archive {
            runtime::Command::ArchiveWorkspace { workspace_id: 1 }
        } else {
            runtime::Command::CloseChat { chat_id: 1 }
        };
        command(&mut s, action).unwrap();
        let workspace = model::workspace(s.store(), 1).unwrap();
        s.store()
            .write(move |c| runtime::finish_run(c, run, 1, 1, Ok(()), 999.0))
            .unwrap();
        assert_eq!(model::chat(s.store(), 1).unwrap().status, "stopped");
        assert!(!model::chat(s.store(), 1).unwrap().unread);
        assert_eq!(model::workspace(s.store(), 1).unwrap().archived, archive);
        if archive {
            assert_eq!(
                model::workspace(s.store(), 1).unwrap().activity,
                workspace.activity
            );
        } else {
            assert!(model::chat(s.store(), 1).unwrap().closed);
        }
        assert_eq!(statuses(&s, "workshop_run"), ["stopped", "stopped"]);
    }
}

#[test]
fn closed_and_archived_records_reject_new_agent_work_and_github_jobs() {
    let mut s = Session::fake(APPS);
    command(&mut s, runtime::Command::CloseChat { chat_id: 1 }).unwrap();
    for action in [
        runtime::Command::Send {
            chat_id: 1,
            text: "no".into(),
            mode: "work".into(),
        },
        runtime::Command::SetProvider {
            chat_id: 1,
            provider: "claude".into(),
        },
        runtime::Command::SetModel {
            chat_id: 1,
            model: "custom".into(),
        },
    ] {
        assert!(command(&mut s, action).unwrap_err().contains("Reopen"));
    }
    command(
        &mut s,
        runtime::Command::ArchiveWorkspace { workspace_id: 1 },
    )
    .unwrap();
    for action in [
        runtime::Command::NewChat { workspace_id: 1 },
        runtime::Command::ReopenChat { chat_id: 1 },
        runtime::Command::Send {
            chat_id: 2,
            text: "no".into(),
            mode: "work".into(),
        },
        runtime::Command::CreatePr {
            workspace_id: 1,
            draft: true,
        },
        runtime::Command::Refresh { workspace_id: 1 },
        runtime::Command::AiReview { workspace_id: 1 },
        runtime::Command::Push { workspace_id: 1 },
    ] {
        assert!(command(&mut s, action).unwrap_err().contains("Restore"));
    }
    assert!(statuses(&s, "workshop_run").is_empty());
    assert!(statuses(&s, "workshop_job").is_empty());
}

#[test]
fn lifecycle_cancellations_are_not_generic_undo_steps_that_can_replay_external_work() {
    for action in [
        runtime::Command::CloseChat { chat_id: 1 },
        runtime::Command::ArchiveWorkspace { workspace_id: 1 },
        runtime::Command::Stop { chat_id: 1 },
    ] {
        let mut s = Session::fake(APPS);
        queued_work(&s);
        assert!(!s.undo());
        command(&mut s, action).unwrap();
        assert!(!s.undo());
        assert_eq!(statuses(&s, "workshop_run"), ["stopped", "stopped"]);
        assert!(statuses(&s, "workshop_tool_call")
            .iter()
            .all(|s| s == "interrupted"));
    }
}

#[test]
fn lifecycle_tools_filter_lists_and_close_open_chat_panels() {
    let mut s = Session::fake(APPS);
    let chat = PanelId::new(Tag("workshop_chat"), ["1"]);
    s.nav(Nav::Open {
        from: 0,
        id: chat.clone(),
        fresh: true,
    });
    s.settle();
    assert!(!s.showing(&chat).is_empty());
    tool(&mut s, "workshop.chats.close", json!({"chat_id":1})).unwrap();
    assert!(s.showing(&chat).is_empty());
    let active = tool(&mut s, "workshop.chats.list", json!({"workspace_id":1})).unwrap();
    assert_eq!(active.as_array().unwrap().len(), 1);
    let all = tool(
        &mut s,
        "workshop.chats.list",
        json!({"workspace_id":1,"include_closed":true}),
    )
    .unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(all[0]["closed"], true);
    tool(
        &mut s,
        "workshop.workspaces.archive",
        json!({"workspace_id":1}),
    )
    .unwrap();
    let active = tool(&mut s, "workshop.workspaces.list", json!({})).unwrap();
    assert!(active.as_array().unwrap().iter().all(|w| w["id"] != 1));
    let archived = tool(&mut s, "workshop.workspaces.list", json!({"archived":true})).unwrap();
    assert_eq!(archived.as_array().unwrap().len(), 1);
    assert_eq!(archived[0]["id"], 1);
    tool(
        &mut s,
        "workshop.workspaces.restore",
        json!({"workspace_id":1}),
    )
    .unwrap();
    tool(&mut s, "workshop.chats.reopen", json!({"chat_id":1})).unwrap();
    assert!(!s.showing(&chat).is_empty());
}

#[test]
fn archive_stops_only_embedded_terminal_and_restore_starts_a_fresh_session() {
    static TERMINALS: &[&dyn App] = &[&crate::apps::terminal::TERMINAL, &WORKSHOP];
    let mut s = Session::fake(TERMINALS);
    let promoted = super::terminal::embedded(s.world(), 1).unwrap();
    super::terminal::try_promote(&mut s, 0, 1).unwrap();
    let embedded = super::terminal::embedded(s.world(), 1).unwrap();
    assert_ne!(promoted.id, embedded.id);
    let reply = command(
        &mut s,
        runtime::Command::ArchiveWorkspace { workspace_id: 1 },
    )
    .unwrap();
    runtime::navigate(&mut s, 0, &reply);
    let service = s
        .world()
        .with_cap::<crate::apps::terminal::TerminalService, _>(|s| *s)
        .unwrap();
    assert!(service.get(s.store(), &embedded.id).is_none());
    assert!(service.get(s.store(), &promoted.id).is_some());
    assert!(super::terminal::embedded(s.world(), 1).is_err());
    command(
        &mut s,
        runtime::Command::RestoreWorkspace { workspace_id: 1 },
    )
    .unwrap();
    let fresh = super::terminal::embedded(s.world(), 1).unwrap();
    assert_ne!(fresh.id, embedded.id);
    assert!(service.get(s.store(), &promoted.id).is_some());
    super::terminal::close(s.world(), &promoted.id);
    super::terminal::close(s.world(), &fresh.id);
}

#[test]
fn restoring_cancelled_worktree_preparation_does_not_replay_cancelled_prompts() {
    let mut s = Session::fake(APPS);
    let created = command(&mut s, runtime::Command::NewWorkspace { project_id: 1 }).unwrap();
    let workspace = created["workspace_id"].as_i64().unwrap();
    let chat = created["chat_id"].as_i64().unwrap();
    command(
        &mut s,
        runtime::Command::Send {
            chat_id: chat,
            text: "wait for checkout".into(),
            mode: "work".into(),
        },
    )
    .unwrap();
    command(
        &mut s,
        runtime::Command::ArchiveWorkspace {
            workspace_id: workspace,
        },
    )
    .unwrap();
    assert_eq!(statuses(&s, "workshop_job"), ["stopped"]);
    command(
        &mut s,
        runtime::Command::RestoreWorkspace {
            workspace_id: workspace,
        },
    )
    .unwrap();
    assert_eq!(statuses(&s, "workshop_job"), ["pending"]);
    assert_eq!(statuses(&s, "workshop_run"), ["stopped"]);
}

#[test]
fn restoring_an_open_workspace_reuses_its_panel() {
    let mut s = Session::fake(APPS);
    let panel = PanelId::new(Tag("workshop_workspace"), ["1"]);
    s.nav(Nav::Open {
        from: 0,
        id: panel.clone(),
        fresh: true,
    });
    s.settle();
    let slot = s.showing(&panel)[0];
    let archived = command(
        &mut s,
        runtime::Command::ArchiveWorkspace { workspace_id: 1 },
    )
    .unwrap();
    runtime::navigate(&mut s, slot, &archived);
    let restored = command(
        &mut s,
        runtime::Command::RestoreWorkspace { workspace_id: 1 },
    )
    .unwrap();
    runtime::navigate(&mut s, slot, &restored);
    s.settle();
    assert_eq!(s.showing(&panel), vec![slot]);
    assert_eq!(s.focus(), Some(slot));
}
