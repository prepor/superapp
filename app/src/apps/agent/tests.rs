//! The floor, driven with no window and no network: the wire on recorded
//! streams, the assembler on fragments, the scripted gateway, and the app
//! in a session.

use kernel::app::App;
use kernel::session::Session;
use kernel::tool::Tool;
use serde_json::{json, Value};

use super::fake::{Answer, Reply, Script};
use super::fixtures::{self, events};
use super::gateway::{request_parts, request_parts_with, stream_completion, Failure, Flow, Parts};
use super::wire::{
    Assembler, ChatRequest, Chunk, Completion, Finish, Message, Role, ToolDef, Usage,
};
use super::{FakeGateway, Gateway, AGENT, GATEWAY, MODEL, PROVIDER, REASONING_EFFORT};

static APPS: &[&dyn App] = &[&AGENT];

/// What a chunk stream comes to, with nothing watching it.
fn read(raw: &str) -> Result<Completion, Failure> {
    stream_completion(events(raw).into_iter(), &mut |_| Flow::Go)
}

/// The same stream, added up by hand, so a test can look at the tail.
fn assemble(raw: &str) -> Assembler {
    let mut assembler = Assembler::new();
    for event in events(raw) {
        let raw = event.expect("an event");
        if raw == "[DONE]" {
            continue;
        }
        let chunk: Chunk = serde_json::from_str(&raw).expect("a chunk");
        assembler.push(&chunk).expect("no error in this one");
    }
    assembler
}

// -- the wire ------------------------------------------------------------------

#[test]
fn a_request_round_trips_through_json() {
    let mut req = ChatRequest::new(
        MODEL,
        vec![Message::system("you are here"), Message::user("hi")],
    );
    req.reasoning_effort = Some(REASONING_EFFORT.to_string());
    req.tools = vec![ToolDef::from(&look())];
    let text = serde_json::to_string(&req).expect("a request serialises");
    let back: ChatRequest = serde_json::from_str(&text).expect("and reads back");
    assert_eq!(back, req);
}

#[test]
fn an_assistant_turn_with_calls_round_trips_through_json() {
    let done = read(fixtures::TOOL_CALL).expect("the recorded call");
    let text = serde_json::to_string(&done.message).expect("a turn serialises");
    let back: Message = serde_json::from_str(&text).expect("and reads back");
    assert_eq!(
        back, done.message,
        "what a turn's row keeps is the wire's own"
    );
    assert_eq!(back.role, Role::Assistant);
}

