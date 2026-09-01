//! HTML mail, reduced to the subset makepad's `Html` widget can draw.
//!
//! Mail arrives as whatever a sending client felt like emitting: nested
//! layout tables, inline styles, tracking pixels, Outlook's conditional
//! comments. The widget draws a small semantic vocabulary and no CSS at
//! all, so the job here is a narrowing, not a rendering — everything the
//! widget cannot say is either unwrapped (tag goes, text stays) or dropped
//! whole (tag and subtree both).
//!
//! The narrowing is also the security story. Nothing survives that could
//! fetch: no `<img>`, no `<iframe>`, no `<script>`, no stylesheet. A
//! tracking pixel has no `alt` text, so it leaves without a trace. Links
//! keep only the schemes a reader could have meant.
//!
//! Entities pass through untouched — makepad's own parser decodes them, and
//! decoding here would mean escaping them again on the way out. With one
//! exception it cannot survive: a numeric reference whose value is not a
//! Unicode scalar is unwrapped straight into a panic there, so those are
//! repaired on the way in (see [`fix_numeric_entities`]).

/// Elements the widget draws, emitted as they came (attributes dropped;
/// `<a>` is the one exception, handled separately).
///
/// The list is exactly what `widgets/src/html.rs` dispatches on, minus the
/// table family: see [`BLOCK`] for why tables are unwrapped instead.
const KEEP: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "s",
    "del",
    "strike",
    "sub",
    "sup",
    "code",
    "pre",
    "ul",
    "ol",
    "li",
    "blockquote",
];

/// Kept elements that stand on their own line. A pending break before one
/// of these is redundant — the element already separates itself — so the
/// emitter drops it rather than opening with a blank line.
const KEEP_BLOCK: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "ul",
    "ol",
    "li",
    "blockquote",
];

/// Dropped whole, subtree included. `script` and `style` matter most: their
/// content is source, not prose, and unwrapping them would spill CSS into
/// the middle of the letter.
const DROP: &[&str] = &[
    "script", "style", "head", "title", "noscript", "iframe", "object", "embed", "svg", "math",
    "template", "select", "option", "button", "input", "textarea", "map", "area",
];

/// Unwrapped, but still worth a line break: the tag goes and the text
/// stays, separated.
///
/// The table family lives here on purpose. Mail uses tables to arrange a
/// page, not to tabulate data — a bordered grid drawn around a header
/// image and a footer reads far worse than the lines it was holding apart.
/// Genuine data tables lose their grid too; that is the trade, and the
/// seam to revisit if invoices start arriving that need it.
const BLOCK: &[&str] = &[
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "aside",
    "nav",
    "center",
    "address",
    "form",
    "fieldset",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "caption",
    "colgroup",
];

/// Unwrapped with a space, not a break: cells sat side by side, so their
/// text should not each claim a line.
const SPACE: &[&str] = &["td", "th"];

/// Link schemes a reader could plausibly have meant. Everything else —
/// `javascript:`, `data:`, the `cid:` that points at an attachment we do
/// not fetch — loses the href and keeps only its text.
const SCHEMES: &[&str] = &["http://", "https://", "mailto:"];

/// What the scanner decided about one tag.
enum Class {
    /// Emit it; the widget knows this one.
    Keep,
    /// Skip the element and everything inside it.
    Drop,
    /// Drop the tag, keep the text, separate with a line break.
    Block,
    /// Drop the tag, keep the text, separate with a space.
    Space,
    /// Drop the tag, keep the text, add nothing (`span`, `font`, `o:p`,
    /// and every tag nobody has invented yet).
    Inline,
}

fn classify(name: &str) -> Class {
    if KEEP.contains(&name) {
        Class::Keep
    } else if DROP.contains(&name) {
        Class::Drop
    } else if BLOCK.contains(&name) {
        Class::Block
    } else if SPACE.contains(&name) {
        Class::Space
    } else {
        Class::Inline
    }
}

/// One parsed tag: `<name attrs>`, `</name>` or `<name attrs/>`.
struct Tag<'a> {
    name: String,
    close: bool,
    self_closing: bool,
    attrs: &'a str,
    /// Index just past the closing `>`.
    end: usize,
}

/// What a `<` turned out to be.
enum Scan<'a> {
    Tag(Tag<'a>),
    /// A `<` that begins no tag: prose, as in `a < b` or `I <3 it`. A tag
    /// name must start with a letter, which is what tells these apart.
    NotATag,
    /// A tag that never closed — the document ended inside it. Browsers
    /// drop the remainder rather than show its source, and so do we.
    Truncated,
}

