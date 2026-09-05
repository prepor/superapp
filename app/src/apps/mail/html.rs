//! Converts outside HTML into the limited markup Makepad's `Html` widget
//! draws.
//!
//! The parser keeps useful text structure and a small set of styles. It
//! removes scripts, frames, hidden content, unsafe link schemes, and
//! unsupported styling. Images remain only when the panel can load their
//! source; tiny tracking images and unsupported sources become alternative
//! text or nothing. Input and output limits keep large or hostile messages
//! from blocking layout.
//!
//! [`sanitize`] runs at **ingest** and the result is stored, so a reading is
//! only as good as the build that wrote it — which is why [`VERSION`] is a
//! `Step::Derived` version in [`schema`](super::schema): bump it and every
//! store narrows its letters again from `raw` on its next open.
//!
//! The repairs a stored reading still needs at the point of use, and the
//! base64 a `data:` image carries its bytes in, are in [`entities`].

use std::fmt::Write as _;

use html5ever::tendril::TendrilSink;
use html5ever::tree_builder::TreeBuilderOpts;
use html5ever::ParseOpts;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use simplecss::{AttributeOperator, Declaration, DeclarationTokenizer, PseudoClass, StyleSheet};

/// The shape of what [`sanitize`] writes. It runs at ingest and the result
/// is stored, so a reading is only as good as the build that wrote it:
/// bump this whenever the narrowing changes what it keeps or how, and the
/// store redoes every reading it holds from raw on its next open.
pub const VERSION: u32 = 5;

/// Input past this is cut before parsing: a letter is not a website, and a
/// multi-megabyte one is a mistake or an attack.
const MAX_IN: usize = 4 << 20;

/// Output past this is cut, and the cut is said. FairEmail's
/// `MAX_FORMAT_TEXT_SIZE`: the widget lays out everything it is given,
/// every frame it draws.
const MAX_OUT: usize = 100 << 10;

/// What the cut says.
const CUT: &str = "<hr><p>Cut here — the rest is too long to show.</p>";

/// Nesting past this is unwrapped. The walks below recurse; Outlook nests
/// deep but finitely, a crafted document does not.
const MAX_DEPTH: usize = 200;

/// A stylesheet past this is not read further.
const MAX_CSS: usize = 256 << 10;

/// An image whose box is this small (px²) or smaller counts opens, not
/// content, and its alt text leaves with it — FairEmail's
/// `TRACKING_PIXEL_SURFACE`.
const PIXEL_SURFACE: f64 = 25.0;

/// Link schemes a reader could plausibly have meant. Everything else —
/// `javascript:`, `data:`, the `cid:` that points at an attachment we do
/// not fetch — loses the href and keeps only its text.
const SCHEMES: &[&str] = &["http://", "https://", "mailto:"];

/// Image sources the panel can show: an attachment of the letter itself,
/// an image embedded in the source, or one on the web.
const IMG_SCHEMES: &[&str] = &["cid:", "http://", "https://", "data:image/"];

/// An embedded image past this stays alt text: it would be stored with the
/// reading, and the raw already keeps it.
const MAX_DATA_URI: usize = 1 << 20;

/// What an image of unknown height counts as in the plain measure.
const IMG_LINES: usize = 6;

/// The widest table worth a grid, the longest a grid cell may run, and the
/// longest a layout cell may be to share its line with the next one.
const MAX_COLS: usize = 8;
const MAX_CELL: usize = 80;
/// A labeled data column can hold a brief description. Layout tables do
/// not earn this allowance just by containing long text.
const MAX_LABELED_CELL: usize = 320;
const MAX_JOINED_CELL: usize = 48;

/// Dropped whole, subtree included: source, head chrome, controls and media
/// the widget cannot draw, whose text — if any — was never prose. `<style>`
/// is not here because its text is read, not shown.
const DROP: &[&str] = &[
    "script", "title", "meta", "link", "base", "noscript", "iframe", "frame", "frameset", "object",
    "embed", "applet", "svg", "math", "canvas", "video", "audio", "source", "track", "template",
    "select", "option", "optgroup", "datalist", "button", "input", "textarea", "map", "area",
    "dialog", "xml",
];

/// Unwrapped, but a line of their own: the tag goes, the text stays,
/// separated from its neighbours by a break. Mail arranges its page with
/// these; the widget draws lines.
const BREAK: &[&str] = &[
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
    "legend",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "caption",
    "hgroup",
    "menu",
    "dir",
];

/// Blocks the widget draws as themselves. `tr` and `table` are handled
/// apart (see [`Walk::table`]); these are emitted lazily, so an empty one —
/// `<p>&nbsp;</p>`, Outlook's blank line — leaves nothing behind.
const BLOCKS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "li",
    "details",
    "summary",
];

// ---------------------------------------------------------------------------
// The tree

/// One node of the parsed document, in an arena: the walks index rather
/// than chase `Rc`s, and depth is capped on the way in.
struct Node {
    kind: Kind,
    parent: Option<usize>,
    children: Vec<usize>,
}

enum Kind {
    Document,
    Element {
        name: String,
        attrs: Vec<(String, String)>,
    },
    Text(String),
}

struct Doc {
    nodes: Vec<Node>,
    /// Every `<style>` in the document, concatenated: the author's rules.
    css: String,
}

impl Doc {
    fn name(&self, i: usize) -> &str {
        match &self.nodes[i].kind {
            Kind::Element { name, .. } => name,
            _ => "",
        }
    }

    fn attrs(&self, i: usize) -> &[(String, String)] {
        match &self.nodes[i].kind {
            Kind::Element { attrs, .. } => attrs,
            _ => &[],
        }
    }

    /// The element children of `i` named one of `names`.
    fn kids(&self, i: usize, names: &[&str]) -> Vec<usize> {
        self.nodes[i]
            .children
            .iter()
            .copied()
            .filter(|&c| names.contains(&self.name(c)))
            .collect()
    }

