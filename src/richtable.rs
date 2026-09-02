//! The rich table (CR-006): what every list panel shares, ported from
//! stelaxis's `RichTable` — a **datasource** behind a uniform seam, a
//! **SQL builder** that turns a [`filter`] AST into the datasource's
//! `WHERE`, and a **paging engine** that virtualizes the data the way the
//! `PortalList` already virtualizes the drawing.
//!
//! Pure: no makepad. The inbox panel in [`crate::panels`] is the first
//! consumer; a feed or a calendar list is the same widget over another
//! [`Datasource`].
//!
//! # Paging, not "load more"
//!
//! A table never holds its rows. It asks its source for a **count** (one
//! `SELECT COUNT(*)` under the current filter) and for **pages** of a fixed
//! size, by offset, exactly when a draw needs a row on screen — so the list
//! can be a hundred thousand rows long and a frame still touches only the
//! pages under the viewport, each a cached, reactive query in the store
//! (a commit re-runs the pages on screen, lazily, on the next draw). The
//! scrollbar is honest because the count is; jumping to the middle of the
//! list fetches the middle's page and nothing else. The list's cursor is
//! kept by **rank**: a row's position is one `COUNT(*)` of the rows the
//! order puts before it, so a mail that moved (a sync landed above it) is
//! found again without walking anything.
//!
//! A source that cannot count (a remote one) says so, and the engine falls
//! back to a growing window: one more page each time the end of the list
//! comes on screen — the same scroll-driven load, without the total.

use std::rc::Rc;

use crate::filter::{self, Ast, Context, Op, ParseError};
use crate::store::{Store, Val};

/// How many suggestions the autocomplete offers at once.
pub const MAX_SUGGESTIONS: usize = 8;

/// What a tag's value is, which decides how a comparison binds in SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagType {
    /// `@tag` alone; takes no value.
    Bool,
    /// Text; `:` is a case-insensitive *contains*.
    Text,
    /// A number; comparisons are numeric.
    Number,
    /// A day (`30.08.2026`) or a minute (`"30.08.2026 09:14"`), compared
    /// against the store's timestamps; `:` means *on that day*.
    Date,
}

/// How a tag's values are completed in the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Values {
    /// Free text only.
    None,
    /// A closed set, `(label, value)` — the label is shown, the value is
    /// what the filter gets.
    Static(&'static [(&'static str, &'static str)]),
    /// Asked of the datasource as the operator types
    /// ([`Datasource::suggest`]) — for sets too large or too live to list.
    Dynamic,
}

/// A tag a datasource understands.
#[derive(Debug, Clone, Copy)]
pub struct TagDef {
    /// Normalized name (`_`, not `-`).
    pub name: &'static str,
    pub kind: TagType,
    /// The operators the tag accepts; empty for a boolean.
    pub ops: &'static [Op],
    /// One line for the autocomplete and the help.
    pub describe: &'static str,
    pub values: Values,
}

impl TagDef {
    /// Whether `@name` wants a value after it.
    #[must_use]
    pub fn takes_value(&self) -> bool {
        !self.ops.is_empty()
    }
}

/// One autocomplete row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What the row shows.
    pub label: String,
    /// What a pick puts into the filter.
    pub value: String,
    /// Muted text beside the label: a tag's description, or a value when
    /// the label differs from it.
    pub describe: String,
}

impl Suggestion {
    /// A value suggestion; the value shows as its own label.
    #[must_use]
    pub fn value(v: impl Into<String>) -> Self {
        let value = v.into();
        Suggestion {
            label: value.clone(),
            value,
            describe: String::new(),
        }
    }

    /// A value suggestion shown under another name.
    #[must_use]
    pub fn labeled(label: impl Into<String>, value: impl Into<String>) -> Self {
        let (label, value) = (label.into(), value.into());
        let describe = if label == value {
            String::new()
        } else {
            value.clone()
        };
        Suggestion {
            label,
            value,
            describe,
        }
    }
}

/// Where a table's rows come from. Everything is keyed on the current
/// filter AST, so the engine can stay stateless about the data.
pub trait Datasource {
    /// One row, as the panel draws it.
    type Row: Clone + 'static;

    /// The tags the filter accepts.
    fn tags(&self) -> &'static [TagDef];

    /// How many rows match, or `None` for a source that cannot say.
    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize>;

    /// Rows `offset..offset+limit` under the filter, in the source's order.
    fn page(&self, store: &Store, ast: Option<&Ast>, offset: usize, limit: usize)
        -> Rc<Vec<Self::Row>>;

    /// Where a row sits in the filtered order, if the source can tell
    /// without walking.
    fn index_of(&self, _store: &Store, _ast: Option<&Ast>, _row: &Self::Row) -> Option<usize> {
        None
    }

    /// Value suggestions for a [`Values::Dynamic`] tag, given what has been
    /// typed so far. Advisory: the operator can still type anything.
    fn suggest(&self, _store: &Store, _tag: &str, _prefix: &str) -> Vec<Suggestion> {
        Vec::new()
    }
}

/// A `static` source is shared by reference.
impl<D: Datasource> Datasource for &D {
    type Row = D::Row;

    fn tags(&self) -> &'static [TagDef] {
        (**self).tags()
    }

    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize> {
        (**self).count(store, ast)
    }

    fn page(&self, store: &Store, ast: Option<&Ast>, offset: usize, limit: usize)
        -> Rc<Vec<Self::Row>> {
        (**self).page(store, ast, offset, limit)
    }

    fn index_of(&self, store: &Store, ast: Option<&Ast>, row: &Self::Row) -> Option<usize> {
        (**self).index_of(store, ast, row)
    }

    fn suggest(&self, store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> {
        (**self).suggest(store, tag, prefix)
    }
}

