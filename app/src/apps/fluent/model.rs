//! What the panels read, and the few things they write.
//!
//! Every read is a registered query, so a panel follows a commit under it
//! and an agent's chip carries the SQL that drew it. Every write is one
//! undoable action with the intent that reverses it: an answer, a grade,
//! a finished lesson.

use std::rc::Rc;
use std::sync::Mutex;

use kernel::effect::World;
use kernel::filter::Op;
use kernel::history::Intent;
use kernel::richtable::{Dir, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, Values};
use kernel::session::{Action, Session};
use kernel::store::{Store, Val};
use kernel::time::{civil_from_days, fmt_date};
use rusqlite::{params, Connection, OptionalExtension};

use super::sm2::{self, DAY};

// ---------------------------------------------------------------------------
// The learner and the desk
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Learner {
    pub name: String,
    pub native: String,
    pub target: String,
    pub level: String,
    pub goal: String,
    pub daily_minutes: i64,
    pub streak: i64,
    pub last_active: Option<f64>,
}

impl Learner {
    /// Whether the streak still counts: fed today or yesterday.
    #[must_use]
    pub fn streak_active(&self, now: f64) -> bool {
        self.last_active
            .is_some_and(|t| sm2::days_between(t, now) <= 1)
    }
}

pub fn learner(store: &Store) -> Option<Learner> {
    store
        .rows_sql(
            "fluent learner",
            "the learner: name, languages, level, streak",
            "SELECT name, native, target, level, goal, daily_minutes, streak, last_active FROM fluent_learner WHERE id = 1",
            &[],
            |r| {
                Ok(Learner {
                    name: r.get(0)?,
                    native: r.get(1)?,
                    target: r.get(2)?,
                    level: r.get(3)?,
                    goal: r.get(4)?,
                    daily_minutes: r.get(5)?,
                    streak: r.get(6)?,
                    last_active: r.get(7)?,
                })
            },
        )
        .first()
        .cloned()
}

/// The setup form's save: the one learner row, written whole.
///
/// The streak and the day it was last fed are the lessons' to keep, so
/// they are carried across rather than taken from the form; `started` is
/// stamped the first time and left alone after. One undoable action, and
/// undoing the first save leaves the course with no learner again, which
/// is where it began.
pub fn save_learner(s: &mut Session, l: &Learner) -> bool {
    struct Saved {
        before: Option<Learner>,
        started_before: Option<f64>,
        after: Learner,
        started: f64,
    }
    fn put(c: &Connection, l: &Learner, started: f64) -> rusqlite::Result<()> {
        c.execute(
            "INSERT INTO fluent_learner(id, name, native, target, level, goal, daily_minutes, streak, last_active, started)
             VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, native = excluded.native,
                 target = excluded.target, level = excluded.level, goal = excluded.goal,
                 daily_minutes = excluded.daily_minutes, started = excluded.started",
            params![l.name, l.native, l.target, l.level, l.goal, l.daily_minutes, l.streak, l.last_active, started],
        )?;
        Ok(())
    }
    impl Intent for Saved {
        fn describe(&self) -> String {
            format!("the course set up for {}", self.after.name)
        }
        fn reverse(&self, w: &World) -> Result<(), String> {
            let (before, started) = (self.before.clone(), self.started_before);
            w.store()
                .write(move |c| {
                    match &before {
                        None => {
                            c.execute("DELETE FROM fluent_learner WHERE id = 1", [])?;
                        }
                        Some(l) => put(c, l, started.unwrap_or(0.0))?,
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())
        }
        fn reapply(&self, w: &World) -> Result<(), String> {
            let (after, started) = (self.after.clone(), self.started);
            w.store()
                .write(move |c| put(c, &after, started))
                .map_err(|e| e.to_string())
        }
    }
    let now = s.now();
    let l = l.clone();
    let label = format!("set the course up for {}", l.name);
    let Some(saved) = s.act(Action::writing("fluent.setup", label, move |c| {
        let before: Option<(Learner, Option<f64>)> = c
            .query_row(
                "SELECT name, native, target, level, goal, daily_minutes, streak, last_active, started
                   FROM fluent_learner WHERE id = 1",
                [],
                |r| {
                    Ok((
                        Learner {
                            name: r.get(0)?,
                            native: r.get(1)?,
                            target: r.get(2)?,
                            level: r.get(3)?,
                            goal: r.get(4)?,
                            daily_minutes: r.get(5)?,
                            streak: r.get(6)?,
                            last_active: r.get(7)?,
                        },
                        r.get::<_, Option<f64>>(8)?,
                    ))
                },
            )
            .optional()?;
        // The streak belongs to the lessons, not to this form.
        let after = Learner {
            streak: before.as_ref().map_or(0, |(b, _)| b.streak),
            last_active: before.as_ref().and_then(|(b, _)| b.last_active),
            ..l.clone()
        };
        let started = before
            .as_ref()
            .and_then(|(_, s)| *s)
            .filter(|s| *s > 0.0)
            .unwrap_or(now);
        put(c, &after, started)?;
        Ok(Saved {
            before: before.as_ref().map(|(b, _)| b.clone()),
            started_before: before.as_ref().and_then(|(_, s)| *s),
            after,
            started,
        })
    })) else {
        return false;
    };
    s.claim(Box::new(saved));
    true
}

/// What is on the shelf: the newest lesson that is not done.
#[derive(Clone, Debug, PartialEq)]
pub struct Shelf {
    pub id: i64,
    pub title: String,
    pub for_date: f64,
    pub focus: Vec<String>,
    pub status: String,
    pub exercises: i64,
    pub reviews: i64,
    pub started: bool,
    /// The tutor's chat about this lesson, where one was started: a local
    /// note the desk reads to put a link on its bar.
    pub chat: Option<i64>,
}

pub fn shelf(store: &Store) -> Option<Shelf> {
    store
        .rows_sql(
            "fluent shelf",
            "the lesson on the shelf: the newest that is building or ready",
            "SELECT l.id, l.title, l.for_date, l.focus, l.status,
                    (SELECT COUNT(*) FROM fluent_exercise e WHERE e.lesson = l.id),
                    (SELECT COUNT(*) FROM fluent_exercise e WHERE e.lesson = l.id AND e.section = 'review'),
                    l.started IS NOT NULL, l.chat
               FROM fluent_lesson l WHERE l.status IN ('ready', 'building')
              ORDER BY l.for_date DESC, l.id DESC LIMIT 1",
            &[],
            |r| {
                Ok(Shelf {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    for_date: r.get(2)?,
                    focus: json_strings(&r.get::<_, String>(3)?),
                    status: r.get(4)?,
                    exercises: r.get(5)?,
                    reviews: r.get(6)?,
                    started: r.get(7)?,
                    chat: r.get(8)?,
                })
            },
        )
        .first()
        .cloned()
}

/// How many items are due by `now`: everything, and the cards alone.
pub fn due_counts(store: &Store, now: f64) -> (i64, i64) {
    let today = sm2::day_start(now);
    store
        .rows_sql(
            "fluent due",
            "how many items are due today, and how many of them are cards",
            "SELECT COUNT(*), SUM(kind = 'vocab') FROM fluent_item WHERE due <= ?1",
            &[Val::F(today)],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .first()
        .copied()
        .unwrap_or((0, 0))
}

/// The accuracy of the last `n` finished lessons, oldest first, with the
/// day each was for.
pub fn recent_accuracy(store: &Store, n: i64) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = store
        .rows_sql(
            "fluent accuracy",
            "the accuracy of the newest finished lessons",
            "SELECT for_date, accuracy FROM fluent_lesson WHERE status = 'done' AND accuracy IS NOT NULL
              ORDER BY for_date DESC, id DESC LIMIT ?1",
            &[Val::I(n)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .to_vec();
    v.reverse();
    v
}

/// Words learned — cards reviewed at least once — and how many of the
/// deck's cards arrived in the last seven days.
pub fn words(store: &Store, now: f64) -> (i64, i64) {
    let week_ago = sm2::day_start(now) - 7.0 * DAY;
    store
        .rows_sql(
            "fluent words",
            "cards learned, and the cards that arrived this week",
            "SELECT SUM(i.reps > 0), SUM(i.created >= ?1) FROM fluent_item i WHERE i.kind = 'vocab'",
            &[Val::F(week_ago)],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                ))
            },
        )
        .first()
        .copied()
        .unwrap_or((0, 0))
}

// ---------------------------------------------------------------------------
// Items and cards
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub ease: f64,
    pub interval: i64,
    pub reps: i64,
    pub due: f64,
    pub reviewed: Option<f64>,
    pub mastery: i64,
}

fn item_tx(c: &Connection, id: &str) -> rusqlite::Result<Option<Item>> {
    c.query_row(
        "SELECT id, kind, content, ease, interval, reps, due, reviewed, mastery FROM fluent_item WHERE id = ?1",
        [id],
        |r| {
            Ok(Item {
                id: r.get(0)?,
                kind: r.get(1)?,
                content: r.get(2)?,
                ease: r.get(3)?,
                interval: r.get(4)?,
                reps: r.get(5)?,
                due: r.get(6)?,
                reviewed: r.get(7)?,
                mastery: r.get(8)?,
            })
        },
    )
    .optional()
}

