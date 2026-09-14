//! Acceptance-path tests: real Session/store/tools with fake outside capabilities.
use super::{git, model, panels, runtime, snapshots, tools, WORKSHOP};
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

/// The hub's chat list is a list: its cursor walks the chats it draws, lands
/// on the first row from nothing whichever way it is asked, and stops at
/// either end rather than wrapping.
#[test]
fn the_hubs_cursor_walks_its_own_chats_and_stops_at_the_ends() {
    let mut s = session();
    let hub = open(&mut s, panel("workshop_workspace", 1));
    let instance = s.panel(hub).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Detail>().unwrap();
    let chats = p.listed_chats();
    assert!(chats.len() > 1, "the fixture has parallel chats");
    assert_eq!(p.cursor, None, "a hub opens with no row under the cursor");
    assert_eq!(
        p.walk(-1),
        Some(chats[0]),
        "up from nothing is the first row"
    );
    assert_eq!(p.walk(-1), Some(chats[0]), "and it stops there");
    assert_eq!(p.cursor_row(), Some(0));
    assert_eq!(p.walk(1), Some(chats[1]));
    for _ in 0..chats.len() {
        p.walk(1);
    }
    assert_eq!(p.cursor, chats.last().copied(), "the walk stops at the end");
    p.set_cursor(chats[0]);
    assert_eq!(p.cursor_row(), Some(0));
}

/// The closed-chat list is the same list, over the other set.
#[test]
fn the_closed_chat_list_walks_the_chats_the_hub_does_not() {
    let mut s = session();
    command(
        &mut s,
        runtime::Command::CloseChat { chat_id: 1 },
        None,
        "human",
    )
    .unwrap();
    let hub = open(&mut s, panel("workshop_workspace", 1));
    let closed = open(&mut s, panel("workshop_closed_chats", 1));
    let listed = |slot: u64| {
        let instance = s.panel(slot).unwrap();
        let mut borrow = instance.borrow_mut();
        borrow
            .as_any()
            .downcast_mut::<panels::Detail>()
            .unwrap()
            .listed_chats()
    };
    assert!(!listed(hub).contains(&1));
    assert_eq!(listed(closed), vec![1]);
}

/// Where a chat was last read is bookkeeping: it is written for a closed
/// chat as readily as an open one, it records no history node, and it does
/// not make the chat recently used — reading is not using.
#[test]
fn a_chats_reading_position_is_saved_without_a_node_or_an_activity_bump() {
    let mut s = session();
    let before = model::chat(s.store(), 1).unwrap();
    assert_eq!(before.anchor_key, "", "a chat starts at its tail");
    assert_eq!(before.anchor_scroll, 0.0);
    let depth = s.history().rows().0.len();
    command(
        &mut s,
        runtime::Command::SaveReading {
            chat_id: 1,
            key: "item:7".into(),
            scroll: 12.5,
        },
        None,
        "human",
    )
    .unwrap();
    let after = model::chat(s.store(), 1).unwrap();
    assert_eq!(after.anchor_key, "item:7");
    assert!((after.anchor_scroll - 12.5).abs() < f64::EPSILON);
    assert_eq!(
        after.last_used, before.last_used,
        "reading a chat is not using it"
    );
    assert_eq!(
        s.history().rows().0.len(),
        depth,
        "a reading position is not undoable"
    );
    command(
        &mut s,
        runtime::Command::CloseChat { chat_id: 1 },
        None,
        "human",
    )
    .unwrap();
    command(
        &mut s,
        runtime::Command::SaveReading {
            chat_id: 1,
            key: String::new(),
            scroll: 0.0,
        },
        None,
        "human",
    )
    .expect("a closed chat is read as readily as an open one");
    assert_eq!(model::chat(s.store(), 1).unwrap().anchor_key, "");
}

