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
    Assembler, ChatRequest, Chunk, Completion, Finish, FunctionCall, Message, Role, ToolCall,
    ToolDef, Usage,
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
    assert_eq!(
        s.apps().tags(),
        vec![super::Agents::TAG, super::Chat::TAG],
        "two kinds, and nobody else's in a build of this app alone"
    );
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

// -- the chat, driven through a session ----------------------------------------
//
// `Session::fake` gives an in-memory store with the agent's schema, the
// scripted gateway, and the passes running inline — so a send is followed by
// the whole of the fake's answer in the same call, and a kick is a tick.

use std::any::Any as StdAny;

use kernel::app::{Apps, ProblemSource, Root};
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{PanelId, PanelKind, VerbAct};
use kernel::session::Action;

use super::model::{self, Chat as ChatRow, ChatId, Cost, Turn};
use super::panels::{Agents, Chat};
use super::problems::GatewayProblems;
use super::{calls, prompt, real, schema, worker, Agent};
use crate::apps::files::FILES;
use crate::apps::mail::MAIL;

/// The build a chat is driven in: the two apps whose tools an agent will one
/// day reach, and the agent itself.
static BUILD: &[&dyn App] = &[&MAIL, &FILES, &AGENT];

fn session() -> Session {
    Session::fake(BUILD)
}

/// This world's gateway — what a test plants a script through.
fn fake(s: &Session) -> FakeGateway {
    s.world()
        .caps(|c| c.get::<FakeGateway>().map(|g| g.clone()))
        .expect("the fake gateway is installed under its own type")
}

fn plant(s: &Session, script: Script) {
    fake(s).plant(script);
}

/// Opens a root panel, as the launcher would.
fn open_root(s: &mut Session, id: PanelId) -> SlotId {
    let show = id.clone();
    s.act(
        Action::new("open", format!("open “{id}”")).moving(move |wm| {
            wm.open(show, None, false);
        }),
    );
    s.settle();
    s.focus().expect("the new slot has focus")
}

/// Reaches a chat panel.
fn with_chat<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Chat) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<Chat>().expect("a chat"))
}

/// Reaches an agents list.
fn with_agents<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Agents) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<Agents>().expect("an agents list"))
}

/// Runs one of a panel's verbs by id, exactly as the bar does.
fn verb(s: &mut Session, slot: SlotId, id: &str) {
    let inst = s.panel(slot).expect("a panel in the slot");
    let act = {
        let b = inst.borrow();
        b.verbs().into_iter().find(|v| v.id == id).map(|v| v.act)
    };
    match act {
        Some(VerbAct::Run) => inst.borrow_mut().run(id, s),
        Some(VerbAct::Call(f)) => f(s),
        Some(VerbAct::Go(n)) => s.nav(n),
        None => panic!("no verb {id} on slot {slot}"),
    }
    s.settle();
}

/// The ids on one panel's bar, in the order it wears them.
fn verb_ids(s: &Session, slot: SlotId) -> Vec<&'static str> {
    s.panel(slot)
        .expect("a panel in the slot")
        .borrow()
        .verbs()
        .iter()
        .map(|v| v.id)
        .collect()
}

/// The chat's messages, as who spoke and what they said.
fn transcript(s: &Session, chat: ChatId) -> Vec<(Role, String)> {
    model::turns(s.store(), chat)
        .iter()
        .map(|t| (t.message.role, t.text().to_string()))
        .collect()
}

/// Sends in a fresh chat and answers which chat it made.
fn send_new(s: &mut Session, text: &str) -> ChatId {
    let (chat, _) = model::send(s, None, text).expect("the send landed");
    s.settle();
    chat
}