fn put_item_tx(c: &Connection, it: &Item) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE fluent_item SET ease = ?2, interval = ?3, reps = ?4, due = ?5, reviewed = ?6, mastery = ?7 WHERE id = ?1",
        params![it.id, it.ease, it.interval, it.reps, it.due, it.reviewed, it.mastery],
    )?;
    Ok(())
}

/// An item's `base` column: where its replay starts, as JSON. A migrated
/// item whose grades were never written down carries the schedule it came
/// with; anything made here carries the day it was first due, so a device
/// that has undone every grade of it knows where it began.
#[must_use]
pub fn base_json(r: &sm2::Replay) -> String {
    serde_json::json!({
        "ease": r.state.ease,
        "interval": r.state.interval,
        "reps": r.state.reps,
        "due": r.due,
        "reviewed": r.reviewed,
        "mastery": r.mastery,
    })
    .to_string()
}

/// The base of an item made today: nothing graded, due on `due`.
#[must_use]
pub fn fresh_base(due: f64) -> String {
    base_json(&sm2::Replay::fresh(due))
}

fn parse_base(text: &str) -> Option<sm2::Replay> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
    Some(sm2::Replay {
        state: sm2::State {
            ease: num("ease")?,
            interval: num("interval")? as i64,
            reps: num("reps")? as i64,
        },
        due: num("due")?,
        reviewed: num("reviewed"),
        mastery: num("mastery")? as i64,
    })
}

/// Replays one item's grades into its schedule, which is what the schedule
/// is: the grades are the record and these columns are this device's cache
/// of what they add up to. Every write that files, rewrites or removes a
/// grade ends with this.
///
/// The replay starts from the item's `base` — the schedule a migrated item
/// came with, or the day a new one was first due — so an item with no
/// grades left, because every one was undone here or on another device,
/// stands where it began rather than where the last grade left it. Grades
/// given in one instant on two devices are ordered by the device's name,
/// which every device agrees on, never by this device's row ids.
pub fn recompute_item_tx(c: &Connection, item: &str) -> rusqlite::Result<()> {
    let Some((base, created)) = c
        .query_row("SELECT base, created FROM fluent_item WHERE id = ?1", [item], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .optional()?
    else {
        return Ok(());
    };
    let mut stmt =
        c.prepare("SELECT at, quality FROM fluent_review WHERE item = ?1 ORDER BY at, device, id")?;
    let reviews: Vec<(f64, i64)> = stmt
        .query_map([item], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let from = parse_base(&base).unwrap_or_else(|| sm2::Replay::fresh(sm2::day_start(created)));
    let r = sm2::replay_from(from, &reviews);
    // Only a row that would change is written: the poll replays every item
    // whenever grades arrive, and an unchanged row should cost nobody a
    // redraw.
    c.execute(
        "UPDATE fluent_item SET ease = ?2, interval = ?3, reps = ?4, due = ?5, reviewed = ?6, mastery = ?7
          WHERE id = ?1 AND (ease IS NOT ?2 OR interval IS NOT ?3 OR reps IS NOT ?4 OR due IS NOT ?5
                             OR reviewed IS NOT ?6 OR mastery IS NOT ?7)",
        params![item, r.state.ease, r.state.interval, r.state.reps, r.due, r.reviewed, r.mastery],
    )?;
    Ok(())
}

/// Replays every item: what a poll does when device sync has brought grades
/// in — or taken them away — for cards this device has not touched, and
/// what the seed and the import do once their rows are down. One query for
/// the items and a replay each, and the same answer however often it runs.
pub fn recompute_all_tx(c: &Connection) -> rusqlite::Result<()> {
    let mut stmt = c.prepare("SELECT id FROM fluent_item")?;
    let items: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for item in &items {
        recompute_item_tx(c, item)?;
    }
    Ok(())
}

/// One card with its schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct CardRow {
    pub item: String,
    pub front: String,
    pub back: String,
    pub example: String,
    pub audio: String,
    pub notes: String,
    pub due: f64,
    pub reps: i64,
    pub ease: f64,
    pub mastery: i64,
    pub reviewed: Option<f64>,
}

const CARD_SELECT: &str =
    "c.item, c.front, c.back, c.example, c.audio, c.notes, i.due, i.reps, i.ease, i.mastery, i.reviewed";

fn card_of(r: &rusqlite::Row) -> rusqlite::Result<CardRow> {
    Ok(CardRow {
        item: r.get(0)?,
        front: r.get(1)?,
        back: r.get(2)?,
        example: r.get(3)?,
        audio: r.get(4)?,
        notes: r.get(5)?,
        due: r.get(6)?,
        reps: r.get(7)?,
        ease: r.get(8)?,
        mastery: r.get(9)?,
        reviewed: r.get(10)?,
    })
}

pub static CARDS: SqlSource<CardRow, String> = SqlSource {
    spec: &SqlSpec {
        id: "fluent cards",
        describe: "the deck: every card with its schedule, soonest due first",
        select: CARD_SELECT,
        from: "fluent_card c JOIN fluent_item i ON i.id = c.item",
        base: "1",
        text: &["c.front", "c.back", "c.example"],
        index: None,
        tags: &[
            ("new", TagSql::Where("i.reps = 0")),
            ("weak", TagSql::Where("(i.ease < 2.0 OR i.mastery <= 2) AND i.reps > 0")),
            ("learned", TagSql::Where("i.reps > 0")),
            ("mastery", TagSql::Col("i.mastery")),
        ],
        order: &[("i.due", Dir::Asc), ("c.front", Dir::Asc)],
        group: None,
        key: "c.item",
        deps: &[],
    },
    tags: &[
        TagDef { name: "new", kind: TagType::Bool, ops: &[], describe: "never reviewed", values: Values::None },
        TagDef { name: "weak", kind: TagType::Bool, ops: &[], describe: "a low ease or a low mastery", values: Values::None },
        TagDef { name: "learned", kind: TagType::Bool, ops: &[], describe: "reviewed at least once", values: Values::None },
        TagDef { name: "mastery", kind: TagType::Number, ops: &[Op::Eq, Op::Lt, Op::Lte, Op::Gt, Op::Gte], describe: "the 0–5 stamp", values: Values::None },
    ],
    map: card_of,
    key: |r| r.item.clone(),
    rank: |r| vec![Val::F(r.due), Val::S(r.front.clone())],
    suggest: |_, _, _| Vec::new(),
};

pub fn card(store: &Store, item: &str) -> Option<CardRow> {
    store
        .rows_sql(
            "fluent card",
            "one card, with its schedule",
            "SELECT c.item, c.front, c.back, c.example, c.audio, c.notes, i.due, i.reps, i.ease, i.mastery, i.reviewed
               FROM fluent_card c JOIN fluent_item i ON i.id = c.item WHERE c.item = ?1",
            &[Val::S(item.to_string())],
            card_of,
        )
        .first()
        .cloned()
}

/// The cards due by `now`, soonest first — the review's queue.
pub fn due_cards(store: &Store, now: f64) -> Rc<Vec<CardRow>> {
    store.rows_sql(
        "fluent due cards",
        "the cards due today, soonest first",
        "SELECT c.item, c.front, c.back, c.example, c.audio, c.notes, i.due, i.reps, i.ease, i.mastery, i.reviewed
           FROM fluent_card c JOIN fluent_item i ON i.id = c.item WHERE i.due <= ?1 ORDER BY i.due, c.front",
        &[Val::F(sm2::day_start(now))],
        card_of,
    )
}

/// The day the next card comes due after `now`, if any is scheduled.
pub fn next_due(store: &Store, now: f64) -> Option<f64> {
    store
        .rows_sql(
            "fluent next due",
            "when the next card comes due",
            "SELECT MIN(i.due) FROM fluent_item i WHERE i.kind = 'vocab' AND i.due > ?1",
            &[Val::F(sm2::day_start(now))],
            |r| r.get::<_, Option<f64>>(0),
        )
        .first()
        .copied()
        .flatten()
}

// ---------------------------------------------------------------------------
// Looking a word up
// ---------------------------------------------------------------------------

/// What a term came to: the dictionary form, what it means, what part of
/// speech it is, and the one note worth carrying — a noun's plural, or how
/// the word is used. The same shape whoever answered it: the deck, the
/// cache, or the tutor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub term: String,
    pub translation: String,
    pub pos: String,
    pub note: String,
}

/// The articles a German noun wears, which a selection may carry and a
/// dictionary form always does.
const ARTICLES: [&str; 5] = ["der", "die", "das", "ein", "eine"];

/// The term with its article taken off, if it had one.
fn bare(term: &str) -> &str {
    let term = term.trim();
    let (head, rest) = match term.split_once(char::is_whitespace) {
        Some(split) => split,
        None => return term,
    };
    if ARTICLES.iter().any(|a| head.eq_ignore_ascii_case(a)) {
        rest.trim()
    } else {
        term
    }
}

/// Every form a term is looked for under: as it was selected, without its
/// article, and with each article in front of what is left. Always the same
/// count, so one query with one shape answers any of them.
///
/// This is the whole of *the same word*: a selection of `Gebühr` finds the
/// deck's `die Gebühr`, and one of `die Gebühr` finds a `Gebühr` — case
/// folded on both sides by the query, which is why the forms go out as they
/// were written.
#[must_use]
pub fn term_forms(term: &str) -> Vec<String> {
    let term = term.trim();
    let bare = bare(term);
    let mut forms = vec![term.to_string(), bare.to_string()];
    forms.extend(ARTICLES.iter().map(|a| format!("{a} {bare}")));
    forms
}

