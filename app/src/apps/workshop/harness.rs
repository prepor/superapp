//! Desktop CLI harnesses. Authentication and resumable transcripts belong to the
//! installed provider CLI; Workshop stores session IDs and normalized events.
//!
//! Protocol references: https://learn.chatgpt.com/docs/non-interactive-mode and
//! https://code.claude.com/docs/en/headless. No prompt is passed through a shell.

use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, Notify},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Codex,
    Claude,
}

impl Provider {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "claude" | "claude-code" | "claude code" => Ok(Self::Claude),
            _ => Err(format!("Unsupported Workshop provider: {value}")),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

/// Explicit UI choices. Neither mode bypasses provider permission rules.
/// WorkspaceWrite permits commands in the workspace sandbox, Git metadata
/// writes and GitHub networking. Further permission denials appear in the chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    ReadOnly,
    WorkspaceWrite,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunRequest {
    pub provider: Provider,
    /// Empty or "default" uses the installed CLI's configured model.
    pub model: String,
    pub cwd: PathBuf,
    pub prompt: String,
    pub resume_session_id: Option<String>,
    pub permission_mode: PermissionMode,
    /// A run-scoped local app bridge; its bearer token is never an argv value.
    pub mcp: Option<McpConfig>,
}

#[derive(Clone)]
pub struct McpConfig {
    pub url: String,
    pub bearer_token: String,
}
impl std::fmt::Debug for McpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConfig")
            .field("url", &self.url)
            .field("bearer_token", &"[redacted]")
            .finish()
    }
}
const MCP_TOKEN_ENV: &str = "SUPERAPP_WORKSHOP_MCP_TOKEN";

#[derive(Clone, Debug)]
pub struct HarnessEvent {
    /// session, assistant_delta, assistant, item, usage, permission_denied,
    /// status, stderr, or error. `assistant` is the full completed text of one
    /// message, not a delta; data.id identifies the message on both. `item`
    /// carries a structured transcript line in `item`.
    pub kind: String,
    pub text: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub data: Value,
    pub item: Option<Item>,
}

#[derive(Clone, Debug, Default)]
pub struct RunOutcome {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub usage: Value,
    pub cancelled: bool,
}

#[derive(Clone, Default)]
pub struct CancelToken(Arc<Cancellation>);
#[derive(Default)]
struct Cancellation {
    cancelled: AtomicBool,
    wake: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.wake.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    pub(super) async fn cancelled(&self) {
        let notified = self.0.wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderInfo {
    pub executable: Option<PathBuf>,
    /// "default" first. Codex entries come from its public local model cache,
    /// Claude entries are CLI aliases; accepting a custom model ID is valid.
    pub models: Vec<String>,
}

pub fn provider_info(provider: Provider) -> ProviderInfo {
    let executable = find_executable(provider);
    let mut models = vec!["default".to_owned()];
    match provider {
        Provider::Claude => models.extend(["fable", "opus", "sonnet", "haiku"].map(String::from)),
        Provider::Codex => {
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")));
            if let Some(path) = home.map(|h| h.join("models_cache.json")) {
                // This file contains model metadata, never authentication data.
                if std::fs::metadata(&path).is_ok_and(|m| m.len() <= 4 * 1024 * 1024) {
                    if let Ok(bytes) = std::fs::read(path) {
                        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                            models.extend(codex_model_ids(&value));
                        }
                    }
                }
            }
        }
    }
    ProviderInfo { executable, models }
}

/// A shell-quoted login recipe; this function performs no discovery or login.
pub fn login_command(provider: Provider, executable: &Path) -> String {
    format!(
        "{} {}",
        shell_quote(&executable.to_string_lossy()),
        match provider {
            Provider::Codex => "login --device-auth",
            Provider::Claude => "auth login --claudeai",
        }
    )
}

fn codex_model_ids(value: &Value) -> Vec<String> {
    let mut models = Vec::new();
    for model in value["models"].as_array().into_iter().flatten() {
        if model["visibility"].as_str().is_some_and(|v| v != "list") {
            continue;
        }
        if let Some(slug) = model["slug"].as_str().filter(|s| !s.is_empty()) {
            if !models.iter().any(|m| m == slug) {
                models.push(slug.to_owned());
            }
        }
    }
    models
}

/// Read-only authentication probe. Return only the method/status, never CLI
/// output that may include account identifiers or partial credentials.
pub async fn authentication_status(provider: Provider) -> Result<String, String> {
    let path = find_executable(provider).ok_or_else(|| missing_cli(provider))?;
    // exec/print may prefer an inherited billing credential to saved OAuth.
    // Inspect presence only; never load or display its value.
    let api_environment = match provider {
        Provider::Codex => ["CODEX_API_KEY", "OPENAI_API_KEY"].as_slice(),
        Provider::Claude => ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"].as_slice(),
    };
    if api_environment
        .iter()
        .any(|key| std::env::var_os(key).is_some())
    {
        return Ok("API credentials configured in environment".into());
    }
    let mut cmd = Command::new(path);
    match provider {
        Provider::Codex => {
            cmd.args(["login", "status"]);
        }
        Provider::Claude => {
            cmd.args(["auth", "status", "--json"]);
        }
    }
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .map_err(|_| "Authentication status timed out".to_owned())?
        .map_err(|e| format!("Cannot check authentication: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(auth_summary(
        provider,
        output.status.success(),
        &stdout,
        &stderr,
    ))
}

fn auth_summary(provider: Provider, success: bool, stdout: &str, stderr: &str) -> String {
    if provider == Provider::Claude {
        if let Ok(value) = serde_json::from_str::<Value>(stdout) {
            if value["loggedIn"] == false {
                return "Not signed in".into();
            }
            if value["loggedIn"] == true {
                return match value["authMethod"].as_str().unwrap_or_default() {
                    "claude.ai" | "oauth" => "Signed in with Claude subscription".into(),
                    "api_key" | "apiKey" | "api-key" => "Signed in with API billing".into(),
                    _ => "Signed in through Claude Code".into(),
                };
            }
        }
    }
    let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    if !success {
        "Not signed in; run the login command".into()
    } else if text.contains("chatgpt") {
        "Signed in with ChatGPT subscription".into()
    } else if text.contains("api key") || text.contains("api_key") {
        "Signed in with API billing".into()
    } else {
        "Signed in through the provider CLI".into()
    }
}

pub fn find_executable(provider: Provider) -> Option<PathBuf> {
    executable_dirs()
        .into_iter()
        .map(|dir| dir.join(provider.as_str()))
        .find(|p| executable_file(p))
}

fn executable_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    // Finder-launched apps often have a minimal PATH. These are ordinary CLI
    // installation locations, not Conductor's private runner or credentials.
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs
}

