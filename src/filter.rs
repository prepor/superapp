//! The filter grammar every rich table speaks (see [`crate::richtable`]),
//! ported from stelaxis's `FilterParser`: one line of text → an [`Ast`],
//! plus the completion context under a caret — what the autocomplete is
//! about to offer, and how a pick splices back into the line.
//!
//! # Syntax
//!
//! | pattern | example | meaning |
//! |---|---|---|
//! | `@tag` | `@unread` | a boolean tag |
//! | `@tag:value` | `@from:vera` | equals (text: contains) |
//! | `@tag:"a b"` | `@subject:"panel model"` | a value with spaces |
//! | `@tag>value` `>=` `<` `<=` | `@date>30.08.2026` | a comparison |
//! | `@not:tag` `@not:tag:value` | `@not:unread` | negation |
//! | `(@a @or @b)` | `(@unread @or @html)` | a group — its members are OR'ed |
//! | `@a @b` | `@unread vera` | implicit AND |
//! | `text` | `budget draft` | free text, one substring |
//!
//! The parser never fails outright: what it cannot read becomes an
//! [`ParseError`] and the rest of the line still filters. Positions are
//! character indices into the trimmed input; the completion context works
//! in **byte** offsets, because that is what a text field's caret is.

/// A comparison operator on a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `@tag:value`
    Eq,
    /// `@tag>value`
    Gt,
    /// `@tag>=value`
    Gte,
    /// `@tag<value`
    Lt,
    /// `@tag<=value`
    Lte,
}

impl Op {
    /// How the operator is spelled in the grammar.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            Op::Eq => ":",
            Op::Gt => ">",
            Op::Gte => ">=",
            Op::Lt => "<",
            Op::Lte => "<=",
        }
    }
}

/// A parsed filter.
#[derive(Debug, Clone, PartialEq)]
pub enum Ast {
    /// Free text: one case-insensitive substring over the table's text
    /// columns.
    Text(String),
    /// A boolean tag.
    Tag(String),
    /// A tag compared with a value.
    Op {
        /// The tag name, normalized (`-` → `_`).
        tag: String,
        op: Op,
        value: String,
    },
    /// `@not:…`
    Not(Box<Ast>),
    /// Every member must hold.
    And(Vec<Ast>),
    /// Any member holds.
    Or(Vec<Ast>),
}

impl Ast {
    /// Every tag name the filter mentions, in order, with repeats.
    #[must_use]
    pub fn tag_names(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_tags(&mut out);
        out
    }

    fn collect_tags<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Ast::Text(_) => {}
            Ast::Tag(t) => out.push(t),
            Ast::Op { tag, .. } => out.push(tag),
            Ast::Not(inner) => inner.collect_tags(out),
            Ast::And(v) | Ast::Or(v) => v.iter().for_each(|a| a.collect_tags(out)),
        }
    }
}

/// Something the parser could not read; the rest of the line still parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Character index into the trimmed input.
    pub position: usize,
    pub message: String,
}

/// The result of [`parse`]: `ast` is `None` for an empty (or wholly
/// unreadable) line.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub ast: Option<Ast>,
    pub errors: Vec<ParseError>,
}

/// Parses a filter line. Lenient: partial results plus errors, never a
/// refusal.
#[must_use]
pub fn parse(input: &str) -> Parsed {
    let input = input.trim();
    if input.is_empty() {
        return Parsed {
            ast: None,
            errors: Vec::new(),
        };
    }
    let chars: Vec<char> = input.chars().collect();
    let mut lexer = Lexer {
        c: &chars,
        i: 0,
        tokens: Vec::new(),
        errors: Vec::new(),
    };
    lexer.run();
    let Lexer { tokens, mut errors, .. } = lexer;
    let (ast, parse_errors) = parse_tokens(&tokens);
    errors.extend(parse_errors);
    Parsed { ast, errors }
}

