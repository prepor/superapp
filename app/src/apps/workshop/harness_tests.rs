use super::*;

fn request(provider: Provider) -> RunRequest {
    RunRequest {
        provider,
        model: "default".into(),
        cwd: std::env::temp_dir(),
        prompt: "Inspect $(touch should-not-exist); `echo literal`\nThen explain.".into(),
        resume_session_id: None,
        permission_mode: PermissionMode::WorkspaceWrite,
        mcp: None,
    }
}

#[test]
fn local_mcp_configuration_never_puts_the_run_token_in_argv_or_debug() {
    for provider in [Provider::Codex, Provider::Claude] {
        let mut req = request(provider);
        req.mcp = Some(McpConfig {
            url: "http://127.0.0.1:8123/mcp".into(),
            bearer_token: "private-run-token".into(),
        });
        let args = command_args(&req, &[]);
        assert!(args.iter().any(|a| a.contains("127.0.0.1:8123")));
        assert!(args.iter().any(|a| a.contains(MCP_TOKEN_ENV)));
        assert!(args.iter().all(|a| !a.contains("private-run-token")));
        assert!(!format!("{req:?}").contains("private-run-token"));
    }
}

#[test]
fn codex_resume_keeps_explicit_sandbox_and_stdin_prompt() {
    let mut req = request(Provider::Codex);
    req.resume_session_id = Some("thread-123".into());
    req.model = "selected-model".into();
    let args = command_args(&req, &[PathBuf::from("/repo/.git")]);
    assert!(args.windows(2).any(|a| a == ["resume", "thread-123"]));
    assert!(args.iter().any(|a| a == "sandbox_mode=\"workspace-write\""));
    assert!(args
        .iter()
        .any(|a| a == "sandbox_workspace_write.network_access=true"));
    assert!(args
        .iter()
        .any(|a| a == "sandbox_workspace_write.writable_roots=[\"/repo/.git\"]"));
    assert!(args.windows(2).any(|a| a == ["--model", "selected-model"]));
    assert_eq!(args.last().unwrap(), "-");
    assert!(!args
        .iter()
        .any(|a| a.contains("dangerously") || a.contains("should-not-exist")));
    assert_eq!(prompt_input(&req), req.prompt.as_bytes());
}

#[test]
fn claude_work_mode_auto_allows_only_sandboxed_commands() {
    let req = request(Provider::Claude);
    let args = command_args(&req, &[PathBuf::from("/repo/.git")]);
    assert!(args
        .windows(2)
        .any(|a| a == ["--permission-mode", "acceptEdits"]));
    let settings_index = args.iter().position(|a| a == "--settings").unwrap();
    let settings: Value = serde_json::from_str(&args[settings_index + 1]).unwrap();
    assert_eq!(settings["sandbox"]["enabled"], true);
    assert_eq!(settings["sandbox"]["autoAllowBashIfSandboxed"], true);
    assert_eq!(settings["sandbox"]["allowUnsandboxedCommands"], false);
    assert_eq!(settings["sandbox"]["failIfUnavailable"], true);
    assert_eq!(
        settings["sandbox"]["filesystem"]["allowWrite"],
        json!(["/repo/.git"])
    );
    assert!(!args
        .iter()
        .any(|a| a == "--bare" || a == "--allowedTools" || a == "--model"));
    let input: Value = serde_json::from_slice(&prompt_input(&req)).unwrap();
    assert_eq!(input["message"]["content"], req.prompt);
}

#[test]
fn read_only_modes_do_not_grant_network_or_git_writes() {
    for provider in [Provider::Codex, Provider::Claude] {
        let mut req = request(provider);
        req.permission_mode = PermissionMode::ReadOnly;
        let args = command_args(&req, &[PathBuf::from("/repo/.git")]);
        assert!(!args
            .iter()
            .any(|a| a.contains("network_access=true") || a.contains("allowWrite")));
        assert!(args
            .iter()
            .any(|a| a == "plan" || a == "sandbox_mode=\"read-only\""));
    }
}