// ---------------------------------------------------------------------------
// The SQL builder
// ---------------------------------------------------------------------------

/// Sort direction of one `ORDER BY` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// How a tag reaches SQL.
#[derive(Debug, Clone, Copy)]
pub enum TagSql {
    /// A boolean tag: a `WHERE` fragment, parameter-free.
    Where(&'static str),
    /// An operator tag: the column (or expression) it compares.
    Col(&'static str),
}

/// A SQL-backed table: the fixed parts of its query. The builder adds the
/// filter's `WHERE`, the paging and the rank.
#[derive(Debug, Clone, Copy)]
pub struct SqlSpec {
    /// Query id prefix for the trace (`inbox` → `inbox page`, `inbox count`).
    pub id: &'static str,
    pub describe: &'static str,
    /// The column list.
    pub select: &'static str,
    /// The `FROM`, joins included.
    pub from: &'static str,
    /// A `WHERE` fragment every query carries, or `""`.
    pub base: &'static str,
    /// Columns free text searches.
    pub text: &'static [&'static str],
    /// Tag name → binding.
    pub tags: &'static [(&'static str, TagSql)],
    /// The order, which is also the rank key. Must be total (end with a
    /// unique column) or paging tears. Under a `group`, these name aliases
    /// of `select`, since the page is read off the grouped subquery.
    pub order: &'static [(&'static str, Dir)],
    /// A row is a group (CR-007: a thread is its inbox messages). The
    /// select is then aggregates over the members, `GROUP BY` this key, and
    /// the filter becomes a membership test: a group matches when **any**
    /// member matches, and its aggregates always cover the whole group.
    pub group: Option<&'static str>,
}

/// Built SQL and its parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Sql {
    pub sql: String,
    pub params: Vec<Val>,
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Three integers around a separator, and nothing else.
fn three(s: &str, sep: char) -> Option<[i64; 3]> {
    let mut it = s.split(sep);
    let a = it.next()?.trim().parse().ok()?;
    let b = it.next()?.trim().parse().ok()?;
    let c = it.next()?.trim().parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some([a, b, c])
}