/// The tags a filter names that the table does not have, each once, in
/// order of first mention.
#[must_use]
pub fn unknown_tags<'a>(ast: Option<&'a Ast>, known: &[&str]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    if let Some(ast) = ast {
        for t in ast.tag_names() {
            if !known.contains(&t) && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// The tag name being typed at the very end of the line — `@` followed by
/// tag characters only — or `None`. An error about *that* tag is noise
/// while the operator is still typing it.
#[must_use]
pub fn typing_tag(input: &str) -> Option<&str> {
    let at = input.rfind('@')?;
    let partial = &input[at + 1..];
    partial
        .chars()
        .all(is_tag_char)
        .then_some(partial)
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Text(String, usize),
    Tag(String, usize),
    OpTag(String, Op, String, usize),
    NotTag(String, usize),
    NotOpTag(String, String, usize),
    LParen(usize),
    RParen(usize),
    Or(usize),
}

fn is_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn normalize_tag(name: &str) -> String {
    name.replace('-', "_")
}

struct Lexer<'a> {
    c: &'a [char],
    i: usize,
    tokens: Vec<Tok>,
    errors: Vec<ParseError>,
}

impl Lexer<'_> {
    fn peek(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(k, ch)| self.peek(k) == Some(ch))
    }

    fn err(&mut self, position: usize, message: &str) {
        self.errors.push(ParseError {
            position,
            message: message.to_string(),
        });
    }

    fn run(&mut self) {
        while self.i < self.c.len() {
            let pos = self.i;
            match self.c[self.i] {
                ' ' => self.i += 1,
                '(' => {
                    self.tokens.push(Tok::LParen(pos));
                    self.i += 1;
                }
                ')' => {
                    self.tokens.push(Tok::RParen(pos));
                    self.i += 1;
                }
                '@' if self.starts_with("@or")
                    && matches!(self.peek(3), None | Some(' ') | Some(')')) =>
                {
                    self.tokens.push(Tok::Or(pos));
                    self.i += 3;
                }
                '@' if self.starts_with("@not:") => {
                    self.i += 5;
                    self.not_tag(pos);
                }
                '@' => {
                    self.i += 1;
                    self.tag(pos);
                }
                _ => self.text(pos),
            }
        }
    }

    /// Reads a tag name. A char that is neither a tag char nor a delimiter
    /// (`:><() `) is swallowed, exactly as the original does.
    fn tag_content(&mut self) -> String {
        let mut acc = String::new();
        while let Some(ch) = self.peek(0) {
            if is_tag_char(ch) {
                acc.push(ch);
                self.i += 1;
            } else if ":><() ".contains(ch) {
                break;
            } else {
                self.i += 1;
                break;
            }
        }
        normalize_tag(&acc)
    }

    fn tag(&mut self, pos: usize) {
        let name = self.tag_content();
        if name.is_empty() {
            self.err(pos, "empty tag name");
            return;
        }
        match self.peek(0) {
            None | Some(' ') | Some(')') => self.tokens.push(Tok::Tag(name, pos)),
            Some(':') => {
                self.i += 1;
                self.op_value(name, Op::Eq, pos);
            }
            Some('>') if self.peek(1) == Some('=') => {
                self.i += 2;
                self.op_value(name, Op::Gte, pos);
            }
            Some('<') if self.peek(1) == Some('=') => {
                self.i += 2;
                self.op_value(name, Op::Lte, pos);
            }
            Some('>') => {
                self.i += 1;
                self.op_value(name, Op::Gt, pos);
            }
            Some('<') => {
                self.i += 1;
                self.op_value(name, Op::Lt, pos);
            }
            Some(_) => self.err(pos, "invalid tag syntax"),
        }
    }

    fn not_tag(&mut self, pos: usize) {
        let name = self.tag_content();
        match self.peek(0) {
            None | Some(' ') | Some(')') => self.tokens.push(Tok::NotTag(name, pos)),
            Some(':') => {
                self.i += 1;
                if let Some(v) = self.value(pos) {
                    self.tokens.push(Tok::NotOpTag(name, v, pos));
                }
            }
            Some(_) => self.err(pos, "invalid tag syntax"),
        }
    }

    fn op_value(&mut self, name: String, op: Op, pos: usize) {
        if let Some(v) = self.value(pos) {
            self.tokens.push(Tok::OpTag(name, op, v, pos));
        }
    }

    /// A value: quoted (to the next `"`, no escapes) or bare (to a space or
    /// a paren). An unclosed quote is an error that eats the rest of the
    /// line.
    fn value(&mut self, pos: usize) -> Option<String> {
        let mut acc = String::new();
        if self.peek(0) == Some('"') {
            self.i += 1;
            loop {
                match self.peek(0) {
                    None => {
                        self.err(pos, "unclosed quote");
                        return None;
                    }
                    Some('"') => {
                        self.i += 1;
                        return Some(acc);
                    }
                    Some(ch) => {
                        acc.push(ch);
                        self.i += 1;
                    }
                }
            }
        }
        while let Some(ch) = self.peek(0) {
            if " )(".contains(ch) {
                break;
            }
            acc.push(ch);
            self.i += 1;
        }
        Some(acc)
    }

    /// Free text runs to the next `@`, `(` or `)`; inner spaces are kept.
    fn text(&mut self, pos: usize) {
        let mut acc = String::new();
        while let Some(ch) = self.peek(0) {
            if "@()".contains(ch) {
                break;
            }
            acc.push(ch);
            self.i += 1;
        }
        let t = acc.trim();
        if t.is_empty() {
            // Whitespace the space rule did not eat (a tab): step over it.
            self.i = self.i.max(pos + 1);
        } else {
            self.tokens.push(Tok::Text(t.to_string(), pos));
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

fn parse_tokens(tokens: &[Tok]) -> (Option<Ast>, Vec<ParseError>) {
    if tokens.is_empty() {
        return (None, Vec::new());
    }
    let mut errors = Vec::new();
    let (branches, rest) = elements_with_or(tokens, &mut errors);
    if let Some(Tok::RParen(p)) = rest.first() {
        errors.push(ParseError {
            position: *p,
            message: "unexpected closing parenthesis".into(),
        });
    }
    (branches_to_ast(branches), errors)
}

fn leaf(tok: &Tok) -> Option<Ast> {
    Some(match tok {
        Tok::Text(t, _) => Ast::Text(t.clone()),
        Tok::Tag(n, _) => Ast::Tag(n.clone()),
        Tok::OpTag(n, op, v, _) => Ast::Op {
            tag: n.clone(),
            op: *op,
            value: v.clone(),
        },
        Tok::NotTag(n, _) => Ast::Not(Box::new(Ast::Tag(n.clone()))),
        Tok::NotOpTag(n, v, _) => Ast::Not(Box::new(Ast::Op {
            tag: n.clone(),
            op: Op::Eq,
            value: v.clone(),
        })),
        Tok::LParen(_) | Tok::RParen(_) | Tok::Or(_) => return None,
    })
}

/// Top level: implicit AND within a branch, `@or` between branches, a
/// paren group as one element. Stops at a stray `)`.
fn elements_with_or<'t>(
    mut tokens: &'t [Tok],
    errors: &mut Vec<ParseError>,
) -> (Vec<Vec<Ast>>, &'t [Tok]) {
    let mut branches: Vec<Vec<Ast>> = Vec::new();
    let mut current: Vec<Ast> = Vec::new();
    while let Some(tok) = tokens.first() {
        match tok {
            Tok::RParen(_) => break,
            Tok::Or(_) => {
                branches.push(std::mem::take(&mut current));
                tokens = &tokens[1..];
            }
            Tok::LParen(pos) => {
                let (group, rest) = or_group(&tokens[1..], *pos, errors);
                if let Some(g) = group {
                    current.push(g);
                }
                tokens = rest;
            }
            other => {
                if let Some(a) = leaf(other) {
                    current.push(a);
                }
                tokens = &tokens[1..];
            }
        }
    }
    branches.push(current);
    (branches, tokens)
}

/// Inside parens every member is OR'ed, `@or` or not; groups nest.
fn or_group<'t>(
    mut tokens: &'t [Tok],
    open_pos: usize,
    errors: &mut Vec<ParseError>,
) -> (Option<Ast>, &'t [Tok]) {
    let mut elements: Vec<Ast> = Vec::new();
    loop {
        match tokens.first() {
            None => {
                errors.push(ParseError {
                    position: open_pos,
                    message: "unclosed parenthesis".into(),
                });
                return (None, tokens);
            }
            Some(Tok::RParen(_)) => {
                let rest = &tokens[1..];
                return match elements.len() {
                    0 => {
                        errors.push(ParseError {
                            position: open_pos,
                            message: "empty OR group".into(),
                        });
                        (None, rest)
                    }
                    1 => (elements.pop(), rest),
                    _ => (Some(Ast::Or(elements)), rest),
                };
            }
            Some(Tok::Or(_)) => tokens = &tokens[1..],
            Some(Tok::LParen(pos)) => {
                let (group, rest) = or_group(&tokens[1..], *pos, errors);
                if let Some(g) = group {
                    elements.push(g);
                }
                tokens = rest;
            }
            Some(other) => {
                if let Some(a) = leaf(other) {
                    elements.push(a);
                }
                tokens = &tokens[1..];
            }
        }
    }
}

