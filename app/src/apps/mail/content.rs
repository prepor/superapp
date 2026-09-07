//! The stored reading of a message, with remote references to its files.
//!
//! `message.raw` used to contain the entire RFC822 message. It now holds a
//! versioned snapshot: the MIME reading without attachment bodies, and the
//! descriptions and IMAP section numbers needed to retrieve those bodies.
//! Keeping the original reading lets HTML and recipient derivations run
//! again without downloading files or putting them in the replicated store.

use base64::Engine as _;
use mail_parser::{Message, MessageParser, PartType};
use serde::{Deserialize, Serialize};

use super::sync::Part;

const PREFIX: &[u8] = b"superapp-mail-1\n";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemotePart {
    pub part: Part,
    pub section: String,
    /// MIME headers, including the transfer encoding, for decoding a fetch.
    #[serde(with = "bytes")]
    pub headers: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct Content {
    #[serde(with = "bytes")]
    pub reading: Vec<u8>,
    pub parts: Vec<RemotePart>,
}

impl Content {
    /// Also accepts legacy RFC822, for the one-time conversion on open and
    /// for messages arriving from an older peer.
    pub fn read(raw: &[u8]) -> Result<Self, String> {
        if let Some(json) = raw.strip_prefix(PREFIX) {
            serde_json::from_slice(json).map_err(|e| format!("cannot read stored mail: {e}"))
        } else {
            Self::from_raw(raw)
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = PREFIX.to_vec();
        out.extend(serde_json::to_vec(self).expect("mail content serializes"));
        out
    }

    pub fn from_raw(raw: &[u8]) -> Result<Self, String> {
        let msg = MessageParser::default()
            .parse(raw)
            .ok_or("cannot parse message")?;
        let sections = sections(&msg);
        let mut ranges = Vec::new();
        let parts = super::sync::parts_of(&msg, None)
            .into_iter()
            .map(|part| {
                let p = &msg.parts[part.at as usize];
                ranges.push((p.offset_body as usize, p.offset_end as usize));
                RemotePart {
                    section: sections[part.at as usize].clone(),
                    headers: raw[p.offset_header as usize..p.offset_body as usize].to_vec(),
                    part,
                }
            })
            .collect();
        ranges.sort_unstable();
        let mut reading = Vec::new();
        let mut start = 0;
        for (body, end) in ranges {
            if body >= start {
                reading.extend_from_slice(&raw[start..body]);
                start = end;
            }
        }
        reading.extend_from_slice(&raw[start..]);
        Ok(Self { reading, parts })
    }
}

/// Compact at every ingest boundary, including fakes and legacy snapshots.
pub fn compact(raw: &[u8]) -> Result<Vec<u8>, String> {
    Content::read(raw).map(|c| c.encode())
}

/// MIME tree positions are IMAP section numbers, not parser part indices.
/// An attached message is one file; its own nested parts stay inside it.
pub fn sections(msg: &Message<'_>) -> Vec<String> {
    fn walk(msg: &Message<'_>, at: u32, path: &str, out: &mut [String]) {
        out[at as usize] =
            if path.is_empty() && !matches!(msg.parts[at as usize].body, PartType::Multipart(_)) {
                "1".into()
            } else {
                path.into()
            };
        if let PartType::Multipart(children) = &msg.parts[at as usize].body {
            for (i, child) in children.iter().enumerate() {
                let path = if path.is_empty() {
                    (i + 1).to_string()
                } else {
                    format!("{path}.{}", i + 1)
                };
                walk(msg, *child, &path, out);
            }
        }
    }
    let mut out = vec![String::new(); msg.parts.len()];
    if !out.is_empty() {
        walk(msg, 0, "", &mut out);
    }
    out
}

/// The original transfer-encoded body, for the fake server's section fetch.
pub fn section_bytes(raw: &[u8], section: &str) -> Option<Vec<u8>> {
    let msg = MessageParser::default().parse(raw)?;
    let at = sections(&msg).iter().position(|s| s == section)?;
    let p = &msg.parts[at];
    Some(
        raw.get(p.offset_body as usize..p.offset_end as usize)?
            .to_vec(),
    )
}

pub fn decode(headers: &[u8], body: &[u8]) -> Result<Vec<u8>, String> {
    use mail_parser::MimeHeaders;
    let msg = MessageParser::default()
        .parse_headers(headers)
        .ok_or("cannot decode attachment headers")?;
    // Decode only the transfer encoding. Parsing a text attachment as a
    // reading would transcode its charset and change the downloaded file.
    match msg.content_transfer_encoding().unwrap_or("7bit").to_ascii_lowercase().as_str() {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(body.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect::<Vec<_>>())
            .map_err(|_| "attachment has invalid base64 encoding".into()),
        "quoted-printable" => mail_parser::decoders::quoted_printable::quoted_printable_decode(body)
            .ok_or_else(|| "attachment has invalid quoted-printable encoding".into()),
        "7bit" | "8bit" | "binary" => Ok(body.to_vec()),
        other => Err(format!("unsupported attachment encoding: {other}")),
    }
}

/// A BODYSTRUCTURE turned into MIME headers and a list of reading sections.
/// No binary body or attached text is requested while mirroring a mailbox.
pub struct FetchPlan {
    root: Node,
    pub readings: Vec<String>,
}

struct Node {
    section: String,
    headers: Vec<u8>,
    boundary: String,
    children: Vec<Node>,
    size: u64,
}

impl FetchPlan {
    pub fn new(
        header: &[u8],
        structure: &imap_proto::types::BodyStructure<'_>,
    ) -> Result<Self, String> {
        let mut root = Node::new(structure, "");
        // Keep the envelope verbatim (including encoded words and charset).
        // BODYSTRUCTURE supplies the MIME headers used by the rebuilt tree.
        let mut headers = Vec::new();
        let mut keep = true;
        for line in header.split_inclusive(|b| *b == b'\n') {
            if line == b"\r\n" || line == b"\n" {
                continue;
            }
            if !matches!(line.first(), Some(b' ' | b'\t')) {
                keep = !line.split(|b| *b == b':').next().is_some_and(|name| {
                    name.len() >= 8 && name[..8].eq_ignore_ascii_case(b"content-")
                });
            }
            if keep {
                headers.extend_from_slice(line);
            }
        }
        if !headers.ends_with(b"\n") {
            headers.extend_from_slice(b"\r\n");
        }
        headers.extend_from_slice(&root.headers);
        root.headers = headers;
        let skeleton = root.render(&std::collections::HashMap::new());
        let msg = MessageParser::default()
            .parse(&skeleton)
            .ok_or("cannot parse MIME structure")?;
        let paths = sections(&msg);
        let readings = msg
            .parts
            .iter()
            .enumerate()
            .filter(|(at, p)| {
                matches!(p.body, PartType::Text(_) | PartType::Html(_))
                    && !msg.attachments.contains(&(*at as u32))
            })
            .map(|(at, _)| paths[at].clone())
            .collect();
        Ok(Self { root, readings })
    }

    pub fn finish(
        &self,
        bodies: &std::collections::HashMap<String, Vec<u8>>,
    ) -> Result<Vec<u8>, String> {
        for section in &self.readings {
            if !bodies.contains_key(section) {
                return Err(format!("server omitted body section {section}"));
            }
        }
        let raw = self.root.render(bodies);
        let mut content = Content::from_raw(&raw)?;
        for p in &mut content.parts {
            // BODYSTRUCTURE reports transfer-encoded octets. Until a file
            // is downloaded, use this conservative size estimate.
            p.part.size = self
                .root
                .size(&p.section)
                .ok_or("MIME section has no body structure")?;
        }
        Ok(content.encode())
    }
}

impl Node {
    fn new(body: &imap_proto::types::BodyStructure<'_>, path: &str) -> Self {
        use imap_proto::types::{BodyStructure, ContentEncoding};
        let (common, other, bodies) = match body {
            BodyStructure::Basic { common, other, .. }
            | BodyStructure::Text { common, other, .. }
            | BodyStructure::Message { common, other, .. } => (common, Some(other), &[][..]),
            BodyStructure::Multipart { common, bodies, .. } => (common, None, bodies.as_slice()),
        };
        let boundary = common
            .ty
            .params
            .as_ref()
            .and_then(|ps| ps.iter().find(|(k, _)| k.eq_ignore_ascii_case("boundary")))
            .map_or_else(|| format!("superapp-mime-{path}"), |(_, v)| v.to_string());
        let mut headers = format!("Content-Type: {}/{}", common.ty.ty, common.ty.subtype);
        parameters(&mut headers, &common.ty.params);
        if other.is_none()
            && !common
                .ty
                .params
                .as_ref()
                .is_some_and(|ps| ps.iter().any(|(k, _)| k.eq_ignore_ascii_case("boundary")))
        {
            headers += &format!("; boundary={}", quoted(&boundary));
        }
        headers += "\r\n";
        if let Some(d) = &common.disposition {
            headers += &format!("Content-Disposition: {}", d.ty);
            parameters(&mut headers, &d.params);
            headers += "\r\n";
        }
        let mut size = 0;
        if let Some(o) = other {
            let encoding = match &o.transfer_encoding {
                ContentEncoding::SevenBit => "7bit",
                ContentEncoding::EightBit => "8bit",
                ContentEncoding::Binary => "binary",
                ContentEncoding::Base64 => "base64",
                ContentEncoding::QuotedPrintable => "quoted-printable",
                ContentEncoding::Other(s) => s,
            };
            headers += &format!("Content-Transfer-Encoding: {encoding}\r\n");
            if let Some(id) = &o.id {
                headers += &format!("Content-ID: {id}\r\n");
            }
            size = if matches!(o.transfer_encoding, ContentEncoding::Base64) {
                u64::from(o.octets).div_ceil(4) * 3
            } else {
                u64::from(o.octets)
            };
        }
        headers += "\r\n";
        let children = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let child = if path.is_empty() {
                    (i + 1).to_string()
                } else {
                    format!("{path}.{}", i + 1)
                };
                Self::new(b, &child)
            })
            .collect();
        Self {
            section: if path.is_empty() {
                "1".into()
            } else {
                path.into()
            },
            headers: headers.into_bytes(),
            boundary,
            children,
            size,
        }
    }

    fn render(&self, bodies: &std::collections::HashMap<String, Vec<u8>>) -> Vec<u8> {
        let mut raw = self.headers.clone();
        if self.children.is_empty() {
            if let Some(body) = bodies.get(&self.section) {
                raw.extend_from_slice(body);
            } else {
                // A nonempty placeholder keeps header-only binary messages
                // classified as files by the MIME parser. It is stripped
                // when Content::from_raw builds the stored reading.
                raw.extend_from_slice(b"AA==");
            }
        } else {
            for child in &self.children {
                raw.extend_from_slice(format!("--{}\r\n", self.boundary).as_bytes());
                raw.extend(child.render(bodies));
                raw.extend_from_slice(b"\r\n");
            }
            raw.extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        }
        raw
    }

    fn size(&self, section: &str) -> Option<u64> {
        if self.children.is_empty() {
            (self.section == section).then_some(self.size)
        } else {
            self.children.iter().find_map(|c| c.size(section))
        }
    }
}

