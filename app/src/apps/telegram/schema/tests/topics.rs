use crate::apps::telegram::{model, schema, seed, topics};
use kernel::{
    app::{Schema, Step},
    richtable::Datasource,
    store::Store,
};

static BEFORE_TOPICS: Schema = Schema {
    app: "telegram",
    steps: &[
        Step::Sql(schema::V1),
        Step::Sql(schema::V2),
        Step::Sql(schema::V3),
        Step::Run(schema::v4_media_columns),
        Step::Sql(schema::V5),
        Step::Sql(schema::V6),
        Step::Run(schema::v7_listing),
        Step::Sql(schema::V8),
    ],
};

fn path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "superapp-topic-schema-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("store.db")
}

#[test]
fn another_builds_migration_counter_cannot_hide_existing_chats() {
    for progress in [9, 10, 30] {
        let path = path(&format!("counter-{progress}"));
        {
            let store = Store::open(Some(&path), &[&BEFORE_TOPICS]).unwrap();
            store.write(move |c| {
                c.execute_batch("ALTER TABLE tg_message ADD COLUMN entities TEXT NOT NULL DEFAULT '[]';
                    INSERT INTO tg_peer(id, kind, name) VALUES(42, 'person', 'Existing chat');
                    INSERT INTO tg_chat(peer, in_main, unread, draft) VALUES(42, 1, 7, 'saved draft');
                    INSERT INTO tg_message(id, chat, date, text) VALUES(1, 42, 1.0, 'kept message');")?;
                c.execute("UPDATE meta SET value = ?1 WHERE key = 'schema:telegram'", [progress])?;
                Ok(())
            }).unwrap();
        }
        {
            let store = Store::open(Some(&path), &[&schema::SCHEMA]).unwrap();
            let chats = model::chats(false).page(&store, None, 0, 10);
            assert_eq!(chats.len(), 1, "the shared counter was {progress}");
            assert_eq!(chats[0].title, "Existing chat");
            assert_eq!(chats[0].last_text, "kept message");
            assert_eq!(chats[0].unread, 7);
            assert_eq!(chats[0].draft.as_deref(), Some("saved draft"));
            let entities: String = store
                .conn()
                .query_row(
                    "SELECT entities FROM tg_message WHERE chat = 42 AND id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(entities, "[]", "the other build's column survives");
            assert_eq!(
                schema::SCHEMA.progress(store.conn()).unwrap(),
                progress.max(13)
            );
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

#[test]
fn early_topic_builds_upgrade_with_link_metadata_and_block_state() {
    for progress in [9, 10, 12] {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY)")
            .unwrap();
        BEFORE_TOPICS.apply(&c).unwrap();
        schema::v13_topic_schema(&c).unwrap();
        c.execute_batch("INSERT INTO tg_peer(id, kind, name, is_forum) VALUES(42, 'group', 'Forum', 1);
            INSERT INTO tg_chat(peer, in_main) VALUES(42, 1);
            INSERT INTO tg_topic(chat, id, name, selected, draft) VALUES(42, 2, 'Meetups', 1, 'saved draft');
            INSERT INTO tg_message(id, chat, date, text, topic) VALUES(1, 42, 1.0, 'kept message', 2);").unwrap();
        c.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema:telegram'",
            [progress],
        )
        .unwrap();

        schema::SCHEMA.apply(&c).unwrap();
        assert_eq!(schema::SCHEMA.progress(&c).unwrap(), 13);
        let saved: (bool, String, i64, String, bool, bool) = c
            .query_row(
                "SELECT t.selected, t.draft, m.topic, m.entities, m.entities_known, p.blocked
             FROM tg_topic t JOIN tg_message m ON m.chat = t.chat AND m.topic = t.id
             JOIN tg_peer p ON p.id = t.chat WHERE t.chat = 42 AND t.id = 2",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            saved,
            (true, "saved draft".into(), 2, "[]".into(), false, false)
        );

        c.execute_batch(
            "UPDATE tg_peer SET blocked = 1 WHERE id = 42;
            UPDATE tg_message SET entities_known = 1 WHERE chat = 42;",
        )
        .unwrap();
        schema::SCHEMA.apply(&c).unwrap();
        let kept: (bool, bool) = c.query_row(
            "SELECT p.blocked, m.entities_known FROM tg_peer p JOIN tg_message m ON m.chat = p.id WHERE p.id = 42",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(kept, (true, true));
    }
}

#[test]
fn repairing_an_incomplete_topic_schema_preserves_selection_and_drafts() {
    let path = path("partial");
    {
        let store = Store::open(Some(&path), &[&schema::SCHEMA]).unwrap();
        seed::seed_if_empty(&store).unwrap();
        store
            .write(|c| {
                topics::select_tx(c, seed::BERLIN, &[2], true)?;
                topics::draft_tx(c, seed::BERLIN, 2, "coffee draft")?;
                c.execute(
                    "UPDATE tg_topic SET pinned = 1, archived = 1 WHERE chat = ?1 AND id = 2",
                    [seed::BERLIN],
                )?;
                c.execute(
                    "UPDATE tg_chat SET muted = 1 WHERE peer = ?1",
                    [seed::BERLIN],
                )?;
                Ok(())
            })
            .unwrap();
    }
    {
        // Model a different build's schema between opens, before a store's
        // changeset recorder has attached to the table's current shape.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "DROP VIEW tg_dialog; DROP INDEX tg_message_topic;
             ALTER TABLE tg_topic DROP COLUMN mute_default;
             UPDATE meta SET value = 30 WHERE key = 'schema:telegram'",
        )
        .unwrap();
    }
    {
        let store = Store::open(Some(&path), &[&schema::SCHEMA]).unwrap();
        let meetup = topics::get(&store, seed::BERLIN, 2).unwrap();
        assert!(meetup.selected && meetup.archived && meetup.muted);
        assert_eq!(meetup.pinned, 1);
        assert_eq!(meetup.draft.as_deref(), Some("coffee draft"));
        assert!(model::chats(true)
            .page(&store, None, 0, 20)
            .iter()
            .any(|row| row.peer == seed::BERLIN && row.topic == 2));
    }
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn opening_a_complete_topic_schema_does_not_rewrite_it() {
    let path = path("complete");
    let version: i64;
    {
        let store = Store::open(Some(&path), &[&schema::SCHEMA]).unwrap();
        seed::seed_if_empty(&store).unwrap();
        store
            .write(|c| topics::select_tx(c, seed::BERLIN, &[2], true))
            .unwrap();
        version = store
            .conn()
            .query_row("PRAGMA schema_version", [], |r| r.get(0))
            .unwrap();
    }
    {
        let store = Store::open(Some(&path), &[&schema::SCHEMA]).unwrap();
        let after: i64 = store
            .conn()
            .query_row("PRAGMA schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, after);
        assert!(topics::get(&store, seed::BERLIN, 2).unwrap().selected);
    }
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