fn executable_file(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn missing_cli(provider: Provider) -> String {
    format!(
        "{} CLI is not installed. Install it and sign in with your subscription, then retry.",
        provider.as_str()
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_args(request: &RunRequest, git_dirs: &[PathBuf]) -> Vec<String> {
    let mut args: Vec<String> = match request.provider {
        Provider::Codex => vec![
            "exec".into(),
            "--json".into(),
            "--color".into(),
            "never".into(),
            "--config".into(),
            format!("sandbox_mode=\"{}\"", request.permission_mode.as_str()),
            // Non-interactive exec cannot answer permission prompts. Never
            // grant escalations: denied actions remain visible tool failures.
            "--config".into(),
            "approval_policy=\"never\"".into(),
        ],
        Provider::Claude => vec![
            "--print".into(),
            "--verbose".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--include-partial-messages".into(),
            "--permission-mode".into(),
            match request.permission_mode {
                PermissionMode::ReadOnly => "plan",
                PermissionMode::WorkspaceWrite => "acceptEdits",
            }
            .into(),
        ],
    };
    if request.permission_mode == PermissionMode::WorkspaceWrite {
        match request.provider {
            Provider::Codex => {
                args.extend([
                    "--config".into(),
                    "sandbox_workspace_write.network_access=true".into(),
                ]);
                if !git_dirs.is_empty() {
                    args.extend([
                        "--config".into(),
                        format!("sandbox_workspace_write.writable_roots={}", json!(git_dirs)),
                    ]);
                }
            }
            Provider::Claude => {
                args.extend(["--settings".into(), json!({"sandbox": {
                    "enabled":true, "failIfUnavailable":true,
                    "autoAllowBashIfSandboxed":true, "allowUnsandboxedCommands":false,
                    "filesystem":{"allowWrite":git_dirs},
                    "network":{"allowedDomains":["github.com","*.github.com","*.githubusercontent.com"]}
                }}).to_string()]);
            }
        }
    }
    if let Some(mcp) = &request.mcp {
        match request.provider {
            Provider::Codex => args.extend([
                "--config".into(),
                format!("mcp_servers.superapp.url={}", json!(mcp.url)),
                "--config".into(),
                format!("mcp_servers.superapp.bearer_token_env_var=\"{MCP_TOKEN_ENV}\""),
                "--config".into(),
                "mcp_servers.superapp.required=true".into(),
                "--config".into(),
                "mcp_servers.superapp.tool_timeout_sec=3600".into(),
            ]),
            Provider::Claude => args.extend([
                "--mcp-config".into(),
                json!({"mcpServers":{"superapp":{
                    "type":"http","url":mcp.url,
                    "headers":{"Authorization":format!("Bearer ${{{MCP_TOKEN_ENV}}}")}
                }}})
                .to_string(),
                // App write approvals belong to the bridge's in-chat UI. This
                // grant names only that server, not shell or other MCP tools.
                "--allowedTools".into(),
                "mcp__superapp__*".into(),
            ]),
        }
    }
    if request.provider == Provider::Codex {
        if let Some(id) = &request.resume_session_id {
            args.extend(["resume".into(), id.clone()]);
        }
    } else if let Some(id) = &request.resume_session_id {
        args.extend(["--resume".into(), id.clone()]);
    }
    if !request.model.is_empty() && request.model != "default" {
        args.extend(["--model".into(), request.model.clone()]);
    }
    if request.provider == Provider::Codex {
        args.push("-".into());
    }
    args
}

async fn git_metadata_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for flag in ["--git-common-dir", "--git-dir"] {
        let mut command = Command::new("git");
        command
            .args(["rev-parse", "--path-format=absolute", flag])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(5), command.output()).await
        {
            if output.status.success() {
                let path = PathBuf::from(
                    String::from_utf8_lossy(&output.stdout).trim_end_matches(['\r', '\n']),
                );
                if path.is_absolute() && path.is_dir() && !dirs.contains(&path) {
                    dirs.push(path);
                }
            }
        }
    }
    dirs
}

fn prompt_input(request: &RunRequest) -> Vec<u8> {
    match request.provider {
        Provider::Codex => request.prompt.as_bytes().to_vec(),
        Provider::Claude => format!("{}\n", json!({
            "type": "user", "message": {"role":"user", "content":request.prompt},
            "parent_tool_use_id":null, "session_id":request.resume_session_id.as_deref().unwrap_or("")
        })).into_bytes(),
    }
}

/// One question, one answer: a turn whose prose comes back as a string rather
/// than streaming into a transcript. Names a workspace's branch after its
/// first message, the way Conductor asks the model in the background.
pub async fn ask(request: RunRequest, cancel: CancelToken) -> Result<String, String> {
    let answers: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let sink = answers.clone();
    run(request, cancel, move |event| {
        if event.kind == "assistant" {
            let key = event.data["id"].as_str().unwrap_or("assistant").to_owned();
            let mut answers = sink.lock().unwrap();
            if let Some(slot) = answers.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = event.text;
            } else {
                answers.push((key, event.text));
            }
        }
    })
    .await?;
    let answers = answers.lock().unwrap();
    Ok(answers
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Run one turn on a Tokio worker. The callback runs synchronously on that
/// worker and should forward events to the application's existing event queue.
/// Stop kills the dedicated process group, including commands the harness ran.
pub async fn run(
    request: RunRequest,
    cancel: CancelToken,
    emit: impl FnMut(HarnessEvent),
) -> Result<RunOutcome, String> {
    let executable =
        find_executable(request.provider).ok_or_else(|| missing_cli(request.provider))?;
    run_with_executable(request, executable, cancel, emit).await
}

pub(super) async fn run_with_executable(
    request: RunRequest,
    executable: PathBuf,
    cancel: CancelToken,
    mut emit: impl FnMut(HarnessEvent),
) -> Result<RunOutcome, String> {
    if request.prompt.trim().is_empty() {
        return Err("A message is required".into());
    }
    if !request.cwd.is_dir() {
        return Err("The workspace directory is unavailable".into());
    }
    if request
        .resume_session_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.starts_with('-'))
    {
        return Err("Invalid provider session ID".into());
    }
    if cancel.is_cancelled() {
        return Ok(RunOutcome {
            cancelled: true,
            ..Default::default()
        });
    }
    let git_dirs = if request.permission_mode == PermissionMode::WorkspaceWrite {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(RunOutcome { cancelled:true, ..Default::default() }),
            dirs = git_metadata_dirs(&request.cwd) => dirs,
        }
    } else {
        Vec::new()
    };
    let mut command = Command::new(executable);
    command
        .args(command_args(&request, &git_dirs))
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(mcp) = &request.mcp {
        command.env(MCP_TOKEN_ENV, &mcp.bearer_token);
        if request.provider == Provider::Claude {
            command.env("MCP_TOOL_TIMEOUT", "3600000");
        }
    } else {
        command.env_remove(MCP_TOKEN_ENV);
    }
    // The model's tools must find the same ordinary installs as the launcher,
    // including gh and language tools in Finder's minimal process environment.
    if let Ok(path) = std::env::join_paths(executable_dirs()) {
        command.env("PATH", path);
    }
    // A Workshop chat is independent of whichever CLI launched Superapp. The
    // provider still reads its own saved subscription login and configuration.
    command
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CODEX_INTERNAL_ORIGINATOR_OVERRIDE")
        .env("NO_COLOR", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start {}: {e}", request.provider.as_str()))?;
    let group = ProcessGroup(child.id());
    let stdout = child
        .stdout
        .take()
        .ok_or("Provider stdout is unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Provider stderr is unavailable")?;
    let mut stdin = child.stdin.take().ok_or("Provider stdin is unavailable")?;
    let input = prompt_input(&request);
    // Drain both outputs while delivering input; a provider may emit startup
    // diagnostics before reading a large prompt.
    let (tx, mut rx) = mpsc::channel(64);
    let out_task = tokio::spawn(read_lines(stdout, false, tx.clone()));
    let err_task = tokio::spawn(read_lines(stderr, true, tx));
    let input_task = tokio::spawn(async move {
        stdin.write_all(&input).await?;
        stdin.shutdown().await
    });
    let mut decoder = Decoder::new(
        request.provider,
        request.resume_session_id.clone(),
        &request.model,
    );
    emit(decoder.event(
        "status",
        "Starting agent",
        json!({
            "provider":request.provider.as_str(), "model":request.model,
            "cwd":request.cwd.to_string_lossy(), "permission_mode":request.permission_mode.as_str()
        }),
    ));
    let mut stderr_tail = String::new();
    let mut io_error = None;
    let mut cancelled = false;
    let mut exit_status = None;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => { cancelled = true; group.kill(); let _ = child.start_kill(); break; }
            status = child.wait(), if exit_status.is_none() => {
                exit_status = Some(status);
                // A finished harness must not leave a descendant holding its
                // output pipes open (or continuing to change the workspace).
                group.kill();
            }
            line = rx.recv() => match line {
                Some(StreamLine::Text { stderr, text }) => {
                    if stderr {
                        append_tail(&mut stderr_tail, &text, 8192);
                        emit(decoder.event("stderr", &text, Value::Null));
                    } else {
                        for event in decoder.decode_line(&text) { emit(event); }
                    }
                }
                Some(StreamLine::Error(error)) => { io_error = Some(error); group.kill(); let _ = child.start_kill(); break; }
                None => break,
            }
        }
    }
    let status = if let Some(status) = exit_status {
        status
    } else {
        tokio::select! {
            _ = cancel.cancelled(), if !cancelled => {
                cancelled = true; group.kill(); let _ = child.start_kill(); child.wait().await
            }
            status = child.wait() => status,
        }
    }
    .map_err(|e| format!("Cannot wait for provider: {e}"))?;
    // Never leave read tasks blocked on inherited pipes after cancellation.
    out_task.abort();
    err_task.abort();
    if cancelled {
        input_task.abort();
        emit(decoder.event("status", "Stopped", Value::Null));
        return Ok(RunOutcome {
            cancelled: true,
            ..decoder.outcome
        });
    }
    let input_error = input_task.await.ok().and_then(Result::err);
    if let Some(error) = io_error {
        return Err(error);
    }
    if !status.success() {
        let message = decoder.failure.unwrap_or_else(|| {
            if stderr_tail.trim().is_empty() {
                format!("{} exited with {status}", request.provider.as_str())
            } else {
                format!(
                    "{} exited with {status}: {}",
                    request.provider.as_str(),
                    stderr_tail.trim()
                )
            }
        });
        return Err(message);
    }
    if let Some(error) = decoder.failure {
        return Err(error);
    }
    if let Some(error) = input_error {
        return Err(format!("Cannot send message to provider: {error}"));
    }
    if !decoder.completed {
        return Err("Provider exited without completing the turn".into());
    }
    Ok(decoder.outcome)
}

