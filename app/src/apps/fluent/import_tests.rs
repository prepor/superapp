//! The migration, over a folder of synthetic notebooks planted on the fake
//! disk: they are shaped like the original's — both spellings of an item's
//! id, a grade history and a device's log, a set piece whose text is in its
//! anchor's prompt — and none of their content is the learner's.

use kernel::caps::{real_path, Disk};
use kernel::panel::Panel;
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::ts;

use super::super::import::{self, Course};
use super::super::model;
use super::super::panels::Import;
use super::super::sm2::{day_start, DAY};
use super::{count, open, session};

const ROOT: &str = "~/fluent-course";

/// The notebooks, as a folder of them. `logs/.cursors.json` is the
/// pipeline's own index of the device logs, and is what names them where a
/// disk cannot list a directory it was handed files for.
const FILES: [(&str, &str); 11] = [
    ("learner-profile.json", include_str!("fixtures/learner-profile.json")),
    ("progress-db.json", include_str!("fixtures/progress-db.json")),
    ("mastery-db.json", include_str!("fixtures/mastery-db.json")),
    ("spaced-repetition.json", include_str!("fixtures/spaced-repetition.json")),
    ("vocab-deck.json", include_str!("fixtures/vocab-deck.json")),
    ("grammar-kb.json", include_str!("fixtures/grammar-kb.json")),
    ("mistakes-db.json", include_str!("fixtures/mistakes-db.json")),
    ("session-log.json", include_str!("fixtures/session-log.json")),
    ("next-session.json", include_str!("fixtures/next-session.json")),
    ("logs/reviews-phone-ab12.jsonl", include_str!("fixtures/reviews-phone-ab12.jsonl")),
    ("logs/.cursors.json", include_str!("fixtures/cursors.json")),
];

fn plant(s: &Session, root: &str, files: &[(&str, &str)]) {
    for (name, body) in files {
        s.world()
            .with_cap::<dyn Disk, _>(|d| d.write_file(&real_path(&format!("{root}/data/{name}")), body.as_bytes()))
            .unwrap()
            .unwrap();
    }
}

/// The demo course out of the way, so what the import writes is all there
/// is to count. The learner's row stays: writing over one and putting it
/// back is what undo owes.
fn empty(s: &Session) {
    s.store()
        .write(|c| {
            c.execute_batch(
                "DELETE FROM fluent_review; DELETE FROM fluent_exercise; DELETE FROM fluent_lesson;
                 DELETE FROM fluent_topic_note; DELETE FROM fluent_topic; DELETE FROM fluent_card;
                 DELETE FROM fluent_item; DELETE FROM fluent_mistake; DELETE FROM fluent_skill;",
            )?;
            Ok(())
        })
        .unwrap();
}

fn course(s: &Session) -> Course {
    s.world()
        .with_cap::<dyn Disk, _>(|d| import::read(d, ROOT))
        .unwrap()
        .unwrap()
}

