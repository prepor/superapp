//! Device-local MCP transport. Tokens live only for one harness run; all tool
//! execution returns to that store's Session and its normal approval/edit path.
use kernel::{
    effect::World,
    session::{Edit, Session},
    store::Store,
    tool::{Prepared, Tool},
};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch, Semaphore},
};

const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_CALLS: usize = 256;
const MAX_RESULT: usize = 256 * 1024;
type Answer = Result<Value, String>;
type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct RunConnection {
    pub url: String,
    pub bearer_token: String,
}
#[derive(Clone, Copy, Debug)]
struct Context {
    chat: i64,
    run: i64,
    read_only: bool,
}
struct Server {
    url: String,
    abort: tokio::task::AbortHandle,
}
struct Cached {
    input: String,
    answer: watch::Sender<Option<Value>>,
}
enum Request {
    Cancel {
        context: Context,
        key: String,
    },
    List {
        context: Context,
        answer: oneshot::Sender<Answer>,
    },
    Call {
        context: Context,
        key: String,
        name: String,
        input: Value,
        answer: watch::Sender<Option<Value>>,
    },
}
#[derive(Clone)]
struct Invocation {
    context: Context,
    key: String,
    tool: Tool,
    input: Value,
    answer: watch::Sender<Option<Value>>,
    started: bool,
}
#[derive(Default)]
struct Inner {
    tokens: HashMap<String, Context>,
    requests: VecDeque<Request>,
    calls: HashMap<i64, Invocation>,
    cache: HashMap<(i64, String), Cached>,
}
#[derive(Default)]
struct Bridge {
    inner: Mutex<Inner>,
    server: tokio::sync::Mutex<Option<Server>>,
    notify: Mutex<Option<Notify>>,
    caller: Mutex<Option<Context>>,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(server) = self.server.get_mut().take() {
            server.abort.abort();
        }
    }
}

/// Register after the run is marked running. The run's plan/work mode also
/// constrains MCP, independently of each provider's filesystem sandbox.
pub async fn register_run(
    world: &World,
    chat_id: i64,
    run_id: i64,
) -> Result<RunConnection, String> {
    let read_only = world
        .store()
        .conn()
        .query_row(
            "SELECT mode='plan' FROM workshop_run WHERE id=?1 AND chat_id=?2 AND status='running'",
            params![run_id, chat_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|_| "The harness run is no longer active".to_owned())?;
    let bridge = world.store().local::<Bridge>();
    let url = start_server(&bridge).await?;
    let token = token()?;
    bridge.inner.lock().unwrap().tokens.insert(
        token.clone(),
        Context {
            chat: chat_id,
            run: run_id,
            read_only,
        },
    );
    Ok(RunConnection {
        url,
        bearer_token: token,
    })
}

pub fn unregister_run(store: &Store, run_id: i64) {
    let bridge = store.local::<Bridge>();
    // A lifecycle tool may intentionally stop its own harness. Its committed
    // reply must reach that request before revocation fails the other calls.
    let calls: Vec<_> = bridge
        .inner
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(_, call)| call.context.run == run_id && stops_own_run(call.tool.name))
        .map(|(id, call)| (*id, call.answer.clone()))
        .collect();
    for (id, answer) in calls {
        let result = store.conn().query_row(
            "SELECT result FROM workshop_tool_call WHERE id=?1 AND run_id=?2 AND status='done'",
            params![id, run_id],
            |row| row.get::<_, String>(0),
        );
        if let Ok(result) = result {
            if let Ok(value) = serde_json::from_str(&result) {
                answer.send_replace(Some(tool_result(Ok(value))));
            }
        }
    }
    revoke(&bridge, run_id);
}

/// Hold immediately after registration. Cancellation, early errors and dropped
/// run futures revoke authentication too; the guard does not retain the store.
pub struct RunGuard {
    bridge: Weak<Bridge>,
    run_id: i64,
}
pub fn guard_run(store: &Store, run_id: i64) -> RunGuard {
    RunGuard {
        bridge: Arc::downgrade(&store.local::<Bridge>()),
        run_id,
    }
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(bridge) = self.bridge.upgrade() {
            revoke(&bridge, self.run_id);
        }
    }
}

fn revoke(bridge: &Bridge, run_id: i64) {
    let mut inner = bridge.inner.lock().unwrap();
    inner.tokens.retain(|_, context| context.run != run_id);
    inner.cache.retain(|(run, _), cached| {
        if *run == run_id {
            if cached.answer.borrow().is_none() {
                cached.answer.send_replace(Some(tool_result(Err(
                    "The harness run ended; the call will not be replayed".into(),
                ))));
            }
            false
        } else {
            true
        }
    });
    drop(inner);
    wake(bridge);
}

fn token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "Could not create local tool authentication".to_owned())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn wake(bridge: &Bridge) {
    if let Some(notify) = bridge.notify.lock().unwrap().clone() {
        notify();
    }
}
fn alive(bridge: &Bridge, context: Context) -> bool {
    bridge
        .inner
        .lock()
        .unwrap()
        .tokens
        .values()
        .any(|active| active.run == context.run && active.chat == context.chat)
}