struct ProcessGroup(Option<u32>);
impl ProcessGroup {
    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // Each child is placed in its own group before exec. Negative PID
            // targets that group, never Superapp's inherited process group.
            if pid > 1 && pid <= i32::MAX as u32 {
                unsafe {
                    kill(-(pid as i32), 9);
                }
            }
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

enum StreamLine {
    Text { stderr: bool, text: String },
    Error(String),
}
async fn read_lines(
    mut reader: impl AsyncRead + Unpin,
    stderr: bool,
    tx: mpsc::Sender<StreamLine>,
) {
    const MAX_LINE: usize = 8 * 1024 * 1024;
    let mut pending = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let size = match reader.read(&mut buffer).await {
            Ok(size) => size,
            Err(error) => {
                let _ = tx
                    .send(StreamLine::Error(format!(
                        "Cannot read provider output: {error}"
                    )))
                    .await;
                return;
            }
        };
        if size == 0 {
            if !pending.is_empty() {
                let _ = tx
                    .send(StreamLine::Text {
                        stderr,
                        text: String::from_utf8_lossy(&pending).into_owned(),
                    })
                    .await;
            }
            return;
        }
        for part in buffer[..size].split_inclusive(|b| *b == b'\n') {
            if pending.len() + part.len() > MAX_LINE {
                let _ = tx
                    .send(StreamLine::Error(
                        "Provider output line exceeds 8 MiB".into(),
                    ))
                    .await;
                return;
            }
            pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                let text = String::from_utf8_lossy(&pending)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                if tx.send(StreamLine::Text { stderr, text }).await.is_err() {
                    return;
                }
                pending.clear();
            }
        }
    }
}