/// One item's schedule as the store holds it: ease, the day it is due,
/// the repetitions and the mastery stamp.
fn state(store: &Store, item: &str) -> (f64, f64, i64, i64) {
    store
        .conn()
        .query_row("SELECT ease, due, reps, mastery FROM fluent_item WHERE id = ?1", [item], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
}

fn one<T: rusqlite::types::FromSql>(store: &Store, sql: &str) -> T {
    store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

const SUMMARY: &str =
    "7 items · 3 cards · 10 grades · 4 lessons · 2 topics · 2 mistakes imported · 0 already in the course";

#[test]
fn reading_maps_every_notebook_the_folder_keeps() {
    let s = session();
    plant(&s, ROOT, &FILES);
    let c = course(&s);

    let learner = c.learner.as_ref().unwrap();
    assert_eq!((learner.name.as_str(), learner.native.as_str(), learner.level.as_str()), ("Andrey", "Russian", "A2"));
    assert_eq!((learner.daily_minutes, learner.streak), (25, 4));
    assert_eq!(learner.started, ts(2026, 7, 15, 0, 0));
    assert_eq!(learner.last_active, Some(ts(2026, 7, 21, 0, 0)));

    // Both files name a skill; the union of them is the list.
    assert_eq!(c.skills.len(), 4, "listening, reading, vocabulary, writing");
    let writing = c.skills.iter().find(|s| s.name == "writing").unwrap();
    assert_eq!((writing.mastery, writing.accuracy, writing.lessons), (3, 0.8, 2));
    let vocab = c.skills.iter().find(|s| s.name == "vocabulary").unwrap();
    assert_eq!((vocab.mastery, vocab.lessons), (2, 4), "only the mastery file counts this one");

    assert_eq!(c.items.len(), 7);
    let kind = |id: &str| c.items.iter().find(|i| i.id == id).unwrap().kind.clone();
    assert_eq!(kind("vocab_die_gebuehr"), "vocab", "item_type vocabulary");
    assert_eq!(kind("v2_word_order"), "grammar", "item_type grammar_rule");
    assert_eq!(kind("eszett_usage"), "error", "the newer spelling: type error_pattern");
    assert_eq!(kind("reading_comprehension"), "grammar", "a type nobody here knows, and no vocab slug");
    assert_eq!(kind("zahlen_10_20"), "vocab", "type vocabulary without the slug");
    let gebuehr = c.items.iter().find(|i| i.id == "vocab_die_gebuehr").unwrap();
    assert_eq!(gebuehr.created, ts(2026, 7, 20, 0, 0), "no created_date: the first grade's day");
    let antrag = c.items.iter().find(|i| i.id == "vocab_der_antrag").unwrap();
    assert_eq!(antrag.created, ts(2026, 6, 1, 0, 0));

    // Eight grades out of the histories and two out of the device's log:
    // the log's third line repeats its first and is not a second grade.
    assert_eq!(c.reviews.len(), 10);
    let mut theirs: Vec<f64> = c
        .reviews
        .iter()
        .filter(|r| r.item == "vocab_der_antrag")
        .map(|r| r.at)
        .collect();
    theirs.sort_by(f64::total_cmp);
    assert_eq!(theirs, vec![ts(2026, 7, 20, 12, 0), ts(2026, 7, 20, 12, 0) + 1.0], "two on one day stay two rows");
    let logged: Vec<&import::Review> = c.reviews.iter().filter(|r| r.device == "phone-ab12").collect();
    assert_eq!(logged.len(), 2);
    assert_eq!(logged[0].at, ts(2026, 7, 17, 9, 30), "the log's own instant, to the second");

    assert_eq!(c.cards.len(), 3);
    assert_eq!(c.cards.iter().find(|c| c.item == "vocab_die_gebuehr").unwrap().front, "die Gebühr");
    assert!(c.cards.iter().all(|c| !c.audio.is_empty()), "audio_text is what a card speaks");

    assert_eq!(c.topics.len(), 2);
    let v2 = c.topics.iter().find(|t| t.id == "v2-wortstellung").unwrap();
    assert_eq!(v2.category, "sentence_structure");
    assert!(v2.sections.contains("\"kind\":\"table\""), "the sections are kept as they were written");
    assert_eq!(v2.related, "[\"artikel-nom-akk\"]");
    assert_eq!(v2.introduced.as_deref(), Some("import-session-001"));
    assert_eq!(v2.practiced.as_deref(), Some("import-session-002"));
    assert_eq!(c.notes.len(), 2);
    assert_eq!(c.notes[0].uid, "import-artikel-nom-akk-0", "named by the rule and its place");

    assert_eq!(c.mistakes.len(), 2);
    let eszett = c.mistakes.iter().find(|m| m.id == "eszett_usage").unwrap();
    assert_eq!((eszett.wrong.as_str(), eszett.right.as_str()), ("heisse", "heiße"));
    assert_eq!(eszett.frequency, 3);
    assert_eq!(eszett.last, Some(ts(2026, 7, 19, 0, 0)));
    let slip = c.mistakes.iter().find(|m| m.id == "v2_word_order").unwrap();
    assert_eq!(slip.wrong, "Morgen ich gehe zum Amt", "the other spelling of an example");

    // Three sittings and the one authored for the next day.
    assert_eq!(c.lessons.len(), 4);
    let lesson = |uid: &str| c.lessons.iter().find(|l| l.uid == uid).unwrap().clone();
    let first = lesson("import-001");
    assert_eq!((first.title.as_str(), first.status.as_str()), ("Erste Stunde.", "done"));
    assert_eq!((first.for_date, first.minutes, first.accuracy), (ts(2026, 7, 16, 0, 0), Some(15.0), Some(0.9)));
    assert_eq!(lesson("import-session-002").title, "reading · writing", "no notes: the skills it practised");
    assert_eq!(lesson("import-session-003").focus, "[\"official_documents\"]", "no focus_areas: what it covered");
    let next = lesson("import-session-004");
    assert_eq!((next.status.as_str(), next.for_date), ("ready", ts(2026, 7, 22, 0, 0)));
    assert_eq!(next.generated, ts(2026, 7, 21, 18, 5), "an ISO instant, not a day");

    // The set piece: one text, two questions, and the anchor's own prompt
    // is the first of them.
    assert_eq!(c.exercises.len(), 3);
    let seq = |n: i64| c.exercises.iter().find(|e| e.seq == n).unwrap().clone();
    assert_eq!(seq(1).section, "warmup");
    assert!(seq(1).passage.is_empty());
    let anchor = seq(2);
    assert_eq!(anchor.section, "set_piece");
    assert_eq!(anchor.prompt, "Was möchte Frau Ivanova machen?");
    assert!(anchor.passage.starts_with("Im Bürgerbüro."));
    assert!(anchor.passage.ends_with("per Post."), "{:?}", anchor.passage);
    assert!(!anchor.passage.contains("Frage:"));
    let sub = seq(3);
    assert_eq!(sub.prompt, "Was muss Frau Ivanova bezahlen?");
    assert_eq!(sub.passage, anchor.passage, "the text is on every question about it");
    assert_eq!(sub.items, "[\"vocab_die_gebuehr\"]");
    assert_eq!(sub.kind, "read_mcq");
}

#[test]
fn a_folder_with_one_notebook_in_it_imports_that_one() {
    let mut s = session();
    empty(&s);
    plant(&s, "~/deck-only", &FILES[4..5]);
    let deck = s
        .world()
        .with_cap::<dyn Disk, _>(|d| import::read(d, "~/deck-only"))
        .unwrap()
        .unwrap();
    assert_eq!(deck.cards.len(), 3);
    assert!(deck.items.is_empty() && deck.learner.is_none());
    let done = import::import(&mut s, deck).unwrap();
    assert_eq!(done.cards.added, 3);
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_card"), 3);
}

#[test]
fn a_folder_with_no_notebooks_in_it_says_so() {
    let s = session();
    let no_such = s
        .world()
        .with_cap::<dyn Disk, _>(|d| import::read(d, "~/not-a-course"))
        .unwrap()
        .unwrap_err();
    assert_eq!(no_such, "no notebooks under ~/not-a-course/data");
    let empty_path = s.world().with_cap::<dyn Disk, _>(|d| import::read(d, "  ")).unwrap().unwrap_err();
    assert_eq!(empty_path, "enter the path to the course folder");
}

#[test]
fn an_import_writes_the_course_and_the_grades_replay_into_the_schedule() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &FILES);
    let read = course(&s);
    let done = import::import(&mut s, read).unwrap();
    assert_eq!(done.summary(), SUMMARY);
    let store = s.store();
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_item"), 7);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_card"), 3);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_review"), 10);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_lesson"), 4);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_exercise"), 3);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_topic"), 2);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_topic_note"), 2);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_mistake"), 2);
    assert_eq!(count(store, "SELECT COUNT(*) FROM fluent_skill"), 4);

    // The learner's row is the one thing an import writes over.
    assert_eq!(one::<i64>(store, "SELECT daily_minutes FROM fluent_learner"), 25);
    assert_eq!(one::<i64>(store, "SELECT streak FROM fluent_learner"), 4);

    // Every item whose history the file kept lands on the day the file
    // says it is due: the grades are the record, and replaying them here
    // is the same arithmetic the original did.
    for (item, ease, due, reps, mastery) in [
        ("vocab_die_gebuehr", 2.6, ts(2026, 7, 27, 0, 0), 2, 2),
        ("vocab_der_antrag", 2.46, ts(2026, 7, 26, 0, 0), 2, 2),
        ("v2_word_order", 2.18, ts(2026, 7, 19, 0, 0), 0, 0),
        ("eszett_usage", 2.6, ts(2026, 7, 20, 0, 0), 1, 1),
        ("reading_comprehension", 2.5, ts(2026, 7, 18, 0, 0), 1, 1),
    ] {
        assert_eq!(state(store, item), (ease, due, reps, mastery), "{item}");
    }
    // The one the device's log graded twice more stands past its file's
    // day, because two grades the file never folded moved it.
    let (_, due, reps, mastery) = state(store, "zahlen_10_20");
    assert_eq!((due, reps, mastery), (ts(2026, 8, 9, 0, 0), 3, 3));
    // The one with no history at all keeps what the file cached for it.
    assert_eq!(state(store, "vocab_der_briefkasten"), (2.5, ts(2026, 7, 22, 0, 0), 0, 0));
    let never: Option<f64> = one(store, "SELECT reviewed FROM fluent_item WHERE id = 'vocab_der_briefkasten'");
    assert_eq!(never, None, "and it was never graded");

    // A lesson is named across devices by its uid; the local ids the
    // triggers keep are what a topic and an exercise wear.
    let numbered: i64 = one(
        store,
        "SELECT COUNT(*) FROM fluent_exercise e JOIN fluent_lesson l ON l.uid = e.lesson_uid AND l.id = e.lesson",
    );
    assert_eq!(numbered, 3, "the trigger numbered every exercise");
    // A topic names its lessons by uid, as every device does; the panel
    // reads this device's number off the lesson.
    assert_eq!(
        one::<String>(store, "SELECT practiced FROM fluent_topic WHERE id = 'v2-wortstellung'"),
        "import-session-002"
    );
    let topic = model::topic(store, "v2-wortstellung").unwrap();
    assert_eq!(topic.practiced, Some(one::<i64>(store, "SELECT id FROM fluent_lesson WHERE uid = 'import-session-002'")));
    assert_eq!(topic.introduced, None, "the grammar names a sitting the log numbers differently");
    assert_eq!(
        one::<f64>(store, "SELECT at FROM fluent_topic_note WHERE topic = 'v2-wortstellung'"),
        ts(2026, 7, 19, 0, 0),
        "a note is dated by the sitting it came from"
    );
    // The grammar list's order comes off the category, by a trigger.
    assert_eq!(one::<i64>(store, "SELECT rank FROM fluent_topic WHERE id = 'artikel-nom-akk'"), 0);
}

