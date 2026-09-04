//! The narrowing, checked against the shapes real mail arrives in.
//!
//! Every case here came out of a letter: a composer's surrogate pair, a
//! newsletter's preheader, a table holding a page together, a pixel counting
//! the open. What the assertions pin is not the tags but the reading.

use super::*;

/// A numeric reference the widget's parser cannot decode is a crash, not
/// a rendering bug: it unwraps `char::from_u32`. The commonest source is
/// an emoji written as its UTF-16 surrogate pair, which is what a real
/// mail in the wild turned out to contain — and which html5ever would
/// otherwise read as two U+FFFD.
#[test]
fn surrogate_pairs_are_put_back_together() {
    // &#55357;&#56538; is 📚, &#55357;&#56960; is 🚀 — as sent.
    assert_eq!(sanitize("&#55357;&#56538;"), "📚");
    assert_eq!(sanitize("<p>&#55357;&#56960; go</p>"), "<p>🚀 go</p>");
    // Hex spelling of the same thing.
    assert_eq!(sanitize("&#xD83D;&#xDE80;"), "🚀");
    // The draw-time guard does the same for rows narrowed by old builds.
    assert_eq!(guard("<p>&#55357;&#56960;</p>"), "<p>🚀</p>");
    assert!(matches!(
        guard("<p>fine &amp; &#233;</p>"),
        std::borrow::Cow::Borrowed(_)
    ));
}

/// Anything else out of range is replaced rather than dropped, so the
/// text keeps its shape and nothing downstream sees a bare surrogate.
#[test]
fn unpaired_or_out_of_range_references_become_replacement() {
    assert_eq!(sanitize("a&#55357;b"), "a\u{FFFD}b");
    assert_eq!(sanitize("a&#56538;b"), "a\u{FFFD}b", "lone low half");
    assert_eq!(
        sanitize("a&#1114112;b"),
        "a\u{FFFD}b",
        "past the last plane"
    );
    assert_eq!(sanitize("a&#99999999999;b"), "a\u{FFFD}b", "past u32 too");
    assert_eq!(sanitize("a&#-1;b"), "a\u{FFFD}b", "negative");
    // A high surrogate followed by something that is not its low half.
    assert_eq!(sanitize("&#55357;&#65;"), "\u{FFFD}A");
}

