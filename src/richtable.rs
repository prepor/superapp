//! Shared list engine for filtering, completion, paging, cursors, and marks.
//!
//! A [`Datasource`] supplies counts, pages, stable keys, and filter metadata.
//! Countable sources load fixed pages as the viewport needs them. Other sources
//! grow their loaded window at the end. Cursors and [`Marks`] use stable keys,
//! so database changes and filters do not silently move the selection.

use std::collections::BTreeSet;
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

    /// What a row *is*, apart from what it currently shows: the inbox's
    /// thread anchor, a feed item's id. A [`Marks`] set is made of these,
    /// so a mark survives everything the store does under the row.
    type Key: Ord + Clone + 'static;

    /// The tags the filter accepts.
    fn tags(&self) -> &'static [TagDef];

    /// This row's identity.
    fn key(&self, row: &Self::Row) -> Self::Key;

    /// How many rows match, or `None` for a source that cannot say.
    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize>;

    /// Rows `offset..offset+limit` under the filter, in the source's order.
    fn page(&self, store: &Store, ast: Option<&Ast>, offset: usize, limit: usize)
        -> Rc<Vec<Self::Row>>;

    /// Every matching row's key, in the source's order — what `mark all`
    /// marks. `None` from a source that cannot list them, and then the
    /// surface does not offer it.
    fn keys(&self, _store: &Store, _ast: Option<&Ast>) -> Option<Vec<Self::Key>> {
        None
    }

    /// Which of these keys match the filter now; the rest are the marks it
    /// hides. Order is the caller's business, and no key comes back twice.
    fn present(&self, store: &Store, ast: Option<&Ast>, keys: &[Self::Key]) -> Vec<Self::Key> {
        let Some(all) = self.keys(store, ast) else {
            // A source that cannot list is taken at its word: nothing is
            // known to be hidden.
            return keys.to_vec();
        };
        let all: BTreeSet<Self::Key> = all.into_iter().collect();
        keys.iter().filter(|k| all.contains(k)).cloned().collect()
    }

    /// The row for a key regardless of the filter — what a hidden mark
    /// shows. The source's own `WHERE` still holds: the inbox knows inbox
    /// threads and nothing else. `None` when the row is gone, and from a
    /// source that cannot fetch one by key.
    fn by_key(&self, _store: &Store, _key: &Self::Key) -> Option<Self::Row> {
        None
    }

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
    type Key = D::Key;

    fn tags(&self) -> &'static [TagDef] {
        (**self).tags()
    }

    fn key(&self, row: &Self::Row) -> Self::Key {
        (**self).key(row)
    }

    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize> {
        (**self).count(store, ast)
    }

    fn page(&self, store: &Store, ast: Option<&Ast>, offset: usize, limit: usize)
        -> Rc<Vec<Self::Row>> {
        (**self).page(store, ast, offset, limit)
    }

    fn keys(&self, store: &Store, ast: Option<&Ast>) -> Option<Vec<Self::Key>> {
        (**self).keys(store, ast)
    }

    fn present(&self, store: &Store, ast: Option<&Ast>, keys: &[Self::Key]) -> Vec<Self::Key> {
        (**self).present(store, ast, keys)
    }

    fn by_key(&self, store: &Store, key: &Self::Key) -> Option<Self::Row> {
        (**self).by_key(store, key)
    }

    fn index_of(&self, store: &Store, ast: Option<&Ast>, row: &Self::Row) -> Option<usize> {
        (**self).index_of(store, ast, row)
    }

    fn suggest(&self, store: &Store, tag: &str, prefix: &str) -> Vec<Suggestion> {
        (**self).suggest(store, tag, prefix)
    }
}

// ---------------------------------------------------------------------------
// Marks
// ---------------------------------------------------------------------------

/// The rows the operator has **marked** for a batch verb: a set of
/// [`Datasource::Key`]s beside the cursor, and nothing else — no rows, no
/// store, no widget. A mark is an identity, so it survives the filter, the
/// paging and a sync landing under the list; sorting the set into what the
/// filter shows and what it hides is the table's job ([`Table::split`]).
///
/// Marks are context, not intent: they are held in a panel's memory, never
/// in the history, and go with the process.
#[derive(Debug, Clone, PartialEq)]
pub struct Marks<K: Ord + Clone> {
    set: BTreeSet<K>,
}

