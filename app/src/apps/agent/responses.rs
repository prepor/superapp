//! OpenAI Responses translated to the agent's transcript and live tail.
//!
//! The store remains the conversation's source of truth: `store: false`,
//! full input on each request, and encrypted reasoning kept with the output
//! for the next tool round. Switching models carries the visible transcript
//! and tool results; only the model that wrote an output reuses its opaque
//! reasoning and message phases.

use serde_json::{json, Value};

use super::gateway::{Failure, Flow};
use super::wire::{
    ChatRequest, Choice, Chunk, Completion, Delta, Finish, FunctionCall, Message,
    PromptTokensDetails, ResponseOutput, Role, ToolCall, Usage,
};

/// OpenAI function names accept letters, digits, underscores and dashes.
/// Escape the app's dotted names and the escape character itself, so the
/// mapping stays reversible even for a name containing a literal `_2e`.
fn wire_name(name: &str) -> String {
    name.replace('_', "_5f").replace('.', "_2e")
}

fn app_name(name: &str) -> String {
    name.replace("_2e", ".").replace("_5f", "_")
}

pub(super) fn request_body(req: &ChatRequest) -> Value {
    let mut input = Vec::new();
    for message in &req.messages {
        if let Some(kept) = message.response.as_ref().filter(|r| r.model == req.model) {
            input.extend(kept.items.iter().cloned());
            continue;
        }
        if message.role == Role::Tool {
            input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id,
                "output": message.text(),
            }));
            continue;
        }
        // A chips-only send still has a user turn. Only omit empty text
        // when an assistant's tool calls carry the message instead.
        if let Some(text) = message.content.as_ref().filter(|t| {
            !t.is_empty() || message.role != Role::Assistant || message.tool_calls.is_empty()
        }) {
            input.push(json!({"role": message.role, "content": text}));
        }
        for call in &message.tool_calls {
            input.push(json!({
                "type": "function_call",
                "call_id": call.id,
                "name": wire_name(&call.function.name),
                "arguments": call.function.arguments,
            }));
        }
    }
    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if let Some(effort) = &req.reasoning_effort {
        body["reasoning"] = json!({"effort": effort, "summary": "auto"});
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": wire_name(&t.function.name),
                    "description": t.function.description,
                    "parameters": t.function.parameters,
                    // The apps' schemas have optional fields. Keep their meaning
                    // rather than letting Responses make every property required.
                    "strict": false,
                })
            })
            .collect();
    }
    body
}

/// Stream text and reasoning summaries as the same chunks the chat already
/// draws. The terminal response contains the complete output in order,
/// including tool calls, their ids, reasoning, and usage.
pub(super) fn stream(
    events: impl Iterator<Item = std::io::Result<String>>,
    model: &str,
    on: &mut dyn FnMut(&Chunk) -> Flow,
) -> Result<Completion, Failure> {
    for event in events {
        let event = event.map_err(|e| Failure::new(format!("the stream broke: {e}")))?;
        let data = event.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let event: Value = serde_json::from_str(data).map_err(|e| {
            Failure::new(format!("the gateway sent no response event — {e}: {data}"))
        })?;
        if let Some(error) = event.get("error").filter(|e| !e.is_null()) {
            return Err(failed(error));
        }
        let kind = string(&event, "type")?;
        let mut delta = Delta::default();
        match kind {
            "response.output_text.delta" | "response.refusal.delta" => {
                delta.content = Some(string(&event, "delta")?.to_string());
            }
            "response.reasoning_summary_text.delta" => {
                delta.reasoning_content = Some(string(&event, "delta")?.to_string());
            }
            "response.failed" | "response.cancelled" => {
                return Err(failed(&event["response"]["error"]))
            }
            "error" => return Err(failed(&event)),
            _ => {}
        }
        // Check stop even while the model is thinking or spelling tool
        // arguments, when there is no visible text to append.
        let chunk = Chunk {
            choices: vec![Choice {
                delta,
                ..Choice::default()
            }],
            ..Chunk::default()
        };
        if on(&chunk) == Flow::Stop {
            return Err(Failure::new("stopped"));
        }
        if matches!(kind, "response.completed" | "response.incomplete") {
            return completion(&event["response"], model);
        }
    }
    Err(Failure::new(
        "the stream ended without a completed response",
    ))
}