/// `x` folded the way both sides of a term comparison are: `casefold` for
/// what is not ASCII — the kernel's own function, because SQLite's `lower`
/// leaves an Ü alone — and `lower` for what is.
const FOLDED: &str = "lower(casefold";

/// `column` against the forms [`term_forms`] made, each side folded.
fn folded_forms(column: &str, forms: usize) -> String {
    let marks = (1..=forms)
        .map(|i| format!("{FOLDED}(?{i}))"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{FOLDED}({column})) IN ({marks})")
}

fn form_params(term: &str) -> Vec<Val> {
    term_forms(term).into_iter().map(Val::S).collect()
}

/// The deck's card for a term, whichever way either of them wears its
/// article. Instant, offline, and the first thing a lookup asks.
#[must_use]
pub fn card_for_term(store: &Store, term: &str) -> Option<CardRow> {
    let params = form_params(term);
    let sql = format!(
        "SELECT {CARD_SELECT} FROM fluent_card c JOIN fluent_item i ON i.id = c.item
          WHERE {} ORDER BY c.front",
        folded_forms("c.front", params.len())
    );
    let rows = store.rows_sql(
        "fluent card for a term",
        "the deck's card for a selected word, with or without its article",
        &sql,
        &params,
        card_of,
    );
    rows.first().cloned()
}

/// What the tutor said about this term once before.
#[must_use]
pub fn cached_lookup(store: &Store, term: &str) -> Option<Entry> {
    let params = form_params(term);
    let sql = format!(
        "SELECT term, translation, pos, note FROM fluent_lookup WHERE {} ORDER BY term",
        folded_forms("term", params.len())
    );
    let rows = store.rows_sql(
        "fluent cached lookup",
        "what the tutor said a word meant, kept so it is asked once",
        &sql,
        &params,
        |r| {
            Ok(Entry {
                term: r.get(0)?,
                translation: r.get(1)?,
                pos: r.get(2)?,
                note: r.get(3)?,
            })
        },
    );
    rows.first().cloned()
}

/// Keeps what the tutor answered.
///
/// A plain write and not an action: a cache is nobody's decision, there is
/// nothing in it to take back, and a store that lost the whole table would
/// only ask again.
pub fn cache_lookup(store: &Store, e: &Entry, now: f64) {
    let e = e.clone();
    let _ = store.write(move |c| {
        c.execute(
            "INSERT INTO fluent_lookup(term, translation, pos, note, at) VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(term) DO UPDATE SET translation = excluded.translation,
                 pos = excluded.pos, note = excluded.note, at = excluded.at",
            params![e.term, e.translation, e.pos, e.note, now],
        )?;
        Ok(())
    });
}

/// The id a word is filed under: what the course has always called them,
/// `vocab_die_gebuehr` for `die Gebühr`.
#[must_use]
pub fn item_slug(front: &str) -> String {
    let mut slug = String::from("vocab_");
    let mut gap = false;
    for c in loose(front).chars() {
        if c.is_alphanumeric() {
            if gap && slug.len() > "vocab_".len() {
                slug.push('_');
            }
            slug.push(c);
            gap = false;
        } else {
            gap = true;
        }
    }
    slug
}

/// A word from a lookup, put in the deck: the item it is scheduled by and
/// the card it is shown on, as one undoable action. Answers with the item's
/// id, or nothing where the deck had it already or the write was refused.
///
/// The card is what the lookup found — the dictionary form on the front,
/// the meaning on the back, the tutor's note under it — and the word itself
/// is what **play** reads out, because a dictionary answers with a word and
/// not with a sentence.
pub fn add_word(s: &mut Session, e: &Entry) -> Option<String> {
    struct Added {
        item: String,
        entry: Entry,
        now: f64,
        /// Whether the schedule already knew this word. An undo takes back
        /// what this write made and nothing else.
        had_item: bool,
    }
    fn put(c: &Connection, item: &str, e: &Entry, now: f64) -> rusqlite::Result<()> {
        let today = sm2::day_start(now);
        c.execute(
            "INSERT INTO fluent_item(id, kind, content, created, due, base) VALUES(?1, 'vocab', ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO NOTHING",
            params![item, e.term, now, today, fresh_base(today)],
        )?;
        c.execute(
            "INSERT INTO fluent_card(item, front, back, example, audio, notes)
             VALUES(?1, ?2, ?3, '', ?2, ?4)
             ON CONFLICT(item) DO UPDATE SET front = excluded.front, back = excluded.back,
                 audio = excluded.audio, notes = excluded.notes",
            params![item, e.term, e.translation, e.note],
        )?;
        Ok(())
    }
    impl Intent for Added {
        fn describe(&self) -> String {
            format!("„{}“ in the deck", self.entry.term)
        }
        fn reverse(&self, w: &World) -> Result<(), String> {
            let (item, had_item) = (self.item.clone(), self.had_item);
            w.store()
                .write(move |c| {
                    c.execute("DELETE FROM fluent_card WHERE item = ?1", [&item])?;
                    if !had_item {
                        c.execute("DELETE FROM fluent_item WHERE id = ?1", [&item])?;
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())
        }
        fn reapply(&self, w: &World) -> Result<(), String> {
            let (item, entry, now) = (self.item.clone(), self.entry.clone(), self.now);
            w.store()
                .write(move |c| put(c, &item, &entry, now))
                .map_err(|e| e.to_string())
        }
    }
    let now = s.now();
    let item = item_slug(&e.term);
    let entry = e.clone();
    let (write_item, write_entry) = (item.clone(), entry.clone());
    let label = format!("add „{}“ to the deck", e.term);
    let Some(Some(added)) = s.act(Action::writing("fluent.add", label, move |c| {
        let had_item: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM fluent_item WHERE id = ?1)",
            [&write_item],
            |r| r.get(0),
        )?;
        let had_card: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM fluent_card WHERE item = ?1)",
            [&write_item],
            |r| r.get(0),
        )?;
        if had_card {
            return Ok(None);
        }
        put(c, &write_item, &write_entry, now)?;
        Ok(Some(Added {
            item: write_item,
            entry: write_entry,
            now,
            had_item,
        }))
    })) else {
        return None;
    };
    s.claim(Box::new(added));
    Some(item)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewRow {
    pub at: f64,
    pub quality: i64,
    pub lesson: Option<i64>,
    pub device: String,
}

pub fn reviews_of(store: &Store, item: &str) -> Rc<Vec<ReviewRow>> {
    store.rows_sql(
        "fluent reviews",
        "every grade one item was ever given, newest first",
        "SELECT at, quality, lesson, device FROM fluent_review WHERE item = ?1 ORDER BY at DESC, id DESC",
        &[Val::S(item.to_string())],
        |r| Ok(ReviewRow { at: r.get(0)?, quality: r.get(1)?, lesson: r.get(2)?, device: r.get(3)? }),
    )
}

// ---------------------------------------------------------------------------
// Lessons and exercises
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct LessonRow {
    pub id: i64,
    /// What names this lesson on every one of the learner's devices; the
    /// `id` beside it is this device's own number for it.
    pub uid: String,
    pub title: String,
    pub for_date: f64,
    pub focus: Vec<String>,
    pub status: String,
    pub started: Option<f64>,
    pub ended: Option<f64>,
    pub accuracy: Option<f64>,
    pub minutes: Option<f64>,
    pub notes: String,
}

fn lesson_of(r: &rusqlite::Row) -> rusqlite::Result<LessonRow> {
    Ok(LessonRow {
        id: r.get(0)?,
        uid: r.get(1)?,
        title: r.get(2)?,
        for_date: r.get(3)?,
        focus: json_strings(&r.get::<_, String>(4)?),
        status: r.get(5)?,
        started: r.get(6)?,
        ended: r.get(7)?,
        accuracy: r.get(8)?,
        minutes: r.get(9)?,
        notes: r.get(10)?,
    })
}

const LESSON_SELECT: &str =
    "l.id, l.uid, l.title, l.for_date, l.focus, l.status, l.started, l.ended, l.accuracy, l.minutes, l.notes";

pub static LESSONS: SqlSource<LessonRow, i64> = SqlSource {
    spec: &SqlSpec {
        id: "fluent lessons",
        describe: "the lessons played, newest first",
        select: LESSON_SELECT,
        from: "fluent_lesson l",
        base: "l.status = 'done'",
        text: &["l.title", "l.focus", "l.notes"],
        index: None,
        tags: &[
            ("date", TagSql::Col("l.for_date")),
            ("accuracy", TagSql::Col("l.accuracy")),
        ],
        order: &[("l.for_date", Dir::Desc), ("l.id", Dir::Desc)],
        group: None,
        key: "l.id",
        deps: &[],
    },
    tags: &[
        TagDef { name: "date", kind: TagType::Date, ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte], describe: "the day the lesson was for", values: Values::None },
        TagDef { name: "accuracy", kind: TagType::Number, ops: &[Op::Lt, Op::Lte, Op::Gt, Op::Gte], describe: "a fraction, 0 to 1", values: Values::None },
    ],
    map: lesson_of,
    key: |r| r.id,
    rank: |r| vec![Val::F(r.for_date), Val::I(r.id)],
    suggest: |_, _, _| Vec::new(),
};

