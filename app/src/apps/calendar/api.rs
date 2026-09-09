//! Google Calendar transport. All requests are effects; credentials never
//! enter payloads, the store, logs, panel context, or agent replies.
use super::{dates, edit, model};
use kernel::{
    app::Env,
    effect::{AsyncEffect as Effect, Ctx},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Request {
    pub email: String,
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Value,
    pub etag: String,
}
impl Request {
    pub fn get(email: &str, path: &str) -> Self {
        Self {
            email: email.into(),
            method: "GET".into(),
            path: path.into(),
            query: Vec::new(),
            body: Value::Null,
            etag: String::new(),
        }
    }
    pub fn query(mut self, k: &str, v: impl ToString) -> Self {
        self.query.push((k.into(), v.to_string()));
        self
    }
    pub fn write(mut self, method: &str, body: Value, etag: &str) -> Self {
        self.method = method.into();
        self.body = body;
        self.etag = etag.into();
        self
    }
}
#[async_trait::async_trait(?Send)]
impl Effect for Request {
    const KIND: &'static str = "calendar.http";
    type Reply = Value;
    fn describe(&self) -> String {
        format!(
            "{} Google Calendar {} for {}",
            self.method, self.path, self.email
        )
    }
    fn writes(&self) -> bool {
        self.method != "GET" && self.path != "/freeBusy"
    }
    async fn perform(&self, cx: &mut Ctx<'_>) -> Result<Value, String> {
        cx.cap::<dyn Api>()?.call(self).await
    }
}
#[async_trait::async_trait(?Send)]
pub trait Api {
    async fn call(&mut self, r: &Request) -> Result<Value, String>;
}
pub struct Http {
    agent: reqwest::Client,
    tokens: crate::identity::Tokens,
}
impl Http {
    pub fn new(env: &Env) -> Self {
        Self {
            agent: reqwest::Client::builder()
                .use_preconfigured_tls((*kernel::http::tls_config()).clone())
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .expect("Calendar HTTP client"),
            tokens: crate::identity::Tokens::new(env),
        }
    }
}
#[async_trait::async_trait(?Send)]
impl Api for Http {
    async fn call(&mut self, r: &Request) -> Result<Value, String> {
        let token = self.tokens.access(&r.email).await?;
        let mut url = url::Url::parse(&format!("https://www.googleapis.com/calendar/v3{}", r.path))
            .map_err(|e| e.to_string())?;
        url.query_pairs_mut().extend_pairs(&r.query);
        let method = reqwest::Method::from_bytes(r.method.as_bytes()).map_err(|e| e.to_string())?;
        let mut req = self
            .agent
            .request(method, url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json");
        if !r.etag.is_empty() {
            req = req.header("If-Match", &r.etag);
        }
        let body = if r.body.is_null() {
            String::new()
        } else {
            r.body.to_string()
        };
        let response = req.body(body).send().await.map_err(|_| {
            "Google Calendar could not be reached; retry this operation when connected".to_string()
        })?;
        let status = response.status().as_u16();
        let bytes = kernel::http::bounded_bytes(response, 16 << 20)
            .await
            .map_err(|_| "Google returned an unreadable response".to_string())?;
        let body = std::str::from_utf8(&bytes)
            .map_err(|_| "Google returned an unreadable response".to_string())?;
        if !(200..300).contains(&status) {
            let reason = match status {
                401 => "reconnect this Google account in Accounts",
                403 => "Calendar access was refused; check granted scopes and sharing permissions",
                404 => "event or calendar no longer exists",
                409 => "event ID already exists",
                410 => "event has been deleted",
                412 => "event changed on Google; reopen it to review the latest version",
                429 => "Google is limiting requests; retry later",
                _ => "Google Calendar request failed; retry later",
            };
            return Err(format!("HTTP {status}: {reason}"));
        }
        if body.is_empty() {
            Ok(json!({}))
        } else {
            serde_json::from_str(body).map_err(|_| "Google returned invalid JSON".into())
        }
    }
}
pub fn segment(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
pub fn events(calendar: &str) -> String {
    format!("/calendars/{}/events", segment(calendar))
}
pub fn event(calendar: &str, id: &str) -> String {
    format!("{}/{}", events(calendar), segment(id))
}
pub async fn pages(w: &kernel::effect::World, request: Request) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    let mut next = String::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        let mut req = request.clone();
        if !next.is_empty() {
            req = req.query("pageToken", &next);
        }
        let page = w.run_async(&req).await?;
        if let Some(rows) = page.get("items") {
            items.extend(
                rows.as_array()
                    .ok_or("Google returned an invalid items array")?
                    .iter()
                    .cloned(),
            );
        } else if !page.is_object() {
            return Err("Google returned an invalid list".into());
        }
        if items.len() > 100_000 {
            return Err(
                "calendar response exceeds the supported cache size; choose a narrower date range"
                    .into(),
            );
        }
        next = model::text(&page, "nextPageToken").into();
        if next.is_empty() {
            return Ok(items);
        }
        if !seen.insert(next.clone()) {
            return Err("Google repeated a page token".into());
        }
    }
}

/// Shared, deterministic fake with ETags, recurrence expansion and free/busy.
#[derive(Clone, Default)]
pub struct Fake {
    pub state: Arc<Mutex<HashMap<String, Value>>>,
}
impl Fake {
    pub fn seeded(now: f64) -> Self {
        let s = Self::default();
        {
            let mut state = s.state.lock().unwrap();
            for v in super::seed::events(now) {
                state.insert(model::text(&v, "id").into(), v);
            }
        }
        s
    }
}
#[async_trait::async_trait(?Send)]
impl Api for Fake {
    async fn call(&mut self, r: &Request) -> Result<Value, String> {
        if r.path == "/users/me/calendarList" {
            return Ok(
                json!({"items":[{"id":"primary","summary":"Work","timeZone":"Europe/Berlin","accessRole":"owner","conferenceProperties":{"allowedConferenceSolutionTypes":["hangoutsMeet"]}},{"id":"studio","summary":"Studio","timeZone":"Europe/Berlin","accessRole":"reader"}]}),
            );
        }
        if r.path == "/freeBusy" {
            let mut calendars = serde_json::Map::new();
            let state = self.state.lock().unwrap();
            let from = model::text(&r.body, "timeMin");
            let until = model::text(&r.body, "timeMax");
            for item in r.body["items"].as_array().into_iter().flatten() {
                let id = model::text(item, "id");
                let known = id == r.email
                    || id == "primary"
                    || id == "studio"
                    || ["nora@studio.example", "leo@studio.example"].contains(&id);
                let busy = state
                    .values()
                    .filter(|v| v["status"] != "cancelled" && v["transparency"] != "transparent")
                    .filter_map(|v| {
                        let (a, _) = dates::read(&v["start"], "Europe/Berlin").ok()?;
                        let (b, _) = dates::read(&v["end"], "Europe/Berlin").ok()?;
                        if a < dates::instant(until, "UTC").ok()?
                            && b > dates::instant(from, "UTC").ok()?
                        {
                            Some(json!({"start":dates::rfc(a),"end":dates::rfc(b)}))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                calendars.insert(
                    id.into(),
                    if known {
                        json!({"busy":busy})
                    } else {
                        json!({"errors":[{"reason":"notFound"}]})
                    },
                );
            }
            return Ok(json!({"calendars":calendars}));
        }
        let tail = r
            .path
            .split("/events")
            .nth(1)
            .ok_or("unknown Calendar API path")?;
        let mut state = self.state.lock().unwrap();
        if r.method == "GET" && tail.is_empty() {
            let get = |key: &str| {
                r.query
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("")
            };
            let start = dates::instant(get("timeMin"), "UTC").unwrap_or(0.0);
            let end = dates::instant(get("timeMax"), "UTC").unwrap_or(start + 366.0 * 86400.0);
            let studio = r.path.contains("/studio/");
            let mut items = Vec::new();
            for v in state.values().filter(|v| (v["id"] == "allhands") == studio) {
                if v["status"] == "cancelled" {
                    continue;
                }
                if let Some(rules) = v["recurrence"].as_array().filter(|a| !a.is_empty()) {
                    let f = edit::Form::from_event(v, "Europe/Berlin");
                    let (a, b) = f.bounds()?;
                    let rules = rules
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let set = edit::recurrence_set(&f.start, &f.zone, f.all_day, &rules)?
                        .after(dates::utc(start - 86400.0).with_timezone(&rrule::Tz::UTC))
                        .before(dates::utc(end).with_timezone(&rrule::Tz::UTC));
                    for dt in set.all(10000).dates {
                        let at = dt.timestamp() as f64;
                        let id = format!("{}_{}", model::text(v, "id"), at as i64);
                        if let Some(exception) = state.get(&id) {
                            if exception["status"] != "cancelled" {
                                items.push(exception.clone());
                            }
                            continue;
                        }
                        let mut instance = v.clone();
                        instance["id"] = json!(id);
                        instance["recurringEventId"] = v["id"].clone();
                        instance.as_object_mut().unwrap().remove("recurrence");
                        if f.all_day {
                            instance["start"] = json!({"date":dates::day(at,&f.zone)});
                            instance["end"] = json!({"date":dates::day(at+b-a,&f.zone)});
                        } else {
                            instance["start"] =
                                json!({"dateTime":dates::rfc(at),"timeZone":f.zone});
                            instance["end"] =
                                json!({"dateTime":dates::rfc(at+b-a),"timeZone":f.zone});
                        }
                        instance["originalStartTime"] = instance["start"].clone();
                        items.push(instance);
                    }
                } else if model::text(v, "recurringEventId").is_empty() {
                    let (a, _) = dates::read(&v["start"], "Europe/Berlin")?;
                    let (b, _) = dates::read(&v["end"], "Europe/Berlin")?;
                    if a < end && b > start {
                        items.push(v.clone());
                    }
                }
            }
            return Ok(json!({"items":items}));
        }
        let id = tail.trim_start_matches('/');
        // Expand one recurring instance on demand, matching the list's identity.
        if !id.is_empty() && !state.contains_key(id) {
            if let Some((master, at)) = id
                .rsplit_once('_')
                .and_then(|(m, t)| t.parse::<f64>().ok().map(|t| (m, t)))
            {
                if let Some(v) = state.get(master).cloned() {
                    let mut i = v.clone();
                    let f = edit::Form::from_event(&v, "Europe/Berlin");
                    let (a, b) = f.bounds()?;
                    i["id"] = json!(id);
                    i["recurringEventId"] = json!(master);
                    i["start"] = json!({"dateTime":dates::rfc(at),"timeZone":f.zone});
                    i["end"] = json!({"dateTime":dates::rfc(at+b-a),"timeZone":f.zone});
                    i["originalStartTime"] = i["start"].clone();
                    i.as_object_mut().unwrap().remove("recurrence");
                    state.insert(id.into(), i);
                }
            }
        }
        if r.method == "POST" && tail.is_empty() {
            let id = model::text(&r.body, "id");
            if state.contains_key(id) {
                return Err("HTTP 409: event ID already exists".into());
            }
            let mut v = r.body.clone();
            v["etag"] = json!("\"1\"");
            v["organizer"] = json!({"email":r.email,"self":true});
            conference(&mut v);
            state.insert(id.into(), v.clone());
            return Ok(v);
        }
        let v = state
            .get_mut(id)
            .ok_or("HTTP 404: event no longer exists")?;
        if r.method == "GET" {
            return Ok(v.clone());
        }
        if !r.etag.is_empty() && r.etag != model::text(v, "etag") {
            return Err("HTTP 412: event changed on Google; reopen it".into());
        }
        if r.method == "DELETE" {
            v["status"] = json!("cancelled");
            return Ok(json!({}));
        }
        if r.method == "PATCH" {
            let revision = model::text(v, "etag")
                .trim_matches('"')
                .parse::<u64>()
                .unwrap_or(0)
                + 1;
            for (k, b) in r.body.as_object().ok_or("invalid update")? {
                v[k] = b.clone();
            }
            v["etag"] = json!(format!("\"{revision}\""));
            conference(v);
            return Ok(v.clone());
        }
        Err("unsupported Calendar API operation".into())
    }
}
fn conference(v: &mut Value) {
    if v["conferenceData"]["createRequest"].is_object() {
        v["conferenceData"]["createRequest"]["status"] = json!({"statusCode":"success"});
        v["conferenceData"]["entryPoints"] =
            json!([{"entryPointType":"video","uri":"https://meet.google.com/demo-calendar"}]);
    }
}