    /// Whether the subtree under `i` holds nothing but text and inline
    /// markup: what fits in a grid cell, or on a shared line.
    fn inline_only(&self, i: usize) -> bool {
        let mut stack = self.nodes[i].children.clone();
        while let Some(n) = stack.pop() {
            let name = self.name(n);
            if BREAK.contains(&name) || BLOCKS.contains(&name) || matches!(name, "table" | "hr") {
                return false;
            }
            stack.extend(self.nodes[n].children.iter().copied());
        }
        true
    }

    /// Characters of collapsed text under `i`.
    fn text_len(&self, i: usize) -> usize {
        let mut n = 0usize;
        let mut stack = vec![i];
        while let Some(k) = stack.pop() {
            match &self.nodes[k].kind {
                Kind::Text(t) => {
                    n += t
                        .split_whitespace()
                        .map(|w| w.chars().count() + 1)
                        .sum::<usize>();
                }
                _ => stack.extend(self.nodes[k].children.iter().copied()),
            }
        }
        n.saturating_sub(1)
    }
}

/// Parses `src` the way a browser would and copies the result into an
/// arena, dropping on the way what will never be shown.
fn parse(src: &str) -> Doc {
    let opts = ParseOpts {
        tree_builder: TreeBuilderOpts {
            scripting_enabled: false,
            ..TreeBuilderOpts::default()
        },
        ..ParseOpts::default()
    };
    let dom = html5ever::parse_document(RcDom::default(), opts).one(src);
    let mut doc = Doc {
        nodes: vec![Node {
            kind: Kind::Document,
            parent: None,
            children: Vec::new(),
        }],
        css: String::new(),
    };
    // Depth-first, iteratively: (node, arena parent, depth).
    let mut stack: Vec<(Handle, usize, usize)> = dom
        .document
        .children
        .borrow()
        .iter()
        .rev()
        .map(|h| (h.clone(), 0, 1))
        .collect();
    while let Some((h, parent, depth)) = stack.pop() {
        match &h.data {
            NodeData::Text { contents } => {
                let text = contents.borrow().to_string();
                let idx = doc.nodes.len();
                doc.nodes.push(Node {
                    kind: Kind::Text(text),
                    parent: Some(parent),
                    children: Vec::new(),
                });
                doc.nodes[parent].children.push(idx);
            }
            NodeData::Element { name, attrs, .. } => {
                let name = (*name.local).to_ascii_lowercase();
                if name == "style" {
                    for c in h.children.borrow().iter() {
                        if let NodeData::Text { contents } = &c.data {
                            if doc.css.len() < MAX_CSS {
                                doc.css.push_str(&contents.borrow());
                                doc.css.push('\n');
                            }
                        }
                    }
                    continue;
                }
                if DROP.contains(&name.as_str()) {
                    continue;
                }
                // Past the depth cap the element is unwrapped: its children
                // hang from the ancestor that fit.
                let idx = if depth > MAX_DEPTH {
                    parent
                } else {
                    let attrs = attrs
                        .borrow()
                        .iter()
                        .map(|a| ((*a.name.local).to_ascii_lowercase(), a.value.to_string()))
                        .collect();
                    let idx = doc.nodes.len();
                    doc.nodes.push(Node {
                        kind: Kind::Element { name, attrs },
                        parent: Some(parent),
                        children: Vec::new(),
                    });
                    doc.nodes[parent].children.push(idx);
                    idx
                };
                for c in h.children.borrow().iter().rev() {
                    stack.push((c.clone(), idx, depth + 1));
                }
            }
            _ => {}
        }
    }
    doc
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// The stylesheet

/// An element as simplecss sees it, for selector matching.
#[derive(Clone, Copy)]
struct El<'a> {
    doc: &'a Doc,
    i: usize,
}

impl simplecss::Element for El<'_> {
    fn parent_element(&self) -> Option<Self> {
        let p = self.doc.nodes[self.i].parent?;
        matches!(self.doc.nodes[p].kind, Kind::Element { .. }).then_some(El {
            doc: self.doc,
            i: p,
        })
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let p = self.doc.nodes[self.i].parent?;
        let sibs = &self.doc.nodes[p].children;
        let at = sibs.iter().position(|&c| c == self.i)?;
        sibs[..at]
            .iter()
            .rev()
            .copied()
            .find(|&c| matches!(self.doc.nodes[c].kind, Kind::Element { .. }))
            .map(|i| El { doc: self.doc, i })
    }

    fn has_local_name(&self, name: &str) -> bool {
        self.doc.name(self.i).eq_ignore_ascii_case(name)
    }

    fn attribute_matches(&self, local_name: &str, operator: AttributeOperator<'_>) -> bool {
        let want = local_name.to_ascii_lowercase();
        attr(self.doc.attrs(self.i), &want).is_some_and(|v| operator.matches(v))
    }

    fn pseudo_class_matches(&self, class: PseudoClass<'_>) -> bool {
        match class {
            PseudoClass::FirstChild => self.prev_sibling_element().is_none(),
            PseudoClass::Link => {
                self.doc.name(self.i) == "a" && attr(self.doc.attrs(self.i), "href").is_some()
            }
            _ => false,
        }
    }
}

/// What the cascade said about one element, reduced to what can be drawn.
/// `Some` is a value the author set; `None` inherits.
#[derive(Default)]
struct Css {
    display_none: bool,
    visibility_hidden: bool,
    opacity_zero: bool,
    mso_hide: bool,
    overflow_hidden: bool,
    max_height_zero: bool,
    width_zero: bool,
    height_zero: bool,
    /// Box size in px, for the tracking-pixel test.
    width: Option<f64>,
    height: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    sub: Option<bool>,
    sup: Option<bool>,
    pre: Option<bool>,
    /// Text too small to read, or painted in no colour: hidden, but
    /// inheritably so, since a child may set a size of its own (the
    /// `font-size: 0` wrapper around inline-block columns).
    tiny: Option<bool>,
    transparent: Option<bool>,
}

