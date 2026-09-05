//! A panel as text for an agent: what it is about, the queries that drew it,
//! their rows as of now, and what has lately left the process about it.
//!
//! The pieces are already there — a panel says what it is
//! [`about`](crate::panel::Panel::about), and every draw records the queries
//! it ran ([`Store::trace_of`]) — so this module is the assembly and nothing
//! more: [`of`] takes what a slot knows and [`render`] writes it out.
//!
//! Two properties are the whole point of the shape below. The **rows are
//! re-read at render time**, off the store's own reader, from the traced SQL
//! and the parameters it was bound with, so the model reads what the panel
//! shows *now* rather than a snapshot from whenever the chip was made. And
//! the text is a **reference, not a transcript**: everything here can be
//! derived again from the identity, which is why a panel that has since
//! closed still renders.
//!
//! [`header_line`] and [`parse_header`] are the other direction — the one
//! line that makes a copied panel context recognizable as a panel when it is
//! pasted back somewhere, so a paste can become a chip instead of prose.

use crate::effect::{Job, LOG};
use crate::layout::SlotId;
use crate::panel::{PanelId, Tag};
use crate::session::Session;
use crate::store::{Store, TraceEntry, Val};
use crate::time::fmt_date;

/// How much text one panel is worth. A chip past this is cut, with a line
/// saying by how much: a chat that quietly dropped half a table would have
/// the model answering about rows it never saw.
pub const CAP: usize = 32 * 1024;

/// How many rows of one query are printed — the panel's own page, which is
/// what the person is looking at.
pub const ROWS: usize = 50;

/// How many recent effects are worth listing. Ten is the tail a person would
/// read; the log panel is where the rest is.
pub const EFFECTS: usize = 10;

/// How wide one cell is allowed to be, in characters, before it is cut. A
/// letter's body in a table cell is the whole letter otherwise.
const CELL: usize = 200;

/// The line that says "this text is a panel". First of what `cmd+i` copies,
/// and the one thing a paste is read for.
const HEADER: &str = "superapp-panel:";

/// Everything about one open panel that an agent is given: who it is, where
/// it stands, what it says of itself, and the queries its last draw ran.
///
/// Taken from a live slot ([`of`]) and then owned: a chip holds one of these,
/// and rendering it needs nothing but the store.
#[derive(Debug, Clone)]
pub struct PanelContext {
    pub id: PanelId,
    pub title: String,
    /// The workspace as a person counts them, from one. Zero for a slot the
    /// layout no longer places.
    pub workspace: usize,
    pub about: String,
    /// The trace of the panel's last draw — its provenance, and what the
    /// rows below are re-read from.
    pub queries: Vec<TraceEntry>,
}

/// What a slot is showing, as an agent would be told it. `None` for a slot
/// with no instance in it.
#[must_use]
pub fn of(s: &Session, slot: SlotId) -> Option<PanelContext> {
    let inst = s.panel(slot)?;
    let (id, title, about) = {
        let p = inst.borrow();
        (p.id().clone(), p.title(), p.about())
    };
    Some(PanelContext {
        id,
        title,
        workspace: s.ws().ws_of(slot).map_or(0, |k| k + 1),
        about,
        queries: s.store().trace_of(slot),
    })
}

