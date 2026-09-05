//! The chat-completions wire, as Workers AI documents it.
//!
//! One shape is spoken to the gateway: the OpenAI-compatible
//! chat-completions API, which Workers AI serves under the AI Gateway route
//! `/workers-ai/v1/chat/completions` and which every provider behind the
//! gateway shares — so a later model, or a later provider, is this wire
//! under another name.
//!
//! Two rules hold over every type here. **Extra fields are ignored**: the
//! providers add their own, and a chat must not fail because one of them
//! grew a field. **Absent is not empty**: what a provider leaves out reads
//! back as `None` or an empty list, and what this app has nothing to say
//! about is left out of what it sends.
//!
//! [`Assembler`] is the other half: a stream arrives as [`Chunk`]s whose
//! deltas have to be added up — text, reasoning, and tool calls whose
//! `arguments` come as string fragments by index — and it is what makes a
//! [`Completion`] out of them. The fake gateway assembles through the same
//! type as the real one, so a scripted run exercises the assembly a real
//! answer will go through.

// The wire is a whole shape, not the half this build happens to read: a
// message of every role, a call's arguments parsed, the assembler's two
// halves apart. What no code path here asks for, a fixture's test does.
#![allow(dead_code)]

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A list the wire may spell as `null`: Workers AI's deltas carry
/// `"tool_calls": null` on every chunk that has none, and serde's `default`
/// covers only a key that is *absent*. Read as empty either way.
fn null_as_empty<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

// -- what goes out -------------------------------------------------------------

/// One request. Always streamed, so the chat has a live tail to draw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// The tools this build offers, as function definitions. Left out when
    /// there are none.
    #[serde(default, deserialize_with = "null_as_empty", skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// How hard the model thinks before it answers, for the models that
    /// have a reasoning mode. One const in the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl ChatRequest {
    /// The one shape this app sends: streamed, with the usage on the last
    /// chunk. The tools and the reasoning effort are the caller's to add.
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages,
            tools: Vec::new(),
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
        }
    }

    /// The latest thing the person said, which is what the fake matches its
    /// script against and what a title is made from.
    #[must_use]
    pub fn last_user(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.content.as_deref())
    }
}

/// What the stream is asked to carry beyond the deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamOptions {
    /// The token counts, on a last chunk with no choices in it.
    pub include_usage: bool,
}

/// Who is speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// One message of a conversation, in the shape the wire keeps it — which is
/// the shape a turn's row stores, so the next request is built from the
/// rows verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// What the assistant asked to run. Empty on every other role.
    #[serde(default, deserialize_with = "null_as_empty", skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Which call this message answers. Only on a `tool` message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// The model's own reasoning, when it sends any: shown folded and
    /// muted, and sent back as it came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl Message {
    /// A message of a role with nothing on it yet.
    #[must_use]
    pub fn of(role: Role) -> Message {
        Message {
            role,
            content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// The prompt.
    #[must_use]
    pub fn system(text: impl Into<String>) -> Message {
        Message {
            content: Some(text.into()),
            ..Message::of(Role::System)
        }
    }

    /// What the person said.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Message {
        Message {
            content: Some(text.into()),
            ..Message::of(Role::User)
        }
    }

    /// What the model said.
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Message {
        Message {
            content: Some(text.into()),
            ..Message::of(Role::Assistant)
        }
    }

    /// What a call came to, answering the call by its id.
    #[must_use]
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Message {
        Message {
            content: Some(content.into()),
            tool_call_id: Some(call_id.into()),
            ..Message::of(Role::Tool)
        }
    }

    /// The text of it, or the empty string where there is none.
    #[must_use]
    pub fn text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// One tool as the request declares it. The wire has one `type`, `function`,
/// and room for others it does not use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(default = "function")]
    pub r#type: String,
    pub function: FunctionDef,
}

/// A tool's name, its sentence, and its JSON Schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    /// The schema, verbatim: [`Tool::input`](kernel::tool::Tool::input).
    pub parameters: Value,
}

