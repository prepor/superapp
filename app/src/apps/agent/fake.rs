//! The scripted gateway: what every test, every suite and every library
//! mount gets instead of a model.
//!
//! It answers from a **script** — a list of replies, each either a text, a
//! tool call with the text that follows its result, or a failure — matched
//! to the person's latest message by a keyword or taken in order. No clock,
//! no network, no token: a scripted run has nothing to find and nothing to
//! wait for.
//!
//! Two things it does not shortcut. It streams: the answer arrives as
//! word-sized chunks through the same [`stream_completion`] loop and the
//! same [`Assembler`](super::wire::Assembler) a real answer does, so a live
//! tail, a stop and a tool call's fragments are all exercised by a fake
//! run. And it records: [`FakeGateway::requests`] is every request it was
//! given, which is how a test reads what the model was told.
//!
//! Shared inside, like mail's servers and the kernel's own fakes: a
//! production world gives each worker its own world, and the run being
//! scripted is one conversation.

// A script is a vocabulary, and this module is where the whole of it is
// spelled: what no suite in this build plants — a filtered answer, the
// record of what the model was told — a test does.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::gateway::{stream_completion, Failure, Flow, Gateway};
use super::wire::{
    ChatRequest, Choice, Chunk, Completion, Delta, FunctionDelta, PromptTokensDetails, Role,
    ToolCallDelta, Usage,
};

/// What the fake will say, in order.
pub type Script = Vec<Reply>;

/// One entry of a script.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// A word that must be in the person's latest message for this reply to
    /// be the one, compared without case. `None` matches anything, which is
    /// what an ordered script uses.
    pub when: Option<String>,
    pub answer: Answer,
}

impl Reply {
    /// A reply for whatever is asked next.
    #[must_use]
    pub fn always(answer: Answer) -> Reply {
        Reply { when: None, answer }
    }

    /// A reply for a message with this word in it.
    #[must_use]
    pub fn when(word: impl Into<String>, answer: Answer) -> Reply {
        Reply {
            when: Some(word.into()),
            answer,
        }
    }
}

/// What one reply says.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// A text turn, finishing `stop`. Two placeholders are filled in:
    /// `{panel}` is the title of the first panel chip in the prompt, and
    /// `{user}` is the person's latest message.
    Text(String),
    /// A tool call, finishing `tool_calls`. `then` is what the fake says on
    /// the next request, once the call's result has come back.
    Call {
        name: String,
        arguments: Value,
        then: String,
    },
    /// The gateway refusing, as a failure with no status.
    Fail(String),
    /// A text that runs out of room: finishing `length`, which the chat
    /// marks *cut short*.
    Cut(String),
    /// A filter, finishing `content_filter` with nothing said.
    Filtered,
}

/// The scripted model.
#[derive(Clone, Default)]
pub struct FakeGateway {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    script: Script,
    /// Which entries have been spent. An entry answers once.
    used: Vec<bool>,
    /// The `then` texts of the calls issued and not yet answered, oldest
    /// first — a queue, because a turn may ask for more than one call.
    pending: VecDeque<String>,
    /// Every request, in order.
    requests: Vec<ChatRequest>,
    /// How many calls this fake has issued, so their ids differ.
    calls: u64,
}

impl std::fmt::Debug for FakeGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.state.lock().expect("the fake gateway");
        f.debug_struct("FakeGateway")
            .field("script", &g.script.len())
            .field("requests", &g.requests.len())
            .finish()
    }
}

impl FakeGateway {
    /// A fake that will say these things.
    #[must_use]
    pub fn new(script: Script) -> FakeGateway {
        let fake = FakeGateway::default();
        fake.plant(script);
        fake
    }

    /// What a build with nobody's script in it says: the answers the e2e
    /// suites ask for, and the greeting for anything else.
    ///
    /// Every entry but the last is a keyword, so a suite says which answer
    /// it wants in the words it types and the order of the list never
    /// matters. The last has no word on it and takes whatever is left,
    /// which is what an unscripted question gets.
    #[must_use]
    pub fn default_script() -> FakeGateway {
        FakeGateway::new(vec![
            Reply::when("fail", Answer::Fail("the gateway is down".into())),
            Reply::when("cut", Answer::Cut("This answer is long and it".into())),
            Reply::when(
                "rename",
                Answer::Call {
                    name: "files.rename".into(),
                    arguments: serde_json::json!({
                        "path": "~/Downloads/README.txt",
                        "name": "readme-renamed.txt",
                    }),
                    then: "Renamed it.".into(),
                },
            ),
            // Twice over, because a reply is spent once it has answered and
            // the gate's suite asks the same thing twice: once to refuse
            // it, once to allow it.
            Self::delete_the_readme(),
            Self::delete_the_readme(),
            Reply::when("looking", Answer::Text("You are looking at {panel}.".into())),
            // What *continue* asks for, and what it is worth: the rest of
            // the sentence the `cut` reply above stopped in the middle of.
            Reply::when("continue", Answer::Text("… and here is the rest.".into())),
            Reply::always(Answer::Text("Hello. I am the assistant.".into())),
        ])
    }