/// A reading the pause has not written yet is not lost to a quit: `flush`
/// is the hook a shutdown calls, and it waits for the write, because the
/// process is about to go. Closing the panel writes it too, without waiting
/// — a close happens on the frame of the press.
#[test]
fn a_reading_inside_the_pause_survives_both_a_quit_and_a_close() {
    let mut s = session();
    let chat = open(&mut s, panel("workshop_chat", 1));
    let instance = s.panel(chat).unwrap();
    instance
        .borrow_mut()
        .as_any()
        .downcast_mut::<panels::Detail>()
        .unwrap()
        .reading = Some(("item:7".into(), 12.5));
    s.begin_shutdown();
    let after = model::chat(s.store(), 1).unwrap();
    assert_eq!(after.anchor_key, "item:7", "a quit writes what it has");
    assert!((after.anchor_scroll - 12.5).abs() < f64::EPSILON);

    // And again on a close, through the panel going away.
    let mut s = session();
    let chat = open(&mut s, panel("workshop_chat", 1));
    let instance = s.panel(chat).unwrap();
    instance
        .borrow_mut()
        .as_any()
        .downcast_mut::<panels::Detail>()
        .unwrap()
        .reading = Some(("msg:3".into(), 4.0));
    drop(instance);
    s.nav(Nav::Close {
        slot: chat,
        label: None,
    });
    s.settle();
    // The close submits rather than waits; this is the barrier behind it.
    s.store().write(|_| Ok(())).unwrap();
    assert_eq!(model::chat(s.store(), 1).unwrap().anchor_key, "msg:3");
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
fn queued_file_comment_keeps_its_reviewed_snapshot_after_later_changes() {
    let mut s = session();
    let original = model::latest_snapshot(s.store(), 1).unwrap();
    let original_content: git::Snapshot = serde_json::from_str(&original.json).unwrap();
    let file = original_content.files[0].clone();
    command(
        &mut s,
        runtime::Command::SaveCommentDraft {
            workspace_id: 1,
            path: Some(file.path.clone()),
            text: "Feedback on the original code".into(),
        },
        None,
        "human",
    )
    .unwrap();
    let accepted = command(
        &mut s,
        runtime::Command::PostComment {
            workspace_id: 1,
            path: Some(file.path.clone()),
            text: "Feedback on the original code".into(),
            snapshot_id: Some(original.id),
        },
        None,
        "human",
    )
    .unwrap();
    let job = accepted["job_id"].as_i64().unwrap();
    let request: Value = serde_json::from_str(
        &s.store()
            .conn()
            .query_row("SELECT payload FROM workshop_job WHERE id=?", [job], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();

    let mut later = original_content;
    later.tree_oid = "later-tree".into();
    later.files[0].new_blob = "later-published-blob".into();
    later.files[0].new_mode = "100755".into();
    later.files[0].patch = "different code that was not reviewed".into();
    s.store()
        .write(move |c| snapshots::put(c, 1, later, 240.0, false))
        .unwrap();
    let workspace = model::workspace(s.store(), 1).unwrap();
    assert_ne!(workspace.snapshot_id, Some(original.id));
    let saved = runtime::comment_snapshot(s.store().conn(), &request, Some(&workspace))
        .unwrap()
        .unwrap();
    assert_eq!(saved.files[0].patch, file.patch);
    assert_eq!(saved.files[0].new_blob, file.new_blob);
    assert_eq!(saved.files[0].new_mode, file.new_mode);

    // A comment opened from the historical preview still queues that version.
    let historical = command(
        &mut s,
        runtime::Command::PostComment {
            workspace_id: 1,
            path: Some(file.path.clone()),
            text: "Feedback from the historical preview".into(),
            snapshot_id: Some(original.id),
        },
        None,
        "human",
    )
    .unwrap();
    let pinned: i64 = s
        .store()
        .conn()
        .query_row(
            "SELECT json_extract(payload,'$.snapshot_id') FROM workshop_job WHERE id=?",
            [historical["job_id"].as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pinned, original.id);

    // A failed published-file identity check keeps the draft for the person.
    let outcome = runtime::OperationOutcome {
        job,
        workspace: Some(1),
        kind: "comment".into(),
        request,
        result: Err("This file has unpublished changes".into()),
        now: 250.0,
    };
    s.store()
        .write(move |c| runtime::record_operation(c, &outcome))
        .unwrap();
    assert_eq!(
        model::comment_draft(s.store(), 1, Some(&file.path)),
        "Feedback on the original code"
    );
}

#[test]
fn file_comment_never_falls_back_from_missing_or_foreign_snapshot() {
    let mut s = session();
    let original = model::latest_snapshot(s.store(), 1).unwrap();
    let workspace = model::workspace(s.store(), 1).unwrap();
    let file = model::changes(s.store(), original.id)[0].path.clone();
    for request in [
        json!({"path":file}),
        json!({"path":file,"snapshot_id":original.id+999}),
        json!({"path":"missing.rs","snapshot_id":original.id}),
    ] {
        assert!(runtime::comment_snapshot(s.store().conn(), &request, Some(&workspace)).is_err());
    }
    let other = model::workspace(s.store(), 2).unwrap();
    let foreign = json!({"path":file,"snapshot_id":original.id});
    assert!(
        runtime::comment_snapshot(s.store().conn(), &foreign, Some(&other))
            .unwrap_err()
            .contains("another workspace")
    );
    assert!(command(
        &mut s,
        runtime::Command::PostComment {
            workspace_id: 1,
            path: Some("missing.rs".into()),
            text: "feedback".into(),
            snapshot_id: Some(original.id),
        },
        None,
        "human"
    )
    .is_err());
    assert_eq!(
        s.store()
            .conn()
            .query_row(
                "SELECT count(*) FROM workshop_job WHERE kind='comment'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    // General PR comments have a queued PR-head guard, without a file snapshot.
    assert!(runtime::comment_snapshot(
        s.store().conn(),
        &json!({"text":"general"}),
        Some(&workspace)
    )
    .unwrap()
    .is_none());
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
            text: "first".into(),
            snapshot_id: None,
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
