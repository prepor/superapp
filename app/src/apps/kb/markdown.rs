//! A page's Markdown: the document with its frontmatter, the links a body
//! names, and the HTML the shared `Html` widget draws it as.
//!
//! The prototype's converter is a pulldown walk with one pass in front of
//! it for `[[wikilinks]]`. Inside the markup an internal link is
//! `kb:page/<slug>` or `kb:file/<path>`, the form CR-022 names; a picture
//! whose source is a file's path is `cid:kb/<hash>`, filed with the
//! reader's pictures before the draw. A dangling link is a `<dangling>`
//! element, which the panel's template draws muted.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;

use super::model::Target;
use crate::shell::widgets::source_input::{Span, Style};

// -- the document ----------------------------------------------------------------

/// What a page's frontmatter says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Front {
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub slug: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    /// Keys the columns do not know, as a JSON object.
    pub extra: String,
}

fn list_of(value: &str) -> Vec<String> {
    let v = value.trim();
    let inner = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(v);
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn list_text(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

/// Splits a document into its frontmatter and its body. A document with no
/// leading `---` block is all body.
#[must_use]
pub fn parse_document(doc: &str) -> (Front, String) {
    let mut front = Front::default();
    let text = doc.strip_prefix('\u{feff}').unwrap_or(doc);
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return (front, text.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (front, text.to_string());
    };
    let block = &rest[..end];
    let mut body = &rest[end + 4..];
    body = body.strip_prefix('\r').unwrap_or(body);
    body = body.strip_prefix('\n').unwrap_or(body);
    let body = body.trim_start_matches('\n').to_string();
    let mut extra: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "type" => front.kind = value.to_string(),
            "title" => front.title = value.trim_matches('"').to_string(),
            "summary" => front.summary = value.trim_matches('"').to_string(),
            "slug" => front.slug = value.to_string(),
            "aliases" => front.aliases = list_of(value),
            "tags" => front.tags = list_of(value),
            "" => {}
            other => {
                extra.insert(other.to_string(), serde_json::Value::String(value.to_string()));
            }
        }
    }
    front.extra = if extra.is_empty() {
        "{}".to_string()
    } else {
        serde_json::to_string(&extra).unwrap_or_else(|_| "{}".to_string())
    };
    (front, body)
}

/// A page's document: the frontmatter block the folder's rules spell,
/// then the body — what the editor shows and what `kb.write` takes.
#[must_use]
pub fn document(front: &Front, body: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("type: {}\n", front.kind));
    out.push_str(&format!("title: {}\n", front.title));
    if !front.summary.is_empty() {
        out.push_str(&format!("summary: {}\n", front.summary));
    }
    if !front.aliases.is_empty() {
        out.push_str(&format!("aliases: {}\n", list_text(&front.aliases)));
    }
    if !front.tags.is_empty() {
        out.push_str(&format!("tags: {}\n", list_text(&front.tags)));
    }
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&front.extra) {
        for (k, v) in map {
            let v = v.as_str().map_or_else(|| v.to_string(), str::to_string);
            out.push_str(&format!("{k}: {v}\n"));
        }
    }
    out.push_str("---\n\n");
    out.push_str(body);
    if !body.ends_with('\n') {
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

/// Every `[[wikilink]]` in a body, outside fenced code: the target and the
/// text, with the byte range it spans.
fn wikilinks(body: &str) -> Vec<(std::ops::Range<usize>, String, String)> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut at = 0;
    for line in body.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            let mut rest = 0;
            while let Some(start) = line[rest..].find("[[") {
                let s = rest + start;
                let Some(len) = line[s + 2..].find("]]") else {
                    break;
                };
                let inner = &line[s + 2..s + 2 + len];
                let (target, text) = match inner.split_once('|') {
                    Some((t, x)) => (t.trim().to_string(), x.trim().to_string()),
                    None => (inner.trim().to_string(), inner.trim().to_string()),
                };
                if !target.is_empty() {
                    out.push((at + s..at + s + 2 + len + 2, target, text));
                }
                rest = s + 2 + len + 2;
            }
        }
        at += line.len();
    }
    out
}

