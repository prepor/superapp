use super::*;
use crate::apps::notes::ops;
use serde_json::{json, Value};

fn call(s: &mut Session, name: &str, input: Value) -> Result<Value, String> {
    let tool = s.apps().tool(name).expect("registered tool").clone();
    tool.check(&input)?;
    if let Some(reader) = tool.reader {
        kernel::runtime::block_on(reader(&input)(s.world()))
    } else if let Some(prepare) = tool.preparer {
        let prepared = kernel::runtime::block_on(prepare(&input)(s.world()))?;
        commit(s, prepared)
    } else {
        (tool.run)(s, &input)
    }
}

fn commit(s: &mut Session, prepared: kernel::tool::Prepared) -> Result<Value, String> {
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let delivered = result.clone();
    prepared.commit(s, move |_, answer| *delivered.borrow_mut() = Some(answer));
    let answer = result
        .borrow_mut()
        .take()
        .expect("fixture commits immediately");
    answer
}
fn read_note(s: &mut Session, id: i64) -> Value {
    call(s, "notes.read", json!({"id": id})).unwrap()
}
fn read_draft(s: &mut Session, path: &str) -> Value {
    call(s, "notes.read_draft", json!({"path": path})).unwrap()
}
fn write_draft(s: &mut Session, path: &str, body: &str, previous: &Value) -> Value {
    let tool = if previous["exists"] == false {
        "notes.create_draft"
    } else {
        "notes.update_draft"
    };
    call(
        s,
        tool,
        json!({"path": path, "body": body, "revision": previous["revision"]}),
    )
    .unwrap()
}
fn seed_note(s: &Session) -> i64 {
    // A note that predates this session's history.
    s.store()
        .write(|c| {
            c.execute(
                "INSERT INTO notes_note(title,body,created,modified) VALUES('Before','Before',1,1)",
                [],
            )?;
            Ok(c.last_insert_rowid())
        })
        .unwrap()
}

#[test]
fn notes_tools_are_registered_with_strict_schemas_and_reversible_writes() {
    let s = Session::fake(APPS);
    let tools = NOTES.tools();
    assert_eq!(tools.len(), 6);
    for tool in tools {
        assert!(s.apps().tool(tool.name).is_some());
        assert!(!tool.asks, "{} is reversible", tool.name);
        assert_eq!(tool.writes, !tool.name.starts_with("notes.read"));
        assert_eq!(tool.input["additionalProperties"], false);
        assert!(tool.check(&json!({})).is_err());
    }
    let update = s.apps().tool("notes.update").unwrap();
    assert!(update.check(&json!({"id": 1, "body": "x"})).is_err());
    assert!(update
        .check(&json!({"id": "1", "body": "x", "revision": "r"}))
        .is_err());
    assert!(update
        .check(&json!({"id": 1, "body": "x", "revision": "r", "title": "invented"}))
        .is_err());
    assert!(s
        .apps()
        .tool("notes.update_draft")
        .unwrap()
        .check(&json!({"path": "~/a", "body": "x", "revision": "r", "original": "bypass"}))
        .is_err());
    assert!(Session::fake(&[]).apps().tool("notes.create").is_none());
}

#[test]
fn tools_create_populated_notes_and_update_titles_in_one_undo_step_each() {
    let mut s = Session::fake(APPS);
    let before = s.history().head();
    let created = call(
        &mut s,
        "notes.create",
        json!({"body": "\n# Café\r\n\n**idea**"}),
    )
    .unwrap();
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["title"], "Café");
    assert_eq!(
        created["panel"],
        json!({"tag": "editor", "args": ["note", id.to_string()]})
    );
    assert_eq!(read_note(&mut s, id)["body"], "\n# Café\n\n**idea**");
    assert!(s.undo());
    assert_eq!(
        s.history().head(),
        before,
        "creation and initial text are one action"
    );
    assert!(call(&mut s, "notes.read", json!({"id": id})).is_err());
    assert!(s.redo());
    let before = read_note(&mut s, id);
    let head = s.history().head();
    let updated = call(
        &mut s,
        "notes.update",
        json!({"id": id, "body": "## Revised\nbody", "revision": before["revision"]}),
    )
    .unwrap();
    assert_eq!(updated["title"], "Revised");
    assert_ne!(updated["revision"], before["revision"]);
    let created_time: f64 = s
        .store()
        .conn()
        .query_row("SELECT created FROM notes_note WHERE id=?", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(created_time, created["modified"].as_f64().unwrap());
    assert!(s.undo());
    assert_eq!(s.history().head(), head);
    assert_eq!(
        read_note(&mut s, id),
        before,
        "undo restores text, title and timestamp"
    );
    assert!(s.redo());
    assert_eq!(read_note(&mut s, id)["revision"], updated["revision"]);
    let head = s.history().head();
    call(
        &mut s,
        "notes.update",
        json!({"id": id, "body": "## Revised\nbody", "revision": updated["revision"]}),
    )
    .unwrap();
    assert_eq!(s.history().head(), head, "unchanged writes add no history");
}