/// Reads the tag starting at `i`, which must point at `<`.
fn parse_tag(src: &str, i: usize) -> Scan<'_> {
    let b = src.as_bytes();
    let mut j = i + 1;
    let close = b.get(j) == Some(&b'/');
    if close {
        j += 1;
    }
    let name_start = j;
    if !b.get(j).is_some_and(u8::is_ascii_alphabetic) {
        return Scan::NotATag;
    }
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b':' || b[j] == b'-') {
        j += 1;
    }
    let name = src[name_start..j].to_ascii_lowercase();

    // Find the '>' that actually ends the tag: attribute values may quote
    // one of their own.
    let attr_start = j;
    let mut quote = 0u8;
    while j < b.len() {
        let c = b[j];
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'>' {
            break;
        }
        j += 1;
    }
    if j >= b.len() {
        return Scan::Truncated;
    }
    let attrs = &src[attr_start..j];
    Scan::Tag(Tag {
        name,
        close,
        self_closing: attrs.trim_end().ends_with('/'),
        attrs,
        end: j + 1,
    })
}

/// Pulls one attribute's value out of a tag's attribute text. Values may be
/// double-quoted, single-quoted, or bare.
fn attr(attrs: &str, want: &str) -> Option<String> {
    let hay = attrs.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(want) {
        let at = from + rel;
        from = at + want.len();
        // The name must stand alone: preceded by space, followed by '='.
        let before_ok = at == 0 || !hay.as_bytes()[at - 1].is_ascii_alphanumeric();
        let rest = hay[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let vs = from + hay[from..].find('=')? + 1;
        let val = attrs[vs..].trim_start();
        let cut = attrs.len() - val.len();
        return Some(match val.as_bytes().first() {
            Some(&q @ (b'"' | b'\'')) => {
                let v = &attrs[cut + 1..];
                v[..v.find(q as char).unwrap_or(v.len())].to_string()
            }
            _ => {
                let v = &attrs[cut..];
                v[..v.find(char::is_whitespace).unwrap_or(v.len())]
                    .trim_end_matches('/')
                    .to_string()
            }
        });
    }
    None
}

/// Skips the element opened at `from` (index just past its `>`), matching
/// nesting, and returns the index just past its close tag.
fn skip_element(src: &str, from: usize, name: &str) -> usize {
    let mut depth = 1usize;
    let mut i = from;
    while i < src.len() {
        let Some(rel) = src[i..].find('<') else { break };
        let at = i + rel;
        match parse_tag(src, at) {
            Scan::Tag(t) if t.name == name => {
                if t.close {
                    depth -= 1;
                    if depth == 0 {
                        return t.end;
                    }
                } else if !t.self_closing {
                    depth += 1;
                }
                i = t.end;
            }
            Scan::Tag(t) => i = t.end,
            Scan::NotATag => i = at + 1,
            // The element never closed: it swallows the rest of the mail,
            // which is exactly what dropping to the end does.
            Scan::Truncated => return src.len(),
        }
    }
    src.len()
}

/// Accumulates output while holding back whitespace, so runs of unwrapped
/// `<div>`s collapse into one break instead of a stack of empty lines, and
/// nothing leading or trailing survives.
#[derive(Default)]
struct Out {
    s: String,
    /// A break owed to the text that comes next, if any ever does.
    brk: bool,
    /// A space owed likewise; a pending break outranks it.
    space: bool,
    /// Kept tags still open, innermost last.
    stack: Vec<String>,
}

impl Out {
    /// Settles what is owed. `block` marks output that separates itself and
    /// so cancels a pending break rather than following it.
    fn flush(&mut self, block: bool) {
        if self.s.is_empty() {
            // Nothing to separate from yet.
        } else if self.brk && !block {
            self.s.push_str("<br>");
        } else if self.space && !block {
            self.s.push(' ');
        }
        self.brk = false;
        self.space = false;
    }

    fn text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        self.flush(false);
        self.s.push_str(t);
    }

    fn open(&mut self, name: &str) {
        self.flush(KEEP_BLOCK.contains(&name));
        self.s.push('<');
        self.s.push_str(name);
        self.s.push('>');
        self.stack.push(name.to_string());
    }

    fn close(&mut self, name: &str) {
        let Some(at) = self.stack.iter().rposition(|n| n == name) else {
            return; // A close with no open: the sender's problem, not ours.
        };
        self.flush(KEEP_BLOCK.contains(&name));
        while self.stack.len() > at {
            let n = self.stack.pop().expect("rposition found it");
            self.s.push_str("</");
            self.s.push_str(&n);
            self.s.push('>');
        }
    }

    /// A void element — `<br>`, `<hr>` — which owes nothing and is owed
    /// nothing.
    fn void(&mut self, name: &str) {
        self.flush(true);
        self.s.push('<');
        self.s.push_str(name);
        self.s.push('>');
    }

    fn finish(mut self) -> String {
        while let Some(n) = self.stack.pop() {
            self.s.push_str("</");
            self.s.push_str(&n);
            self.s.push('>');
        }
        self.s
    }
}