fn quoted(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\r', '\n'], " ")
    )
}

fn parameters(out: &mut String, params: &imap_proto::types::BodyParams<'_>) {
    if let Some(params) = params {
        for (key, val) in params {
            *out += &format!("; {key}={}", quoted(val));
        }
    }
}

// Base64 preserves the reading's original charset while keeping JSON small.
mod bytes {
    use super::*;
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    pub const NESTED: &str = "From: Vera <vera@example.org>\r\nTo: me@example.org\r\nSubject: files\r\nMessage-ID: <files@example.org>\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n\
--m\r\nContent-Type: multipart/related; boundary=r\r\n\r\n\
--r\r\nContent-Type: multipart/alternative; boundary=a\r\n\r\n\
--a\r\nContent-Type: text/plain\r\n\r\nhello\r\n\
--a\r\nContent-Type: text/html\r\n\r\n<p>hello</p><img src=\"cid:picture\">\r\n\
--a--\r\n\
--r\r\nContent-Type: image/png\r\nContent-ID: <picture>\r\nContent-Transfer-Encoding: base64\r\n\r\naW1hZ2U=\r\n\
--r--\r\n\
--m\r\nContent-Type: text/plain; name=note.txt\r\nContent-Disposition: attachment; filename=note.txt\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nprivate=20file\r\n\
--m\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=letter.eml\r\n\r\nFrom: nested@example.org\r\nSubject: inside\r\n\r\nprivate nested message\r\n\
--m--\r\n";