fn failed(error: &Value) -> Failure {
    Failure::new(
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the model could not finish the response"),
    )
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Failure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new(format!("the response is missing {key}")))
}

fn completion(response: &Value, model: &str) -> Result<Completion, Failure> {
    let status = string(response, "status")?;
    if !matches!(status, "completed" | "incomplete") {
        return Err(failed(&response["error"]));
    }
    let items = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::new("the response is missing output"))?;
    let mut message = Message::of(Role::Assistant);
    let mut text = String::new();
    let mut reasoning = Vec::new();
    for item in items {
        match string(item, "type")? {
            "message" => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        match part["type"].as_str() {
                            Some("output_text") => text.push_str(string(part, "text")?),
                            Some("refusal") => text.push_str(string(part, "refusal")?),
                            _ => {}
                        }
                    }
                }
            }
            "reasoning" => {
                if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                    reasoning.extend(parts.iter().filter_map(|p| p["text"].as_str()));
                }
            }
            "function_call" if status == "completed" => {
                let call = ToolCall {
                    id: string(item, "call_id")?.to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: app_name(string(item, "name")?),
                        arguments: string(item, "arguments")?.to_string(),
                    },
                };
                // Never execute a partial or malformed call.
                if item
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s != "completed")
                {
                    return Err(Failure::new(
                        "the response contains an unfinished tool call",
                    ));
                }
                call.input().map_err(Failure::new)?;
                message.tool_calls.push(call);
            }
            _ => {}
        }
    }
    message.content = (!text.is_empty()).then_some(text);
    message.reasoning_content = (!reasoning.is_empty()).then(|| reasoning.join("\n"));
    // An incomplete tool call has no result to pair with; retain just the
    // visible text so continue can send a valid transcript.
    if status == "completed" {
        message.response = Some(ResponseOutput {
            model: model.to_string(),
            items: items.clone(),
        });
    }
    let finish = if status == "incomplete" {
        match response["incomplete_details"]["reason"].as_str() {
            Some("max_output_tokens") => Finish::Length,
            Some("content_filter") => Finish::ContentFilter,
            Some(other) => Finish::Other(other.to_string()),
            None => return Err(Failure::new("the incomplete response has no reason")),
        }
    } else if message.tool_calls.is_empty() {
        Finish::Stop
    } else {
        Finish::ToolCalls
    };
    let usage = response
        .get("usage")
        .filter(|u| u.is_object())
        .map(|u| Usage {
            prompt_tokens: u["input_tokens"].as_u64().unwrap_or_default(),
            completion_tokens: u["output_tokens"].as_u64().unwrap_or_default(),
            total_tokens: u["total_tokens"].as_u64().unwrap_or_default(),
            prompt_tokens_details: u["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .map(|cached_tokens| PromptTokensDetails { cached_tokens }),
        });
    Ok(Completion {
        message,
        finish,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::super::gateway::{request_parts_with, Provider};
    use super::super::model::Turn;
    use super::*;

    fn read(events: Vec<Value>) -> Result<Completion, Failure> {
        stream(
            events.into_iter().map(|e| Ok(e.to_string())),
            "gpt-6-astra",
            &mut |_| Flow::Go,
        )
    }

    fn done(output: Value) -> Value {
        json!({"type": "response.completed", "response": {
            "status": "completed", "output": output,
            "usage": {"input_tokens": 25, "output_tokens": 8, "total_tokens": 33,
                      "input_tokens_details": {"cached_tokens": 20}}
        }})
    }

    fn text_output() -> Value {
        json!([{"id": "msg_1", "type": "message", "role": "assistant",
                "phase": "final_answer", "status": "completed", "content": [
            {"type": "output_text", "text": "Hello.", "annotations": []}
        ]}])
    }

    #[test]
    fn an_empty_assistant_message_with_a_tool_call_emits_only_the_call() {
        let mut message = Message::assistant("");
        message.tool_calls.push(ToolCall {
            id: "call_1".to_string(),
            r#type: "function".to_string(),
            function: FunctionCall {
                name: "test.echo".to_string(),
                arguments: "{}".to_string(),
            },
        });
        let req = ChatRequest::new("gpt-6-astra", vec![message, Message::tool("call_1", "")]);
        assert_eq!(
            request_body(&req)["input"],
            json!([
                {"type": "function_call", "call_id": "call_1", "name": "test_2eecho", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": ""},
            ])
        );
    }

    #[test]
    fn responses_stream_into_the_live_tail_and_keep_usage() {
        let mut output = text_output();
        output.as_array_mut().unwrap().insert(0, json!({
            "type": "reasoning", "id": "rs_1", "encrypted_content": "opaque",
            "summary": [{"type": "summary_text", "text": "Checking the request."}],
        }));
        let events = vec![
            json!({"type": "response.created", "response": {"status": "in_progress"}}),
            json!({"type": "response.reasoning_summary_text.delta", "delta": "Checking "}),
            json!({"type": "response.reasoning_summary_text.delta", "delta": "the request."}),
            json!({"type": "response.output_text.delta", "delta": "Hel"}),
            json!({"type": "response.output_text.delta", "delta": "lo."}),
            done(output),
        ];
        let mut tail = String::new();
        let mut reasoning = String::new();
        let answer = stream(
            events.into_iter().map(|e| Ok(e.to_string())),
            "gpt-6-astra",
            &mut |c| {
                for choice in &c.choices {
                    tail.push_str(choice.delta.content.as_deref().unwrap_or(""));
                    reasoning.push_str(choice.delta.reasoning_content.as_deref().unwrap_or(""));
                }
                Flow::Go
            },
        )
        .unwrap();
        assert_eq!(
            tail, "Hello.",
            "the terminal event does not duplicate the deltas"
        );
        assert_eq!(answer.message.text(), tail);
        assert_eq!(reasoning, "Checking the request.");
        assert_eq!(answer.message.reasoning_content.as_deref(), Some(reasoning.as_str()));
        let turn: Turn = serde_json::from_str(&Turn::new(answer.message).body()).unwrap();
        assert_eq!(turn.message.reasoning_content.as_deref(), Some(reasoning.as_str()));
        assert_eq!(answer.finish, Finish::Stop);
        let usage = answer.usage.unwrap();
        assert_eq!(
            (usage.prompt_tokens, usage.completion_tokens, usage.cached()),
            (25, 8, 20)
        );
    }

    #[test]
    fn tool_rounds_replay_reasoning_phases_and_call_ids_from_the_saved_turn() {
        let output = json!([
            {"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque",
             "summary": [{"type": "summary_text", "text": "Looking up the file."}]},
            {"type": "message", "id": "msg_1", "role": "assistant", "phase": "commentary",
             "status": "completed", "content": [{"type": "output_text", "text": "I will look.", "annotations": []}]},
            {"type": "function_call", "id": "fc_1", "call_id": "call_1", "status": "completed",
             "name": "files_2eread_5ftext", "arguments": "{\"path\":\"readme.txt\"}"}
        ]);
        let answer = read(vec![done(output.clone())]).unwrap();
        assert_eq!(answer.finish, Finish::ToolCalls);
        assert_eq!(
            answer.message.tool_calls[0].function.name,
            "files.read_text"
        );
        assert_eq!(answer.message.tool_calls[0].id, "call_1");
        assert_eq!(
            answer.message.reasoning_content.as_deref(),
            Some("Looking up the file.")
        );
        let turn: Turn = serde_json::from_str(&Turn::new(answer.message).body()).unwrap();
        let mut req = ChatRequest::new(
            "gpt-6-astra",
            vec![
                Message::system("help with files"),
                turn.message,
                Message::tool("call_1", "file contents"),
            ],
        );
        let body = request_body(&req);
        let input = body["input"].as_array().unwrap();
        assert_eq!(&input[1..4], output.as_array().unwrap());
        assert_eq!(
            input[4],
            json!({"type": "function_call_output", "call_id": "call_1", "output": "file contents"})
        );

        // Another OpenAI model gets the visible transcript and complete
        // tool pairs, without reusing a different model's encrypted state.
        req.model = "gpt-5.6-sol".into();
        let body = request_body(&req);
        assert!(!body.to_string().contains("opaque"));
        assert_eq!(body["input"][1]["content"], "I will look.");
        assert_eq!(body["input"][2]["name"], "files_2eread_5ftext");
        assert_eq!(body["input"][2]["call_id"], body["input"][3]["call_id"]);

        req.model = super::super::MODEL.into();
        let parts = request_parts_with(&Provider::WorkersAi, "a", "g", "token", &req, false);
        let body: Value = serde_json::from_slice(&parts.body).unwrap();
        assert!(body["messages"][1].get("response").is_none());
        assert!(!body.to_string().contains("opaque"));
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "files.read_text"
        );
    }

    #[test]
    fn dotted_tool_names_do_not_collide_with_literal_escape_sequences() {
        for name in [
            "sql.query",
            "mail.send_draft",
            "files.read_2e_text",
            "test._5f",
        ] {
            assert_eq!(app_name(&wire_name(name)), name);
        }
        assert_ne!(wire_name("files.read"), wire_name("files_2eread"));
    }

    #[test]
    fn responses_refusals_and_length_limits_keep_their_words() {
        let refusal = read(vec![done(json!([{"type": "message", "content": [
            {"type": "refusal", "refusal": "I cannot do that."}
        ]}]))])
        .unwrap();
        assert_eq!(refusal.message.text(), "I cannot do that.");
        for (reason, finish) in [
            ("max_output_tokens", Finish::Length),
            ("content_filter", Finish::ContentFilter),
        ] {
            let mut event = done(text_output());
            event["type"] = json!("response.incomplete");
            event["response"]["status"] = json!("incomplete");
            event["response"]["incomplete_details"] = json!({"reason": reason});
            let answer = read(vec![event]).unwrap();
            assert_eq!(answer.finish, finish);
            assert_eq!(answer.message.text(), "Hello.");
            assert!(answer.message.response.is_none());
        }
    }

    #[test]
    fn failed_truncated_and_malformed_responses_never_produce_tool_calls() {
        let why = read(vec![json!({"type": "response.failed", "response": {
            "error": {"message": "model unavailable"}
        }})])
        .unwrap_err();
        assert_eq!(why.message, "model unavailable");
        assert!(read(vec![json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "call_id": "call_1", "name": "files_2edelete", "arguments": "{}"
        }})]).is_err(), "EOF before a terminal response is not permission to run a tool");
        assert!(read(vec![done(
            json!([{"type": "function_call", "call_id": "call_1",
            "name": "files_2edelete", "arguments": "{bad json"}])
        )])
        .is_err());
        assert!(read(vec![json!({"type": "error", "message": "no"})]).is_err());
        assert!(stream(
            [Err(std::io::Error::other("disconnected"))].into_iter(),
            "gpt-6-astra",
            &mut |_| Flow::Go
        )
        .is_err());
    }

    #[test]
    fn stop_works_during_reasoning_before_any_text_arrives() {
        let mut polled = 0;
        let events = std::iter::repeat_with(|| {
            polled += 1;
            Ok(json!({"type": "response.in_progress"}).to_string())
        });
        let why = stream(events, "gpt-6-astra", &mut |_| Flow::Stop).unwrap_err();
        assert_eq!(why.message, "stopped");
        assert_eq!(polled, 1);
    }
}