/// Escapes a string for an attribute value. The source was already HTML,
/// so its `&` are entities and stay; only the quote and the brackets could
/// break out of the attribute we are rebuilding.
fn escape_attr(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Collapses whitespace the way HTML would: any run becomes one space.
/// Returns the text plus whether it began or ended on whitespace, which
/// the caller turns into pending spaces.
fn collapse(t: &str) -> (String, bool, bool) {
    let lead = t.starts_with(char::is_whitespace);
    let trail = t.ends_with(char::is_whitespace);
    let mut s = String::with_capacity(t.len());
    for (i, w) in t.split_whitespace().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(w);
    }
    (s, lead, trail)
}

/// One numeric character reference at `at`: its value and the index past
/// the `;`. `None` when this is not one — including digits the widget's own
/// parser would also reject, which it does cleanly.
fn num_ref(src: &str, at: usize) -> Option<(i64, usize)> {
    let b = src.as_bytes();
    if at + 3 >= b.len() || b[at] != b'&' || b[at + 1] != b'#' {
        return None;
    }
    let hex = b[at + 2] | 0x20 == b'x';
    let start = if hex { at + 3 } else { at + 2 };
    let mut j = start;
    while j < b.len() && b[j] != b';' {
        j += 1;
    }
    if j >= b.len() || j == start {
        return None;
    }
    let digits = &src[start..j];
    let n = if hex {
        i64::from_str_radix(digits, 16)
    } else {
        digits.parse::<i64>()
    };
    n.ok().map(|n| (n, j + 1))
}

/// Whether `src` carries a numeric reference the widget's parser would die
/// on. A pure scan with no allocation, so [`guard`] costs nothing on the
/// letters that are fine — which is all of them but the odd one.
fn needs_repair(src: &str) -> bool {
    let b = src.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let Some(rel) = b[i..].iter().position(|&c| c == b'&') else {
            return false;
        };
        let at = i + rel;
        match num_ref(src, at) {
            Some((n, end)) => {
                if u32::try_from(n).ok().and_then(char::from_u32).is_none() {
                    return true;
                }
                i = end;
            }
            None => i = at + 1,
        }
    }
    false
}