#[test]
fn what_a_message_leaves_out_it_leaves_out() {
    let text = serde_json::to_string(&Message::user("hi")).expect("a message serialises");
    assert_eq!(text, r#"{"role":"user","content":"hi"}"#);
    let said = serde_json::to_string(&Message::assistant("hi")).expect("serialises");
    assert_eq!(said, r#"{"role":"assistant","content":"hi"}"#);
    let answer = serde_json::to_string(&Message::tool("call_1", "done")).expect("serialises");
    assert_eq!(
        answer,
        r#"{"role":"tool","content":"done","tool_call_id":"call_1"}"#
    );
}

#[test]
fn a_chunk_ignores_the_fields_this_build_has_no_use_for() {
    let raw = r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"m","system_fingerprint":"fp","choices":[{"index":0,"delta":{"content":"hi"},"logprobs":null,"finish_reason":null}]}"#;
    let chunk: Chunk = serde_json::from_str(raw).expect("a chunk with more on it than we read");
    assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
}

#[test]
fn a_finish_reason_this_build_has_no_word_for_is_kept_as_it_came() {
    assert_eq!(Finish::parse("stop"), Finish::Stop);
    assert_eq!(Finish::parse("tool_calls").word(), "tool_calls");
    assert_eq!(Finish::parse("length"), Finish::Length);
    assert_eq!(Finish::parse("content_filter"), Finish::ContentFilter);
    assert_eq!(Finish::parse("eos").word(), "eos");
}

#[test]
fn a_tool_of_this_build_becomes_a_function_definition() {
    let def = ToolDef::from(&look());
    assert_eq!(def.r#type, "function");
    assert_eq!(def.function.name, "test.look");
    assert_eq!(def.function.parameters, look().input);
    let text = serde_json::to_string(&def).expect("a definition serialises");
    assert!(
        text.starts_with(r#"{"type":"function","function":{"name":"test.look""#),
        "{text}"
    );
}

// -- the assembler -------------------------------------------------------------

#[test]
fn a_text_turn_is_added_up_as_it_arrives() {
    let mut assembler = Assembler::new();
    let mut tails = Vec::new();
    for event in events(fixtures::TEXT) {
        let raw = event.expect("an event");
        if raw == "[DONE]" {
            continue;
        }
        let chunk: Chunk = serde_json::from_str(&raw).expect("a chunk");
        assembler.push(&chunk).expect("no error in this one");
        tails.push(assembler.text().to_string());
    }
    assert_eq!(tails[1], "Your ", "the live tail is what a chat draws");
    assert_eq!(tails[2], "Your inbox ");
    let done = assembler.finish().expect("the whole answer");
    assert_eq!(done.message.text(), "Your inbox has three unread letters.");
    assert_eq!(done.finish, Finish::Stop);
    assert_eq!(
        done.usage,
        Some(Usage {
            prompt_tokens: 2100,
            completion_tokens: 310,
            total_tokens: 2410,
            prompt_tokens_details: Some(super::wire::PromptTokensDetails {
                cached_tokens: 1900
            }),
        })
    );
    assert_eq!(done.usage.expect("the usage").cached(), 1900);
}

#[test]
fn arguments_that_arrive_in_fragments_are_parsed_once_whole() {
    let done = read(fixtures::TOOL_CALL).expect("the recorded call");
    assert_eq!(done.finish, Finish::ToolCalls);
    let call = &done.message.tool_calls[0];
    assert_eq!(call.id, "call_9d2");
    assert_eq!(call.r#type, "function");
    assert_eq!(call.function.name, "files.rename");
    assert_eq!(
        call.input().expect("the arguments read as JSON"),
        json!({"path": "~/notes.md", "to": "~/notes-2026.md"})
    );
    assert_eq!(
        call.function.arguments, r#"{"path":"~/notes.md","to":"~/notes-2026.md"}"#,
        "re-serialised compactly, not the fragments' seams"
    );
    assert_eq!(done.message.content, None, "a call turn said nothing");
}

#[test]
fn two_calls_in_one_turn_are_kept_apart_by_index() {
    let mut assembler = Assembler::new();
    let raws = [
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"files.list","arguments":"{\"dir\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"mail.search","arguments":"{\"q\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"tax\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"~\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    for raw in raws {
        let chunk: Chunk = serde_json::from_str(raw).expect("a chunk");
        assembler.push(&chunk).expect("no error");
    }
    let done = assembler.finish().expect("both calls");
    let names: Vec<&str> = done
        .message
        .tool_calls
        .iter()
        .map(|c| c.function.name.as_str())
        .collect();
    assert_eq!(names, vec!["files.list", "mail.search"], "in index order");
    assert_eq!(
        done.message.tool_calls[1].input().expect("read"),
        json!({"q": "tax"})
    );
}

#[test]
fn a_call_whose_arguments_are_not_json_fails_the_completion() {
    let mut assembler = Assembler::new();
    let raw = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"files.list","arguments":"{oh no"}}]},"finish_reason":"tool_calls"}]}"#;
    let chunk: Chunk = serde_json::from_str(raw).expect("a chunk");
    assembler.push(&chunk).expect("the chunk itself is fine");
    let why = assembler.finish().expect_err("the arguments are not");
    assert!(why.contains("files.list"), "{why}");
    assert!(
        why.contains("{oh no"),
        "and it quotes what was written: {why}"
    );
}

