//! A page's Markdown: the document with its frontmatter, the links a body
//! names, and the HTML the shared `Html` widget draws it as.
//!
//! One scan finds every link a body has — inline links and images by
//! pulldown's own offsets, `[[wikilinks]]` by a pass over the text that
//! skips whatever pulldown says is code — and the reader, the link rows
//! and a rename all read that one scan, so what is drawn, what is derived
//! and what is rewritten can never disagree about where a link is.
//!
//! Inside the markup an internal link is `kb:page/<slug>` or
//! `kb:file/<path>`, the form CR-022 names; a picture whose source is a
//! file's path is `cid:kb/<hash>`, filed with the reader's pictures before
//! the draw; a link to nothing is a `<dangling>` element, a muted run.

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;
use std::ops::Range;

use super::model::Target;
use crate::shell::widgets::source_input::{Span, Style};

const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_STRIKETHROUGH);

// -- the document ----------------------------------------------------------------

/// What a page's frontmatter says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Front {
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub slug: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    /// Keys the columns do not know, as a JSON object of key → the value's
    /// text exactly as it was written after the colon — a nested map or a
    /// block list included, with its newlines — so it goes back out
    /// byte for byte.
    pub extra: String,
}

impl Default for Front {
    /// A block that says nothing: no known key, and `extra` the empty
    /// object the column defaults to.
    fn default() -> Front {
        Front {
            kind: String::new(),
            title: String::new(),
            summary: String::new(),
            slug: String::new(),
            aliases: Vec::new(),
            tags: Vec::new(),
            extra: "{}".to_string(),
        }
    }
}

/// A line of the frontmatter, `\r` shed.
fn lines(block: &str) -> impl Iterator<Item = &str> {
    block.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// Whether a line is the delimiter: `---` alone on it.
fn is_delimiter(line: &str) -> bool {
    line.strip_suffix('\r').unwrap_or(line) == "---"
}

/// `key: value` where the key starts the line: a top-level entry.
fn top_key(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(':')?;
    let ok = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then_some((key, rest))
}

/// A quoted scalar, its quotes shed; anything else trimmed.
fn scalar(raw: &str) -> String {
    let v = raw.trim();
    if v.len() >= 2 {
        let (first, last) = (v.as_bytes()[0], v.as_bytes()[v.len() - 1]);
        if first == last && (first == b'"' || first == b'\'') {
            let inner = &v[1..v.len() - 1];
            return if first == b'"' { inner.replace("\\\"", "\"") } else { inner.replace("''", "'") };
        }
    }
    v.to_string()
}

/// A flow list `[a, "b, c", 'd']`: commas outside quotes split it.
fn flow_list(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut item = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                item.push(c);
            }
            Some(_) => item.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                item.push(c);
            }
            None if c == ',' => out.push(std::mem::take(&mut item)),
            None => item.push(c),
        }
    }
    out.push(item);
    out.into_iter().map(|s| scalar(&s)).filter(|s| !s.is_empty()).collect()
}

/// A list: a flow list on the line, or block items on the lines under it.
fn list(raw: &str) -> Vec<String> {
    let mut it = lines(raw);
    let head = it.next().unwrap_or("").trim();
    if let Some(inner) = head.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return flow_list(inner);
    }
    let mut out = Vec::new();
    if !head.is_empty() {
        out.push(scalar(head));
    }
    for l in it {
        if let Some(item) = l.trim_start().strip_prefix('-') {
            let item = scalar(item);
            if !item.is_empty() {
                out.push(item);
            }
        }
    }
    out
}