#[test]
fn a_send_makes_the_chat_the_turn_and_the_answer() {
    let mut s = session();
    let chat = send_new(&mut s, "hello");

    let row = model::chat(s.store(), chat).expect("the chat row");
    assert_eq!(row.title, "hello", "the first line of the first thing said");
    assert_eq!(row.model, MODEL, "which model answered, on the row");

    assert_eq!(
        transcript(&s, chat),
        vec![
            (Role::User, "hello".to_string()),
            (Role::Assistant, "Hello. I am the assistant.".to_string()),
        ],
        "the inline workers ran the whole round in the same call"
    );

    let run = model::latest_run(s.store(), chat).expect("the run");
    assert_eq!(run.status, model::DONE);
    assert_eq!(run.error, None);
    let usage = run.usage.expect("the usage came on the last chunk");
    assert!(usage.input > 0 && usage.output > 0, "{usage:?}");
    assert_eq!(
        AGENT.tail(run.id),
        None,
        "the live tail is cleared once the turn is a row"
    );

    let answer = model::turns(s.store(), chat)[1].clone();
    assert_eq!(answer.finish.as_deref(), Some("stop"));
    assert_eq!(answer.run, Some(run.id));
}

#[test]
fn a_title_is_the_first_line_clipped() {
    assert_eq!(model::title_of("hello"), "hello");
    assert_eq!(model::title_of("hello\nand the rest"), "hello");
    assert_eq!(model::title_of("   "), model::UNTITLED);
    let long = "x".repeat(200);
    assert_eq!(model::title_of(&long).chars().count(), 60);
}

#[test]
fn a_turn_round_trips_through_the_body_column() {
    let mut turn = Turn::new(Message::assistant("said")).finishing("length");
    turn.id = 7;
    turn.chat = 3;
    turn.seq = 2;
    turn.run = Some(9);
    let body = turn.body();
    assert!(
        body.starts_with(r#"{"role":"assistant","content":"said""#),
        "the wire's own message, flattened in: {body}"
    );
    assert!(!body.contains("\"seq\""), "the row's own columns stay out");
    let back: Turn = serde_json::from_str(&body).expect("and reads back");
    assert_eq!(back.message, turn.message);
    assert_eq!(back.finish.as_deref(), Some("length"));
    assert_eq!(back.seq, 0, "which is why the read fills them in");
}

#[test]
fn a_call_the_build_has_no_tool_for_fails_in_words_and_the_run_goes_on() {
    let mut s = session();
    plant(
        &s,
        vec![Reply::when(
            "rename",
            Answer::Call {
                name: "files.rename".into(),
                arguments: json!({"path": "~/notes.md", "to": "~/notes-2026.md"}),
                then: "I could not rename it.".into(),
            },
        )],
    );
    let chat = send_new(&mut s, "please rename that file");

    let run = model::latest_run(s.store(), chat).expect("the run");
    assert_eq!(
        run.status,
        model::WAITING,
        "the calls are the chat's to run"
    );
    let pending = model::pending_calls(s.store(), run.id);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool, "files.rename");
    assert_eq!(
        pending[0].input(),
        json!({"path": "~/notes.md", "to": "~/notes-2026.md"})
    );

    assert_eq!(calls::run_pending_calls(&mut s, chat), 1);
    s.settle();
    let call = model::calls(s.store(), run.id)[0].clone();
    assert_eq!(call.status, model::CALL_FAILED);
    assert!(
        call.said().contains("no such tool in this build"),
        "{}",
        call.said()
    );

    // The worker was kicked, wrote the `tool` turn, and asked again.
    assert_eq!(
        transcript(&s, chat)
            .iter()
            .map(|(r, _)| *r)
            .collect::<Vec<_>>(),
        vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]
    );
    assert_eq!(
        model::turns(s.store(), chat)[3].text(),
        "I could not rename it."
    );
    assert_eq!(
        model::latest_run(s.store(), chat).expect("the run").status,
        model::DONE
    );

    // …and what the model was told the second time carries the error.
    let asked = fake(&s).requests();
    assert_eq!(asked.len(), 2);
    let last = asked[1].messages.last().expect("the tool result").clone();
    assert_eq!(last.role, Role::Tool);
    assert_eq!(last.tool_call_id.as_deref(), Some("call_1"));
    assert!(
        last.text().contains("no such tool in this build"),
        "{last:?}"
    );
}