#[test]
fn a_call_with_no_arguments_at_all_is_an_empty_object() {
    let mut assembler = Assembler::new();
    let raw = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"problems.list","arguments":""}}]},"finish_reason":"tool_calls"}]}"#;
    let chunk: Chunk = serde_json::from_str(raw).expect("a chunk");
    assembler.push(&chunk).expect("fine");
    let done = assembler.finish().expect("a call with nothing to say");
    assert_eq!(done.message.tool_calls[0].input().expect("read"), json!({}));
}

#[test]
fn a_stream_that_never_said_why_it_stopped_says_none() {
    let done = Assembler::new().finish().expect("nothing at all");
    assert_eq!(done.finish, Finish::Other("none".into()));
    assert_eq!(done.message.content, None);
}

// -- reading a stream ----------------------------------------------------------

#[test]
fn a_turn_that_ran_out_of_room_says_so() {
    let done = read(fixtures::LENGTH).expect("a cut turn is still a turn");
    assert_eq!(done.finish, Finish::Length);
    assert_eq!(done.message.text(), "The first thing to say is");
}

#[test]
fn a_filter_ends_a_turn_with_nothing_said() {
    let done = read(fixtures::CONTENT_FILTER).expect("a filtered turn is still a turn");
    assert_eq!(done.finish, Finish::ContentFilter);
    assert_eq!(done.message.content, None);
}

#[test]
fn the_model_thinking_out_loud_is_kept_apart_from_what_it_said() {
    let assembler = assemble(fixtures::REASONING);
    assert_eq!(
        assembler.reasoning(),
        "The person is asking about their inbox.",
        "the folded line, added up as a tail of its own"
    );
    assert_eq!(assembler.text(), "Three unread.");
    let done = assembler.finish().expect("the whole answer");
    assert_eq!(done.message.text(), "Three unread.");
    assert_eq!(
        done.message.reasoning_content.as_deref(),
        Some("The person is asking about their inbox.")
    );
}

#[test]
fn an_error_mid_stream_is_the_failure_in_the_gateways_own_words() {
    let why = read(fixtures::MID_STREAM_ERROR).expect_err("the stream said it failed");
    assert_eq!(why.message, "Rate limit exceeded");
    assert_eq!(why.status, None);
    assert_eq!(why.to_string(), "Rate limit exceeded");
    assert_eq!(
        Failure {
            status: Some(401),
            message: "no such token".into()
        }
        .to_string(),
        "401: no such token",
        "a status leads when there is one"
    );
}

#[test]
fn an_event_that_is_not_a_chunk_is_quoted_back() {
    let why = stream_completion(
        [Ok("<html>bad account</html>".to_string())].into_iter(),
        &mut |_| Flow::Go,
    )
    .expect_err("that is no chunk");
    assert!(why.message.contains("<html>bad account</html>"), "{why}");
}

#[test]
fn stop_cuts_the_stream_at_its_next_chunk() {
    let mut seen = 0;
    let why = stream_completion(events(fixtures::TEXT).into_iter(), &mut |_| {
        seen += 1;
        if seen >= 2 {
            Flow::Stop
        } else {
            Flow::Go
        }
    })
    .expect_err("stopped");
    assert_eq!(why.message, "stopped");
    assert_eq!(seen, 2, "and nothing after it was read");
}

// -- what goes out -------------------------------------------------------------

fn parts(log_payload: bool) -> Parts {
    let mut req = ChatRequest::new(MODEL, vec![Message::user("what is in my inbox?")]);
    req.reasoning_effort = Some(PROVIDER.reasoning_effort.to_string());
    req.tools = vec![ToolDef::from(&look())];
    request_parts_with(&PROVIDER, "acc0unt", GATEWAY, "t0ken", &req, log_payload)
}

#[test]
fn a_request_goes_to_the_gateway_in_front_of_the_provider() {
    assert_eq!(
        parts(false).url,
        "https://gateway.ai.cloudflare.com/v1/acc0unt/superapp/workers-ai/v1/chat/completions"
    );
}