fn branches_to_ast(branches: Vec<Vec<Ast>>) -> Option<Ast> {
    let mut asts: Vec<Ast> = branches
        .into_iter()
        .filter_map(|mut b| match b.len() {
            0 => None,
            1 => b.pop(),
            _ => Some(Ast::And(b)),
        })
        .collect();
    match asts.len() {
        0 => None,
        1 => asts.pop(),
        _ => Some(Ast::Or(asts)),
    }
}

// ---------------------------------------------------------------------------
// Completion context
// ---------------------------------------------------------------------------

/// What the caret is in the middle of typing, if it is something the
/// autocomplete can finish. Offsets are byte indices into the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// `@na…` — a tag name; `start` is the `@`.
    Tag {
        start: usize,
        /// Lowercased.
        partial: String,
    },
    /// `@tag:va…` — a value after an operator; `start` is where the value
    /// begins (just after the operator).
    Value {
        start: usize,
        /// Normalized tag name.
        tag: String,
        op: Op,
        /// As typed (a leading quote included).
        partial: String,
    },
}

impl Context {
    /// Where the text being completed starts.
    #[must_use]
    pub fn start(&self) -> usize {
        match self {
            Context::Tag { start, .. } | Context::Value { start, .. } => *start,
        }
    }
}

/// Classifies the caret: walk back to the start of the space/paren-bounded
/// token, take its *first* `@` as the tag marker (a later one, as in an
/// address value, is part of the value), and read what follows. `None`
/// when completion would only be noise — plain text, or just past `@not:`
/// or `@or`.
#[must_use]
pub fn context(text: &str, cursor: usize) -> Option<Context> {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let bytes = text.as_bytes();
    // The token starts after the last delimiter that is not inside quotes:
    // a quoted value keeps its spaces while it is being typed.
    let mut token_start = 0;
    let mut in_quote = false;
    for (i, b) in bytes[..cursor].iter().enumerate() {
        if *b == b'"' {
            in_quote = !in_quote;
        } else if !in_quote && matches!(b, b' ' | b'(' | b')') {
            token_start = i + 1;
        }
    }
    let at = (token_start..cursor).find(|&i| bytes[i] == b'@')?;
    let token = &text[at + 1..cursor];
    let offset = if token.len() >= 4 && token[..4].eq_ignore_ascii_case("not:") {
        4
    } else {
        0
    };
    let body = &token[offset..];
    let name_len = body
        .char_indices()
        .find(|&(_, c)| !is_tag_char(c))
        .map_or(body.len(), |(i, _)| i);
    if name_len > 0 {
        let after = &body[name_len..];
        let op = [
            (">=", Op::Gte),
            ("<=", Op::Lte),
            (":", Op::Eq),
            (">", Op::Gt),
            ("<", Op::Lt),
        ]
        .into_iter()
        .find(|(s, _)| after.starts_with(s));
        if let Some((sym, op)) = op {
            return Some(Context::Value {
                start: at + 1 + offset + name_len + sym.len(),
                tag: normalize_tag(&body[..name_len]),
                op,
                partial: after[sym.len()..].to_string(),
            });
        }
    }
    if offset > 0 || token.eq_ignore_ascii_case("or") {
        return None;
    }
    Some(Context::Tag {
        start: at,
        partial: token.to_lowercase(),
    })
}