#[test]
fn a_second_import_of_the_same_folder_adds_nothing() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &FILES);
    let read = course(&s);
    import::import(&mut s, read).unwrap();
    let read = course(&s);
    let again = import::import(&mut s, read).unwrap();
    assert_eq!(
        again.summary(),
        "0 items · 0 cards · 0 grades · 0 lessons · 0 topics · 0 mistakes imported · 37 already in the course"
    );
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_item"), 7);
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_review"), 10);
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_lesson"), 4);
}

#[test]
fn undo_takes_the_whole_import_back_and_redo_puts_it_down_again() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &FILES);
    let before = one::<i64>(s.store(), "SELECT daily_minutes FROM fluent_learner");
    let read = course(&s);
    import::import(&mut s, read).unwrap();
    assert_eq!(one::<i64>(s.store(), "SELECT daily_minutes FROM fluent_learner"), 25);

    assert!(s.undo());
    for table in ["fluent_item", "fluent_card", "fluent_review", "fluent_lesson", "fluent_exercise",
                  "fluent_topic", "fluent_topic_note", "fluent_mistake", "fluent_skill"] {
        assert_eq!(count(s.store(), &format!("SELECT COUNT(*) FROM {table}")), 0, "{table}");
    }
    assert_eq!(
        one::<i64>(s.store(), "SELECT daily_minutes FROM fluent_learner"),
        before,
        "the learner it wrote over is put back"
    );

    assert!(s.redo());
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_item"), 7);
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_review"), 10);
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_exercise"), 3);
    assert_eq!(one::<i64>(s.store(), "SELECT daily_minutes FROM fluent_learner"), 25);
    assert_eq!(state(s.store(), "vocab_die_gebuehr").1, ts(2026, 7, 27, 0, 0), "and the schedule with them");
}