/// Splits a document into its frontmatter and its body. The block is the
/// lines between two `---` lines at the top; a document without one, or
/// with an unclosed one, is all body. An empty block is a frontmatter that
/// says nothing.
#[must_use]
pub fn parse_document(doc: &str) -> (Front, String) {
    let mut front = Front::default();
    let text = doc.strip_prefix('\u{feff}').unwrap_or(doc);
    let Some((first, rest)) = text.split_once('\n') else {
        return (front, text.to_string());
    };
    if !is_delimiter(first) {
        return (front, text.to_string());
    }
    // The closing line: `---` on a line of its own.
    let mut close: Option<(usize, usize)> = None;
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if is_delimiter(bare) {
            close = Some((at, at + line.len()));
            break;
        }
        at += line.len();
    }
    let Some((block_end, body_start)) = close else {
        return (front, text.to_string());
    };
    let block = rest[..block_end].strip_suffix('\n').unwrap_or(&rest[..block_end]);
    let block = block.strip_suffix('\r').unwrap_or(block);
    let mut body = &rest[body_start..];
    // One blank line after the block is the block's, not the body's.
    if let Some(b) = body.strip_prefix("\r\n").or_else(|| body.strip_prefix('\n')) {
        body = b;
    }

    // Entries: a top-level key and everything up to the next one.
    let mut entries: Vec<(String, String)> = Vec::new();
    for line in lines(block) {
        match top_key(line) {
            Some((key, rest)) if !line.starts_with([' ', '\t', '-']) => {
                entries.push((key.to_string(), rest.to_string()));
            }
            _ => {
                if let Some((_, raw)) = entries.last_mut() {
                    raw.push('\n');
                    raw.push_str(line);
                }
            }
        }
    }
    let mut extra: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (key, raw) in entries {
        match key.as_str() {
            "type" => front.kind = scalar(&raw),
            "title" => front.title = scalar(&raw),
            "summary" => front.summary = scalar(&raw),
            "slug" => front.slug = scalar(&raw),
            "aliases" => front.aliases = list(&raw),
            "tags" => front.tags = list(&raw),
            _ => {
                extra.insert(key, serde_json::Value::String(raw));
            }
        }
    }
    front.extra = if extra.is_empty() {
        "{}".to_string()
    } else {
        serde_json::to_string(&extra).unwrap_or_else(|_| "{}".to_string())
    };
    (front, body.to_string())
}

/// A scalar as the block spells it: quoted when it would otherwise read
/// as something else.
fn quoted(s: &str) -> String {
    let plain = !s.is_empty()
        && !s.contains(['"', '\'', '[', ']', ',', '#', '\n'])
        && !s.contains(": ")
        && !s.ends_with(':')
        && s.trim() == s;
    if plain {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('"', "\\\""))
    }
}

fn list_text(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|i| quoted(i)).collect::<Vec<_>>().join(", "))
}

/// A page's document: the frontmatter block the folder's rules spell,
/// then the body — what the editor shows and what `kb.write` takes. The
/// known keys are spelled the app's way; every other key goes back out as
/// it came in.
#[must_use]
pub fn document(front: &Front, body: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("type: {}\n", front.kind));
    out.push_str(&format!("title: {}\n", quoted(&front.title)));
    if !front.summary.is_empty() {
        out.push_str(&format!("summary: {}\n", quoted(&front.summary)));
    }
    if !front.aliases.is_empty() {
        out.push_str(&format!("aliases: {}\n", list_text(&front.aliases)));
    }
    if !front.tags.is_empty() {
        out.push_str(&format!("tags: {}\n", list_text(&front.tags)));
    }
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&front.extra) {
        for (k, v) in map {
            let raw = v.as_str().map_or_else(|| format!(" {v}"), str::to_string);
            out.push_str(&format!("{k}:{raw}\n"));
        }
    }
    out.push_str("---\n\n");
    out.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// The kebab-case word a title becomes.
#[must_use]
pub fn slug_of(title: &str) -> String {
    let mut out = String::new();
    let mut dash = true;
    for c in title.chars() {
        let c = c.to_lowercase().next().unwrap_or(c);
        if c.is_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

// -- links -----------------------------------------------------------------------

/// Whether a link's destination is somewhere on the web.
fn external(dest: &str) -> bool {
    let lc = dest.to_ascii_lowercase();
    lc.starts_with("http://") || lc.starts_with("https://") || lc.starts_with("mailto:")
}

/// What kind of link a body wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Wiki,
    Md,
    File,
    Image,
}

impl LinkKind {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            LinkKind::Wiki => "wiki",
            LinkKind::Md => "md",
            LinkKind::File => "file",
            LinkKind::Image => "image",
        }
    }

    #[must_use]
    pub fn of(word: &str) -> LinkKind {
        match word {
            "wiki" => LinkKind::Wiki,
            "md" => LinkKind::Md,
            "image" => LinkKind::Image,
            _ => LinkKind::File,
        }
    }
}

/// One link in a body: where it is, where its destination's text is, and
/// what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The whole link in the source.
    pub range: Range<usize>,
    /// The destination's own characters in the source, where a rewrite
    /// can put a new one: a wikilink's word, an inline link's address.
    /// `None` for a link whose address is not in the source at that place
    /// (a reference link, an autolink).
    pub dest: Option<Range<usize>>,
    /// The destination as written, trimmed.
    pub target: String,
    pub kind: LinkKind,
}

