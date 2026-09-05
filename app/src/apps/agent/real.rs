//! The actual gateway: one POST, one long streamed answer.
//!
//! Nothing here runs under a script — [`outside`](super::Agent::outside)
//! hands a scripted or virtual-clock world the fake instead — and nothing
//! here is proved by a test that opens a socket. What *is* proved is the
//! part with no network in it: the shape of a refusal, which is the only
//! thing about a failed request a person ever reads.
//!
//! The credentials are resolved per request rather than held: the token is
//! device sync's, a settings form may file a new one between two chats, and
//! a lookup of the keychain costs nothing beside the round trip that
//! follows it.

use std::path::PathBuf;
use std::sync::Mutex;

use kernel::app::Env;
use kernel::caps::{MemSecrets, Secrets, SecretsFactory};
use kernel::http::{self, Request};
use kernel::repl::r2;
use kernel::sse::SseReader;
use serde_json::Value;

use super::gateway::{request_parts, stream_completion, Failure, Flow, Gateway};
use super::wire::{ChatRequest, Chunk, Completion};
use super::{GATEWAY, PROVIDER};

/// How much of a refusal's body is worth reading. An endpoint that answers
/// a gigabyte to a bad account is not owed the memory.
const BODY_CAP: usize = 64 * 1024;

/// Where Cloudflare says whose a token is: the one call a device with no
/// bucket makes, to learn the account it cannot read off a host.
const ACCOUNTS_URL: &str = "https://api.cloudflare.com/client/v4/accounts?per_page=5";

/// The account Cloudflare named for this process's token, kept once it has
/// answered: an account does not change under a running app, and a round
/// trip per request would buy nothing.
static ACCOUNT: Mutex<Option<String>> = Mutex::new(None);

/// The gateway, for one world.
pub struct RealGateway {
    /// Where the `bucket` file is, which is where the account comes from.
    dir: Option<PathBuf>,
    /// The bucket the shell resolved, `--bucket` included — the first place
    /// an account is read off.
    bucket: Option<String>,
    /// The machine's own secret store, when the shell installed one.
    backend: Option<SecretsFactory>,
    /// The shared map otherwise — which is what a build with no platform
    /// store falls back to, and what a form's write-only field fills.
    memory: MemSecrets,
}

impl RealGateway {
    /// One for this world, out of what the shell put on the environment.
    #[must_use]
    pub fn new(env: &Env) -> RealGateway {
        RealGateway {
            dir: env.db_dir.clone(),
            bucket: env.bucket.clone(),
            backend: env.secrets_backend.clone(),
            memory: env.secrets.clone(),
        }
    }

    /// A secret store for one lookup, the way every other world gets one.
    fn secrets(&self) -> Box<dyn Secrets> {
        match &self.backend {
            Some(f) => f.make(),
            None => Box::new(self.memory.clone()),
        }
    }

    /// Whose account the request goes to: off the bucket's host when there
    /// is a bucket on R2, else the one Cloudflare names for the token — asked
    /// once per process, the answer kept.
    fn account(&self, token: &str) -> Result<String, String> {
        if let Some(a) = r2::account_from(self.bucket.as_deref(), self.dir.as_deref()) {
            return Ok(a);
        }
        if let Some(a) = ACCOUNT.lock().ok().and_then(|g| g.clone()) {
            return Ok(a);
        }
        let headers = [
            ("authorization", format!("Bearer {token}")),
            ("accept", "application/json".to_string()),
        ];
        let resp = http::send(&Request {
            method: "GET",
            url: ACCOUNTS_URL,
            headers: &headers,
            body: &[],
        })
        .map_err(|e| no_account(&e))?;
        let status = resp.status;
        let body = resp.text(BODY_CAP).map_err(|e| no_account(&e))?;
        let account = account_named(status, &body).map_err(|e| no_account(&e))?;
        if let Ok(mut kept) = ACCOUNT.lock() {
            *kept = Some(account.clone());
        }
        Ok(account)
    }
}