pub fn lesson(store: &Store, id: i64) -> Option<LessonRow> {
    store
        .rows_sql(
            "fluent lesson",
            "one lesson: its title, day, focus, status and outcome",
            "SELECT l.id, l.uid, l.title, l.for_date, l.focus, l.status, l.started, l.ended, l.accuracy, l.minutes, l.notes
               FROM fluent_lesson l WHERE l.id = ?1",
            &[Val::I(id)],
            lesson_of,
        )
        .first()
        .cloned()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Exercise {
    pub id: i64,
    pub lesson: i64,
    /// The uid of the lesson this belongs to: what names it, and the
    /// exercise's `seq` with it, on every one of the learner's devices.
    pub lesson_uid: String,
    pub seq: i64,
    pub section: String,
    pub kind: String,
    pub grading: String,
    pub prompt: String,
    pub passage: String,
    pub audio: String,
    pub choices: Vec<String>,
    pub accepted: Vec<String>,
    pub model: String,
    pub hints: Vec<String>,
    pub explanation: String,
    pub items: Vec<String>,
    pub difficulty: i64,
    pub answer: Option<String>,
    pub result: Option<String>,
    pub self_grade: Option<i64>,
    pub tutor_grade: Option<i64>,
    pub tutor_note: String,
    pub tutor_fix: String,
    pub hints_shown: i64,
    pub elapsed: f64,
    pub answered: Option<f64>,
}

impl Exercise {
    /// Whether the learner answers by typing (a field) or by choosing.
    #[must_use]
    pub fn typed(&self) -> bool {
        matches!(self.kind.as_str(), "cloze" | "translate" | "free_write")
    }

    #[must_use]
    pub fn self_check(&self) -> bool {
        self.grading == "self_check"
    }

    /// Whether the record says this one is finished with: a closed answer
    /// graded, or a self-check one self-graded.
    #[must_use]
    pub fn done(&self) -> bool {
        self.answer.is_some() && (self.result.is_some() || self.self_grade.is_some())
    }

    /// Whether it counts as right for the accuracy line.
    #[must_use]
    pub fn counts_correct(&self) -> bool {
        match self.result.as_deref() {
            Some("correct" | "almost") => true,
            Some(_) => false,
            None => self.tutor_grade.or(self.self_grade).is_some_and(|q| q >= 3),
        }
    }

    /// The answer to show beside a wrong one: the model, or the first
    /// accepted.
    #[must_use]
    pub fn key_answer(&self) -> String {
        if !self.model.is_empty() {
            self.model.clone()
        } else {
            self.accepted.first().cloned().unwrap_or_default()
        }
    }

    /// The caption over the exercise: `REVIEW · CLOZE`.
    #[must_use]
    pub fn caption(&self) -> String {
        format!("{} · {}", section_word(&self.section), kind_word(&self.kind)).to_uppercase()
    }
}

#[must_use]
pub fn section_word(s: &str) -> &str {
    match s {
        "warmup" => "warm-up",
        "set_piece" => "set piece",
        other => other,
    }
}

#[must_use]
pub fn kind_word(k: &str) -> &str {
    match k {
        "free_write" => "free write",
        "listen_mcq" => "listen",
        "read_mcq" => "read",
        "mcq" => "choose",
        other => other,
    }
}

const EXERCISE_SELECT: &str = "e.id, e.lesson, e.lesson_uid, e.seq, e.section, e.kind, e.grading, e.prompt, e.passage, e.audio, e.choices, e.accepted, e.model, e.hints, e.explanation, e.items, e.difficulty, e.answer, e.result, e.self_grade, e.tutor_grade, e.tutor_note, e.tutor_fix, e.hints_shown, e.elapsed, e.answered";

fn exercise_of(r: &rusqlite::Row) -> rusqlite::Result<Exercise> {
    Ok(Exercise {
        id: r.get(0)?,
        lesson: r.get(1)?,
        lesson_uid: r.get(2)?,
        seq: r.get(3)?,
        section: r.get(4)?,
        kind: r.get(5)?,
        grading: r.get(6)?,
        prompt: r.get(7)?,
        passage: r.get(8)?,
        audio: r.get(9)?,
        choices: json_strings(&r.get::<_, String>(10)?),
        accepted: json_strings(&r.get::<_, String>(11)?),
        model: r.get(12)?,
        hints: json_strings(&r.get::<_, String>(13)?),
        explanation: r.get(14)?,
        items: json_strings(&r.get::<_, String>(15)?),
        difficulty: r.get(16)?,
        answer: r.get(17)?,
        result: r.get(18)?,
        self_grade: r.get(19)?,
        tutor_grade: r.get(20)?,
        tutor_note: r.get(21)?,
        tutor_fix: r.get(22)?,
        hints_shown: r.get(23)?,
        elapsed: r.get(24)?,
        answered: r.get(25)?,
    })
}

pub fn exercises(store: &Store, lesson: i64) -> Rc<Vec<Exercise>> {
    store.rows_sql(
        "fluent exercises",
        "a lesson's exercises in order, with what was answered",
        "SELECT e.id, e.lesson, e.lesson_uid, e.seq, e.section, e.kind, e.grading, e.prompt, e.passage, e.audio, e.choices, e.accepted, e.model, e.hints, e.explanation, e.items, e.difficulty, e.answer, e.result, e.self_grade, e.tutor_grade, e.tutor_note, e.tutor_fix, e.hints_shown, e.elapsed, e.answered
           FROM fluent_exercise e WHERE e.lesson = ?1 ORDER BY e.seq",
        &[Val::I(lesson)],
        exercise_of,
    )
}

/// One exercise by its local id — what a reply that took its time reads,
/// to see whether the row it was about is still there and still ungraded.
#[must_use]
pub fn exercise(store: &Store, id: i64) -> Option<Exercise> {
    store
        .rows_sql(
            "fluent exercise",
            "one exercise with what was answered",
            "SELECT e.id, e.lesson, e.lesson_uid, e.seq, e.section, e.kind, e.grading, e.prompt, e.passage, e.audio, e.choices, e.accepted, e.model, e.hints, e.explanation, e.items, e.difficulty, e.answer, e.result, e.self_grade, e.tutor_grade, e.tutor_note, e.tutor_fix, e.hints_shown, e.elapsed, e.answered
               FROM fluent_exercise e WHERE e.id = ?1",
            &[Val::I(id)],
            exercise_of,
        )
        .first()
        .cloned()
}

fn exercise_tx(c: &Connection, id: i64) -> rusqlite::Result<Option<Exercise>> {
    c.query_row(
        &format!("SELECT {EXERCISE_SELECT} FROM fluent_exercise e WHERE e.id = ?1"),
        [id],
        exercise_of,
    )
    .optional()
}

// ---------------------------------------------------------------------------
// Grading
// ---------------------------------------------------------------------------

/// The most choices an exercise may offer: one per digit key, which is
/// what the player has rows for. `fluent.author` refuses a lesson over it
/// and the import leaves such an exercise out and says so.
pub const MAX_CHOICES: usize = 9;

/// What a closed exercise says about an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    Correct,
    Almost,
    Wrong,
}

impl Closed {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Closed::Correct => "correct",
            Closed::Almost => "almost",
            Closed::Wrong => "wrong",
        }
    }

    /// The verdict back out of the word a row keeps it as.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Closed> {
        match word {
            "correct" => Some(Closed::Correct),
            "almost" => Some(Closed::Almost),
            "wrong" => Some(Closed::Wrong),
            _ => None,
        }
    }

    /// The headline the feedback wears.
    #[must_use]
    pub fn headline(self) -> &'static str {
        match self {
            Closed::Correct => "Richtig!",
            Closed::Almost => "Fast!",
            Closed::Wrong => "Nicht ganz.",
        }
    }

    /// The SM-2 quality a closed grade stands for.
    #[must_use]
    pub fn quality(self) -> i64 {
        match self {
            Closed::Correct => 5,
            Closed::Almost => 4,
            Closed::Wrong => 2,
        }
    }
}

fn loose(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace('ß', "ss")
        .replace('ä', "ae")
        .replace('ö', "oe")
        .replace('ü', "ue")
        .trim_end_matches(['.', '!', '?'])
        .to_string()
}

/// A closed grade: exact after trimming is right; the same letters with a
/// case or umlaut slip is almost; anything else is wrong.
#[must_use]
pub fn grade_closed(answer: &str, accepted: &[String]) -> Closed {
    let a = answer.trim();
    if accepted.iter().any(|x| x.trim() == a) {
        return Closed::Correct;
    }
    let l = loose(a);
    if !l.is_empty() && accepted.iter().any(|x| loose(x) == l) {
        return Closed::Almost;
    }
    Closed::Wrong
}

/// A self-check answer that is the model answer, or one of the accepted
/// ones, needs no judgement.
#[must_use]
pub fn matches_model(answer: &str, model: &str, accepted: &[String]) -> bool {
    let l = loose(answer);
    !l.is_empty() && (loose(model) == l || accepted.iter().any(|x| loose(x) == l))
}