/// A word the course already knew keeps its row and takes the grades; and
/// when the import goes back, so does where those grades put it.
#[test]
fn undo_puts_back_the_schedule_of_a_word_the_course_already_had() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &FILES);
    let stood = ts(2026, 9, 1, 0, 0);
    s.store()
        .write(move |c| {
            c.execute(
                "INSERT INTO fluent_item(id, kind, content, created, due, ease, interval, reps, mastery, base)
                 VALUES('zahlen_10_20', 'vocab', 'Zahlen 10–20', ?1, ?1, 2.5, 1, 0, 0, ?2)",
                rusqlite::params![stood, model::fresh_base(stood)],
            )?;
            Ok(())
        })
        .unwrap();
    let read = course(&s);
    let done = import::import(&mut s, read).unwrap();
    assert_eq!((done.items.added, done.items.skipped), (6, 1), "the word was already here");
    assert_eq!(state(s.store(), "zahlen_10_20").1, ts(2026, 8, 9, 0, 0), "and three grades moved it");

    assert!(s.undo());
    assert_eq!(state(s.store(), "zahlen_10_20"), (2.5, stood, 0, 0), "the row stays, where it stood");
}

#[test]
fn the_form_reads_through_the_disk_capability_and_says_what_it_wrote() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &FILES);
    let slot = open(&mut s, Import::id());
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    let p = borrow.as_any().downcast_mut::<Import>().unwrap();
    // It opens on the folder the original keeps, which is not there.
    assert!(p.path.starts_with("~/Library/Mobile Documents/"));
    p.run("fluent.import", &mut s);
    assert!(p.error.starts_with("no notebooks under"), "{}", p.error);
    assert_eq!(p.status, "");

    p.path = ROOT.into();
    p.run("fluent.import", &mut s);
    assert_eq!(p.error, "");
    assert_eq!(p.status, SUMMARY);
}