#[test]
fn codex_events_keep_session_text_tools_and_usage() {
    let mut decoder = Decoder::new(Provider::Codex, None, "default");
    let session = decoder.decode_line(r#"{"type":"thread.started","thread_id":"thread-1"}"#);
    assert_eq!(session[0].session_id.as_deref(), Some("thread-1"));
    decoder.decode_line(r#"{"type":"turn.started"}"#);
    let tool = decoder.decode_line(r#"{"type":"item.started","item":{"id":"cmd-1","type":"command_execution","command":"cargo test","status":"in_progress"}}"#);
    assert_eq!(tool[0].kind, "item");
    let item = tool[0].item.as_ref().unwrap();
    assert_eq!((item.key.as_str(), item.kind.as_str(), item.name.as_str()), ("cmd-1", "tool", "shell"));
    assert_eq!(item.title, "cargo test");
    assert_eq!(item.status, "running");
    let done = decoder.decode_line(r#"{"type":"item.completed","item":{"id":"cmd-1","type":"command_execution","command":"cargo test","aggregated_output":"12 passed","exit_code":0,"status":"completed"}}"#);
    let item = done[0].item.as_ref().unwrap();
    assert_eq!(item.status, "done");
    assert_eq!(item.body.as_deref(), Some("12 passed"));
    let first = decoder.decode_line(
        r#"{"type":"item.updated","item":{"id":"msg-1","type":"agent_message","text":"Hello"}}"#,
    );
    assert_eq!(first[0].text, "Hello");
    let second = decoder.decode_line(r#"{"type":"item.updated","item":{"id":"msg-1","type":"agent_message","text":"Hello there"}}"#);
    assert_eq!(second[0].text, " there");
    let final_message = decoder.decode_line(r#"{"type":"item.completed","item":{"id":"msg-1","type":"agent_message","text":"Hello there"}}"#);
    assert_eq!(final_message[0].kind, "assistant");
    assert_eq!(final_message[0].text, "Hello there");
    decoder.decode_line(r#"{"type":"turn.completed","usage":{"input_tokens":15,"cached_input_tokens":9,"output_tokens":3}}"#);
    assert!(decoder.completed);
    assert_eq!(decoder.outcome.usage["input_tokens"], 15);
}

#[test]
fn a_transient_codex_error_can_recover_but_failed_turn_cannot() {
    let mut decoder = Decoder::new(Provider::Codex, None, "default");
    let events = decoder.decode_line(r#"{"type":"error","message":"Reconnecting"}"#);
    assert_eq!(events[0].kind, "error");
    assert!(decoder.failure.is_none());
    decoder
        .decode_line(r#"{"type":"turn.failed","error":{"message":"Subscription limit reached"}}"#);
    assert_eq!(
        decoder.failure.as_deref(),
        Some("Subscription limit reached")
    );
}

#[test]
fn claude_stream_does_not_duplicate_the_result_and_keeps_native_model() {
    let mut decoder = Decoder::new(Provider::Claude, None, "opus");
    decoder.decode_line(r#"{"type":"system","subtype":"init","session_id":"session-1","model":"claude-test-model"}"#);
    let delta = decoder.decode_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}}"#);
    assert_eq!(delta[0].kind, "assistant_delta");
    assert_eq!(delta[0].model.as_deref(), Some("claude-test-model"));
    let full = decoder.decode_line(r#"{"type":"assistant","message":{"id":"msg-1","model":"claude-test-model","content":[{"type":"text","text":"Hi"}]},"session_id":"session-1"}"#);
    assert_eq!(full[0].kind, "assistant");
    let result = decoder.decode_line(r#"{"type":"result","subtype":"success","is_error":false,"session_id":"session-1","result":"Hi","usage":{"input_tokens":10,"output_tokens":1},"total_cost_usd":0.0001}"#);
    assert!(result.iter().all(|e| e.kind != "assistant"));
    assert!(decoder.completed);
    assert_eq!(decoder.outcome.session_id.as_deref(), Some("session-1"));
    assert_eq!(decoder.outcome.usage["usage"]["input_tokens"], 10);
}

#[test]
fn claude_denied_tools_and_error_result_reach_the_chat() {
    let mut decoder = Decoder::new(Provider::Claude, Some("session-1".into()), "default");
    let denied = decoder.decode_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call1","is_error":true,"content":"Permission denied for Bash"}]}}"#);
    assert_eq!(denied[0].item.as_ref().unwrap().status, "denied");
    assert!(denied.iter().any(|e| e.kind == "permission_denied"));
    let result = decoder.decode_line(r#"{"type":"result","is_error":true,"errors":["Approval required"],"permission_denials":[{"tool_name":"Bash","tool_use_id":"call1"}]}"#);
    assert!(result.iter().any(|e| e.kind == "permission_denied"));
    assert_eq!(decoder.failure.as_deref(), Some("Approval required"));
}

#[test]
fn model_cache_filters_hidden_models_and_deduplicates() {
    assert_eq!(
        codex_model_ids(&json!({"models":[
            {"slug":"model-1","visibility":"list"}, {"slug":"hidden","visibility":"hide"},
            {"slug":"model-1"}, {"slug":"model-2"}, {"slug":""}
        ]})),
        ["model-1", "model-2"]
    );
}

#[test]
fn authentication_status_never_returns_credentials_or_account_identifiers() {
    assert_eq!(
        auth_summary(
            Provider::Codex,
            true,
            "",
            "Logged in using an API key - sk-secret123"
        ),
        "Signed in with API billing"
    );
    assert_eq!(
        auth_summary(
            Provider::Claude,
            true,
            r#"{"loggedIn":true,"authMethod":"claude.ai","email":"private@example.com"}"#,
            ""
        ),
        "Signed in with Claude subscription"
    );
    assert_eq!(
        auth_summary(
            Provider::Claude,
            false,
            r#"{"loggedIn":false}"#,
            "token: private"
        ),
        "Not signed in"
    );
}

#[tokio::test]
async fn cancellation_remains_observable_before_and_after_waiters_register() {
    let cancelled = CancelToken::new();
    cancelled.cancel();
    tokio::time::timeout(Duration::from_millis(100), cancelled.cancelled())
        .await
        .unwrap();
    let token = CancelToken::new();
    let waiter = token.clone();
    let task = tokio::spawn(async move {
        waiter.cancelled().await;
    });
    tokio::task::yield_now().await;
    token.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
struct FakeCli {
    dir: PathBuf,
    path: PathBuf,
}
#[cfg(unix)]
impl FakeCli {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "workshop-harness-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fake-cli");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir, path }
    }
}
#[cfg(unix)]
impl Drop for FakeCli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn mock_child_receives_literal_prompt_cwd_and_resume_and_streams_output() {
    let cli = FakeCli::new(
        r#"
cat > received-prompt
printf '%s\n' "$@" > received-args
printf '%s' "$SUPERAPP_WORKSHOP_MCP_TOKEN" > received-bridge-token
printf '%s\n' '{"type":"thread.started","thread_id":"resumed-thread"}'
printf '%s\n' 'startup diagnostic' >&2
printf '%s\n' '{"type":"item.completed","item":{"id":"msg-1","type":"agent_message","text":"Done"}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'
"#,
    );
    let mut req = request(Provider::Codex);
    req.cwd = cli.dir.clone();
    req.resume_session_id = Some("resumed-thread".into());
    req.mcp = Some(McpConfig {
        url: "http://127.0.0.1:1/mcp".into(),
        bearer_token: "fixture-run-token".into(),
    });
    let prompt = req.prompt.clone();
    let mut events = Vec::new();
    let result = run_with_executable(req, cli.path.clone(), CancelToken::new(), |e| {
        events.push(e)
    })
    .await
    .unwrap();
    assert_eq!(result.session_id.as_deref(), Some("resumed-thread"));
    assert_eq!(
        std::fs::read_to_string(cli.dir.join("received-prompt")).unwrap(),
        prompt
    );
    assert!(std::fs::read_to_string(cli.dir.join("received-args"))
        .unwrap()
        .contains("resume\nresumed-thread\n"));
    assert!(!cli.dir.join("should-not-exist").exists());
    assert_eq!(
        std::fs::read_to_string(cli.dir.join("received-bridge-token")).unwrap(),
        "fixture-run-token"
    );
    assert!(!std::fs::read_to_string(cli.dir.join("received-args"))
        .unwrap()
        .contains("fixture-run-token"));
    assert!(events
        .iter()
        .any(|e| e.kind == "assistant" && e.text == "Done"));
    assert!(events
        .iter()
        .any(|e| e.kind == "stderr" && e.text == "startup diagnostic"));
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_stops_the_child_and_its_long_running_command() {
    let cli = FakeCli::new("cat >/dev/null\nsleep 30 &\nwait");
    let mut req = request(Provider::Codex);
    req.cwd = cli.dir.clone();
    let token = CancelToken::new();
    let stop = token.clone();
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.cancel();
    });
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        run_with_executable(req, cli.path.clone(), token, |_| {}),
    )
    .await
    .unwrap()
    .unwrap();
    task.await.unwrap();
    assert!(outcome.cancelled);
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_the_run_future_kills_the_provider_and_its_descendant() {
    let cli = FakeCli::new(
        "cat >/dev/null\nsleep 30 &\nprintf '%s %s\\n' \"$$\" \"$!\" > child-pids\nwait",
    );
    let mut req = request(Provider::Codex);
    req.cwd = cli.dir.clone();
    let executable = cli.path.clone();
    let task = tokio::spawn(async move {
        run_with_executable(req, executable, CancelToken::new(), |_| {}).await
    });
    let pids = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(cli.dir.join("child-pids")) {
                let pids: Vec<u32> = text
                    .split_whitespace()
                    .map(|pid| pid.parse().unwrap())
                    .collect();
                if pids.len() == 2 {
                    break pids;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let running = |pid: u32| {
        let output = std::process::Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        // Some Unix hosts leave orphaned zombies until their init reaps them.
        // A zombie cannot keep running commands or retaining terminal pipes.
        output.status.success() && !state.trim().is_empty() && !state.trim().starts_with('Z')
    };
    assert!(pids.iter().all(|pid| running(*pid)));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), async {
        while pids.iter().any(|pid| running(*pid)) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping the harness future left its provider or command running");
}

#[cfg(unix)]
#[tokio::test]
async fn success_exit_without_protocol_completion_is_not_a_successful_turn() {
    let cli = FakeCli::new("cat >/dev/null\nprintf 'bad protocol\\n'");
    let mut req = request(Provider::Codex);
    req.cwd = cli.dir.clone();
    let error = run_with_executable(req, cli.path.clone(), CancelToken::new(), |_| {})
        .await
        .unwrap_err();
    assert!(error.contains("without completing"));
}

#[cfg(unix)]
#[tokio::test]
async fn provider_exit_reports_stderr_and_cannot_hang_on_orphaned_pipes() {
    let cli = FakeCli::new("cat >/dev/null\nprintf 'Sign in required\\n' >&2\nsleep 30 &\nexit 7");
    let mut req = request(Provider::Claude);
    req.cwd = cli.dir.clone();
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        run_with_executable(req, cli.path.clone(), CancelToken::new(), |_| {}),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("Sign in required"));
}

#[test]
fn claude_tool_calls_become_one_item_from_start_to_result() {
    let mut decoder = Decoder::new(Provider::Claude, None, "default");
    let start = decoder.decode_line(r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}},"parent_tool_use_id":null}"#);
    let item = start[0].item.as_ref().unwrap();
    assert_eq!((item.key.as_str(), item.name.as_str(), item.status.as_str()), ("toolu_1", "Bash", "running"));
    let full = decoder.decode_line(r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"Running the tests."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test review\n# second line","description":"Run review tests"}}]},"parent_tool_use_id":null}"#);
    let call = full.iter().find(|e| e.kind == "item").unwrap().item.as_ref().unwrap();
    assert_eq!(call.title, "cargo test review…");
    assert_eq!(call.input.as_ref().unwrap()["command"], "cargo test review\n# second line");
    let said = full.iter().find(|e| e.kind == "assistant").unwrap();
    assert_eq!((said.text.as_str(), said.data["id"].as_str()), ("Running the tests.", Some("msg_1")));
    let result = decoder.decode_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"12 passed"}]}]},"parent_tool_use_id":null}"#);
    let item = result[0].item.as_ref().unwrap();
    assert_eq!((item.key.as_str(), item.name.as_str(), item.status.as_str()), ("toolu_1", "Bash", "done"));
    assert_eq!(item.body.as_deref(), Some("12 passed"));
}

#[test]
fn claude_text_keeps_streaming_and_completion_apart_by_message() {
    let mut decoder = Decoder::new(Provider::Claude, None, "default");
    decoder.decode_line(r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1","model":"claude-test"}}}"#);
    decoder.decode_line(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#);
    let delta = decoder.decode_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}"#);
    assert_eq!((delta[0].kind.as_str(), delta[0].text.as_str(), delta[0].data["id"].as_str()), ("assistant_delta", "Hel", Some("msg_1")));
    decoder.decode_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}"#);
    decoder.decode_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#);
    let full = decoder.decode_line(r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"Hello"}]}}"#);
    assert_eq!(full.len(), 1);
    assert_eq!((full[0].kind.as_str(), full[0].text.as_str()), ("assistant", "Hello"));
    // A second text block of the same message, after a tool call, joins it
    // rather than replacing it.
    let again = decoder.decode_line(r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"Done."}]}}"#);
    assert_eq!(again[0].text, "Hello\n\nDone.");
    // Forwarded subagent prose never reaches the main thread's text.
    let nested = decoder.decode_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"inner"}},"parent_tool_use_id":"toolu_agent"}"#);
    assert!(nested.is_empty());
}