/// Available only during the originating run's synchronous tool stage/run.
/// Tool adapters can retain this origin instead of guessing from global focus.
pub fn caller(store: &Store) -> Option<(i64, i64)> {
    store
        .local::<Bridge>()
        .caller
        .lock()
        .unwrap()
        .map(|context| (context.chat, context.run))
}
struct CallerScope {
    bridge: Arc<Bridge>,
    previous: Option<Context>,
}
impl CallerScope {
    fn enter(store: &Store, context: Context) -> Self {
        let bridge = store.local::<Bridge>();
        let previous = bridge.caller.lock().unwrap().replace(context);
        Self { bridge, previous }
    }
}
impl Drop for CallerScope {
    fn drop(&mut self) {
        *self.bridge.caller.lock().unwrap() = self.previous;
    }
}

async fn start_server(bridge: &Arc<Bridge>) -> Result<String, String> {
    let mut server = bridge.server.lock().await;
    if let Some(server) = server.as_ref() {
        return Ok(server.url.clone());
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|error| format!("Start local app tools: {error}"))?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let url = format!("http://{address}/mcp");
    let weak = Arc::downgrade(bridge);
    let host = address.to_string();
    let task = kernel::runtime::spawn(async move {
        let permits = Arc::new(Semaphore::new(32));
        while let Ok((stream, peer)) = listener.accept().await {
            if !peer.ip().is_loopback() {
                continue;
            }
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                continue;
            };
            let weak = weak.clone();
            let host = host.clone();
            kernel::runtime::spawn(async move {
                let _permit = permit;
                let _ = connection(stream, weak, &host).await;
            });
        }
    });
    *server = Some(Server {
        url: url.clone(),
        abort: task.abort_handle(),
    });
    Ok(url)
}

