use crate::apps::telegram::{schema::SCHEMA, search_index};
use kernel::app::Schema;
use rusqlite::Connection;

#[test]
fn substring_index_upgrades_cached_history_and_tracks_message_identity() {
    let c = Connection::open_in_memory().unwrap();
    c.pragma_update(None, "recursive_triggers", true).unwrap();
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
    c.execute_batch("INSERT OR REPLACE INTO tg_message(seq, id, chat, date, text)
        SELECT seq, id, chat, date, 'coffee' FROM tg_message WHERE chat = 20;").unwrap();
    assert!(chats("pot").is_empty());
    assert_eq!(chats("fee"), [20]);
}