impl From<&kernel::tool::Tool> for ToolDef {
    /// A tool of this build, as the request declares it.
    fn from(t: &kernel::tool::Tool) -> ToolDef {
        ToolDef {
            r#type: function(),
            function: FunctionDef {
                name: t.name.to_string(),
                description: t.description.to_string(),
                parameters: t.input.clone(),
            },
        }
    }
}

fn function() -> String {
    "function".to_string()
}

/// One use of a tool, as the assistant's message carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(default = "function")]
    pub r#type: String,
    pub function: FunctionCall,
}

/// The name and the arguments of one call. The arguments are the wire's
/// JSON *string*, not an object: the model writes them a fragment at a
/// time, and they are parsed once whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    /// The arguments as JSON, which is what a tool is checked and run
    /// against.
    ///
    /// # Errors
    ///
    /// If the model wrote something that is not JSON.
    pub fn input(&self) -> Result<Value, String> {
        parse_arguments(&self.function.name, &self.function.arguments)
    }
}

// -- what comes back -----------------------------------------------------------

/// One event of a stream. A chunk carries deltas, or the usage with no
/// choices at all, or — the gateway's own way of failing mid-stream — an
/// error and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty", skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

/// One choice of a chunk. This app asks for one and reads them all the
/// same.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub delta: Delta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// What this chunk adds to the message being assembled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty", skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallDelta>,
}

/// A piece of one tool call. `index` is which call of the turn it belongs
/// to: the id and the name arrive once, the arguments in fragments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCallDelta {
    #[serde(default)]
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionDelta>,
}

/// A piece of one call's function: the name, or another fragment of the
/// arguments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// What the turn cost. Comes on the stream's last chunk, which has no
/// choices on it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

impl Usage {
    /// How much of the prompt the model had already: the number the muted
    /// line under a turn shows in brackets.
    #[must_use]
    pub fn cached(&self) -> u64 {
        self.prompt_tokens_details.map_or(0, |d| d.cached_tokens)
    }
}

/// The part of the prompt the model did not have to read again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u64,
}

/// A failure the gateway sent as an event rather than as a status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    pub message: String,
    /// A number on one provider and a string on another, so it is kept as
    /// it came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// It said what it had to say.
    Stop,
    /// It wants tools run; the calls are on the message.
    ToolCalls,
    /// It ran out of room. The turn is cut, and *continue* is offered.
    Length,
    /// A filter stopped it. The turn says so, muted, and the chat goes on.
    ContentFilter,
    /// Something this build has no word for, kept as it came — including
    /// `none`, for a stream that ended without saying.
    Other(String),
}

impl Finish {
    /// The wire's word, read.
    #[must_use]
    pub fn parse(reason: &str) -> Finish {
        match reason {
            "stop" => Finish::Stop,
            "tool_calls" => Finish::ToolCalls,
            "length" => Finish::Length,
            "content_filter" => Finish::ContentFilter,
            other => Finish::Other(other.to_string()),
        }
    }

    /// The word the transcript and the run's row keep.
    #[must_use]
    pub fn word(&self) -> &str {
        match self {
            Finish::Stop => "stop",
            Finish::ToolCalls => "tool_calls",
            Finish::Length => "length",
            Finish::ContentFilter => "content_filter",
            Finish::Other(w) => w,
        }
    }
}

/// One assembled answer: the message to store, why it stopped, and what it
/// cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub message: Message,
    pub finish: Finish,
    pub usage: Option<Usage>,
}

// -- adding the stream up ------------------------------------------------------

/// A stream, added up as it arrives.
///
/// [`Assembler::text`] is the live tail — what the chat draws while the
/// answer is still coming — and [`Assembler::finish`] is the whole of it,
/// with each call's arguments parsed once, whole.
#[derive(Debug, Default)]
pub struct Assembler {
    text: String,
    reasoning: String,
    calls: Vec<Partial>,
    finish: Option<Finish>,
    usage: Option<Usage>,
}

