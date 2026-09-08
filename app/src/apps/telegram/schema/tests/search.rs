use crate::apps::telegram::{schema::SCHEMA, search_index};
use kernel::app::Schema;
use rusqlite::Connection;

#[test]
fn substring_index_upgrades_cached_history_and_tracks_message_identity() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY)").unwrap();
    Schema { app: "telegram", steps: &SCHEMA.steps[..14] }.apply(&c).unwrap();
    c.execute_batch("INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'One'), (20, 'group', 'Two');
        INSERT INTO tg_chat(peer) VALUES(10), (20);
        INSERT INTO tg_message(id, chat, date, text) VALUES(1, 10, 1, 'thermos'), (1, 20, 2, 'Straße');
        INSERT INTO tg_message(id, chat, date, text, service) VALUES(2, 10, 3, 'thermos', 1);").unwrap();
    SCHEMA.apply(&c).unwrap();

    let chats = |query: &str| -> Vec<i64> {
        let q = search_index::predicate(query).unwrap();
        c.prepare(&format!("SELECT m.chat FROM tg_message m WHERE {} ORDER BY m.chat", q.sql))
            .unwrap().query_map(rusqlite::params_from_iter(q.params), |r| r.get(0))
            .unwrap().collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(chats("rm"), [10]);
    assert_eq!(chats("SS"), [20]);
    assert_eq!(chats("ße"), [20]);
    let changed = c.total_changes();
    SCHEMA.apply(&c).unwrap();
    assert_eq!(c.total_changes(), changed, "reopening does not rebuild the index");

    c.execute_batch("UPDATE tg_message SET seq = 1000 WHERE chat = 10 AND id = 1;
        UPDATE tg_message SET text = 'teapot' WHERE chat = 20 AND id = 1;
        UPDATE tg_message SET service = 0 WHERE chat = 10 AND id = 2;").unwrap();
    assert_eq!(chats("rm"), [10, 10]);
    assert!(chats("SS").is_empty());
    assert_eq!(chats("pot"), [20]);

    c.execute_batch("BEGIN; DELETE FROM tg_message WHERE chat = 10; ROLLBACK;").unwrap();
    assert_eq!(chats("rm"), [10, 10], "rollback restores index entries too");
    c.execute_batch("UPDATE tg_message SET service = 1 WHERE chat = 10 AND id = 2;
        DELETE FROM tg_message WHERE chat = 10 AND id = 1;").unwrap();
    assert!(chats("rm").is_empty());
}

#[test]
fn substring_index_repairs_stale_grams_from_older_writers_once() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY)").unwrap();
    SCHEMA.apply(&c).unwrap();
    // Reproduce a store made by the old writer, including an orphan posting
    // and a replaced row whose key was reused. No repair marker existed yet.
    c.pragma_update(None, "recursive_triggers", false).unwrap();
    c.execute_batch("DELETE FROM meta WHERE key = 'telegram:message-substr';
        INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'One');
        INSERT INTO tg_chat(peer) VALUES(10);
        INSERT INTO tg_message(seq, id, chat, date, text)
            VALUES(1, 1, 10, 1, 'teapot'), (2, 2, 10, 2, 'teapot');
        INSERT OR REPLACE INTO tg_message(seq, id, chat, date, text)
            VALUES(1, 1, 10, 1, 'coffee'), (3, 2, 10, 2, 'coffee');").unwrap();
    let count = |text: &str| -> i64 {
        let q = search_index::predicate(text).unwrap();
        c.query_row("SELECT COUNT(*) FROM tg_message_substr WHERE tg_message_substr MATCH ?",
            rusqlite::params_from_iter(&q.params), |r| r.get(0)).unwrap()
    };
    assert_eq!(count("pot"), 2, "old REPLACE retained both posting lists");
    SCHEMA.apply(&c).unwrap();
    assert_eq!(count("pot"), 0);
    assert_eq!(count("fee"), 2);
    let changed = c.total_changes();
    SCHEMA.apply(&c).unwrap();
    assert_eq!(c.total_changes(), changed, "repair only runs once");
}