impl<K: Ord + Clone> Default for Marks<K> {
    fn default() -> Self {
        Marks {
            set: BTreeSet::new(),
        }
    }
}

impl<K: Ord + Clone> Marks<K> {
    /// Nothing marked.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Whether this row is marked.
    #[must_use]
    pub fn has(&self, key: &K) -> bool {
        self.set.contains(key)
    }

    /// Marks an unmarked row, unmarks a marked one; returns what it became.
    pub fn toggle(&mut self, key: K) -> bool {
        if self.set.remove(&key) {
            false
        } else {
            self.set.insert(key);
            true
        }
    }

    /// Marks a row; marking a marked row does nothing.
    pub fn add(&mut self, key: K) {
        self.set.insert(key);
    }

    pub fn remove(&mut self, key: &K) {
        self.set.remove(key);
    }

    /// Marks all of them — a range walked by shift+arrow, or every key
    /// under the filter.
    pub fn extend(&mut self, keys: impl IntoIterator<Item = K>) {
        self.set.extend(keys);
    }

    /// Keeps the marks the predicate holds — what a batch verb could not
    /// do stays marked.
    pub fn retain(&mut self, keep: impl FnMut(&K) -> bool) {
        self.set.retain(keep);
    }

    /// Empties the set and hands it over: what a batch verb acts on.
    pub fn take(&mut self) -> BTreeSet<K> {
        std::mem::take(&mut self.set)
    }

    pub fn clear(&mut self) {
        self.set.clear();
    }