#[test]
fn one_token_opens_the_provider_and_the_gateway_at_once() {
    assert_eq!(
        parts(false).headers,
        vec![
            ("authorization", "Bearer t0ken".to_string()),
            ("cf-aig-authorization", "Bearer t0ken".to_string()),
            ("content-type", "application/json".to_string()),
            ("accept", "text/event-stream".to_string()),
            ("cf-aig-collect-log-payload", "false".to_string()),
        ]
    );
}

/// The knob is the one thing about a request the environment gets a say
/// in, so what it says is what the two spellings must agree on.
#[test]
fn the_policy_is_the_apps_and_the_environment_only_turns_it_off() {
    let mut req = ChatRequest::new(MODEL, vec![Message::user("what is in my inbox?")]);
    req.reasoning_effort = Some(PROVIDER.reasoning_effort.to_string());
    let knob = std::env::var("SUPERAPP_AGENT_LOG_PAYLOAD").is_ok_and(|v| v.trim() == "1");
    assert_eq!(
        request_parts(&PROVIDER, "acc0unt", GATEWAY, "t0ken", &req),
        request_parts_with(&PROVIDER, "acc0unt", GATEWAY, "t0ken", &req, knob)
    );
}

#[test]
fn the_gateway_is_told_to_keep_no_prose_unless_this_run_asked_it_to() {
    let asked = parts(true);
    assert!(
        !asked
            .headers
            .iter()
            .any(|(k, _)| *k == "cf-aig-collect-log-payload"),
        "with the knob on the app says nothing and the dashboard decides"
    );
}

#[test]
fn the_body_carries_the_model_the_stream_and_the_tools() {
    let body: Value = serde_json::from_slice(&parts(false).body).expect("the body is JSON");
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "test.look");
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["additionalProperties"],
        false
    );
    assert_eq!(body["messages"][0]["role"], "user");
}

// -- the scripted gateway ------------------------------------------------------

/// One request with a system prompt and a question, as a run builds it.
fn ask(text: &str) -> ChatRequest {
    ChatRequest::new(
        MODEL,
        vec![
            Message::system("you are the assistant"),
            Message::user(text),
        ],
    )
}

/// The whole answer, with nothing watching the stream.
fn say(fake: &mut FakeGateway, req: &ChatRequest) -> Result<Completion, Failure> {
    fake.complete(req, &mut |_| Flow::Go)
}

#[test]
fn a_build_with_nobodys_script_still_answers() {
    let mut fake = FakeGateway::default_script();
    let done = say(&mut fake, &ask("hello")).expect("the default script");
    assert_eq!(done.message.text(), "Hello. I am the assistant.");
    assert_eq!(done.finish, Finish::Stop);
}

#[test]
fn the_fake_streams_its_answer_a_word_at_a_time() {
    let mut fake = FakeGateway::default_script();
    let mut tails = Vec::new();
    fake.complete(&ask("hello"), &mut |chunk| {
        if let Some(text) = chunk.choices.first().and_then(|c| c.delta.content.clone()) {
            tails.push(text);
        }
        Flow::Go
    })
    .expect("the answer");
    assert_eq!(
        tails,
        vec!["", "Hello. ", "I ", "am ", "the ", "assistant."],
        "a live tail has something to draw between the words"
    );
}

#[test]
fn a_reply_is_chosen_by_a_word_in_what_was_asked() {
    let script: Script = vec![
        Reply::when("inbox", Answer::Text("three unread".into())),
        Reply::always(Answer::Text("I do not know".into())),
    ];
    let mut fake = FakeGateway::new(script);
    assert_eq!(
        say(&mut fake, &ask("what is in my INBOX?"))
            .expect("matched without case")
            .message
            .text(),
        "three unread"
    );
    assert_eq!(
        say(&mut fake, &ask("and the weather?"))
            .expect("the unused one with no word on it")
            .message
            .text(),
        "I do not know"
    );
    assert_eq!(
        say(&mut fake, &ask("anything else?"))
            .expect("a script that has run out still answers")
            .message
            .text(),
        "…"
    );
}

