//! Turn RSS, Atom and JSON Feed into the same stored reading as mail.

use crate::reader::html;
use feed_rs::model::{Link, Person, Text};

#[derive(Clone, Debug)]
pub struct Article {
    pub guid: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub published: Option<f64>,
    pub html: String,
    pub raw: String,
    pub content_type: String,
    pub base_url: String,
}

#[derive(Clone, Debug)]
pub struct Feed {
    pub title: String,
    pub articles: Vec<Article>,
}

/// Only web addresses are feed sources or article destinations.
pub fn web_url(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "enter a full feed URL (https://…)")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("feed URLs must use http or https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("use a feed URL without embedded credentials".into());
    }
    Ok(url)
}

/// A subscription's unique key ignores fragments; article destinations do not.
pub fn feed_url(raw: &str) -> Result<String, String> {
    let mut url = web_url(raw)?;
    url.set_fragment(None);
    Ok(url.into())
}

fn link(links: &[Link]) -> Option<String> {
    links
        .iter()
        .filter(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        .filter(|l| {
            l.media_type
                .as_deref()
                .is_none_or(|t| t == "text/html" || t == "application/xhtml+xml")
        })
        .find_map(|l| web_url(&l.href).ok().map(String::from))
}

pub(super) fn reading(text: &str, kind: &str, base: &str) -> String {
    if matches!(kind, "text/html" | "application/xhtml+xml") {
        html::sanitize_with_base(text, Some(base))
    } else {
        html::from_text(text)
    }
}

fn title(text: Option<&Text>) -> String {
    text.map(|t| html::text_content(&reading(&t.content, t.content_type.as_ref(), "")))
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn authors(people: &[Person]) -> String {
    people
        .iter()
        .map(|p| {
            // RSS contacts put the element's role in `name` and the actual
            // author text in `email`; Atom puts the person's name in `name`.
            match (p.name.as_str(), p.email.as_deref()) {
                ("author" | "managingEditor" | "webMaster" | "", Some(contact)) => contact,
                (name, _) => name,
            }
        })
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn parse(bytes: &[u8], source: &str) -> Result<Feed, String> {
    let feed = feed_rs::parser::Builder::new()
        .base_uri(Some(source))
        .sanitize_content(false)
        // Missing IDs must remain stable across refreshes, including items
        // without a link. The parser's random fallback would duplicate them.
        .id_generator(|links, _, _| link(links).unwrap_or_default())
        .build()
        .parse(bytes)
        .map_err(|e| format!("cannot read this feed: {e}"))?;
    let feed_title = title(feed.title.as_ref());
    let feed_authors = authors(&feed.authors);
    let articles = feed
        .entries
        .into_iter()
        .map(|entry| {
            let url = link(&entry.links).unwrap_or_default();
            let base = if url.is_empty() { source } else { &url };
            let body = entry
                .content
                .as_ref()
                .and_then(|c| {
                    c.body
                        .as_ref()
                        .map(|b| (b.as_str(), c.content_type.to_string()))
                })
                .or_else(|| {
                    entry
                        .summary
                        .as_ref()
                        .map(|s| (s.content.as_str(), s.content_type.to_string()))
                });
            let (raw, content_type) = body
                .map(|(text, kind)| (text.to_string(), kind))
                .unwrap_or_else(|| (String::new(), "text/plain".into()));
            let html = reading(&raw, &content_type, base);
            let base_url = base.to_string();
            let name = title(entry.title.as_ref());
            let published = entry
                .published
                .or(entry.updated)
                .map(|d| d.timestamp() as f64);
            let guid = if entry.id.trim().is_empty() {
                // No publisher ID or permalink: distinguish even untitled
                // entries, without depending on the time of this refresh.
                let identity = format!("{name}\n{published:?}\n{html}");
                let digest = ring::digest::digest(&ring::digest::SHA256, identity.as_bytes());
                format!(
                    "content:{}",
                    digest
                        .as_ref()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                )
            } else {
                entry.id
            };
            Article {
                guid,
                title: if name.is_empty() {
                    "untitled article".into()
                } else {
                    name
                },
                author: if entry.authors.is_empty() {
                    feed_authors.clone()
                } else {
                    authors(&entry.authors)
                },
                published,
                url,
                html,
                raw,
                content_type,
                base_url,
            }
        })
        .collect();
    Ok(Feed {
        title: if feed_title.is_empty() {
            source.into()
        } else {
            feed_title
        },
        articles,
    })
}
