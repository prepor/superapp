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
    assert_eq!(tool[0].kind, "tool");
    assert_eq!(tool[0].text, "cargo test");
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
    assert_eq!(denied[0].kind, "permission_denied");
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