    #[test]
    fn stored_reading_keeps_text_and_remote_sections_without_file_bodies() {
        let c = Content::from_raw(NESTED.as_bytes()).unwrap();
        let sections: Vec<_> = c.parts.iter().map(|p| p.section.as_str()).collect();
        assert_eq!(sections, ["1.2", "2", "3"]);
        let reading = String::from_utf8_lossy(&c.reading);
        assert!(reading.contains("<p>hello</p>"));
        for file in [
            "aW1hZ2U=",
            "private=20file",
            "private nested message",
            "nested@example.org",
        ] {
            assert!(!reading.contains(file), "{reading}");
        }
        let stored = c.encode();
        assert_eq!(compact(&stored).unwrap(), stored);
        let parsed = super::super::sync::parse_mail(&stored);
        assert_eq!(parsed.body, "hello");
        assert_eq!(parsed.to, "me@example.org");
        assert_eq!(parsed.attachments.len(), 2, "the picture is inline");
        assert_eq!(parsed.attachments[0].name, "note.txt");
        assert_eq!(parsed.attachments[1].mime, "message/rfc822");
        for p in c.parts {
            let body = section_bytes(NESTED.as_bytes(), &p.section).unwrap();
            let downloaded = decode(&p.headers, &body).unwrap();
            assert_eq!(
                downloaded,
                super::super::sync::part_bytes(NESTED.as_bytes(), p.part.at).unwrap()
            );
        }
    }