/// Percent-encodes a wikilink's word into a destination Markdown accepts:
/// spaces and brackets would end it early.
fn encode(target: &str) -> String {
    let mut out = String::new();
    for b in target.bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The reverse, for what [`encode`] made.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Every link a body has, in source order: pulldown's inline links and
/// images, then the `[[wikilinks]]` found in the text between them — not
/// inside a code span, a fence of any length or kind, or an inline link.
#[must_use]
pub fn scan(body: &str) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    // Where nothing may be a wikilink: code, and the inline links found.
    let mut closed: Vec<Range<usize>> = Vec::new();
    let mut block: Option<usize> = None;
    for (event, range) in Parser::new_ext(body, OPTIONS).into_offset_iter() {
        match event {
            Event::Code(_) | Event::Html(_) | Event::InlineHtml(_) => closed.push(range),
            Event::Start(Tag::CodeBlock(_)) => block = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                closed.push(block.take().unwrap_or(range.start)..range.end);
            }
            Event::Start(Tag::Link { link_type, dest_url, .. }) => {
                let dest = dest_url.to_string();
                closed.push(range.clone());
                if external(&dest) || dest.starts_with('#') || dest.is_empty() {
                    continue;
                }
                let kind = if dest.ends_with(".md") { LinkKind::Md } else { LinkKind::File };
                let at = (link_type == LinkType::Inline).then(|| dest_in(body, &range)).flatten();
                out.push(Found { range, dest: at, target: dest, kind });
            }
            Event::Start(Tag::Image { link_type, dest_url, .. }) => {
                let dest = dest_url.to_string();
                closed.push(range.clone());
                if external(&dest) || dest.is_empty() {
                    continue;
                }
                let at = (link_type == LinkType::Inline).then(|| dest_in(body, &range)).flatten();
                out.push(Found { range, dest: at, target: dest, kind: LinkKind::Image });
            }
            _ => {}
        }
    }
    let inside = |i: usize| closed.iter().any(|r| r.contains(&i));
    let mut rest = 0;
    while let Some(start) = body[rest..].find("[[") {
        let s = rest + start;
        let Some(len) = body[s + 2..].find("]]") else {
            break;
        };
        let end = s + 2 + len + 2;
        rest = s + 2;
        if inside(s) || body[s + 2..s + 2 + len].contains('\n') {
            continue;
        }
        let inner = &body[s + 2..s + 2 + len];
        let word = inner.split_once('|').map_or(inner, |(t, _)| t);
        let lead = word.len() - word.trim_start().len();
        let trimmed = word.trim();
        if trimmed.is_empty() {
            continue;
        }
        let dest_start = s + 2 + lead;
        out.push(Found {
            range: s..end,
            dest: Some(dest_start..dest_start + trimmed.len()),
            target: trimmed.to_string(),
            kind: LinkKind::Wiki,
        });
        rest = end;
    }
    out.sort_by_key(|f| f.range.start);
    out
}

/// Where an inline link's address sits inside its source: after the last
/// `](`, past any space and an opening `<`, up to a space, a `>` or the
/// closing paren.
fn dest_in(body: &str, range: &Range<usize>) -> Option<Range<usize>> {
    let src = &body[range.clone()];
    let open = src.rfind("](")? + 2;
    let mut start = open;
    while start < src.len() && src.as_bytes()[start] == b' ' {
        start += 1;
    }
    let angled = src.as_bytes().get(start) == Some(&b'<');
    if angled {
        start += 1;
    }
    let mut end = start;
    while end < src.len() {
        let b = src.as_bytes()[end];
        if angled && b == b'>' {
            break;
        }
        if !angled && (b == b' ' || b == b')' || b == b'\t' || b == b'\n') {
            break;
        }
        end += 1;
    }
    (end > start).then_some(range.start + start..range.start + end)
}

/// Every link a body names, as written, with its kind, each once.
#[must_use]
pub fn links(body: &str) -> Vec<(String, LinkKind)> {
    let mut out: Vec<(String, LinkKind)> = Vec::new();
    for f in scan(body) {
        if !out.iter().any(|(t, k)| *t == f.target && *k == f.kind) {
            out.push((f.target, f.kind));
        }
    }
    out
}