impl Css {
    /// The element and everything in it is not shown.
    fn hidden(&self) -> bool {
        self.display_none
            || self.visibility_hidden
            || self.opacity_zero
            || self.mso_hide
            || (self.overflow_hidden
                && (self.max_height_zero || self.width_zero || self.height_zero))
    }

    fn set(&mut self, d: &Declaration<'_>) {
        let name = d.name.trim().to_ascii_lowercase();
        let value = d.value.trim().to_ascii_lowercase();
        let v = value.as_str();
        match name.as_str() {
            "display" => self.display_none = v == "none",
            "visibility" => self.visibility_hidden = matches!(v, "hidden" | "collapse"),
            "opacity" => self.opacity_zero = num(v) == Some(0.0),
            "mso-hide" => self.mso_hide = v == "all",
            "overflow" | "overflow-x" | "overflow-y" => {
                self.overflow_hidden = matches!(v, "hidden" | "clip")
            }
            "max-height" => self.max_height_zero = num(v) == Some(0.0),
            "width" => {
                self.width_zero = num(v) == Some(0.0);
                self.width = px(v);
            }
            "height" => {
                self.height_zero = num(v) == Some(0.0);
                self.height = px(v);
            }
            "font-size" => self.tiny = Some(tiny(v)),
            "color" => {
                self.transparent = Some(
                    v == "transparent"
                        || (v.starts_with("rgba(") && v.replace(' ', "").ends_with(",0)")),
                )
            }
            "font-weight" => {
                if let Some(b) = weight(v) {
                    self.bold = Some(b);
                }
            }
            "font-style" => match v {
                "italic" | "oblique" => self.italic = Some(true),
                "normal" => self.italic = Some(false),
                _ => {}
            },
            "text-decoration" | "text-decoration-line" => {
                if v.starts_with("none") {
                    self.underline = Some(false);
                    self.strike = Some(false);
                } else {
                    if v.contains("underline") {
                        self.underline = Some(true);
                    }
                    if v.contains("line-through") {
                        self.strike = Some(true);
                    }
                }
            }
            // The reader chooses its prose face. A sender's Courier is
            // often branding for a whole receipt, not computer code;
            // only semantic code tags opt into the fixed face below.
            "font-family" => {}
            "white-space" => match v {
                "pre" | "pre-wrap" | "pre-line" | "break-spaces" => self.pre = Some(true),
                "normal" | "nowrap" => self.pre = Some(false),
                _ => {}
            },
            "vertical-align" => match v {
                "sub" => self.sub = Some(true),
                "super" => self.sup = Some(true),
                "baseline" => {
                    self.sub = Some(false);
                    self.sup = Some(false);
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Lays the author's choices over the inherited ones.
    fn apply(&self, fmt: &mut Fmt, ws: &mut Ws) {
        let set = |slot: &mut bool, v: Option<bool>| {
            if let Some(v) = v {
                *slot = v;
            }
        };
        set(&mut fmt.bold, self.bold);
        set(&mut fmt.italic, self.italic);
        set(&mut fmt.underline, self.underline);
        set(&mut fmt.strike, self.strike);
        set(&mut fmt.sub, self.sub);
        set(&mut fmt.sup, self.sup);
        set(&mut fmt.tiny, self.tiny);
        set(&mut fmt.transparent, self.transparent);
        match self.pre {
            Some(true) if *ws != Ws::Pre => *ws = Ws::PreLines,
            Some(false) if *ws == Ws::PreLines => *ws = Ws::Collapse,
            _ => {}
        }
    }
}

/// The leading number of a CSS value: `12px` → 12, `0` → 0, `auto` → none.
fn num(v: &str) -> Option<f64> {
    let end = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(v.len());
    v[..end].parse().ok()
}

/// A length in px (or unitless), else none — `%` and `em` are not sizes.
fn px(v: &str) -> Option<f64> {
    let n = num(v)?;
    let unit =
        v.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-' || c == '+');
    matches!(unit.trim(), "" | "px").then_some(n)
}

/// A font size nobody could read: the preheader's `1px`, the spacer's `0`.
fn tiny(v: &str) -> bool {
    let Some(n) = num(v) else { return false };
    let unit =
        v.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-' || c == '+');
    match unit.trim() {
        "" | "px" | "pt" => n <= 1.0,
        "em" | "rem" => n <= 0.1,
        "%" => n <= 10.0,
        _ => false,
    }
}

fn weight(v: &str) -> Option<bool> {
    match v {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        _ => num(v).map(|n| n >= 600.0),
    }
}

// ---------------------------------------------------------------------------
// The emitter

/// The inline state of a run of text: what the widget's inline tags can say
/// about it, plus the two ways to be invisible without being hidden.
#[derive(Clone, Default, PartialEq)]
struct Fmt {
    /// The link the run sits in, already checked against [`SCHEMES`].
    link: Option<String>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    mono: bool,
    sub: bool,
    sup: bool,
    tiny: bool,
    transparent: bool,
}

impl Fmt {
    /// The inline tags this run needs, in the one order they are nested.
    /// Inside `<pre>` the widget is already in the fixed face, so `<code>`
    /// would only add its background.
    ///
    /// The link goes **innermost**, under every style the run carries. The
    /// widget draws a link as a child widget and hands it the first text it
    /// finds under the `<a>` — and a tag in between leaves an empty text
    /// node there, so `<a><u>go</u></a>` would draw a link that says
    /// nothing. `<u><a>go</a></u>` reads the same and says it.
    fn tags(&self, in_pre: bool) -> Vec<Tag> {
        let mut v = Vec::new();
        if self.bold {
            v.push(Tag::Bold);
        }
        if self.italic {
            v.push(Tag::Italic);
        }
        if self.underline {
            v.push(Tag::Underline);
        }
        if self.strike {
            v.push(Tag::Strike);
        }
        if self.mono && !in_pre {
            v.push(Tag::Code);
        }
        if self.sub {
            v.push(Tag::Sub);
        }
        if self.sup {
            v.push(Tag::Sup);
        }
        if let Some(h) = &self.link {
            v.push(Tag::Link(h.clone()));
        }
        v
    }
}

/// An inline tag the widget draws.
#[derive(Clone, PartialEq, Debug)]
enum Tag {
    Link(String),
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Sub,
    Sup,
}

impl Tag {
    fn open(&self) -> String {
        match self {
            Tag::Link(h) => format!("<a href=\"{}\">", esc_attr(h)),
            Tag::Bold => "<b>".into(),
            Tag::Italic => "<i>".into(),
            Tag::Underline => "<u>".into(),
            Tag::Strike => "<s>".into(),
            Tag::Code => "<code>".into(),
            Tag::Sub => "<sub>".into(),
            Tag::Sup => "<sup>".into(),
        }
    }

    fn close(&self) -> &'static str {
        match self {
            Tag::Link(_) => "</a>",
            Tag::Bold => "</b>",
            Tag::Italic => "</i>",
            Tag::Underline => "</u>",
            Tag::Strike => "</s>",
            Tag::Code => "</code>",
            Tag::Sub => "</sub>",
            Tag::Sup => "</sup>",
        }
    }
}

/// How whitespace in text is read.
#[derive(Clone, Copy, PartialEq, Default)]
enum Ws {
    /// Runs collapse to one space; newlines are spaces.
    #[default]
    Collapse,
    /// `white-space: pre*` outside a `<pre>`: newlines break lines, the
    /// rest collapses.
    PreLines,
    /// Inside `<pre>`: verbatim.
    Pre,
}

/// A block entered but not necessarily written: it is written when the
/// first text inside it arrives, so an empty one leaves nothing.
struct Block {
    name: &'static str,
    open: String,
    opened: bool,
}

/// Accumulates output while holding back whitespace, so a stack of unwrapped
/// `<div>`s costs one break and not five, nothing leading or trailing
/// survives, and inline tags open and close exactly where the text changes.
#[derive(Default)]
struct Out {
    s: String,
    /// Line breaks owed to the next text, 0–2: a second one is a blank
    /// line, a third would be more blank than any letter meant. The widget
    /// gives an empty line no height, so a blank line is written as an
    /// empty paragraph, whose margins it does draw.
    brk: u8,
    /// A space owed likewise; a break outranks it.
    space: bool,
    /// The last thing written was a block's own close tag, which separates
    /// itself: one owed break is already paid.
    after_block: bool,
    /// Inline tags open right now, outermost first.
    fmt: Vec<Tag>,
    /// Blocks entered, outermost first.
    blocks: Vec<Block>,
    /// The output budget is spent; the walk stops descending.
    cut: bool,
    /// Grids being written, nested.
    in_grid: usize,
}

impl Out {
    fn br(&mut self) {
        self.brk = (self.brk + 1).min(2);
    }

    /// A line boundary: an unwrapped block began or ended.
    fn boundary(&mut self) {
        self.brk = self.brk.max(1);
    }

    fn space(&mut self) {
        self.space = true;
    }

    fn enter(&mut self, name: &'static str, open: String) {
        self.blocks.push(Block {
            name,
            open,
            opened: false,
        });
    }

    fn leave(&mut self) {
        let Some(b) = self.blocks.pop() else { return };
        if b.opened {
            self.close_fmt();
            self.s.push_str("</");
            self.s.push_str(b.name);
            self.s.push('>');
            self.brk = 0;
            self.space = false;
            self.after_block = true;
        } else {
            // Held nothing; still, what follows is not on the same line as
            // what came before.
            self.boundary();
        }
    }

    /// Writes the blocks entered but not yet written. Anything owed before
    /// them is void: a block separates itself.
    fn open_blocks(&mut self) {
        if self.blocks.iter().all(|b| b.opened) {
            return;
        }
        self.close_fmt();
        for k in 0..self.blocks.len() {
            if !self.blocks[k].opened {
                let open = std::mem::take(&mut self.blocks[k].open);
                self.s.push_str(&open);
                self.blocks[k].opened = true;
            }
        }
        self.brk = 0;
        self.space = false;
        self.after_block = false;
    }

    /// A void block — `<hr>` — that owes nothing and is owed nothing.
    fn void(&mut self, tag: &str) {
        self.open_blocks();
        self.close_fmt();
        self.s.push('<');
        self.s.push_str(tag);
        self.s.push('>');
        self.brk = 0;
        self.space = false;
        self.after_block = true;
    }

    /// Structure written as-is, eagerly: the grid of a data table.
    fn raw(&mut self, t: &str) {
        self.close_fmt();
        self.s.push_str(t);
        self.brk = 0;
        self.space = false;
        self.after_block = false;
    }

    fn close_fmt(&mut self) {
        while let Some(t) = self.fmt.pop() {
            self.s.push_str(t.close());
        }
    }

    /// One run of text in one inline state.
    fn run(&mut self, text: &str, fmt: &Fmt, in_pre: bool) {
        if text.is_empty() || self.cut || fmt.tiny || fmt.transparent {
            return;
        }
        self.open_blocks();
        let (breaks, space) = self.owed();
        let target = fmt.tags(in_pre);
        if breaks > 0 {
            // A line break closes every inline tag: a link is one item to
            // the widget and cannot span lines.
            self.close_fmt();
            self.s.push_str(if breaks > 1 { "<p></p>" } else { "<br>" });
        } else {
            let common = self
                .fmt
                .iter()
                .zip(&target)
                .take_while(|(a, b)| a == b)
                .count();
            while self.fmt.len() > common {
                let t = self.fmt.pop().expect("len > common");
                self.s.push_str(t.close());
            }
            if space {
                self.s.push(' ');
            }
        }
        for t in &target[self.fmt.len()..] {
            self.s.push_str(&t.open());
            self.fmt.push(t.clone());
        }
        let room = MAX_OUT.saturating_sub(self.s.len());
        if text.len() > room {
            let mut end = room;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            esc(&mut self.s, &text[..end]);
            self.cut = true;
        } else {
            esc(&mut self.s, text);
        }
    }

    /// What separates the next thing from the last, and clears it: line
    /// breaks owed, else a space. Nothing separates the first thing, or
    /// what follows a block's own close.
    fn owed(&mut self) -> (u8, bool) {
        let (mut breaks, mut space) = (self.brk, self.space);
        if self.s.is_empty() {
            breaks = 0;
            space = false;
        }
        if self.after_block {
            breaks = breaks.saturating_sub(1);
            space = false;
        }
        self.brk = 0;
        self.space = false;
        self.after_block = false;
        (breaks, space)
    }

    /// An inline image: an item the widget places in the flow. The widget's
    /// link is a text item and cannot hold one, so the inline tags close
    /// before it and reopen after, and the link it sat in rides on the tag
    /// as its `href` — the image item is the link then.
    fn img(&mut self, src: &str, link: Option<&str>, alt: &str, w: Option<f64>, h: Option<f64>) {
        if self.cut {
            return;
        }
        self.open_blocks();
        let (breaks, space) = self.owed();
        self.close_fmt();
        if breaks > 0 {
            self.s.push_str(if breaks > 1 { "<p></p>" } else { "<br>" });
        } else if space {
            self.s.push(' ');
        }
        let _ = write!(self.s, "<img src=\"{}\"", esc_attr(src));
        if let Some(link) = link {
            let _ = write!(self.s, " href=\"{}\"", esc_attr(link));
        }
        if !alt.is_empty() {
            let _ = write!(self.s, " alt=\"{}\"", esc_attr(alt));
        }
        for (name, v) in [("width", w), ("height", h)] {
            if let Some(v) = v.filter(|v| *v >= 1.0) {
                let _ = write!(self.s, " {name}=\"{}\"", v.round() as i64);
            }
        }
        self.s.push_str("/>");
        if self.s.len() > MAX_OUT {
            self.cut = true;
        }
    }

    fn finish(mut self) -> String {
        self.close_fmt();
        while let Some(b) = self.blocks.pop() {
            if b.opened {
                self.s.push_str("</");
                self.s.push_str(b.name);
                self.s.push('>');
            }
        }
        if self.cut {
            self.s.push_str(CUT);
        }
        self.s
    }
}

/// Escapes text for the widget's parser, which decodes entities in text and
/// in attribute values alike.
fn esc(out: &mut String, t: &str) {
    for c in t.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn esc_attr(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        match c {
            '&' => s.push_str("&amp;"),
            '<' => s.push_str("&lt;"),
            '>' => s.push_str("&gt;"),
            '"' => s.push_str("&quot;"),
            c => s.push(c),
        }
    }
    s
}

/// Collapses whitespace the way HTML does: any run becomes one space.
/// Returns the text plus whether it began or ended on whitespace, which the
/// caller turns into owed spaces. `char::is_whitespace` takes the no-break
/// space with it, which is right: mail pads its layout with `&nbsp;`.
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

// ---------------------------------------------------------------------------
// The walk

/// What a node inherits from the elements above it.
#[derive(Clone, Default)]
struct Ctx {
    fmt: Fmt,
    ws: Ws,
}

struct Walk<'a> {
    doc: &'a Doc,
    sheet: StyleSheet<'a>,
    out: Out,
}

/// Where a declaration stands in the cascade — `(important, inline,
/// specificity, source order)` — so that sorting by it puts the winner last.
type Rank = (bool, bool, [u8; 3], usize);

/// The rows of a table that reads as data, each a list of
/// `(cell, is header, colspan)`.
struct Grid {
    rows: Vec<Vec<(usize, bool, usize)>>,
    /// The first row is all `<th>`: a header.
    header: bool,
    /// Every column has its own nonempty header, suitable as a field label.
    labeled: bool,
}

impl Walk<'_> {
    fn node(&mut self, i: usize, ctx: &Ctx) {
        if self.out.cut {
            return;
        }
        match &self.doc.nodes[i].kind {
            Kind::Document => self.children(i, ctx),
            Kind::Text(t) => self.text(t, ctx),
            Kind::Element { .. } => self.element(i, ctx),
        }
    }

    fn children(&mut self, i: usize, ctx: &Ctx) {
        let doc = self.doc;
        for &c in &doc.nodes[i].children {
            self.node(c, ctx);
        }
    }

    fn text(&mut self, t: &str, ctx: &Ctx) {
        match ctx.ws {
            Ws::Pre => self.out.run(t, &ctx.fmt, true),
            Ws::PreLines => {
                for (k, line) in t.split('\n').enumerate() {
                    if k > 0 {
                        self.out.br();
                    }
                    self.collapsed(line, &ctx.fmt);
                }
            }
            Ws::Collapse => self.collapsed(t, &ctx.fmt),
        }
    }

    fn collapsed(&mut self, t: &str, fmt: &Fmt) {
        let (s, lead, trail) = collapse(t);
        if s.is_empty() {
            if lead || trail {
                self.out.space();
            }
            return;
        }
        if lead {
            self.out.space();
        }
        self.out.run(&s, fmt, false);
        if trail {
            self.out.space();
        }
    }

    /// The cascade for one element: the rules that match it, by importance,
    /// origin and specificity, then its `style` attribute on top.
    fn css_for(&self, i: usize) -> Css {
        let attrs = self.doc.attrs(i);
        let style = attr(attrs, "style");
        let mut css = Css::default();
        if self.sheet.rules.is_empty() && style.is_none() {
            return css;
        }
        let mut decls: Vec<(Rank, Declaration<'_>)> = Vec::new();
        if !self.sheet.rules.is_empty() {
            let el = El { doc: self.doc, i };
            for (order, rule) in self.sheet.rules.iter().enumerate() {
                if rule.selector.matches(&el) {
                    let spec = rule.selector.specificity();
                    for d in &rule.declarations {
                        decls.push(((d.important, false, spec, order), *d));
                    }
                }
            }
        }
        if let Some(style) = style {
            for d in DeclarationTokenizer::from(style) {
                decls.push(((d.important, true, [0; 3], usize::MAX), d));
            }
        }
        decls.sort_by_key(|(k, _)| *k);
        for (_, d) in &decls {
            css.set(d);
        }
        css
    }

    fn element(&mut self, i: usize, ctx: &Ctx) {
        let doc = self.doc;
        let Kind::Element { name, attrs } = &doc.nodes[i].kind else {
            return;
        };
        let name = name.as_str();
        if attr(attrs, "hidden").is_some() {
            return;
        }
        let css = self.css_for(i);
        if css.hidden() {
            return;
        }
        let mut ctx = ctx.clone();
        // What the tag itself says — the user-agent sheet — under what the
        // author's sheet says.
        match name {
            "b" | "strong" | "dt" => ctx.fmt.bold = true,
            "i" | "em" | "cite" | "dfn" | "var" => ctx.fmt.italic = true,
            "u" | "ins" => ctx.fmt.underline = true,
            "s" | "del" | "strike" => ctx.fmt.strike = true,
            "code" | "tt" | "kbd" | "samp" => ctx.fmt.mono = true,
            "sub" => ctx.fmt.sub = true,
            "sup" => ctx.fmt.sup = true,
            "pre" => ctx.ws = Ws::Pre,
            _ => {}
        }
        css.apply(&mut ctx.fmt, &mut ctx.ws);

        match name {
            "br" => self.out.br(),
            "hr" => self.out.void("hr"),
            "img" => self.img(attrs, &css, &ctx),
            "a" => {
                if let Some(h) = href(attrs) {
                    ctx.fmt.link = Some(h);
                }
                self.children(i, &ctx);
            }
            "table" => self.table(i, &ctx),
            // Cells reach here from `table`, which has already placed the
            // separator before each. In a grid the widget bolds a header
            // cell itself; flattened to a line, the cell says so.
            "td" => self.children(i, &ctx),
            "th" => {
                ctx.fmt.bold = self.out.in_grid == 0 || ctx.fmt.bold;
                self.children(i, &ctx);
            }
            _ if BLOCKS.contains(&name) => {
                self.out.enter(block_name(name), open_tag(name, attrs));
                self.children(i, &ctx);
                self.out.leave();
            }
            _ if BREAK.contains(&name) => {
                self.out.boundary();
                self.children(i, &ctx);
                self.out.boundary();
            }
            _ => self.children(i, &ctx),
        }
    }

    /// An image stays an image when its source is one the panel can show,
    /// its alt text and size hints along; any other becomes its alt text.
    /// A box of a few pixels was counting opens, and leaves without a trace
    /// even when it carried an alt.
    fn img(&mut self, attrs: &[(String, String)], css: &Css, ctx: &Ctx) {
        let dim = |n: &str, c: Option<f64>| attr(attrs, n).and_then(px).or(c);
        let (w, h) = (dim("width", css.width), dim("height", css.height));
        let pixel = match (w, h) {
            (Some(w), Some(h)) => w * h <= PIXEL_SURFACE,
            (Some(x), None) | (None, Some(x)) => x <= 1.0,
            (None, None) => false,
        };
        if pixel {
            return;
        }
        let alt = attr(attrs, "alt")
            .map(|a| collapse(a).0)
            .unwrap_or_default();
        if let Some(src) = img_src(attrs) {
            self.out.img(&src, ctx.fmt.link.as_deref(), &alt, w, h);
        } else if !alt.is_empty() {
            self.collapsed(&alt, &ctx.fmt);
        }
    }

    /// A table is either data — a grid the widget draws — or layout, whose
    /// rows are lines and whose cells share a line when they are short.
    fn table(&mut self, i: usize, ctx: &Ctx) {
        let doc = self.doc;
        let rows = self.rows(i);
        if let Some(grid) = self.grid(i, &rows) {
            self.data_table(&grid, ctx);
            return;
        }
        self.out.boundary();
        for &c in &doc.nodes[i].children {
            match doc.name(c) {
                "tr" => self.layout_row(c, ctx),
                "thead" | "tbody" | "tfoot" => {
                    for r in doc.kids(c, &["tr"]) {
                        self.layout_row(r, ctx);
                    }
                }
                "colgroup" | "col" => {}
                _ => self.node(c, ctx),
            }
        }
        self.out.boundary();
    }

    fn layout_row(&mut self, tr: usize, ctx: &Ctx) {
        let doc = self.doc;
        let cells = doc.kids(tr, &["td", "th"]);
        // Short cells sat side by side — a label and its value, a row of
        // links — read as one line; anything longer takes its own.
        let join = cells.len() >= 2
            && cells
                .iter()
                .all(|&c| doc.inline_only(c) && doc.text_len(c) <= MAX_JOINED_CELL);
        self.out.boundary();
        let mut first = true;
        for &c in &doc.nodes[tr].children {
            if matches!(doc.name(c), "td" | "th") {
                if !first {
                    if join {
                        self.out.space();
                    } else {
                        self.out.boundary();
                    }
                }
                first = false;
            }
            self.node(c, ctx);
        }
        self.out.boundary();
    }

    /// The rows of a table, through its sections, in order. Rows of a
    /// nested table are its own.
    fn rows(&self, table: usize) -> Vec<usize> {
        let doc = self.doc;
        let mut rows = Vec::new();
        for &c in &doc.nodes[table].children {
            match doc.name(c) {
                "tr" => rows.push(c),
                "thead" | "tbody" | "tfoot" => rows.extend(doc.kids(c, &["tr"])),
                _ => {}
            }
        }
        rows
    }

    /// Whether a table reads as data: two or more rows of the same two to
    /// eight columns, every cell inline text, no nesting, and not declared
    /// presentation. Cells stay short unless a full row of column labels
    /// identifies the data beneath it: descriptions can wrap in that grid.
    /// Mail holds its page together with tables; a label-free table still
    /// has to pass the compact-cell heuristic.
    fn grid(&self, table: usize, rows: &[usize]) -> Option<Grid> {
        let doc = self.doc;
        if matches!(
            attr(doc.attrs(table), "role").map(str::trim),
            Some("presentation" | "none")
        ) {
            return None;
        }
        let mut grid = Grid {
            rows: Vec::new(),
            header: false,
            labeled: false,
        };
        let mut cols = None;
        let mut wide = 0usize;
        for &tr in rows {
            let mut cells = Vec::new();
            for c in doc.kids(tr, &["td", "th"]) {
                let span = attr(doc.attrs(c), "colspan")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_COLS);
                let max_cell = if grid.labeled {
                    MAX_LABELED_CELL
                } else {
                    MAX_CELL
                };
                if !doc.inline_only(c) || doc.text_len(c) > max_cell {
                    return None;
                }
                cells.push((c, doc.name(c) == "th", span));
            }
            if cells.is_empty() {
                continue;
            }
            let width: usize = cells.iter().map(|c| c.2).sum();
            if !(2..=MAX_COLS).contains(&width) || cols.is_some_and(|w| w != width) {
                return None;
            }
            cols = Some(width);
            if cells.len() >= 2 {
                wide += 1;
            }
            if grid.rows.is_empty() {
                grid.header = cells.iter().all(|c| c.1);
                grid.labeled = grid.header
                    && cells.len() >= 2
                    && cells
                        .iter()
                        .all(|&(c, _, span)| span == 1 && doc.text_len(c) > 0);
            }
            grid.rows.push(cells);
        }
        (grid.rows.len() >= 2 && wide >= 2).then_some(grid)
    }

    fn data_table(&mut self, grid: &Grid, ctx: &Ctx) {
        // Makepad divides every grid into equal columns. A four-column
        // table with sentence-length descriptions wastes most of its
        // short columns and wraps the prose into a narrow vertical strip.
        // Keep each record together with its labels instead; compact and
        // two-/three-column tables still have room to read as grids.
        if grid.labeled
            && grid.rows[0].len() >= 4
            && grid.rows[1..]
                .iter()
                .flatten()
                .any(|&(cell, _, _)| self.doc.text_len(cell) > MAX_CELL)
        {
            self.labeled_rows(grid, ctx);
            return;
        }
        self.out.open_blocks();
        self.out.raw("<table>");
        self.out.in_grid += 1;
        for (r, cells) in grid.rows.iter().enumerate() {
            let head = r == 0 && grid.header;
            self.out.raw(if head { "<thead><tr>" } else { "<tr>" });
            for &(cell, th, span) in cells {
                let tag = if th { "th" } else { "td" };
                self.out.raw(&format!("<{tag}>"));
                self.node(cell, ctx);
                self.out.raw(&format!("</{tag}>"));
                for _ in 1..span {
                    self.out.raw("<td></td>");
                }
            }
            self.out.raw(if head { "</tr></thead>" } else { "</tr>" });
        }
        self.out.in_grid -= 1;
        self.out.raw("</table>");
        self.out.after_block = true;
    }

    fn labeled_rows(&mut self, grid: &Grid, ctx: &Ctx) {
        let mut label_fmt = ctx.fmt.clone();
        label_fmt.bold = true;
        for cells in &grid.rows[1..] {
            self.out.enter("p", "<p>".into());
            let mut column = 0;
            for &(cell, _, span) in cells {
                if column > 0 {
                    self.out.boundary();
                }
                // A spanning value belongs to all the labels it covers.
                for offset in 0..span {
                    if offset > 0 {
                        self.collapsed(" / ", &label_fmt);
                    }
                    self.node(grid.rows[0][column + offset].0, ctx);
                }
                self.collapsed(": ", &label_fmt);
                self.node(cell, ctx);
                column += span;
            }
            self.out.leave();
        }
    }
}

fn block_name(name: &str) -> &'static str {
    match name {
        "p" => "p",
        "h1" => "h1",
        "h2" => "h2",
        "h3" => "h3",
        "h4" => "h4",
        "h5" => "h5",
        "h6" => "h6",
        "blockquote" => "blockquote",
        "pre" => "pre",
        "ul" => "ul",
        "ol" => "ol",
        "li" => "li",
        "details" => "details",
        "summary" => "summary",
        _ => unreachable!("not a block: {name}"),
    }
}