/// What a device with no bucket says when Cloudflare could not name the
/// account either: the reason, and the three places a bucket goes.
fn no_account(why: &str) -> String {
    format!(
        "no bucket to read the account off, and cloudflare could not say whose token \
         this is ({why}) — run with `--bucket URL`, set SUPERAPP_BUCKET, or put it on \
         line 1 of the `bucket` file"
    )
}

/// The one account a token opens, out of the API's answer to
/// `GET /accounts`: `{"success": true, "result": [{"id", "name"}, …]}`.
///
/// # Errors
///
/// A refusal, in the API's own words; a token that opens no account; and a
/// token that opens several, which is asked to name one by its bucket —
/// there is no guessing which of a person's accounts a chat should bill.
pub fn account_named(status: u16, body: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| format!("status {status}, and the answer was not JSON"))?;
    if status != 200 || v.get("success").and_then(Value::as_bool) != Some(true) {
        let said = v
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|e| e.first())
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("no reason given");
        return Err(format!("status {status}: {said}"));
    }
    let accounts: Vec<(String, String)> = v
        .get("result")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let id = x.get("id")?.as_str()?.to_string();
                    let name = x.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    Some((id, name))
                })
                .collect()
        })
        .unwrap_or_default();
    match accounts.as_slice() {
        [] => Err("the token opens no account".to_string()),
        [(id, _)] => Ok(id.clone()),
        many => Err(format!(
            "the token opens {} accounts ({}) — name one with \
             `--bucket https://<account>.r2.cloudflarestorage.com/…`",
            many.len(),
            many.iter()
                .map(|(_, n)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

impl Gateway for RealGateway {
    fn complete(
        &mut self,
        req: &ChatRequest,
        on: &mut dyn FnMut(&Chunk) -> Flow,
    ) -> Result<Completion, Failure> {
        let mut secrets = self.secrets();
        // The token out of the keychain, the account off the bucket's host or
        // out of Cloudflare's own answer: device sync's credentials, borne
        // whole. Every sentence this can fail with says where to put what is
        // missing, and the `gateway:` in front of it is what the problem
        // source keys on.
        let (_key_id, token) = r2::gateway_token(self.dir.as_deref(), &mut *secrets)
            .map_err(|e| Failure::new(format!("gateway: {e}")))?;
        let account = self
            .account(&token)
            .map_err(|e| Failure::new(format!("gateway: {e}")))?;
        let parts = request_parts(&PROVIDER, &account, GATEWAY, &token, req);
        let resp = http::send(&Request {
            method: "POST",
            url: &parts.url,
            headers: &parts.headers,
            body: &parts.body,
        })
        .map_err(|e| Failure::new(format!("gateway: {e}")))?;
        if resp.status != 200 {
            let status = resp.status;
            let body = resp
                .text(BODY_CAP)
                .map_err(|e| Failure::new(format!("gateway: {e}")))?;
            return Err(refused(status, body));
        }
        let mut events = SseReader::new(resp.body);
        stream_completion(std::iter::from_fn(|| events.next_event().transpose()), on)
    }
}

/// What a status that is not 200 comes to, in words.
///
/// The providers answer a refusal as `{"error": {"message": …}}` when they
/// answer JSON at all, Workers AI as `{"name": "AiError", "message": …}`,
/// and the gateway as an HTML page when the account in the URL is nobody's
/// — so the body is read every way, and what a person sees is the sentence
/// if there is one and the page if there is not.
///
/// A 401 is the *token*, which is a standing condition and not one run's
/// bad luck: it wears the `gateway: unauthorized` the problem source looks
/// for. A 403 is a refusal of something — a model the plan does not carry,
/// a gateway that wants its own header — and says so without guessing which.
#[must_use]
pub fn refused(status: u16, body: String) -> Failure {
    let said = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            let nested = v.get("error").and_then(|e| e.get("message"));
            nested
                .or_else(|| v.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or(body);
    let said = said.trim().to_string();
    let message = match status {
        401 => format!("gateway: unauthorized — {said}"),
        403 => format!("gateway: refused — {said}"),
        _ => said,
    };
    Failure {
        status: Some(status),
        message,
    }
}