#[test]
fn a_failed_run_says_why_stands_as_a_problem_and_retries() {
    let mut s = session();
    plant(
        &s,
        vec![Reply::always(Answer::Fail(
            "gateway: unauthorized — the token is not this account's".into(),
        ))],
    );
    let chat = send_new(&mut s, "hello");
    let run = model::latest_run(s.store(), chat).expect("the run");
    assert_eq!(run.status, model::FAILED);
    assert!(
        run.error
            .as_deref()
            .is_some_and(|e| e.starts_with("gateway:")),
        "{:?}",
        run.error
    );

    let problems = GatewayProblems.list(s.store());
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].key, "gateway");
    assert_eq!(problems[0].label, "gateway");
    assert_eq!(
        problems[0].line, "unauthorized — the token is not this account's",
        "the line has the word the row is keyed on taken off it"
    );
    assert!(problems[0].detail.contains("hello"));

    // …and a round that answers clears it.
    plant(&s, vec![Reply::always(Answer::Text("here I am".into()))]);
    let again = model::retry(&mut s, chat).expect("a fresh round");
    s.settle();
    assert_ne!(again, run.id, "the failed round stays where it is");
    assert_eq!(
        model::latest_run(s.store(), chat).expect("the run").status,
        model::DONE
    );
    assert_eq!(
        model::turns(s.store(), chat).last().expect("it").text(),
        "here I am"
    );
    assert!(GatewayProblems.list(s.store()).is_empty());
}

#[test]
fn a_run_that_answered_is_nothing_to_retry() {
    let mut s = session();
    let chat = send_new(&mut s, "hello");
    assert_eq!(model::retry(&mut s, chat), None);
}

#[test]
fn a_turn_the_model_cut_short_or_a_filter_stopped_says_so() {
    let mut s = session();
    plant(
        &s,
        vec![
            Reply::when("long", Answer::Cut("as I was saying".into())),
            Reply::when("rude", Answer::Filtered),
        ],
    );
    let cut = send_new(&mut s, "say something long");
    let turns = model::turns(s.store(), cut);
    assert_eq!(turns[1].finish.as_deref(), Some("length"));
    assert_eq!(turns[1].text(), "as I was saying");

    let filtered = send_new(&mut s, "say something rude");
    let turns = model::turns(s.store(), filtered);
    assert_eq!(turns[1].finish.as_deref(), Some("content_filter"));
    assert_eq!(turns[1].message.content, None);
}

#[test]
fn undoing_a_send_is_the_chat_as_it_was_and_redo_asks_again() {
    let mut s = session();
    // Two entries, and each answers once: the second is what proves that a
    // redo really asked the model again rather than putting a row back.
    plant(
        &s,
        vec![
            Reply::always(Answer::Text("the first time".into())),
            Reply::always(Answer::Text("the second time".into())),
        ],
    );
    let chat = send_new(&mut s, "hello");
    assert!(model::latest_run(s.store(), chat).is_some());
    assert_eq!(model::turns(s.store(), chat).len(), 2);

    assert!(s.undo());
    s.settle();
    assert!(
        model::turns(s.store(), chat).is_empty(),
        "the person's turn and the answer to it both go"
    );
    assert!(model::latest_run(s.store(), chat).is_none());
    assert!(
        model::chat(s.store(), chat).is_some(),
        "the conversation itself stays: a send is not the chat"
    );

    assert!(s.redo());
    s.settle();
    assert_eq!(
        transcript(&s, chat),
        vec![
            (Role::User, "hello".to_string()),
            (Role::Assistant, "the second time".to_string()),
        ],
        "the turn is filed again and a fresh round answers it"
    );
    let again = model::latest_run(s.store(), chat).expect("the run");
    assert_eq!(again.status, model::DONE);
    assert_eq!(
        fake(&s).requests().len(),
        2,
        "the model was asked twice: the round came back, it was not restored"
    );
}

#[test]
fn deleting_chats_takes_their_rows_and_undo_puts_them_back() {
    let mut s = session();
    let one = send_new(&mut s, "the first");
    let two = send_new(&mut s, "the second");
    let list = open_root(&mut s, Agents::id());
    assert_eq!(with_agents(&s, list, |a| a.len()), 2);

    with_agents(&s, list, |a| {
        a.go(0);
        a.toggle_mark();
    });
    assert_eq!(verb_ids(&s, list), vec!["agent.delete"]);
    verb(&mut s, list, "agent.delete");

    // The newest is on top, so the mark took the second one.
    assert!(model::chat(s.store(), two).is_none());
    assert!(model::chat(s.store(), one).is_some());
    assert!(model::turns(s.store(), two).is_empty());

    assert!(s.undo());
    s.settle();
    assert!(model::chat(s.store(), two).is_some(), "the rows come back");
    assert_eq!(model::turns(s.store(), two).len(), 2);
    assert_eq!(
        model::latest_run(s.store(), two).expect("the run").status,
        model::DONE
    );
}