    fn plan(structure: &str) -> FetchPlan {
        use imap_proto::types::{AttributeValue, Response};
        let response = format!("* 1 FETCH (BODYSTRUCTURE {structure})\r\n");
        let (_, response) = imap_proto::parser::parse_response(response.as_bytes()).unwrap();
        let Response::Fetch(_, attrs) = response else {
            panic!("fetch")
        };
        let AttributeValue::BodyStructure(body) = &attrs[0] else {
            panic!("structure")
        };
        FetchPlan::new(b"From: me@example.org\r\nTo: you@example.org\r\nSubject: parts\r\nContent-Type: ignored\r\n\r\n", body).unwrap()
    }

    #[test]
    fn bodystructure_fetches_readings_and_leaves_attached_text_and_images_remote() {
        let p = plan(concat!(
            "((((\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 5 1)",
            "(\"TEXT\" \"HTML\" NIL NIL NIL \"7BIT\" 36 1) \"ALTERNATIVE\")",
            "(\"IMAGE\" \"PNG\" NIL \"<picture>\" NIL \"BASE64\" 8) \"RELATED\")",
            "(\"TEXT\" \"PLAIN\" (\"NAME\" \"note.txt\") NIL NIL \"QUOTED-PRINTABLE\" 14 1 NIL (\"ATTACHMENT\" (\"FILENAME\" \"note.txt\"))) \"MIXED\")"
        ));
        assert_eq!(p.readings, ["1.1.1", "1.1.2"]);
        let bodies = HashMap::from([
            ("1.1.1".into(), b"hello".to_vec()),
            (
                "1.1.2".into(),
                b"<p>hello</p><img src=\"cid:picture\">".to_vec(),
            ),
        ]);
        let stored = p.finish(&bodies).unwrap();
        let c = Content::read(&stored).unwrap();
        assert_eq!(c.parts.len(), 2);
        assert_eq!(c.parts[0].section, "1.2");
        assert_eq!(c.parts[1].section, "2");
        assert_eq!(c.parts[1].part.name, "note.txt");
        let parsed = super::super::sync::parse_mail(&stored);
        assert_eq!(parsed.body, "hello");
        assert_eq!(parsed.attachments.len(), 1);
        assert!(
            p.finish(&HashMap::new()).is_err(),
            "a missing reading cannot be silently stored"
        );
    }

    #[test]
    fn a_single_binary_body_is_an_attachment_and_plain_text_is_a_reading() {
        let p = plan("(\"APPLICATION\" \"PDF\" NIL NIL NIL \"BASE64\" 12000)");
        assert!(p.readings.is_empty());
        let c = Content::read(&p.finish(&HashMap::new()).unwrap()).unwrap();
        assert_eq!(c.parts.len(), 1);
        assert_eq!(c.parts[0].section, "1");
        assert_eq!(c.parts[0].part.size, 9000);
        let p = plan("(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"ISO-8859-1\") NIL NIL \"8BIT\" 4 1)");
        assert_eq!(p.readings, ["1"]);
        let stored = p
            .finish(&HashMap::from([("1".into(), b"caf\xe9".to_vec())]))
            .unwrap();
        assert_eq!(super::super::sync::parse_mail(&stored).body, "café");
    }

    #[test]
    fn downloaded_text_files_keep_their_original_charset_and_line_endings() {
        let headers = b"Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n";
        assert_eq!(decode(headers, b"caf=E9\r\n").unwrap(), b"caf\xe9\r\n");
        let headers = b"Content-Type: application/octet-stream\r\nContent-Transfer-Encoding: base64\r\n\r\n";
        assert_eq!(decode(headers, b"aG Vs\r\nbG8=").unwrap(), b"hello");
        assert!(decode(headers, b"this is invalid!").is_err());
    }
}