/// A body with every link whose destination `names` the old page spelling
/// `new` there instead — the word of a wikilink, the address of an inline
/// link with its `.md` kept — and nothing else touched: code, fences, the
/// link's own text and title stay as they were.
#[must_use]
pub fn rename_links(body: &str, names: &dyn Fn(&str, LinkKind) -> bool, new: &str) -> String {
    let mut out = body.to_string();
    let found = scan(body);
    for f in found.iter().rev() {
        let Some(dest) = &f.dest else {
            continue;
        };
        if !names(&f.target, f.kind) {
            continue;
        }
        let written = &body[dest.clone()];
        let replacement = match f.kind {
            LinkKind::Wiki => new.to_string(),
            _ if written.ends_with(".md") => format!("{new}.md"),
            _ => new.to_string(),
        };
        out.replace_range(dest.clone(), &replacement);
    }
    out
}

// -- HTML ------------------------------------------------------------------------

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

/// The `href` an internal target draws with.
#[must_use]
pub fn href_of(target: &Target) -> Option<String> {
    match target {
        Target::Page { slug, .. } => Some(format!("kb:page/{slug}")),
        Target::File { path, .. } => Some(format!("kb:file/{path}")),
        Target::Dangling(_) => None,
    }
}

/// Where an internal `href` goes: the page's slug or the file's path.
#[must_use]
pub fn route(href: &str) -> Option<(&'static str, String)> {
    if let Some(slug) = href.strip_prefix("kb:page/") {
        return Some(("page", slug.to_string()));
    }
    if let Some(path) = href.strip_prefix("kb:file/") {
        return Some(("file", path.to_string()));
    }
    None
}