fn append_tail(tail: &mut String, text: &str, limit: usize) {
    tail.push_str(text);
    tail.push('\n');
    if tail.len() > limit {
        let mut start = tail.len() - limit;
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        tail.drain(..start);
    }
}

/// One line of a run's structured transcript, as the harness reports it and
/// Workshop stores it. A later event for the same `key` updates the item in
/// place; `None` fields leave what an earlier event set.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Item {
    pub key: String,
    /// The tool use that spawned the subagent this item belongs to; empty on
    /// the main thread.
    pub parent: String,
    /// text, reasoning, tool, todo, agent, task, denied or error.
    pub kind: String,
    pub name: String,
    pub title: String,
    pub input: Option<Value>,
    pub body: Option<String>,
    /// Merged into the stored meta: task ids, progress, exit codes.
    pub meta: Option<Value>,
    /// running, background, done, failed, denied or stopped; empty keeps the
    /// stored status.
    pub status: String,
}

impl Item {
    fn new(key: &str, kind: &str) -> Self {
        Self {
            key: key.to_owned(),
            kind: kind.to_owned(),
            ..Default::default()
        }
    }
}

/// What a Claude tool use is known to be, from its start to its result.
#[derive(Clone, Debug)]
struct ToolUse {
    name: String,
    parent: String,
    background: bool,
}

struct Decoder {
    provider: Provider,
    outcome: RunOutcome,
    completed: bool,
    failure: Option<String>,
    streamed: HashMap<String, String>,
    assistant_seen: bool,
    /// Claude: the main thread's current message id, its finished text blocks
    /// per message, the block streaming now, and the block types by index.
    message: String,
    texts: HashMap<String, Vec<String>>,
    live: String,
    blocks: HashMap<u64, String>,
    tools: HashMap<String, ToolUse>,
    /// Background task ids by the tool use that started them.
    tasks: HashMap<String, String>,
    seq: u64,
}

