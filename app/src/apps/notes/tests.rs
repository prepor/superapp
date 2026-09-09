use super::{
    model,
    panels::{Editor, NoteList},
    NOTES, SCHEMA,
};
use kernel::app::App;
use kernel::caps::{real_path, Disk};
use kernel::nav::Nav;
use kernel::panel::{Panel, PanelId};
use kernel::richtable::{Datasource, ListState};
use kernel::session::{Action, Session};
use kernel::store::Store;

static APPS: &[&dyn App] = &[&NOTES];
#[path = "tool_tests.rs"]
mod tools;

fn open(s: &mut Session, id: PanelId) -> kernel::layout::SlotId {
    s.act(Action::new("open", "open editor").moving(move |wm| {
        wm.open(id, None, false);
    }));
    s.settle();
    s.focus().unwrap()
}
fn editor<R>(
    s: &mut Session,
    slot: kernel::layout::SlotId,
    f: impl FnOnce(&mut Editor, &mut Session) -> R,
) -> R {
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    f(borrow.as_any().downcast_mut::<Editor>().unwrap(), s)
}
fn disk_write(s: &Session, path: &str, bytes: &[u8]) {
    s.world()
        .with_cap::<dyn Disk, _>(|d| d.write_file(&real_path(path), bytes))
        .unwrap()
        .unwrap();
}
fn disk_read(s: &Session, path: &str) -> Vec<u8> {
    s.world()
        .with_cap::<dyn Disk, _>(|d| d.read_file(&real_path(path), model::MAX_FILE_BYTES + 1))
        .unwrap()
        .unwrap()
}

fn background_session() -> Session {
    use kernel::app::{Apps, Env, Mode, Workers};
    use kernel::caps::{DemoDisk, DiskFactory};
    use std::rc::Rc;

    let env = Env {
        disk: Some(DiskFactory::shared(DemoDisk::new(Default::default()))),
        ..Env::default()
    };
    let apps = Apps::new(APPS);
    let store = Store::open(None, &apps.schemas()).unwrap();
    let world = Rc::new(apps.world(store, Mode::Fake, &env));
    let workers = Workers::inline(APPS, world.clone());
    let session = Session::new(apps, world, workers, Mode::Fake);
    session.store().attach_ui(|| {});
    session
}

fn flush_editor_io(s: &Session) {
    kernel::runtime::block_on(super::io::flush(s.store().db()));
}

#[test]
fn background_save_completion_survives_later_typing_before_the_next_ui_poll() {
    let mut s = background_session();
    let path = "~/coalesced-save.md";
    disk_write(&s, path, b"original\r\n");
    let slot = open(&mut s, Editor::file(path));
    flush_editor_io(&s);
    editor(&mut s, slot, |p, s| {
        assert!(p.background(), "exercise the actual document I/O service");
        p.observe();
        assert!(p.available);
        p.edited("first\n".into());
        p.run("notes.save", s);
        p.edited("second\n".into());
        assert_eq!(p.status(), "saving file…");
    });

    // The watch channel retains only the later Edit presentation. The Save
    // completion must still reach the editor when it finally observes it.
    flush_editor_io(&s);
    assert_eq!(disk_read(&s, path), b"first\r\n");
    let draft = model::draft(s.store(), path).unwrap();
    assert_eq!(draft.original, "first\r\n");
    assert_eq!(draft.body, "second\r\n");
    editor(&mut s, slot, |p, s| {
        p.observe();
        assert_eq!(p.text, "second\n", "keep typing after the saved version");
        assert!(p.error.is_empty(), "{}", p.error);
        assert!(p.dirty());
        assert_eq!(p.status(), "draft saved · save to update file");
        p.run("notes.save", s);
        assert_eq!(p.status(), "saving file…", "a second save is accepted");
    });
    flush_editor_io(&s);
    editor(&mut s, slot, |p, _| {
        p.observe();
        assert!(!p.dirty());
        assert_eq!(p.status(), "saved to file");
        assert!(p.error.is_empty(), "{}", p.error);
    });
    assert_eq!(disk_read(&s, path), b"second\r\n");
    assert!(model::draft(s.store(), path).is_none());
}