/// What a record writes onto an exercise.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    pub answer: Option<String>,
    pub result: Option<Closed>,
    pub self_grade: Option<i64>,
    pub hints_shown: i64,
    pub elapsed: f64,
    /// When `Some`, a review of this quality is filed for each of the
    /// exercise's items and their schedules move.
    pub quality: Option<i64>,
}

#[derive(Debug)]
struct Recorded {
    exercise: i64,
    before: Exercise,
    items_before: Vec<Item>,
    reviews: Mutex<Vec<i64>>,
    patch: Patch,
    at: f64,
    device: String,
}

/// Writes a patch onto an exercise and files its grades. `ids` are the row
/// ids the grades were filed under the first time, where this is a redo:
/// the rows come back under the same ids, so a tutor's verdict recorded
/// against them — undone and redone after this — still finds its rows.
/// The table never reuses an id, so a freed one is free.
fn apply_patch_tx(
    c: &Connection,
    ex: &Exercise,
    patch: &Patch,
    at: f64,
    device: &str,
    ids: &[i64],
) -> rusqlite::Result<(Vec<Item>, Vec<i64>)> {
    c.execute(
        "UPDATE fluent_exercise SET answer = COALESCE(?2, answer), result = COALESCE(?3, result),
                self_grade = COALESCE(?4, self_grade), hints_shown = MAX(hints_shown, ?5),
                elapsed = elapsed + ?6, answered = COALESCE(answered, ?7)
          WHERE id = ?1",
        params![
            ex.id,
            patch.answer,
            patch.result.map(Closed::word),
            patch.self_grade,
            patch.hints_shown,
            patch.elapsed,
            at
        ],
    )?;
    c.execute(
        "UPDATE fluent_lesson SET started = COALESCE(started, ?2) WHERE id = ?1",
        params![ex.lesson, at],
    )?;
    let mut before = Vec::new();
    let mut reviews = Vec::new();
    if let Some(q) = patch.quality {
        for (n, item) in ex.items.iter().enumerate() {
            let Some(it) = item_tx(c, item)? else { continue };
            before.push(it.clone());
            // A review's row key is the item, the instant and the device: two
            // grades of one item inside one second are stamped a millisecond
            // apart, so the key stays unique whatever the clock says. The
            // exercise it came from is named the way every device names it —
            // the lesson's uid and the seq inside that lesson.
            let stamp = free_stamp(c, item, at + n as f64 * 0.001, device)?;
            c.execute(
                "INSERT INTO fluent_review(id, item, at, quality, device, lesson_uid, seq) VALUES(?7, ?1, ?2, ?3, ?4, ?5, ?6)",
                params![item, stamp, q, device, ex.lesson_uid, ex.seq, ids.get(reviews.len())],
            )?;
            reviews.push(c.last_insert_rowid());
            recompute_item_tx(c, item)?;
        }
    }
    Ok((before, reviews))
}

/// The nearest instant at or after `at` no review of this item on this
/// device has been stamped with.
fn free_stamp(c: &Connection, item: &str, at: f64, device: &str) -> rusqlite::Result<f64> {
    let mut stamp = at;
    loop {
        let taken: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM fluent_review WHERE item = ?1 AND at = ?2 AND device = ?3)",
            params![item, stamp, device],
            |r| r.get(0),
        )?;
        if !taken {
            return Ok(stamp);
        }
        stamp += 0.001;
    }
}

fn restore_exercise_tx(c: &Connection, ex: &Exercise) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE fluent_exercise SET answer = ?2, result = ?3, self_grade = ?4, hints_shown = ?5, elapsed = ?6, answered = ?7 WHERE id = ?1",
        params![ex.id, ex.answer, ex.result, ex.self_grade, ex.hints_shown, ex.elapsed, ex.answered],
    )?;
    Ok(())
}