/// Repairs numeric character references the widget's parser cannot decode.
///
/// It parses `&#N;` into a `u32` and then calls `char::from_u32(..).unwrap()`,
/// so a value that is not a Unicode scalar takes the process down. Real mail
/// is full of them: composers routinely emit an emoji as the two halves of
/// its UTF-16 surrogate pair, and `&#55357;&#56538;` is an ordinary 📚.
///
/// A pair is put back together into the character it meant; anything else out
/// of range becomes U+FFFD. References the parser *can* read are copied
/// through byte for byte, so the module's pass-them-through rule still holds
/// everywhere it was ever true.
fn fix_numeric_entities(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    while i < src.len() {
        if let Some((n, end)) = num_ref(src, i) {
            // Readable as it stands: leave it exactly alone.
            if u32::try_from(n).ok().and_then(char::from_u32).is_some() {
                out.push_str(&src[i..end]);
                i = end;
                continue;
            }
            // A high surrogate whose low half follows it is half of a real
            // character, not a broken one.
            if (0xD800..=0xDBFF).contains(&n) {
                if let Some((lo, end2)) = num_ref(src, end) {
                    if (0xDC00..=0xDFFF).contains(&lo) {
                        let c = 0x1_0000 + ((n - 0xD800) << 10) + (lo - 0xDC00);
                        out.push(
                            u32::try_from(c)
                                .ok()
                                .and_then(char::from_u32)
                                .unwrap_or('\u{FFFD}'),
                        );
                        i = end2;
                        continue;
                    }
                }
            }
            out.push('\u{FFFD}');
            i = end;
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The last thing between stored HTML and the widget.
///
/// [`sanitize`] runs at **ingest**, so what the store holds was narrowed by
/// whichever version was current when the mail arrived. Tighten the narrowing
/// — as the surrogate repair did — and every row written before it is stale,
/// still carrying whatever the old rules let through. A mail that crashes the
/// parser crashes it on every frame that draws it, which means the app cannot
/// be opened rather than that one letter looks wrong, so the guarantee has to
/// hold at the point of use and not only at the point of writing.
///
/// Borrows unless there is something to repair, so a letter that is fine
/// costs one scan and no allocation.
#[must_use]
pub fn guard(src: &str) -> std::borrow::Cow<'_, str> {
    if needs_repair(src) {
        std::borrow::Cow::Owned(fix_numeric_entities(src))
    } else {
        std::borrow::Cow::Borrowed(src)
    }
}

/// Narrows an HTML mail body to what the `Html` widget draws.
///
/// The result is safe to hand straight to the widget: no network-fetching
/// element survives, link hrefs carry only [`SCHEMES`], and no numeric
/// character reference is left that the widget would panic decoding.
#[must_use]
pub fn sanitize(src: &str) -> String {
    let fixed = fix_numeric_entities(src);
    let src = fixed.as_str();
    let mut out = Out::default();
    let mut i = 0usize;
    // `<pre>` keeps its whitespace; everything else collapses.
    let mut pre = 0usize;

    while i < src.len() {
        let Some(rel) = src[i..].find('<') else {
            let (t, lead, _) = collapse(&src[i..]);
            if pre > 0 {
                out.text(&src[i..]);
            } else {
                if lead {
                    out.space = true;
                }
                out.text(&t);
            }
            break;
        };
        let at = i + rel;

        // The text before this tag.
        if at > i {
            let raw = &src[i..at];
            if pre > 0 {
                out.text(raw);
            } else {
                let (t, lead, trail) = collapse(raw);
                if lead && !t.is_empty() {
                    out.space = true;
                }
                out.text(&t);
                if trail {
                    out.space = true;
                }
            }
        }

        // Comments, including Outlook's conditional ones.
        if src[at..].starts_with("<!--") {
            i = src[at..].find("-->").map_or(src.len(), |e| at + e + 3);
            continue;
        }
        if src[at..].starts_with("<!") || src[at..].starts_with("<?") {
            i = src[at..].find('>').map_or(src.len(), |e| at + e + 1);
            continue;
        }

        let tag = match parse_tag(src, at) {
            Scan::Tag(t) => t,
            Scan::NotATag => {
                // A '<' that is prose. Escape it and carry on.
                out.text("&lt;");
                i = at + 1;
                continue;
            }
            Scan::Truncated => break,
        };

        match tag.name.as_str() {
            "br" if !tag.close => {
                out.brk = true;
            }
            "hr" if !tag.close => out.void("hr"),
            "img" if !tag.close => {
                // Everything visual goes; the alt text is the only part
                // that was ever prose. A tracking pixel has none, so it
                // leaves nothing behind.
                if let Some(a) = attr(tag.attrs, "alt").filter(|a| !a.trim().is_empty()) {
                    let (t, ..) = collapse(&a);
                    out.text(&t);
                }
            }
            "a" => {
                if tag.close {
                    out.close("a");
                } else {
                    let href = attr(tag.attrs, "href").filter(|h| {
                        let h = h.trim().to_ascii_lowercase();
                        SCHEMES.iter().any(|s| h.starts_with(s))
                    });
                    match href {
                        // Unwrapped when the scheme is not one we follow:
                        // the text stays, the destination does not.
                        None => {}
                        Some(h) => {
                            out.flush(false);
                            out.s.push_str("<a href=\"");
                            out.s.push_str(&escape_attr(h.trim()));
                            out.s.push_str("\">");
                            out.stack.push("a".into());
                        }
                    }
                }
            }
            _ => match classify(&tag.name) {
                Class::Keep => {
                    if tag.name == "pre" {
                        if tag.close {
                            pre = pre.saturating_sub(1);
                        } else {
                            pre += 1;
                        }
                    }
                    if tag.close {
                        out.close(&tag.name);
                    } else if !tag.self_closing {
                        out.open(&tag.name);
                    }
                }
                Class::Drop => {
                    if !tag.close && !tag.self_closing {
                        i = skip_element(src, tag.end, &tag.name);
                        continue;
                    }
                }
                Class::Block => out.brk = true,
                Class::Space => out.space = true,
                Class::Inline => {}
            },
        }
        i = tag.end;
    }

    out.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A numeric reference the widget's parser cannot decode is a crash, not
    /// a rendering bug: it unwraps `char::from_u32`. The commonest source is
    /// an emoji written as its UTF-16 surrogate pair, which is what a real
    /// mail in the wild turned out to contain.
    #[test]
    fn surrogate_pairs_are_put_back_together() {
        // &#55357;&#56538; is 📚, &#55357;&#56960; is 🚀 — as sent.
        assert_eq!(sanitize("&#55357;&#56538;"), "📚");
        assert_eq!(sanitize("<p>&#55357;&#56960; go</p>"), "<p>🚀 go</p>");
        // Hex spelling of the same thing.
        assert_eq!(sanitize("&#xD83D;&#xDE80;"), "🚀");
    }

    /// Anything else out of range is replaced rather than dropped, so the
    /// text keeps its shape and nothing downstream sees a bare surrogate.
    #[test]
    fn unpaired_or_out_of_range_references_become_replacement() {
        assert_eq!(sanitize("a&#55357;b"), "a\u{FFFD}b");
        assert_eq!(sanitize("a&#56538;b"), "a\u{FFFD}b", "lone low half");
        assert_eq!(sanitize("a&#1114112;b"), "a\u{FFFD}b", "past the last plane");
        assert_eq!(sanitize("a&#99999999999;b"), "a\u{FFFD}b", "past u32 too");
        assert_eq!(sanitize("a&#-1;b"), "a\u{FFFD}b", "negative");
        // A high surrogate followed by something that is not its low half.
        assert_eq!(sanitize("&#55357;&#65;"), "\u{FFFD}&#65;");
    }

    /// Everything the parser can read is still handed over byte for byte —
    /// decoding here would mean re-escaping on the way out.
    #[test]
    fn readable_references_pass_through_untouched() {
        assert_eq!(sanitize("<p>a &amp; b</p>"), "<p>a &amp; b</p>");
        assert_eq!(sanitize("<p>&#60;tag&#62;</p>"), "<p>&#60;tag&#62;</p>");
        assert_eq!(sanitize("<p>&#x1F4DA;</p>"), "<p>&#x1F4DA;</p>");
        assert_eq!(sanitize("<p>caf&#233;</p>"), "<p>caf&#233;</p>");
        // Not a numeric reference at all: left for the parser to reject.
        assert_eq!(sanitize("<p>a &# b &#; c</p>"), "<p>a &# b &#; c</p>");
    }

    /// The vocabulary the widget draws survives intact.
    #[test]
    fn kept_tags_pass_through() {
        assert_eq!(sanitize("<p>hi <b>there</b></p>"), "<p>hi <b>there</b></p>");
        assert_eq!(
            sanitize("<ul><li>one</li><li>two</li></ul>"),
            "<ul><li>one</li><li>two</li></ul>"
        );
        assert_eq!(
            sanitize("<blockquote>quoted</blockquote>"),
            "<blockquote>quoted</blockquote>"
        );
    }

    /// Script and style leave with their contents; unwrapping them would
    /// spill source into the letter.
    #[test]
    fn script_and_style_leave_whole() {
        assert_eq!(sanitize("<style>p{color:red}</style><p>a</p>"), "<p>a</p>");
        assert_eq!(sanitize("<script>alert(1)</script>hi"), "hi");
        assert_eq!(
            sanitize("<div><script>var a = '<b>x</b>';</script>after</div>"),
            "after"
        );
    }

    /// Layout scaffolding is unwrapped: the tags go, the text and its line
    /// structure stay.
    #[test]
    fn layout_is_unwrapped_but_lines_survive() {
        assert_eq!(sanitize("<div>one</div><div>two</div>"), "one<br>two");
        assert_eq!(
            sanitize("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>"),
            "a b<br>c"
        );
        // A stack of empty wrappers collapses to one break, not five.
        assert_eq!(
            sanitize("<div><div><div>x</div></div></div><div>y</div>"),
            "x<br>y"
        );
        // Nothing leading or trailing survives.
        assert_eq!(sanitize("<div></div><p>x</p><div></div>"), "<p>x</p>");
    }

    /// Unknown and inline-only tags vanish without touching the text.
    #[test]
    fn unknown_tags_unwrap_silently() {
        assert_eq!(sanitize("<span style='color:red'>red</span>"), "red");
        assert_eq!(sanitize("<font size=3>big</font>"), "big");
        assert_eq!(sanitize("<o:p>outlook</o:p>"), "outlook");
    }

    /// Only schemes a reader could have meant keep their href; the rest
    /// keep their text and lose the link.
    #[test]
    fn links_are_scheme_filtered() {
        assert_eq!(
            sanitize(r#"<a href="https://x.dev">go</a>"#),
            r#"<a href="https://x.dev">go</a>"#
        );
        assert_eq!(
            sanitize(r#"<a href="mailto:me@prepor.dev">mail</a>"#),
            r#"<a href="mailto:me@prepor.dev">mail</a>"#
        );
        assert_eq!(
            sanitize(r#"<a href="javascript:evil()">click</a>"#),
            "click"
        );
        assert_eq!(sanitize(r#"<a href="cid:part1.abc">inline</a>"#), "inline");
        // A quote in the value cannot break out of the attribute.
        assert!(!sanitize(r#"<a href='https://x.dev/"onmouseover=1'>x</a>"#)
            .contains(r#"/"onmouseover"#));
    }

    /// Images leave; their alt text is the only prose they carried, and a
    /// tracking pixel has none.
    #[test]
    fn images_reduce_to_alt_text() {
        assert_eq!(
            sanitize(r#"<p><img src="u" alt="A chart"></p>"#),
            "<p>A chart</p>"
        );
        assert_eq!(
            sanitize(r#"<p>a<img src="https://t.co/px.gif" width="1" height="1">b</p>"#),
            "<p>ab</p>"
        );
    }

    /// Entities are makepad's to decode, so they cross unchanged — decoding
    /// here would only mean escaping them again.
    #[test]
    fn entities_are_left_alone() {
        assert_eq!(
            sanitize("<p>a &amp; b &nbsp; c &#39;</p>"),
            "<p>a &amp; b &nbsp; c &#39;</p>"
        );
    }

    /// Whitespace collapses like HTML, except inside `<pre>`.
    #[test]
    fn whitespace_collapses_outside_pre() {
        assert_eq!(sanitize("<p>a   \n  b</p>"), "<p>a b</p>");
        assert_eq!(sanitize("<pre>a   \n  b</pre>"), "<pre>a   \n  b</pre>");
        assert_eq!(sanitize("<b>a</b> <b>b</b>"), "<b>a</b> <b>b</b>");
    }

    /// Malformed input is the common case, not the exception: unbalanced
    /// closes, an unfinished tag, a bare `<`.
    #[test]
    fn malformed_input_does_not_panic() {
        assert_eq!(sanitize("</p>stray"), "stray");
        assert_eq!(sanitize("<p>unclosed"), "<p>unclosed</p>");
        assert_eq!(sanitize(""), "");
        // A '<' is prose unless a letter follows it, so arithmetic and
        // affection both survive.
        assert_eq!(sanitize("a < b"), "a &lt; b");
        assert_eq!(sanitize("<p>trailing <"), "<p>trailing &lt;</p>");
        assert_eq!(sanitize("I <3 it"), "I &lt;3 it");
        // A tag the document ended inside is dropped, not spelled out.
        assert_eq!(sanitize("<div"), "");
        assert_eq!(sanitize("<p>said</p><a href=\"http://x.dev"), "<p>said</p>");
        // Including one that would have been dropped whole anyway.
        assert_eq!(sanitize("<p>ok</p><script>var a ="), "<p>ok</p>");
        // Comments, including Outlook's conditional flavour.
        assert_eq!(
            sanitize("<!--[if mso]><p>x</p><![endif]--><p>y</p>"),
            "<p>y</p>"
        );
        assert_eq!(sanitize("<!DOCTYPE html><p>z</p>"), "<p>z</p>");
    }

    /// `<br>` collapses with the breaks its neighbours owe, so a wrapped
    /// paragraph does not gain blank lines.
    #[test]
    fn breaks_collapse() {
        assert_eq!(sanitize("a<br>b"), "a<br>b");
        assert_eq!(sanitize("<div>a</div><br><div>b</div>"), "a<br>b");
        assert_eq!(sanitize("<br><br><p>x</p>"), "<p>x</p>");
    }
}