#[test]
fn background_save_failure_survives_later_autosave_and_can_be_retried() {
    let mut s = background_session();
    let path = "~/coalesced-conflict.md";
    disk_write(&s, path, b"original");
    let slot = open(&mut s, Editor::file(path));
    flush_editor_io(&s);
    editor(&mut s, slot, |p, _| {
        assert!(p.background(), "exercise the actual document I/O service");
        p.observe();
        assert!(p.available);
    });
    disk_write(&s, path, b"changed elsewhere");
    editor(&mut s, slot, |p, s| {
        p.edited("first".into());
        p.run("notes.save", s);
        p.edited("second".into());
    });
    flush_editor_io(&s);
    editor(&mut s, slot, |p, _| {
        p.observe();
        assert_eq!(p.text, "second");
        assert!(p.error.contains("changed on disk"), "{}", p.error);
        assert!(p.dirty());
        assert_eq!(p.status(), "draft saved · save to update file");
    });
    assert_eq!(disk_read(&s, path), b"changed elsewhere");
    let draft = model::draft(s.store(), path).unwrap();
    assert_eq!(draft.original, "original");
    assert_eq!(draft.body, "second");

    // Restore the reviewed baseline so retry can safely write the newest
    // draft; observing the failed save must have released the pending state.
    disk_write(&s, path, b"original");
    editor(&mut s, slot, |p, s| {
        p.run("notes.save", s);
        assert_eq!(p.status(), "saving file…");
    });
    flush_editor_io(&s);
    editor(&mut s, slot, |p, _| {
        p.observe();
        assert!(p.error.is_empty(), "{}", p.error);
        assert!(!p.dirty());
        assert_eq!(p.status(), "saved to file");
    });
    assert_eq!(disk_read(&s, path), b"second");
    assert!(model::draft(s.store(), path).is_none());
}

#[test]
fn notes_autosave_filter_reopen_and_delete_with_undo() {
    let mut s = Session::fake(APPS);
    let id = model::create(&mut s).unwrap();
    let slot = open(&mut s, Editor::note(id));
    editor(&mut s, slot, |p, _| {
        assert!(p.verbs().is_empty(), "notes have no save action");
        p.edited("# Café\n\nA **bright** idea.".into());
        assert_eq!(p.title(), "Café");
        assert_eq!(p.status(), "saved automatically");
    });
    assert_eq!(
        model::body(s.store(), id).unwrap(),
        "# Café\n\nA **bright** idea."
    );
    let mut list = ListState::new(&model::NOTES, 50);
    list.set_filter("bright");
    assert_eq!(list.len(s.store()), 1, "the whole body is searchable");
    s.nav(Nav::Close { slot, label: None });
    s.settle();
    let slot = open(&mut s, Editor::note(id));
    editor(&mut s, slot, |p, _| {
        assert_eq!(p.text, "# Café\n\nA **bright** idea.")
    });
    assert!(model::delete(&mut s, vec![id]));
    editor(&mut s, slot, |p, _| {
        p.observe();
        assert!(!p.available);
    });
    assert_eq!(model::NOTES.count(s.store(), None), Some(0));
    s.undo();
    editor(&mut s, slot, |p, _| {
        p.observe();
        assert!(p.available);
    });
    assert_eq!(model::NOTES.count(s.store(), None), Some(1));
}

#[test]
fn creation_undo_retains_the_text_for_redo_and_typing_is_not_workspace_history() {
    let mut s = Session::fake(APPS);
    let id = model::create(&mut s).unwrap();
    model::edit(s.store(), id, "kept".into(), s.now()).unwrap();
    s.undo();
    assert!(model::body(s.store(), id).is_none());
    s.redo();
    assert_eq!(model::body(s.store(), id).as_deref(), Some("kept"));
}

#[test]
fn file_edits_stay_in_drafts_until_explicit_save_and_reopen_recovers_them() {
    let mut s = Session::fake(APPS);
    let path = "~/notes.md";
    disk_write(&s, path, b"original\n");
    let slot = open(&mut s, Editor::file(path));
    assert!(model::draft(s.store(), path).is_none());
    editor(&mut s, slot, |p, _| p.edited("changed **source**\n".into()));
    assert_eq!(disk_read(&s, path), b"original\n");
    assert_eq!(
        model::draft(s.store(), path).unwrap().body,
        "changed **source**\n"
    );
    s.nav(Nav::Close { slot, label: None });
    s.settle();
    let slot = open(&mut s, Editor::file(path));
    editor(&mut s, slot, |p, s| {
        assert_eq!(p.text, "changed **source**\n");
        assert!(p.dirty());
        p.run("notes.save", s);
        assert!(p.error.is_empty(), "{}", p.error);
        assert!(!p.dirty());
        p.edited("second draft".into());
    });
    assert_eq!(disk_read(&s, path), b"changed **source**\n");
    assert_eq!(
        model::draft(s.store(), path).unwrap().original,
        "changed **source**\n"
    );
}