/// Splices a picked tag over the `@token` under the caret: `@name:` when
/// the tag takes a value (the value list opens right behind it), `@name `
/// for a boolean. Returns the new line and where the caret lands.
#[must_use]
pub fn insert_tag(
    text: &str,
    cursor: usize,
    start: usize,
    name: &str,
    takes_value: bool,
) -> (String, usize) {
    let cursor = cursor.min(text.len()).max(start);
    let suffix = if takes_value { ":" } else { " " };
    let inserted = format!("@{name}{suffix}");
    let out = format!("{}{}{}", &text[..start], inserted, &text[cursor..]);
    (out, start + inserted.len())
}

/// Splices a picked value over the partial typed after the operator,
/// quoting it if the grammar would otherwise split it, with a separating
/// space when it lands at the end of the line.
#[must_use]
pub fn insert_value(text: &str, cursor: usize, start: usize, value: &str) -> (String, usize) {
    let cursor = cursor.min(text.len()).max(start);
    let after = &text[cursor..];
    let mut inserted = quote(value);
    if after.is_empty() {
        inserted.push(' ');
    }
    let out = format!("{}{}{}", &text[..start], inserted, after);
    (out, start + inserted.len())
}

/// A bare value reads up to the next space or paren, so one that contains
/// either is quoted; embedded quotes are dropped (the grammar has no
/// escape).
#[must_use]
pub fn quote(v: &str) -> String {
    if v.chars().any(|c| c.is_whitespace() || c == '(' || c == ')') {
        format!("\"{}\"", v.replace('"', ""))
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ast(s: &str) -> Option<Ast> {
        let p = parse(s);
        assert!(p.errors.is_empty(), "{s:?}: {:?}", p.errors);
        p.ast
    }

    fn tag(n: &str) -> Ast {
        Ast::Tag(n.into())
    }

    fn op(n: &str, o: Op, v: &str) -> Ast {
        Ast::Op {
            tag: n.into(),
            op: o,
            value: v.into(),
        }
    }

    fn text(t: &str) -> Ast {
        Ast::Text(t.into())
    }

    #[test]
    fn text_search() {
        assert_eq!(ast("john"), Some(text("john")));
        assert_eq!(ast("john doe"), Some(text("john doe")));
        assert_eq!(ast(""), None);
        assert_eq!(ast("   "), None);
    }

    #[test]
    fn boolean_tags() {
        assert_eq!(ast("@active"), Some(tag("active")));
        assert_eq!(ast("@is_admin"), Some(tag("is_admin")));
        assert_eq!(ast("@is-admin"), Some(tag("is_admin")), "hyphens normalize");
    }

    #[test]
    fn operator_tags() {
        assert_eq!(ast("@role:admin"), Some(op("role", Op::Eq, "admin")));
        assert_eq!(ast("@name:\"John Doe\""), Some(op("name", Op::Eq, "John Doe")));
        assert_eq!(ast("@age>18"), Some(op("age", Op::Gt, "18")));
        assert_eq!(ast("@age>=18"), Some(op("age", Op::Gte, "18")));
        assert_eq!(ast("@age<18"), Some(op("age", Op::Lt, "18")));
        assert_eq!(ast("@age<=18"), Some(op("age", Op::Lte, "18")));
        assert_eq!(ast("@amount>\"100.50\""), Some(op("amount", Op::Gt, "100.50")));
    }

    #[test]
    fn negation() {
        assert_eq!(ast("@not:deleted"), Some(Ast::Not(Box::new(tag("deleted")))));
        assert_eq!(
            ast("@not:role:guest"),
            Some(Ast::Not(Box::new(op("role", Op::Eq, "guest"))))
        );
    }

    #[test]
    fn implicit_and() {
        assert_eq!(
            ast("@active @admin"),
            Some(Ast::And(vec![tag("active"), tag("admin")]))
        );
        assert_eq!(
            ast("@active @role:admin john"),
            Some(Ast::And(vec![
                tag("active"),
                op("role", Op::Eq, "admin"),
                text("john")
            ]))
        );
        assert_eq!(
            ast("@active    @admin"),
            Some(Ast::And(vec![tag("active"), tag("admin")]))
        );
        assert_eq!(ast("  @active  "), Some(tag("active")));
    }

    #[test]
    fn or_groups() {
        assert_eq!(
            ast("(@active @or @pending)"),
            Some(Ast::Or(vec![tag("active"), tag("pending")]))
        );
        assert_eq!(
            ast("(@active @or @pending @or @draft)"),
            Some(Ast::Or(vec![tag("active"), tag("pending"), tag("draft")]))
        );
        assert_eq!(
            ast("(@status:active @or @status:pending)"),
            Some(Ast::Or(vec![
                op("status", Op::Eq, "active"),
                op("status", Op::Eq, "pending")
            ]))
        );
        // Inside parens the keyword is optional: members are OR'ed anyway.
        assert_eq!(
            ast("(@a @b)"),
            Some(Ast::Or(vec![tag("a"), tag("b")]))
        );
    }

    #[test]
    fn top_level_or() {
        assert_eq!(
            ast("@tag1 @or @tag2"),
            Some(Ast::Or(vec![tag("tag1"), tag("tag2")]))
        );
        assert_eq!(
            ast("@tag1 @or @tag2 hi"),
            Some(Ast::Or(vec![
                tag("tag1"),
                Ast::And(vec![tag("tag2"), text("hi")])
            ]))
        );
        assert_eq!(
            ast("@tag1 @or @tag2 @or @tag3"),
            Some(Ast::Or(vec![tag("tag1"), tag("tag2"), tag("tag3")]))
        );
        assert_eq!(
            ast("a @tag1 @or @tag2 b"),
            Some(Ast::Or(vec![
                Ast::And(vec![text("a"), tag("tag1")]),
                Ast::And(vec![tag("tag2"), text("b")])
            ]))
        );
        assert_eq!(ast("@or @tag1"), Some(tag("tag1")));
        assert_eq!(ast("@tag1 @or"), Some(tag("tag1")));
        assert_eq!(ast("@tag1 @or @or @tag2"), Some(Ast::Or(vec![tag("tag1"), tag("tag2")])));
        assert_eq!(
            ast("@not:deleted @or @active"),
            Some(Ast::Or(vec![Ast::Not(Box::new(tag("deleted"))), tag("active")]))
        );
        assert_eq!(
            ast("@tag1 @or (@a @or @b)"),
            Some(Ast::Or(vec![tag("tag1"), Ast::Or(vec![tag("a"), tag("b")])]))
        );
        assert_eq!(
            ast("hello @or world"),
            Some(Ast::Or(vec![text("hello"), text("world")]))
        );
        assert_eq!(
            ast("@active @role:admin @or @not:deleted john"),
            Some(Ast::Or(vec![
                Ast::And(vec![tag("active"), op("role", Op::Eq, "admin")]),
                Ast::And(vec![Ast::Not(Box::new(tag("deleted"))), text("john")])
            ]))
        );
        // `@order` is a tag, not the keyword.
        assert_eq!(ast("@order"), Some(tag("order")));
    }

    #[test]
    fn complex_expressions() {
        assert_eq!(
            ast("@admin (@active @or @pending)"),
            Some(Ast::And(vec![
                tag("admin"),
                Ast::Or(vec![tag("active"), tag("pending")])
            ]))
        );
        assert_eq!(
            ast("john @active"),
            Some(Ast::And(vec![text("john"), tag("active")]))
        );
        assert_eq!(
            ast("@role:admin @not:deleted (@active @or @pending) john"),
            Some(Ast::And(vec![
                op("role", Op::Eq, "admin"),
                Ast::Not(Box::new(tag("deleted"))),
                Ast::Or(vec![tag("active"), tag("pending")]),
                text("john")
            ]))
        );
        assert_eq!(
            ast("@url:\"http://example.com\""),
            Some(op("url", Op::Eq, "http://example.com"))
        );
        assert_eq!(
            ast("@email:\"user@example.com\""),
            Some(op("email", Op::Eq, "user@example.com"))
        );
        // A bare address value: the `@` inside is part of the value.
        assert_eq!(
            ast("@from:vera@kovac.io"),
            Some(op("from", Op::Eq, "vera@kovac.io"))
        );
    }

    #[test]
    fn errors_are_partial_not_fatal() {
        let p = parse("@name:\"John Doe");
        assert_eq!(p.errors.len(), 1);
        assert!(p.errors[0].message.contains("quote"));

        let p = parse("(@active @or @pending");
        assert_eq!(p.errors[0].message, "unclosed parenthesis");

        let p = parse("@");
        assert_eq!(p.ast, None);
        assert_eq!(p.errors[0].message, "empty tag name");

        let p = parse("@active)");
        assert_eq!(p.ast, Some(tag("active")));
        assert_eq!(p.errors[0].message, "unexpected closing parenthesis");

        let p = parse("()");
        assert_eq!(p.ast, None);
        assert_eq!(p.errors[0].message, "empty OR group");

        // The unreadable part is dropped, the rest still filters.
        let p = parse("@role!x @active");
        assert_eq!(p.errors[0].message, "invalid tag syntax");
        assert_eq!(p.ast, Some(Ast::And(vec![text("x"), tag("active")])));

        let p = parse("@not:x>3");
        assert_eq!(p.errors[0].message, "invalid tag syntax");
    }

    #[test]
    fn tag_names_and_validation() {
        let a = ast("@a @b:1 (@c @or @not:a) x").unwrap();
        assert_eq!(a.tag_names(), vec!["a", "b", "c", "a"]);
        assert_eq!(unknown_tags(Some(&a), &["a", "c"]), vec!["b"]);
        assert_eq!(unknown_tags(None, &[]), Vec::<&str>::new());
        assert_eq!(typing_tag("vera @un"), Some("un"));
        assert_eq!(typing_tag("vera @"), Some(""));
        assert_eq!(typing_tag("@from:v"), None);
        assert_eq!(typing_tag("vera"), None);
    }

    #[test]
    fn completion_context() {
        assert_eq!(
            context("@un", 3),
            Some(Context::Tag {
                start: 0,
                partial: "un".into()
            })
        );
        assert_eq!(
            context("vera @", 6),
            Some(Context::Tag {
                start: 5,
                partial: "".into()
            })
        );
        assert_eq!(
            context("@From:ve", 8),
            Some(Context::Value {
                start: 6,
                tag: "From".into(),
                op: Op::Eq,
                partial: "ve".into()
            })
        );
        assert_eq!(
            context("@date>=2026", 11),
            Some(Context::Value {
                start: 7,
                tag: "date".into(),
                op: Op::Gte,
                partial: "2026".into()
            })
        );
        // A later @ is the value's, not a new tag.
        assert_eq!(
            context("@from:vera@ko", 13),
            Some(Context::Value {
                start: 6,
                tag: "from".into(),
                op: Op::Eq,
                partial: "vera@ko".into()
            })
        );
        // @not: negates but still completes the tag/value behind it.
        assert_eq!(
            context("@not:from:v", 11),
            Some(Context::Value {
                start: 10,
                tag: "from".into(),
                op: Op::Eq,
                partial: "v".into()
            })
        );
        assert_eq!(context("@not:", 5), None);
        // A quoted value being typed keeps its spaces; a bare one ends at
        // the first space.
        assert_eq!(
            context("@subject:\"panel mo", 18),
            Some(Context::Value {
                start: 9,
                tag: "subject".into(),
                op: Op::Eq,
                partial: "\"panel mo".into()
            })
        );
        assert_eq!(context("@subject:panel mo", 17), None);
        assert_eq!(context("@or", 3), None);
        assert_eq!(context("vera", 4), None);
        assert_eq!(context("@unread vera", 12), None);
        // The caret in the middle of a line completes the token it is in.
        assert_eq!(
            context("@un @from:x", 3),
            Some(Context::Tag {
                start: 0,
                partial: "un".into()
            })
        );
        // Parens bound tokens.
        assert_eq!(
            context("(@un", 4),
            Some(Context::Tag {
                start: 1,
                partial: "un".into()
            })
        );
        // A caret past the end, or inside a multibyte char, is clamped.
        assert_eq!(
            context("@é", 2),
            Some(Context::Tag {
                start: 0,
                partial: "".into()
            })
        );
    }

    #[test]
    fn splicing() {
        assert_eq!(insert_tag("vera @un", 8, 5, "unread", false), ("vera @unread ".into(), 13));
        assert_eq!(insert_tag("@fr x", 3, 0, "from", true), ("@from: x".into(), 6));
        assert_eq!(
            insert_value("@from:ve", 8, 6, "vera@kovac.io"),
            ("@from:vera@kovac.io ".into(), 20)
        );
        assert_eq!(
            insert_value("@from:ve @unread", 8, 6, "Vera Kovac"),
            ("@from:\"Vera Kovac\" @unread".into(), 18)
        );
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("a (b)"), "\"a (b)\"");
        assert_eq!(quote("say \"hi\" there"), "\"say hi there\"");
    }
}