/// The open tag of a block, with the attributes the widget reads: a list's
/// numbering, an item's number, whether a fold starts open.
fn open_tag(name: &str, attrs: &[(String, String)]) -> String {
    let mut s = format!("<{name}");
    match name {
        "ol" => {
            if let Some(n) = attr(attrs, "start").and_then(|v| v.trim().parse::<i64>().ok()) {
                let _ = write!(s, " start=\"{n}\"");
            }
            if let Some(t) = attr(attrs, "type")
                .map(str::trim)
                .filter(|t| matches!(*t, "1" | "a" | "A" | "i" | "I"))
            {
                let _ = write!(s, " type=\"{t}\"");
            }
        }
        "li" => {
            if let Some(n) = attr(attrs, "value").and_then(|v| v.trim().parse::<i64>().ok()) {
                let _ = write!(s, " value=\"{n}\"");
            }
        }
        "details" if attr(attrs, "open").is_some() => s.push_str(" open"),
        _ => {}
    }
    s.push('>');
    s
}

/// An image source the panel can show, its scheme normalised to lower
/// case (`CID:` and `cid:` name the same part). An embedded image must be
/// base64 and under [`MAX_DATA_URI`].
fn img_src(attrs: &[(String, String)]) -> Option<String> {
    let s = attr(attrs, "src")?.trim();
    let lc = s.to_ascii_lowercase();
    let scheme = IMG_SCHEMES.iter().find(|p| lc.starts_with(*p))?;
    if scheme.starts_with("data:") && (s.len() > MAX_DATA_URI || !lc.contains(";base64,")) {
        return None;
    }
    Some(format!("{scheme}{}", &s[scheme.len()..]))
}