#[test]
fn conflicts_and_failed_autosaves_preserve_the_draft_and_original_file() {
    let mut s = Session::fake(APPS);
    let path = "~/notes.md";
    disk_write(&s, path, b"original");
    let slot = open(&mut s, Editor::file(path));
    editor(&mut s, slot, |p, _| p.edited("my draft".into()));
    disk_write(&s, path, b"changed elsewhere");
    editor(&mut s, slot, |p, s| {
        p.run("notes.save", s);
        assert!(p.error.contains("changed on disk"));
        assert!(p.dirty());
        assert_eq!(p.text, "my draft");
    });
    assert_eq!(disk_read(&s, path), b"changed elsewhere");
    assert_eq!(model::draft(s.store(), path).unwrap().body, "my draft");

    let id = model::create(&mut s).unwrap();
    let note = open(&mut s, Editor::note(id));
    s.store().set_writable(false);
    editor(&mut s, note, |p, _| {
        p.edited("held in the editor".into());
        assert!(!p.error.is_empty());
        assert!(p.status().starts_with("not saved"));
        p.observe();
        assert_eq!(p.text, "held in the editor");
    });
    editor(&mut s, slot, |p, s| p.run("notes.save", s));
    assert_eq!(disk_read(&s, path), b"changed elsewhere");
    s.store().set_writable(true);
    editor(&mut s, note, |p, _| p.edited("held in the editor".into()));
    assert_eq!(
        model::body(s.store(), id).as_deref(),
        Some("held in the editor")
    );
}

#[test]
fn interrupted_save_can_retry_after_the_file_was_already_replaced() {
    let mut s = Session::fake(APPS);
    let path = "~/notes.md";
    disk_write(&s, path, b"original");
    let slot = open(&mut s, Editor::file(path));
    editor(&mut s, slot, |p, _| {
        p.edited("committed bytes, a longer file".into())
    });
    // Simulate the process stopping between the filesystem rename and the
    // transaction that removes the draft. Reopening must remain recoverable.
    disk_write(&s, path, b"committed bytes, a longer file");
    s.nav(Nav::Close { slot, label: None });
    s.settle();
    let slot = open(&mut s, Editor::file(path));
    editor(&mut s, slot, |p, s| {
        p.run("notes.save", s);
        assert!(p.error.is_empty(), "{}", p.error);
        assert!(!p.dirty());
    });
    assert!(model::draft(s.store(), path).is_none());
}

#[test]
fn file_read_is_complete_utf8_or_refused_and_line_endings_are_preserved() {
    let mut s = Session::fake(APPS);
    let path = "~/notes.md";
    for bytes in [
        vec![b'x'; model::MAX_FILE_BYTES + 1],
        vec![0xff],
        b"binary\0data".to_vec(),
    ] {
        disk_write(&s, path, &bytes);
        assert!(s.world().run(&model::ReadFile(path.into())).is_err());
        assert_eq!(disk_read(&s, path), bytes);
    }
    let original = "\u{feff}# Café\r\n\tline\r\n";
    disk_write(&s, path, original.as_bytes());
    let slot = open(&mut s, Editor::file(path));
    editor(&mut s, slot, |p, s| {
        assert_eq!(p.text, "# Café\n\tline\n");
        assert!(!p.dirty());
        p.edited("# Café\n\tchanged\n".into());
        p.run("notes.save", s);
    });
    assert_eq!(
        disk_read(&s, path),
        "\u{feff}# Café\r\n\tchanged\r\n".as_bytes()
    );
}