#[test]
fn claude_subagents_nest_under_their_tool_use_and_report_progress() {
    let mut decoder = Decoder::new(Provider::Claude, None, "default");
    let spawn = decoder.decode_line(r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_agent","name":"Agent","input":{"description":"Find the scroll bug","prompt":"Search widgets for scroll handling","subagent_type":"Explore"}}]},"parent_tool_use_id":null}"#);
    let agent = spawn[0].item.as_ref().unwrap();
    assert_eq!((agent.kind.as_str(), agent.name.as_str(), agent.title.as_str()), ("agent", "Agent", "Find the scroll bug"));
    let started = decoder.decode_line(r#"{"type":"system","subtype":"task_started","task_id":"task_9","tool_use_id":"toolu_agent","description":"Find the scroll bug","subagent_type":"Explore","task_type":"subagent"}"#);
    let item = started[0].item.as_ref().unwrap();
    assert_eq!((item.key.as_str(), item.kind.as_str(), item.status.as_str()), ("toolu_agent", "agent", "running"));
    assert_eq!(item.meta.as_ref().unwrap()["subagent_type"], "Explore");
    let nested = decoder.decode_line(r#"{"type":"assistant","message":{"id":"msg_2","content":[{"type":"tool_use","id":"toolu_inner","name":"Read","input":{"file_path":"app/src/widgets.rs"}}]},"parent_tool_use_id":"toolu_agent"}"#);
    let inner = nested[0].item.as_ref().unwrap();
    assert_eq!((inner.key.as_str(), inner.parent.as_str(), inner.title.as_str()), ("toolu_inner", "toolu_agent", "app/src/widgets.rs"));
    assert!(nested.iter().all(|e| e.kind != "assistant"));
    let progress = decoder.decode_line(r#"{"type":"system","subtype":"task_progress","task_id":"task_9","tool_use_id":"toolu_agent","description":"Find the scroll bug","usage":{"total_tokens":12000,"tool_uses":3,"duration_ms":4000},"last_tool_name":"Read"}"#);
    let meta = progress[0].item.as_ref().unwrap().meta.clone().unwrap();
    assert_eq!(meta["progress"]["tool_uses"], 3);
    assert_eq!(meta["progress"]["last_tool_name"], "Read");
    let result = decoder.decode_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_agent","content":"The bug is in widgets.rs"}]},"parent_tool_use_id":null}"#);
    let item = result[0].item.as_ref().unwrap();
    assert_eq!((item.kind.as_str(), item.status.as_str()), ("agent", "done"));
    assert_eq!(item.body.as_deref(), Some("The bug is in widgets.rs"));
    let notice = decoder.decode_line(r#"{"type":"system","subtype":"task_notification","task_id":"task_9","tool_use_id":"toolu_agent","status":"completed","output_file":"","summary":"Found it","usage":{"total_tokens":15000,"tool_uses":4,"duration_ms":5000}}"#);
    let item = notice[0].item.as_ref().unwrap();
    assert_eq!((item.key.as_str(), item.status.as_str()), ("toolu_agent", "done"));
}

#[test]
fn claude_background_commands_stay_open_until_their_notification() {
    let mut decoder = Decoder::new(Provider::Claude, None, "default");
    decoder.decode_line(r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"cargo build","run_in_background":true}}]},"parent_tool_use_id":null}"#);
    let result = decoder.decode_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_bg","content":"Command running in background with ID: b1"}]},"parent_tool_use_id":null}"#);
    assert_eq!(result[0].item.as_ref().unwrap().status, "background");
    let started = decoder.decode_line(r#"{"type":"system","subtype":"task_started","task_id":"b1","tool_use_id":"toolu_bg","description":"cargo build","task_type":"local_bash"}"#);
    assert_eq!(started[0].item.as_ref().unwrap().status, "background");
    let failed = decoder.decode_line(r#"{"type":"system","subtype":"task_notification","task_id":"b1","status":"failed","output_file":"/tmp/b1.out","summary":"exit 101"}"#);
    let item = failed[0].item.as_ref().unwrap();
    assert_eq!((item.key.as_str(), item.status.as_str()), ("toolu_bg", "failed"));
    assert_eq!(item.body.as_deref(), Some("exit 101"));
    // A task nobody's tool use started still has a card of its own.
    let orphan = decoder.decode_line(r#"{"type":"system","subtype":"task_started","task_id":"w1","description":"remote workflow","task_type":"workflow"}"#);
    let item = orphan[0].item.as_ref().unwrap();
    assert_eq!((item.key.as_str(), item.kind.as_str(), item.name.as_str()), ("task:w1", "task", "workflow"));
}

#[test]
fn todo_lists_from_both_providers_share_one_shape() {
    let mut claude = Decoder::new(Provider::Claude, None, "default");
    let events = claude.decode_line(r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_todo","name":"TodoWrite","input":{"todos":[{"content":"Read the code","status":"completed","activeForm":"Reading the code"},{"content":"Fix the scroll","status":"in_progress","activeForm":"Fixing the scroll"},{"content":"Run tests","status":"pending","activeForm":"Running tests"}]}}]},"parent_tool_use_id":null}"#);
    let todo = events[0].item.as_ref().unwrap();
    assert_eq!((todo.kind.as_str(), todo.title.as_str()), ("todo", "1 of 3 done"));
    assert_eq!(todo.body.as_deref(), Some("[x] Read the code\n[>] Fixing the scroll\n[ ] Run tests"));
    let done = claude.decode_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_todo","content":"Todos have been modified successfully. Ensure that you continue to use the todo list to track your progress."}]},"parent_tool_use_id":null}"#);
    let item = done[0].item.as_ref().unwrap();
    assert_eq!((item.kind.as_str(), item.status.as_str()), ("todo", "done"));
    assert!(item.body.is_none(), "the acknowledgement must not replace the list");
    claude.decode_line(r#"{"type":"assistant","message":{"id":"msg_2","content":[{"type":"tool_use","id":"toolu_todo2","name":"TodoWrite","input":{"todos":[{"content":"","status":"pending"}]}}]},"parent_tool_use_id":null}"#);
    let failed = claude.decode_line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_todo2","is_error":true,"content":"InputValidationError: todos.0.content must not be empty"}]},"parent_tool_use_id":null}"#);
    let item = failed[0].item.as_ref().unwrap();
    assert_eq!((item.kind.as_str(), item.status.as_str()), ("todo", "failed"));
    assert_eq!(item.body.as_deref(), Some("InputValidationError: todos.0.content must not be empty"));
    let mut codex = Decoder::new(Provider::Codex, None, "default");
    let events = codex.decode_line(r#"{"type":"item.updated","item":{"id":"todo-1","type":"todo_list","items":[{"text":"Read the code","completed":true},{"text":"Fix the scroll","completed":false}]}}"#);
    let todo = events[0].item.as_ref().unwrap();
    assert_eq!((todo.kind.as_str(), todo.title.as_str()), ("todo", "1 of 2 done"));
    assert_eq!(todo.input.as_ref().unwrap()["todos"][1]["status"], "pending");
    assert_eq!(todo.body.as_deref(), Some("[x] Read the code\n[ ] Fix the scroll"));
}

#[test]
fn codex_collab_reasoning_and_mcp_items_carry_their_state() {
    let mut decoder = Decoder::new(Provider::Codex, None, "default");
    let collab = decoder.decode_line(r#"{"type":"item.started","item":{"id":"collab-1","type":"collab_tool_call","tool":"spawn_agent","sender_thread_id":"t0","receiver_thread_ids":["t1"],"prompt":"Review the diff for races","agents_states":{"t1":{"status":"running","message":null}},"status":"in_progress"}}"#);
    let item = collab[0].item.as_ref().unwrap();
    assert_eq!((item.kind.as_str(), item.name.as_str(), item.title.as_str(), item.status.as_str()), ("agent", "spawn agent", "Review the diff for races", "running"));
    assert_eq!(item.body.as_deref(), Some("t1: running"));
    let reasoning = decoder.decode_line(r#"{"type":"item.completed","item":{"id":"r-1","type":"reasoning","text":"Considering the options"}}"#);
    let item = reasoning[0].item.as_ref().unwrap();
    assert_eq!((item.kind.as_str(), item.status.as_str()), ("reasoning", "done"));
    let mcp = decoder.decode_line(r#"{"type":"item.completed","item":{"id":"mcp-1","type":"mcp_tool_call","server":"superapp","tool":"workshop.workspaces.list","arguments":{"archived":false},"result":{"content":[{"type":"text","text":"[]"}]},"status":"completed"}}"#);
    let item = mcp[0].item.as_ref().unwrap();
    assert_eq!((item.name.as_str(), item.title.as_str(), item.body.as_deref()), ("superapp.workshop.workspaces.list", "false", Some("[]")));
    let denied = decoder.decode_line(r#"{"type":"item.completed","item":{"id":"cmd-2","type":"command_execution","command":"rm -rf /","aggregated_output":"sandbox denied","exit_code":1,"status":"failed"}}"#);
    assert_eq!(denied[0].item.as_ref().unwrap().status, "denied");
    assert!(denied.iter().any(|e| e.kind == "permission_denied"));
    let files = decoder.decode_line(r#"{"type":"item.completed","item":{"id":"fc-1","type":"file_change","changes":[{"path":"src/a.rs","kind":"update"},{"path":"src/b.rs","kind":"add"}],"status":"completed"}}"#);
    let item = files[0].item.as_ref().unwrap();
    assert_eq!((item.name.as_str(), item.title.as_str(), item.status.as_str()), ("edit", "src/a.rs, src/b.rs", "done"));
}