/// `dd.mm.yyyy`, optionally followed by ` HH:MM`, → the `[start, end)` span
/// it names in epoch seconds: a day, or a minute. The ISO spelling
/// (`yyyy-mm-dd`) is read too, so a pasted timestamp still works.
#[must_use]
pub fn date_span(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    let (day, time) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (y, m, d) = if day.contains('.') {
        let [d, m, y] = three(day, '.')?;
        (y, m, d)
    } else {
        let [y, m, d] = three(day, '-')?;
        (y, m, d)
    };
    let (m, d) = (u32::try_from(m).ok()?, u32::try_from(d).ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day_start = days_from_civil(y, m, d) as f64 * 86_400.0;
    match time {
        None => Some((day_start, day_start + 86_400.0)),
        Some(t) => {
            let mut hm = t.split(':');
            let h: f64 = hm.next()?.parse().ok()?;
            let min: f64 = hm.next()?.parse().ok()?;
            if hm.next().is_some() || h >= 24.0 || min >= 60.0 {
                return None;
            }
            let start = day_start + h * 3600.0 + min * 60.0;
            Some((start, start + 60.0))
        }
    }
}

impl SqlSpec {
    fn binding(&self, name: &str) -> Option<TagSql> {
        self.tags.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
    }

    fn kind(tags: &[TagDef], name: &str) -> Option<TagType> {
        tags.iter().find(|t| t.name == name).map(|t| t.kind)
    }

    /// The filter as a SQL expression, or `None` when nothing in it binds
    /// (an unknown tag, a boolean used with a value): the table then shows
    /// everything rather than nothing, and the error line says why.
    fn expr(&self, tags: &[TagDef], ast: &Ast, params: &mut Vec<Val>) -> Option<String> {
        match ast {
            Ast::Text(t) => {
                if self.text.is_empty() {
                    return None;
                }
                let pat = format!("%{}%", escape_like(t));
                let parts: Vec<String> = self
                    .text
                    .iter()
                    .map(|c| {
                        params.push(Val::S(pat.clone()));
                        format!("{c} LIKE ? ESCAPE '\\'")
                    })
                    .collect();
                Some(format!("({})", parts.join(" OR ")))
            }
            Ast::Tag(name) => match self.binding(name)? {
                TagSql::Where(w) => Some(format!("({w})")),
                TagSql::Col(_) => None,
            },
            Ast::Op { tag, op, value } => {
                let TagSql::Col(col) = self.binding(tag)? else {
                    return None;
                };
                let kind = Self::kind(tags, tag)?;
                Some(match kind {
                    TagType::Bool => return None,
                    TagType::Text => match op {
                        Op::Eq => {
                            params.push(Val::S(format!("%{}%", escape_like(value))));
                            format!("{col} LIKE ? ESCAPE '\\'")
                        }
                        _ => {
                            params.push(Val::S(value.clone()));
                            format!("{col} {} ?", cmp(*op))
                        }
                    },
                    TagType::Number => {
                        // Unreadable: dropped, like a date — the error
                        // line says so, rather than a string compare that
                        // quietly matches nothing.
                        params.push(Val::F(value.trim().parse::<f64>().ok()?));
                        format!("{col} {} ?", cmp(*op))
                    }
                    TagType::Date => {
                        let (start, end) = date_span(value)?;
                        match op {
                            Op::Eq => {
                                params.push(Val::F(start));
                                params.push(Val::F(end));
                                format!("({col} >= ? AND {col} < ?)")
                            }
                            Op::Gt => {
                                params.push(Val::F(end));
                                format!("{col} >= ?")
                            }
                            Op::Gte => {
                                params.push(Val::F(start));
                                format!("{col} >= ?")
                            }
                            Op::Lt => {
                                params.push(Val::F(start));
                                format!("{col} < ?")
                            }
                            Op::Lte => {
                                params.push(Val::F(end));
                                format!("{col} < ?")
                            }
                        }
                    }
                })
            }
            Ast::Not(inner) => {
                let e = self.expr(tags, inner, params)?;
                Some(format!("NOT ({e})"))
            }
            Ast::And(v) | Ast::Or(v) => {
                let joiner = if matches!(ast, Ast::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                let parts: Vec<String> = v
                    .iter()
                    .filter_map(|a| self.expr(tags, a, params))
                    .collect();
                match parts.len() {
                    0 => None,
                    1 => parts.into_iter().next(),
                    _ => Some(format!("({})", parts.join(joiner))),
                }
            }
        }
    }

    /// The `WHERE` clause (leading space included) for the base condition
    /// and the filter, or `""`.
    #[must_use]
    pub fn where_clause(&self, tags: &[TagDef], ast: Option<&Ast>) -> (String, Vec<Val>) {
        let mut params = Vec::new();
        let filter = ast.and_then(|a| self.expr(tags, a, &mut params));
        let clause = match (self.base.is_empty(), filter) {
            (true, None) => String::new(),
            (false, None) => format!(" WHERE {}", self.base),
            (true, Some(f)) => format!(" WHERE {f}"),
            (false, Some(f)) => format!(" WHERE {} AND {f}", self.base),
        };
        (clause, params)
    }

    fn order_by(&self) -> String {
        let parts: Vec<String> = self
            .order
            .iter()
            .map(|(c, d)| match d {
                Dir::Asc => c.to_string(),
                Dir::Desc => format!("{c} DESC"),
            })
            .collect();
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", parts.join(", "))
        }
    }

    /// `FROM … WHERE …` (and `GROUP BY` under a group), with the filter in
    /// its place: on the rows for a flat spec, as a membership test on the
    /// members for a grouped one.
    fn body(&self, tags: &[TagDef], ast: Option<&Ast>) -> (String, Vec<Val>) {
        let (w, params) = self.where_clause(tags, ast);
        let Some(g) = self.group else {
            return (format!("FROM {}{w}", self.from), params);
        };
        let mut parts: Vec<String> = Vec::new();
        if !self.base.is_empty() {
            parts.push(self.base.to_string());
        }
        if ast.and_then(|a| self.expr(tags, a, &mut Vec::new())).is_some() {
            parts.push(format!("{g} IN (SELECT {g} FROM {}{w})", self.from));
        }
        let wh = if parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", parts.join(" AND "))
        };
        (format!("FROM {}{wh} GROUP BY {g}", self.from), params)
    }

    /// One page of rows.
    #[must_use]
    pub fn page(&self, tags: &[TagDef], ast: Option<&Ast>, offset: usize, limit: usize) -> Sql {
        let (body, mut params) = self.body(tags, ast);
        params.push(Val::I(limit as i64));
        params.push(Val::I(offset as i64));
        let sql = if self.group.is_some() {
            format!(
                "SELECT * FROM (SELECT {} {body}){} LIMIT ? OFFSET ?",
                self.select,
                self.order_by()
            )
        } else {
            format!("SELECT {} {body}{} LIMIT ? OFFSET ?", self.select, self.order_by())
        };
        Sql { sql, params }
    }

    /// How many rows match.
    #[must_use]
    pub fn count(&self, tags: &[TagDef], ast: Option<&Ast>) -> Sql {
        let (body, params) = self.body(tags, ast);
        let sql = if self.group.is_some() {
            format!("SELECT COUNT(*) FROM (SELECT 1 {body})")
        } else {
            format!("SELECT COUNT(*) {body}")
        };
        Sql { sql, params }
    }

    /// How many matching rows the order puts *before* a row with this key
    /// — its index. `key` has one value per `order` column.
    #[must_use]
    pub fn rank(&self, tags: &[TagDef], ast: Option<&Ast>, key: &[Val]) -> Sql {
        let (body, mut params) = self.body(tags, ast);
        let mut alts: Vec<String> = Vec::new();
        for (i, (col, dir)) in self.order.iter().enumerate() {
            let mut conj: Vec<String> = Vec::new();
            for (prev, _) in &self.order[..i] {
                conj.push(format!("{prev} = ?"));
            }
            conj.push(format!(
                "{col} {} ?",
                match dir {
                    Dir::Asc => "<",
                    Dir::Desc => ">",
                }
            ));
            for k in &key[..=i] {
                params.push(k.clone());
            }
            alts.push(format!("({})", conj.join(" AND ")));
        }
        let before = alts.join(" OR ");
        let sql = if self.group.is_some() {
            format!(
                "SELECT COUNT(*) FROM (SELECT {} {body}) WHERE ({before})",
                self.select
            )
        } else if body.contains(" WHERE ") {
            format!("SELECT COUNT(*) {body} AND ({before})")
        } else {
            format!("SELECT COUNT(*) {body} WHERE {before}")
        };
        Sql { sql, params }
    }
}