/// A link's destination, when it is one a reader could have meant.
fn href(attrs: &[(String, String)]) -> Option<String> {
    let h = attr(attrs, "href")?.trim();
    let lc = h.to_ascii_lowercase();
    SCHEMES
        .iter()
        .any(|s| lc.starts_with(s))
        .then(|| h.to_string())
}

// ---------------------------------------------------------------------------
// The entry points

/// Narrows an HTML document to what the `Html` widget draws.
///
/// The result is safe to hand straight to the widget: no fetching element
/// survives, link hrefs carry only [`SCHEMES`], text and attributes are
/// escaped, and nothing is left that the widget's own parser could not
/// decode.
#[must_use]
pub fn sanitize(src: &str) -> String {
    let mut src = src;
    if src.len() > MAX_IN {
        let mut end = MAX_IN;
        while !src.is_char_boundary(end) {
            end -= 1;
        }
        src = &src[..end];
    }
    // The parser turns each half of a surrogate pair into U+FFFD, as the
    // spec says; composers send emoji that way, so the pairs are put back
    // together first.
    let fixed = entities::guard(src);
    let doc = parse(&fixed);
    let sheet = StyleSheet::parse(&doc.css);
    let mut walk = Walk {
        doc: &doc,
        sheet,
        out: Out::default(),
    };
    walk.node(0, &Ctx::default());
    walk.out.finish()
}