struct HttpRequest {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}
async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let boundary = loop {
        if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            if at > MAX_HEADERS {
                return Err("Headers exceed limit".into());
            }
            break at;
        }
        if bytes.len() > MAX_HEADERS {
            return Err("Headers exceed limit".into());
        }
        let count = stream
            .read(&mut chunk)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("Incomplete request".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    };
    let header = std::str::from_utf8(&bytes[..boundary]).map_err(|_| "Invalid HTTP headers")?;
    let mut lines = header.split("\r\n");
    let request: Vec<_> = lines.next().unwrap_or("").split(' ').collect();
    if request.len() != 3 || request[2] != "HTTP/1.1" {
        return Err("HTTP/1.1 required".into());
    }
    let method = request[0].to_owned();
    let target = request[1].to_owned();
    let mut headers = HashMap::new();
    for line in lines {
        let (key, value) = line.split_once(':').ok_or("Invalid HTTP header")?;
        if headers
            .insert(key.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err("Duplicate HTTP header".into());
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err("Chunked requests are not supported".into());
    }
    let length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>().map_err(|_| "Invalid content length"))
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err("Request body exceeds 1 MiB".into());
    }
    let start = boundary + 4;
    while bytes.len() < start + length {
        let count = stream
            .read(&mut chunk)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("Incomplete request body".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(HttpRequest {
        method,
        target,
        headers,
        body: bytes[start..start + length].to_vec(),
    })
}

async fn response(stream: &mut TcpStream, status: u16, body: Value) -> Result<(), String> {
    let encoded = if status == 202 {
        Vec::new()
    } else {
        serde_json::to_vec(&body).map_err(|error| error.to_string())?
    };
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let header=format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",encoded.len());
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stream
        .write_all(&encoded)
        .await
        .map_err(|error| error.to_string())?;
    stream.shutdown().await.map_err(|error| error.to_string())
}
fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.into()}})
}
fn rpc_reply(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn tool_result(result: Answer) -> Value {
    match result {
        Ok(value) => json!({"content":[{"type":"text","text":value.to_string()}],"isError":false}),
        Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
    }
}

async fn connection(mut stream: TcpStream, weak: Weak<Bridge>, host: &str) -> Result<(), String> {
    let request = match tokio::time::timeout(Duration::from_secs(15), read_request(&mut stream))
        .await
    {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => return response(&mut stream, 400, json!({"error":error})).await,
        Err(_) => return response(&mut stream, 400, json!({"error":"Request timed out"})).await,
    };
    if request.headers.get("host").map(String::as_str) != Some(host)
        || request.headers.contains_key("origin")
    {
        return response(
            &mut stream,
            403,
            json!({"error":"Only native loopback clients are accepted"}),
        )
        .await;
    }
    if request.target != "/mcp" {
        return response(&mut stream, 404, json!({"error":"Not found"})).await;
    }
    let Some(bridge) = weak.upgrade() else {
        return response(&mut stream, 401, json!({"error":"App tools are closed"})).await;
    };
    let authorization = request
        .headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "));
    let context =
        authorization.and_then(|token| bridge.inner.lock().unwrap().tokens.get(token).copied());
    let Some(context) = context else {
        return response(
            &mut stream,
            401,
            json!({"error":"Invalid or expired run token"}),
        )
        .await;
    };
    if request.method != "POST" {
        return response(&mut stream, 405, json!({"error":"Use POST"})).await;
    }
    let value: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => {
            return response(
                &mut stream,
                200,
                rpc_error(Value::Null, -32700, "Invalid JSON"),
            )
            .await
        }
    };
    if value["jsonrpc"] != "2.0" || !value.is_object() {
        return response(
            &mut stream,
            200,
            rpc_error(value["id"].clone(), -32600, "Invalid JSON-RPC request"),
        )
        .await;
    }
    if request
        .headers
        .get("mcp-protocol-version")
        .is_some_and(|version| {
            !["2025-03-26", "2025-06-18", "2025-11-25"].contains(&version.as_str())
        })
    {
        return response(
            &mut stream,
            400,
            json!({"error":"Unsupported MCP protocol version"}),
        )
        .await;
    }
    let id = value["id"].clone();
    let method = value["method"].as_str().unwrap_or("");
    if id.is_null() && method == "notifications/initialized" {
        return response(&mut stream, 202, Value::Null).await;
    }
    if id.is_null() && method == "notifications/cancelled" {
        let key = value["params"]["requestId"].to_string();
        let accepted = {
            let mut inner = bridge.inner.lock().unwrap();
            if inner.requests.len() >= MAX_CALLS {
                false
            } else {
                inner.requests.push_back(Request::Cancel { context, key });
                true
            }
        };
        if !accepted {
            return response(
                &mut stream,
                400,
                json!({"error":"Too many pending app tool requests"}),
            )
            .await;
        }
        wake(&bridge);
        return response(&mut stream, 202, Value::Null).await;
    }
    if id.is_null() || !(id.is_string() || id.is_number()) {
        return response(
            &mut stream,
            200,
            rpc_error(id, -32600, "A request ID is required"),
        )
        .await;
    }
    let answer = match method {
        "initialize" => {
            let requested = value["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2025-03-26");
            let protocol = if ["2025-03-26", "2025-06-18", "2025-11-25"].contains(&requested) {
                requested
            } else {
                "2025-03-26"
            };
            rpc_reply(
                id,
                json!({"protocolVersion":protocol,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"Superapp","version":"1"},"instructions":"Tools act in the running Superapp on this Mac. Irreversible actions pause for approval in your Workshop chat. Human review marks cannot be set by agents. Query sql.schema to inspect available local app tables."}),
            )
        }
        "ping" => rpc_reply(id, json!({})),
        "tools/list" => {
            let (answer, receive) = oneshot::channel();
            if bridge.inner.lock().unwrap().requests.len() >= MAX_CALLS {
                rpc_error(id, -32603, "Too many pending app tool requests")
            } else {
                bridge
                    .inner
                    .lock()
                    .unwrap()
                    .requests
                    .push_back(Request::List { context, answer });
                wake(&bridge);
                match tokio::time::timeout(Duration::from_secs(30), receive).await {
                    Ok(Ok(Ok(tools))) => rpc_reply(id, tools),
                    Ok(Ok(Err(error))) => rpc_error(id, -32603, error),
                    _ => rpc_error(
                        id,
                        -32603,
                        "The app did not answer the tool catalogue request",
                    ),
                }
            }
        }
        "tools/call" => {
            let name = value["params"]["name"].as_str().unwrap_or("");
            let input = value["params"]
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if name.is_empty() || !input.is_object() {
                rpc_error(id, -32602, "Expected a tool name and object arguments")
            } else {
                let key = (context.run, id.to_string());
                let signature = json!([name, input]).to_string();
                let receive = {
                    let mut inner = bridge.inner.lock().unwrap();
                    if let Some(cached) = inner.cache.get(&key) {
                        if cached.input != signature {
                            Err("Request ID was already used with different arguments".to_owned())
                        } else {
                            Ok(cached.answer.subscribe())
                        }
                    } else if inner.cache.len() >= MAX_CALLS {
                        Err("This app has too many active tool calls; finish the current turn before starting more".to_owned())
                    } else {
                        let (answer, receive) = watch::channel(None);
                        inner.cache.insert(
                            key,
                            Cached {
                                input: signature,
                                answer: answer.clone(),
                            },
                        );
                        inner.requests.push_back(Request::Call {
                            context,
                            key: id.to_string(),
                            name: name.to_owned(),
                            input,
                            answer,
                        });
                        Ok(receive)
                    }
                };
                match receive {
                    Err(error) => rpc_error(id, -32602, error),
                    Ok(mut receive) => {
                        wake(&bridge);
                        let wait = async {
                            loop {
                                if let Some(answer) = receive.borrow().clone() {
                                    break answer;
                                }
                                if receive.changed().await.is_err() {
                                    break tool_result(Err("App tool connection closed".into()));
                                }
                            }
                        };
                        match tokio::time::timeout(Duration::from_secs(3600),wait).await{Ok(result)=>rpc_reply(id,result),Err(_)=>rpc_error(id,-32603,"Tool approval or execution timed out; inspect the chat before retrying")}
                    }
                }
            }
        }
        _ => rpc_error(id, -32601, "Unsupported method"),
    };
    response(&mut stream, 200, answer).await
}