/// Text is decoded on the way in and escaped again on the way out: only
/// the three characters the widget's parser reads as markup are spelled
/// as entities, and nothing else can reach it as one.
#[test]
fn entities_decode_on_the_way_in() {
    assert_eq!(sanitize("<p>a &amp; b</p>"), "<p>a &amp; b</p>");
    assert_eq!(sanitize("<p>&#60;tag&#62;</p>"), "<p>&lt;tag&gt;</p>");
    assert_eq!(
        sanitize("<p>&#x1F4DA; caf&#233; &mdash;</p>"),
        "<p>📚 café —</p>"
    );
    assert_eq!(sanitize("<p>a &nbsp; b &#39;</p>"), "<p>a b '</p>");
    // Not a reference at all: kept as the text it is.
    assert_eq!(
        sanitize("<p>a &# b &#; c</p>"),
        "<p>a &amp;# b &amp;#; c</p>"
    );
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
    assert_eq!(
        sanitize("<h2>Title</h2><p>x<sup>2</sup> H<sub>2</sub>O</p>"),
        "<h2>Title</h2><p>x<sup>2</sup> H<sub>2</sub>O</p>"
    );
    assert_eq!(
        sanitize("<p><strong>a</strong> <em>b</em> <del>c</del></p>"),
        "<p><b>a</b> <i>b</i> <s>c</s></p>"
    );
    assert_eq!(sanitize("<p>a</p><hr><p>b</p>"), "<p>a</p><hr><p>b</p>");
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
    assert_eq!(
        sanitize("<title>t</title><select><option>o</option></select>x"),
        "x"
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
    // A block separates itself: no break is added around it.
    assert_eq!(sanitize("<div>a</div><p>b</p><div>c</div>"), "a<p>b</p>c");
}

/// The measure's reading of a narrowed letter: tags out, the tags that
/// stand on their own line become breaks, cells a space apart, and an
/// entity counts as the one character it decodes to.
#[test]
fn plain_reduces_the_letter_to_lines() {
    assert_eq!(plain(&sanitize("<div>one</div><div>two</div>")), "one\ntwo");
    assert_eq!(plain("<p>a</p><ul><li>x</li><li>y</li></ul>"), "a\nx\ny");
    assert_eq!(plain("first<br>second"), "first\nsecond");
    // Emphasis is not a line of its own; a paragraph is.
    assert_eq!(plain("<p>plain <b>bold</b> tail</p>"), "plain bold tail");
    // Seven characters of entity read as one.
    assert_eq!(plain("a&mdash;b"), "a\u{b7}b");
    assert_eq!(plain("bare & ampersand"), "bare & ampersand");
    assert_eq!(
        plain("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"),
        "a b\nc d"
    );
}

/// Unknown and inline-only tags vanish without touching the text.
#[test]
fn unknown_tags_unwrap_silently() {
    assert_eq!(sanitize("<span style='color:red'>red</span>"), "red");
    assert_eq!(sanitize("<font size=3>big</font>"), "big");
    assert_eq!(sanitize("<o:p>outlook</o:p>"), "outlook");
    assert_eq!(
        sanitize("<center><small>fine</small> <abbr>print</abbr></center>"),
        "fine print"
    );
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
    // A quote in the value cannot break out of the attribute, and an
    // ampersand is spelled for a parser that decodes attribute values.
    assert!(
        !sanitize(r#"<a href='https://x.dev/"onmouseover=1'>x</a>"#).contains(r#"/"onmouseover"#)
    );
    assert_eq!(
        sanitize(r#"<a href="https://x.dev/?a=1&amp;b=2">q</a>"#),
        r#"<a href="https://x.dev/?a=1&amp;b=2">q</a>"#
    );
}

/// A styled link keeps its words. The widget hands a link the first text
/// it finds under the `<a>`, and a tag in between leaves an empty text
/// node there — so the styles wrap the link and not the other way round,
/// and an underlined newsletter link still says what it links to.
#[test]
fn a_styled_link_keeps_its_text() {
    assert_eq!(
        sanitize(r#"<a href="https://x.dev" style="text-decoration: underline">go</a>"#),
        r#"<u><a href="https://x.dev">go</a></u>"#
    );
    assert_eq!(
        sanitize(r#"<b><a href="https://x.dev"><i>go</i></a></b>"#),
        r#"<b><i><a href="https://x.dev">go</a></i></b>"#
    );
    // Style that changes inside the link splits it into two items, each
    // holding its own words rather than one holding none.
    assert_eq!(
        sanitize(r#"<a href="https://x.dev">read <b>this</b></a>"#),
        r#"<a href="https://x.dev">read</a> <b><a href="https://x.dev">this</a></b>"#
    );
}

/// A link is one item to the widget: it cannot span lines, so a link
/// around block content (a newsletter's button) is re-opened on each
/// line, and one with no text at all vanishes.
#[test]
fn links_are_reopened_per_line() {
    assert_eq!(
        sanitize(r#"<a href="https://x.dev"><div>one</div><div>two</div></a>"#),
        r#"<a href="https://x.dev">one</a><br><a href="https://x.dev">two</a>"#
    );
    assert_eq!(sanitize(r#"<a href="https://x.dev"></a>after"#), "after");
    // An image inside a link is placed beside it with the link on its
    // own tag: the widget's link is a text item and cannot hold one,
    // so the picture is the link, and the text after it links again.
    assert_eq!(
        sanitize(
            r#"<a href="https://x.dev"><img src="https://x.dev/b.png" alt="View the run"> go</a>"#
        ),
        r#"<img src="https://x.dev/b.png" href="https://x.dev" alt="View the run"/> <a href="https://x.dev">go</a>"#
    );
    assert_eq!(
        sanitize(r#"<a href="https://x.dev"><img src="https://x.dev/b.png"></a>"#),
        r#"<img src="https://x.dev/b.png" href="https://x.dev"/>"#
    );
    // A link the reader could not have meant is not carried either, and
    // an image with no source to show is its alt text, linked.
    assert_eq!(
        sanitize(r#"<a href="javascript:x()"><img src="https://x.dev/b.png"></a>"#),
        r#"<img src="https://x.dev/b.png"/>"#
    );
    assert_eq!(
        sanitize(r#"<a href="https://x.dev"><img src="btn.png" alt="View the run"></a>"#),
        r#"<a href="https://x.dev">View the run</a>"#
    );
}

/// An image whose source the panel can show stays an image — its alt
/// text and size hints along, the scheme normalised — and any other is
/// its alt text. A tracking pixel, a box of a few pixels by attribute
/// or by style, leaves nothing even when it carried an alt.
#[test]
fn images_stay_when_they_can_be_shown() {
    assert_eq!(
        sanitize(r#"<p><img src="https://x.dev/a.png" alt="A chart" width="300"></p>"#),
        r#"<p><img src="https://x.dev/a.png" alt="A chart" width="300"/></p>"#
    );
    assert_eq!(
        sanitize(r#"<img src=" CID:part1@x " alt="">"#),
        r#"<img src="cid:part1@x"/>"#
    );
    assert_eq!(
        sanitize(r#"<img src="data:image/png;base64,iVBORw0KGgo=" alt="dot">"#),
        r#"<img src="data:image/png;base64,iVBORw0KGgo=" alt="dot"/>"#
    );
    assert_eq!(
        sanitize(r#"<img src="file:///etc/hosts" alt="local">"#),
        "local"
    );
    assert_eq!(
        sanitize(r#"<p><img src="u" alt="A chart"></p>"#),
        "<p>A chart</p>"
    );
    let big = format!(
        r#"<img src="data:image/png;base64,{}" alt="big">"#,
        "A".repeat(MAX_DATA_URI)
    );
    assert_eq!(sanitize(&big), "big");
    // Percent widths are not size hints.
    assert_eq!(
        sanitize(r#"<img src="https://x.dev/a.png" width="100%" height="auto">"#),
        r#"<img src="https://x.dev/a.png"/>"#
    );
    // Around an image, the spacing a run would get.
    assert_eq!(
        sanitize(r#"<p>see <img src="https://x.dev/a.png"> here</p>"#),
        r#"<p>see <img src="https://x.dev/a.png"/> here</p>"#
    );
    assert_eq!(
        sanitize(r#"<div>a</div><div><img src="https://x.dev/a.png"></div>"#),
        r#"a<br><img src="https://x.dev/a.png"/>"#
    );
    // Pixels, by attribute or by style, with or without an alt.
    assert_eq!(
        sanitize(r#"<p>a<img src="https://t.co/px.gif" width="1" height="1">b</p>"#),
        "<p>ab</p>"
    );
    assert_eq!(
        sanitize(r#"<img src="o.gif" width="1" height="1" alt="Open tracker">x"#),
        "x"
    );
    assert_eq!(
        sanitize(r#"<img src="o.gif" style="width:1px;height:1px" alt="hidden">x"#),
        "x"
    );
    assert_eq!(
        sanitize(r#"<img src="o.gif" width="0" alt="spacer">x"#),
        "x"
    );
}

/// Whitespace collapses like HTML, except inside `<pre>`; between two
/// runs of the same emphasis a space stays inside it.
#[test]
fn whitespace_collapses_outside_pre() {
    assert_eq!(sanitize("<p>a   \n  b</p>"), "<p>a b</p>");
    assert_eq!(sanitize("<pre>a   \n  b</pre>"), "<pre>a   \n  b</pre>");
    assert_eq!(sanitize("<b>a</b> <b>b</b>"), "<b>a b</b>");
    assert_eq!(sanitize("<b>a</b> <i>b</i>"), "<b>a</b> <i>b</i>");
    // `<code>` inside `<pre>` would only add a background to a face the
    // widget is already in.
    assert_eq!(sanitize("<pre><code>x</code></pre>"), "<pre>x</pre>");
}

/// `white-space: pre-wrap` is how several composers keep the author's
/// line breaks without a `<br>` per line: the newlines are lines, the
/// rest of the whitespace still collapses.
#[test]
fn white_space_pre_keeps_the_lines() {
    assert_eq!(
        sanitize("<div style=\"white-space:pre-wrap\">one\ntwo\n\n\nthree</div>"),
        "one<br>two<p></p>three"
    );
    assert_eq!(
        sanitize("<div style=\"white-space: pre-line\">a   b</div>"),
        "a b"
    );
    assert_eq!(
        sanitize(
            "<div style=\"white-space:pre\"><span style=\"white-space:normal\">a\nb</span></div>"
        ),
        "a b"
    );
}

/// Malformed input is the common case, not the exception: unbalanced
/// closes, an unfinished tag, a bare `<`, tags never closed.
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
    // Implied end tags: the tree a browser builds, not the tags as sent.
    assert_eq!(sanitize("<p>one<p>two"), "<p>one</p><p>two</p>");
    assert_eq!(
        sanitize("<ul><li>a<li>b</ul>"),
        "<ul><li>a</li><li>b</li></ul>"
    );
    assert_eq!(
        sanitize("<b>bold <i>both</b> italic</i>"),
        "<b>bold <i>both</i></b> <i>italic</i>"
    );
}

/// Nesting is capped, not trusted: a document nested past any real
/// composer's depth is unwrapped from there on rather than recursed
/// into.
#[test]
fn nesting_is_capped() {
    let deep = format!("{}deep{}", "<div>".repeat(5_000), "</div>".repeat(5_000));
    assert_eq!(sanitize(&deep), "deep");
    let deep = format!("{}x{}", "<b>".repeat(3_000), "</b>".repeat(3_000));
    assert_eq!(sanitize(&deep), "<b>x</b>");
}

/// `<br>` collapses with the breaks its neighbours owe, so a wrapped
/// paragraph does not gain blank lines — but a blank line the author
/// made (`<br><br>`, Gmail's `<div><br></div>`) is kept, as one, spelled
/// as the empty paragraph the widget gives height to.
#[test]
fn breaks_collapse() {
    assert_eq!(sanitize("a<br>b"), "a<br>b");
    assert_eq!(sanitize("<div>a<br></div><div>b<br></div>"), "a<br>b");
    assert_eq!(
        sanitize("<div>a</div><div><br></div><div>b</div>"),
        "a<p></p>b"
    );
    assert_eq!(sanitize("a<br><br><br><br>b"), "a<p></p>b");
    assert_eq!(sanitize("<br><br><p>x</p>"), "<p>x</p>");
    assert_eq!(sanitize("<p>a</p><br><p>b</p>"), "<p>a</p><p>b</p>");
    assert_eq!(sanitize("<p>a</p><div>b</div>"), "<p>a</p>b");
}

/// Empty blocks leave nothing: Outlook's `<p>&nbsp;</p>` blank lines,
/// an empty list item, a paragraph of nothing but space.
#[test]
fn empty_blocks_vanish() {
    assert_eq!(sanitize("<p></p><p> </p><p>&nbsp;</p><p>x</p>"), "<p>x</p>");
    assert_eq!(sanitize("<ul><li></li></ul>"), "");
    assert_eq!(sanitize("<blockquote><p></p></blockquote>after"), "after");
}

/// The author's stylesheet is read for what it hides: `display: none`
/// by class, the preheader tricks, `visibility`, `opacity`,
/// `mso-hide`, the `hidden` attribute — each taking its subtree with it.
#[test]
fn the_stylesheet_hides_what_it_hides() {
    assert_eq!(
        sanitize("<html><head><style>.pre{display:none} p.x{font-weight:bold}</style></head>\
                  <body><div class=\"pre\">preview text</div><p class=\"x\">shown</p></body></html>"),
        "<p><b>shown</b></p>"
    );
    assert_eq!(
        sanitize(
            r#"<div style="display:none;font-size:1px;color:#fff;max-height:0;overflow:hidden">Preview</div>a"#
        ),
        "a"
    );
    assert_eq!(
        sanitize(r#"<div style="max-height:0px;overflow:hidden">Preview</div>a"#),
        "a"
    );
    assert_eq!(
        sanitize(r#"<div style="height:0;overflow:hidden">Preview</div>a"#),
        "a"
    );
    assert_eq!(
        sanitize(
            r#"<div style="visibility:hidden">x</div><span style="opacity:0">y</span><span style="mso-hide:all">z</span>a"#
        ),
        "a"
    );
    assert_eq!(sanitize("<p hidden>x</p><p>y</p>"), "<p>y</p>");
    assert_eq!(
        sanitize(r#"<div style="display:none"><p>a</p></div><p>b</p>"#),
        "<p>b</p>"
    );
    // A descendant selector, and a rule inside `@media`, which is skipped.
    assert_eq!(
        sanitize(
            "<style>td b{font-style:italic} @media (max-width:600px){.m{display:none}}</style>\
                  <table><tr><td><b>x</b></td></tr></table><span class=\"m\">shown</span>"
        ),
        "<b><i>x</i></b><br>shown"
    );
}

/// Text too small or too pale to read is not shown either — but as an
/// inherited property, so a child that sets its own size (the
/// `font-size: 0` wrapper around inline-block columns) is.
#[test]
fn unreadable_text_is_not_shown() {
    assert_eq!(
        sanitize(
            r#"<span style="font-size:1px">tiny</span> <span style="color:transparent">clear</span>a"#
        ),
        "a"
    );
    assert_eq!(
        sanitize(
            r#"<div style="font-size:0"><span style="font-size:14px">a</span> <span style="font-size:0.9em">b</span></div>"#
        ),
        "a b"
    );
    assert_eq!(
        sanitize(r#"<p style="font-size:1px">Preheader</p><p>x</p>"#),
        "<p>x</p>"
    );
}

/// The cascade in the order the author meant it: a rule, then the
/// `style` attribute over it, then `!important` over that.
#[test]
fn the_cascade_is_ordered() {
    assert_eq!(
        sanitize(
            r#"<style>.a{font-weight:bold}</style><span class="a" style="font-weight:normal">x</span>"#
        ),
        "x"
    );
    assert_eq!(
        sanitize(
            r#"<style>.a{display:none !important}</style><span class="a" style="display:block">x</span>"#
        ),
        ""
    );
    assert_eq!(
        sanitize(
            r#"<style>span{font-weight:bold} .a{font-weight:normal}</style><span class="a">x</span>"#
        ),
        "x"
    );
    assert_eq!(
        sanitize(r#"<style>#t{font-style:italic}</style><span id="t">x</span>"#),
        "<i>x</i>"
    );
}

/// Emphasis is decided per run, whatever spelled it: a tag, a `style`
/// attribute, a rule. So a `font-weight: normal` inside a `<b>` is
/// really not bold, which no tag-for-tag copy could say.
#[test]
fn styled_spans_become_emphasis() {
    assert_eq!(
        sanitize(r#"<span style="font-weight:bold">x</span>"#),
        "<b>x</b>"
    );
    assert_eq!(
        sanitize(r#"<span style="font-weight:700">x</span>"#),
        "<b>x</b>"
    );
    assert_eq!(sanitize(r#"<span style="font-weight:400">x</span>"#), "x");
    assert_eq!(
        sanitize(r#"<span style="font-style:italic">x</span>"#),
        "<i>x</i>"
    );
    assert_eq!(
        sanitize(r#"<span style="text-decoration: underline line-through">x</span>"#),
        "<u><s>x</s></u>"
    );
    assert_eq!(sanitize(r#"<u style="text-decoration:none">x</u>"#), "x");
    assert_eq!(
        sanitize(r#"<span style="font-family: Menlo, monospace">x</span>"#),
        "<code>x</code>"
    );
    assert_eq!(
        sanitize(r#"<span style="vertical-align:super">2</span>"#),
        "<sup>2</sup>"
    );
    assert_eq!(
        sanitize(r#"<b>a <span style="font-weight:normal">b</span> c</b>"#),
        "<b>a</b> b <b>c</b>"
    );
    // Nested the one way, whatever order the tags came in.
    assert_eq!(sanitize("<i><b>x</b></i>"), "<b><i>x</i></b>");
    assert_eq!(sanitize("<b><b>x</b></b>"), "<b>x</b>");
    // Emphasis does not cross a block; each paragraph carries its own.
    assert_eq!(
        sanitize("<b><p>a</p><p>b</p></b>"),
        "<p><b>a</b></p><p><b>b</b></p>"
    );
}

/// A table with something to tabulate keeps its grid — a header row of
/// `<th>`, or just two short columns over two or more rows — with a
/// spanning cell padded out to the column count.
#[test]
fn data_tables_keep_their_grid() {
    assert_eq!(
        sanitize(
            "<table border=\"1\"><tr><th>Item</th><th>Qty</th></tr>\
                  <tr><td>Apples</td><td>3</td></tr><tr><td>Pears</td><td>12</td></tr></table>"
        ),
        "<table><thead><tr><th>Item</th><th>Qty</th></tr></thead>\
         <tr><td>Apples</td><td>3</td></tr><tr><td>Pears</td><td>12</td></tr></table>"
    );
    assert_eq!(
        sanitize("<table><tbody><tr><td>Subtotal</td><td>$40</td></tr><tr><td>Tax</td><td>$2</td></tr></tbody></table>"),
        "<table><tr><td>Subtotal</td><td>$40</td></tr><tr><td>Tax</td><td>$2</td></tr></table>"
    );
    assert_eq!(
        sanitize(
            "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr>\
                  <tr><td colspan=\"2\"><b>Total</b></td></tr></table>"
        ),
        "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr>\
         <tr><td><b>Total</b></td><td></td></tr></table>"
    );
}

/// Everything else a table does is layout, and layout is lines: a
/// declared presentation role, a single column, block content or long
/// text in a cell, a nested table, uneven rows.
#[test]
fn layout_tables_are_flattened() {
    let two_by_two = |attrs: &str, a: &str| {
        format!(
            "<table {attrs}><tr><td>{a}</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"
        )
    };
    assert_eq!(
        sanitize(&two_by_two("role=\"presentation\"", "a")),
        "a b<br>c d"
    );
    assert_eq!(sanitize(&two_by_two("", "<p>a</p>")), "<p>a</p>b<br>c d");
    let long = "x".repeat(MAX_CELL + 1);
    assert_eq!(
        sanitize(&two_by_two("", &long)),
        format!("{long}<br>b<br>c d")
    );
    assert_eq!(
        sanitize(
            "<table><tr><td><table><tr><td>x</td></tr></table></td><td>b</td></tr>\
                  <tr><td>c</td><td>d</td></tr></table>"
        ),
        "x<br>b<br>c d"
    );
    assert_eq!(
        sanitize("<table width=\"100%\"><tr><td><b>Head</b></td></tr><tr><td><p>Body</p></td></tr></table>"),
        "<b>Head</b><p>Body</p>"
    );
    // Short cells share a line; a long one takes its own.
    assert_eq!(
        sanitize("<table><tr><td>Order</td><td>#123</td><td>$42</td></tr></table>"),
        "Order #123 $42"
    );
    let long = "y".repeat(MAX_JOINED_CELL + 1);
    assert_eq!(
        sanitize(&format!(
            "<table><tr><td>{long}</td><td>b</td></tr></table>"
        )),
        format!("{long}<br>b")
    );
}

/// The attributes the widget reads survive, checked: a list's start and
/// numbering, an item's value, a fold's initial state.
#[test]
fn block_attributes_the_widget_reads_survive() {
    assert_eq!(
        sanitize("<ol start=\"3\" type=\"a\"><li value=\"7\">x</li></ol>"),
        "<ol start=\"3\" type=\"a\"><li value=\"7\">x</li></ol>"
    );
    assert_eq!(
        sanitize("<ol start=\"x\" type=\"disc\"><li>x</li></ol>"),
        "<ol><li>x</li></ol>"
    );
    assert_eq!(
        sanitize("<details open><summary>More</summary><p>x</p></details>"),
        "<details open><summary>More</summary><p>x</p></details>"
    );
    assert_eq!(
        sanitize("<dl><dt>Term</dt><dd>Its meaning</dd></dl>"),
        "<b>Term</b><br>Its meaning"
    );
}

/// Past the budget the letter is cut and says so, with every open tag
/// closed: the widget lays out everything it is given, every frame.
#[test]
fn long_input_is_cut_and_said_so() {
    let big = "<p>".to_string()
        + &"word word word word word word word word word word ".repeat(400)
        + "</p>";
    let h = sanitize(&big.repeat(80));
    assert!(h.len() <= MAX_OUT + CUT.len() + 16, "{}", h.len());
    assert!(h.ends_with(&format!("</p>{CUT}")), "{}", &h[h.len() - 80..]);
    // Well under it, nothing is said.
    assert!(!sanitize(&big).contains("<hr>"));
}

/// The measure counts an image as lines of its own — its height when it
/// said one, a guess when not — the first reading as its alt text.
#[test]
fn plain_counts_images_as_lines() {
    assert_eq!(plain(r#"a<img src="x" height="32"/>b"#), "a\n·\n·\nb");
    assert_eq!(
        plain(r#"<img src="x" alt="the badge" height="20"/>"#),
        "the badge\n·"
    );
    assert_eq!(
        plain(r#"<p>a</p><img src="x"/>"#).lines().count(),
        1 + IMG_LINES
    );
}

/// `cid:` sources are scoped to their letter before the widget sees
/// them, so two open letters cannot answer for each other's parts.
#[test]
fn cid_sources_are_scoped_to_their_letter() {
    assert_eq!(
        scope_cids(
            r#"<img src="cid:a@x"/><img src="https://x.dev/a.png"/>"#,
            "m7"
        ),
        r#"<img src="cid:m7/a@x"/><img src="https://x.dev/a.png"/>"#
    );
    assert!(matches!(
        scope_cids("<p>none</p>", "m7"),
        std::borrow::Cow::Borrowed(_)
    ));
}

#[test]
fn base64_decodes_what_data_images_carry() {
    assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
    assert_eq!(base64_decode("aGVs\nbG8").as_deref(), Some(&b"hello"[..]));
    assert_eq!(base64_decode("-_8").as_deref(), Some(&[0xfb, 0xff][..]));
    assert_eq!(base64_decode("not base64!"), None);
}