/// One tool call being put together, still in fragments.
#[derive(Debug, Default)]
struct Partial {
    index: u32,
    id: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl Assembler {
    #[must_use]
    pub fn new() -> Assembler {
        Assembler::default()
    }

    /// One chunk added.
    ///
    /// # Errors
    ///
    /// If the chunk is the gateway saying the stream failed: the message it
    /// gave, which is what the run's row records and the chat shows.
    pub fn push(&mut self, chunk: &Chunk) -> Result<(), String> {
        if let Some(err) = &chunk.error {
            return Err(err.message.clone());
        }
        for choice in &chunk.choices {
            if let Some(text) = &choice.delta.content {
                self.text.push_str(text);
            }
            if let Some(text) = &choice.delta.reasoning_content {
                self.reasoning.push_str(text);
            }
            for call in &choice.delta.tool_calls {
                self.take(call);
            }
            if let Some(reason) = &choice.finish_reason {
                self.finish = Some(Finish::parse(reason));
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }
        Ok(())
    }

    /// A tool call's piece, by the index that says which call it is.
    fn take(&mut self, delta: &ToolCallDelta) {
        let partial = match self.calls.iter_mut().find(|c| c.index == delta.index) {
            Some(found) => found,
            None => {
                self.calls.push(Partial {
                    index: delta.index,
                    ..Partial::default()
                });
                self.calls.last_mut().expect("the call just pushed")
            }
        };
        if let Some(id) = &delta.id {
            partial.id = Some(id.clone());
        }
        if let Some(kind) = &delta.r#type {
            partial.kind = Some(kind.clone());
        }
        if let Some(f) = &delta.function {
            if let Some(name) = &f.name {
                partial.name = Some(name.clone());
            }
            if let Some(fragment) = &f.arguments {
                partial.arguments.push_str(fragment);
            }
        }
    }

    /// The answer so far — the live tail a chat draws, with a cursor block
    /// at its end.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The model's reasoning so far.
    #[must_use]
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    /// The whole answer: the message, why it stopped, what it cost.
    ///
    /// The arguments of each call are parsed here, once, whole — the point
    /// of keeping them as fragments until the end.
    ///
    /// # Errors
    ///
    /// If a call's arguments are not JSON: the call is named and its text
    /// quoted, because that is a bug in the model's writing and a person
    /// has to see what it wrote.
    pub fn finish(mut self) -> Result<Completion, String> {
        self.calls.sort_by_key(|c| c.index);
        let mut tool_calls = Vec::with_capacity(self.calls.len());
        for partial in &self.calls {
            let name = partial.name.clone().unwrap_or_default();
            let input = parse_arguments(&name, &partial.arguments)?;
            tool_calls.push(ToolCall {
                id: partial.id.clone().unwrap_or_default(),
                r#type: partial.kind.clone().unwrap_or_else(function),
                function: FunctionCall {
                    name,
                    // Re-serialised from what was parsed, so the string
                    // stored on the turn is the JSON the call was read as
                    // and not the fragments' seams.
                    arguments: input.to_string(),
                },
            });
        }
        let message = Message {
            role: Role::Assistant,
            content: (!self.text.is_empty()).then(|| self.text.clone()),
            tool_calls,
            tool_call_id: None,
            reasoning_content: (!self.reasoning.is_empty()).then(|| self.reasoning.clone()),
        };
        Ok(Completion {
            message,
            finish: self.finish.unwrap_or_else(|| Finish::Other("none".into())),
            usage: self.usage,
        })
    }
}

/// One call's arguments, read. An empty string is a call with no arguments,
/// which is how the models spell one.
///
/// # Errors
///
/// If they are not JSON.
fn parse_arguments(name: &str, arguments: &str) -> Result<Value, String> {
    let text = if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    };
    serde_json::from_str(text)
        .map_err(|e| format!("the call to {name} sent arguments that are not JSON — {e}: {text}"))
}