fn active(c: &Connection, call: i64, context: Context) -> rusqlite::Result<bool> {
    c.query_row("SELECT EXISTS(SELECT 1 FROM workshop_tool_call t JOIN workshop_run r ON r.id=t.run_id WHERE t.id=?1 AND t.run_id=?2 AND t.chat_id=?3 AND t.status='running' AND r.status='running')",params![call,context.run,context.chat],|row|row.get(0))
}
fn stops_own_run(name: &str) -> bool {
    matches!(
        name,
        "workshop.chats.stop" | "workshop.chats.close" | "workshop.workspaces.archive"
    )
}
fn stopped_by_edit(
    c: &Connection,
    call: i64,
    context: Context,
    result: &Value,
) -> rusqlite::Result<bool> {
    // stop_runs is produced by the trusted lifecycle transaction from runs
    // that were pending/running BEFORE it stopped them. An earlier external
    // cancellation cannot appear here, so cannot authorize a stale edit.
    if !result["stop_runs"]
        .as_array()
        .is_some_and(|runs| runs.iter().any(|run| run.as_i64() == Some(context.run)))
    {
        return Ok(false);
    }
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workshop_tool_call t JOIN workshop_run r ON r.id=t.run_id WHERE t.id=?1 AND t.run_id=?2 AND t.chat_id=?3 AND t.status='interrupted' AND r.status='stopped')",
        params![call, context.run, context.chat],
        |row| row.get(0),
    )
}
fn run_active(c: &Connection, context: Context) -> bool {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workshop_run WHERE id=?1 AND chat_id=?2 AND status='running')",
        params![context.run, context.chat],
        |row| row.get(0),
    )
    .unwrap_or(false)
}

/// Called from Workshop::poll. No socket thread invokes a UI or store tool.
pub fn poll(session: &mut Session) {
    let bridge = session.store().local::<Bridge>();
    *bridge.notify.lock().unwrap() = session.store().ui_waker();
    let requests: Vec<_> = {
        let mut inner = bridge.inner.lock().unwrap();
        let count = inner.requests.len().min(16);
        inner.requests.drain(..count).collect()
    };
    for request in requests {
        match request {
            Request::Cancel { context, key } => {
                let calls: Vec<_> = bridge
                    .inner
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|(_, call)| call.context.run == context.run && call.key == key)
                    .map(|(id, call)| (*id, call.clone()))
                    .collect();
                for (id, call) in calls {
                    finish(
                        session,
                        id,
                        call,
                        Err("The harness cancelled this tool request".into()),
                        "interrupted",
                    );
                }
                if let Some(cached) = bridge.inner.lock().unwrap().cache.get(&(context.run, key)) {
                    if cached.answer.borrow().is_none() {
                        cached.answer.send_replace(Some(tool_result(Err(
                            "The harness cancelled this tool request".into(),
                        ))));
                    }
                }
            }
            Request::List { context, answer } => {
                let result = if !alive(&bridge, context)
                    || !run_active(session.store().conn(), context)
                {
                    Err("The harness run is no longer active".into())
                } else {
                    Ok(
                        json!({"tools":session.apps().tools().iter().filter(|tool|!context.read_only||!tool.writes).map(|tool|json!({"name":tool.name,"description":tool.description,"inputSchema":tool.input,"annotations":{"readOnlyHint":!tool.writes,"destructiveHint":tool.asks}})).collect::<Vec<_>>()}),
                    )
                };
                let _ = answer.send(result);
            }
            Request::Call {
                context,
                key,
                name,
                input,
                answer,
            } => {
                let tool = session.apps().tool(&name).cloned();
                let validation = if !alive(&bridge, context)
                    || !run_active(session.store().conn(), context)
                {
                    Err("The harness run is no longer active".into())
                } else if let Some(tool) = &tool {
                    if context.read_only && tool.writes {
                        Err("This is a read-only plan run; a work run is required for writing tools".into())
                    } else {
                        tool.check(&input)
                    }
                } else {
                    Err(format!("Unknown app tool {name}"))
                };
                if let Err(error) = validation {
                    answer.send_replace(Some(tool_result(Err(error))));
                    continue;
                }
                let tool = tool.unwrap();
                let invocation = Invocation {
                    context,
                    key,
                    tool: tool.clone(),
                    input: input.clone(),
                    answer: answer.clone(),
                    started: false,
                };
                let now = session.now();
                session.act_async_result(Edit::writing("workshop.tool.received","Receive app tool request",move|c|{
    if !run_active(c,context){return Err(cancelled());}
    c.execute("INSERT INTO workshop_tool_call(chat_id,run_id,name,arguments,status,created) VALUES(?1,?2,?3,?4,?5,?6)",params![context.chat,context.run,name,input.to_string(),if tool.asks{"pending"}else{"approved"},now])?;Ok(c.last_insert_rowid())
   }).record_if(|_|false).wake_if(|_|false),move|session,result|{match result{Ok(id)=>{let cancelled=invocation.answer.borrow().is_some();if cancelled{finish(session,id,invocation,Err("The harness cancelled this tool request before it was recorded".into()),"interrupted");}else{session.store().local::<Bridge>().inner.lock().unwrap().calls.insert(id,invocation);}},Err(error)=>{answer.send_replace(Some(tool_result(Err(error.to_string()))));}}});
            }
        }
    }
    let calls: Vec<_> = bridge
        .inner
        .lock()
        .unwrap()
        .calls
        .iter()
        .map(|(id, call)| (*id, call.clone()))
        .collect();
    for (id, call) in calls {
        if call.started {
            continue;
        }
        let status = session.store().conn().query_row(
            "SELECT status FROM workshop_tool_call WHERE id=?",
            [id],
            |row| row.get::<_, String>(0),
        );
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                finish(session, id, call, Err(error.to_string()), "failed");
                continue;
            }
        };
        if status == "refused" {
            finish(
                session,
                id,
                call,
                Err("The person refused this tool call".into()),
                "refused",
            );
            continue;
        }
        if !alive(&bridge, call.context) || !run_active(session.store().conn(), call.context) {
            finish(
                session,
                id,
                call,
                Err("The harness run ended before this tool call started".into()),
                "interrupted",
            );
            continue;
        }
        if status == "approved" {
            if let Some(pending) = bridge.inner.lock().unwrap().calls.get_mut(&id) {
                pending.started = true;
            }
            let context = call.context;
            session.act_async_result(Edit::writing("workshop.tool.start","Start app tool",move|c|{if !run_active(c,context){return Err(cancelled());}let count=c.execute("UPDATE workshop_tool_call SET status='running' WHERE id=? AND status='approved'",[id])?;if count!=1{return Err(cancelled());}Ok(())}).record_if(|_|false).wake_if(|_|false),move|session,result|match result{Ok(())=>invoke(session,id,call),Err(error)=>finish(session,id,call,Err(error.to_string()),"interrupted")});
        } else if !matches!(status.as_str(), "pending" | "running") {
            finish(
                session,
                id,
                call,
                Err(format!("Tool call is {status}")),
                "interrupted",
            );
        }
    }
}