    /// The marks in key order.
    pub fn iter(&self) -> impl Iterator<Item = &K> + '_ {
        self.set.iter()
    }

    /// The marks in key order, owned.
    #[must_use]
    pub fn keys(&self) -> Vec<K> {
        self.set.iter().cloned().collect()
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
    /// A row is a group (a thread is its inbox messages). The
    /// select is then aggregates over the members, `GROUP BY` this key, and
    /// the filter becomes a membership test: a group matches when **any**
    /// member matches, and its aggregates always cover the whole group.
    pub group: Option<&'static str>,
    /// The column that *is* the row: what a mark holds and what
    /// [`SqlSpec::keys`], [`SqlSpec::present`] and [`SqlSpec::by_key`] read
    /// and compare. The `group`'s alias under a group (a thread is its
    /// `thread`), else the unique column the `order` ends in. Named as the
    /// page names it — an alias of `select` under a group, since the page
    /// is read off the grouped subquery.
    pub key: &'static str,
    /// Tables this query reads that SQLite's authorizer cannot report, so
    /// that the store still knows when to re-run it. Empty for every table
    /// whose `from` is honest SQL; the effect log's is not — one arm of its
    /// union comes out of memory through a function, and rows that were
    /// never in the database are invisible to a read-set.
    pub deps: &'static [&'static str],
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
                // `COALESCE`, because a bare `NOT` is not a complement in
                // SQL: a column with no answer makes the inner expression
                // NULL, and `NOT NULL` is NULL — so the rows a tag says
                // nothing about would fall out of *both* halves of it.
                // `@not:risky` means every row that is not risky, and an
                // effect nobody was going to retry is one of them.
                Some(format!("NOT COALESCE(({e}), 0)"))
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
        self.body_and(tags, ast, None)
    }

    /// [`SqlSpec::body`] with one more condition on the row — under a
    /// group, on the group's own key, so it holds for the whole group and
    /// not for the member that matched. Its parameters follow the filter's,
    /// which sit further left in the text.
    fn body_and(&self, tags: &[TagDef], ast: Option<&Ast>, extra: Option<&str>)
        -> (String, Vec<Val>) {
        let (w, params) = self.where_clause(tags, ast);
        let Some(g) = self.group else {
            let w = match (w.is_empty(), extra) {
                (_, None) => w,
                (true, Some(e)) => format!(" WHERE {e}"),
                (false, Some(e)) => format!("{w} AND {e}"),
            };
            return (format!("FROM {}{w}", self.from), params);
        };
        let mut parts: Vec<String> = Vec::new();
        if !self.base.is_empty() {
            parts.push(self.base.to_string());
        }
        if ast.and_then(|a| self.expr(tags, a, &mut Vec::new())).is_some() {
            parts.push(format!("{g} IN (SELECT {g} FROM {}{w})", self.from));
        }
        if let Some(e) = extra {
            parts.push(e.to_string());
        }
        let wh = if parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", parts.join(" AND "))
        };
        (format!("FROM {}{wh} GROUP BY {g}", self.from), params)
    }

    /// The key as the body names it: the group's expression under a group
    /// (the alias only exists once the rows are grouped), else the key
    /// column itself.
    fn key_col(&self) -> &'static str {
        self.group.unwrap_or(self.key)
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

    /// Every matching row's key, in the table's order — what `mark all`
    /// marks. Under a group it reads off the same grouped subquery the page
    /// does, since the order names that subquery's aliases.
    #[must_use]
    pub fn keys(&self, tags: &[TagDef], ast: Option<&Ast>) -> Sql {
        let (body, params) = self.body(tags, ast);
        let sql = if self.group.is_some() {
            format!(
                "SELECT {} FROM (SELECT {} {body}){}",
                self.key,
                self.select,
                self.order_by()
            )
        } else {
            format!("SELECT {} {body}{}", self.key, self.order_by())
        };
        Sql { sql, params }
    }

    /// Which of `n` keys match — the marks the filter still shows. Built
    /// like the count: the same body, so a group matches when any member
    /// does, with `IN (?, …)` on the key. The keys' values follow the
    /// returned parameters, in the order they are asked about.
    #[must_use]
    pub fn present(&self, tags: &[TagDef], ast: Option<&Ast>, n: usize) -> Sql {
        let col = self.key_col();
        let holes = vec!["?"; n].join(", ");
        let (body, params) = self.body_and(tags, ast, Some(&format!("{col} IN ({holes})")));
        Sql {
            sql: format!("SELECT {col} {body}"),
            params,
        }
    }

    /// The row for one key, under the base condition only — a mark the
    /// filter hides is still read fresh, not shown from a snapshot. The
    /// key's value is the query's one parameter, appended by the caller.
    #[must_use]
    pub fn by_key(&self) -> Sql {
        let col = self.key_col();
        let sql = match (self.base.is_empty(), self.group) {
            (true, None) => format!("SELECT {} FROM {} WHERE {col} = ?", self.select, self.from),
            (false, None) => format!(
                "SELECT {} FROM {} WHERE {} AND {col} = ?",
                self.select, self.from, self.base
            ),
            (true, Some(g)) => format!(
                "SELECT {} FROM {} WHERE {col} = ? GROUP BY {g}",
                self.select, self.from
            ),
            (false, Some(g)) => format!(
                "SELECT {} FROM {} WHERE {} AND {col} = ? GROUP BY {g}",
                self.select, self.from, self.base
            ),
        };
        Sql {
            sql,
            params: Vec::new(),
        }
    }

    /// How many matching rows the order puts *before* a row with this
    /// order key — its index. `order_key` has one value per `order` column.
    #[must_use]
    pub fn rank(&self, tags: &[TagDef], ast: Option<&Ast>, order_key: &[Val]) -> Sql {
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
            for k in &order_key[..=i] {
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

/// How many keys one `IN (…)` carries: a marked-all inbox can be longer
/// than SQLite's parameter limit, so [`SqlSource::present`] asks in
/// chunks — every full one the same SQL text, so the cache is not blown
/// with a query text per set size either.
const KEYS_PER_QUERY: usize = 400;

/// A [`Datasource`] over the store: a [`SqlSpec`] plus how to read a row,
/// what its identity is and what its rank key is. Declared `static` beside
/// the domain's other queries; the suggest function is the one dynamic hook.
pub struct SqlSource<R, K> {
    pub spec: &'static SqlSpec,
    pub tags: &'static [TagDef],
    /// Decodes one row of `spec.select`.
    pub map: fn(&rusqlite::Row) -> rusqlite::Result<R>,
    /// A row's identity — the value of `spec.key`.
    pub key: fn(&R) -> K,
    /// A row's values for `spec.order`, in order.
    pub rank: fn(&R) -> Vec<Val>,
    /// Suggestions for a dynamic tag: `(store, tag, typed prefix)`.
    pub suggest: fn(&Store, &str, &str) -> Vec<Suggestion>,
}

impl<R, K> Datasource for SqlSource<R, K>
where
    R: Clone + 'static,
    K: Ord + Clone + Into<Val> + rusqlite::types::FromSql + 'static,
{
    type Row = R;
    type Key = K;

    fn tags(&self) -> &'static [TagDef] {
        self.tags
    }

    fn key(&self, row: &R) -> K {
        (self.key)(row)
    }

    fn count(&self, store: &Store, ast: Option<&Ast>) -> Option<usize> {
        let q = self.spec.count(self.tags, ast);
        let n = store
            .rows_sql_deps(
                self.spec.id,
                self.spec.describe,
                &q.sql,
                &q.params,
                self.spec.deps,
                |r| r.get::<_, i64>(0),
            )
            .first()
            .copied()
            .unwrap_or(0);
        Some(n.max(0) as usize)
    }

    fn page(&self, store: &Store, ast: Option<&Ast>, offset: usize, limit: usize) -> Rc<Vec<R>> {
        let q = self.spec.page(self.tags, ast, offset, limit);
        store.rows_sql_deps(
            self.spec.id,
            self.spec.describe,
            &q.sql,
            &q.params,
            self.spec.deps,
            self.map,
        )
    }

    fn keys(&self, store: &Store, ast: Option<&Ast>) -> Option<Vec<K>> {
        let q = self.spec.keys(self.tags, ast);
        let rows = store.rows_sql_deps(
            self.spec.id,
            self.spec.describe,
            &q.sql,
            &q.params,
            self.spec.deps,
            |r| r.get::<_, K>(0),
        );
        Some(rows.as_ref().clone())
    }

    fn present(&self, store: &Store, ast: Option<&Ast>, keys: &[K]) -> Vec<K> {
        let mut out = Vec::new();
        for chunk in keys.chunks(KEYS_PER_QUERY) {
            let mut q = self.spec.present(self.tags, ast, chunk.len());
            q.params.extend(chunk.iter().cloned().map(Into::into));
            let rows = store.rows_sql_deps(
                self.spec.id,
                self.spec.describe,
                &q.sql,
                &q.params,
                self.spec.deps,
                |r| r.get::<_, K>(0),
            );
            out.extend(rows.iter().cloned());
        }
        out
    }

    fn by_key(&self, store: &Store, key: &K) -> Option<R> {
        let mut q = self.spec.by_key();
        q.params.push(key.clone().into());
        store
            .rows_sql_deps(
                self.spec.id,
                self.spec.describe,
                &q.sql,
                &q.params,
                self.spec.deps,
                self.map,
            )
            .first()
            .cloned()
    }

    fn index_of(&self, store: &Store, ast: Option<&Ast>, row: &R) -> Option<usize> {
        let q = self.spec.rank(self.tags, ast, &(self.rank)(row));
        store
            .rows_sql_deps(
                self.spec.id,
                self.spec.describe,
                &q.sql,
                &q.params,
                self.spec.deps,
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

    /// Points the table at another source of the same shape — a mailbox
    /// panel replaced in place, inbox → archive. The filter text is carried
    /// over and read again, because what a tag means (and whether the new
    /// source has it at all) is the source's business; the window starts
    /// over, since a page offset into the old rows names nothing here.
    ///
    /// Carrying it over is not a promise that it survives: a table's filter
    /// belongs to whatever owns the field above it, and a panel whose kind
    /// changed reseeds that field from the new kind's params on the same
    /// frame. This only keeps a caller with no field of its own from losing
    /// one it never got the chance to re-supply.
    pub fn retarget(&mut self, ds: D) {
        self.ds = ds;
        let text = std::mem::take(&mut self.text);
        self.set_filter(&text);
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

    /// A row's identity — what a mark holds.
    #[must_use]
    pub fn key(&self, row: &D::Row) -> D::Key {
        self.ds.key(row)
    }

    /// Every key under the current filter, in the table's order — what
    /// `mark all` marks; `None` from a source that cannot list them.
    #[must_use]
    pub fn keys(&self, store: &Store) -> Option<Vec<D::Key>> {
        self.ds.keys(store, self.ast.as_ref())
    }

    /// Which of these keys the current filter still shows.
    #[must_use]
    pub fn present(&self, store: &Store, keys: &[D::Key]) -> Vec<D::Key> {
        self.ds.present(store, self.ast.as_ref(), keys)
    }

    /// The row for a key, filter or no filter — how a hidden mark is drawn.
    #[must_use]
    pub fn by_key(&self, store: &Store, key: &D::Key) -> Option<D::Row> {
        self.ds.by_key(store, key)
    }

    /// The marks the filter shows and the marks it hides, both in the set's
    /// order. A hidden mark is still a mark: it counts, it is drawn above
    /// the rows, and a batch verb acts on it.
    #[must_use]
    pub fn split(&self, store: &Store, marks: &Marks<D::Key>) -> (Vec<D::Key>, Vec<D::Key>) {
        let keys = marks.keys();
        let shown: BTreeSet<D::Key> = self.present(store, &keys).into_iter().collect();
        keys.into_iter().partition(|k| shown.contains(k))
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
        key: "id",
        deps: &[],
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

    /// The same items as **groups** — one row per `ok`, its members
    /// aggregated — so the marks' queries can be held to the page's shape
    /// under a `group`, where a group matches when any member does.
    static GROUPS: SqlSpec = SqlSpec {
        id: "groups",
        describe: "the test items, grouped",
        select: "i.ok AS g, MAX(i.at) AS last, COUNT(*) AS members",
        from: "item i",
        base: "i.id > 0",
        text: &["i.name"],
        tags: &[("ok", TagSql::Where("i.ok = 1")), ("name", TagSql::Col("i.name"))],
        order: &[("last", Dir::Desc), ("g", Dir::Desc)],
        group: Some("i.ok"),
        key: "g",
        deps: &[],
    };

    #[derive(Debug, Clone, PartialEq)]
    struct Group {
        g: i64,
        last: f64,
        members: i64,
    }

    fn group_row(r: &rusqlite::Row) -> rusqlite::Result<Group> {
        Ok(Group {
            g: r.get(0)?,
            last: r.get(1)?,
            members: r.get(2)?,
        })
    }

    static GROUP_SOURCE: SqlSource<Group, i64> = SqlSource {
        spec: &GROUPS,
        tags: TAGS,
        map: group_row,
        key: |g| g.g,
        rank: |g| vec![Val::F(g.last), Val::I(g.g)],
        suggest: suggest_names,
    };

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

    static SOURCE: SqlSource<Item, i64> = SqlSource {
        spec: &SPEC,
        tags: TAGS,
        map: item_row,
        key: |it| it.id,
        rank: |it| vec![Val::F(it.n), Val::I(it.id)],
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

    /// The marks' three questions, as SQL: the keys in the table's order,
    /// the `IN (…)` that sorts a set into shown and hidden, and the row for
    /// one key under the base condition only. Under a `group` all three are
    /// built the way the page and the count are, so a group answers for its
    /// members.
    #[test]
    fn builds_the_marks_queries() {
        let q = SPEC.keys(TAGS, None);
        assert_eq!(q.sql, "SELECT id FROM item WHERE id > 0 ORDER BY n DESC, id");
        assert!(q.params.is_empty());

        let q = SPEC.keys(TAGS, a("@ok").as_ref());
        assert_eq!(
            q.sql,
            "SELECT id FROM item WHERE id > 0 AND (ok = 1) ORDER BY n DESC, id"
        );

        let q = SPEC.present(TAGS, None, 3);
        assert_eq!(q.sql, "SELECT id FROM item WHERE id > 0 AND id IN (?, ?, ?)");
        assert!(q.params.is_empty(), "the keys' values are the caller's");

        let q = SPEC.present(TAGS, a("@n>1").as_ref(), 2);
        assert_eq!(
            q.sql,
            "SELECT id FROM item WHERE id > 0 AND n > ? AND id IN (?, ?)"
        );
        assert_eq!(q.params, vec![Val::F(1.0)], "the filter's, then the keys'");

        let q = SPEC.by_key();
        assert_eq!(q.sql, "SELECT id, name, n, ok, at FROM item WHERE id > 0 AND id = ?");

        // Grouped: the keys read off the same subquery the page does (the
        // order names its aliases), and the membership test is the filter's.
        let q = GROUPS.keys(TAGS, None);
        assert_eq!(
            q.sql,
            "SELECT g FROM (SELECT i.ok AS g, MAX(i.at) AS last, COUNT(*) AS members \
             FROM item i WHERE i.id > 0 GROUP BY i.ok) ORDER BY last DESC, g DESC"
        );
        let q = GROUPS.keys(TAGS, a("@ok").as_ref());
        assert_eq!(
            q.sql,
            "SELECT g FROM (SELECT i.ok AS g, MAX(i.at) AS last, COUNT(*) AS members \
             FROM item i WHERE i.id > 0 \
             AND i.ok IN (SELECT i.ok FROM item i WHERE i.id > 0 AND (i.ok = 1)) \
             GROUP BY i.ok) ORDER BY last DESC, g DESC"
        );
        // The `IN (…)` is on the group's key, not on the member that
        // matched — like the count, it needs no aggregates.
        let q = GROUPS.present(TAGS, a("@ok").as_ref(), 2);
        assert_eq!(
            q.sql,
            "SELECT i.ok FROM item i WHERE i.id > 0 \
             AND i.ok IN (SELECT i.ok FROM item i WHERE i.id > 0 AND (i.ok = 1)) \
             AND i.ok IN (?, ?) GROUP BY i.ok"
        );
        let q = GROUPS.by_key();
        assert_eq!(
            q.sql,
            "SELECT i.ok AS g, MAX(i.at) AS last, COUNT(*) AS members \
             FROM item i WHERE i.id > 0 AND i.ok = ? GROUP BY i.ok"
        );
    }

    #[test]
    fn builds_every_filter_shape() {
        let w = |s: &str| SPEC.where_clause(TAGS, a(s).as_ref());
        assert_eq!(w("@ok"), (" WHERE id > 0 AND (ok = 1)".into(), vec![]));
        // `COALESCE`, so a negation is the complement even where the tag's
        // column has no answer — `NOT NULL` is NULL, and those rows would
        // otherwise fall out of both halves of the tag.
        assert_eq!(
            w("@not:ok"),
            (" WHERE id > 0 AND NOT COALESCE(((ok = 1)), 0)".into(), vec![])
        );
        assert_eq!(
            w("@name:Al"),
            (" WHERE id > 0 AND name LIKE ? ESCAPE '\\'".into(), vec![Val::S("%Al%".into())])
        );
        assert_eq!(
            w("@not:name:al"),
            (
                " WHERE id > 0 AND NOT COALESCE((name LIKE ? ESCAPE '\\'), 0)".into(),
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

    /// The marks: a set of keys, and nothing else — no store in sight.
    #[test]
    fn marks_are_a_set_of_keys() {
        let mut m: Marks<i64> = Marks::new();
        assert!(m.is_empty() && !m.has(&1));
        assert_eq!(m.len(), 0);
        assert_eq!(m, Marks::default());

        assert!(m.toggle(7), "marked");
        assert!(!m.toggle(7), "and unmarked again");
        assert!(m.is_empty());

        m.add(3);
        m.add(3);
        assert_eq!(m.len(), 1, "marking a marked row does nothing");
        m.extend([1, 2, 3]);
        assert_eq!(m.keys(), vec![1, 2, 3], "in key order, whatever the order marked");
        assert_eq!(m.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(m.has(&2));
        m.remove(&2);
        m.remove(&2);
        assert_eq!(m.keys(), vec![1, 3]);

        // What a verb could not do stays marked.
        m.extend(4..=6);
        m.retain(|k| k % 2 == 0);
        assert_eq!(m.keys(), vec![4, 6]);

        // Take empties and hands the set over; clear just empties.
        let taken = m.take();
        assert_eq!(taken.into_iter().collect::<Vec<_>>(), vec![4, 6]);
        assert!(m.is_empty());
        m.extend([9]);
        m.clear();
        assert!(m.is_empty());

        // Any key, not just an id.
        let mut s: Marks<String> = Marks::new();
        assert!(s.toggle("b".to_string()));
        s.add("a".to_string());
        assert_eq!(s.keys(), vec!["a".to_string(), "b".to_string()]);
    }

    /// The three questions under the filter: every matching key (what `all`
    /// marks), which of a set the filter still shows, and the row for a key
    /// whether or not it matches — so a mark the filter hides is still a
    /// mark, read fresh.
    #[test]
    fn a_table_sorts_its_marks() {
        let s = store_with(25);
        let mut t = Table::new(&SOURCE, 10);

        // Keys are the table's order, and all of it — past the page size.
        let all = t.keys(&s).expect("a source that can list");
        assert_eq!(all.len(), 25);
        assert_eq!(all[..3], [6, 13, 20], "n DESC, id — the page's order");
        assert_eq!(t.key(&t.row(&s, 0).unwrap()), 6);

        t.set_filter("@ok");
        let ok = t.keys(&s).expect("keys");
        assert_eq!(ok.len(), 12);
        assert_eq!(ok, t.rows(&s, 0, 12).iter().map(|i| i.id).collect::<Vec<_>>());

        // A mark that left the filter sorts into hidden; the rest is shown.
        let mut marks = Marks::new();
        marks.extend([6, 7, 20]); // 7 is odd: not ok, so not shown.
        assert_eq!(t.present(&s, &marks.keys()), vec![6, 20]);
        assert_eq!(t.split(&s, &marks), (vec![6, 20], vec![7]));
        assert_eq!(t.split(&s, &Marks::new()), (vec![], vec![]));

        // And it still has a row: by_key ignores the filter, keeps the base.
        let hidden = t.by_key(&s, &7).expect("the row behind a hidden mark");
        assert_eq!((hidden.id, hidden.ok), (7, false));
        assert_eq!(t.by_key(&s, &999), None, "a key that is not there");

        // Reactive like the page: the keys follow a commit.
        s.write(|c| c.execute("UPDATE item SET ok = 1 WHERE id = 7", []))
            .unwrap();
        assert_eq!(t.present(&s, &marks.keys()), vec![6, 7, 20]);
        assert_eq!(t.keys(&s).map(|k| k.len()), Some(13));
    }

    /// Under a `group` the three queries answer for the group: it is marked
    /// when any member matches the filter, and its row is the aggregate.
    #[test]
    fn a_grouped_source_marks_by_group() {
        let s = store_with(25);
        let mut t = Table::new(&GROUP_SOURCE, 10);
        assert_eq!(t.len(&s), 2, "the ok items and the rest");
        assert_eq!(t.keys(&s), Some(vec![0, 1]), "latest first: item 25 is not ok");

        t.set_filter("@ok");
        assert_eq!(t.keys(&s), Some(vec![1]));
        let mut marks = Marks::new();
        marks.extend([0, 1]);
        assert_eq!(t.split(&s, &marks), (vec![1], vec![0]));
        // A group whose *members* match a text filter comes back whole.
        t.set_filter("alpha");
        assert_eq!(t.keys(&s), Some(vec![0, 1]));
        assert_eq!(t.by_key(&s, &0).map(|g| g.members), Some(13), "all of it");
    }

    /// A source that cannot count: the window grows a page at a time as
    /// the end comes on screen.
    struct Stream(Vec<i64>);

    impl Datasource for Stream {
        type Row = i64;
        type Key = i64;
        fn tags(&self) -> &'static [TagDef] {
            &[]
        }
        fn key(&self, row: &i64) -> i64 {
            *row
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
        // It cannot list its keys either, so `all` is not offered; marks
        // are taken at their word, and nothing is known to be hidden.
        assert_eq!(t.keys(&s), None);
        assert_eq!(t.present(&s, &[3, 99]), vec![3, 99]);
        assert_eq!(t.split(&s, &Marks::default()), (vec![], vec![]));
        assert_eq!(t.by_key(&s, &3), None);
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