#[test]
fn the_sweep_fails_a_run_a_crash_left_streaming() {
    let mut s = session();
    let chat = send_new(&mut s, "hello");
    let run = model::latest_run(s.store(), chat).expect("the run").id;
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE agent_run SET status = 'streaming', ended = NULL WHERE id = ?1",
                [run],
            )
            .map(|_| ())
        })
        .expect("the row, by hand");

    s.store()
        .write(|c| schema::SCHEMA.apply(c))
        .expect("the next open");
    let after = model::run(s.store(), run).expect("the run");
    assert_eq!(after.status, model::FAILED);
    assert_eq!(after.error.as_deref(), Some("interrupted"));
    assert!(after.ended.is_some(), "and it has an end, however guessed");
}

#[test]
fn the_sweep_leaves_the_two_statuses_that_can_still_be_resumed() {
    let mut s = session();
    let chat = send_new(&mut s, "hello");
    let run = model::latest_run(s.store(), chat).expect("the run").id;
    for status in [model::PENDING, model::WAITING] {
        s.store()
            .write(move |c| {
                c.execute(
                    "UPDATE agent_run SET status = ?2 WHERE id = ?1",
                    rusqlite::params![run, status],
                )
                .map(|_| ())
            })
            .expect("the row, by hand");
        s.store()
            .write(|c| schema::SCHEMA.apply(c))
            .expect("an open");
        assert_eq!(
            model::run(s.store(), run).expect("the run").status,
            status,
            "a resumable round is left where it is"
        );
    }
}

#[test]
fn a_worker_a_run_and_none_at_all_on_a_store_that_may_not_be_written() {
    let mut s = session();
    plant(
        &s,
        vec![Reply::always(Answer::Call {
            name: "files.rename".into(),
            arguments: json!({}),
            then: "done".into(),
        })],
    );
    let chat = send_new(&mut s, "rename it");
    let run = model::latest_run(s.store(), chat).expect("the run");
    assert_eq!(run.status, model::WAITING);

    let names: Vec<String> = worker::workers(s.store())
        .iter()
        .map(|w| w.name())
        .collect();
    assert_eq!(names, vec![format!("agent-run-{}", run.id)]);
    assert_eq!(
        worker::workers(s.store())[0].entity().as_deref(),
        Some(model::run_entity(run.id).as_str())
    );

    // A device that may not write runs no agent: a replicated run row must
    // not be paid for twice.
    s.store().set_writable(false);
    assert!(worker::workers(s.store()).is_empty());
    s.store().set_writable(true);
}

#[test]
fn the_chat_panel_names_its_conversation_and_wears_its_bar() {
    let mut s = session();
    let blank = open_root(&mut s, Chat::new_id());
    assert_eq!(with_chat(&s, blank, |c| c.chat()), None);
    assert_eq!(
        s.panel(blank).expect("the panel").borrow().title(),
        model::UNTITLED
    );
    assert_eq!(
        verb_ids(&s, blank),
        vec!["agent.new", "agent.agents"],
        "nothing to send and nothing going"
    );

    with_chat(&s, blank, |c| c.set_draft("hello"));
    assert_eq!(
        verb_ids(&s, blank),
        vec!["agent.send", "agent.new", "agent.agents"]
    );

    verb(&mut s, blank, "agent.send");
    // The slot now names the real conversation, not the blank one.
    let id = s.panel(blank).expect("the panel").borrow().id().clone();
    let chat = Chat::of(&id).expect("a chat id in the slot");
    assert_eq!(id, Chat::id(chat));
    assert_eq!(s.panel(blank).expect("the panel").borrow().title(), "hello");
    assert_eq!(with_chat(&s, blank, |c| c.draft().to_string()), "");
    assert_eq!(
        with_chat(&s, blank, |c| c.status()).as_deref(),
        Some(model::DONE)
    );
    assert_eq!(with_chat(&s, blank, |c| c.turns().len()), 2);
    assert!(s
        .panel(blank)
        .expect("the panel")
        .borrow()
        .about()
        .contains("2 turns"));
}

