use super::*;
use serde_json::json;

#[test]
fn file_viewer_downloads_on_demand_and_uses_the_cached_file_offline() {
    use crate::shell::widgets::viewer::Preview;
    use kernel::caps::Blobs;
    use super::super::downloads;
    let mut s = session();
    let td = FakeTd::new();
    let acc = sync::Account::new(td.clone(), 17844, std::env::temp_dir().join("tg-viewer"), None);
    acc.drain(s.world());
    acc.on_update(s.world(), &json!({"@type": "updateNewMessage", "message": {
        "@type": "message", "chat_id": ANNA, "id": 4242, "date": s.now(),
        "content": {"@type": "messageDocument", "document": {"file_name": "report.PDF",
            "document": {"id": 77, "remote": {"id": "remote", "unique_id": "doc"}}}}
    }}).to_string());
    assert!(td.sent().is_empty(), "receiving a document must not fetch it");
    let rt = runtime::of(s.store());
    let slot = open_root(&mut s, Viewer::id(ANNA, 4242));
    let inbox = rt.connect();
    let instance = s.panel(slot).unwrap();
    let mut panel = instance.borrow_mut();
    let viewer = panel.as_any().downcast_mut::<Viewer>().unwrap();
    let m = viewer.msg().unwrap();
    assert!(matches!(viewer.file_preview(&m).1, Preview::Loading(_)));
    let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "getMessage");
    assert_eq!(request["@extra"]["context"], format!("cache:{ANNA}:4242"));
    viewer.file_preview(&m);
    assert!(inbox.try_recv().is_err(), "redraws share a single download");
    let reference = downloads::reference(&m).unwrap();
    let path = s.world().with_cap::<dyn Blobs, _>(|b| b.put(reference, kernel::caps::demo::PDF)).unwrap().unwrap();
    rt.disconnect();
    let (_, preview) = viewer.file_preview(&m);
    assert!(matches!(preview, Preview::Path { path: cached, name, kind: kernel::caps::FileKind::Pdf, .. }
        if cached == path && name == "report.PDF"));
    assert!(inbox.try_recv().is_err(), "opening a cached document needs no connection");
}

#[test]
fn file_download_is_available_in_the_chat_card_and_viewer_and_survives_closing_them() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = sync::Account::new(
        td.clone(),
        17844,
        std::env::temp_dir().join("tg-download-ui"),
        None,
    );
    acc.drain(s.world());
    acc.on_update(
        s.world(),
        &json!({"@type": "updateNewMessage", "message": {
            "@type": "message", "chat_id": ANNA, "id": 4242, "date": s.now(),
            "content": {"@type": "messageDocument", "document": {"file_name": "report.pdf",
                "document": {"id": 77, "remote": {"id": "remote", "unique_id": "doc"}}}}
        }})
        .to_string(),
    );
    assert!(
        td.sent().is_empty(),
        "documents are never downloaded on arrival"
    );
    let chat = open_root(&mut s, Chat::id(ANNA));
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), 4242)));
    let card = open_root(&mut s, Line::id(ANNA, 4242));
    let viewer = open_root(&mut s, Viewer::id(ANNA, 4242));
    for slot in [chat, card, viewer] {
        assert!(verb_ids(&s, slot).contains(&"telegram.download"));
        verb(&mut s, slot, "telegram.download");
        go(&mut s, Nav::Close { slot, label: None });
    }
    let rt = runtime::of(s.store());
    let context = requests::save_context(ANNA, 4242);
    assert_eq!(
        rt.operations
            .list()
            .iter()
            .filter(|op| op.context() == Some(&context))
            .count(),
        1,
        "repeated download presses share the pending operation"
    );
    acc.drain(s.world());
    assert!(
        !td.sent_types().contains(&"getMessage".into()),
        "uncached downloads wait for authorization"
    );
    acc.on_update(
        s.world(),
        &json!({"@type": "updateAuthorizationState",
        "authorization_state": {"@type": "authorizationStateReady"}})
        .to_string(),
    );
    acc.on_update(
        s.world(),
        &json!({"@type": "updateNewChat", "chat": {
            "id": ANNA, "title": "Anna", "type": {"@type": "chatTypePrivate", "user_id": ANNA}
        }})
        .to_string(),
    );
    acc.drain(s.world());
    let request = td
        .sent()
        .iter()
        .map(|r| serde_json::from_str::<serde_json::Value>(r).unwrap())
        .find(|r| r["@extra"]["context"] == context)
        .expect("the closed panels' download starts when ready");
    assert_eq!(request["@type"], "getMessage");
    assert_eq!(request["message_id"], 4242);
}

#[test]
fn text_locations_and_demo_media_have_no_download_action() {
    let mut s = session();
    let messages = model::history(s.store(), STELAXIS).to_vec();
    let mut location = messages[0].clone();
    location.media = Some(model::Media::of("location"));
    assert!(super::super::downloads::reference(&location).is_none());
    for m in messages {
        let line = open_root(&mut s, Line::id(m.chat, m.id));
        assert!(!verb_ids(&s, line).contains(&"telegram.download"));
        go(
            &mut s,
            Nav::Close {
                slot: line,
                label: None,
            },
        );
    }
}