#[test]
fn an_iso_instant_is_read_to_the_second_and_a_day_to_its_midnight() {
    assert_eq!(import::parse_ts("2026-08-05T15:10:58Z"), Some(ts(2026, 8, 5, 15, 10) + 58.0));
    assert_eq!(import::parse_ts("2026-08-05T15:10:58.421Z"), Some(ts(2026, 8, 5, 15, 10) + 58.0));
    assert_eq!(import::parse_ts("2026-08-05T17:10:58+02:00"), Some(ts(2026, 8, 5, 15, 10) + 58.0));
    assert_eq!(import::parse_ts("2026-08-05T13:10:58-02:00"), Some(ts(2026, 8, 5, 15, 10) + 58.0));
    assert_eq!(import::parse_ts("2026-08-05 15:10:58"), Some(ts(2026, 8, 5, 15, 10) + 58.0));
    assert_eq!(import::parse_ts("2026-08-05"), None);
    assert_eq!(import::parse_day("2026-08-05"), Some(ts(2026, 8, 5, 0, 0)));
    assert_eq!(import::parse_day("2026-13-05"), None);
    assert_eq!(import::parse_day("nope"), None);
}

/// The bar of the one panel that was added with this import, checked the
/// way every other bar in the app is.
#[test]
fn the_import_bar_wears_one_unreserved_letter() {
    let mut s = session();
    let slot = open(&mut s, Import::id());
    let verbs = s.panel_verbs(slot);
    let letters: Vec<char> = verbs.iter().filter_map(|v| v.accel).collect();
    assert_eq!(letters, vec!['m'], "i is the workspace's own");
    for v in &verbs {
        assert!(v.label.contains(v.accel.unwrap()));
        assert!(!crate::shell::keys::is_reserved(v.accel.unwrap()));
    }
}

/// A folder with a deck and no schedule: each card's word is put on the
/// schedule as a fresh item, due today, so the deck shows it.
#[test]
fn a_deck_alone_puts_its_words_on_the_schedule() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &[("vocab-deck.json", include_str!("fixtures/vocab-deck.json"))]);
    let read = course(&s);
    assert!(read.items.is_empty() && read.cards.len() == 3);
    let done = import::import(&mut s, read).unwrap();
    assert_eq!((done.items.added, done.cards.added), (3, 3));
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_card c JOIN fluent_item i ON i.id = c.item"), 3);
    let today = day_start(s.now());
    assert_eq!(state(s.store(), "vocab_der_briefkasten"), (2.5, today, 0, 0));
    assert!(s.undo());
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_item"), 0);
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_card"), 0);
}