#[test]
fn a_chats_identity_reads_back_and_a_blank_one_carries_none() {
    assert_eq!(Chat::of(&Chat::id(42)), Some(42));
    assert_eq!(Chat::id(42).to_string(), "chat(42)");
    assert_eq!(Chat::new_id().to_string(), "chat(new)");
    assert_eq!(Chat::of(&Chat::new_id()), None);
    assert_eq!(Chat::of(&Agents::id()), None);
    assert_eq!(Agents::id().to_string(), "agents");
}

#[test]
fn stop_ends_the_run_and_the_bar_offers_retry() {
    let mut s = session();
    let chat = send_new(&mut s, "hello");
    // A round that is going, put where a stop would find it. A scripted
    // fake answers whole, so this is the only honest way to stand one up.
    let run = s
        .act(Action::writing("test", "a round in flight", move |tx| {
            let run = model::new_run_tx(tx, chat, 0.0)?;
            model::set_run_status_tx(tx, run, model::STREAMING, None, 0.0)?;
            Ok(run)
        }))
        .expect("the row");
    s.settle();
    assert_eq!(
        model::run(s.store(), run).expect("the run").status,
        model::STREAMING
    );

    model::stop(&mut s, run);
    s.settle();
    assert_eq!(
        model::run(s.store(), run).expect("the run").status,
        model::STOPPED
    );

    let slot = open_root(&mut s, Chat::id(chat));
    assert_eq!(
        verb_ids(&s, slot),
        vec!["agent.retry", "agent.new", "agent.agents"],
        "a round that was stopped is one to ask again"
    );
}

#[test]
fn the_usage_line_reads_as_a_person_counts() {
    let mut s = session();
    let chat = send_new(&mut s, "hello");
    let run = model::latest_run(s.store(), chat).expect("the run");
    s.store()
        .write(move |c| {
            model::set_run_usage_tx(
                c,
                run.id,
                &Cost {
                    input: 2100,
                    output: 310,
                    cached: 1900,
                },
            )
        })
        .expect("a usage worth reading");
    let slot = open_root(&mut s, Chat::id(chat));
    let turn = with_chat(&s, slot, |c| c.turns()[1].clone());
    assert_eq!(
        with_chat(&s, slot, |c| c.usage_line(&turn)),
        Some("2.1k in (1.9k cached), 310 out".to_string())
    );
}

#[test]
fn the_agents_list_shows_what_each_chat_is_doing_and_narrows_to_it() {
    let mut s = session();
    plant(&s, vec![Reply::always(Answer::Fail("gateway: no".into()))]);
    send_new(&mut s, "the failed one");
    plant(&s, vec![Reply::always(Answer::Text("answered".into()))]);
    let good = send_new(&mut s, "the second one");

    let list = open_root(&mut s, Agents::id());
    let rows = with_agents(&s, list, |a| a.list_mut().table().rows(s.store(), 0, 10));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].title, "the second one", "newest first");
    assert_eq!(rows[0].status, model::DONE);
    assert_eq!(rows[0].model, MODEL);
    assert_eq!(rows[1].status, model::FAILED);

    // The tags, and the free text over the turns' bodies.
    with_agents(&s, list, |a| a.list_mut().set_filter("@failed"));
    assert_eq!(with_agents(&s, list, |a| a.len()), 1);
    with_agents(&s, list, |a| a.list_mut().set_filter("answered"));
    assert_eq!(
        with_agents(&s, list, |a| a.len()),
        1,
        "the word is in a turn, not in the title"
    );
    with_agents(&s, list, |a| {
        a.list_mut().set_filter(&format!("@model:{MODEL}"))
    });
    assert_eq!(with_agents(&s, list, |a| a.len()), 2);
    with_agents(&s, list, |a| a.list_mut().set_filter(""));

    // The cursor shows the chat beside the list.
    let nav = with_agents(&s, list, |a| a.go(0)).expect("a row");
    assert_eq!(
        nav,
        Nav::Preview {
            from: list,
            id: Chat::id(good)
        }
    );
}