/// The panel as one block of text for the model.
///
/// The shape, in order: the opening tag with the identity, the title and the
/// workspace as attributes; the panel's own paragraph; `## queries`, and per
/// traced query its purpose, its parameters, its SQL, and a markdown table of
/// the rows **as they read now** — the SQL is prepared again on the store's
/// reader (query-only by construction) and bound with the values the draw
/// bound; then `## recent effects`, at most [`EFFECTS`] lines, absent when
/// there are none.
///
/// Nothing here asks the session for anything, so a panel that has since
/// closed renders exactly as it did while it was open.
#[must_use]
pub fn render(store: &Store, cx: &PanelContext, effects: &[Job]) -> String {
    let mut out = format!(
        "<panel id=\"{}\" title=\"{}\" workspace=\"{}\">\n",
        attr(&cx.id.to_string()),
        attr(&cx.title),
        cx.workspace
    );
    out.push_str(cx.about.trim_end());
    out.push_str("\n\n## queries\n");
    for e in &cx.queries {
        out.push_str(&format!("\n### {} — {}\n", e.id, e.describe));
        if !e.params.is_empty() {
            out.push_str(&format!("params: {}\n", e.params));
        }
        out.push_str(&format!("```sql\n{}\n```\n", collapse(&e.sql)));
        out.push_str(&rows_now(store, e));
    }
    let recent: Vec<&Job> = effects.iter().take(EFFECTS).collect();
    if !recent.is_empty() {
        out.push_str("\n## recent effects\n");
        for job in recent {
            out.push_str(&effect_line(job));
            out.push('\n');
        }
    }
    cut(&mut out);
    out.push_str("</panel>\n");
    out
}

/// The line that says which panel a copied context is, so that a paste of it
/// somewhere else can be read back as a panel rather than as prose:
/// `superapp-panel: message ["42"]`.
#[must_use]
pub fn header_line(id: &PanelId) -> String {
    format!("{HEADER} {} {}", id.tag.as_str(), id.args_json())
}

/// The panel a text names, read off its **first line** and nothing else.
/// `None` for any other text — which is what makes the paste rule safe: a
/// paste that is not a panel context is ordinary text.
///
/// The tag is interned, since the spelling arrives as a `String` and the app
/// that owns it may not be in this build at all.
#[must_use]
pub fn parse_header(text: &str) -> Option<PanelId> {
    let line = text.lines().next()?.trim();
    let rest = line.strip_prefix(HEADER)?.trim_start();
    let (tag, args) = rest.split_once(char::is_whitespace)?;
    if tag.is_empty() {
        return None;
    }
    PanelId::from_row(Tag::intern(tag), args.trim())
}

/// The effect log's rows about this panel's arguments — the queue and the
/// ring both, newest first, `n` at most.
///
/// An entity **names** an argument when it is that argument (a path) or when
/// it ends in it after a colon (`mail:42`, `outbox:7`) — the `action.entity`
/// vocabulary is the apps' own and the kernel reads no further into it than
/// that. Two apps that number different things the same way therefore read
/// the same here; the sentence on each line says which is which.
///
/// Only what **wrote** — the same narrowing the log panel opens on, for the
/// same reason: a background pass asks the outside a dozen questions for
/// every answer it acts on, and a chip full of *connect*, *select*, *search*
/// tells a model nothing about what happened to the thing it is looking at.
///
/// A panel with no arguments stands for no one thing, so it gets nothing: an
/// unfiltered inbox is not what a move of uid 91 was about.
#[must_use]
pub fn recent_effects(store: &Store, id: &PanelId, n: usize) -> Vec<Job> {
    if id.args.is_empty() || n == 0 {
        return Vec::new();
    }
    // Built from the log's own spec rather than from a second copy of its
    // SQL: the union of the queue and the ring is one text in one place.
    let spec = LOG.spec;
    let mut params: Vec<Val> = Vec::new();
    let mut conds: Vec<String> = Vec::new();
    for (i, arg) in id.args.iter().enumerate() {
        let p = i + 1;
        conds.push(format!(
            "e.entity = ?{p} OR (instr(e.entity, ':') > 0 \
             AND substr(e.entity, instr(e.entity, ':') + 1) = ?{p})"
        ));
        params.push(Val::S(arg.clone()));
    }
    let limit = params.len() + 1;
    let sql = format!(
        "SELECT {} FROM {} WHERE e.writes = 1 AND e.entity IS NOT NULL AND ({}) \
         ORDER BY e.created DESC, e.id DESC LIMIT ?{limit}",
        spec.select,
        spec.from,
        conds.join(" OR ")
    );
    params.push(Val::I(i64::try_from(n).unwrap_or(i64::MAX)));
    let rows = store.rows_sql_deps(
        "panel effects",
        "what has lately left the process about this panel's arguments",
        &sql,
        &params,
        spec.deps,
        LOG.map,
    );
    (*rows).clone()
}

