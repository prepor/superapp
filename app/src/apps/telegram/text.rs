//! Telegram text entities and a selectable HTML reading of message text.
//!
//! Entity offsets count UTF-16 code units, including in captions. Only links
//! become markup; everything the sender wrote is escaped. Code entities stay
//! literal. Only messages without entity metadata use local URL detection.

use std::ops::Range;

use linkify::{LinkFinder, LinkKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    pub offset: u32,
    pub length: u32,
    #[serde(rename = "type")]
    pub kind: EntityKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "@type")]
pub enum EntityKind {
    #[serde(rename = "textEntityTypeUrl")]
    Url,
    #[serde(rename = "textEntityTypeTextUrl")]
    TextUrl { url: String },
    #[serde(rename = "textEntityTypeEmailAddress")]
    EmailAddress,
    #[serde(rename = "textEntityTypeCode")]
    Code,
    #[serde(rename = "textEntityTypePre")]
    Pre,
    #[serde(rename = "textEntityTypePreCode")]
    PreCode,
    #[serde(other)]
    Other,
}

/// Read each entity independently, so a malformed one cannot hide its peers.
pub fn entities(value: &Value) -> Vec<Entity> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect()
}

fn byte_at_utf16(text: &str, offset: u32) -> Option<usize> {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += c.len_utf16() as u32;
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

impl Entity {
    fn range(&self, text: &str) -> Option<Range<usize>> {
        if self.length == 0 {
            return None;
        }
        let end = self.offset.checked_add(self.length)?;
        Some(byte_at_utf16(text, self.offset)?..byte_at_utf16(text, end)?)
    }
}

/// A link may open the browser, mail client or Telegram. Unqualified domains
/// use HTTPS; other schemes and whitespace in a destination remain inert.
pub fn destination(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let parsed = Url::parse(raw);
    // A bare domain followed by a port looks like a custom scheme to Url.
    let domain_port =
        parsed.as_ref().is_ok_and(|url| url.scheme().contains('.')) && !raw.contains("://");
    let qualified = if raw.starts_with("//") {
        format!("https:{raw}")
    } else if domain_port || matches!(parsed, Err(url::ParseError::RelativeUrlWithoutBase)) {
        format!("https://{raw}")
    } else {
        raw.to_string()
    };
    let parsed = Url::parse(&qualified).ok()?;
    match parsed.scheme() {
        "http" | "https" if parsed.host_str().is_some() => Some(qualified),
        "mailto" | "tg" if !parsed.path().is_empty() || parsed.host_str().is_some() => {
            Some(qualified)
        }
        _ => None,
    }
}

/// Received entities are authoritative, even when empty. Only `None` (older
/// cached text or a local fixture edit) permits conservative link detection.
pub fn html(text: &str, entities: Option<&[Entity]>) -> String {
    let mut out = String::new();
    let Some(entities) = entities else {
        plain(&mut out, text);
        return out;
    };
    let mut spans: Vec<_> = entities
        .iter()
        .filter_map(|entity| {
            if entity.kind == EntityKind::Other {
                return None;
            }
            let range = entity.range(text)?;
            let label = &text[range.clone()];
            let url = match &entity.kind {
                EntityKind::Url => destination(label),
                EntityKind::TextUrl { url } => destination(url),
                EntityKind::EmailAddress => destination(&format!("mailto:{label}")),
                _ => None,
            };
            Some((range, url))
        })
        .collect();
    spans.sort_by_key(|(range, _)| (range.start, range.end));

    let mut cursor = 0;
    for (range, url) in spans {
        if range.start < cursor {
            continue;
        }
        escape(&mut out, &text[cursor..range.start]);
        span(&mut out, &text[range.clone()], url.as_deref());
        cursor = range.end;
    }
    escape(&mut out, &text[cursor..]);
    out
}

fn plain(out: &mut String, text: &str) {
    // Require a URL scheme: arbitrary dotted text such as main.rs, Dr.Smith
    // and it.Then must not acquire a destination. Email detection stays on.
    let finder = LinkFinder::new();
    let mut cursor = 0;
    for link in finder.links(text) {
        escape(out, &text[cursor..link.start()]);
        let url = match link.kind() {
            LinkKind::Email => destination(&format!("mailto:{}", link.as_str())),
            _ => destination(link.as_str()),
        };
        span(out, link.as_str(), url.as_deref());
        cursor = link.end();
    }
    escape(out, &text[cursor..]);
}

fn span(out: &mut String, text: &str, url: Option<&str>) {
    // HtmlLink draws a single text node. A multiline label needs one anchor
    // per line, with the break outside it, so all its words stay visible.
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("<br/>");
        }
        if line.is_empty() {
            continue;
        }
        if let Some(url) = url {
            out.push_str("<a href=\"");
            escape(out, url);
            out.push_str("\">");
        }
        escape(out, line);
        if url.is_some() {
            out.push_str("</a>");
        }
    }
}

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push_str("<br/>"),
            // Numeric entities survive the HTML parser's whitespace folding
            // without changing the characters a selection copies.
            ' ' => out.push_str("&#32;"),
            '\t' => out.push_str("&#9;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(offset: u32, length: u32, url: &str) -> Entity {
        Entity {
            offset,
            length,
            kind: EntityKind::TextUrl { url: url.into() },
        }
    }

    #[test]
    fn unicode_offsets_and_multiline_labels_keep_all_the_text() {
        let text = "👋 Привет\nworld <&>";
        let out = html(text, Some(&[link(3, 12, "https://example.org/?a=1&b=2")]));
        assert_eq!(out, "👋&#32;<a href=\"https://example.org/?a=1&amp;b=2\">Привет</a><br/><a href=\"https://example.org/?a=1&amp;b=2\">world</a>&#32;&lt;&amp;&gt;");
    }

    #[test]
    fn fallback_requires_a_scheme_and_leaves_punctuation_outside() {
        let out = html(
            "See (https://example.org/a_(b)), www.example.org.\nt.me/example and me@example.org",
            None,
        );
        assert_eq!(out, "See&#32;(<a href=\"https://example.org/a_(b)\">https://example.org/a_(b)</a>),&#32;www.example.org.<br/>t.me/example&#32;and&#32;<a href=\"mailto:me@example.org\">me@example.org</a>");
        for text in ["main.rs", "notes.md", "report.pdf", "Dr.Smith", "it.Then"] {
            assert_eq!(html(text, None), text);
        }
    }

    #[test]
    fn received_entities_are_authoritative_including_an_empty_list() {
        let text =
            "docs main.rs notes.md report.pdf Dr.Smith it.Then https://example.org me@example.org";
        let mut escaped = String::new();
        escape(&mut escaped, text);
        assert_eq!(html(text, Some(&[])), escaped);
        assert_eq!(
            html(
                text,
                Some(&[Entity {
                    offset: 0,
                    length: 4,
                    kind: EntityKind::Other
                }])
            ),
            escaped
        );

        let out = html(text, Some(&[link(0, 4, "https://docs.example.org")]));
        assert_eq!(out.matches("<a ").count(), 1);
        assert_eq!(
            out,
            format!(
                "<a href=\"https://docs.example.org\">docs</a>{}",
                &escaped[4..]
            )
        );
    }

    #[test]
    fn server_marked_domains_email_and_labeled_links_still_open() {
        for text in [
            "example.org",
            "www.example.org",
            "t.me/telegram",
            "example.org:8080/notes",
            "main.rs",
        ] {
            let out = html(
                text,
                Some(&[Entity {
                    offset: 0,
                    length: text.len() as u32,
                    kind: EntityKind::Url,
                }]),
            );
            assert_eq!(out, format!("<a href=\"https://{text}\">{text}</a>"));
        }
        assert_eq!(
            html(
                "me@example.org",
                Some(&[Entity {
                    offset: 0,
                    length: 14,
                    kind: EntityKind::EmailAddress
                }])
            ),
            "<a href=\"mailto:me@example.org\">me@example.org</a>"
        );
        assert_eq!(
            html(
                "example.org",
                Some(&[
                    Entity {
                        offset: 0,
                        length: 11,
                        kind: EntityKind::Other
                    },
                    link(0, 11, "https://example.net"),
                ])
            ),
            "<a href=\"https://example.net\">example.org</a>"
        );
    }

    #[test]
    fn code_and_unsafe_destinations_stay_literal() {
        for kind in [EntityKind::Code, EntityKind::Pre, EntityKind::PreCode] {
            assert_eq!(
                html(
                    "https://example.org",
                    Some(&[Entity {
                        offset: 0,
                        length: 19,
                        kind
                    }])
                ),
                "https://example.org"
            );
        }
        for url in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,hello",
            "https://exa\nmple.org",
            "",
            "https://",
        ] {
            assert_eq!(destination(url), None, "{url}");
            assert_eq!(
                html("example.org", Some(&[link(0, 11, url)])),
                "example.org"
            );
        }
        assert_eq!(
            destination("tg://resolve?domain=telegram"),
            Some("tg://resolve?domain=telegram".into())
        );
        assert_eq!(
            html("<a href=\"bad\">  hi\tthere\n</a>", None),
            "&lt;a&#32;href=&quot;bad&quot;&gt;&#32;&#32;hi&#9;there<br/>&lt;/a&gt;"
        );
    }

    #[test]
    fn malformed_and_overlapping_entities_cannot_drop_text_or_panic() {
        let out = html(
            "👋 go!",
            Some(&[
                link(1, 1, "https://invalid.org"), // inside a surrogate pair
                link(2, u32::MAX, "https://invalid.org"),
                link(80, 2, "https://invalid.org"),
                link(3, 2, "https://example.org"),
                link(3, 3, "https://overlap.org"),
            ]),
        );
        assert_eq!(out, "👋&#32;<a href=\"https://example.org\">go</a>!");
        let decoded = entities(&json!([
            {"offset": -1, "length": 2, "type": {"@type": "textEntityTypeUrl"}},
            {"offset": 0, "length": 2, "type": {"@type": "textEntityTypeTextUrl", "url": "https://example.org"}}
        ]));
        assert_eq!(decoded, vec![link(0, 2, "https://example.org")]);
    }
}
