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
        Arc,
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
    /// session, assistant_delta, assistant, tool, usage, permission_denied,
    /// status, stderr, or error. `assistant` is the full completed item, not a
    /// delta; its data.id identifies the item when the provider supplies one.
    pub kind: String,
    pub text: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub data: Value,
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
    async fn cancelled(&self) {
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
        Provider::Claude => models.extend(["opus", "sonnet", "haiku"].map(String::from)),
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

async fn run_with_executable(
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

struct Decoder {
    provider: Provider,
    outcome: RunOutcome,
    completed: bool,
    failure: Option<String>,
    streamed: HashMap<String, String>,
    assistant_seen: bool,
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
        }
    }
    fn event(&self, kind: &str, text: &str, data: Value) -> HarnessEvent {
        HarnessEvent {
            kind: kind.into(),
            text: text.into(),
            session_id: self.outcome.session_id.clone(),
            model: self.outcome.model.clone(),
            data,
        }
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
                let item = &value["item"];
                let item_type = item["type"].as_str().unwrap_or_default();
                let id = item["id"].as_str().unwrap_or("assistant");
                if item_type == "agent_message" {
                    let text = item["text"].as_str().unwrap_or_default();
                    let prior = self.streamed.get(id).map(String::as_str).unwrap_or("");
                    let mut events = Vec::new();
                    if kind != "item.completed" {
                        if let Some(delta) = text.strip_prefix(prior).filter(|s| !s.is_empty()) {
                            events.push(self.event("assistant_delta", delta, json!({"id":id})));
                        }
                    } else {
                        events.push(self.event("assistant", text, item.clone()));
                        self.assistant_seen = true;
                    }
                    self.streamed.insert(id.to_owned(), text.to_owned());
                    events
                } else {
                    let text = match item_type {
                        "command_execution" => item["command"].as_str().unwrap_or("Command"),
                        "mcp_tool_call" => item["tool"].as_str().unwrap_or("Tool"),
                        "reasoning" => item["text"].as_str().unwrap_or("Reasoning"),
                        "error" => item["message"].as_str().unwrap_or("Tool error"),
                        "file_change" => "Files changed",
                        "web_search" => item["query"].as_str().unwrap_or("Web search"),
                        _ => item_type,
                    };
                    let mut events = vec![self.event("tool", text, value.clone())];
                    let output = item["aggregated_output"].as_str().unwrap_or_default();
                    if permission_denial(output) {
                        events.push(self.event("permission_denied", output, item.clone()));
                    }
                    events
                }
            }
            _ => vec![self.event("status", "", value)],
        }
    }
    fn claude(&mut self, value: Value) -> Vec<HarnessEvent> {
        if let Some(session) = value["session_id"].as_str().filter(|s| !s.is_empty()) {
            self.outcome.session_id = Some(session.to_owned());
        }
        match value["type"].as_str().unwrap_or_default() {
            "system" if value["subtype"] == "init" => {
                self.outcome.model = value["model"]
                    .as_str()
                    .map(String::from)
                    .or(self.outcome.model.take());
                vec![self.event("session", "", value)]
            }
            "system" if value["subtype"] == "permission_denied" => vec![self.event(
                "permission_denied",
                "The provider denied a tool request",
                value,
            )],
            "stream_event" => {
                let event = &value["event"];
                match event["type"].as_str().unwrap_or_default() {
                    "message_start" => {
                        if let Some(model) = event["message"]["model"].as_str() {
                            self.outcome.model = Some(model.to_owned());
                        }
                        vec![]
                    }
                    "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                        let text = event["delta"]["text"].as_str().unwrap_or_default();
                        vec![self.event("assistant_delta", text, value.clone())]
                    }
                    "content_block_start" if event["content_block"]["type"] == "tool_use" => {
                        let text = event["content_block"]["name"].as_str().unwrap_or("Tool");
                        vec![self.event("tool", text, value.clone())]
                    }
                    _ => vec![],
                }
            }
            "assistant" => {
                if let Some(model) = value["message"]["model"].as_str() {
                    self.outcome.model = Some(model.to_owned());
                }
                let mut events = Vec::new();
                let mut text = Vec::new();
                for block in value["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or_default() {
                        "text" => {
                            if let Some(part) = block["text"].as_str() {
                                text.push(part);
                            }
                        }
                        "tool_use" => events.push(self.event(
                            "tool",
                            block["name"].as_str().unwrap_or("Tool"),
                            block.clone(),
                        )),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    events.push(self.event(
                        "assistant",
                        &text.join("\n"),
                        json!({"id":value["message"]["id"],"message":value["message"]}),
                    ));
                    self.assistant_seen = true;
                }
                events
            }
            "user" => {
                let mut events = Vec::new();
                for block in value["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        let text = block["content"]
                            .as_str()
                            .map(String::from)
                            .unwrap_or_else(|| block["content"].to_string());
                        let kind = if block["is_error"] == true && permission_denial(&text) {
                            "permission_denied"
                        } else {
                            "tool"
                        };
                        events.push(self.event(kind, &text, block.clone()));
                    }
                }
                events
            }
            "result" => {
                self.completed = true;
                self.outcome.usage = json!({"usage":value["usage"],"modelUsage":value["modelUsage"],"total_cost_usd":value["total_cost_usd"]});
                let mut events = Vec::new();
                for denied in value["permission_denials"].as_array().into_iter().flatten() {
                    events.push(
                        self.event(
                            "permission_denied",
                            denied["tool_name"]
                                .as_str()
                                .unwrap_or("Tool permission denied"),
                            denied.clone(),
                        ),
                    );
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
                        events.push(self.event("assistant", text, value.clone()));
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
