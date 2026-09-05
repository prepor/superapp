//! Streams as the gateway sends them, recorded.
//!
//! Written in the wire's own shape — `data:` lines, a blank line between
//! events, `data: [DONE]` at the end — because that is what one can compare
//! against a capture when the wire changes. [`events`] does the framing
//! `kernel::sse` will do at runtime, so what the tests hand
//! [`stream_completion`](super::gateway::stream_completion) is what a
//! reader would hand a gateway: one event's data at a time.
//!
//! The extra fields are on purpose: `object`, `created` and the model's
//! name are in every real chunk and in none of the types, and a chat must
//! not fail because a provider grew another one.

/// The events of an SSE body, as the reader yields them: the `data:` of one
/// event, multi-line data joined, the framing gone.
pub fn events(raw: &str) -> Vec<std::io::Result<String>> {
    raw.split("\n\n")
        .filter_map(|block| {
            let data: Vec<&str> = block
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                .collect();
            (!data.is_empty()).then(|| Ok(data.join("\n")))
        })
        .collect()
}

/// A text turn: the role, three words of content, `stop`, and the usage on
/// a last chunk with no choices at all.
pub const TEXT: &str = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1756900000,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1756900000,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"content":"Your "},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1756900000,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"content":"inbox "},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1756900000,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"content":"has three unread letters."},"finish_reason":null}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1756900000,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1756900000,"model":"@cf/zai-org/glm-5.3-flash","choices":[],"usage":{"prompt_tokens":2100,"completion_tokens":310,"total_tokens":2410,"prompt_tokens_details":{"cached_tokens":1900}}}

data: [DONE]
"#;

/// A tool-call turn: the id and the name first with the arguments empty,
/// then the arguments in three fragments, then `tool_calls`.
pub const TOOL_CALL: &str = r#"data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1756900001,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1756900001,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_9d2","type":"function","function":{"name":"files.rename","arguments":""}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1756900001,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\": \"~/"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1756900001,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"notes.md\", \"to\": "}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1756900001,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"~/notes-2026.md\"}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1756900001,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1756900001,"model":"@cf/zai-org/glm-5.3-flash","choices":[],"usage":{"prompt_tokens":2200,"completion_tokens":48,"total_tokens":2248}}

data: [DONE]
"#;

/// A turn that ran out of room.
pub const LENGTH: &str = r#"data: {"id":"chatcmpl-3","object":"chat.completion.chunk","created":1756900002,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-3","object":"chat.completion.chunk","created":1756900002,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"content":"The first thing to say is"},"finish_reason":null}]}

data: {"id":"chatcmpl-3","object":"chat.completion.chunk","created":1756900002,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}

data: [DONE]
"#;

/// A stream that failed halfway: the gateway's own error, as an event.
pub const MID_STREAM_ERROR: &str = r#"data: {"id":"chatcmpl-4","object":"chat.completion.chunk","created":1756900003,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"One moment"},"finish_reason":null}]}

data: {"error":{"message":"Rate limit exceeded","code":10000}}

data: [DONE]
"#;

/// A model that thinks out loud before it answers.
pub const REASONING: &str = r#"data: {"id":"chatcmpl-5","object":"chat.completion.chunk","created":1756900004,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-5","object":"chat.completion.chunk","created":1756900004,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"reasoning_content":"The person is asking "},"finish_reason":null}]}

data: {"id":"chatcmpl-5","object":"chat.completion.chunk","created":1756900004,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"reasoning_content":"about their inbox."},"finish_reason":null}]}

data: {"id":"chatcmpl-5","object":"chat.completion.chunk","created":1756900004,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"content":"Three unread."},"finish_reason":null}]}

data: {"id":"chatcmpl-5","object":"chat.completion.chunk","created":1756900004,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#;

/// A turn a filter stopped, with nothing said.
pub const CONTENT_FILTER: &str = r#"data: {"id":"chatcmpl-6","object":"chat.completion.chunk","created":1756900005,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-6","object":"chat.completion.chunk","created":1756900005,"model":"@cf/zai-org/glm-5.3-flash","choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}

data: {"id":"chatcmpl-6","object":"chat.completion.chunk","created":1756900005,"model":"@cf/zai-org/glm-5.3-flash","choices":[],"usage":{"prompt_tokens":90,"completion_tokens":0,"total_tokens":90}}

data: [DONE]
"#;