#[test]
fn no_bar_wears_a_letter_twice_or_a_reserved_one() {
    let mut s = session();
    let chat = send_new(&mut s, "hello");
    let list = open_root(&mut s, Agents::id());
    with_agents(&s, list, |a| {
        a.go(0);
        a.toggle_mark();
    });
    let open = open_root(&mut s, Chat::id(chat));
    with_chat(&s, open, |c| c.set_draft("more"));
    let blank = open_root(&mut s, Chat::new_id());

    for slot in [list, open, blank] {
        let verbs = s.panel(slot).expect("the panel").borrow().verbs();
        assert!(!verbs.is_empty(), "slot {slot} wears nothing");
        let mut seen: Vec<char> = Vec::new();
        for v in &verbs {
            let Some(c) = v.accel else { continue };
            let c = c.to_ascii_lowercase();
            assert!(
                !crate::shell::keys::is_reserved(c),
                "{} wears cmd+{c}, which the workspace keeps",
                v.id
            );
            assert!(!seen.contains(&c), "two verbs on one bar wear cmd+{c}");
            seen.push(c);
        }
    }
}

#[test]
fn the_app_registers_its_two_kinds_its_ladder_and_its_roots() {
    let s = session();
    assert!(s.apps().kind(Chat::TAG).is_some());
    assert!(s.apps().kind(Agents::TAG).is_some());
    let roots: Vec<Root> = AGENT.roots();
    let labels: Vec<&str> = roots.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, vec!["agents", "new chat"]);
    assert_eq!(roots[0].id, Agents::id());
    assert_eq!(roots[1].id, Chat::new_id());
    assert!(AGENT.schema().is_some());
    assert_eq!(AGENT.problems().len(), 1);
}

// -- the prompt ------------------------------------------------------------------

#[test]
fn the_request_says_what_the_model_is_told() {
    let chat = ChatRow {
        id: 1,
        title: "hello".into(),
        model: MODEL.to_string(),
        created: 0.0,
        updated: 0.0,
    };
    let turns = vec![
        Turn::new(Message::user("what is in my inbox?")),
        Turn::new(Message::assistant("three letters")),
        Turn::new(Message::user("archive them")),
    ];
    let tools = vec![look()];
    let describes = vec![
        (
            "mail",
            "a conversation is rows of `message` joined by `thread`.",
        ),
        ("files", "the disk is the state; there are no tables."),
    ];
    let req = prompt::request(&chat, &turns, &tools, &describes, None);

    assert_eq!(req.model, MODEL);
    assert!(req.stream);
    assert_eq!(req.reasoning_effort.as_deref(), Some(REASONING_EFFORT));
    assert_eq!(
        req.tools
            .iter()
            .map(|t| t.function.name.as_str())
            .collect::<Vec<_>>(),
        vec!["test.look"]
    );

    let system = req.messages[0].text().to_string();
    assert_eq!(req.messages[0].role, Role::System);
    assert!(system.contains("superapp"));
    assert!(system.contains("sql.query") && system.contains("sql.write"));
    for (id, said) in &describes {
        assert!(system.contains(&format!("### {id}")), "{system}");
        assert!(system.contains(said), "{system}");
    }
    assert!(
        !system.contains("what the person is looking at"),
        "no panel was given"
    );

    // The turns follow, in order and unchanged.
    assert_eq!(
        req.messages[1..]
            .iter()
            .map(Message::text)
            .collect::<Vec<_>>(),
        vec!["what is in my inbox?", "three letters", "archive them"]
    );

    // Nothing of the moment: the same chat asks the same question twice.
    let again = prompt::request(&chat, &turns, &tools, &describes, None);
    assert_eq!(again, req, "no clock, no counts, nothing per-request");

    // …and a panel in context lands under its own heading.
    let with_panel = prompt::request(
        &chat,
        &turns,
        &tools,
        &describes,
        Some("<panel id=\"inbox\" title=\"inbox\">rows</panel>"),
    );
    let system = with_panel.messages[0].text();
    assert!(system.contains("what the person is looking at"));
    assert!(system.contains("<panel id=\"inbox\""));
}

// -- the real gateway's refusals ------------------------------------------------