#[test]
fn a_call_is_followed_by_what_it_was_scripted_to_say_after() {
    let mut fake = FakeGateway::new(vec![Reply::when(
        "rename",
        Answer::Call {
            name: "files.rename".into(),
            arguments: json!({"path": "~/notes.md", "to": "~/notes-2026.md"}),
            then: "renamed it.".into(),
        },
    )]);
    let asked = say(&mut fake, &ask("please rename that file")).expect("the call");
    assert_eq!(asked.finish, Finish::ToolCalls);
    let call = &asked.message.tool_calls[0];
    assert_eq!(call.function.name, "files.rename");
    assert_eq!(call.id, "call_1");
    assert_eq!(
        call.input().expect("the arguments"),
        json!({"path": "~/notes.md", "to": "~/notes-2026.md"})
    );

    // The run comes back with what the call came to.
    let mut back = ask("please rename that file");
    back.messages.push(asked.message.clone());
    back.messages.push(Message::tool(&call.id, "ok"));
    let after = say(&mut fake, &back).expect("what follows");
    assert_eq!(after.message.text(), "renamed it.");
    assert_eq!(after.finish, Finish::Stop);
}

#[test]
fn a_text_answer_can_name_the_panel_and_the_person() {
    let mut fake = FakeGateway::new(vec![Reply::always(Answer::Text(
        "you asked about {panel}: {user}".into(),
    ))]);
    let mut req = ask("how many unread?");
    req.messages[0] = Message::system(
        "you are the assistant\n<panel id=\"inbox\" title=\"inbox\" workspace=\"1\">\nrows\n</panel>",
    );
    assert_eq!(
        say(&mut fake, &req).expect("filled in").message.text(),
        "you asked about inbox: how many unread?"
    );

    // And says so when there is no chip in the prompt at all.
    let mut bare = FakeGateway::new(vec![Reply::always(Answer::Text("about {panel}".into()))]);
    assert_eq!(
        say(&mut bare, &ask("hi"))
            .expect("still answers")
            .message
            .text(),
        "about no panel"
    );
}

#[test]
fn a_scripted_failure_is_a_failure_with_no_status() {
    let mut fake = FakeGateway::new(vec![Reply::always(Answer::Fail(
        "the gateway is not answering".into(),
    ))]);
    let why = say(&mut fake, &ask("hello")).expect_err("scripted to fail");
    assert_eq!(why.status, None);
    assert_eq!(why.to_string(), "the gateway is not answering");
}

#[test]
fn a_scripted_turn_can_be_cut_short_or_filtered() {
    let mut fake = FakeGateway::new(vec![
        Reply::when("long", Answer::Cut("as I was saying".into())),
        Reply::when("rude", Answer::Filtered),
    ]);
    let cut = say(&mut fake, &ask("say something long")).expect("a cut turn");
    assert_eq!(cut.finish, Finish::Length);
    assert_eq!(cut.message.text(), "as I was saying");
    let filtered = say(&mut fake, &ask("say something rude")).expect("a filtered turn");
    assert_eq!(filtered.finish, Finish::ContentFilter);
    assert_eq!(filtered.message.content, None);
}

#[test]
fn the_fake_counts_tokens_by_a_rule_a_suite_can_assert_on() {
    let mut fake = FakeGateway::new(vec![Reply::always(Answer::Text("12345678".into()))]);
    let req = ask("hello");
    let done = say(&mut fake, &req).expect("the answer");
    let sent = serde_json::to_string(&req).expect("the request").len() as u64;
    let usage = done.usage.expect("the usage came on the last chunk");
    assert_eq!(usage.prompt_tokens, 100 + sent / 4);
    assert_eq!(
        usage.completion_tokens, 2,
        "eight characters, four to a token"
    );
    assert_eq!(usage.total_tokens, usage.prompt_tokens + 2);
    assert_eq!(usage.cached(), 0);
}

#[test]
fn a_stop_cuts_the_fakes_stream_too() {
    let mut fake = FakeGateway::default_script();
    let mut seen = 0;
    let why = fake
        .complete(&ask("hello"), &mut |_| {
            seen += 1;
            if seen >= 2 {
                Flow::Stop
            } else {
                Flow::Go
            }
        })
        .expect_err("stopped");
    assert_eq!(why.message, "stopped");
    assert_eq!(seen, 2);
}

