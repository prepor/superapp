//! An upgraded conversation has two server histories. Keep their identities
//! intact and persist only the link Telegram supplies, never infer it by title.

use kernel::store::{Store, Val, Q};
use rusqlite::Connection;
use serde_json::Value;

use super::model::PeerId;

pub const SUPERGROUP_BASE: i64 = -1_000_000_000_000;

static Q_ORIGINAL: Q = Q {
    id: "tg original group",
    sql: "SELECT old_chat FROM tg_chat_upgrade WHERE new_chat = ?1",
    describe: "the basic group whose history precedes this supergroup",
};

pub fn original(store: &Store, chat: PeerId) -> Option<PeerId> {
    store.rows(&Q_ORIGINAL, &[Val::I(chat)], |r| r.get(0)).first().copied()
}

pub fn record(c: &Connection, old: PeerId, new: PeerId) -> rusqlite::Result<()> {
    c.execute("INSERT INTO tg_chat_upgrade(old_chat, new_chat) VALUES(?1, ?2)
        ON CONFLICT(old_chat) DO UPDATE SET new_chat = excluded.new_chat
        WHERE new_chat != excluded.new_chat", [old, new])?;
    Ok(())
}

/// Metadata can arrive before either chat, or only with a history page.
pub fn decode(v: &Value) -> Vec<(PeerId, PeerId)> {
    let pair = match v["@type"].as_str().unwrap_or("") {
        "updateSupergroupFullInfo" => v["supergroup_full_info"]["upgraded_from_basic_group_id"]
            .as_i64().zip(v["supergroup_id"].as_i64()).map(|(old, new)| (-old, SUPERGROUP_BASE - new)),
        "supergroupFullInfo" => v["upgraded_from_basic_group_id"].as_i64()
            .zip(v["@extra"].as_str().and_then(|s| s.strip_prefix("upgrade_info:")?.parse::<i64>().ok()))
            .map(|(old, new)| (-old, new)),
        "updateBasicGroup" => v["basic_group"]["id"].as_i64()
            .zip(v["basic_group"]["upgraded_to_supergroup_id"].as_i64())
            .filter(|(_, new)| *new > 0).map(|(old, new)| (-old, SUPERGROUP_BASE - new)),
        "basicGroup" => v["id"].as_i64().zip(v["upgraded_to_supergroup_id"].as_i64())
            .filter(|(_, new)| *new > 0).map(|(old, new)| (-old, SUPERGROUP_BASE - new)),
        "updateNewMessage" | "updateMessageSendSucceeded" => return decode(&v["message"]),
        "updateNewChat" => return decode(&v["chat"]),
        "chat" | "updateChatLastMessage" => return decode(&v["last_message"]),
        "messages" | "foundChatMessages" => return v["messages"].as_array().into_iter()
            .flatten().flat_map(decode).collect(),
        "message" => match v["content"]["@type"].as_str() {
            Some("messageChatUpgradeFrom") => v["content"]["basic_group_id"].as_i64()
                .zip(v["chat_id"].as_i64()).map(|(old, new)| (-old, new)),
            Some("messageChatUpgradeTo") => v["chat_id"].as_i64()
                .zip(v["content"]["supergroup_id"].as_i64()).filter(|(_, new)| *new > 0)
                .map(|(old, new)| (old, SUPERGROUP_BASE - new)),
            _ => None,
        },
        _ => None,
    };
    pair.filter(|(old, new)| *old < 0 && *old > SUPERGROUP_BASE && *new < SUPERGROUP_BASE)
        .into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn telegram_metadata_and_service_messages_link_the_same_two_histories() {
        let pair = vec![(-123, SUPERGROUP_BASE - 456)];
        for update in [
            json!({"@type": "updateBasicGroup", "basic_group": {"id": 123, "upgraded_to_supergroup_id": 456}}),
            json!({"@type": "updateSupergroupFullInfo", "supergroup_id": 456,
                "supergroup_full_info": {"upgraded_from_basic_group_id": 123}}),
            json!({"@type": "updateNewMessage", "message": {"@type": "message", "chat_id": -123,
                "content": {"@type": "messageChatUpgradeTo", "supergroup_id": 456}}}),
            json!({"@type": "message", "chat_id": SUPERGROUP_BASE - 456,
                "content": {"@type": "messageChatUpgradeFrom", "basic_group_id": 123}}),
        ] { assert_eq!(decode(&update), pair); }
        for update in [
            json!({"@type": "updateBasicGroup", "basic_group": {"id": 123, "upgraded_to_supergroup_id": 0}}),
            json!({"@type": "updateSupergroupFullInfo", "supergroup_id": 456,
                "supergroup_full_info": {"upgraded_from_basic_group_id": 0}}),
            json!({"@type": "message", "chat_id": SUPERGROUP_BASE - 456,
                "content": {"@type": "messageChatUpgradeFrom"}}),
        ] { assert!(decode(&update).is_empty()); }
    }

    #[test]
    fn schema_upgrade_preserves_cached_history_drafts_and_persisted_links() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY)").unwrap();
        let old = kernel::app::Schema { app: "telegram", steps: &super::super::schema::SCHEMA.steps[..16] };
        old.apply(&c).unwrap();
        c.execute_batch("INSERT INTO tg_peer(id,kind,name) VALUES(-123,'group','Old group');
            INSERT INTO tg_chat(peer,draft) VALUES(-123,'unsent');
            INSERT INTO tg_message(chat,id,date,text,content_type) VALUES(-123,42,1,'kept','messageText');
            INSERT INTO tg_message(chat,id,date,text,content_type) VALUES(-123,43,2,'ChatUpgradeTo','messageChatUpgradeTo')").unwrap();
        super::super::schema::SCHEMA.apply(&c).unwrap();
        record(&c, -123, SUPERGROUP_BASE - 456).unwrap();
        c.execute("UPDATE tg_message SET reply_to = 42, reply_chat = -123 WHERE id = 43", []).unwrap();
        super::super::schema::SCHEMA.apply(&c).unwrap();
        assert_eq!(c.query_row("SELECT draft FROM tg_chat WHERE peer = -123", [], |r| r.get::<_, String>(0)).unwrap(), "unsent");
        assert_eq!(c.query_row("SELECT text FROM tg_message WHERE id = 42", [], |r| r.get::<_, String>(0)).unwrap(), "kept");
        assert!(c.query_row("SELECT service FROM tg_message WHERE id = 43", [], |r| r.get::<_, bool>(0)).unwrap());
        assert_eq!(c.query_row("SELECT old_chat FROM tg_chat_upgrade", [], |r| r.get::<_, i64>(0)).unwrap(), -123);
        assert_eq!(c.query_row("SELECT reply_chat FROM tg_message WHERE id = 43", [], |r| r.get::<_, i64>(0)).unwrap(), -123);
    }
}
