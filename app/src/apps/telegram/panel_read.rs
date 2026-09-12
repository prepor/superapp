//! Short-lived, cancelable reads owned by a panel, rather than stored data.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use kernel::store::Store;
use serde_json::Value;

type Response = Mutex<Option<Result<Value, String>>>;
type Reply = Arc<Response>;

#[derive(Default)]
pub(super) struct Reads {
    next: u64,
    pending: HashMap<u64, Weak<Response>>,
}

impl Reads {
    fn register(&mut self, reply: &Reply) -> u64 {
        self.pending.retain(|_, reply| reply.strong_count() > 0);
        self.next += 1;
        self.pending.insert(self.next, Arc::downgrade(reply));
        self.next
    }

    pub fn alive(&self, id: u64) -> bool {
        self.pending.get(&id).is_some_and(|reply| reply.strong_count() > 0)
    }

    pub fn finish(&mut self, id: u64, result: Result<Value, String>) {
        if let Some(reply) = self.pending.remove(&id).and_then(|reply| reply.upgrade()) {
            *reply.lock().unwrap() = Some(result);
        }
    }

    pub fn disconnect(&mut self) {
        for (_, reply) in self.pending.drain() {
            if let Some(reply) = reply.upgrade() {
                *reply.lock().unwrap() = Some(Err("Telegram is disconnected".into()));
            }
        }
    }
}

pub(super) fn id(context: &str) -> Option<u64> {
    context.strip_prefix("panel_read:")?.parse().ok()
}

pub(super) struct Read {
    reply: Reply,
    due: f64,
    id: u64,
    runtime: Weak<super::runtime::Runtime>,
}

impl Read {
    pub fn start(store: &Store, request: &str, now: f64) -> Self {
        let reply = Arc::new(Mutex::new(None));
        let rt = super::runtime::of(store);
        let id = rt.reads.lock().unwrap().register(&reply);
        let mut request: Value = serde_json::from_str(request).expect("panel read request");
        request["@extra"] = Value::String(format!("panel_read:{id}"));
        if !super::panels::wire(store, &request.to_string()) {
            rt.reads.lock().unwrap().finish(id, Err("Telegram is disconnected".into()));
        }
        Self { reply, due: now + 30.0, id, runtime: Arc::downgrade(&rt) }
    }

    pub fn poll(&self, now: f64) -> Option<Result<Value, String>> {
        self.reply.lock().unwrap().take().or_else(||
            (now >= self.due).then(|| Err("Telegram did not answer. Try again.".into())))
    }
}

impl Drop for Read {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.reads.lock().unwrap().pending.remove(&self.id);
            runtime.operations.retire_context(&format!("panel_read:{}", self.id));
        }
    }
}
