//! Answers as selectable text with inline, clickable source links.
//!
//! Parse Markdown, then emit only the markup this view uses. Raw HTML is
//! escaped; images show their labels without loading external content.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

pub(super) fn web_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
    })
}

pub(super) fn html(body: &str) -> String {
    let mut out = String::new();
    let mut link = None;
    for event in Parser::new(body) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                link = web_url(&dest_url).then(|| dest_url.to_string());
            }
            Event::End(TagEnd::Link) => link = None,
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                span(&mut out, &text, link.as_deref());
            }
            Event::Code(text) => {
                out.push_str("<code>");
                span(&mut out, &text, link.as_deref());
                out.push_str("</code>");
            }
            Event::SoftBreak | Event::HardBreak => out.push_str("<br/>"),
            Event::Start(Tag::Paragraph) => out.push_str("<p>"),
            Event::End(TagEnd::Paragraph) => out.push_str("</p>"),
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            Event::Start(Tag::Heading { .. }) => out.push_str("<p><b>"),
            Event::End(TagEnd::Heading(_)) => out.push_str("</b></p>"),
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
            Event::Rule => out.push_str("<hr/>"),
            _ => {}
        }
    }
    out
}

fn span(out: &mut String, text: &str, link: Option<&str>) {
    // HtmlLink draws one text node: reopen it for styled runs and line
    // breaks so no words disappear inside an anchor.
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 { out.push_str("<br/>"); }
        if let Some(url) = link {
            out.push_str("<a href=\"");
            escape(out, url);
            out.push_str("\">");
        }
        escape(out, line);
        if link.is_some() { out.push_str("</a>"); }
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
            ' ' => out.push_str("&#32;"),
            '\t' => out.push_str("&#9;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_are_inline_links_with_their_readable_labels() {
        assert_eq!(html("I found [the Rust book](https://doc.rust-lang.org/book/)."),
            "<p>I&#32;found&#32;<a href=\"https://doc.rust-lang.org/book/\">the&#32;Rust&#32;book</a>.</p>");
        assert!(html("[source](https://example.org/?a=1&b=2)").contains("?a=1&amp;b=2"));
        assert!(html("[`source`](https://example.org)").contains("<a href=\"https://example.org\">source</a>"));
    }

    #[test]
    fn an_answer_cannot_inject_html_or_open_non_web_links() {
        assert!(web_url("http://example.org/source"));
        for url in ["javascript:alert(1)", "file:///etc/passwd", "data:text/html,test", "relative/path"] {
            assert!(!web_url(url), "{url}");
            assert!(!html(&format!("[source]({url})")).contains("<a "));
        }
        assert!(!html("<img src=\"https://example.org/image\">").contains("<img"));
        assert!(!html("![picture](https://example.org/image)").contains("<img"));
        assert!(html("`<tag>`").contains("<code>&lt;tag&gt;</code>"));
    }
}