#[test]
fn a_refusal_reads_as_its_sentence_and_a_bad_token_says_so() {
    let json = r#"{"error":{"message":"Authentication error","code":10000}}"#;
    let why = real::refused(401, json.to_string());
    assert_eq!(why.status, Some(401));
    assert_eq!(
        why.message, "gateway: unauthorized — Authentication error",
        "401 and 403 are the token, and the problem source keys on the word"
    );

    let why = real::refused(403, "no".to_string());
    assert!(
        why.message.starts_with("gateway: unauthorized — "),
        "{why:?}"
    );

    // A body that is not JSON is the body, which is what an HTML page from
    // a bad account id comes to.
    let why = real::refused(404, "<html>no such gateway</html>".to_string());
    assert_eq!(why.status, Some(404));
    assert_eq!(why.message, "<html>no such gateway</html>");
    assert_eq!(why.to_string(), "404: <html>no such gateway</html>");

    // JSON without the shape the providers use is still its own text.
    let why = real::refused(500, r#"{"oops":true}"#.to_string());
    assert_eq!(why.message, r#"{"oops":true}"#);
}

// -- what `attach` copies out of the registry -----------------------------------

/// A test app with something to say and something to offer.
struct Tester;

impl App for Tester {
    fn id(&self) -> &'static str {
        "tester"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        &[]
    }
    fn describe(&self) -> Option<&'static str> {
        Some("one table, `tester_thing`, and a row is a thing.")
    }
    fn tools(&self) -> Vec<Tool> {
        vec![look()]
    }
    fn as_any(&self) -> &dyn StdAny {
        self
    }
}
static TESTER: Tester = Tester;
static WITH_TESTER: &[&dyn App] = &[&TESTER, &AGENT];

#[test]
fn attach_copies_every_tool_and_every_describe() {
    let apps = Apps::new(WITH_TESTER);
    let (tools, describes) = Agent::registry_of(&apps);
    assert_eq!(
        tools.iter().map(|t| t.name).collect::<Vec<_>>(),
        vec!["test.look"],
        "the whole list, whoever offered it"
    );
    assert_eq!(
        describes,
        vec![("tester", "one table, `tester_thing`, and a row is a thing.")],
        "an app with nothing to say says nothing"
    );
}

#[test]
fn a_second_round_of_calls_answers_only_its_own() {
    let mut s = session();
    plant(
        &s,
        vec![Reply::when(
            "rename",
            Answer::Call {
                name: "files.rename".into(),
                arguments: json!({}),
                then: "and that is done.".into(),
            },
        )],
    );
    let chat = send_new(&mut s, "please rename it");
    let run = model::latest_run(s.store(), chat).expect("the run").id;
    calls::run_pending_calls(&mut s, chat);
    s.settle();
    assert_eq!(answered_calls(&s, chat), vec!["call_1".to_string()]);

    // A second round on the same run, by hand — the fake answers a tool
    // result with words and cannot script one: another assistant turn
    // asking for a call, the call already answered, the run back to
    // waiting.
    s.store()
        .write(move |c| {
            let (turn, _) = model::add_turn_tx(
                c,
                chat,
                &Turn::new(Message::of(Role::Assistant)).by(run),
                0.0,
            )?;
            let call = ToolCall {
                id: "call_2".into(),
                r#type: "function".into(),
                function: FunctionCall {
                    name: "files.rename".into(),
                    arguments: "{}".into(),
                },
            };
            let id = model::add_call_tx(c, run, turn, &call, 0.0)?;
            model::set_call_tx(c, id, model::CALL_DONE, "ok", 0.0)?;
            model::set_run_status_tx(c, run, model::WAITING, None, 0.0)
        })
        .expect("a second round, by hand");
    s.workers().kick_all();
    s.settle();

    assert_eq!(
        answered_calls(&s, chat),
        vec!["call_1".to_string(), "call_2".to_string()],
        "each call is answered once, whatever round it came in"
    );
}

/// Which calls the transcript has a `tool` message for, in order.
fn answered_calls(s: &Session, chat: ChatId) -> Vec<String> {
    model::turns(s.store(), chat)
        .iter()
        .filter(|t| t.message.role == Role::Tool)
        .map(|t| t.message.tool_call_id.clone().unwrap_or_default())
        .collect()
}
