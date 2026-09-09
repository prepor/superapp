use super::*;
use crate::apps::telegram::{panel_read::Read, requests};

#[test]
fn panel_reads_resolve_through_the_account_and_disconnect_promptly() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
    acc.drain(&w);
    let request = last_request(&td, "getMessageAddedReactions");
    assert!(read.poll(w.now()).is_none());
    let result = json!({"@type": "addedReactions", "total_count": 1,
        "reactions": [{"type": {"@type": "reactionTypeEmoji", "emoji": "👍"},
            "sender_id": {"@type": "messageSenderUser", "user_id": 9}}],
        "next_offset": "", "@extra": request["@extra"]});
    acc.on_update(&w, &result.to_string());
    assert_eq!(read.poll(w.now()).unwrap().unwrap()["reactions"], result["reactions"]);

    let read = Read::start(w.store(), &requests::search_mention_members(7, 0, "an"), w.now());
    acc.drain(&w);
    let request = last_request(&td, "searchChatMembers");
    acc.on_update(&w, &json!({"@type": "error", "code": 403, "message": "CHAT_ACCESS_DENIED",
        "@extra": request["@extra"]}).to_string());
    assert_eq!(read.poll(w.now()).unwrap().unwrap_err(), "CHAT_ACCESS_DENIED");

    let read = Read::start(w.store(), &requests::search_mention_members(7, 0, "ve"), w.now());
    runtime::of(w.store()).disconnect();
    assert!(read.poll(w.now()).unwrap().unwrap_err().contains("disconnected"));
}

#[test]
fn canceled_panel_reads_are_retired_before_reaching_the_transport() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    let read = Read::start(w.store(), &requests::search_mention_members(7, 0, "old"), w.now());
    drop(read);
    acc.drain(&w);
    assert!(!td.sent_types().contains(&"searchChatMembers".to_string()));
    assert!(runtime::of(w.store()).operations.list().iter()
        .all(|operation| !operation.context().is_some_and(|context| context.starts_with("panel_read:"))));
}