// -- the parts -----------------------------------------------------------------

/// One traced query's rows, re-read now: the columns, at most [`ROWS`] rows,
/// and the line that says how many of how many. A query that will not run
/// again — a table dropped, a text no longer valid — says so on one line
/// instead, because the point of the SQL above it is that it can be checked.
fn rows_now(store: &Store, e: &TraceEntry) -> String {
    match read(store, e) {
        Ok((table, k)) => format!("{table}rows ({k} of {}, the panel's own page)\n", e.rows),
        Err(err) => format!(
            "could not re-read this query: {}\n",
            one_line(&err.to_string())
        ),
    }
}

/// The markdown table, and how many rows went into it.
fn read(store: &Store, e: &TraceEntry) -> rusqlite::Result<(String, usize)> {
    let mut stmt = store.conn().prepare(&e.sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(ToString::to_string)
        .collect();
    let mut out = md_row(names.iter().map(String::as_str));
    out.push_str(&md_rule(names.len()));
    let mut rows = stmt.query(rusqlite::params_from_iter(e.values.iter()))?;
    let mut k = 0;
    while k < ROWS {
        let Some(r) = rows.next()? else { break };
        let mut cells: Vec<String> = Vec::with_capacity(names.len());
        for i in 0..names.len() {
            cells.push(cell(&r.get_ref(i)?));
        }
        out.push_str(&md_row(cells.iter().map(String::as_str)));
        k += 1;
    }
    Ok((out, k))
}

/// One value as text. Numbers as they were written, text on one line and cut
/// where it runs long, a blob as its size, null as nothing at all.
fn cell(v: &rusqlite::types::ValueRef<'_>) -> String {
    use rusqlite::types::ValueRef;
    let text = match v {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => one_line(&String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => format!("<blob {} bytes>", b.len()),
    };
    // The pipe is the table's own punctuation: a value carrying one would
    // silently give the model a column it does not have.
    clip(&text).replace('|', "\\|")
}

/// One row of a markdown table.
fn md_row<'a>(cells: impl Iterator<Item = &'a str>) -> String {
    let mut s = String::from("|");
    for c in cells {
        s.push(' ');
        s.push_str(c);
        s.push_str(" |");
    }
    s.push('\n');
    s
}

/// The rule under a markdown table's head.
fn md_rule(n: usize) -> String {
    let mut s = String::from("|");
    for _ in 0..n {
        s.push_str(" --- |");
    }
    s.push('\n');
    s
}

/// One effect as the log reads it aloud: what it was, how it went, and when.
/// A filed job's sentence is the effect registry's, which only a world can
/// decode — the caller fills [`Job::what`] from it where it has one, and this
/// falls back to the kind and the payload rather than saying nothing.
fn effect_line(job: &Job) -> String {
    let what = job.what.clone().unwrap_or_else(|| {
        let payload = clip(&one_line(&job.payload));
        if payload.is_empty() {
            job.kind.clone()
        } else {
            format!("{} {payload}", job.kind)
        }
    });
    let mut status = job.status_line();
    if let Some(err) = &job.error {
        status.push_str(&format!(": {}", clip(&one_line(err))));
    }
    format!("{what} — {status}, {}", fmt_date(job.created))
}

/// The whole text, cut to [`CAP`] with a line saying what was lost. The
/// closing tag is added after this, so a cut block is still a whole block.
fn cut(out: &mut String) {
    if out.len() <= CAP {
        return;
    }
    let mut at = CAP;
    while at > 0 && !out.is_char_boundary(at) {
        at -= 1;
    }
    let lost = out.len() - at;
    out.truncate(at);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("… cut: {lost} more bytes\n"));
}