#[test]
fn the_fake_records_what_the_model_was_told() {
    let mut fake = FakeGateway::default_script();
    assert!(fake.requests().is_empty());
    say(&mut fake, &ask("first")).expect("one");
    say(&mut fake, &ask("second")).expect("two");
    let asked = fake.requests();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].last_user(), Some("first"));
    assert_eq!(asked[1].last_user(), Some("second"));
    assert_eq!(asked[1].model, MODEL);

    // Planting a script starts the fake over.
    fake.plant(vec![Reply::always(Answer::Text("new".into()))]);
    assert!(fake.requests().is_empty());
    assert_eq!(
        say(&mut fake, &ask("again"))
            .expect("the new script")
            .message
            .text(),
        "new"
    );
}

// -- the app in a session ------------------------------------------------------

#[test]
fn the_agent_app_is_in_this_build_and_offers_no_tools_yet() {
    let s = Session::fake(APPS);
    assert!(s.apps().get("agent").is_some());
    assert!(s.apps().tags().is_empty(), "phase 1 registers the panels");
    assert!(
        s.apps().tools().is_empty(),
        "phase 3 fills this; the registry is here for it"
    );
    assert!(s.apps().tool("agent.nothing").is_none());
}

#[test]
fn a_scripted_world_reaches_the_fake_gateway_under_both_its_names() {
    let s = Session::fake(APPS);
    assert!(
        s.world().caps(|c| c.get::<FakeGateway>().is_some()),
        "a test plants its script through the concrete type"
    );
    assert!(
        s.world().caps(|c| c.get::<dyn Gateway>().is_some()),
        "and a run asks for the trait"
    );

    // The one a test reaches and the one a run reaches are one gateway.
    let fake = s
        .world()
        .caps(|c| c.get::<FakeGateway>().map(|f| f.clone()))
        .expect("the fake");
    fake.plant(vec![Reply::always(Answer::Text("planted".into()))]);
    let done = s
        .world()
        .caps(|c| {
            c.get::<dyn Gateway>()
                .expect("the trait")
                .complete(&ask("hello"), &mut |_| Flow::Go)
        })
        .expect("the answer");
    assert_eq!(done.message.text(), "planted");
    assert_eq!(fake.requests().len(), 1, "and the test sees what was asked");
}

// -- the tool contract, from an app's side -------------------------------------

/// A tool as an app will declare one in phase 3.
fn look() -> Tool {
    Tool::new(
        "test.look",
        "reads a directory",
        json!({
            "type": "object",
            "properties": {
                "dir": {"type": "string"},
                "depth": {"type": "integer"}
            },
            "required": ["dir"],
            "additionalProperties": false
        }),
        false,
        |_s, _input| Ok(json!({"entries": []})),
    )
}

#[test]
fn a_tools_schema_is_what_the_arguments_are_held_to() {
    let t = look();
    assert_eq!(t.check(&json!({"dir": "~"})), Ok(()));
    assert_eq!(t.check(&json!({"dir": "~", "depth": 2})), Ok(()));
    assert_eq!(t.check(&json!({})), Err("missing `dir`".to_string()));
    assert_eq!(
        t.check(&json!({"dir": 7})),
        Err("`dir` must be a string".to_string())
    );
    assert_eq!(
        t.check(&json!({"dir": "~", "recursive": true})),
        Err("unknown key `recursive`".to_string())
    );
}

#[test]
fn what_a_model_wrote_is_checked_before_a_tool_runs_it() {
    let done = read(fixtures::TOOL_CALL).expect("the recorded call");
    let input = done.message.tool_calls[0].input().expect("the arguments");
    // The recorded call is for a tool this test does not have; what the
    // chat does is find it by name and hold its arguments to its schema.
    let t = look();
    assert_eq!(
        t.check(&input),
        Err("missing `dir`".to_string()),
        "a model's JSON is a claim, not a promise"
    );
}
