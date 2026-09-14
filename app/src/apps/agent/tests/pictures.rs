//! A picture a tool found, on its way to a model that can look at one.
//!
//! The demo disk answers every `.png` path with the app's own icon, so
//! `files.read` is the one road in this build that yields real picture bytes
//! with no server and no TDLib behind it. What these check is the road, not
//! the icon: the round is found, the turn is placed, the bytes are there,
//! and a chat on a model that cannot see sends exactly what it always did.

use super::*;
use crate::apps::agent::wire::{Content, Part};

const SEEING: &str = "gpt-5.6-sol";
const SHOT: &str = "~/Downloads/screenshot-2026-08-30.png";

fn reads_the_screenshot() -> Script {
    vec![Reply::always(Answer::Call {
        name: "files.read".into(),
        arguments: json!({"path": SHOT}),
        then: "It is a small icon.".into(),
    })]
}

/// Opens a chat on `model`, asks it something, and lets the round run.
fn asked(s: &mut Session, model: &str) -> ChatId {
    let slot = open_root(s, Chat::new_id());
    choose_model(s, slot, model);
    with_chat(s, slot, |c| c.set_draft("what is in this screenshot?"));
    verb(s, slot, "agent.send");
    // The inline scheduler advances only when a test ticks it; production
    // workers follow the immediate wake returned after a tool-call turn.
    s.workers().tick();
    s.settle();
    with_chat(s, slot, |c| c.chat().expect("the chat the send made"))
}

/// The one picture a request carries, as its data URL.
fn only_picture(req: &ChatRequest) -> String {
    let shown: Vec<&Message> = req
        .messages
        .iter()
        .filter(|m| m.parts().is_some())
        .collect();
    assert_eq!(shown.len(), 1, "one round, one turn carrying its pictures");
    let parts = shown[0].parts().expect("parts");
    assert!(
        matches!(parts.first(), Some(Part::Text { .. })),
        "the line that says what follows comes first"
    );
    let urls: Vec<&str> = parts
        .iter()
        .filter_map(|p| match p {
            Part::ImageUrl { image_url } => Some(image_url.url.as_str()),
            Part::Text { .. } => None,
        })
        .collect();
    assert_eq!(urls.len(), 1, "one picture was read");
    urls[0].to_string()
}

#[test]
fn a_picture_a_tool_found_is_put_in_front_of_a_model_that_can_see() {
    let mut s = session();
    plant(&s, reads_the_screenshot());
    let chat = asked(&mut s, SEEING);
    let run = model::latest_run(s.store(), chat).unwrap();
    assert_eq!(run.status, model::DONE, "{:?}", run.error);

    let requests = fake(&s).requests();
    let last = requests.last().expect("the round came back for its answer");
    // The tool said what it found, in words, and named where the bytes are.
    let said = last
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the tool turn");
    let said: Value = serde_json::from_str(said.text()).expect("the reply is JSON");
    assert_eq!(said["format"], "image");
    assert_eq!(said["mime"], "image/png");
    assert!(said["look"][0]["blob"].as_str().unwrap().starts_with("agent:"));

    // And the picture itself came after it, as its own turn.
    let at = last
        .messages
        .iter()
        .position(|m| m.role == Role::Tool)
        .unwrap();
    assert!(
        last.messages[at + 1].parts().is_some(),
        "the pictures ride directly after the round that named them"
    );
    let url = only_picture(last);
    assert!(url.starts_with("data:image/png;base64,"), "{}", &url[..40.min(url.len())]);
    assert!(url.len() > "data:image/png;base64,".len(), "the bytes are really there");

    // The fake answered with the call's `then` rather than falling through
    // to its script: a turn carrying pictures is not the person coming back.
    let said = transcript(&s, chat);
    assert!(
        said.iter().any(|(_, text)| text.contains("It is a small icon.")),
        "{said:?}"
    );
    assert_eq!(last.last_user(), Some("what is in this screenshot?"));
}