#[test]
fn stale_note_revisions_and_undo_redo_preserve_newer_typing() {
    let mut s = Session::fake(APPS);
    let id = seed_note(&s);
    let before = read_note(&mut s, id);
    let updated = call(
        &mut s,
        "notes.update",
        json!({"id": id, "body": "Agent", "revision": before["revision"]}),
    )
    .unwrap();
    let head = s.history().head();
    assert!(call(
        &mut s,
        "notes.update",
        json!({"id": id, "body": "Stale", "revision": before["revision"]})
    )
    .unwrap_err()
    .contains("changed"));
    assert_eq!(s.history().head(), head);
    assert!(s.undo());
    model::edit(s.store(), id, "Later typing".into(), s.now()).unwrap();
    s.redo();
    assert_eq!(model::body(s.store(), id).unwrap(), "Later typing");
    assert!(call(
        &mut s,
        "notes.update",
        json!({"id": id, "body": "Stale", "revision": updated["revision"]})
    )
    .is_err());
    let current = read_note(&mut s, id);
    call(
        &mut s,
        "notes.update",
        json!({"id": id, "body": "Another edit", "revision": current["revision"]}),
    )
    .unwrap();
    model::edit(s.store(), id, "Even later typing".into(), s.now()).unwrap();
    s.undo();
    assert_eq!(model::body(s.store(), id).unwrap(), "Even later typing");
}

#[test]
fn writer_transaction_rechecks_the_snapshot_before_committing() {
    let mut s = Session::fake(APPS);
    let id = seed_note(&s);
    let before = model::note_text(s.store().conn(), id).unwrap().unwrap();
    let edit = ops::Edit::Note {
        id,
        before,
        after: model::NoteText::new("Agent".into(), s.now()),
    };
    model::edit(
        s.store(),
        id,
        "Changed between read and transaction".into(),
        s.now(),
    )
    .unwrap();
    let head = s.history().head();
    assert!(commit(&mut s, edit.prepared(json!({})))
        .unwrap_err()
        .contains("changed"));
    assert_eq!(s.history().head(), head);
    assert_eq!(
        model::body(s.store(), id).unwrap(),
        "Changed between read and transaction"
    );

    let path = "~/race.md";
    let edit = ops::Edit::Draft {
        path: path.into(),
        before: None,
        after: None,
    };
    model::save_draft(
        s.store(),
        path.into(),
        model::Draft {
            original: "Before".into(),
            body: "Human draft".into(),
        },
        s.now(),
    )
    .unwrap();
    assert!(commit(&mut s, edit.prepared(json!({})))
        .unwrap_err()
        .contains("changed"));
    assert_eq!(model::draft(s.store(), path).unwrap().body, "Human draft");
    assert_eq!(s.history().head(), head);
}

#[test]
fn draft_tools_preserve_file_bytes_through_create_update_clear_undo_and_redo() {
    let mut s = Session::fake(APPS);
    let path = "~/agent.md";
    let original = "\u{feff}one\r\ntwo\nthree\r\n";
    disk_write(&s, path, original.as_bytes());
    let initial = read_draft(&mut s, path);
    assert_eq!(initial["exists"], false);
    assert_eq!(initial["body"], "one\ntwo\nthree\n");
    assert_eq!(
        s.history().head(),
        0,
        "reading creates neither a draft nor history"
    );
    let first = write_draft(&mut s, path, "one\nTWO\nthree\n", &initial);
    let stored = model::draft(s.store(), path).unwrap();
    assert_eq!(stored.original, original);
    assert_eq!(stored.body, "\u{feff}one\r\nTWO\nthree\r\n");
    assert_eq!(first["exists"], true);
    assert!(s.undo());
    assert!(model::draft(s.store(), path).is_none());
    assert!(s.redo());
    assert_eq!(model::draft(s.store(), path).unwrap(), stored);
    let second = write_draft(&mut s, path, "one\nTWO\nTHREE\n", &first);
    assert!(s.undo());
    assert_eq!(read_draft(&mut s, path)["revision"], first["revision"]);
    assert!(s.redo());
    assert_eq!(read_draft(&mut s, path)["revision"], second["revision"]);
    let cleared = write_draft(&mut s, path, "one\ntwo\nthree\n", &second);
    assert_eq!(cleared["exists"], false);
    assert!(model::draft(s.store(), path).is_none());
    assert!(s.undo());
    assert_eq!(read_draft(&mut s, path)["revision"], second["revision"]);
    assert!(s.redo());
    assert!(model::draft(s.store(), path).is_none());
    let head = s.history().head();
    write_draft(&mut s, path, "one\ntwo\nthree\n", &cleared);
    assert_eq!(s.history().head(), head, "an unchanged file needs no draft");
    assert_eq!(
        disk_read(&s, path),
        original.as_bytes(),
        "no operation or undo wrote the file"
    );
}