/// An attribute's value: the three characters that would end it early.
fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// SQL as `cmd+i` prints it: the whitespace a `static` was laid out with
/// collapsed away.
fn collapse(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A value on one line: every run of whitespace or control characters,
/// newlines included, is a single space. The control characters matter — a
/// column that packs a list into one string with a separator byte (the
/// mailbox's participants do) would otherwise run its words together.
fn one_line(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Cut to [`CELL`] characters, with the ellipsis that says so.
fn clip(s: &str) -> String {
    match s.char_indices().nth(CELL) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::app::{App, Root};
    use crate::effect::{Ctx, Effect};
    use crate::panel::{Opening, Panel, PanelId, PanelKind, Tag};
    use crate::session::{Action, Session};
    use crate::store::Q;

    // -- an app with one table, one panel, and one thing it can do ----------

    const NOTE: Tag = Tag("note");

    static NOTES: Q = Q {
        id: "notes",
        sql: "SELECT id, body FROM note WHERE id >= ?1 ORDER BY id",
        describe: "the notes from one id on",
    };

    /// The same rows, wide enough that a page of them is worth more than one
    /// chip — what the cap is there for.
    static WIDE: Q = Q {
        id: "notes wide",
        sql: "SELECT id, body AS a, body AS b, body AS c, body AS d, body AS e,
                     body AS f, body AS g, body AS h, body AS i, body AS j
                FROM note ORDER BY id",
        describe: "the notes, every column of them",
    };

    /// A panel that reads its rows the way a real one does — through the
    /// store, so the read lands in the trace the shell opened.
    struct Notes {
        id: PanelId,
        store: std::rc::Rc<crate::store::Store>,
    }

    impl Notes {
        /// One draw's worth of reading. A panel asked for `wide` reads the
        /// fat query too, which is one query more in its trace.
        fn draw(&self) {
            let from: i64 = self.id.arg(0).and_then(|a| a.parse().ok()).unwrap_or(0);
            let _ = self.store.rows(&NOTES, &[Val::I(from)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            });
            if self.id.arg(1) == Some("wide") {
                let _ = self.store.rows(&WIDE, &[], |r| r.get::<_, i64>(0));
            }
        }
    }

    impl Panel for Notes {
        fn id(&self) -> &PanelId {
            &self.id
        }
        fn title(&self) -> String {
            "notes".into()
        }
        fn about(&self) -> String {
            "the notes, one row a note.".into()
        }
        fn as_any(&mut self) -> &mut dyn Any {
            self
        }
    }

    struct NoteKind;
    impl PanelKind for NoteKind {
        fn tag(&self) -> Tag {
            NOTE
        }
        fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
            Box::new(Notes {
                id: id.clone(),
                store: cx.session().store().clone(),
            })
        }
    }

    static NOTE_KIND: NoteKind = NoteKind;
    static KINDS: &[&dyn PanelKind] = &[&NOTE_KIND];

    static SCHEMA: crate::app::Schema = crate::app::Schema {
        app: "notes",
        steps: &[crate::app::Step::Sql(
            "CREATE TABLE note(id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
        )],
    };

    /// One thing that leaves the process and changes something, so the ring
    /// has a row to be found by. In memory: it is filed nowhere and still
    /// shows in the log.
    #[derive(Serialize, Deserialize)]
    struct Look {
        mail: i64,
    }

    /// One that only asks. The log's own default leaves these out, and so
    /// does a chip: a pass asks a dozen questions for every answer it acts
    /// on.
    #[derive(Serialize, Deserialize)]
    struct Peek {
        mail: i64,
    }

    impl Effect for Peek {
        type Reply = ();
        const KIND: &'static str = "notes.peek";
        fn describe(&self) -> String {
            format!("peek at mail {}", self.mail)
        }
        fn writes(&self) -> bool {
            false
        }
        fn entity(&self) -> Option<String> {
            Some(format!("mail:{}", self.mail))
        }
        fn perform(&self, _cx: &mut Ctx<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    impl Effect for Look {
        type Reply = ();
        const KIND: &'static str = "notes.look";
        fn describe(&self) -> String {
            format!("look at mail {}", self.mail)
        }
        fn writes(&self) -> bool {
            true
        }
        fn entity(&self) -> Option<String> {
            Some(format!("mail:{}", self.mail))
        }
        fn perform(&self, _cx: &mut Ctx<'_>) -> Result<(), String> {
            Ok(())
        }
    }

    struct NoteApp;
    impl App for NoteApp {
        fn id(&self) -> &'static str {
            "notes"
        }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] {
            KINDS
        }
        fn schema(&self) -> Option<&'static crate::app::Schema> {
            Some(&SCHEMA)
        }
        fn roots(&self) -> Vec<Root> {
            vec![Root::new(PanelId::bare(NOTE), "notes", "")]
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    static NOTE_APP: NoteApp = NoteApp;
    static APPS: &[&dyn App] = &[&NOTE_APP];

    /// A session with `n` notes in it, and one note panel open on the slot
    /// this answers — drawn once, so its trace is there.
    fn session(n: usize, body: &str) -> (Session, SlotId) {
        session_on(PanelId::new(NOTE, ["1"]), n, body)
    }

    /// The same, on an identity the caller picks.
    fn session_on(id: PanelId, n: usize, body: &str) -> (Session, SlotId) {
        let mut s = Session::fake(APPS);
        let body = body.to_string();
        s.store()
            .write(move |tx| {
                for i in 1..=n {
                    tx.execute(
                        "INSERT INTO note(id, body) VALUES(?1, ?2)",
                        rusqlite::params![i64::try_from(i).unwrap_or(0), body],
                    )?;
                }
                Ok(())
            })
            .expect("the notes");
        s.act(Action::new("open", "open").moving(move |wm| {
            wm.open(id, None, false);
        }));
        s.settle();
        let slot = s.focus().expect("the new slot");
        draw(&s, slot);
        (s, slot)
    }

    /// What the shell does around a panel's draw.
    fn draw(s: &Session, slot: SlotId) {
        s.store().trace_begin(slot);
        let inst = s.panel(slot).expect("the instance");
        let mut b = inst.borrow_mut();
        b.as_any()
            .downcast_mut::<Notes>()
            .expect("a note panel")
            .draw();
        s.store().trace_end();
    }

    /// The whole shape, on a panel that read one query: the identity in the
    /// attributes, the panel's own paragraph, the SQL as it ran, the rows as
    /// they read now, and the count line under them.
    #[test]
    fn a_panel_renders_as_its_identity_its_words_and_its_rows() {
        let (s, slot) = session(3, "hello");
        let cx = of(&s, slot).expect("a context for an open slot");
        assert_eq!(cx.id, PanelId::new(NOTE, ["1"]));
        assert_eq!(cx.title, "notes");
        assert_eq!(cx.workspace, 1);
        assert_eq!(cx.queries.len(), 1, "one query drew it");

        let text = render(s.store(), &cx, &[]);
        assert!(
            text.starts_with("<panel id=\"note(1)\" title=\"notes\" workspace=\"1\">\n"),
            "{text}"
        );
        assert!(text.contains("the notes, one row a note."), "{text}");
        assert!(
            text.contains("### notes — the notes from one id on"),
            "{text}"
        );
        assert!(text.contains("params: 1"), "{text}");
        assert!(
            text.contains("```sql\nSELECT id, body FROM note WHERE id >= ?1 ORDER BY id\n```"),
            "{text}"
        );
        assert!(text.contains("| id | body |\n| --- | --- |\n"), "{text}");
        assert!(text.contains("| 1 | hello |\n"), "{text}");
        assert!(text.contains("| 3 | hello |\n"), "{text}");
        assert!(
            text.contains("rows (3 of 3, the panel's own page)"),
            "{text}"
        );
        assert!(text.ends_with("</panel>\n"), "{text}");
        assert!(
            !text.contains("## recent effects"),
            "no effects, no section: {text}"
        );
    }

    /// The rows are read at render time, not at chip time: a note written
    /// after the draw is in the text.
    #[test]
    fn the_rows_are_the_rows_as_they_read_now() {
        let (s, slot) = session(1, "first");
        let cx = of(&s, slot).expect("a context");
        s.store()
            .write(|tx| {
                tx.execute("INSERT INTO note(id, body) VALUES(2, 'second')", [])?;
                Ok(())
            })
            .expect("the second note");
        let text = render(s.store(), &cx, &[]);
        assert!(text.contains("| 2 | second |"), "{text}");
        // The count line still says what the *draw* found: that is the page
        // the person is looking at.
        assert!(
            text.contains("rows (2 of 1, the panel's own page)"),
            "{text}"
        );
    }

    /// A closed panel still renders: nothing below `of` asks the session
    /// anything.
    #[test]
    fn a_panel_that_has_closed_still_renders() {
        let (mut s, slot) = session(2, "kept");
        let cx = of(&s, slot).expect("a context");
        s.act(Action::new("close", "close").moving(move |wm| {
            wm.close(slot);
        }));
        s.settle();
        assert!(s.panel(slot).is_none(), "the slot is gone");
        assert!(of(&s, slot).is_none(), "and so is its context");

        let text = render(s.store(), &cx, &[]);
        assert!(text.contains("| 1 | kept |"), "{text}");
        assert!(
            text.contains("rows (2 of 2, the panel's own page)"),
            "{text}"
        );
    }

    /// A table too fat for one chip is cut, and the text says by how much.
    #[test]
    fn a_fat_table_is_cut_and_says_so() {
        let (s, slot) = session_on(PanelId::new(NOTE, ["1", "wide"]), 60, &"x".repeat(190));
        let cx = of(&s, slot).expect("a context");
        let text = render(s.store(), &cx, &[]);
        assert!(text.len() > CAP, "the cut line and the tag follow it");
        assert!(
            text.len() < CAP + 200,
            "and nothing else does: {}",
            text.len()
        );
        let cut = text
            .lines()
            .rev()
            .nth(1)
            .expect("the line above the closing tag");
        assert!(cut.starts_with("… cut: "), "{cut}");
        assert!(cut.ends_with(" more bytes"), "{cut}");
        assert!(text.ends_with("</panel>\n"));
    }

    /// A value long enough to bury the table is cut in its cell, and a value
    /// with a newline in it stays on one line.
    #[test]
    fn a_long_value_is_cut_and_a_multi_line_one_is_flattened() {
        let (s, slot) = session(1, &format!("one\ntwo {}", "y".repeat(400)));
        let cx = of(&s, slot).expect("a context");
        let text = render(s.store(), &cx, &[]);
        let row = text
            .lines()
            .find(|l| l.starts_with("| 1 |"))
            .expect("the note's row");
        assert!(row.contains("one two yyy"), "{row}");
        assert!(row.ends_with("… |"), "{row}");
        assert_eq!(text.lines().filter(|l| l.starts_with("| 1 |")).count(), 1);
    }

    /// A query the store will not run again says so where its table would
    /// have been, and the rest of the panel is unaffected.
    #[test]
    fn a_query_that_will_not_run_again_says_so() {
        let (s, slot) = session(1, "hello");
        let mut cx = of(&s, slot).expect("a context");
        cx.queries[0].sql = "SELECT * FROM nothing_of_the_sort".to_string();
        cx.queries[0].values.clear();
        let text = render(s.store(), &cx, &[]);
        assert!(text.contains("could not re-read this query:"), "{text}");
        assert!(text.contains("nothing_of_the_sort"), "{text}");
        assert!(text.ends_with("</panel>\n"), "{text}");
    }

    /// The attributes are escaped, so a title with a quote in it does not
    /// end one early.
    #[test]
    fn the_attributes_are_escaped() {
        let cx = PanelContext {
            id: PanelId::new(Tag("note"), ["a & b"]),
            title: "he said \"hi\" <loudly>".into(),
            workspace: 2,
            about: "a note".into(),
            queries: Vec::new(),
        };
        let s = Session::fake(APPS);
        let text = render(s.store(), &cx, &[]);
        assert!(
            text.starts_with(
                "<panel id=\"note(a &amp; b)\" \
                 title=\"he said &quot;hi&quot; &lt;loudly>\" workspace=\"2\">\n"
            ),
            "{text}"
        );
    }

    /// The header line round-trips: bare, with an argument, and with an
    /// argument that carries a quote and a space.
    #[test]
    fn the_header_line_names_the_panel_and_reads_back() {
        for id in [
            PanelId::bare(Tag("inbox")),
            PanelId::new(Tag("message"), ["42"]),
            PanelId::new(Tag("files"), ["~/a \"b\" c"]),
            PanelId::new(Tag("attachment"), ["42", "3"]),
        ] {
            let line = header_line(&id);
            assert_eq!(parse_header(&line), Some(id.clone()), "{line}");
            // The line leads a whole markdown document, and only the first
            // line of it is read.
            let doc = format!("{line}\n\n# superapp panel context\n\npanel: …\n");
            assert_eq!(parse_header(&doc), Some(id));
        }
        assert_eq!(
            header_line(&PanelId::bare(Tag("inbox"))),
            "superapp-panel: inbox []"
        );
        assert_eq!(
            header_line(&PanelId::new(Tag("message"), ["42"])),
            "superapp-panel: message [\"42\"]"
        );
    }

    /// Anything else is text, which is what makes the paste rule safe.
    #[test]
    fn anything_that_is_not_that_line_is_not_a_panel() {
        for text in [
            "",
            "hello",
            "superapp-panel",
            "superapp-panel: inbox",
            "superapp-panel: inbox not-json",
            "superapp-panel:  []",
            "a line first\nsuperapp-panel: inbox []",
        ] {
            assert_eq!(parse_header(text), None, "{text:?}");
        }
    }

    /// What has lately left the process about this panel's arguments, newest
    /// first — and nothing at all for a panel that stands for no one thing.
    #[test]
    fn the_recent_effects_are_the_ones_about_the_panel() {
        let s = Session::fake(APPS);
        for mail in [42, 7, 42] {
            let _ = s.world().run(&Look { mail });
        }
        let _ = s.world().run(&Peek { mail: 42 });
        let mine = recent_effects(s.store(), &PanelId::new(NOTE, ["42"]), EFFECTS);
        assert_eq!(mine.len(), 2, "the two that wrote about mail:42");
        assert!(
            mine.iter().all(|j| j.writes),
            "a read is not what happened to it"
        );
        assert_eq!(mine[0].entity.as_deref(), Some("mail:42"));
        assert!(
            mine.iter().all(|j| j.transient()),
            "the ring's, not the queue's"
        );
        assert_eq!(
            recent_effects(s.store(), &PanelId::new(NOTE, ["7"]), EFFECTS).len(),
            1
        );
        assert!(
            recent_effects(s.store(), &PanelId::bare(NOTE), EFFECTS).is_empty(),
            "a panel with no arguments stands for no one thing"
        );
        assert!(
            recent_effects(s.store(), &PanelId::new(NOTE, ["1"]), EFFECTS).is_empty(),
            "and an argument nothing was filed about gets nothing"
        );
        // `n` is a limit, not a wish.
        assert_eq!(
            recent_effects(s.store(), &PanelId::new(NOTE, ["42"]), 1).len(),
            1
        );
    }

    /// The effects section: one line an effect, the sentence it described
    /// itself with, how it went, and when.
    #[test]
    fn the_effects_read_as_lines_under_their_own_heading() {
        let (s, slot) = session(1, "hello");
        let _ = s.world().run(&Look { mail: 42 });
        let cx = of(&s, slot).expect("a context");
        let jobs = recent_effects(s.store(), &PanelId::new(NOTE, ["42"]), EFFECTS);
        let text = render(s.store(), &cx, &jobs);
        assert!(text.contains("\n## recent effects\n"), "{text}");
        assert!(
            text.contains("look at mail 42 — done · in memory, sep 01 12:00"),
            "{text}"
        );
        assert!(text.ends_with("</panel>\n"), "{text}");
    }
}
