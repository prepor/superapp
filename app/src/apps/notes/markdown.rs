//! CodeMirror's Markdown source mode is the reference: visible punctuation,
//! typographic emphasis, no rendered widgets or code-language highlighting.
//! CommonMark source ranges handle escaping, nesting and code correctly.
//! https://codemirror.net/5/mode/markdown/

use crate::shell::widgets::source_input::{Span, Style};
use pulldown_cmark::{Event, Options, Parser, Tag};

pub fn spans(text: &str) -> Vec<Span> {
    let mut changes = Vec::new();
    for (event, range) in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH).into_offset_iter() {
        let style = match event {
            Event::Start(Tag::Strong | Tag::Heading { .. }) => Style {
                bold: true,
                ..Style::default()
            },
            Event::Start(Tag::Emphasis) => Style {
                italic: true,
                ..Style::default()
            },
            Event::Start(
                Tag::CodeBlock(_) | Tag::BlockQuote(_) | Tag::Strikethrough | Tag::Link { .. },
            )
            | Event::Code(_) => Style {
                dim: true,
                ..Style::default()
            },
            _ => continue,
        };
        changes.push((range.start, 1i32, style));
        changes.push((range.end, -1i32, style));
    }
    changes.sort_by_key(|change| change.0);
    let mut counts = [0i32; 3];
    let mut previous = 0;
    let mut spans = Vec::new();
    for (offset, delta, style) in changes {
        let active = Style {
            bold: counts[0] > 0,
            italic: counts[1] > 0,
            dim: counts[2] > 0,
        };
        if previous < offset && active != Style::default() {
            spans.push(Span {
                range: previous..offset,
                style: active,
            });
        }
        for (i, on) in [style.bold, style.italic, style.dim]
            .into_iter()
            .enumerate()
        {
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
    fn at(text: &str, needle: &str) -> Style {
        let at = text.find(needle).unwrap();
        spans(text)
            .into_iter()
            .find(|span| span.range.contains(&at))
            .map(|s| s.style)
            .unwrap_or_default()
    }
    #[test]
    fn nested_emphasis_escapes_and_code_follow_commonmark() {
        let text = "# Heading\n\n**bold *nested*** café _italic_\n\n\\*literal\\* snake_case_name\n\n`**code**`\n\n```rust\n**fenced**\n```\n";
        assert!(at(text, "Heading").bold);
        assert!(at(text, "bold").bold);
        assert!(at(text, "nested").bold && at(text, "nested").italic);
        assert!(at(text, "italic").italic);
        for word in ["literal", "snake", "code", "fenced"] {
            assert!(!at(text, word).bold && !at(text, word).italic, "{word}");
        }
        for span in spans(text) {
            assert!(
                text.is_char_boundary(span.range.start) && text.is_char_boundary(span.range.end)
            );
        }
    }
    #[test]
    fn unmatched_marks_and_multiline_emphasis_keep_their_source_ranges() {
        assert_eq!(at("**unfinished", "unfinished"), Style::default());
        assert!(at("**two\nlines**", "lines").bold);
        assert!(at("setext\n======", "setext").bold);
        assert!(!at("    **indented code**", "indented").bold);
        let s = spans("é **重い** _résumé_");
        assert!(s.windows(2).all(|w| w[0].range.end <= w[1].range.start));
    }
}