fn cmp(op: Op) -> &'static str {
    match op {
        Op::Eq => "=",
        Op::Gt => ">",
        Op::Gte => ">=",
        Op::Lt => "<",
        Op::Lte => "<=",
    }
}

/// A [`Datasource`] over the store: a [`SqlSpec`] plus how to read a row
/// and what its rank key is. Declared `static` beside the domain's other
/// queries; the suggest function is the one dynamic hook.
pub struct SqlSource<R> {
    pub spec: &'static SqlSpec,
    pub tags: &'static [TagDef],
    /// Decodes one row of `spec.select`.
    pub map: fn(&rusqlite::Row) -> rusqlite::Result<R>,
    /// A row's values for `spec.order`, in order.
    pub key: fn(&R) -> Vec<Val>,
    /// Suggestions for a dynamic tag: `(store, tag, typed prefix)`.
    pub suggest: fn(&Store, &str, &str) -> Vec<Suggestion>,
}

impl<R: Clone + 'static> Datasource for SqlSource<R> {
    type Row = R;

    fn tags(&self) -> &'static [TagDef] {
        self.tags
    }

    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize> {
        let q = self.spec.count(self.tags, ast);
        let n = store
            .rows_sql(
                self.spec.id,
                self.spec.describe,
                &q.sql,
                &q.params,
                |r| r.get::<_, i64>(0),
            )
            .first()
            .copied()
            .unwrap_or(0);
        Some(n.max(0) as usize)
    }

    fn page(&self, store: &Store, ast: Option<&Ast>, offset: usize, limit: usize) -> Rc<Vec<R>> {
        let q = self.spec.page(self.tags, ast, offset, limit);
        store.rows_sql(self.spec.id, self.spec.describe, &q.sql, &q.params, self.map)
    }

    fn index_of(&self, store: &Store, ast: Option<&Ast>, row: &R) -> Option<usize> {
        let q = self.spec.rank(self.tags, ast, &(self.key)(row));
        store
            .rows_sql(
                self.spec.id,
                self.spec.describe,
                &q.sql,
                &q.params,
                |r| r.get::<_, i64>(0),
            )
            .first()
            .map(|n| (*n).max(0) as usize)
    }

    fn suggest(&self, store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> {
        (self.suggest)(store, tag, prefix)
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// A rich table's state: the filter and the paging window over one
/// datasource. Holds no rows — every `row(i)` is a page lookup through the
/// store's cache.
pub struct Table<D: Datasource> {
    ds: D,
    page_size: usize,
    text: String,
    ast: Option<Ast>,
    errors: Vec<ParseError>,
    /// For a source without a count: how many pages the window covers.
    window: usize,
}

impl<D: Datasource> Table<D> {
    #[must_use]
    pub fn new(ds: D, page_size: usize) -> Self {
        Table {
            ds,
            page_size: page_size.max(1),
            text: String::new(),
            ast: None,
            errors: Vec::new(),
            window: 1,
        }
    }

    #[must_use]
    pub fn source(&self) -> &D {
        &self.ds
    }

    #[must_use]
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// The filter as typed.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn ast(&self) -> Option<&Ast> {
        self.ast.as_ref()
    }

    /// What could not be read, plus the tags this table does not have.
    #[must_use]
    pub fn errors(&self) -> &[ParseError] {
        &self.errors
    }

    /// The errors worth showing while the operator is still typing: a tag
    /// being spelled at the end of the line is not wrong yet.
    #[must_use]
    pub fn errors_while_typing(&self) -> Vec<&ParseError> {
        let partial = filter::typing_tag(&self.text);
        self.errors
            .iter()
            .filter(|e| match partial {
                None => true,
                Some(p) => {
                    e.message != "empty tag name" && e.message != format!("unknown tag: @{p}")
                }
            })
            .collect()
    }

    /// Re-parses the filter; the window resets. Returns whether it changed.
    pub fn set_filter(&mut self, text: &str) -> bool {
        if text == self.text {
            return false;
        }
        self.text = text.to_string();
        let parsed = filter::parse(text);
        let known: Vec<&str> = self.ds.tags().iter().map(|t| t.name).collect();
        let mut errors = parsed.errors;
        for t in filter::unknown_tags(parsed.ast.as_ref(), &known) {
            errors.push(ParseError {
                position: 0,
                message: format!("unknown tag: @{t}"),
            });
        }
        if let Some(ast) = &parsed.ast {
            self.value_errors(ast, &mut errors);
        }
        self.ast = parsed.ast;
        self.errors = errors;
        self.window = 1;
        true
    }

    /// A value a typed tag cannot read is an error the operator should
    /// see: the builder drops the comparison, and a silently dropped
    /// `@date>yesterday` would look like a filter that matched everything.
    fn value_errors(&self, ast: &Ast, errors: &mut Vec<ParseError>) {
        match ast {
            Ast::Op { tag, value, .. } => {
                let bad = match self.tag(tag).map(|t| t.kind) {
                    Some(TagType::Date) if date_span(value).is_none() => {
                        Some(format!("@{tag} wants a day, dd.mm.yyyy: {value}"))
                    }
                    Some(TagType::Number) if value.trim().parse::<f64>().is_err() => {
                        Some(format!("@{tag} wants a number: {value}"))
                    }
                    _ => None,
                };
                if let Some(message) = bad {
                    errors.push(ParseError {
                        position: 0,
                        message,
                    });
                }
            }
            Ast::Not(inner) => self.value_errors(inner, errors),
            Ast::And(v) | Ast::Or(v) => v.iter().for_each(|a| self.value_errors(a, errors)),
            Ast::Text(_) | Ast::Tag(_) => {}
        }
    }

    /// How many rows the table has — the count, or, for a source without
    /// one, what the window has loaded so far.
    #[must_use]
    pub fn len(&self, store: &Store) -> usize {
        if let Some(n) = self.ds.count(store, self.ast.as_ref()) {
            return n;
        }
        let mut n = 0;
        for p in 0..self.window {
            let page = self.ds.page(store, self.ast.as_ref(), p * self.page_size, self.page_size);
            n += page.len();
            if page.len() < self.page_size {
                break;
            }
        }
        n
    }

    #[must_use]
    pub fn is_empty(&self, store: &Store) -> bool {
        self.len(store) == 0
    }

    /// Row `i`, through its page.
    #[must_use]
    pub fn row(&self, store: &Store, i: usize) -> Option<D::Row> {
        let page = self.ds.page(
            store,
            self.ast.as_ref(),
            (i / self.page_size) * self.page_size,
            self.page_size,
        );
        page.get(i % self.page_size).cloned()
    }

    /// Rows `lo..hi`, as far as the table has them.
    #[must_use]
    pub fn rows(&self, store: &Store, lo: usize, hi: usize) -> Vec<D::Row> {
        (lo..hi).map_while(|i| self.row(store, i)).collect()
    }

    /// Where a row sits now — by rank when the source can say, else by
    /// walking the loaded window.
    #[must_use]
    pub fn index_of(&self, store: &Store, row: &D::Row) -> Option<usize>
    where
        D::Row: PartialEq,
    {
        if let Some(i) = self.ds.index_of(store, self.ast.as_ref(), row) {
            return (self.row(store, i).as_ref() == Some(row)).then_some(i);
        }
        let n = self.len(store);
        (0..n).find(|&i| self.row(store, i).as_ref() == Some(row))
    }

    /// The end of the list came on screen: a source without a count grows
    /// its window by a page, if the last one was full. A counted source
    /// needs nothing — every row already has a place.
    pub fn extend(&mut self, store: &Store) -> bool {
        if self.ds.count(store, self.ast.as_ref()).is_some() {
            return false;
        }
        let last = self.ds.page(
            store,
            self.ast.as_ref(),
            (self.window - 1) * self.page_size,
            self.page_size,
        );
        if last.len() < self.page_size {
            return false;
        }
        self.window += 1;
        true
    }

    /// What the autocomplete offers for a caret context: tag names, a
    /// static set, or the source's dynamic values — at most
    /// [`MAX_SUGGESTIONS`].
    #[must_use]
    pub fn suggestions(&self, store: &Store, ctx: &Context) -> Vec<Suggestion> {
        let mut out = match ctx {
            Context::Tag { partial, .. } => self
                .ds
                .tags()
                .iter()
                .filter(|t| t.name.starts_with(partial.as_str()))
                .map(|t| Suggestion {
                    label: format!("@{}", t.name),
                    value: t.name.to_string(),
                    describe: t.describe.to_string(),
                })
                .collect(),
            Context::Value { tag, partial, .. } => {
                let Some(def) = self.ds.tags().iter().find(|t| t.name == tag) else {
                    return Vec::new();
                };
                let typed = partial.trim_start_matches('"').to_lowercase();
                match def.values {
                    Values::None => Vec::new(),
                    Values::Static(list) => list
                        .iter()
                        .filter(|(l, v)| {
                            l.to_lowercase().starts_with(&typed)
                                || v.to_lowercase().starts_with(&typed)
                        })
                        .map(|(l, v)| Suggestion::labeled(*l, *v))
                        .collect(),
                    Values::Dynamic => self.ds.suggest(store, tag, &typed),
                }
            }
        };
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    /// The tag a value context is for, when the table has it.
    #[must_use]
    pub fn tag(&self, name: &str) -> Option<&'static TagDef> {
        self.ds.tags().iter().find(|t| t.name == name)
    }
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

/// What a text field completes: how the caret's context is read off the
/// line, what is offered for it, and how a pick splices back in. The box
/// under the field, its keys and the pick itself are one component in
/// `panels` (`Suggest`) that takes any of these — the filter grammar is one
/// ([`Table`] implements it), a compose panel's recipient list is another.
///
/// Pure by design: a completion is text in, text out, so it is tested
/// without a widget in sight.
pub trait Completion {
    /// What the caret is in the middle of typing.
    type Ctx: Clone + PartialEq;

    /// Classifies the caret, or `None` when completion would only be
    /// noise. Offsets in the context are byte indices into `text`.
    fn context(&self, text: &str, cursor: usize) -> Option<Self::Ctx>;

    /// The offer for a context — at most [`MAX_SUGGESTIONS`] rows.
    fn offer(&self, store: &Store, ctx: &Self::Ctx) -> Vec<Suggestion>;

    /// Splices a pick over what the context covers: the new line and
    /// where the caret lands.
    fn splice(&self, text: &str, cursor: usize, ctx: &Self::Ctx, pick: &Suggestion)
        -> (String, usize);
}

/// The filter grammar as a completion: tag names, then a tag's values,
/// spliced by [`filter::insert_tag`] and [`filter::insert_value`] — a
/// picked `@from` lands as `@from:` so its values open at once.
impl<D: Datasource> Completion for Table<D> {
    type Ctx = Context;

    fn context(&self, text: &str, cursor: usize) -> Option<Context> {
        filter::context(text, cursor)
    }

    fn offer(&self, store: &Store, ctx: &Context) -> Vec<Suggestion> {
        self.suggestions(store, ctx)
    }

    fn splice(
        &self,
        text: &str,
        cursor: usize,
        ctx: &Context,
        pick: &Suggestion,
    ) -> (String, usize) {
        match ctx {
            Context::Tag { start, .. } => {
                let takes_value = self.tag(&pick.value).is_some_and(|t| t.takes_value());
                filter::insert_tag(text, cursor, *start, &pick.value, takes_value)
            }
            Context::Value { start, .. } => filter::insert_value(text, cursor, *start, &pick.value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Item {
        id: i64,
        name: String,
        n: f64,
        ok: bool,
        at: f64,
    }

    fn item_row(r: &rusqlite::Row) -> rusqlite::Result<Item> {
        Ok(Item {
            id: r.get(0)?,
            name: r.get(1)?,
            n: r.get(2)?,
            ok: r.get(3)?,
            at: r.get(4)?,
        })
    }

    static SPEC: SqlSpec = SqlSpec {
        id: "items",
        describe: "the test items",
        select: "id, name, n, ok, at",
        from: "item",
        base: "id > 0",
        text: &["name"],
        tags: &[
            ("ok", TagSql::Where("ok = 1")),
            ("name", TagSql::Col("name")),
            ("n", TagSql::Col("n")),
            ("at", TagSql::Col("at")),
        ],
        order: &[("n", Dir::Desc), ("id", Dir::Asc)],
        group: None,
    };

    static TAGS: &[TagDef] = &[
        TagDef {
            name: "ok",
            kind: TagType::Bool,
            ops: &[],
            describe: "only the ok ones",
            values: Values::None,
        },
        TagDef {
            name: "name",
            kind: TagType::Text,
            ops: &[Op::Eq],
            describe: "by name",
            values: Values::Dynamic,
        },
        TagDef {
            name: "n",
            kind: TagType::Number,
            ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte],
            describe: "the number",
            values: Values::None,
        },
        TagDef {
            name: "at",
            kind: TagType::Date,
            ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte],
            describe: "when",
            values: Values::Static(&[("today", "02.09.2026"), ("yesterday", "01.09.2026")]),
        },
    ];

    fn suggest_names(store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> {
        assert_eq!(tag, "name");
        let p = prefix.to_string();
        store
            .rows_sql(
                "item names",
                "distinct names",
                "SELECT DISTINCT name FROM item ORDER BY name",
                &[],
                |r| r.get::<_, String>(0),
            )
            .iter()
            .filter(|n| n.to_lowercase().starts_with(&p))
            .map(Suggestion::value)
            .collect()
    }

    static SOURCE: SqlSource<Item> = SqlSource {
        spec: &SPEC,
        tags: TAGS,
        map: item_row,
        key: |it| vec![Val::F(it.n), Val::I(it.id)],
        suggest: suggest_names,
    };

    fn a(s: &str) -> Option<Ast> {
        filter::parse(s).ast
    }

    fn store_with(n: usize) -> Store {
        let s = Store::open(None).expect("store");
        s.write(move |c| {
            c.execute_batch(
                "CREATE TABLE item(id INTEGER PRIMARY KEY, name TEXT NOT NULL,
                                   n REAL NOT NULL, ok INTEGER NOT NULL, at REAL NOT NULL)",
            )?;
            for i in 1..=n {
                c.execute(
                    "INSERT INTO item(name, n, ok, at) VALUES(?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        format!("item {i:03} {}", ["alpha", "beta", "gamma"][i % 3]),
                        (i % 7) as f64,
                        i % 2 == 0,
                        // One a day from 2026-08-01 08:30.
                        (days_from_civil(2026, 8, 1) as f64) * 86_400.0
                            + 8.5 * 3600.0
                            + (i as f64) * 86_400.0,
                    ],
                )?;
            }
            Ok(())
        })
        .expect("seed");
        s
    }

    #[test]
    fn builds_pages_counts_and_ranks() {
        let q = SPEC.page(TAGS, None, 20, 10);
        assert_eq!(
            q.sql,
            "SELECT id, name, n, ok, at FROM item WHERE id > 0 ORDER BY n DESC, id LIMIT ? OFFSET ?"
        );
        assert_eq!(q.params, vec![Val::I(10), Val::I(20)]);

        let q = SPEC.count(TAGS, a("john").as_ref());
        assert_eq!(
            q.sql,
            "SELECT COUNT(*) FROM item WHERE id > 0 AND (name LIKE ? ESCAPE '\\')"
        );
        assert_eq!(q.params, vec![Val::S("%john%".into())]);

        let q = SPEC.count(TAGS, a("100% _x_").as_ref());
        assert_eq!(q.params, vec![Val::S("%100\\% \\_x\\_%".into())]);

        let q = SPEC.rank(TAGS, None, &[Val::F(3.0), Val::I(7)]);
        assert_eq!(
            q.sql,
            "SELECT COUNT(*) FROM item WHERE id > 0 AND ((n > ?) OR (n = ? AND id < ?))"
        );
        assert_eq!(q.params, vec![Val::F(3.0), Val::F(3.0), Val::I(7)]);
    }

    #[test]
    fn builds_every_filter_shape() {
        let w = |s: &str| SPEC.where_clause(TAGS, a(s).as_ref());
        assert_eq!(w("@ok"), (" WHERE id > 0 AND (ok = 1)".into(), vec![]));
        assert_eq!(w("@not:ok"), (" WHERE id > 0 AND NOT ((ok = 1))".into(), vec![]));
        assert_eq!(
            w("@name:Al"),
            (" WHERE id > 0 AND name LIKE ? ESCAPE '\\'".into(), vec![Val::S("%Al%".into())])
        );
        assert_eq!(
            w("@not:name:al"),
            (
                " WHERE id > 0 AND NOT (name LIKE ? ESCAPE '\\')".into(),
                vec![Val::S("%al%".into())]
            )
        );
        assert_eq!(w("@n>3"), (" WHERE id > 0 AND n > ?".into(), vec![Val::F(3.0)]));
        assert_eq!(w("@n<=3.5"), (" WHERE id > 0 AND n <= ?".into(), vec![Val::F(3.5)]));
        assert_eq!(w("@n:x"), (" WHERE id > 0".into(), vec![]), "unreadable: dropped");
        assert_eq!(
            w("@ok @n>1 alpha"),
            (
                " WHERE id > 0 AND ((ok = 1) AND n > ? AND (name LIKE ? ESCAPE '\\'))".into(),
                vec![Val::F(1.0), Val::S("%alpha%".into())]
            )
        );
        assert_eq!(
            w("(@ok @or @n>5)"),
            (" WHERE id > 0 AND ((ok = 1) OR n > ?)".into(), vec![Val::F(5.0)])
        );
        assert_eq!(
            w("@ok @or beta"),
            (
                " WHERE id > 0 AND ((ok = 1) OR (name LIKE ? ESCAPE '\\'))".into(),
                vec![Val::S("%beta%".into())]
            )
        );
        // Dates: a day is a span; `>` means after it, `:` means on it.
        let d0 = days_from_civil(2026, 8, 30) as f64 * 86_400.0;
        assert_eq!(
            w("@at>30.08.2026"),
            (" WHERE id > 0 AND at >= ?".into(), vec![Val::F(d0 + 86_400.0)])
        );
        assert_eq!(w("@at>=30.08.2026"), (" WHERE id > 0 AND at >= ?".into(), vec![Val::F(d0)]));
        assert_eq!(w("@at<30.08.2026"), (" WHERE id > 0 AND at < ?".into(), vec![Val::F(d0)]));
        assert_eq!(
            w("@at<=30.08.2026"),
            (" WHERE id > 0 AND at < ?".into(), vec![Val::F(d0 + 86_400.0)])
        );
        assert_eq!(
            w("@at:30.08.2026"),
            (
                " WHERE id > 0 AND (at >= ? AND at < ?)".into(),
                vec![Val::F(d0), Val::F(d0 + 86_400.0)]
            )
        );
        // The ISO spelling reads the same.
        assert_eq!(w("@at:2026-08-30"), w("@at:30.08.2026"));
        assert_eq!(
            w("@at:\"30.08.2026 09:14\""),
            (
                " WHERE id > 0 AND (at >= ? AND at < ?)".into(),
                vec![Val::F(d0 + 9.0 * 3600.0 + 14.0 * 60.0), Val::F(d0 + 9.0 * 3600.0 + 15.0 * 60.0)]
            )
        );
        // What does not bind is dropped, not turned into "nothing matches".
        assert_eq!(w("@bogus"), (" WHERE id > 0".into(), vec![]));
        assert_eq!(w("@ok:yes"), (" WHERE id > 0".into(), vec![]));
        assert_eq!(w("@at:whenever"), (" WHERE id > 0".into(), vec![]));
        assert_eq!(w("@bogus @ok"), (" WHERE id > 0 AND (ok = 1)".into(), vec![]));
        assert_eq!(date_span("01.13.2026"), None);
        assert_eq!(date_span("30.08.2026 25:00"), None);
        assert_eq!(date_span("30/08/2026"), None);
    }

    #[test]
    fn a_table_pages_through_the_store() {
        let s = store_with(25);
        let mut t = Table::new(&SOURCE, 10);
        assert_eq!(t.len(&s), 25);
        // Order: n DESC (i % 7), then id.
        let first = t.row(&s, 0).unwrap();
        assert_eq!((first.n, first.id), (6.0, 6));
        assert_eq!(t.row(&s, 1).unwrap().id, 13);
        assert_eq!(t.rows(&s, 8, 12).len(), 4, "a range spans pages");
        assert_eq!(t.row(&s, 24).unwrap().n, 0.0);
        assert_eq!(t.row(&s, 25), None);
        // Rank finds a row without walking.
        let r = t.row(&s, 17).unwrap();
        assert_eq!(t.index_of(&s, &r), Some(17));
        let ghost = Item { id: 999, ..r.clone() };
        assert_eq!(t.index_of(&s, &ghost), None, "a row that is not there");

        assert!(t.set_filter("@ok"));
        assert!(!t.set_filter("@ok"), "unchanged");
        assert_eq!(t.len(&s), 12);
        assert!(t.rows(&s, 0, 12).iter().all(|i| i.ok));
        t.set_filter("@n>=5 alpha");
        let rows = t.rows(&s, 0, 100);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|i| i.n >= 5.0 && i.name.contains("alpha")));
        t.set_filter("@name:\"item 003\"");
        assert_eq!(t.len(&s), 1);
        t.set_filter("@not:ok @n:0");
        assert_eq!(t.rows(&s, 0, 9).iter().map(|i| i.id).collect::<Vec<_>>(), vec![7, 21]);
        let d = days_from_civil(2026, 8, 1) + 20;
        t.set_filter(&format!("@at>{}.08.2026", d - days_from_civil(2026, 8, 1) + 1));
        assert_eq!(t.len(&s), 5);
        t.set_filter("@at:05.08.2026");
        assert_eq!(t.rows(&s, 0, 9).iter().map(|i| i.id).collect::<Vec<_>>(), vec![4]);

        // Errors: syntax, unknown tags, and what typing suppresses.
        t.set_filter("@bogus @name:\"x");
        assert_eq!(t.errors().len(), 2);
        t.set_filter("alpha @na");
        assert_eq!(t.errors()[0].message, "unknown tag: @na");
        assert!(t.errors_while_typing().is_empty());
        t.set_filter("alpha @na @ok");
        assert_eq!(t.errors_while_typing().len(), 1);
        // A value a typed tag cannot read is said, not silently dropped.
        t.set_filter("@at>yesterday @n:many");
        assert_eq!(t.errors()[0].message, "@at wants a day, dd.mm.yyyy: yesterday");
        assert_eq!(t.errors()[1].message, "@n wants a number: many");
        assert_eq!(t.len(&s), 25, "and the comparison is dropped");

        // Reactive: a commit re-runs the count and the pages on next access.
        t.set_filter("");
        assert_eq!(t.len(&s), 25);
        s.write(|c| {
            c.execute("INSERT INTO item(name, n, ok, at) VALUES('new', 9, 1, 0)", [])
        })
        .unwrap();
        assert_eq!(t.len(&s), 26);
        assert_eq!(t.row(&s, 0).unwrap().name, "new");
    }

    /// A source that cannot count: the window grows a page at a time as
    /// the end comes on screen.
    struct Stream(Vec<i64>);

    impl Datasource for Stream {
        type Row = i64;
        fn tags(&self) -> &'static [TagDef] {
            &[]
        }
        fn count(&self, _: &Store, _: Option<&Ast>) -> Option<usize> {
            None
        }
        fn page(&self, _: &Store, _: Option<&Ast>, offset: usize, limit: usize) -> Rc<Vec<i64>> {
            Rc::new(self.0.iter().skip(offset).take(limit).copied().collect())
        }
    }

    #[test]
    fn a_countless_source_grows_by_pages() {
        let s = Store::open(None).unwrap();
        let mut t = Table::new(Stream((0..23).collect()), 10);
        assert_eq!(t.len(&s), 10);
        assert!(t.extend(&s));
        assert_eq!(t.len(&s), 20);
        assert!(t.extend(&s));
        assert_eq!(t.len(&s), 23);
        assert!(!t.extend(&s), "the last page was short: nothing more");
        assert_eq!(t.len(&s), 23);
        assert_eq!(t.index_of(&s, &22), Some(22));
        assert_eq!(t.index_of(&s, &99), None);
        t.set_filter("x");
        assert_eq!(t.len(&s), 10, "a new filter resets the window");
    }

    #[test]
    fn suggestions_for_tags_and_values() {
        let s = store_with(3);
        let t = Table::new(&SOURCE, 10);
        let ctx = |text: &str| filter::context(text, text.len()).expect("a context");
        let names = |v: Vec<Suggestion>| v.into_iter().map(|s| s.label).collect::<Vec<_>>();
        assert_eq!(names(t.suggestions(&s, &ctx("@"))), vec!["@ok", "@name", "@n", "@at"]);
        assert_eq!(names(t.suggestions(&s, &ctx("@n"))), vec!["@name", "@n"]);
        let sug = t.suggestions(&s, &ctx("@o"));
        assert_eq!(sug[0].describe, "only the ok ones");
        assert_eq!(names(t.suggestions(&s, &ctx("@zz"))), Vec::<String>::new());
        // A static set completes on label or value; the pick is the value.
        let sug = t.suggestions(&s, &ctx("@at:t"));
        assert_eq!((sug[0].label.as_str(), sug[0].value.as_str()), ("today", "02.09.2026"));
        assert_eq!(sug[0].describe, "02.09.2026");
        assert_eq!(names(t.suggestions(&s, &ctx("@at:01.09"))), vec!["yesterday"]);
        // A dynamic set is the source's, under the typed prefix. A space
        // ends a bare value, so a partial with one is typed quoted — which
        // is how a pick with a space lands (see `filter::quote`).
        assert_eq!(
            names(t.suggestions(&s, &ctx("@name:item"))),
            vec!["item 001 beta", "item 002 gamma", "item 003 alpha"]
        );
        assert_eq!(names(t.suggestions(&s, &ctx("@name:\"item 002"))), vec!["item 002 gamma"]);
        assert_eq!(filter::context("@name:item 00", 13), None);
        // No values declared: nothing to offer.
        assert!(t.suggestions(&s, &ctx("@n>")).is_empty());
        assert!(t.suggestions(&s, &ctx("@bogus:")).is_empty());
    }

    /// The table as a completion: a pick over a tag context lands `@name:`
    /// for a tag with values and `@name ` for a boolean, so the value list
    /// opens by itself only where there is one; a value pick lands quoted
    /// when it has to be, with a space to type on.
    #[test]
    fn the_filter_is_a_completion() {
        let s = store_with(3);
        let t = Table::new(&SOURCE, 10);
        let pick = |text: &str, i: usize| {
            let c = t.context(text, text.len()).expect("a context");
            let offer = t.offer(&s, &c);
            t.splice(text, text.len(), &c, &offer[i])
        };
        assert_eq!(pick("@na", 0), ("@name:".into(), 6));
        assert_eq!(pick("@o", 0), ("@ok ".into(), 4));
        assert_eq!(pick("@ok @name:\"item 00", 0), ("@ok @name:\"item 001 beta\" ".into(), 26));
        assert_eq!(pick("x @at:t", 0), ("x @at:02.09.2026 ".into(), 17));
        assert_eq!(t.context("plain words", 5), None);
    }
}