fn cancelled() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
        Some("The app tool call was cancelled".into()),
    )
}
fn invoke(session: &mut Session, id: i64, call: Invocation) {
    if !alive(&session.store().local::<Bridge>(), call.context)
        || !active(session.store().conn(), id, call.context).unwrap_or(false)
    {
        finish(
            session,
            id,
            call,
            Err("The tool call was cancelled".into()),
            "interrupted",
        );
        return;
    }
    let prepare = if let Some(stage) = call.tool.stager {
        let _scope = CallerScope::enter(session.store(), call.context);
        Some(stage(session, &call.input))
    } else if let Some(prepare) = call.tool.preparer {
        Some(Ok(prepare(&call.input)))
    } else if let Some(reader) = call.tool.reader {
        let read = reader(&call.input);
        let prepare: kernel::tool::Prepare =
            Box::new(move |world| Box::pin(async move { read(world).await.map(Prepared::Reply) }));
        Some(Ok(prepare))
    } else {
        None
    };
    match prepare {
        Some(Ok(prepare)) => session.prepare_tool(prepare, move |session, result| {
            prepared(session, id, call, result)
        }),
        Some(Err(error)) => finish(session, id, call, Err(error), "failed"),
        None => {
            let result = {
                let _scope = CallerScope::enter(session.store(), call.context);
                (call.tool.run)(session, &call.input)
            };
            let status = if result.is_ok() { "done" } else { "failed" };
            finish(session, id, call, result, status);
        }
    }
}
fn prepared(session: &mut Session, id: i64, call: Invocation, result: Result<Prepared, String>) {
    if !alive(&session.store().local::<Bridge>(), call.context)
        || !active(session.store().conn(), id, call.context).unwrap_or(false)
    {
        finish(
            session,
            id,
            call,
            Err("The tool call was cancelled during preparation".into()),
            "interrupted",
        );
        return;
    }
    match result {
        Ok(Prepared::Edit(edit)) => {
            let context = call.context;
            let self_stop = stops_own_run(call.tool.name);
            let bridge = Arc::downgrade(&session.store().local::<Bridge>());
            let answer = call.answer.clone();
            let edit = edit.then_write(move |c, result| {
                if !bridge
                    .upgrade()
                    .is_some_and(|bridge| alive(&bridge, context))
                    || answer.borrow().is_some()
                    || !(active(c, id, context)?
                        || (self_stop && stopped_by_edit(c, id, context, result)?))
                {
                    return Err(cancelled());
                }
                c.execute(
                    "UPDATE workshop_tool_call SET status='done',result=?2,error='' WHERE id=?1",
                    params![id, result.to_string()],
                )?;
                Ok(())
            });
            session.act_async_result(edit, move |session, result| match result {
                Ok(value) => reply(session, id, call, Ok(value)),
                Err(error) => finish(session, id, call, Err(error.to_string()), "failed"),
            });
        }
        Ok(prepared) => prepared.commit(session, move |session, result| {
            let status = if result.is_ok() { "done" } else { "failed" };
            finish(session, id, call, result, status);
        }),
        Err(error) => finish(session, id, call, Err(error), "failed"),
    }
}
fn finish(session: &mut Session, id: i64, call: Invocation, result: Answer, status: &'static str) {
    let value = result
        .as_ref()
        .ok()
        .map(Value::to_string)
        .unwrap_or_default();
    let error = result.as_ref().err().cloned().unwrap_or_default();
    session.act_async_result(
        Edit::writing("workshop.tool.result", "Record app tool result", move |c| {
            c.execute(
                "UPDATE workshop_tool_call SET status=?2,result=?3,error=?4 WHERE id=?1",
                params![id, status, value, error],
            )?;
            Ok(())
        })
        .record_if(|_| false)
        .wake_if(|_| false),
        move |session, stored| {
            reply(
                session,
                id,
                call,
                match stored {
                    Ok(()) => result,
                    Err(error) => Err(format!(
                        "Could not record tool outcome; inspect the app before retrying: {error}"
                    )),
                },
            );
        },
    );
}
fn reply(session: &Session, id: i64, call: Invocation, result: Answer) {
    session
        .store()
        .local::<Bridge>()
        .inner
        .lock()
        .unwrap()
        .calls
        .remove(&id);
    let answer = tool_result(result);
    let answer = if answer.to_string().len() > MAX_RESULT {
        tool_result(Ok(
            json!({"tool_call_id":id,"message":"The tool completed. Its full result exceeds the transport limit; inspect workshop_tool_call.result with a narrower SQL query."}),
        ))
    } else {
        answer
    };
    call.answer.send_replace(Some(answer));
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::app::App;
    static APPS: &[&dyn App] = &[&super::super::WORKSHOP];

    fn fixture(plan: bool) -> (Session, RunConnection, i64) {
        let session = Session::fake(APPS);
        let run=session.store().write(move|c|{c.execute("INSERT INTO workshop_run(chat_id,provider,model,prompt,mode,status,created) VALUES(1,'codex','default','tool fixture',?1,'running',1)",[if plan{"plan"}else{"work"}])?;Ok(c.last_insert_rowid())}).unwrap();
        let connection = kernel::runtime::block_on(register_run(session.world(), 1, run)).unwrap();
        (session, connection, run)
    }

    fn request(
        connection: &RunConnection,
        value: Value,
        authorized: bool,
    ) -> tokio::task::JoinHandle<(u16, Value)> {
        let url = connection.url.clone();
        let token = connection.bearer_token.clone();
        kernel::runtime::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap();
            let mut request = client
                .post(url)
                .header("Content-Type", "application/json")
                .body(value.to_string());
            if authorized {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            let bytes = response.bytes().await.unwrap();
            (
                status,
                if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                },
            )
        })
    }
    fn pump(session: &mut Session, request: tokio::task::JoinHandle<(u16, Value)>) -> (u16, Value) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !request.is_finished() {
            session.settle();
            poll(session);
            assert!(
                std::time::Instant::now() < deadline,
                "local MCP request did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        kernel::runtime::block_on(request).unwrap()
    }
    fn call(id: u64, name: &str, input: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":input}})
    }

    #[test]
    fn local_http_requires_run_token_and_uses_this_sessions_catalogue() {
        let (mut session, connection, run) = fixture(false);
        let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}});
        let unauthorized = request(&connection, initialize.clone(), false);
        assert_eq!(pump(&mut session, unauthorized).0, 401);
        let authorized = request(&connection, initialize, true);
        let (status, response) = pump(&mut session, authorized);
        assert_eq!(status, 200);
        assert_eq!(response["result"]["serverInfo"]["name"], "Superapp");
        let list = request(
            &connection,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            true,
        );
        let (_, response) = pump(&mut session, list);
        let tools = response["result"]["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "workshop.review.mark_file"));
        assert!(tools.iter().any(|tool| tool["name"] == "sql.schema"));
        assert!(!tools
            .iter()
            .any(|tool| tool["name"].as_str().unwrap().starts_with("mail.")));
        let list = request(
            &connection,
            call(3, "workshop.workspaces.list", json!({})),
            true,
        );
        let (_, response) = pump(&mut session, list);
        assert_eq!(response["result"]["isError"], false);
        unregister_run(session.store(), run);
        let expired = request(
            &connection,
            json!({"jsonrpc":"2.0","id":4,"method":"ping"}),
            true,
        );
        assert_eq!(pump(&mut session, expired).0, 401);
    }

    #[test]
    fn lifecycle_tools_can_stop_their_own_harness_and_return_the_committed_reply() {
        for (name, input, column) in [
            ("workshop.chats.stop", json!({"chat_id":1}), None),
            ("workshop.chats.close", json!({"chat_id":1}), Some("closed")),
            (
                "workshop.workspaces.archive",
                json!({"workspace_id":1}),
                Some("archived"),
            ),
        ] {
            let (mut session, connection, run) = fixture(false);
            let pending = request(&connection, call(1, name, input), true);
            let (_, response) = pump(&mut session, pending);
            assert_eq!(response["result"]["isError"], false, "{name}: {response}");
            assert_eq!(
                session
                    .store()
                    .conn()
                    .query_row("SELECT status FROM workshop_run WHERE id=?", [run], |row| {
                        row.get::<_, String>(0)
                    },)
                    .unwrap(),
                "stopped",
            );
            assert_eq!(
                session
                    .store()
                    .conn()
                    .query_row(
                        "SELECT status FROM workshop_tool_call WHERE run_id=? AND name=?",
                        params![run, name],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "done",
            );
            if let Some(column) = column {
                let query = if column == "closed" {
                    "SELECT closed FROM workshop_chat WHERE id=1"
                } else {
                    "SELECT archived FROM workshop_workspace WHERE id=1"
                };
                assert!(session
                    .store()
                    .conn()
                    .query_row(query, [], |row| { row.get::<_, bool>(0) })
                    .unwrap());
            }
            let expired = request(
                &connection,
                json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
                true,
            );
            assert_eq!(pump(&mut session, expired).0, 401);
        }
    }

    #[test]
    fn external_cancellation_ahead_of_a_prepared_lifecycle_edit_rolls_it_back() {
        let (mut session, connection, run) = fixture(false);
        let context = Context {
            chat: 1,
            run,
            read_only: false,
        };
        let name = "workshop.workspaces.archive";
        let input = json!({"workspace_id":1});
        let tool = session.apps().tool(name).unwrap().clone();
        let id = session.store().write(move |c| {
            c.execute(
                "INSERT INTO workshop_tool_call(chat_id,run_id,name,arguments,status,created) VALUES(1,?1,?2,'{\"workspace_id\":1}','running',1)",
                params![run, name],
            )?;
            Ok(c.last_insert_rowid())
        }).unwrap();
        let prepare = {
            let _scope = CallerScope::enter(session.store(), context);
            tool.stager.unwrap()(&mut session, &input).unwrap()
        };
        let result = kernel::runtime::block_on(prepare(session.world()));
        let (answer, receive) = watch::channel(None);
        let invocation = Invocation {
            context,
            key: "delayed-lifecycle".into(),
            tool,
            input,
            answer,
            started: true,
        };
        session.store().attach_ui(|| {});
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let _blocking = session
            .store()
            .submit_write(move |_| {
                entered.send(()).unwrap();
                held.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        // The UI reader still sees an active call when prepared() checks it,
        // but this already accepted cancellation commits before its edit.
        let _cancel = session
            .store()
            .submit_write(move |c| {
                c.execute("UPDATE workshop_run SET status='stopped' WHERE id=?", [run])?;
                c.execute(
                    "UPDATE workshop_tool_call SET status='interrupted' WHERE id=?",
                    [id],
                )?;
                Ok(())
            })
            .unwrap();
        prepared(&mut session, id, invocation, result);
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while receive.borrow().is_none() {
            session.settle();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(receive.borrow().as_ref().unwrap()["isError"], true);
        assert!(!session
            .store()
            .conn()
            .query_row(
                "SELECT archived FROM workshop_workspace WHERE id=1",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
        assert_eq!(
            session
                .store()
                .conn()
                .query_row("SELECT status FROM workshop_run WHERE id=?", [run], |row| {
                    row.get::<_, String>(0)
                },)
                .unwrap(),
            "stopped"
        );
        unregister_run(session.store(), run);
        drop(connection);
    }

    #[test]
    fn archiving_interrupts_an_approved_tool_before_it_can_publish() {
        let (mut session, connection, run) = fixture(false);
        let pending = request(
            &connection,
            call(1, "workshop.github.merge", json!({"workspace_id":1})),
            true,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let id = loop {
            poll(&mut session);
            let id = session
                .store()
                .conn()
                .query_row(
                    "SELECT id FROM workshop_tool_call WHERE run_id=? AND status='pending'",
                    [run],
                    |row| row.get::<_, i64>(0),
                )
                .ok();
            if let Some(id) = id {
                break id;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        session
            .store()
            .write(move |c| {
                c.execute(
                    "UPDATE workshop_tool_call SET status='approved' WHERE id=?",
                    [id],
                )?;
                Ok(())
            })
            .unwrap();
        let command = super::super::runtime::command_edit(
            super::super::runtime::Command::ArchiveWorkspace { workspace_id: 1 },
            session.now(),
            "/sample/workshop".into(),
            None,
            "human".into(),
        );
        session.act_async_result(command, |_, result| {
            result.unwrap();
        });
        let (_, response) = pump(&mut session, pending);
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            session
                .store()
                .conn()
                .query_row(
                    "SELECT status FROM workshop_tool_call WHERE id=?",
                    [id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "interrupted"
        );
        assert_eq!(
            session
                .store()
                .conn()
                .query_row("SELECT count(*) FROM workshop_job", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
        unregister_run(session.store(), run);
    }

    #[test]
    fn asks_wait_for_approval_and_http_retries_do_not_repeat_external_jobs() {
        let (mut session, connection, run) = fixture(false);
        let payload = call(1, "workshop.github.merge", json!({"workspace_id":1}));
        let pending = request(&connection, payload.clone(), true);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let id = loop {
            poll(&mut session);
            let id = session
                .store()
                .conn()
                .query_row(
                    "SELECT id FROM workshop_tool_call WHERE status='pending' LIMIT 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .ok();
            if let Some(id) = id {
                break id;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(!pending.is_finished());
        assert_eq!(
            session
                .store()
                .conn()
                .query_row("SELECT count(*) FROM workshop_job", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        session
            .store()
            .write(move |c| {
                c.execute(
                    "UPDATE workshop_tool_call SET status='approved' WHERE id=?",
                    [id],
                )?;
                Ok(())
            })
            .unwrap();
        let (_, first) = pump(&mut session, pending);
        assert_eq!(first["result"]["isError"], false);
        let duplicate = request(&connection, payload, true);
        let (_, second) = pump(&mut session, duplicate);
        assert_eq!(first, second);
        assert_eq!(
            session
                .store()
                .conn()
                .query_row(
                    "SELECT count(*) FROM workshop_job WHERE kind='merge'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        unregister_run(session.store(), run);
    }

    #[test]
    fn plan_tokens_hide_and_refuse_writes_and_refusal_does_not_queue_work() {
        let (mut session, connection, run) = fixture(true);
        let list = request(
            &connection,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            true,
        );
        let (_, response) = pump(&mut session, list);
        assert!(response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true));
        let write = request(
            &connection,
            call(2, "workshop.chats.start", json!({"workspace_id":1})),
            true,
        );
        let (_, response) = pump(&mut session, write);
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            session
                .store()
                .conn()
                .query_row("SELECT count(*) FROM workshop_tool_call", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        unregister_run(session.store(), run);
        let (mut session, connection, run) = fixture(false);
        let pending = request(
            &connection,
            call(
                1,
                "workshop.chats.send",
                json!({"chat_id":2,"text":"must not send"}),
            ),
            true,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let id = loop {
            poll(&mut session);
            let id = session
                .store()
                .conn()
                .query_row(
                    "SELECT id FROM workshop_tool_call WHERE status='pending' LIMIT 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .ok();
            if let Some(id) = id {
                break id;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        session
            .store()
            .write(move |c| {
                c.execute(
                    "UPDATE workshop_tool_call SET status='refused' WHERE id=?",
                    [id],
                )?;
                Ok(())
            })
            .unwrap();
        let (_, response) = pump(&mut session, pending);
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            session
                .store()
                .conn()
                .query_row(
                    "SELECT count(*) FROM workshop_message WHERE chat_id=2",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        unregister_run(session.store(), run);
    }

    #[test]
    fn listener_does_not_own_the_store_or_bridge() {
        let bridge = Arc::new(Bridge::default());
        let weak = Arc::downgrade(&bridge);
        let _url = kernel::runtime::block_on(start_server(&bridge)).unwrap();
        drop(bridge);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn cancelled_notification_withdraws_pending_approval_without_running_the_tool() {
        let (mut session, connection, run) = fixture(false);
        let pending = request(
            &connection,
            call(1, "workshop.github.merge", json!({"workspace_id":1})),
            true,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let id = loop {
            poll(&mut session);
            let id = session
                .store()
                .conn()
                .query_row(
                    "SELECT id FROM workshop_tool_call WHERE status='pending' LIMIT 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .ok();
            if let Some(id) = id {
                break id;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        let cancel = request(
            &connection,
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
            true,
        );
        assert_eq!(pump(&mut session, cancel).0, 202);
        let (_, response) = pump(&mut session, pending);
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            session
                .store()
                .conn()
                .query_row(
                    "SELECT status FROM workshop_tool_call WHERE id=?",
                    [id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "interrupted"
        );
        assert_eq!(
            session
                .store()
                .conn()
                .query_row("SELECT count(*) FROM workshop_job", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        unregister_run(session.store(), run);
    }
    #[test]
    fn run_tokens_are_unique_and_do_not_contain_credentials() {
        let first = token().unwrap();
        let second = token().unwrap();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    #[test]
    fn unregister_fails_pending_requests_and_clears_only_the_ended_run() {
        let store = Store::open(None, &[], kernel::sync::Device::fake()).unwrap();
        let bridge = store.local::<Bridge>();
        let (answer, mut receive) = watch::channel(None);
        {
            let mut inner = bridge.inner.lock().unwrap();
            inner.tokens.insert(
                "one".into(),
                Context {
                    chat: 1,
                    run: 1,
                    read_only: false,
                },
            );
            inner.tokens.insert(
                "two".into(),
                Context {
                    chat: 2,
                    run: 2,
                    read_only: false,
                },
            );
            inner.cache.insert(
                (1, "1".into()),
                Cached {
                    input: "{}".into(),
                    answer,
                },
            );
        }
        let guard = guard_run(&store, 1);
        drop(guard);
        assert_eq!(bridge.inner.lock().unwrap().tokens.len(), 1);
        assert!(receive.borrow_and_update().as_ref().unwrap()["isError"]
            .as_bool()
            .unwrap());
    }
}