    /// One call of a tool that asks before it runs — what the gate's own
    /// suite sends for. The `then` is what the model says once the call has
    /// answered, whatever it answered: this fake does not read the result,
    /// so *Deleted it.* follows a refusal as readily as it follows a
    /// deletion.
    fn delete_the_readme() -> Reply {
        Reply::when(
            "delete",
            Answer::Call {
                name: "files.trash".into(),
                arguments: serde_json::json!({"path": "~/Downloads/README.txt"}),
                then: "Deleted it.".into(),
            },
        )
    }

    /// Puts a script in and starts it over: nothing spent, no call waiting
    /// on its result, nothing recorded. What a test does before it sends.
    ///
    /// # Panics
    ///
    /// If a previous holder panicked with the fake locked.
    pub fn plant(&self, script: Script) {
        let mut g = self.state.lock().expect("the fake gateway");
        g.used = vec![false; script.len()];
        g.script = script;
        g.pending.clear();
        g.requests.clear();
        g.calls = 0;
    }

    /// Every request this fake has been given, in order — what a test reads
    /// to see what the model was told.
    ///
    /// # Panics
    ///
    /// As [`FakeGateway::plant`].
    #[must_use]
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.state
            .lock()
            .expect("the fake gateway")
            .requests
            .clone()
    }

    /// Which reply answers this request, spent as it is chosen.
    ///
    /// A request whose last message is a tool result is the run coming back
    /// with what a call came to: the answer is the `then` of the call that
    /// asked for it, and no script entry is spent.
    fn choose(&self, req: &ChatRequest) -> Answer {
        let mut g = self.state.lock().expect("the fake gateway");
        g.requests.push(req.clone());
        if req.messages.last().is_some_and(|m| m.role == Role::Tool) {
            return Answer::Text(g.pending.pop_front().unwrap_or_else(|| "done".into()));
        }
        let asked = req.last_user().unwrap_or_default().to_lowercase();
        let by_word = (0..g.script.len()).find(|&i| {
            !g.used[i]
                && g.script[i]
                    .when
                    .as_ref()
                    .is_some_and(|w| asked.contains(&w.to_lowercase()))
        });
        let chosen = by_word
            .or_else(|| (0..g.script.len()).find(|&i| !g.used[i] && g.script[i].when.is_none()));
        match chosen {
            Some(i) => {
                g.used[i] = true;
                g.script[i].answer.clone()
            }
            // A script that has run out still answers: a suite that sends
            // one message more than it planned should not hang.
            None => Answer::Text("…".into()),
        }
    }

    /// The next call id, and the text that follows the call's result.
    fn issue(&self, then: &str) -> String {
        let mut g = self.state.lock().expect("the fake gateway");
        g.calls += 1;
        g.pending.push_back(then.to_string());
        format!("call_{}", g.calls)
    }
}

impl Gateway for FakeGateway {
    fn complete(
        &mut self,
        req: &ChatRequest,
        on: &mut dyn FnMut(&Chunk) -> Flow,
    ) -> Result<Completion, Failure> {
        let answer = self.choose(req);
        if let Answer::Fail(why) = &answer {
            return Err(Failure::new(why.clone()));
        }
        let events = self.events(req, &answer);
        stream_completion(events.into_iter().map(Ok), on)
    }
}