#[test]
fn a_chat_on_a_model_that_cannot_see_is_told_so_and_sends_no_picture() {
    let mut s = session();
    plant(&s, reads_the_screenshot());
    let chat = asked(&mut s, MODEL);
    assert_eq!(model::latest_run(s.store(), chat).unwrap().status, model::DONE);

    let requests = fake(&s).requests();
    let last = requests.last().expect("the round came back");
    assert!(
        last.messages.iter().all(|m| m.parts().is_none()),
        "a model that cannot look at a picture is never sent one"
    );
    // It is told once, in the prompt, because it is a fact about the chat
    // rather than an event in it.
    let system = last.messages[0].text();
    assert!(system.contains("You cannot look at pictures"), "{system}");
    assert!(system.contains("Sol and Astra"), "{system}");
    assert!(!system.contains("You can look at pictures"));
    // The tool still says what it found, so nothing is silently lost.
    let said = last.messages.iter().find(|m| m.role == Role::Tool).unwrap();
    assert!(said.text().contains("\"format\":\"image\""), "{}", said.text());
}

#[test]
fn a_picture_whose_bytes_are_gone_becomes_a_line_and_not_a_gap() {
    let mut s = session();
    plant(&s, reads_the_screenshot());
    let chat = asked(&mut s, SEEING);
    let key = {
        let requests = fake(&s).requests();
        let said = requests
            .last()
            .unwrap()
            .messages
            .iter()
            .find(|m| m.role == Role::Tool)
            .unwrap()
            .text()
            .to_string();
        let said: Value = serde_json::from_str(&said).unwrap();
        said["look"][0]["blob"].as_str().unwrap().to_string()
    };
    // The cache is bounded and device-local: what it evicts, a later round
    // simply does not have.
    s.world()
        .with_cap::<dyn kernel::caps::Blobs, _>(|b| b.remove(&key))
        .unwrap();

    plant(&s, vec![Reply::always(Answer::Text("It was an icon.".into()))]);
    send_model(&mut s, Some(chat), "and what colour was it?", Carried::default())
        .expect("the second send landed");
    s.workers().tick();
    s.settle();

    let requests = fake(&s).requests();
    let last = requests.last().unwrap();
    let shown = last
        .messages
        .iter()
        .find(|m| m.parts().is_some())
        .expect("the round still has its turn");
    match shown.content.as_ref().expect("content") {
        Content::Parts(parts) => {
            assert_eq!(parts.len(), 1, "a line stands where the picture was");
            assert!(
                shown.text().contains("shown earlier in this chat"),
                "{}",
                shown.text()
            );
            assert!(shown.text().contains("screenshot-2026-08-30.png"), "{}", shown.text());
        }
        Content::Text(text) => panic!("a shown turn is always parts: {text}"),
    }
}

#[test]
fn the_pictures_of_one_round_are_found_together_and_wear_the_names_a_person_uses() {
    use crate::apps::agent::prompt;
    // The turns of one round, as the worker writes them: an assistant turn
    // with its calls, then a tool turn per call, then the answer.
    let turn = |role: Role, said: &str| {
        let mut message = Message::of(role);
        message.content = Some(Content::Text(said.to_string()));
        if role == Role::Tool {
            message.tool_call_id = Some("call_1".into());
        }
        model::Turn::new(message)
    };
    let turns = vec![
        turn(Role::User, "read both of these"),
        turn(Role::Assistant, ""),
        turn(Role::Tool, r#"{"format":"image","name":"receipt.jpg","look":[{"blob":"tg:one"}]}"#),
        turn(Role::Tool, r#"{"format":"image","path":"~/shot.png","look":[{"blob":"agent:two"}]}"#),
        turn(Role::Assistant, "both read"),
        turn(Role::User, "and this one"),
        turn(Role::Tool, r#"{"format":"image","name":"later.png","look":[{"blob":"mail:three"}]}"#),
    ];
    let rounds = prompt::looks(&turns);
    assert_eq!(rounds.len(), 2, "two rounds, and the run of tool turns is one");
    assert_eq!(rounds[0].0, 3, "a round's turn goes after its last tool result");
    assert_eq!(
        rounds[0]
            .1
            .iter()
            .map(|n| (n.label.as_str(), n.blob.as_str()))
            .collect::<Vec<_>>(),
        vec![("receipt.jpg", "tg:one"), ("~/shot.png", "agent:two")]
    );
    assert_eq!(rounds[1].0, 6);

    // A key no app of this build minted is not a picture this build sends.
    let foreign = vec![turn(
        Role::Tool,
        r#"{"format":"image","name":"x","look":[{"blob":"file:///etc/passwd"}]}"#,
    )];
    assert!(prompt::looks(&foreign).is_empty());
    // Nor is a reply that merely has the word in it somewhere.
    let rows = vec![turn(
        Role::Tool,
        r#"{"columns":["look"],"rows":[["a picture of a cat"]]}"#,
    )];
    assert!(prompt::looks(&rows).is_empty());
}
