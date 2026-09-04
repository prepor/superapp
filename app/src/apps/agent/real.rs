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

/// The gateway, for one world.
pub struct RealGateway {
    /// Where the `bucket` file is, which is where the account comes from.
    dir: Option<PathBuf>,
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
}

impl Gateway for RealGateway {
    fn complete(
        &mut self,
        req: &ChatRequest,
        on: &mut dyn FnMut(&Chunk) -> Flow,
    ) -> Result<Completion, Failure> {
        let mut secrets = self.secrets();
        // The account off the bucket's host, the token out of the keychain:
        // device sync's own credentials, borne whole. Every sentence this
        // can fail with says where to put what is missing, and the `gateway:`
        // in front of it is what the problem source keys on.
        let creds = r2::gateway(self.dir.as_deref(), &mut *secrets)
            .map_err(|e| Failure::new(format!("gateway: {e}")))?;
        let parts = request_parts(&PROVIDER, &creds.account, GATEWAY, &creds.token, req);
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
/// answer JSON at all, and as an HTML page when the account in the URL is
/// nobody's — so the body is read either way, and what a person sees is
/// the sentence if there is one and the page if there is not.
///
/// A 401 or a 403 is the *token*, which is a standing condition and not one
/// run's bad luck: it wears the `gateway: unauthorized` the problem source
/// looks for.
#[must_use]
pub fn refused(status: u16, body: String) -> Failure {
    let said = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_string))
        .unwrap_or(body);
    let said = said.trim().to_string();
    let message = if status == 401 || status == 403 {
        format!("gateway: unauthorized — {said}")
    } else {
        said
    };
    Failure {
        status: Some(status),
        message,
    }
}