/// Every link a body names, as written, with its kind: `wiki`, `md`,
/// `file` or `image`. Web links are not the KB's and are left out.
#[must_use]
pub fn links(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |target: String, kind: &str| {
        if !out.iter().any(|(t, k)| *t == target && k == kind) {
            out.push((target, kind.to_string()));
        }
    };
    for (_, target, _) in wikilinks(body) {
        push(target, "wiki");
    }
    let parser = Parser::new_ext(body, Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS);
    for event in parser {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                let dest = dest_url.to_string();
                if external(&dest) || dest.starts_with('#') || dest.is_empty() {
                    continue;
                }
                let kind = if dest.ends_with(".md") { "md" } else { "file" };
                push(dest, kind);
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                let dest = dest_url.to_string();
                if !external(&dest) && !dest.is_empty() {
                    push(dest, "image");
                }
            }
            _ => {}
        }
    }
    out
}

/// A body with every link to `old` spelling `new`.
#[must_use]
pub fn rename_links(body: &str, old: &str, new: &str) -> String {
    body.replace(&format!("[[{old}]]"), &format!("[[{new}]]"))
        .replace(&format!("[[{old}|"), &format!("[[{new}|"))
        .replace(&format!("]({old}.md)"), &format!("]({new}.md)"))
        .replace(&format!("]({old})"), &format!("]({new})"))
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
pub fn html(body: &str, resolve: &dyn Fn(&str) -> Target) -> String {
    // The wikilink pass: each becomes a Markdown link on a `wiki:` target,
    // so the walk below sees one kind of link.
    let mut prepared = String::with_capacity(body.len());
    let mut at = 0;
    for (range, target, text) in wikilinks(body) {
        prepared.push_str(&body[at..range.start]);
        prepared.push_str(&format!("[{}](wiki:{})", text.replace(']', "\\]"), target.replace(')', "%29")));
        at = range.end;
    }
    prepared.push_str(&body[at..]);

    let mut out = String::new();
    let mut open_link: Option<Option<String>> = None;
    let mut image: Option<(Option<String>, String)> = None;
    let parser = Parser::new_ext(&prepared, Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH);
    for event in parser {
        if let Some((_, alt)) = &mut image {
            match event {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => {
                    let (src, alt) = image.take().unwrap();
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
                } else {
                    let raw = dest.strip_prefix("wiki:").unwrap_or(&dest).replace("%29", ")");
                    href_of(&resolve(&raw))
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
            Event::End(TagEnd::Link) => {
                match open_link.take() {
                    Some(Some(_)) => out.push_str("</a>"),
                    _ => out.push_str("</dangling>"),
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                let dest = dest_url.to_string();
                let src = if external(&dest) {
                    Some(dest)
                } else {
                    match resolve(&dest) {
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
            Event::Start(Tag::CodeBlock(kind)) => {
                let _ = matches!(kind, CodeBlockKind::Fenced(_));
                out.push_str("<pre>");
            }
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
/// the same walk, kept here so notes is untouched.
#[must_use]
pub fn spans(text: &str) -> Vec<Span> {
    let mut changes = Vec::new();
    for (event, range) in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH).into_offset_iter() {
        let style = match event {
            Event::Start(Tag::Strong | Tag::Heading { .. }) => Style { bold: true, ..Style::default() },
            Event::Start(Tag::Emphasis) => Style { italic: true, ..Style::default() },
            Event::Start(Tag::CodeBlock(_) | Tag::BlockQuote(_) | Tag::Strikethrough | Tag::Link { .. })
            | Event::Code(_) => Style { dim: true, ..Style::default() },
            _ => continue,
        };
        changes.push((range.start, 1i32, style));
        changes.push((range.end, -1i32, style));
    }
    // The frontmatter block is chrome: dim, as a code block is.
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let dim = Style { dim: true, ..Style::default() };
            changes.push((0, 1, dim));
            changes.push((4 + end + 4, -1, dim));
        }
    }
    changes.sort_by_key(|change| change.0);
    let mut counts = [0i32; 3];
    let mut previous = 0;
    let mut spans = Vec::new();
    for (offset, delta, style) in changes {
        let active = Style { bold: counts[0] > 0, italic: counts[1] > 0, dim: counts[2] > 0 };
        if previous < offset && active != Style::default() {
            spans.push(Span { range: previous..offset, style: active });
        }
        for (i, on) in [style.bold, style.italic, style.dim].into_iter().enumerate() {
            if on {
                counts[i] += delta;
            }
        }
        previous = offset;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(target: &str) -> Target {
        match target {
            "porto-lume" | "Porto Lume" => Target::Page { slug: "porto-lume".into(), title: "Porto Lume".into() },
            "sources/mooring-fees-2026.pdf" => Target::File { path: target.into(), hash: "abc".into() },
            "attachments/harbour.png" => Target::File { path: target.into(), hash: "def".into() },
            other => Target::Dangling(other.into()),
        }
    }

    #[test]
    fn the_frontmatter_round_trips() {
        let doc = "---\ntype: entity\ntitle: Porto Lume\nsummary: the harbour town\naliases: [Lume, porto]\ntags: [city, home]\ncaptured: 2026-08-30\n---\n\nA town.\n";
        let (front, body) = parse_document(doc);
        assert_eq!(front.kind, "entity");
        assert_eq!(front.title, "Porto Lume");
        assert_eq!(front.summary, "the harbour town");
        assert_eq!(front.aliases, ["Lume", "porto"]);
        assert_eq!(front.tags, ["city", "home"]);
        assert_eq!(front.extra, "{\"captured\":\"2026-08-30\"}");
        assert_eq!(body, "A town.\n");
        assert_eq!(document(&front, &body), doc);
        // No block: all body.
        let (f, b) = parse_document("just words");
        assert_eq!(f, Front::default());
        assert_eq!(b, "just words");
    }

    #[test]
    fn every_link_shape_the_folder_uses_is_read() {
        let body = "See [[porto-lume]] and [[Porto Lume|the town]], the [fees](sources/mooring-fees-2026.pdf), \
                    a [page](joule-heating.md), a [scan](sources/tax%20letter.pdf), a [tricky](sources/a%2Fb.pdf) \
                    and ![the harbour](attachments/harbour.png), but not [the web](https://example.org).\n\n```\n[[not-a-link]]\n```\n";
        let found = links(body);
        assert_eq!(
            found,
            vec![
                ("porto-lume".to_string(), "wiki".to_string()),
                ("Porto Lume".to_string(), "wiki".to_string()),
                ("sources/mooring-fees-2026.pdf".to_string(), "file".to_string()),
                ("joule-heating.md".to_string(), "md".to_string()),
                ("sources/tax%20letter.pdf".to_string(), "file".to_string()),
                ("sources/a%2Fb.pdf".to_string(), "file".to_string()),
                ("attachments/harbour.png".to_string(), "image".to_string()),
            ]
        );
    }

    #[test]
    fn the_html_carries_the_kb_forms_and_says_what_dangles() {
        let body = "A [[porto-lume|town]] with a [pdf](sources/mooring-fees-2026.pdf), a [[tide-tables]] and ![sea](attachments/harbour.png).\n\n- [ ] moor\n- [x] pay\n";
        let html = html(body, &resolver);
        assert!(html.contains("<a href=\"kb:page/porto-lume\">town</a>"), "{html}");
        assert!(html.contains("<a href=\"kb:file/sources/mooring-fees-2026.pdf\">pdf</a>"), "{html}");
        assert!(html.contains("<dangling>tide-tables</dangling>"), "{html}");
        assert!(html.contains("<img src=\"cid:kb/def\" alt=\"sea\"/>"), "{html}");
        assert!(html.contains("\u{2610} moor") && html.contains("\u{2611} pay"), "{html}");
        assert_eq!(route("kb:page/porto-lume"), Some(("page", "porto-lume".to_string())));
        assert_eq!(route("kb:file/a/b.pdf"), Some(("file", "a/b.pdf".to_string())));
        assert_eq!(route("https://x"), None);
    }

    #[test]
    fn a_title_becomes_a_slug_and_a_rename_rewrites_links() {
        assert_eq!(slug_of("Porto Lume: the harbour!"), "porto-lume-the-harbour");
        assert_eq!(slug_of("  Über  Ærø "), "über-ærø");
        let body = "[[old]] [[old|x]] [y](old.md) [[older]]";
        assert_eq!(rename_links(body, "old", "new"), "[[new]] [[new|x]] [y](new.md) [[older]]");
    }

    #[test]
    fn the_frontmatter_is_dim_in_the_editor() {
        let text = "---\ntype: note\n---\n\n# Head\n";
        let s = spans(text);
        let at = |needle: &str| s.iter().find(|sp| sp.range.contains(&text.find(needle).unwrap())).map(|sp| sp.style).unwrap_or_default();
        assert!(at("type").dim);
        assert!(at("Head").bold && !at("Head").dim);
    }
}