/// A narrowed document as plain lines: tags go, the ones that stand on their
/// own line become breaks, cells are set apart by a space, and an entity
/// counts as the one character it decodes to.
///
/// This is a measure, not a reading — nobody sees the result. It exists so
/// the shell can ask how long a letter is without laying it out, which is
/// what a message panel's height wish is made of. Input is [`sanitize`]'s
/// output: already well-formed, already narrowed.
#[must_use]
pub fn plain(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find('<') {
        text(&mut out, &rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('>') else { break };
        let close = after.starts_with('/');
        let name = after[..end]
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let breaks = matches!(name.as_str(), "br" | "hr" | "tr" | "table" | "thead")
            || BLOCKS.contains(&name.as_str());
        if name == "img" {
            // An image is lines of its own: as many as its height says, a
            // guess when it says nothing. The first carries the alt text —
            // the line a closed row previews — and each is a mark rather
            // than blank, so the trailing trim cannot eat them.
            let tag = &after[..end];
            let lines = attr_in(tag, "height")
                .and_then(|h| h.parse::<f64>().ok())
                .map_or(IMG_LINES, |h| (h / 16.0).ceil().clamp(1.0, 40.0) as usize);
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for k in 0..lines {
                if k > 0 {
                    out.push('\n');
                }
                match attr_in(tag, "alt").filter(|a| k == 0 && !a.is_empty()) {
                    Some(alt) => text(&mut out, alt),
                    None => out.push('·'),
                }
            }
            out.push('\n');
        } else if breaks && !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        } else if !close
            && matches!(name.as_str(), "td" | "th")
            && !out.is_empty()
            && !out.ends_with(char::is_whitespace)
        {
            out.push(' ');
        }
        rest = &after[end + 1..];
    }
    text(&mut out, rest);
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out
}