impl Decoder {
    fn new(provider: Provider, session_id: Option<String>, model: &str) -> Self {
        Self {
            provider,
            outcome: RunOutcome {
                session_id,
                model: (!model.is_empty() && model != "default").then(|| model.to_owned()),
                ..Default::default()
            },
            completed: false,
            failure: None,
            streamed: HashMap::new(),
            assistant_seen: false,
            message: "assistant".into(),
            texts: HashMap::new(),
            live: String::new(),
            blocks: HashMap::new(),
            tools: HashMap::new(),
            tasks: HashMap::new(),
            seq: 0,
        }
    }
    fn event(&self, kind: &str, text: &str, data: Value) -> HarnessEvent {
        HarnessEvent {
            kind: kind.into(),
            text: text.into(),
            session_id: self.outcome.session_id.clone(),
            model: self.outcome.model.clone(),
            data,
            item: None,
        }
    }
    fn item(&self, item: Item) -> HarnessEvent {
        let mut event = self.event("item", &item.title, Value::Null);
        event.item = Some(item);
        event
    }
    fn next_key(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{}", self.seq)
    }
    fn decode_line(&mut self, line: &str) -> Vec<HarnessEvent> {
        if line.trim().is_empty() {
            return vec![];
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return vec![self.event("stderr", line, Value::Null)],
        };
        match self.provider {
            Provider::Codex => self.codex(value),
            Provider::Claude => self.claude(value),
        }
    }
    fn fail(&mut self, message: &str, value: Value) -> Vec<HarnessEvent> {
        self.failure = Some(message.to_owned());
        vec![self.event("error", message, value)]
    }
    fn codex(&mut self, value: Value) -> Vec<HarnessEvent> {
        match value["type"].as_str().unwrap_or_default() {
            "thread.started" => {
                self.outcome.session_id = value["thread_id"].as_str().map(String::from);
                if let Some(model) = value["model"].as_str() {
                    self.outcome.model = Some(model.to_owned());
                }
                vec![self.event("session", "", value)]
            }
            "turn.started" => vec![self.event("status", "Running", value)],
            "turn.completed" => {
                self.completed = true;
                self.outcome.usage = value["usage"].clone();
                vec![self.event("usage", "", value["usage"].clone())]
            }
            "turn.failed" => {
                let message = value["error"]["message"]
                    .as_str()
                    .unwrap_or("Codex turn failed")
                    .to_owned();
                self.fail(&message, value)
            }
            "error" => {
                // Codex may emit transient error events before retrying. Only
                // turn.failed or the process status determines final failure.
                let text = value["message"].as_str().unwrap_or("Codex error");
                vec![self.event("error", text, value.clone())]
            }
            kind if kind.starts_with("item.") => {
                let completed = kind == "item.completed";
                let item = value["item"].clone();
                self.codex_item(completed, &item, value)
            }
            _ => vec![self.event("status", "", value)],
        }
    }
    fn codex_item(&mut self, completed: bool, item: &Value, value: Value) -> Vec<HarnessEvent> {
        let item_type = item["type"].as_str().unwrap_or_default();
        let id = item["id"].as_str().unwrap_or("assistant");
        if item_type == "agent_message" {
            let text = item["text"].as_str().unwrap_or_default();
            let prior = self.streamed.get(id).map(String::as_str).unwrap_or("");
            let mut events = Vec::new();
            if !completed {
                if let Some(delta) = text.strip_prefix(prior).filter(|s| !s.is_empty()) {
                    events.push(self.event("assistant_delta", delta, json!({"id":id})));
                }
            } else {
                events.push(self.event("assistant", text, json!({"id":id,"item":item})));
                self.assistant_seen = true;
            }
            self.streamed.insert(id.to_owned(), text.to_owned());
            return events;
        }
        let status = |s: &str| match s {
            "completed" => "done",
            "failed" => "failed",
            "declined" => "denied",
            _ => "running",
        };
        let item_status = item["status"].as_str().unwrap_or_default();
        let mut out = Item::new(id, "tool");
        let mut denied = None;
        match item_type {
            "reasoning" => {
                out.kind = "reasoning".into();
                out.body = Some(item["text"].as_str().unwrap_or_default().to_owned());
                out.status = if completed { "done" } else { "running" }.into();
            }
            "command_execution" => {
                let command = item["command"].as_str().unwrap_or("command");
                out.name = "shell".into();
                out.title = first_line(command, 160);
                out.input = Some(json!({"command":command}));
                let output = item["aggregated_output"].as_str().unwrap_or_default();
                out.body = Some(output.to_owned());
                let exit = item["exit_code"].as_i64();
                out.meta = Some(json!({"exit_code":exit}));
                out.status = match item_status {
                    "completed" if exit.is_some_and(|code| code != 0) => "failed",
                    other => status(other),
                }
                .into();
                if permission_denial(output) {
                    out.status = "denied".into();
                    denied = Some(output.to_owned());
                }
            }
            "file_change" => {
                let changes = item["changes"].as_array().cloned().unwrap_or_default();
                let paths: Vec<String> = changes
                    .iter()
                    .filter_map(|c| c["path"].as_str().map(str::to_owned))
                    .collect();
                out.name = "edit".into();
                out.title = clip(&paths.join(", "), 160);
                out.input = Some(json!({"changes":changes}));
                out.status = status(item_status).into();
            }
            "mcp_tool_call" => {
                let server = item["server"].as_str().unwrap_or("mcp");
                let tool = item["tool"].as_str().unwrap_or("tool");
                out.name = format!("{server}.{tool}");
                out.title = summarize(&item["arguments"]);
                out.input = Some(item["arguments"].clone());
                let body = if let Some(message) = item["error"]["message"].as_str() {
                    message.to_owned()
                } else {
                    mcp_result_text(&item["result"])
                };
                out.body = Some(body);
                out.status = status(item_status).into();
            }
            "web_search" => {
                out.name = "web search".into();
                out.title = clip(item["query"].as_str().unwrap_or_default(), 160);
                out.input = Some(json!({"query":item["query"]}));
                out.status = "done".into();
            }
            "todo_list" => {
                let todos: Vec<Value> = item["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| {
                        json!({"content":t["text"],"status":if t["completed"]==true{"completed"}else{"pending"}})
                    })
                    .collect();
                out = todo_item(id, &todos);
            }
            "collab_tool_call" => {
                let tool = item["tool"].as_str().unwrap_or("agent");
                out.kind = "agent".into();
                out.name = tool.replace('_', " ");
                let receivers = item["receiver_thread_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                out.title = match item["prompt"].as_str() {
                    Some(prompt) if !prompt.is_empty() => first_line(prompt, 160),
                    _ => receivers.clone(),
                };
                out.input = Some(json!({"prompt":item["prompt"],"receivers":item["receiver_thread_ids"]}));
                let mut states = Vec::new();
                if let Some(map) = item["agents_states"].as_object() {
                    for (agent, state) in map {
                        let mut line = format!(
                            "{agent}: {}",
                            state["status"].as_str().unwrap_or("unknown")
                        );
                        if let Some(message) = state["message"].as_str().filter(|m| !m.is_empty()) {
                            line.push_str(" · ");
                            line.push_str(&first_line(message, 200));
                        }
                        states.push(line);
                    }
                }
                out.body = Some(states.join("\n"));
                out.status = status(item_status).into();
            }
            "error" => {
                out.kind = "error".into();
                out.body = Some(item["message"].as_str().unwrap_or("Tool error").to_owned());
                out.status = "failed".into();
            }
            other => {
                out.name = other.replace('_', " ");
                out.status = if completed { "done" } else { "running" }.into();
            }
        }
        let mut events = vec![self.item(out)];
        if let Some(output) = denied {
            events.push(self.event(
                "permission_denied",
                &output,
                json!({"tool_use_id":id,"item":item}),
            ));
        }
        let _ = value;
        events
    }
    fn claude(&mut self, value: Value) -> Vec<HarnessEvent> {
        if let Some(session) = value["session_id"].as_str().filter(|s| !s.is_empty()) {
            self.outcome.session_id = Some(session.to_owned());
        }
        let parent = value["parent_tool_use_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        match value["type"].as_str().unwrap_or_default() {
            "system" => self.claude_system(value),
            "stream_event" => self.claude_stream(&parent, &value),
            "assistant" => self.claude_assistant(&parent, &value),
            "user" => self.claude_user(&parent, &value),
            "result" => {
                self.completed = true;
                self.outcome.usage = json!({"usage":value["usage"],"modelUsage":value["modelUsage"],"total_cost_usd":value["total_cost_usd"]});
                let mut events = Vec::new();
                for denied in value["permission_denials"].as_array().into_iter().flatten() {
                    events.push(self.event(
                        "permission_denied",
                        denied["tool_name"]
                            .as_str()
                            .unwrap_or("Tool permission denied"),
                        denied.clone(),
                    ));
                }
                if value["is_error"] == true {
                    let message = value["result"]
                        .as_str()
                        .map(String::from)
                        .or_else(|| {
                            value["errors"].as_array().map(|items| {
                                items
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                        })
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Claude turn failed".into());
                    events.extend(self.fail(&message, value.clone()));
                } else if !self.assistant_seen {
                    if let Some(text) = value["result"].as_str().filter(|s| !s.is_empty()) {
                        events.push(self.event("assistant", text, json!({"id":"result"})));
                    }
                }
                events.push(self.event("usage", "", self.outcome.usage.clone()));
                events
            }
            "error" => {
                let message = value["error"]["message"]
                    .as_str()
                    .or(value["message"].as_str())
                    .unwrap_or("Claude error")
                    .to_owned();
                self.fail(&message, value)
            }
            _ => vec![self.event("status", "", value)],
        }
    }
    fn claude_system(&mut self, value: Value) -> Vec<HarnessEvent> {
        match value["subtype"].as_str().unwrap_or_default() {
            "init" => {
                self.outcome.model = value["model"]
                    .as_str()
                    .map(String::from)
                    .or(self.outcome.model.take());
                vec![self.event("session", "", value)]
            }
            "permission_denied" => {
                let text = value["message"]
                    .as_str()
                    .or(value["reason"].as_str())
                    .unwrap_or("The provider denied a tool request")
                    .to_owned();
                let key = value["tool_use_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| self.next_key("denied"));
                let mut item = Item::new(&key, "denied");
                item.name = value["tool_name"].as_str().unwrap_or_default().to_owned();
                item.body = Some(text.clone());
                item.status = "denied".into();
                if let Some(known) = self.tools.get(&key) {
                    item.kind = "tool".into();
                    item.name = known.name.clone();
                    item.parent = known.parent.clone();
                }
                vec![
                    self.item(item),
                    self.event("permission_denied", &text, value),
                ]
            }
            "task_started" | "task_progress" | "task_notification" => self.claude_task(value),
            _ => vec![self.event("status", "", value)],
        }
    }
    /// Background work and subagents report by task id; a task started by a
    /// tool use updates that tool's own card rather than adding another line.
    fn claude_task(&mut self, value: Value) -> Vec<HarnessEvent> {
        let task_id = value["task_id"].as_str().unwrap_or_default().to_owned();
        let tool_use = value["tool_use_id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| self.tasks.get(&task_id).cloned());
        if let (Some(tool_use), false) = (&tool_use, task_id.is_empty()) {
            self.tasks.insert(task_id.clone(), tool_use.clone());
        }
        let key = tool_use.clone().unwrap_or_else(|| format!("task:{task_id}"));
        let known = self.tools.get(&key).cloned();
        let mut item = Item::new(&key, "task");
        if let Some(known) = &known {
            item.kind = if is_agent_tool(&known.name) { "agent" } else { "tool" }.into();
            item.name = known.name.clone();
            item.parent = known.parent.clone();
        }
        let description = value["description"].as_str().unwrap_or_default();
        match value["subtype"].as_str().unwrap_or_default() {
            "task_started" => {
                if known.is_none() {
                    item.name = value["task_type"]
                        .as_str()
                        .or(value["subagent_type"].as_str())
                        .unwrap_or("task")
                        .replace('_', " ");
                    item.title = clip(description, 160);
                }
                item.meta = Some(json!({"task_id":task_id,"task_type":value["task_type"],"subagent_type":value["subagent_type"],"description":description}));
                item.status = if known.as_ref().is_some_and(|k| k.background) {
                    "background"
                } else {
                    "running"
                }
                .into();
            }
            "task_progress" => {
                item.meta = Some(json!({"task_id":task_id,"progress":{
                    "tool_uses":value["usage"]["tool_uses"],"total_tokens":value["usage"]["total_tokens"],
                    "duration_ms":value["usage"]["duration_ms"],"last_tool_name":value["last_tool_name"],
                    "summary":value["summary"]}}));
            }
            _ => {
                item.status = match value["status"].as_str().unwrap_or_default() {
                    "failed" => "failed",
                    "stopped" => "stopped",
                    _ => "done",
                }
                .into();
                let summary = value["summary"].as_str().unwrap_or_default();
                item.meta = Some(json!({"task_id":task_id,"output_file":value["output_file"],"summary":summary}));
                if !summary.is_empty() {
                    item.body = Some(summary.to_owned());
                }
            }
        }
        vec![self.item(item)]
    }
    fn claude_stream(&mut self, parent: &str, value: &Value) -> Vec<HarnessEvent> {
        let event = &value["event"];
        let index = event["index"].as_u64().unwrap_or(0);
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                if let Some(model) = event["message"]["model"].as_str() {
                    self.outcome.model = Some(model.to_owned());
                }
                if parent.is_empty() {
                    if let Some(id) = event["message"]["id"].as_str() {
                        self.message = id.to_owned();
                    }
                    self.live.clear();
                    self.blocks.clear();
                }
                vec![]
            }
            "content_block_start" => {
                let block = &event["content_block"];
                let kind = block["type"].as_str().unwrap_or_default().to_owned();
                if parent.is_empty() {
                    self.blocks.insert(index, kind.clone());
                    if kind == "text" {
                        self.live.clear();
                    }
                }
                if kind == "tool_use" {
                    let id = block["id"].as_str().unwrap_or_default();
                    let name = block["name"].as_str().unwrap_or("Tool");
                    if id.is_empty() {
                        return vec![];
                    }
                    return vec![self.tool_start(id, name, parent, &Value::Null)];
                }
                vec![]
            }
            "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                if !parent.is_empty() {
                    return vec![];
                }
                let text = event["delta"]["text"].as_str().unwrap_or_default();
                self.live.push_str(text);
                vec![self.event("assistant_delta", text, json!({"id":self.message}))]
            }
            "content_block_stop" if parent.is_empty() => {
                if self.blocks.get(&index).map(String::as_str) == Some("text") && !self.live.is_empty()
                {
                    let live = std::mem::take(&mut self.live);
                    self.texts.entry(self.message.clone()).or_default().push(live);
                }
                vec![]
            }
            _ => vec![],
        }
    }
    fn tool_start(&mut self, id: &str, name: &str, parent: &str, input: &Value) -> HarnessEvent {
        let background = input["run_in_background"] == true;
        let known = self.tools.entry(id.to_owned()).or_insert_with(|| ToolUse {
            name: name.to_owned(),
            parent: parent.to_owned(),
            background: false,
        });
        known.background |= background;
        let kind = if name == "TodoWrite" {
            "todo"
        } else if is_agent_tool(name) {
            "agent"
        } else {
            "tool"
        };
        let mut item = Item::new(id, kind);
        item.name = name.to_owned();
        item.parent = parent.to_owned();
        item.status = "running".into();
        if !input.is_null() {
            if kind == "todo" {
                let todos = input["todos"].as_array().cloned().unwrap_or_default();
                item = todo_item(id, &todos);
                item.parent = parent.to_owned();
                item.status = "running".into();
            } else {
                item.title = tool_title(name, input);
                item.input = Some(input.clone());
                if background {
                    item.meta = Some(json!({"background":true}));
                }
            }
        }
        self.item(item)
    }
    fn claude_assistant(&mut self, parent: &str, value: &Value) -> Vec<HarnessEvent> {
        if let Some(model) = value["message"]["model"].as_str() {
            self.outcome.model = Some(model.to_owned());
        }
        let id = value["message"]["id"]
            .as_str()
            .unwrap_or("assistant")
            .to_owned();
        let mut events = Vec::new();
        let mut said = false;
        for block in value["message"]["content"].as_array().into_iter().flatten() {
            match block["type"].as_str().unwrap_or_default() {
                "text" => {
                    let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) else {
                        continue;
                    };
                    if parent.is_empty() {
                        let texts = self.texts.entry(id.clone()).or_default();
                        if self.live == text {
                            self.live.clear();
                        }
                        if !texts.iter().any(|t| t == text) {
                            texts.push(text.to_owned());
                        }
                        said = true;
                    } else {
                        // Forwarded subagent prose stays under its card.
                        let mut item = Item::new(&format!("{id}:text"), "text");
                        item.parent = parent.to_owned();
                        item.body = Some(text.to_owned());
                        item.status = "done".into();
                        events.push(self.item(item));
                    }
                }
                "thinking" => {
                    if let Some(text) = block["thinking"].as_str().filter(|t| !t.is_empty()) {
                        let mut item = Item::new(&format!("{id}:reasoning"), "reasoning");
                        item.parent = parent.to_owned();
                        item.body = Some(text.to_owned());
                        item.status = "done".into();
                        events.push(self.item(item));
                    }
                }
                "tool_use" => {
                    let tool_id = block["id"].as_str().unwrap_or_default();
                    let name = block["name"].as_str().unwrap_or("Tool");
                    if !tool_id.is_empty() {
                        events.push(self.tool_start(tool_id, name, parent, &block["input"]));
                    }
                }
                _ => {}
            }
        }
        if said {
            let text = self.texts.get(&id).map(|t| t.join("\n\n")).unwrap_or_default();
            events.push(self.event(
                "assistant",
                &text,
                json!({"id":id,"message":value["message"]}),
            ));
            self.assistant_seen = true;
        }
        events
    }
    fn claude_user(&mut self, parent: &str, value: &Value) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        for block in value["message"]["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let id = block["tool_use_id"].as_str().unwrap_or_default().to_owned();
            let text = tool_result_text(&block["content"]);
            let error = block["is_error"] == true;
            let denied = error && permission_denial(&text);
            let known = self.tools.get(&id).cloned();
            let key = if id.is_empty() {
                self.next_key("result")
            } else {
                id.clone()
            };
            let mut item = Item::new(&key, "tool");
            if let Some(known) = &known {
                item.name = known.name.clone();
                item.parent = known.parent.clone();
                item.kind = if known.name == "TodoWrite" {
                    "todo"
                } else if is_agent_tool(&known.name) {
                    "agent"
                } else {
                    "tool"
                }
                .into();
            } else {
                item.parent = parent.to_owned();
            }
            // A todo list's result only acknowledges the write, and the card
            // keeps the list it was given — unless the write failed, when the
            // diagnostic is what there is to show.
            if item.kind != "todo" || error {
                item.body = Some(text.clone());
            }
            item.status = if denied {
                "denied"
            } else if error {
                "failed"
            } else if known.as_ref().is_some_and(|k| k.background) {
                "background"
            } else {
                "done"
            }
            .into();
            events.push(self.item(item));
            if denied {
                events.push(self.event(
                    "permission_denied",
                    &text,
                    json!({"tool_use_id":id,"block":block}),
                ));
            }
        }
        events
    }
}