/// The body as HTML for the shared `Html` widget. `resolve` says what a
/// wikilink, a relative link or a picture's path names.
pub fn html(body: &str, resolve: &dyn Fn(&str, LinkKind) -> Target) -> String {
    // The wikilink pass: each becomes an inline link on a `wiki:` address,
    // its word percent-encoded so a space or a bracket in it survives the
    // parse, and the walk below sees one kind of link.
    let mut prepared = String::with_capacity(body.len());
    let mut at = 0;
    for f in scan(body).into_iter().filter(|f| f.kind == LinkKind::Wiki) {
        let inner = &body[f.range.start + 2..f.range.end - 2];
        let text = inner.split_once('|').map_or(f.target.as_str(), |(_, t)| t.trim());
        prepared.push_str(&body[at..f.range.start]);
        prepared.push_str(&format!("[{}](wiki:{})", text.replace(']', "\\]"), encode(&f.target)));
        at = f.range.end;
    }
    prepared.push_str(&body[at..]);

    let mut out = String::new();
    let mut open_link: Option<Option<String>> = None;
    let mut image: Option<(Option<String>, String)> = None;
    for event in Parser::new_ext(&prepared, OPTIONS) {
        if let Some((_, alt)) = &mut image {
            match event {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => {
                    let (src, alt) = image.take().unwrap_or_default();
                    match src {
                        Some(src) => {
                            out.push_str("<img src=\"");
                            escape(&mut out, &src);
                            out.push_str("\" alt=\"");
                            escape(&mut out, &alt);
                            out.push_str("\"/>");
                        }
                        None => {
                            out.push_str("<dangling>");
                            escape(&mut out, if alt.is_empty() { "a picture that is not here" } else { &alt });
                            out.push_str("</dangling>");
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                let dest = dest_url.to_string();
                let href = if external(&dest) {
                    Some(dest)
                } else if let Some(word) = dest.strip_prefix("wiki:") {
                    href_of(&resolve(&decode(word), LinkKind::Wiki))
                } else {
                    let kind = if dest.ends_with(".md") { LinkKind::Md } else { LinkKind::File };
                    href_of(&resolve(&dest, kind))
                };
                match &href {
                    Some(h) => {
                        out.push_str("<a href=\"");
                        escape(&mut out, h);
                        out.push_str("\">");
                    }
                    None => out.push_str("<dangling>"),
                }
                open_link = Some(href);
            }
            Event::End(TagEnd::Link) => match open_link.take() {
                Some(Some(_)) => out.push_str("</a>"),
                _ => out.push_str("</dangling>"),
            },
            Event::Start(Tag::Image { dest_url, .. }) => {
                let dest = dest_url.to_string();
                let src = if external(&dest) {
                    Some(dest)
                } else {
                    match resolve(&dest, LinkKind::Image) {
                        Target::File { hash, .. } => Some(format!("cid:kb/{hash}")),
                        _ => None,
                    }
                };
                image = Some((src, String::new()));
            }
            Event::Text(text) => escape(&mut out, &text),
            Event::Html(text) | Event::InlineHtml(text) => escape(&mut out, &text),
            Event::Code(text) => {
                out.push_str("<code>");
                escape(&mut out, &text);
                out.push_str("</code>");
            }
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push_str("<br/>"),
            Event::Start(Tag::Paragraph) => out.push_str("<p>"),
            Event::End(TagEnd::Paragraph) => out.push_str("</p>"),
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            Event::Start(Tag::Strikethrough) => out.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</s>"),
            Event::Start(Tag::Heading { level, .. }) => {
                out.push_str(if (level as u8) <= 2 { "<h3>" } else { "<h4>" });
            }
            Event::End(TagEnd::Heading(level)) => {
                out.push_str(if (level as u8) <= 2 { "</h3>" } else { "</h4>" });
            }
            Event::Start(Tag::CodeBlock(_)) => out.push_str("<pre>"),
            Event::End(TagEnd::CodeBlock) => out.push_str("</pre>"),
            Event::Start(Tag::BlockQuote(_)) => out.push_str("<blockquote>"),
            Event::End(TagEnd::BlockQuote(_)) => out.push_str("</blockquote>"),
            Event::Start(Tag::List(Some(start))) => out.push_str(&format!("<ol start=\"{start}\">")),
            Event::Start(Tag::List(None)) => out.push_str("<ul>"),
            Event::End(TagEnd::List(true)) => out.push_str("</ol>"),
            Event::End(TagEnd::List(false)) => out.push_str("</ul>"),
            Event::Start(Tag::Item) => out.push_str("<li>"),
            Event::End(TagEnd::Item) => out.push_str("</li>"),
            Event::TaskListMarker(done) => out.push_str(if done { "\u{2611} " } else { "\u{2610} " }),
            Event::Start(Tag::Table(_)) => out.push_str("<table>"),
            Event::End(TagEnd::Table) => out.push_str("</table>"),
            Event::Start(Tag::TableHead) => out.push_str("<thead><tr>"),
            Event::End(TagEnd::TableHead) => out.push_str("</tr></thead>"),
            Event::Start(Tag::TableRow) => out.push_str("<tr>"),
            Event::End(TagEnd::TableRow) => out.push_str("</tr>"),
            Event::Start(Tag::TableCell) => out.push_str("<td>"),
            Event::End(TagEnd::TableCell) => out.push_str("</td>"),
            Event::Rule => out.push_str("<hr/>"),
            _ => {}
        }
    }
    out
}

// -- the editor's spans -----------------------------------------------------------

/// The Markdown source spans the editor draws: CodeMirror's Markdown mode
/// is the reference, as the notes editor's parser has it. The plan moves
/// that parser to `shell/widgets/` for both editors in phase 1; this is
/// the same walk, kept here so notes is untouched, over the body alone —
/// the frontmatter block is chrome, dim from its first line to its last.
#[must_use]
pub fn spans(text: &str) -> Vec<Span> {
    let dim = Style { dim: true, ..Style::default() };
    let mut spans = Vec::new();
    let mut offset = 0;
    let mut body = text;
    if let Some((first, rest)) = text.split_once('\n') {
        if is_delimiter(first) {
            let mut at = 0;
            for line in rest.split_inclusive('\n') {
                let bare = line.strip_suffix('\n').unwrap_or(line);
                at += line.len();
                if is_delimiter(bare) {
                    let end = (first.len() + 1 + at).min(text.len());
                    spans.push(Span { range: 0..end, style: dim });
                    offset = end;
                    body = &text[offset..];
                    break;
                }
            }
        }
    }
    let mut changes = Vec::new();
    for (event, range) in Parser::new_ext(body, Options::ENABLE_STRIKETHROUGH).into_offset_iter() {
        let style = match event {
            Event::Start(Tag::Strong | Tag::Heading { .. }) => Style { bold: true, ..Style::default() },
            Event::Start(Tag::Emphasis) => Style { italic: true, ..Style::default() },
            Event::Start(Tag::CodeBlock(_) | Tag::BlockQuote(_) | Tag::Strikethrough | Tag::Link { .. })
            | Event::Code(_) => dim,
            _ => continue,
        };
        changes.push((range.start, 1i32, style));
        changes.push((range.end, -1i32, style));
    }
    changes.sort_by_key(|change| change.0);
    let mut counts = [0i32; 3];
    let mut previous = 0;
    for (at, delta, style) in changes {
        let active = Style { bold: counts[0] > 0, italic: counts[1] > 0, dim: counts[2] > 0 };
        if previous < at && active != Style::default() {
            spans.push(Span { range: offset + previous..offset + at, style: active });
        }
        for (i, on) in [style.bold, style.italic, style.dim].into_iter().enumerate() {
            if on {
                counts[i] += delta;
            }
        }
        previous = at;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(target: &str, _: LinkKind) -> Target {
        let t = target.trim_end_matches(".md");
        match t {
            "porto-lume" | "Porto Lume" => Target::Page { uid: "u1".into(), slug: "porto-lume".into(), title: "Porto Lume".into() },
            "sources/mooring-fees-2026.pdf" => Target::File { path: target.into(), hash: "abc".into() },
            "attachments/harbour.png" => Target::File { path: target.into(), hash: "def".into() },
            _ => Target::Dangling(target.into()),
        }
    }

    #[test]
    fn the_frontmatter_round_trips_and_keeps_what_it_does_not_know() {
        let doc = "---\ntype: entity\ntitle: Porto Lume\nsummary: the harbour town\naliases: [Lume, porto]\ntags: [city, home]\ncaptured: 2026-08-30\n---\n\nA town.\n";
        let (front, body) = parse_document(doc);
        assert_eq!(front.kind, "entity");
        assert_eq!(front.title, "Porto Lume");
        assert_eq!(front.summary, "the harbour town");
        assert_eq!(front.aliases, ["Lume", "porto"]);
        assert_eq!(front.tags, ["city", "home"]);
        assert_eq!(front.extra, "{\"captured\":\" 2026-08-30\"}");
        assert_eq!(body, "A town.\n");
        assert_eq!(document(&front, &body), doc);
        // No block: all body. An unclosed block: all body.
        assert_eq!(parse_document("just words"), (Front::default(), "just words".to_string()));
        assert_eq!(parse_document("---\ntype: x\nno end").1, "---\ntype: x\nno end");
        // An empty block is a frontmatter that says nothing, not a body.
        let (f, b) = parse_document("---\n---\nbody\n");
        assert_eq!(f, Front::default());
        assert_eq!(b, "body\n");
        // A thematic break in the body is not a delimiter: the block ends
        // at the first `---` line, and the body keeps its own.
        let (_, b) = parse_document("---\ntitle: t\n---\n\nabove\n\n---\n\nbelow\n");
        assert_eq!(b, "above\n\n---\n\nbelow\n");
    }

    #[test]
    fn quoted_items_block_lists_nested_maps_and_crlf_survive() {
        let doc = "---\r\ntype: source\r\ntitle: \"Fees: the list\"\r\naliases: [\"one, two\", 'it''s']\r\ntags:\r\n  - sailing\r\n  - \"prices, 2026\"\r\nsource:\r\n  kind: mail\r\n  from: the office\r\ncaptured: 2026-08-30\r\n---\r\n\r\nBody line.\r\n";
        let (front, body) = parse_document(doc);
        assert_eq!(front.title, "Fees: the list");
        assert_eq!(front.aliases, ["one, two", "it's"]);
        assert_eq!(front.tags, ["sailing", "prices, 2026"]);
        let extra: serde_json::Value = serde_json::from_str(&front.extra).unwrap();
        assert_eq!(extra["source"], "\n  kind: mail\n  from: the office");
        assert_eq!(extra["captured"], " 2026-08-30");
        assert_eq!(body, "Body line.\r\n");
        // Out again: the known keys the app's way, quoted where they must
        // be; the unknown ones byte for byte.
        let again = document(&front, &body);
        assert!(again.starts_with("---\ntype: source\ntitle: \"Fees: the list\"\naliases: [\"one, two\", \"it's\"]\ntags: [sailing, \"prices, 2026\"]\ncaptured: 2026-08-30\nsource:\n  kind: mail\n  from: the office\n---\n"), "{again}");
        let (front2, _) = parse_document(&again);
        assert_eq!(front2, front);
    }

    #[test]
    fn every_link_shape_the_folder_uses_is_read_and_code_is_not() {
        let body = "See [[porto-lume]] and [[ Porto Lume | the town ]], the [fees](sources/mooring-fees-2026.pdf \"a title\"), \
                    a [page](joule-heating.md), a [scan](sources/tax%20letter.pdf), a [tricky](<sources/a%2Fb.pdf>) \
                    and ![the harbour](attachments/harbour.png), but not [the web](https://example.org) nor `[[code]]`.\n\n\
                    ~~~\n[[not-a-link]]\n~~~\n\n````md\n```\n[[nor-this]]\n```\n````\n";
        let found = links(body);
        assert_eq!(
            found,
            vec![
                ("porto-lume".to_string(), LinkKind::Wiki),
                ("Porto Lume".to_string(), LinkKind::Wiki),
                ("sources/mooring-fees-2026.pdf".to_string(), LinkKind::File),
                ("joule-heating.md".to_string(), LinkKind::Md),
                ("sources/tax%20letter.pdf".to_string(), LinkKind::File),
                ("sources/a%2Fb.pdf".to_string(), LinkKind::File),
                ("attachments/harbour.png".to_string(), LinkKind::Image),
            ]
        );
        // The destination ranges point at the address, not the text.
        for f in scan(body) {
            let d = f.dest.clone().expect("inline links have an address in the source");
            assert_eq!(body[d].trim_end_matches(".md"), f.target.trim_end_matches(".md"), "{f:?}");
        }
    }

    #[test]
    fn the_html_carries_the_kb_forms_and_says_what_dangles() {
        let body = "A [[Porto Lume|town]] with a [pdf](sources/mooring-fees-2026.pdf), a [[tide-tables]] and ![sea](attachments/harbour.png).\n\n- [ ] moor\n- [x] pay\n";
        let html = html(body, &resolver);
        assert!(html.contains("<a href=\"kb:page/porto-lume\">town</a>"), "{html}");
        assert!(html.contains("<a href=\"kb:file/sources/mooring-fees-2026.pdf\">pdf</a>"), "{html}");
        assert!(html.contains("<dangling>tide-tables</dangling>"), "{html}");
        assert!(html.contains("<img src=\"cid:kb/def\" alt=\"sea\"/>"), "{html}");
        assert!(html.contains("\u{2610} moor") && html.contains("\u{2611} pay"), "{html}");
        assert!(!html.contains("wiki:"), "a word with a space is a link, not text: {html}");
        assert_eq!(route("kb:page/porto-lume"), Some(("page", "porto-lume".to_string())));
        assert_eq!(route("kb:file/a/b.pdf"), Some(("file", "a/b.pdf".to_string())));
        assert_eq!(route("https://x"), None);
        // Code is never a link.
        let coded = super::html("`[[x]]`\n\n```\n[[y]]\n```\n", &resolver);
        assert!(coded.contains("<code>[[x]]</code>") && coded.contains("[[y]]") && !coded.contains("dangling"), "{coded}");
    }

    #[test]
    fn a_rename_rewrites_addresses_alone_and_leaves_code_be() {
        let names = |t: &str, _: LinkKind| t.trim_end_matches(".md") == "berlin";
        let body = "[[berlin]] [[ berlin | the city ]] [b](berlin.md \"title\") [c](berlin) [[bonn]] `[[berlin]]`\n\n```\n[[berlin]]\n```\n[d](<berlin.md>)";
        assert_eq!(
            rename_links(body, &names, "bonn"),
            "[[bonn]] [[ bonn | the city ]] [b](bonn.md \"title\") [c](bonn) [[bonn]] `[[berlin]]`\n\n```\n[[berlin]]\n```\n[d](<bonn.md>)"
        );
        assert_eq!(slug_of("Porto Lume: the harbour!"), "porto-lume-the-harbour");
        assert_eq!(slug_of("  Über  Ærø "), "über-ærø");
    }

    #[test]
    fn the_frontmatter_is_dim_in_the_editor() {
        let text = "---\ntype: note\n---\n\n# Head\n";
        let s = spans(text);
        let at = |needle: &str| s.iter().find(|sp| sp.range.contains(&text.find(needle).unwrap())).map(|sp| sp.style).unwrap_or_default();
        assert!(at("type").dim && !at("type").bold);
        assert!(at("Head").bold && !at("Head").dim);
        assert_eq!(s[0].range, 0..text.find("# Head").unwrap() - 1);
    }
}