#[test]
fn draft_tools_refuse_stale_versions_and_preserve_original_save_conflicts() {
    let mut s = Session::fake(APPS);
    let path = "~/conflict.md";
    disk_write(&s, path, b"original");
    let initial = read_draft(&mut s, path);
    disk_write(&s, path, b"external edit");
    assert!(call(
        &mut s,
        "notes.create_draft",
        json!({"path": path, "body": "agent", "revision": initial["revision"]})
    )
    .unwrap_err()
    .contains("changed"));
    assert!(model::draft(s.store(), path).is_none());
    let initial = read_draft(&mut s, path);
    let first = write_draft(&mut s, path, "agent", &initial);
    assert!(call(
        &mut s,
        "notes.create_draft",
        json!({"path": path, "body": "overwrite", "revision": initial["revision"]})
    )
    .unwrap_err()
    .contains("already exists"));
    let slot = open(&mut s, Editor::file(path));
    editor(&mut s, slot, |p, _| p.edited("human draft".into()));
    assert!(call(
        &mut s,
        "notes.update_draft",
        json!({"path": path, "body": "stale", "revision": first["revision"]})
    )
    .unwrap_err()
    .contains("changed"));
    let current = read_draft(&mut s, path);
    disk_write(&s, path, b"new external edit");
    write_draft(&mut s, path, "agent after human", &current);
    assert_eq!(
        model::draft(s.store(), path).unwrap().original,
        "external edit"
    );
    editor(&mut s, slot, |p, s| {
        p.observe();
        assert_eq!(p.text, "agent after human");
        p.run("notes.save", s);
        assert!(p.error.contains("changed on disk"));
    });
    assert_eq!(
        model::draft(s.store(), path).unwrap().body,
        "agent after human"
    );
    assert_eq!(disk_read(&s, path), b"new external edit");
}

#[test]
fn open_editors_observe_agent_changes_and_save_is_explicit() {
    let mut s = Session::fake(APPS);
    let id = seed_note(&s);
    let slot = open(&mut s, Editor::note(id));
    let before = read_note(&mut s, id);
    call(
        &mut s,
        "notes.update",
        json!({"id": id, "body": "Agent text", "revision": before["revision"]}),
    )
    .unwrap();
    editor(&mut s, slot, |p, _| {
        p.observe();
        assert_eq!(p.text, "Agent text");
    });
    assert!(s.undo());
    editor(&mut s, slot, |p, _| {
        p.observe();
        assert_eq!(p.text, "Before");
    });

    let path = "~/save.md";
    disk_write(&s, path, b"before\r\n");
    let slot = open(&mut s, Editor::file(path));
    let before = read_draft(&mut s, path);
    let after = write_draft(&mut s, path, "after\n", &before);
    editor(&mut s, slot, |p, s| {
        p.observe();
        assert_eq!(p.text, "after\n");
        assert!(p.dirty());
        assert_eq!(disk_read(s, path), b"before\r\n");
        p.run("notes.save", s);
        assert!(p.error.is_empty());
    });
    assert_eq!(disk_read(&s, path), b"after\r\n");
    assert!(model::draft(s.store(), path).is_none());
    assert!(call(
        &mut s,
        "notes.update_draft",
        json!({"path": path, "body": "stale", "revision": after["revision"]})
    )
    .is_err());
    s.undo();
    assert_eq!(disk_read(&s, path), b"after\r\n");
    assert!(
        model::draft(s.store(), path).is_none(),
        "undo after Save cannot resurrect the saved draft"
    );
}

#[test]
fn draft_undo_and_redo_do_not_overwrite_later_typing() {
    let mut s = Session::fake(APPS);
    let path = "~/typing.md";
    disk_write(&s, path, b"before");
    let initial = read_draft(&mut s, path);
    let first = write_draft(&mut s, path, "first", &initial);
    write_draft(&mut s, path, "second", &first);
    assert!(s.undo());
    model::save_draft(
        s.store(),
        path.into(),
        model::Draft {
            original: "before".into(),
            body: "human".into(),
        },
        s.now(),
    )
    .unwrap();
    s.redo();
    assert_eq!(model::draft(s.store(), path).unwrap().body, "human");
    let current = read_draft(&mut s, path);
    write_draft(&mut s, path, "agent", &current);
    model::save_draft(
        s.store(),
        path.into(),
        model::Draft {
            original: "before".into(),
            body: "later human".into(),
        },
        s.now(),
    )
    .unwrap();
    s.undo();
    assert_eq!(model::draft(s.store(), path).unwrap().body, "later human");
    assert_eq!(disk_read(&s, path), b"before");
}