#[test]
fn mixed_line_endings_survive_edits_draft_recovery_and_save() {
    let original = "\u{feff}# Café\r\nfirst\nsecond\r\nthird\nlast";
    for (edited, expected) in [
        (
            "# Café!\nfirst\nsecond\nthird\nlast",
            "\u{feff}# Café!\r\nfirst\nsecond\r\nthird\nlast",
        ),
        (
            "# Café\nfirst!\nsecond\nthird!\nlast",
            "\u{feff}# Café\r\nfirst!\nsecond\r\nthird!\nlast",
        ),
        (
            "intro\n# Café\nfirst\nsecond\nthird\nlast",
            "\u{feff}intro\r\n# Café\r\nfirst\nsecond\r\nthird\nlast",
        ),
        (
            "first\nsecond\nthird\nlast",
            "\u{feff}first\nsecond\r\nthird\nlast",
        ),
        (
            "# Café\nfirst\nsecond\ninserted\nthird\nlast",
            "\u{feff}# Café\r\nfirst\nsecond\r\ninserted\nthird\nlast",
        ),
        (
            "# Café\nfirst\nsecond\nthird\nlast\n",
            "\u{feff}# Café\r\nfirst\nsecond\r\nthird\nlast\n",
        ),
    ] {
        let mut s = Session::fake(APPS);
        let path = "~/notes.md";
        disk_write(&s, path, original.as_bytes());
        let slot = open(&mut s, Editor::file(path));
        editor(&mut s, slot, |p, _| {
            assert_eq!(p.text, "# Café\nfirst\nsecond\nthird\nlast");
            assert!(!p.dirty());
            p.edited(edited.into());
            assert!(p.dirty());
        });
        assert_eq!(disk_read(&s, path), original.as_bytes());
        assert_eq!(model::draft(s.store(), path).unwrap().body, expected);
        s.nav(Nav::Close { slot, label: None });
        s.settle();
        let slot = open(&mut s, Editor::file(path));
        editor(&mut s, slot, |p, s| {
            assert_eq!(p.text, edited);
            assert!(p.dirty());
            // Undoing to the original source also restores its exact endings.
            p.edited("# Café\nfirst\nsecond\nthird\nlast".into());
            assert!(!p.dirty());
            assert!(model::draft(s.store(), path).is_none());
            p.edited(edited.into());
            p.run("notes.save", s);
            assert!(p.error.is_empty(), "{}", p.error);
            assert!(!p.dirty());
        });
        assert_eq!(disk_read(&s, path), expected.as_bytes());
        assert!(model::draft(s.store(), path).is_none());
        // A subsequent edit uses the saved file's endings as its baseline.
        editor(&mut s, slot, |p, s| {
            p.edited(format!("!{}", p.text));
            p.run("notes.save", s);
            assert!(p.error.is_empty(), "{}", p.error);
        });
        assert_eq!(
            disk_read(&s, path),
            format!("\u{feff}!{}", expected.trim_start_matches('\u{feff}')).as_bytes()
        );
    }
}

#[test]
fn notes_and_drafts_survive_a_database_restart() {
    let path = std::env::temp_dir().join(format!("superapp-notes-{}.db", std::process::id()));
    {
        let store = Store::open(Some(&path), &[&SCHEMA]).unwrap();
        store
            .write(|c| {
                c.execute(
                    "INSERT INTO notes_note(id,created,modified) VALUES(1,1,1)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        model::edit(&store, 1, "persistent note".into(), 2.0).unwrap();
        model::save_draft(
            &store,
            "~/draft.txt".into(),
            model::Draft {
                original: "base".into(),
                body: "persistent draft".into(),
            },
            3.0,
        )
        .unwrap();
    }
    {
        let store = Store::open(Some(&path), &[&SCHEMA]).unwrap();
        assert_eq!(model::body(&store, 1).as_deref(), Some("persistent note"));
        assert_eq!(
            model::draft(&store, "~/draft.txt").unwrap().body,
            "persistent draft"
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn multiple_open_editors_observe_the_same_note_and_draft() {
    let mut s = Session::fake(APPS);
    let id = model::create(&mut s).unwrap();
    let a = open(&mut s, Editor::note(id));
    let b = open(&mut s, Editor::note(id));
    editor(&mut s, a, |p, _| p.edited("same note".into()));
    editor(&mut s, b, |p, _| {
        p.observe();
        assert_eq!(p.text, "same note");
    });
    let a = open(&mut s, Editor::file("~/notes.md"));
    let b = open(&mut s, Editor::file("~/notes.md"));
    editor(&mut s, a, |p, _| p.edited("same draft".into()));
    editor(&mut s, b, |p, s| {
        p.observe();
        assert_eq!(p.text, "same draft");
        p.run("notes.save", s);
    });
    editor(&mut s, a, |p, _| {
        p.observe();
        assert!(!p.dirty());
        assert_eq!(p.text, "same draft");
    });
}

#[test]
fn file_cards_offer_the_editor_only_when_the_app_is_registered() {
    use crate::apps::files::{Card, FILES};
    static WITH: &[&dyn App] = &[&FILES, &NOTES];
    static WITHOUT: &[&dyn App] = &[&FILES];
    for (apps, offered) in [(WITH, true), (WITHOUT, false)] {
        let mut s = Session::fake(apps);
        let slot = open(&mut s, Card::id("~/notes.md"));
        assert_eq!(
            s.panel(slot)
                .unwrap()
                .borrow()
                .verbs()
                .iter()
                .any(|v| v.id == "files.edit"),
            offered
        );
        let slot = open(&mut s, Card::id("~/Downloads/report-q3.pdf"));
        assert!(!s
            .panel(slot)
            .unwrap()
            .borrow()
            .verbs()
            .iter()
            .any(|v| v.id == "files.edit"));
    }
    assert_eq!(NOTES.roots()[0].id, NoteList::id());
}
