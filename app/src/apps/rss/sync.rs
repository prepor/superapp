//! One background refresher for all subscriptions. Network reads are effects;
//! transactions begin only once a complete, parsed answer is in hand.

use kernel::app::{Wake, Worker};
use kernel::effect::{Ctx, Effect, Job, World};
use rusqlite::{params, OptionalExtension};
use std::time::Duration;
use ureq::ResponseExt;

use super::{model, parse, seed};

const INTERVAL: f64 = 15.0 * 60.0;
const STORE_RETRY: Duration = Duration::from_secs(30);
pub const MAX_FEED: u64 = 8 << 20;

#[derive(Clone, Debug)]
pub struct Request {
    pub id: i64,
    pub url: String,
    pub etag: String,
    pub modified: String,
}

#[derive(Clone, Debug)]
pub enum Response {
    Unchanged,
    Updated {
        url: String,
        bytes: Vec<u8>,
        etag: String,
        modified: String,
    },
}

pub trait Fetch {
    fn get(&mut self, request: &Request) -> Result<Response, String>;
}

pub struct Http(ureq::Agent);

impl Default for Http {
    fn default() -> Self {
        Self(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(30)))
                .max_redirects(5)
                .user_agent("superapp-rss/0.1")
                .build()
                .into(),
        )
    }
}

impl Fetch for Http {
    fn get(&mut self, r: &Request) -> Result<Response, String> {
        let url = parse::feed_url(&r.url)?;
        let mut req=self.0.get(&url).header("Accept","application/atom+xml, application/rss+xml, application/feed+json, application/xml, text/xml;q=0.9, */*;q=0.5");
        if !r.etag.is_empty() {
            req = req.header("If-None-Match", &r.etag);
        }
        if !r.modified.is_empty() {
            req = req.header("If-Modified-Since", &r.modified);
        }
        let mut response = req.call().map_err(|e| e.to_string())?;
        if response.status().as_u16() == 304 {
            return Ok(Response::Unchanged);
        }
        if !response.status().is_success() {
            return Err(format!("feed returned HTTP {}", response.status()));
        }
        let url = response.get_uri().to_string();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        let (etag, modified) = (header("etag"), header("last-modified"));
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_FEED)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        Ok(Response::Updated {
            url,
            bytes,
            etag,
            modified,
        })
    }
}

pub struct Fake;
impl Fetch for Fake {
    fn get(&mut self, r: &Request) -> Result<Response, String> {
        let bytes = seed::fixture(&r.url)
            .ok_or("demo feed not found")?
            .as_bytes()
            .to_vec();
        Ok(Response::Updated {
            url: r.url.clone(),
            bytes,
            etag: String::new(),
            modified: String::new(),
        })
    }
}

impl Effect for Request {
    const KIND: &'static str = "rss.fetch";
    type Reply = Response;
    fn describe(&self) -> String {
        format!("fetch {}", self.url)
    }
    fn writes(&self) -> bool {
        false
    }
    fn entity(&self) -> Option<String> {
        Some(format!("rss-feed:{}", self.id))
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Response, String> {
        cx.cap::<dyn Fetch>()?.get(self)
    }
}

/// One thread, database reader and HTTP connection pool for all feeds.
/// A worker per subscription exhausts macOS's 256-descriptor launch limit
/// after an OPML import, leaving other engines unable to open their sockets.
/// Each pass reads the next feed from the store, so the retained worker also
/// picks up newly added feeds. Session actions kick the whole worker set.
pub struct RefreshWorker;

impl Worker for RefreshWorker {
    fn name(&self) -> String {
        "rss-refresh".into()
    }
    fn claims(&self, _: &Job) -> bool {
        false
    }
    fn pass(&mut self, w: &World) -> Wake {
        let state = w.store().conn().query_row(
            "SELECT id,url,etag,modified,checked,requested,completed FROM rss_feed
             WHERE subscribed=1
             ORDER BY requested != completed DESC, checked, id LIMIT 1",
            [],
            |r| Ok((Request { id: r.get(0)?, url: r.get(1)?, etag: r.get(2)?, modified: r.get(3)? },
                r.get::<_, Option<f64>>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?))
        ).optional();
        let (request, checked, requested, completed) = match state {
            Ok(Some(state)) => state,
            Ok(None) => return Wake::OnKick,
            Err(_) => return Wake::After(STORE_RETRY),
        };
        let now = w.now();
        if requested == completed {
            if let Some(checked) = checked {
                let left = INTERVAL - (now - checked);
                if left > 0.0 {
                    return Wake::After(Duration::from_secs_f64(left.min(INTERVAL)));
                }
            }
        }
        let id = request.id;
        let result = w.run(&request).and_then(|r| match r {
            Response::Unchanged => Ok(None),
            Response::Updated {
                url,
                bytes,
                etag,
                modified,
            } => {
                if bytes.len() as u64 > MAX_FEED {
                    return Err("feed exceeds 8 MiB".into());
                }
                parse::parse(&bytes, &url).map(|feed| Some((feed, etag, modified)))
            }
        });
        let done = w.now();
        let saved = w.store().write(move |c| {
            // A removed subscription must not be revived by an in-flight response.
            let active = c
                .query_row("SELECT subscribed FROM rss_feed WHERE id=?", [id], |r| {
                    r.get::<_, bool>(0)
                })
                .optional()?
                .unwrap_or(false);
            if !active {
                return Ok(());
            }
            let error = match result {
                Ok(Some((feed, etag, modified))) => {
                    model::ingest(c, id, &feed, done)?;
                    c.execute(
                        "UPDATE rss_feed SET etag=?1,modified=?2 WHERE id=?3",
                        params![etag, modified, id],
                    )?;
                    String::new()
                }
                Ok(None) => String::new(),
                Err(why) => why,
            };
            c.execute(
                "UPDATE rss_feed SET checked=?1,error=?2,completed=?3 WHERE id=?4",
                params![done, error, requested, id],
            )?;
            Ok(())
        });
        // Hand the thread back between feeds so it can retire promptly.
        // The next pass either fetches another due feed or sleeps until the
        // earliest deadline. A failed write must not cause a hot retry loop.
        Wake::After(if saved.is_ok() { Duration::ZERO } else { STORE_RETRY })
    }
}
