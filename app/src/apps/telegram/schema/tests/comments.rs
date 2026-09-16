//! The rung that made room for a post's comments: a store from before it
//! keeps everything it held, and ends with the columns a fresh install has.

use crate::apps::telegram::{model, model::Scope, schema, threads};
use kernel::{
    app::{Schema, Step},
    store::Store,
};

/// The oldest shape a store can be in: the ladder's published SQL, before
/// any of the repairs. Opening it once gives the file the kernel's own
/// tables, which the rungs are then applied over.
static FIRST: Schema = Schema {
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
        "superapp-comments-schema-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("store.db")
}

#[test]
fn a_store_from_before_the_rung_keeps_its_lines_and_gains_the_comments() {
    let path = path("upgrade");
    drop(Store::open(Some(&path), &[&FIRST], kernel::sync::Device::fake()).unwrap());
    {
        // Every rung up to — but not including — the one that made room for
        // a post's comments: where a store built by the previous build
        // stands.
        let c = rusqlite::Connection::open(&path).unwrap();
        Schema { app: "telegram", steps: &schema::SCHEMA.steps[..19] }.apply(&c).unwrap();
        assert!(!schema::columns(&c, "tg_message").unwrap().contains("thread"));
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name) VALUES(-70, 'channel', 'Rust Weekly');
             INSERT INTO tg_chat(peer, draft) VALUES(-70, 'half a post');
             INSERT INTO tg_message(id, chat, date, text, comments, entities_known,
                                    unread_mention)
                VALUES(5, -70, 1, 'a post', 8, 1, 0);",
        )
        .unwrap();
    }
    let upgraded = Store::open(Some(&path), &[&schema::SCHEMA], kernel::sync::Device::fake()).unwrap();
    let line = model::line(&upgraded, -70, 5).expect("the post is still there");
    assert_eq!((line.text.as_str(), line.comments, line.thread), ("a post", Some(8), 0));
    assert!(!line.comments_new, "nothing is known about its comments yet");
    assert_eq!(model::peer(&upgraded, -70).unwrap().draft.as_deref(), Some("half a post"));
    assert_eq!(model::peer(&upgraded, -70).unwrap().linked, None);
    assert!(threads::get(&upgraded, -70, 5).is_none());
    // The new table takes a thread, and the transcript can be scoped to it.
    upgraded
        .write(|c| {
            c.execute(
                "INSERT INTO tg_thread(chat, post, group_id, root, count, last, last_read)
                 VALUES(-70, 5, -71, 9, 8, 12, 10)",
                [],
            )
            .map(|_| ())
        })
        .unwrap();
    let thread = threads::get(&upgraded, -70, 5).expect("the thread row");
    assert_eq!(thread.where_it_is(), Some((-71, 9)));
    assert!(model::history_in(&upgraded, -71, Scope::Thread(9)).is_empty());
    assert!(model::line(&upgraded, -70, 5).unwrap().comments_new);

    // An upgraded store and a fresh one must agree about what a row is:
    // the rung sits inside the range the column-order repair builds its
    // canonical from, so both end with the same columns in the same places.
    let fresh = Store::open(None, &[&schema::SCHEMA], kernel::sync::Device::fake()).unwrap();
    for table in ["tg_peer", "tg_message", "tg_thread"] {
        let names = |s: &Store| {
            s.conn()
                .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .unwrap()
                .query_map([table], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(names(&upgraded), names(&fresh), "{table} either way");
    }
    drop(upgraded);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