#[test]
fn reads_page_on_utf8_boundaries_and_share_paths_with_the_editor() {
    let mut s = Session::fake(APPS);
    let body = format!("{}🦀\nlast", "x".repeat(64 * 1024 - 1));
    let created = call(&mut s, "notes.create", json!({"body": body})).unwrap();
    let id = created["id"].as_i64().unwrap();
    let first = read_note(&mut s, id);
    let second = call(
        &mut s,
        "notes.read",
        json!({"id": id, "offset": first["next_offset"]}),
    )
    .unwrap();
    assert_eq!(first["next_offset"], 64 * 1024 - 1);
    assert_eq!(second["body"], "🦀\nlast");
    assert_eq!(second["next_offset"], Value::Null);
    assert_eq!(first["revision"], second["revision"]);
    assert_eq!(
        format!(
            "{}{}",
            first["body"].as_str().unwrap(),
            second["body"].as_str().unwrap()
        ),
        body
    );
    for offset in [json!(-1), json!(64 * 1024), json!(body.len() + 1)] {
        assert!(call(&mut s, "notes.read", json!({"id": id, "offset": offset})).is_err());
    }
    let path = "~/pages.md";
    disk_write(&s, path, body.as_bytes());
    let first = read_draft(&mut s, path);
    let absolute = real_path(path).to_string_lossy().into_owned();
    let second = call(
        &mut s,
        "notes.read_draft",
        json!({"path": absolute, "offset": first["next_offset"]}),
    )
    .unwrap();
    assert_eq!(first["revision"], second["revision"]);
    assert_eq!(second["body"], "🦀\nlast");
    write_draft(&mut s, &absolute, "changed", &first);
    assert_eq!(model::draft(s.store(), path).unwrap().body, "changed");
    assert_eq!(read_draft(&mut s, path)["path"], path);
}

#[test]
fn invalid_inputs_make_no_changes() {
    let mut s = Session::fake(APPS);
    for body in [
        "binary\0data".to_string(),
        "x".repeat(model::MAX_FILE_BYTES + 1),
    ] {
        assert!(call(&mut s, "notes.create", json!({"body": body})).is_err());
    }
    for path in ["relative.md", "~someone/file", "", "/a\0b"] {
        assert!(call(&mut s, "notes.read_draft", json!({"path": path})).is_err());
    }
    for bytes in [
        vec![0xff],
        b"binary\0data".to_vec(),
        vec![b'x'; model::MAX_FILE_BYTES + 1],
    ] {
        disk_write(&s, "~/binary", &bytes);
        assert!(call(&mut s, "notes.read_draft", json!({"path": "~/binary"})).is_err());
    }
    assert_eq!(s.history().head(), 0);
}

#[test]
fn the_agent_dispatches_a_note_tool_and_receives_its_result() {
    use crate::apps::agent::{
        self,
        fake::{Answer, Reply},
        model as chat, FakeGateway, AGENT,
    };
    static BUILD: &[&dyn App] = &[&NOTES, &AGENT];
    let mut s = Session::fake(BUILD);
    let gateway = s.world().caps(|c| c.get::<FakeGateway>().unwrap().clone());
    gateway.plant(vec![Reply::when(
        "write a note",
        Answer::Call {
            name: "notes.create".into(),
            arguments: json!({"body": "# Agent note\nSaved automatically"}),
            then: "Created.".into(),
        },
    )]);
    let created = std::rc::Rc::new(std::cell::Cell::new(None));
    let received = created.clone();
    chat::send(
        &mut s,
        None,
        "write a note",
        chat::Carried::default(),
        move |_, result| received.set(result),
    );
    let (id, _) = created.get().expect("fixture send completes");
    s.settle();
    let run = chat::latest_run(s.store(), id).unwrap();
    assert_eq!(run.status, chat::WAITING);
    assert!(gateway.requests()[0]
        .tools
        .iter()
        .any(|t| t.function.name == "notes.create"));
    assert_eq!(agent::calls::run_pending_calls(&mut s, id), 1);
    s.settle();
    let calls = chat::calls(s.store(), run.id);
    assert_eq!(calls[0].status, chat::CALL_DONE, "{}", calls[0].said());
    assert!(calls[0].said().contains("Agent note"));
    assert_eq!(model::NOTES.count(s.store(), None), Some(1));
    assert_eq!(chat::latest_run(s.store(), id).unwrap().status, chat::DONE);
}