/// The value of a double-quoted attribute in a tag's text — the shape
/// [`sanitize`] writes.
fn attr_in<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let at = tag.find(&key)? + key.len();
    let rest = &tag[at..];
    Some(&rest[..rest.find('"')?])
}

/// Rewrites a narrowed document's `cid:` sources to `cid:<scope>/…`, so
/// the attachments of one letter cannot answer for another's when both are
/// open — the message panel names the scope after the mail, and files the
/// letter's images under the same names.
#[must_use]
pub fn scope_cids<'a>(html: &'a str, scope: &str) -> std::borrow::Cow<'a, str> {
    if !html.contains("src=\"cid:") {
        return std::borrow::Cow::Borrowed(html);
    }
    std::borrow::Cow::Owned(html.replace("src=\"cid:", &format!("src=\"cid:{scope}/")))
}

/// Text between two tags, entities collapsed to one character each: a line
/// that reads `&mdash;` is one character wider there, not seven.
fn text(out: &mut String, mut src: &str) {
    while let Some(at) = src.find('&') {
        out.push_str(&src[..at]);
        match src[at..].find(';').filter(|&e| e <= 12) {
            Some(e) => {
                out.push('·');
                src = &src[at + e + 1..];
            }
            None => {
                out.push('&');
                src = &src[at + 1..];
            }
        }
    }
    out.push_str(src);
}

mod entities;

#[cfg(test)]
mod tests;

pub use entities::{base64_decode, base64_encode, guard};