impl FakeGateway {
    /// The answer as the events a reader would hand a gateway: the chunks,
    /// serialised, and the stream's full stop.
    fn events(&self, req: &ChatRequest, answer: &Answer) -> Vec<String> {
        let mut chunks = vec![role_chunk()];
        let spoken = match answer {
            Answer::Text(text) | Answer::Cut(text) => {
                let text = self.fill(req, text);
                chunks.extend(words(&text));
                text
            }
            Answer::Call {
                name,
                arguments,
                then,
            } => {
                let id = self.issue(then);
                let arguments = arguments.to_string();
                chunks.extend(call_chunks(&id, name, &arguments));
                arguments
            }
            // Nothing said, and nothing to say it in.
            Answer::Filtered | Answer::Fail(_) => String::new(),
        };
        chunks.push(finish_chunk(match answer {
            Answer::Call { .. } => "tool_calls",
            Answer::Cut(_) => "length",
            Answer::Filtered => "content_filter",
            _ => "stop",
        }));
        chunks.push(usage_chunk(req, &spoken));
        let mut events: Vec<String> = chunks
            .iter()
            // A chunk of this fake's own making always serialises.
            .map(|c| serde_json::to_string(c).unwrap_or_default())
            .collect();
        events.push("[DONE]".to_string());
        events
    }

    /// The two placeholders a scripted text may carry.
    fn fill(&self, req: &ChatRequest, text: &str) -> String {
        text.replace("{panel}", &panel_title(req))
            .replace("{user}", req.last_user().unwrap_or_default())
    }
}

/// The title of the first panel chip the prompt carries, or *no panel*.
///
/// The chip is rendered into a system message as `<panel id="…" title="…"
/// …>`, so the fake reads what a real model would read and a test can prove
/// the chip reached the request.
fn panel_title(req: &ChatRequest) -> String {
    req.messages
        .iter()
        .filter(|m| m.role == Role::System)
        .find_map(|m| title_in(m.text()))
        .unwrap_or_else(|| "no panel".to_string())
}

/// `title="…"` of the first `<panel …>` tag in a text.
fn title_in(text: &str) -> Option<String> {
    let after = text.split("<panel ").nth(1)?;
    let tag = after.split('>').next().unwrap_or(after);
    let value = tag.split("title=\"").nth(1)?;
    Some(value.split('"').next()?.to_string())
}

/// The chunk that opens a turn: who is speaking, and nothing yet.
fn role_chunk() -> Chunk {
    Chunk {
        id: Some("fake".into()),
        choices: vec![Choice {
            index: 0,
            delta: Delta {
                role: Some(Role::Assistant),
                content: Some(String::new()),
                ..Delta::default()
            },
            finish_reason: None,
        }],
        ..Chunk::default()
    }
}

/// The text, a word at a time — what makes a live tail worth drawing.
fn words(text: &str) -> Vec<Chunk> {
    text.split_inclusive(' ')
        .map(|word| Chunk {
            choices: vec![Choice {
                delta: Delta {
                    content: Some(word.to_string()),
                    ..Delta::default()
                },
                ..Choice::default()
            }],
            ..Chunk::default()
        })
        .collect()
}

/// One tool call, in the two pieces the wire sends it in: the id and the
/// name first, the arguments after.
fn call_chunks(id: &str, name: &str, arguments: &str) -> Vec<Chunk> {
    let piece = |delta: ToolCallDelta| Chunk {
        choices: vec![Choice {
            delta: Delta {
                tool_calls: vec![delta],
                ..Delta::default()
            },
            ..Choice::default()
        }],
        ..Chunk::default()
    };
    vec![
        piece(ToolCallDelta {
            index: 0,
            id: Some(id.to_string()),
            r#type: Some("function".into()),
            function: Some(FunctionDelta {
                name: Some(name.to_string()),
                arguments: Some(String::new()),
            }),
        }),
        piece(ToolCallDelta {
            index: 0,
            function: Some(FunctionDelta {
                name: None,
                arguments: Some(arguments.to_string()),
            }),
            ..ToolCallDelta::default()
        }),
    ]
}

/// Why it stopped.
fn finish_chunk(reason: &str) -> Chunk {
    Chunk {
        choices: vec![Choice {
            index: 0,
            delta: Delta::default(),
            finish_reason: Some(reason.to_string()),
        }],
        ..Chunk::default()
    }
}

/// The last chunk: no choices, only what the turn cost.
///
/// The arithmetic is fixed rather than realistic — four characters to a
/// token, a hundred for the wrapping — so a suite can assert on it.
fn usage_chunk(req: &ChatRequest, spoken: &str) -> Chunk {
    let sent = serde_json::to_string(req).unwrap_or_default().len() as u64;
    let prompt_tokens = 100 + sent / 4;
    let completion_tokens = spoken.chars().count() as u64 / 4;
    Chunk {
        usage: Some(Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_tokens_details: Some(PromptTokensDetails { cached_tokens: 0 }),
        }),
        ..Chunk::default()
    }
}