/// A migrated item whose notebook cached a schedule without its history
/// carries on from there: the first grade given here is its next
/// repetition, not its first — and that grade undone puts it back there.
#[test]
fn a_migrated_schedule_without_history_carries_on_from_where_it_stood() {
    let mut s = session();
    empty(&s);
    let mut schedule: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/spaced-repetition.json")).unwrap();
    schedule["items"]["vocab_der_briefkasten"] = serde_json::json!({
        "item_type": "vocabulary", "content": "der Briefkasten", "easiness_factor": 2.5,
        "interval_days": 15, "repetitions": 3, "due_date": "2026-07-22", "last_reviewed": "2026-07-07",
        "mastery_level": 3
    });
    let owned = schedule.to_string();
    plant(&s, ROOT, &[("spaced-repetition.json", owned.as_str())]);
    let read = course(&s);
    import::import(&mut s, read).unwrap();
    let store = s.store().clone();
    assert_eq!(state(&store, "vocab_der_briefkasten"), (2.5, ts(2026, 7, 22, 0, 0), 3, 3), "as the file had it");
    // A grade from the deck: the fourth repetition, an interval of 15 × 2.5.
    assert!(model::review_card(&mut s, "vocab_der_briefkasten", 4, "good"));
    let after = model::item_state(&store, "vocab_der_briefkasten");
    assert_eq!((after.reps, after.interval, after.mastery), (4, 38, 4));
    assert_eq!(after.due, day_start(s.now()) + 38.0 * DAY);
    assert!(s.undo());
    assert_eq!(state(&store, "vocab_der_briefkasten"), (2.5, ts(2026, 7, 22, 0, 0), 3, 3), "back where it stood");
}

/// An import and an answer against one of its exercises, both undone and
/// both redone: the exercise comes back under the id it had, so the
/// answer finds its row.
#[test]
fn redoing_an_import_and_an_answer_keeps_the_answer() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &FILES);
    let read = course(&s);
    import::import(&mut s, read).unwrap();
    let store = s.store().clone();
    let first: i64 = one(&store, "SELECT id FROM fluent_exercise ORDER BY lesson_uid, seq LIMIT 1");
    let ex = model::exercise(&store, first).unwrap();
    let patch = model::Patch {
        answer: Some("richtig".into()),
        result: Some(model::Closed::Correct),
        quality: Some(5),
        ..model::Patch::default()
    };
    assert!(model::record(&mut s, &ex, patch, "answer"));
    assert!(s.undo(), "the answer");
    assert!(s.undo(), "the import");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_exercise"), 0);
    assert!(s.redo(), "the import");
    assert!(s.redo(), "the answer");
    let again = model::exercise(&store, first).expect("the same row");
    assert_eq!((again.lesson_uid.as_str(), again.seq), (ex.lesson_uid.as_str(), ex.seq));
    assert_eq!(again.answer.as_deref(), Some("richtig"));
    assert_eq!(again.lesson, one::<i64>(&store, &format!("SELECT id FROM fluent_lesson WHERE uid = '{}'", ex.lesson_uid)));
}

/// A question with more choices than the player has digit keys for is
/// left out of the import, and the summary says which.
#[test]
fn a_question_with_too_many_choices_is_left_out_and_said_so() {
    let mut s = session();
    empty(&s);
    plant(&s, ROOT, &FILES);
    let mut next: serde_json::Value = serde_json::from_str(include_str!("fixtures/next-session.json")).unwrap();
    let many: Vec<String> = (1..=11).map(|n| format!("Antwort {n}")).collect();
    next["exercises"]["s4-e1"]["type"] = serde_json::json!("mcq");
    next["exercises"]["s4-e1"]["choices"] = serde_json::json!(many);
    let owned = next.to_string();
    plant(&s, ROOT, &[("next-session.json", owned.as_str())]);
    let read = course(&s);
    assert_eq!(read.refused.len(), 1, "{:?}", read.refused);
    assert!(read.refused[0].ends_with("Q1: 11 choices, the player shows 9"), "{}", read.refused[0]);
    let done = import::import(&mut s, read).unwrap();
    assert!(done.summary().ends_with(&format!(" · left out: {}", done.refused[0])), "{}", done.summary());
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_exercise"), 2, "the other two are in");
}