impl Intent for Recorded {
    fn describe(&self) -> String {
        format!("answer to exercise {}", self.exercise)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let before = self.before.clone();
        let items = self.items_before.clone();
        let reviews = self.reviews.lock().map_or_else(|e| e.into_inner().clone(), |r| r.clone());
        w.store()
            .write(move |c| {
                restore_exercise_tx(c, &before)?;
                for id in reviews {
                    c.execute("DELETE FROM fluent_review WHERE id = ?1", [id])?;
                }
                // The schedule each item stood at, and then the grades it
                // still has: the snapshot is what an item with no history
                // goes back to, the replay what one with a history says.
                for it in &items {
                    put_item_tx(c, it)?;
                    recompute_item_tx(c, &it.id)?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let before = self.before.clone();
        let patch = self.patch.clone();
        let (at, device) = (self.at, self.device.clone());
        let had = self.reviews.lock().map_or_else(|e| e.into_inner().clone(), |r| r.clone());
        let ids = w
            .store()
            .write(move |c| apply_patch_tx(c, &before, &patch, at, &device, &had).map(|(_, ids)| ids))
            .map_err(|e| e.to_string())?;
        if let Ok(mut r) = self.reviews.lock() {
            *r = ids;
        }
        Ok(())
    }
}

/// Records what happened on one exercise, as one undoable action.
pub fn record(s: &mut Session, ex: &Exercise, patch: Patch, label: impl Into<String>) -> bool {
    let at = s.now();
    let device = s.store().device();
    let id = ex.id;
    let write_patch = patch.clone();
    let write_device = device.clone();
    let Some(Some((before, items_before, reviews))) = s.act(Action::writing(
        "fluent.answer",
        label,
        move |c| {
            let Some(before) = exercise_tx(c, id)? else { return Ok(None) };
            let (items, reviews) = apply_patch_tx(c, &before, &write_patch, at, &write_device, &[])?;
            Ok(Some((before, items, reviews)))
        },
    )) else {
        return false;
    };
    s.claim(Box::new(Recorded {
        exercise: id,
        before,
        items_before,
        reviews: Mutex::new(reviews),
        patch,
        at,
        device,
    }));
    true
}

/// What the tutor wrote on an exercise: the grade, the note, the fix.
type Verdict = (Option<i64>, String, String);

/// A grade the answer filed, as it stood before the tutor read it:
/// `(row, item, quality)`.
type Filed = (i64, String, i64);

/// Writes the tutor's verdict onto an exercise and puts the grades that
/// answer filed at `quality` — the tutor's, or each one's own again when
/// the verdict is taken back — replaying every item they touch.
fn put_verdict_tx(
    c: &Connection,
    exercise: i64,
    verdict: &Verdict,
    filed: &[Filed],
    quality: Option<i64>,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE fluent_exercise SET tutor_grade = ?2, tutor_note = ?3, tutor_fix = ?4 WHERE id = ?1",
        params![exercise, verdict.0, verdict.1, verdict.2],
    )?;
    for (row, item, was) in filed {
        c.execute(
            "UPDATE fluent_review SET quality = ?2 WHERE id = ?1",
            params![row, quality.unwrap_or(*was)],
        )?;
        recompute_item_tx(c, item)?;
    }
    // A verdict on a lesson already closed changes what its row says it
    // scored: the accuracy the desk, the history and the chart read is
    // stamped again from the exercises as they stand now.
    let lesson: Option<i64> = c
        .query_row("SELECT lesson FROM fluent_exercise WHERE id = ?1", [exercise], |r| r.get(0))
        .optional()?;
    if let Some(lesson) = lesson {
        restamp_accuracy_tx(c, lesson)?;
    }
    Ok(())
}

/// Writes a finished lesson's accuracy again from its exercises. A lesson
/// still being played has no accuracy yet and keeps none.
fn restamp_accuracy_tx(c: &Connection, lesson: i64) -> rusqlite::Result<()> {
    let done: bool = c
        .query_row("SELECT status = 'done' FROM fluent_lesson WHERE id = ?1", [lesson], |r| r.get(0))
        .optional()?
        .unwrap_or(false);
    if !done {
        return Ok(());
    }
    let exs = exercises_tx(c, lesson)?;
    let (right, total, _) = outcome(&exs);
    let acc = if total == 0 { 0.0 } else { right as f64 / total as f64 };
    c.execute("UPDATE fluent_lesson SET accuracy = ?2 WHERE id = ?1", params![lesson, acc])?;
    Ok(())
}

fn exercises_tx(c: &Connection, lesson: i64) -> rusqlite::Result<Vec<Exercise>> {
    let mut stmt =
        c.prepare(&format!("SELECT {EXERCISE_SELECT} FROM fluent_exercise e WHERE e.lesson = ?1 ORDER BY e.seq"))?;
    let exs = stmt.query_map([lesson], exercise_of)?.collect::<rusqlite::Result<_>>();
    exs
}

/// The grades one exercise filed, by the name every device knows it by.
fn filed_by_tx(c: &Connection, lesson_uid: &str, seq: i64) -> rusqlite::Result<Vec<Filed>> {
    if lesson_uid.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt =
        c.prepare("SELECT id, item, quality FROM fluent_review WHERE lesson_uid = ?1 AND seq = ?2")?;
    let filed = stmt
        .query_map(params![lesson_uid, seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>();
    filed
}

/// The tutor's word on a self-check answer: a grade, a note, a corrected
/// text — and the last word on the schedule too. The learner graded
/// themselves when they answered and the items moved on it; the tutor's
/// quality replaces it in the grades that answer filed, and each item is
/// replayed from there. An answer nobody graded files nothing, so there is
/// nothing to rewrite. One undo puts the verdict and the grades back.
pub fn tutor_grade(s: &mut Session, exercise: i64, quality: i64, note: &str, fix: &str) -> bool {
    struct Graded {
        exercise: i64,
        before: Verdict,
        after: Verdict,
        filed: Vec<Filed>,
        quality: i64,
    }
    impl Intent for Graded {
        fn describe(&self) -> String {
            format!("tutor grade on exercise {}", self.exercise)
        }
        fn reverse(&self, w: &World) -> Result<(), String> {
            let (id, v, filed) = (self.exercise, self.before.clone(), self.filed.clone());
            w.store()
                .write(move |c| put_verdict_tx(c, id, &v, &filed, None))
                .map_err(|e| e.to_string())
        }
        fn reapply(&self, w: &World) -> Result<(), String> {
            let (id, v, filed, q) =
                (self.exercise, self.after.clone(), self.filed.clone(), self.quality);
            w.store()
                .write(move |c| put_verdict_tx(c, id, &v, &filed, Some(q)))
                .map_err(|e| e.to_string())
        }
    }
    let quality = quality.clamp(0, 5);
    let after: Verdict = (Some(quality), note.to_string(), fix.to_string());
    let write_after = after.clone();
    let Some(Some((before, filed))) = s.act(Action::writing(
        "fluent.tutor",
        format!("tutor grades exercise {exercise}: {quality}/5"),
        move |c| {
            let row = c
                .query_row(
                    "SELECT tutor_grade, tutor_note, tutor_fix, lesson_uid, seq FROM fluent_exercise WHERE id = ?1",
                    [exercise],
                    |r| {
                        Ok((
                            (r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((before, lesson_uid, seq)) = row else { return Ok(None) };
            let filed = filed_by_tx(c, &lesson_uid, seq)?;
            put_verdict_tx(c, exercise, &write_after, &filed, Some(quality))?;
            Ok(Some((before, filed)))
        },
    )) else {
        return false;
    };
    s.claim(Box::new(Graded { exercise, before, after, filed, quality }));
    true
}

/// A lesson's outcome, as its row will carry it: how many right of how
/// many, and the minutes — the time spent on the exercises themselves,
/// which each one records as it is answered, so a lesson closed overnight
/// and finished in the morning is not a night's study.
#[must_use]
pub fn outcome(exs: &[Exercise]) -> (usize, usize, f64) {
    let right = exs.iter().filter(|e| e.done() && e.counts_correct()).count();
    let seconds: f64 = exs.iter().filter(|e| e.answer.is_some()).map(|e| e.elapsed).sum();
    let minutes = if exs.iter().any(|e| e.answer.is_some()) { (seconds / 60.0).max(1.0).round() } else { 0.0 };
    (right, exs.len(), minutes)
}

/// The streak after a day's work: one longer when the last day was
/// yesterday, unchanged when it was today — a second lesson in a day does
/// not raise it, though it does begin one — and back to one after any
/// longer gap. What counts is the day the learner played, not the day the
/// lesson was written for: a lesson played late is still a day at the desk.
#[must_use]
pub fn streak_after(streak: i64, last_active: Option<f64>, now: f64) -> i64 {
    match last_active.map(|t| sm2::days_between(t, now)) {
        Some(0) => streak.max(1),
        Some(1) => streak + 1,
        _ => 1,
    }
}

/// The learner's streak and when it was last fed.
type Streak = (i64, Option<f64>);

fn put_streak_tx(c: &Connection, streak: Streak) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE fluent_learner SET streak = ?1, last_active = ?2 WHERE id = 1",
        params![streak.0, streak.1],
    )?;
    Ok(())
}

/// Closes a lesson: done, with its accuracy and minutes stamped, the
/// learner's streak fed, and a building row put on the shelf for tomorrow
/// — the tutor's to fill.
pub fn finish(s: &mut Session, lesson: i64) -> bool {
    struct Finished {
        lesson: i64,
        before: (String, Option<f64>, Option<f64>, Option<f64>),
        after: (String, Option<f64>, Option<f64>, Option<f64>),
        streak_before: Streak,
        streak_after: Streak,
        /// The placeholder this finish put on the shelf, by id and uid —
        /// none where the shelf already had a lesson. A redo puts the same
        /// row back, so the lesson authored over it, redone after this,
        /// takes that one off the shelf and not a stranger.
        building: Mutex<Option<Placeholder>>,
        tomorrow: f64,
    }
    impl Intent for Finished {
        fn describe(&self) -> String {
            format!("lesson {} finished", self.lesson)
        }
        fn reverse(&self, w: &World) -> Result<(), String> {
            let (id, v, streak) = (self.lesson, self.before.clone(), self.streak_before);
            let building = self.building.lock().map_or(None, |b| b.clone());
            w.store()
                .write(move |c| {
                    c.execute(
                        "UPDATE fluent_lesson SET status = ?2, ended = ?3, accuracy = ?4, minutes = ?5 WHERE id = ?1",
                        params![id, v.0, v.1, v.2, v.3],
                    )?;
                    put_streak_tx(c, streak)?;
                    if let Some((b, _)) = building {
                        c.execute("DELETE FROM fluent_lesson WHERE id = ?1 AND status = 'building'", [b])?;
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())
        }
        fn reapply(&self, w: &World) -> Result<(), String> {
            let (id, v, tomorrow) = (self.lesson, self.after.clone(), self.tomorrow);
            let streak = self.streak_after;
            let again = self.building.lock().map_or(None, |b| b.clone());
            let b = w
                .store()
                .write(move |c| {
                    c.execute(
                        "UPDATE fluent_lesson SET status = ?2, ended = ?3, accuracy = ?4, minutes = ?5 WHERE id = ?1",
                        params![id, v.0, v.1, v.2, v.3],
                    )?;
                    put_streak_tx(c, streak)?;
                    match again {
                        Some((id, uid)) => building_again_tx(c, tomorrow, id, &uid),
                        None => building_tx(c, tomorrow),
                    }
                })
                .map_err(|e| e.to_string())?;
            if let Ok(mut slot) = self.building.lock() {
                *slot = b;
            }
            Ok(())
        }
    }
    let now = s.now();
    let tomorrow = sm2::day_start(now) + DAY;
    let Some(Some((before, after, streaks, building))) = s.act(Action::writing(
        "fluent.finish",
        "finish the lesson",
        move |c| {
            let before = c
                .query_row(
                    "SELECT status, ended, accuracy, minutes FROM fluent_lesson WHERE id = ?1",
                    [lesson],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<f64>>(1)?,
                            r.get::<_, Option<f64>>(2)?,
                            r.get::<_, Option<f64>>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((status, ended, accuracy, minutes)) = before else { return Ok(None) };
            let exs = exercises_tx(c, lesson)?;
            let (right, total, mins) = outcome(&exs);
            let acc = if total == 0 { 0.0 } else { right as f64 / total as f64 };
            let after = ("done".to_string(), Some(now), Some(acc), Some(mins));
            c.execute(
                "UPDATE fluent_lesson SET status = 'done', ended = ?2, accuracy = ?3, minutes = ?4 WHERE id = ?1",
                params![lesson, now, acc, mins],
            )?;
            let streak_before: Streak = c
                .query_row("SELECT streak, last_active FROM fluent_learner WHERE id = 1", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
                .unwrap_or((0, None));
            let streak_after = (streak_after(streak_before.0, streak_before.1, now), Some(now));
            put_streak_tx(c, streak_after)?;
            let building = building_tx(c, tomorrow)?;
            Ok(Some((
                (status, ended, accuracy, minutes),
                after,
                (streak_before, streak_after),
                building,
            )))
        },
    )) else {
        return false;
    };
    s.claim(Box::new(Finished {
        lesson,
        before,
        after,
        streak_before: streaks.0,
        streak_after: streaks.1,
        building: Mutex::new(building),
        tomorrow,
    }));
    true
}

/// A placeholder on the shelf: its local id and its uid.
type Placeholder = (i64, String);

/// Whether the shelf holds a lesson already — ready or building — in
/// which case a finish places nothing.
fn shelf_taken_tx(c: &Connection) -> rusqlite::Result<bool> {
    let open: i64 = c.query_row(
        "SELECT COUNT(*) FROM fluent_lesson WHERE status IN ('ready', 'building')",
        [],
        |r| r.get(0),
    )?;
    Ok(open > 0)
}

/// The shelf's placeholder for the lesson the tutor has yet to author:
/// one, never two. `day` is the day it stands for — tomorrow after a
/// lesson is finished, today when the learner asks for one now.
fn building_tx(c: &Connection, day: f64) -> rusqlite::Result<Option<Placeholder>> {
    if shelf_taken_tx(c)? {
        return Ok(None);
    }
    let Some(id) = place_building_tx(c, day)? else { return Ok(None) };
    let uid: String = c.query_row("SELECT uid FROM fluent_lesson WHERE id = ?1", [id], |r| r.get(0))?;
    Ok(Some((id, uid)))
}

/// The placeholder a finish made, put back under the same id and uid: what
/// a redo of that finish does, so the rows that name it still do.
fn building_again_tx(c: &Connection, day: f64, id: i64, uid: &str) -> rusqlite::Result<Option<Placeholder>> {
    if shelf_taken_tx(c)? {
        return Ok(None);
    }
    c.execute(
        "INSERT INTO fluent_lesson(id, uid, title, for_date, status, generated) VALUES(?1, ?2, '', ?3, 'building', ?3)",
        params![id, uid, day],
    )?;
    Ok(Some((id, uid.to_string())))
}

/// The same row, made whatever else is on the shelf: what *build* and
/// *build fresh* put there so the desk can say the tutor is at work and
/// carry a link to its chat.
///
/// A placeholder already building is answered rather than doubled, and a
/// lesson that is merely stale is left where it stands — it is still
/// playable, and the tutor's own lesson takes the shelf from it by being
/// for a later day.
fn place_building_tx(c: &Connection, day: f64) -> rusqlite::Result<Option<i64>> {
    let standing: Option<i64> = c
        .query_row(
            "SELECT id FROM fluent_lesson WHERE status = 'building' ORDER BY for_date DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = standing {
        return Ok(Some(id));
    }
    c.execute(
        "INSERT INTO fluent_lesson(title, for_date, status, generated) VALUES('', ?1, 'building', ?1)",
        [day],
    )?;
    Ok(Some(c.last_insert_rowid()))
}

/// The placeholder for `day`, made if there is not one. Bookkeeping, not
/// an action: pressing *build* is a question put to the tutor, and what
/// `cmd+z` is for is the lesson the tutor answers with.
pub fn ensure_building(store: &Store, day: f64) -> Option<i64> {
    store.write(move |c| place_building_tx(c, day)).ok().flatten()
}

/// How many lessons the learner has played through. Nought is a course
/// with no history at all, which is what the tutor's first brief answers.
pub fn lessons_played(store: &Store) -> i64 {
    store
        .rows_sql(
            "fluent lessons played",
            "how many lessons have been finished",
            "SELECT COUNT(*) FROM fluent_lesson WHERE status = 'done'",
            &[],
            |r| r.get::<_, i64>(0),
        )
        .first()
        .copied()
        .unwrap_or(0)
}

/// The placeholder standing on the shelf, if one is — what a finish left
/// for the tutor to fill.
pub fn building_lesson(store: &Store) -> Option<i64> {
    store
        .rows_sql(
            "fluent building",
            "the placeholder the tutor has yet to fill",
            "SELECT id FROM fluent_lesson WHERE status = 'building' ORDER BY for_date DESC LIMIT 1",
            &[],
            |r| r.get::<_, i64>(0),
        )
        .first()
        .copied()
}

/// Which chat the tutor is building this lesson in — a local note, so the
/// desk and the summary can put a link on the bar. Bookkeeping like the
/// placeholder itself: a chat id is this device's own and undo has no
/// business with it.
pub fn set_lesson_chat(store: &Store, lesson: i64, chat: i64) {
    let _ = store.write(move |c| {
        c.execute(
            "UPDATE fluent_lesson SET chat = ?2 WHERE id = ?1",
            params![lesson, chat],
        )?;
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// Reviewing a card
// ---------------------------------------------------------------------------

struct Reviewed {
    item: String,
    before: Item,
    at: f64,
    quality: i64,
    device: String,
    review: Mutex<Option<i64>>,
}

/// Files one grade and replays the item from its grades — the card's next
/// day, and how far the learner has it.
fn file_review_tx(
    c: &Connection,
    item: &str,
    at: f64,
    quality: i64,
    device: &str,
) -> rusqlite::Result<i64> {
    let stamp = free_stamp(c, item, at, device)?;
    c.execute(
        "INSERT INTO fluent_review(item, at, quality, device) VALUES(?1, ?2, ?3, ?4)",
        params![item, stamp, quality, device],
    )?;
    let id = c.last_insert_rowid();
    recompute_item_tx(c, item)?;
    Ok(id)
}

impl Intent for Reviewed {
    fn describe(&self) -> String {
        format!("review of {}", self.item)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let before = self.before.clone();
        let id = self.review.lock().map_or(None, |r| *r);
        w.store()
            .write(move |c| {
                if let Some(id) = id {
                    c.execute("DELETE FROM fluent_review WHERE id = ?1", [id])?;
                }
                put_item_tx(c, &before)?;
                recompute_item_tx(c, &before.id)?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (at, q, device, item) =
            (self.at, self.quality, self.device.clone(), self.item.clone());
        let id = w
            .store()
            .write(move |c| file_review_tx(c, &item, at, q, &device))
            .map_err(|e| e.to_string())?;
        if let Ok(mut r) = self.review.lock() {
            *r = Some(id);
        }
        Ok(())
    }
}

/// One grade on one card: a review row, and the card's schedule moved.
pub fn review_card(s: &mut Session, item: &str, quality: i64, label: impl Into<String>) -> bool {
    let at = s.now();
    let device = s.store().device();
    let q = quality.clamp(0, 5);
    let (write_item, write_device) = (item.to_string(), device.clone());
    let Some(Some((before, id))) = s.act(Action::writing("fluent.review", label, move |c| {
        let Some(before) = item_tx(c, &write_item)? else { return Ok(None) };
        let id = file_review_tx(c, &write_item, at, q, &write_device)?;
        Ok(Some((before, id)))
    })) else {
        return false;
    };
    s.claim(Box::new(Reviewed {
        item: item.to_string(),
        before,
        at,
        quality: q,
        device,
        review: Mutex::new(Some(id)),
    }));
    true
}

// ---------------------------------------------------------------------------
// Grammar
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct TopicRow {
    pub id: String,
    pub title: String,
    pub category: String,
    /// Where the category sits in the list, kept by the schema's own
    /// trigger: the course's order (Fälle, Präpositionen, Adjektive, …),
    /// which is not the slug's.
    pub rank: i64,
    pub level: String,
    pub summary: String,
    pub mastery: Option<i64>,
    pub introduced: Option<i64>,
    pub practiced: Option<i64>,
    pub sections: String,
    pub related: Vec<String>,
}

/// The lessons a topic names are uids, which is how every device names
/// them; what the panel prints is this device's own number for each, read
/// off the lesson as the row goes by.
const TOPIC_SELECT: &str = "t.id, t.title, t.category, t.rank, t.level, t.summary, t.mastery, \
     (SELECT id FROM fluent_lesson WHERE uid = t.introduced), \
     (SELECT id FROM fluent_lesson WHERE uid = t.practiced), t.sections, t.related";

fn topic_of(r: &rusqlite::Row) -> rusqlite::Result<TopicRow> {
    Ok(TopicRow {
        id: r.get(0)?,
        title: r.get(1)?,
        category: r.get(2)?,
        rank: r.get(3)?,
        level: r.get(4)?,
        summary: r.get(5)?,
        mastery: r.get(6)?,
        introduced: r.get(7)?,
        practiced: r.get(8)?,
        sections: r.get(9)?,
        related: json_strings(&r.get::<_, String>(10)?),
    })
}

pub static TOPICS: SqlSource<TopicRow, String> = SqlSource {
    spec: &SqlSpec {
        id: "fluent topics",
        describe: "the grammar reference the tutor keeps, by category",
        select: TOPIC_SELECT,
        from: "fluent_topic t",
        base: "1",
        text: &["t.title", "t.summary"],
        index: None,
        tags: &[
            ("level", TagSql::Col("t.level")),
            ("category", TagSql::Col("t.category")),
            ("mastery", TagSql::Col("t.mastery")),
        ],
        order: &[("t.rank", Dir::Asc), ("t.level", Dir::Asc), ("t.title", Dir::Asc)],
        group: None,
        key: "t.id",
        deps: &[],
    },
    tags: &[
        TagDef { name: "level", kind: TagType::Text, ops: &[Op::Eq], describe: "CEFR level of the rule", values: Values::Dynamic },
        TagDef { name: "category", kind: TagType::Text, ops: &[Op::Eq], describe: "cases, verbs, prepositions, …", values: Values::Dynamic },
        TagDef { name: "mastery", kind: TagType::Number, ops: &[Op::Eq, Op::Lt, Op::Lte, Op::Gt, Op::Gte], describe: "the 0–5 stamp", values: Values::None },
    ],
    map: topic_of,
    key: |r| r.id.clone(),
    rank: |r| vec![Val::I(r.rank), Val::S(r.level.clone()), Val::S(r.title.clone())],
    suggest: suggest_topics,
};

fn suggest_topics(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    let sql = match tag {
        "level" => "SELECT DISTINCT level FROM fluent_topic ORDER BY level",
        "category" => "SELECT DISTINCT category FROM fluent_topic ORDER BY category",
        _ => return Vec::new(),
    };
    store
        .rows_sql("fluent topic tags", "the levels and categories the topics use", sql, &[], |r| {
            r.get::<_, String>(0)
        })
        .iter()
        .filter(|v| v.to_lowercase().starts_with(&typed.to_lowercase()))
        .map(|v| Suggestion::value(v.clone()))
        .collect()
}

pub fn topic(store: &Store, id: &str) -> Option<TopicRow> {
    store
        .rows_sql(
            "fluent topic",
            "one grammar topic, whole",
            &format!("SELECT {TOPIC_SELECT} FROM fluent_topic t WHERE t.id = ?1"),
            &[Val::S(id.to_string())],
            topic_of,
        )
        .first()
        .cloned()
}

/// The category's name in the panel, in the course's language.
#[must_use]
pub fn category_word(c: &str) -> &str {
    match c {
        "cases" => "Fälle",
        "verbs" => "Verben",
        "sentence_structure" => "Satzbau",
        "prepositions" => "Präpositionen",
        "adjectives" => "Adjektive",
        "pronouns" => "Pronomen",
        "nouns" => "Nomen",
        _ => "Sonstiges",
    }
}

/// One block of a topic.
#[derive(Clone, Debug, PartialEq)]
pub enum Section {
    Text(String),
    Tip(String),
    Table { caption: String, columns: Vec<String>, rows: Vec<Vec<String>> },
    Examples(Vec<(String, String)>),
}

/// The blocks a topic's `sections` JSON holds; anything unreadable is left
/// out rather than shown as a blank.
#[must_use]
pub fn sections(json: &str) -> Vec<Section> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let strs = |v: Option<&serde_json::Value>| -> Vec<String> {
        v.and_then(|v| v.as_array())
            .map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default()
    };
    items
        .iter()
        .filter_map(|it| {
            let text = |k: &str| it.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            match it.get("kind").and_then(|k| k.as_str())? {
                "text" => Some(Section::Text(text("body"))),
                "tip" => Some(Section::Tip(text("body"))),
                "table" => Some(Section::Table {
                    caption: text("caption"),
                    columns: strs(it.get("columns")),
                    rows: it
                        .get("rows")
                        .and_then(|r| r.as_array())
                        .map(|rows| rows.iter().map(|row| strs(Some(row))).collect())
                        .unwrap_or_default(),
                }),
                "examples" => Some(Section::Examples(
                    it.get("items")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|e| {
                                    (
                                        e.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                        e.get("note").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                )),
                _ => None,
            }
        })
        .collect()
}

/// A table laid out in the mono face: every column as wide as its widest
/// cell, two spaces between, the header row over a rule of dashes.
#[must_use]
pub fn table_text(columns: &[String], rows: &[Vec<String>]) -> String {
    let n = columns.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut widths = vec![0usize; n];
    for row in std::iter::once(columns).chain(rows.iter().map(Vec::as_slice)) {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |row: &[String]| -> String {
        (0..n)
            .map(|i| {
                let cell = row.get(i).map_or("", String::as_str);
                let pad = widths[i] - cell.chars().count();
                format!("{cell}{}", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![line(columns)];
    out.push(widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("  "));
    out.extend(rows.iter().map(|r| line(r)));
    out.join("\n")
}

#[derive(Clone, Debug, PartialEq)]
pub struct TopicNote {
    pub lesson: Option<i64>,
    pub note: String,
    pub at: f64,
}

pub fn topic_notes(store: &Store, id: &str) -> Rc<Vec<TopicNote>> {
    store.rows_sql(
        "fluent topic notes",
        "the learner's own stumbles on one topic, newest first",
        "SELECT lesson, note, at FROM fluent_topic_note WHERE topic = ?1 ORDER BY at DESC, id DESC",
        &[Val::S(id.to_string())],
        |r| Ok(TopicNote { lesson: r.get(0)?, note: r.get(1)?, at: r.get(2)? }),
    )
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Mistake {
    pub id: String,
    pub category: String,
    pub frequency: i64,
    pub last: Option<f64>,
    pub wrong: String,
    pub right: String,
    pub mastery: i64,
}

pub fn top_mistakes(store: &Store, n: i64) -> Rc<Vec<Mistake>> {
    store.rows_sql(
        "fluent mistakes",
        "the error patterns seen most often",
        "SELECT m.id, m.category, m.frequency, m.last, m.wrong, m.right, COALESCE(i.mastery, 0)
           FROM fluent_mistake m LEFT JOIN fluent_item i ON i.id = m.id
          ORDER BY m.frequency DESC, m.last DESC LIMIT ?1",
        &[Val::I(n)],
        |r| {
            Ok(Mistake {
                id: r.get(0)?,
                category: r.get(1)?,
                frequency: r.get(2)?,
                last: r.get(3)?,
                wrong: r.get(4)?,
                right: r.get(5)?,
                mastery: r.get(6)?,
            })
        },
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub mastery: i64,
    pub accuracy: f64,
    pub lessons: i64,
}

pub fn skills(store: &Store) -> Rc<Vec<Skill>> {
    store.rows_sql(
        "fluent skills",
        "mastery and accuracy per skill",
        "SELECT name, mastery, accuracy, lessons FROM fluent_skill ORDER BY mastery DESC, name",
        &[],
        |r| Ok(Skill { name: r.get(0)?, mastery: r.get(1)?, accuracy: r.get(2)?, lessons: r.get(3)? }),
    )
}

/// The last grade each item was given since `since`, by item: what a
/// review sitting reads to see which of its cards are done and how they
/// went, however the grades came and went.
pub fn graded_since(store: &Store, since: f64) -> std::collections::HashMap<String, i64> {
    let rows = store.rows_sql(
        "fluent sitting grades",
        "every grade given since an instant, oldest first",
        "SELECT item, quality FROM fluent_review WHERE at >= ?1 ORDER BY at, device, id",
        &[Val::F(since)],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    );
    rows.iter().cloned().collect()
}

/// Minutes studied on each of the last 56 days, oldest first: what the
/// activity strip is drawn from. A lesson counts on the day it was
/// finished, which is the day the streak was fed — a stale lesson played
/// anyway is today's work — and on the day it was for where the record
/// has no end, as a migrated one has not.
pub fn activity(store: &Store, now: f64) -> Vec<f64> {
    let today = sm2::day_start(now);
    let first = today - 55.0 * DAY;
    let rows = store.rows_sql(
        "fluent activity",
        "minutes per day over the last eight weeks",
        "SELECT COALESCE(ended, for_date), COALESCE(minutes, 0) FROM fluent_lesson
          WHERE status = 'done' AND COALESCE(ended, for_date) >= ?1",
        &[Val::F(first)],
        |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)),
    );
    let mut days = vec![0.0; 56];
    for (day, minutes) in rows.iter() {
        let i = ((sm2::day_start(*day) - first) / DAY).round() as i64;
        if (0..56).contains(&i) {
            days[i as usize] += minutes;
        }
    }
    days
}

// ---------------------------------------------------------------------------
// Words for dates and numbers
// ---------------------------------------------------------------------------

/// `today`, `tomorrow`, `in 3 days`, `2 days ago`, or the date.
#[must_use]
pub fn relative_day(day: f64, now: f64) -> String {
    match sm2::days_between(now, day) {
        0 => "today".into(),
        1 => "tomorrow".into(),
        -1 => "yesterday".into(),
        d if d > 1 && d < 14 => format!("in {d} days"),
        d if d < 0 && d > -14 => format!("{} days ago", -d),
        _ => fmt_day(day),
    }
}

/// `sep 01`, without the hour the list style carries.
#[must_use]
pub fn fmt_day(ts: f64) -> String {
    fmt_date(ts).split(' ').take(2).collect::<Vec<_>>().join(" ")
}

/// The weekday, `Montag` to `Sonntag` — the course's language for a card
/// that says when the next one comes.
#[must_use]
pub fn weekday(ts: f64) -> &'static str {
    const DAYS: [&str; 7] = ["Donnerstag", "Freitag", "Samstag", "Sonntag", "Montag", "Dienstag", "Mittwoch"];
    let days = (ts / DAY).floor() as i64;
    DAYS[days.rem_euclid(7) as usize]
}

/// `2026-09-14` — the day as the tools write it and `fluent.author` reads
/// it back, which is the one spelling a model is ever asked for.
#[must_use]
pub fn fmt_iso_day(ts: f64) -> String {
    let (y, m, d) = civil_from_days((ts / DAY).floor() as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `1 Sep` — the day with its month, for a shelf.
#[must_use]
pub fn fmt_day_month(ts: f64) -> String {
    let (_, m, d) = civil_from_days((ts / DAY).floor() as i64);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {}", MONTHS[(m - 1) as usize])
}

#[must_use]
pub fn json_strings(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

#[must_use]
pub fn percent(fraction: f64) -> String {
    format!("{}%", (fraction * 100.0).round() as i64)
}

/// The German greeting for the hour.
#[must_use]
pub fn greeting(now: f64) -> &'static str {
    let hour = ((now.rem_euclid(DAY)) / 3600.0) as u32;
    match hour {
        5..=11 => "Guten Morgen",
        12..=17 => "Guten Tag",
        _ => "Guten Abend",
    }
}

/// One item's schedule, as a test reads it.
#[cfg(test)]
pub fn item_state(store: &Store, id: &str) -> Item {
    store
        .rows_sql(
            "fluent item",
            "one item's schedule",
            "SELECT id, kind, content, ease, interval, reps, due, reviewed, mastery FROM fluent_item WHERE id = ?1",
            &[Val::S(id.to_string())],
            |r| {
                Ok(Item {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    content: r.get(2)?,
                    ease: r.get(3)?,
                    interval: r.get(4)?,
                    reps: r.get(5)?,
                    due: r.get(6)?,
                    reviewed: r.get(7)?,
                    mastery: r.get(8)?,
                })
            },
        )
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("no item {id}"))
}
