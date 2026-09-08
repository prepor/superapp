//! Subscription lists, including exports with unescaped attribute text.

use kernel::caps::{real_path, Disk};
use kernel::effect::{Ctx, Effect};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use std::collections::{HashMap, HashSet};

use super::parse::web_url;

pub const MAX_OPML: usize = 2 << 20;

#[derive(Debug)]
pub struct Subscription {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct Document {
    pub feeds: Vec<Subscription>,
    pub skipped: usize,
}

pub struct Read(pub String);

impl Effect for Read {
    const KIND: &'static str = "rss.read_opml";
    type Reply = Document;
    fn describe(&self) -> String {
        format!("read OPML {}", self.0)
    }
    fn writes(&self) -> bool {
        false
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Document, String> {
        let path = self.0.trim();
        if path.is_empty() {
            return Err("enter the path to an OPML file".into());
        }
        let bytes = cx
            .cap::<dyn Disk>()?
            .read_file(&real_path(path), MAX_OPML + 1)?;
        parse(&bytes)
    }
}

pub fn parse(bytes: &[u8]) -> Result<Document, String> {
    if bytes.len() > MAX_OPML {
        return Err("OPML file exceeds 2 MiB".into());
    }
    let src = std::str::from_utf8(bytes).map_err(|_| "OPML must be UTF-8 text")?;
    let src = src.trim_start_matches('\u{feff}');
    // Try the original first: a valid XML attribute may end in a literal
    // backslash. Only broken exports need their JSON-style quotes repaired.
    let doc = xml(src).or_else(|_| xml(&repair_attributes(src)))?;
    if doc.feeds.is_empty() {
        return Err("no usable feed URLs in this OPML file".into());
    }
    Ok(doc)
}

fn xml(src: &str) -> Result<Document, String> {
    let mut reader = Reader::from_str(src);
    let mut version = XmlVersion::Implicit1_0;
    let mut doc = Document::default();
    let mut urls = HashSet::new();
    // Whether this element is in the subscription body, outside comments.
    let mut stack = Vec::new();
    let mut root = false;
    let mut body = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("cannot read OPML: {e}"))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name = e.name();
                if stack.is_empty() {
                    if root || name.as_ref() != b"opml" {
                        return Err("expected an OPML document".into());
                    }
                    root = true;
                }
                if stack.len() >= 128 {
                    return Err("OPML folders are nested too deeply".into());
                }
                let attrs = e
                    .attributes()
                    .map(|a| {
                        let a = a.map_err(|e| format!("cannot read OPML attribute: {e}"))?;
                        let value = a
                            .decoded_and_normalized_value(version, reader.decoder())
                            .map_err(|e| format!("cannot read OPML attribute: {e}"))?;
                        Ok((
                            String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                            value.into_owned(),
                        ))
                    })
                    .collect::<Result<HashMap<_, _>, String>>()?;
                let is_body = stack.len() == 1 && name.as_ref() == b"body";
                body |= is_body;
                let active = (is_body || stack.last().copied().unwrap_or(false))
                    && !attrs
                        .get("isComment")
                        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
                if active && name.as_ref() == b"outline" {
                    if let Some(raw) = attrs.get("xmlUrl") {
                        match web_url(raw) {
                            Ok(url) if urls.insert(url.clone()) => {
                                let title = attrs
                                    .get("title")
                                    .filter(|s| !s.trim().is_empty())
                                    .or_else(|| attrs.get("text"))
                                    .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| super::model::initial_title(&url));
                                doc.feeds.push(Subscription { url, title });
                            }
                            _ => doc.skipped += 1,
                        }
                    }
                }
                if matches!(event, Event::Start(_)) {
                    stack.push(active);
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Decl(e) => {
                version = e
                    .xml_version()
                    .map_err(|e| format!("cannot read OPML: {e}"))?;
            }
            Event::DocType(_) => return Err("OPML document types are not supported".into()),
            Event::Text(e)
                if stack.is_empty() && !e.as_ref().iter().all(u8::is_ascii_whitespace) =>
            {
                return Err("expected an OPML document".into());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root || !body || !stack.is_empty() {
        return Err("expected a complete OPML document with a body".into());
    }
    Ok(doc)
}

/// Some readers export titles with `\"`, bare `&` and even `<` inside
/// attributes. Repair just those values; comments, CDATA and tag structure
/// stay intact and still go through the XML parser's structural checks.
fn repair_attributes(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    let mut tag = false;
    let mut quote = None;
    while !rest.is_empty() {
        if !tag {
            let special = [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>")]
                .into_iter()
                .find(|(start, _)| rest.starts_with(start));
            if let Some((_, end)) = special {
                let len = rest.find(end).map_or(rest.len(), |at| at + end.len());
                out.push_str(&rest[..len]);
                rest = &rest[len..];
                continue;
            }
        }
        let ch = rest.chars().next().unwrap();
        rest = &rest[ch.len_utf8()..];
        if let Some(q) = quote {
            if ch == '\\' && rest.starts_with(q) {
                out.push_str(if q == '"' { "&quot;" } else { "&apos;" });
                rest = &rest[1..];
                continue;
            }
            if ch == '<' {
                out.push_str("&lt;");
                continue;
            }
            if ch == '&' {
                let valid = rest
                    .as_bytes()
                    .iter()
                    .take(13)
                    .position(|b| *b == b';')
                    .is_some_and(|at| {
                        quick_xml::escape::unescape(&format!("&{}", &rest[..=at])).is_ok()
                    });
                if !valid {
                    out.push_str("&amp;");
                    continue;
                }
            }
            if ch == q {
                quote = None;
            }
        } else if tag {
            match ch {
                '\'' | '"' => quote = Some(ch),
                '>' => tag = false,
                _ => {}
            }
        } else if ch == '<' {
            tag = true;
        }
        out.push(ch);
    }
    out
}