fn is_agent_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
}

/// A todo list as one item: the count on its line, the list itself as input
/// and as plain text for tools and context.
fn todo_item(key: &str, todos: &[Value]) -> Item {
    let done = todos
        .iter()
        .filter(|t| t["status"] == "completed")
        .count();
    let mut item = Item::new(key, "todo");
    item.name = "todo".into();
    item.title = format!("{done} of {} done", todos.len());
    item.input = Some(json!({"todos":todos}));
    item.body = Some(
        todos
            .iter()
            .map(|t| {
                let content = t["content"].as_str().unwrap_or_default();
                // Markers the bundled faces draw: no dingbats.
                match t["status"].as_str().unwrap_or("pending") {
                    "completed" => format!("[x] {content}"),
                    "in_progress" => {
                        format!("[>] {}", t["activeForm"].as_str().unwrap_or(content))
                    }
                    _ => format!("[ ] {content}"),
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    item.status = "done".into();
    item
}

/// A tool use in one line: the argument a person would name it by.
fn tool_title(name: &str, input: &Value) -> String {
    let text = |key: &str| input[key].as_str().unwrap_or_default();
    let title = match name {
        "Bash" => first_line(text("command"), 160),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => text("file_path").to_owned(),
        "Grep" => match text("path") {
            "" => text("pattern").to_owned(),
            path => format!("{} in {path}", text("pattern")),
        },
        "Glob" => text("pattern").to_owned(),
        "WebFetch" => text("url").to_owned(),
        "WebSearch" => text("query").to_owned(),
        "Agent" | "Task" => text("description").to_owned(),
        "Monitor" => first_line(text("command"), 160),
        "TaskOutput" | "KillShell" | "TaskStop" => text("task_id").to_owned(),
        _ => String::new(),
    };
    if title.is_empty() {
        summarize(input)
    } else {
        clip(&title, 160)
    }
}

/// The values the model wrote, on one line.
fn summarize(input: &Value) -> String {
    let parts: Vec<String> = match input {
        Value::Object(map) => map
            .values()
            .map(|v| match v {
                Value::String(s) => clip(&first_line(s, 80), 80),
                Value::Null => String::new(),
                other => clip(&other.to_string(), 80),
            })
            .collect(),
        Value::Null => vec![],
        other => vec![clip(&other.to_string(), 80)],
    };
    clip(
        &parts
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        160,
    )
}

fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let more = text.lines().filter(|l| !l.trim().is_empty()).count() > 1;
    let mut out = clip(line, max);
    if more && !out.ends_with('…') {
        out.push('…');
    }
    out
}

pub(super) fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// A tool result's text: a string, or the text parts of a content array.
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part["text"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| (part["type"] == "image").then(|| "[image]".to_owned()))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn mcp_result_text(result: &Value) -> String {
    if result.is_null() {
        return String::new();
    }
    let text = tool_result_text(&result["content"]);
    if !text.is_empty() {
        return text;
    }
    match &result["structured_content"] {
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn permission_denial(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "permission denied",
        "permission to use",
        "permission request",
        "requires approval",
        "approval required",
        "not allowed",
        "sandbox denied",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
#[path = "harness_tests.rs"]
mod tests;
