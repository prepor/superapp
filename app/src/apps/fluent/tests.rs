use super::model::{self, Closed, Patch};
use super::panels::{
    Card, Cards, Desk, Found, Grammar, History, Lesson, Lookup, Phase, Progress, Review, Setup, Topic,
};
use super::prompt::{self, Task};
use super::seed::{uid, FIRST_DUE, LAST_DONE, READY};
use super::sm2::{self, day_start, Replay, DAY};
use super::{tools, FLUENT};
use crate::apps::agent::{model as agent, Chat, FakeGateway, AGENT};
use kernel::app::App;
use kernel::layout::SlotId;
use kernel::panel::{Panel, PanelId};
use kernel::richtable::Datasource;
use kernel::session::{Action, Session};
use kernel::store::Store;

static APPS: &[&dyn App] = &[&FLUENT];

/// The migration: its own file, over its own folder of fixtures.
#[path = "import_tests.rs"]
mod import;

/// The course with a tutor behind it: what every test of the tutor runs
/// in, since the agent app is what `fluent` asks for and works without.
static WITH_TUTOR: &[&dyn App] = &[&FLUENT, &AGENT];

fn session() -> Session {
    Session::fake(APPS)
}

fn tutored() -> Session {
    Session::fake(WITH_TUTOR)
}

/// What the session has said and not yet drawn, as one string.
fn said(s: &Session) -> String {
    s.notes().iter().map(|n| n.msg.clone()).collect::<Vec<_>>().join(" · ")
}

fn setup<R>(s: &mut Session, slot: SlotId, f: impl FnOnce(&mut Setup, &mut Session) -> R) -> R {
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    f(borrow.as_any().downcast_mut::<Setup>().unwrap(), s)
}

fn open(s: &mut Session, id: PanelId) -> SlotId {
    s.act(Action::new("open", "open").moving(move |wm| {
        wm.open(id, None, false);
    }));
    s.settle();
    s.focus().unwrap()
}

fn lesson<R>(s: &mut Session, slot: SlotId, f: impl FnOnce(&mut Lesson, &mut Session) -> R) -> R {
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    f(borrow.as_any().downcast_mut::<Lesson>().unwrap(), s)
}

fn lookup<R>(s: &mut Session, slot: SlotId, f: impl FnOnce(&mut Lookup, &mut Session) -> R) -> R {
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    f(borrow.as_any().downcast_mut::<Lookup>().unwrap(), s)
}

/// This world's scripted gateway — what a test reads to see what the model
/// was told, and whether it was asked at all.
fn gateway(s: &Session) -> FakeGateway {
    s.world()
        .caps(|c| c.get::<FakeGateway>().cloned())
        .expect("a tutored world keeps its fake gateway under its own type")
}

fn verbs(s: &Session, slot: SlotId) -> Vec<&'static str> {
    s.panel_verbs(slot).iter().map(|v| v.id).collect()
}

fn review<R>(s: &mut Session, slot: SlotId, f: impl FnOnce(&mut Review, &mut Session) -> R) -> R {
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    f(borrow.as_any().downcast_mut::<Review>().unwrap(), s)
}

fn count(store: &Store, sql: &str) -> i64 {
    store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn real(store: &Store, sql: &str) -> f64 {
    store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn grades_of(store: &Store, item: &str) -> Vec<(f64, i64)> {
    store
        .conn()
        .prepare("SELECT at, quality FROM fluent_review WHERE item = ?1 ORDER BY at, id")
        .unwrap()
        .query_map([item], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Where an item's grades say it stands, which is what its row must say.
fn replayed(store: &Store, item: &str) -> Replay {
    sm2::replay(&grades_of(store, item))
}

fn same(item: &model::Item, r: &Replay) -> bool {
    (item.ease, item.interval, item.reps, item.due, item.reviewed, item.mastery)
        == (r.state.ease, r.state.interval, r.state.reps, r.due, r.reviewed, r.mastery)
}

/// Every bar in the app over the demo world: no reserved letter, no letter
/// twice, and the grade pad's six wear none at all.
#[test]
fn every_bar_wears_distinct_unreserved_letters() {
    let mut s = session();
    let ids = vec![
        Desk::id(),
        Lesson::id(READY),
        Lesson::id(LAST_DONE),
        Review::id(),
        Cards::id(),
        Grammar::id(),
        History::id(),
        Card::id("vocab_die_gebuehr"),
        Topic::id("artikel-nom-akk-dat"),
        Progress::id(),
        Setup::id(),
    ];
    let mut checked = 0;
    for id in ids {
        let slot = open(&mut s, id.clone());
        for phase in 0..3 {
            if id.tag == Lesson::TAG && id.arg(0) == Some(&READY.to_string()) {
                // Walk the player into each of its phases.
                match phase {
                    1 => lesson(&mut s, slot, |l, s| l.choose(0, s)),
                    2 => {
                        lesson(&mut s, slot, |l, s| {
                            l.advance(s);
                            // The ninth is the free write; skip to it by
                            // answering the others correctly.
                            while l.phase() != Phase::Done && !l.current().is_some_and(|e| e.self_check()) {
                                let ex = l.current().unwrap();
                                let key = ex.accepted.first().cloned().unwrap();
                                if ex.typed() {
                                    l.set_typed(key);
                                    l.submit(s);
                                } else {
                                    let i = ex.shown_choices().iter().position(|c| *c == key).unwrap();
                                    l.choose(i, s);
                                }
                                l.advance(s);
                            }
                            l.set_typed("Ich brauche ein Reisepass.".into());
                            l.submit(s);
                            assert_eq!(l.phase(), Phase::SelfGrade);
                        });
                    }
                    _ => {}
                }
            } else if phase > 0 {
                break;
            }
            let verbs = s.panel_verbs(slot);
            let mut seen = Vec::new();
            for v in &verbs {
                if let Some(c) = v.accel {
                    assert!(!crate::shell::keys::is_reserved(c), "{}: {c} is reserved", v.id);
                    assert!(!seen.contains(&c), "{}: {c} twice on {id}", v.id);
                    assert!(v.label.contains(c), "{}: the letter is in the label", v.id);
                    seen.push(c);
                }
            }
            checked += 1;
        }
    }
    assert!(checked >= 12);
    // The grade pad: six verbs, no letters, the digit leading each label.
    let slot = open(&mut s, Review::id());
    review(&mut s, slot, |r, s| r.flip(s));
    let verbs = s.panel_verbs(slot);
    let pad: Vec<&kernel::panel::Verb> = verbs.iter().filter(|v| v.id.starts_with("fluent.grade")).collect();
    assert_eq!(pad.len(), 6);
    for (q, v) in pad.iter().enumerate() {
        assert!(v.accel.is_none());
        assert!(v.label.starts_with(&q.to_string()), "{}", v.label);
    }
}

#[test]
fn the_desk_reads_the_demo_course() {
    let s = session();
    let store = s.store();
    let learner = model::learner(store).unwrap();
    assert_eq!(learner.name, "Andrey");
    assert!(learner.streak_active(s.now()));
    let shelf = model::shelf(store).unwrap();
    assert_eq!(shelf.id, READY);
    assert_eq!(shelf.exercises, 9);
    assert_eq!(shelf.reviews, 3);
    let (all, cards) = model::due_counts(store, s.now());
    assert_eq!(cards, 10, "seven reviews and three new cards");
    assert!(all > cards, "grammar is on the schedule too");
    assert_eq!(model::recent_accuracy(store, 5).len(), 5);
    assert_eq!(model::activity(store, s.now()).len(), 56);
    assert_eq!(model::CARDS.count(store, None), Some(24));
    assert_eq!(model::TOPICS.count(store, None), Some(6));
    assert_eq!(model::LESSONS.count(store, None), Some(26));
}

#[test]
fn a_closed_answer_is_graded_filed_and_undone_as_one() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    let store = s.store().clone();
    let before_reviews = count(&store, "SELECT COUNT(*) FROM fluent_review");
    let item_before = model::item_state(&store, "verb_sein");
    lesson(&mut s, slot, |l, s| {
        assert_eq!(l.phase(), Phase::Answering);
        assert_eq!(l.current().unwrap().seq, 1);
        let bist = l.current().unwrap().shown_choices().iter().position(|c| c == "bist").unwrap();
        l.choose(bist, s);
        assert_eq!(l.phase(), Phase::Feedback(Closed::Wrong));
    });
    let ex = model::exercises(&store, READY)[0].clone();
    assert_eq!(ex.answer.as_deref(), Some("bist"));
    assert_eq!(ex.result.as_deref(), Some("wrong"));
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_review"), before_reviews + 1);
    let after = model::item_state(&store, "verb_sein");
    assert_eq!(after.reps, 0, "a miss resets");
    assert!(model::lesson(&store, READY).unwrap().started.is_some());
    s.undo();
    let ex = model::exercises(&store, READY)[0].clone();
    assert!(ex.answer.is_none());
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_review"), before_reviews);
    assert_eq!(model::item_state(&store, "verb_sein"), item_before);
    s.redo();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_review"), before_reviews + 1);
}

#[test]
fn closed_grading_knows_exact_almost_and_wrong() {
    let acc = vec!["die Gebühr".to_string()];
    assert_eq!(model::grade_closed(" die Gebühr ", &acc), Closed::Correct);
    assert_eq!(model::grade_closed("die gebuehr", &acc), Closed::Almost);
    assert_eq!(model::grade_closed("der Gebühr", &acc), Closed::Wrong);
    assert_eq!(model::grade_closed("", &acc), Closed::Wrong);
    assert!(model::matches_model("die Kaution.", "die Kaution", &[]));
}

#[test]
fn a_free_answer_waits_for_the_grade_and_the_lesson_finishes_after_the_last() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    let store = s.store().clone();
    // Answer everything but the free write correctly.
    lesson(&mut s, slot, |l, s| {
        while l.current().is_some_and(|e| !e.self_check()) {
            let ex = l.current().unwrap();
            let key = ex.accepted.first().cloned().unwrap();
            if ex.typed() {
                l.set_typed(key);
                l.submit(s);
            } else {
                let i = ex.shown_choices().iter().position(|c| *c == key).unwrap();
                l.choose(i, s);
            }
            assert!(matches!(l.phase(), Phase::Feedback(Closed::Correct)));
            l.advance(s);
        }
        assert_eq!(l.current().unwrap().seq, 9);
        l.set_typed("Ich brauche ein Reisepass und Passfoto.".into());
        l.submit(s);
        assert_eq!(l.phase(), Phase::SelfGrade, "not the model answer: the learner grades");
    });
    let ex = model::exercises(&store, READY)[8].clone();
    assert!(ex.answer.is_some() && ex.self_grade.is_none());
    let reviews_before = count(&store, "SELECT COUNT(*) FROM fluent_review");
    lesson(&mut s, slot, |l, s| {
        l.grade(3, s);
        assert_eq!(l.phase(), Phase::Done);
        assert_eq!(l.outcome().0, 9);
    });
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_review"), reviews_before + 2, "one review per item");
    let row = model::lesson(&store, READY).unwrap();
    assert_eq!(row.status, "done");
    assert_eq!(row.accuracy, Some(1.0));
    let shelf = model::shelf(&store).unwrap();
    assert_eq!(shelf.status, "building", "tomorrow's placeholder is on the shelf");
    // Undo the finish: the lesson is open again and the placeholder gone.
    s.undo();
    assert_eq!(model::lesson(&store, READY).unwrap().status, "ready");
    assert_eq!(model::shelf(&store).unwrap().id, READY);
    s.redo();
    assert_eq!(model::shelf(&store).unwrap().status, "building");
}

#[test]
fn a_reopened_lesson_resumes_and_a_finished_one_is_its_summary() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        l.choose(0, s);
        l.advance(s);
    });
    let again = open(&mut s, Lesson::id(READY));
    lesson(&mut s, again, |l, _| {
        assert_eq!(l.index(), 1);
        assert_eq!(l.phase(), Phase::Answering);
    });
    let done = open(&mut s, Lesson::id(LAST_DONE));
    lesson(&mut s, done, |l, _| {
        assert_eq!(l.phase(), Phase::Done);
        assert_eq!(l.outcome(), (6, 8, 22.0));
    });
}

#[test]
fn grading_a_card_moves_its_date_and_undo_puts_it_back() {
    let mut s = session();
    let slot = open(&mut s, Review::id());
    let store = s.store().clone();
    let before = model::item_state(&store, FIRST_DUE);
    let filed = count(&store, &format!("SELECT COUNT(*) FROM fluent_review WHERE item = '{FIRST_DUE}'"));
    review(&mut s, slot, |r, s| {
        assert_eq!(r.total(), 10);
        assert_eq!(r.current().unwrap().item, FIRST_DUE, "longest overdue first");
        r.grade(4, s);
        assert_eq!(r.pos(), 0, "a card is graded from its back");
        r.flip(s);
        r.grade(4, s);
        assert_eq!(r.pos(), 1);
        assert!(!r.flipped());
    });
    let after = model::item_state(&store, FIRST_DUE);
    assert_eq!(after.reps, before.reps + 1);
    assert!(after.due > s.now());
    assert!(same(&after, &replayed(&store, FIRST_DUE)), "the schedule is the replay of the grades");
    assert_eq!(count(&store, &format!("SELECT COUNT(*) FROM fluent_review WHERE item = '{FIRST_DUE}'")), filed + 1);
    s.undo();
    assert_eq!(model::item_state(&store, FIRST_DUE), before);
    assert!(same(&before, &replayed(&store, FIRST_DUE)), "and so is the cache undo put back");
    assert_eq!(count(&store, &format!("SELECT COUNT(*) FROM fluent_review WHERE item = '{FIRST_DUE}'")), filed);
    s.redo();
    assert_eq!(model::item_state(&store, FIRST_DUE).reps, before.reps + 1);
}

#[test]
fn the_tools_read_the_course_and_write_through_the_apps_own_paths() {
    let mut s = session();
    let apps = kernel::app::Apps::new(APPS);
    let due = apps.tool("fluent.due").unwrap();
    let out = (due.run)(&mut s, &serde_json::json!({})).unwrap();
    assert!(out["due"].as_array().unwrap().len() >= 10);
    let lesson_tool = apps.tool("fluent.lesson").unwrap();
    let out = (lesson_tool.run)(&mut s, &serde_json::json!({})).unwrap();
    assert_eq!(out["id"], LAST_DONE);
    assert_eq!(out["exercises"].as_array().unwrap().len(), 8);
    let free = out["exercises"][7]["id"].as_i64().unwrap();
    let grade = apps.tool("fluent.grade").unwrap();
    assert!(grade.writes && !grade.asks);
    (grade.run)(&mut s, &serde_json::json!({"exercise": free, "quality": 4, "note": "besser", "fix": "…"})).unwrap();
    assert_eq!(model::exercises(s.store(), LAST_DONE)[7].tutor_grade, Some(4));
    s.undo();
    assert_eq!(model::exercises(s.store(), LAST_DONE)[7].tutor_grade, Some(3));
    let author = apps.tool("fluent.author").unwrap();
    // Finish today's, so the shelf holds a placeholder to replace.
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| l.finish(s));
    assert_eq!(model::shelf(s.store()).unwrap().status, "building");
    let refused = (author.run)(&mut s, &serde_json::json!({"title": "x", "exercises": [{"section": "warmup", "kind": "mcq", "grading": "closed", "prompt": "?", "items": ["verb_sein"]}]}));
    assert!(refused.unwrap_err().contains("choices"));
    let out = (author.run)(&mut s, &serde_json::json!({
        "title": "Beim Bäcker", "focus": ["vocab:food"],
        "exercises": [
            {"section": "warmup", "kind": "mcq", "grading": "closed", "prompt": "Ein ___ Brot, bitte.", "choices": ["halbes", "halb", "halben"], "accepted": ["halbes"], "items": ["adjektiv_endungen"]},
            {"section": "cooldown", "kind": "free_write", "grading": "self_check", "prompt": "Bestell etwas.", "model": "Ich hätte gern zwei Brötchen.", "items": ["writing_official"]}
        ]
    })).unwrap();
    assert_eq!(out["exercises"], 2);
    let shelf = model::shelf(s.store()).unwrap();
    assert_eq!(shelf.status, "ready");
    assert_eq!(shelf.title, "Beim Bäcker");
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_lesson WHERE status = 'building'"), 0);
    s.undo();
    assert_eq!(model::shelf(s.store()).unwrap().status, "building");
    s.redo();
    assert_eq!(model::shelf(s.store()).unwrap().title, "Beim Bäcker");
    assert_eq!(tools::all().len(), 4);
}

#[test]
fn topics_unpack_their_sections_and_tables_align() {
    let s = session();
    let t = model::topic(s.store(), "artikel-nom-akk-dat").unwrap();
    let sections = model::sections(&t.sections);
    assert_eq!(sections.len(), 5);
    let Some(model::Section::Table { columns, rows, .. }) = sections.get(1) else {
        panic!("a table second");
    };
    let text = model::table_text(columns, rows);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0].trim_end(), "           Maskulin  Feminin  Neutrum  Plural");
    assert!(lines[1].starts_with("---------  --------"));
    assert_eq!(lines[2], "Nominativ  der       die      das      die");
    assert_eq!(model::topic_notes(s.store(), "artikel-nom-akk-dat").len(), 2);
    assert_eq!(model::sections("not json").len(), 0);
}

#[test]
fn a_patch_records_hints_and_time_without_a_grade() {
    let mut s = session();
    let ex = model::exercises(s.store(), READY)[2].clone();
    let filed = count(s.store(), "SELECT COUNT(*) FROM fluent_review");
    let patch = Patch { answer: Some("die Gebühr".into()), hints_shown: 2, elapsed: 12.0, ..Patch::default() };
    assert!(model::record(&mut s, &ex, patch, "answer"));
    let after = model::exercises(s.store(), READY)[2].clone();
    assert_eq!(after.hints_shown, 2);
    assert_eq!(after.elapsed, 12.0);
    assert!(after.result.is_none());
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_review"), filed, "no grade, no review");
}

/// The demo course is consistent with its own history: every item that was
/// ever graded stands exactly where replaying those grades puts it, no
/// grade is stamped in a day that has not happened, and none is older than
/// the item it is about.
#[test]
fn the_seeded_schedule_is_a_replay_of_the_seeded_grades() {
    let s = session();
    let store = s.store();
    let graded: Vec<String> = store
        .conn()
        .prepare("SELECT DISTINCT item FROM fluent_review ORDER BY item")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(graded.len() > 20, "most of the course has a history");
    for id in &graded {
        let item = model::item_state(store, id);
        assert!(same(&item, &replayed(store, id)), "{id} stands where its grades put it");
    }
    let now = s.now();
    assert_eq!(count(store, &format!("SELECT COUNT(*) FROM fluent_review WHERE at > {now}")), 0);
    assert_eq!(
        count(
            store,
            "SELECT COUNT(*) FROM fluent_review r JOIN fluent_item i ON i.id = r.item WHERE r.at < i.created"
        ),
        0,
        "nothing was graded before it was learned"
    );
    // Yesterday's lesson filed its own grades, the tutor's where the tutor
    // overruled the learner.
    assert_eq!(
        count(store, &format!("SELECT COUNT(*) FROM fluent_review WHERE lesson = {LAST_DONE}")),
        10,
        "one per item of each answered exercise"
    );
    let free: Vec<i64> = store
        .conn()
        .prepare("SELECT quality FROM fluent_review WHERE lesson_uid = ?1 AND seq = 8 ORDER BY item")
        .unwrap()
        .query_map([uid(LAST_DONE)], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(free, vec![3, 3], "self-graded 4, the tutor said 3");
}

/// A grade another device filed arrives as an ordinary row, and the next
/// look at the store replays the item it is about.
#[test]
fn a_grade_from_another_device_reaches_the_schedule_at_the_next_poll() {
    let mut s = session();
    s.settle();
    let store = s.store().clone();
    let before = model::item_state(&store, "vocab_der_termin");
    let at = s.now() - 3600.0;
    store
        .write(move |c| {
            c.execute(
                "INSERT INTO fluent_review(item, at, quality, device) VALUES('vocab_der_termin', ?1, 5, 'phone')",
                [at],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(model::item_state(&store, "vocab_der_termin"), before, "nothing has replayed it yet");
    s.settle();
    let after = model::item_state(&store, "vocab_der_termin");
    assert_eq!(after.reps, before.reps + 1);
    assert!(after.due > before.due);
    assert!(same(&after, &replayed(&store, "vocab_der_termin")));
    // A poll that finds the table where it left it does nothing.
    let revision = store.revision(&["fluent_item"]);
    s.settle();
    assert_eq!(store.revision(&["fluent_item"]), revision);
}

// ---------------------------------------------------------------------------
// Looking a word up
// ---------------------------------------------------------------------------

/// A word the deck has is answered by the deck: no question, no network,
/// and the card behind it on the bar. The article is not part of the word —
/// a selection of `gebühr` finds `die Gebühr`.
#[test]
fn a_word_the_deck_has_is_looked_up_offline_and_links_to_its_card() {
    let mut s = tutored();
    let slot = open(&mut s, Lookup::id("gebühr"));
    lookup(&mut s, slot, |p, s| p.poll(s));
    let found = lookup(&mut s, slot, |p, _| p.found());
    assert!(
        matches!(&found, Found::Deck(item, _) if item == "vocab_die_gebuehr"),
        "{found:?}"
    );
    assert_eq!(found.source(), "from your deck");
    let entry = found.entry().expect("the card, as an entry");
    assert_eq!(entry.term, "die Gebühr");
    assert_eq!(entry.translation, "сбор, пошлина / a fee");
    assert_eq!(lookup(&mut s, slot, |p, _| p.shown_term()), "die Gebühr");
    assert_eq!(lookup(&mut s, slot, |p, _| p.title()), "gebühr", "titled by what was selected");

    assert_eq!(verbs(&s, slot), vec!["fluent.play", "fluent.card"]);
    let card = s
        .panel_verbs(slot)
        .into_iter()
        .find(|v| v.id == "fluent.card")
        .expect("the card link");
    assert_eq!(card.accel, Some('c'));
    assert!(gateway(&s).requests().is_empty(), "the deck asks nobody");
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM fluent_lookup"), 0);
}

/// A word the deck has never seen goes to the tutor — once. The answer is
/// kept, the next panel reads the cache, and **add** puts the word in the
/// deck as an item and a card that one undo takes back.
#[test]
fn a_word_the_deck_lacks_is_asked_of_the_tutor_once_and_then_added() {
    let mut s = tutored();
    let store = s.store().clone();
    let slot = open(&mut s, Lookup::id("Tüte"));
    lookup(&mut s, slot, |p, s| p.poll(s));

    let found = lookup(&mut s, slot, |p, _| p.found());
    let Found::Tutor(entry) = &found else { panic!("{found:?}") };
    assert_eq!(entry.term, "die Tüte", "a noun comes back with its article");
    assert_eq!(entry.pos, "Substantiv");
    assert_eq!(found.source(), "from the tutor");

    let asked = gateway(&s).requests();
    assert_eq!(asked.len(), 1, "one question, and no chat behind it");
    assert!(asked[0].tools.is_empty(), "a dictionary has no hands");
    assert_eq!(asked[0].last_user(), Some("LOOK UP THE WORD „Tüte“"));
    let system = asked[0].messages[0].text();
    assert!(system.contains("dictionary for a learner of German"), "{system}");
    assert!(system.contains("whose language is Russian"), "{system}");

    // Kept: the same word asked again, article and all, reads the cache.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_lookup"), 1);
    let again = open(&mut s, Lookup::id("die Tüte"));
    lookup(&mut s, again, |p, s| p.poll(s));
    assert!(matches!(lookup(&mut s, again, |p, _| p.found()), Found::Cache(_)));
    assert_eq!(gateway(&s).requests().len(), 1, "a word costs one question ever");
    assert_eq!(verbs(&s, again), vec!["fluent.play", "fluent.add"]);

    // Added: an item on the schedule and a card in the deck.
    lookup(&mut s, again, |p, s| p.run("fluent.add", s));
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_card WHERE item = \'vocab_die_tuete\'"), 1);
    let card = model::card(&store, "vocab_die_tuete").expect("the card just filed");
    assert_eq!((card.front.as_str(), card.audio.as_str()), ("die Tüte", "die Tüte"));
    assert!(card.back.contains("Tüte"), "{}", card.back);
    assert_eq!(card.reps, 0, "a new word, due today");
    assert!(said(&s).contains("added to the deck"), "{}", said(&s));
    assert_eq!(verbs(&s, again), vec!["fluent.play", "fluent.card"], "the bar follows");

    s.undo();
    assert!(model::card(&store, "vocab_die_tuete").is_none());
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_item WHERE id = \'vocab_die_tuete\'"), 0);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM fluent_lookup"),
        1,
        "the cache is nobody\'s decision and undo does not touch it"
    );
    s.redo();
    assert!(model::card(&store, "vocab_die_tuete").is_some());
}

/// A build with no tutor says so in the panel rather than sitting on
/// *asking* forever.
#[test]
fn a_lookup_with_no_agent_app_says_there_is_no_tutor() {
    let mut s = session();
    let slot = open(&mut s, Lookup::id("Tüte"));
    lookup(&mut s, slot, |p, s| p.poll(s));
    let found = lookup(&mut s, slot, |p, _| p.found());
    assert!(matches!(&found, Found::Failed(why) if why.contains("no agent app")), "{found:?}");
    assert_eq!(verbs(&s, slot), vec!["fluent.play"], "nothing to add and no card");
}

/// What a selection has to be before the lesson's bar wears **lookup**, and
/// where the verb goes.
#[test]
fn a_selection_of_up_to_three_words_puts_lookup_on_the_lessons_bar() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    assert!(!verbs(&s, slot).contains(&"fluent.lookup"), "nothing selected");

    lesson(&mut s, slot, |l, _| {
        assert!(l.set_selection(Some("  Gebühr ".into())));
        assert!(!l.set_selection(Some("  Gebühr ".into())), "the same selection is no news");
        assert_eq!(l.lookup_term().as_deref(), Some("Gebühr"));
        assert_eq!(
            {
                l.set_selection(Some("die Gebühr zahlen".into()));
                l.lookup_term()
            },
            Some("die Gebühr zahlen".to_string()),
            "three words is still a term"
        );
        for never in ["____", "   ", "die Gebühr für den Antrag", &"ü".repeat(41)] {
            l.set_selection(Some(never.to_string()));
            assert_eq!(l.lookup_term(), None, "{never:?}");
        }
        l.set_selection(Some("Gebühr".into()));
    });

    let verb = s
        .panel_verbs(slot)
        .into_iter()
        .find(|v| v.id == "fluent.lookup")
        .expect("lookup on the bar");
    assert_eq!(verb.accel, Some('k'));
    assert_eq!(verb.label, "lookup");

    // Answering the exercise takes the selection with it.
    lesson(&mut s, slot, |l, s| l.choose(0, s));
    lesson(&mut s, slot, |l, s| l.advance(s));
    assert!(!verbs(&s, slot).contains(&"fluent.lookup"));
}

// ---------------------------------------------------------------------------
// The grade that comes back while the learner is still reading
// ---------------------------------------------------------------------------

/// A self-check answer is sent to the tutor the moment it is written, and
/// the verdict lands on the exercise it was about. The learner waits for
/// none of it: the phase is the grade pad either way.
#[test]
fn the_tutor_grades_a_self_check_answer_as_it_is_written() {
    let mut s = tutored();
    let store = s.store().clone();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        while l.current().is_some_and(|e| !e.self_check()) {
            let ex = l.current().unwrap();
            let key = ex.accepted.first().cloned().unwrap();
            if ex.typed() {
                l.set_typed(key);
                l.submit(s);
            } else {
                let i = ex.shown_choices().iter().position(|c| *c == key).unwrap();
                l.choose(i, s);
            }
            l.advance(s);
        }
        l.set_typed("Ich brauche ein Reisepass.".into());
        l.submit(s);
        assert_eq!(l.phase(), Phase::SelfGrade, "the grade pad, at once");
    });

    // The fake answers where it is asked, so the verdict is already here.
    let ex = model::exercises(&store, READY)[8].clone();
    assert_eq!(ex.tutor_grade, Some(4));
    assert_eq!(ex.tutor_note, "Fast richtig — ein kleiner Fehler.");
    assert_eq!(ex.tutor_fix, "Ich brauche ein Reisepass.");
    assert!(said(&s).contains("tutor checked Q9 — 4/5"), "{}", said(&s));

    let asked = gateway(&s).requests();
    assert_eq!(asked.len(), 1);
    let question = asked[0].last_user().expect("the answer to grade");
    assert!(question.starts_with("GRADE THIS ANSWER by a learner of German at A2."), "{question}");
    assert!(question.contains("Schreib zwei Sätze"), "the question it answers: {question}");
    assert!(question.contains("Ich brauche meinen Reisepass"), "the model answer: {question}");
    assert!(question.ends_with("What they wrote: „Ich brauche ein Reisepass.“"), "{question}");
    let system = asked[0].messages[0].text();
    assert!(system.contains("- 5 perfect"), "the brief\'s own scale: {system}");
    assert!(system.contains("one JSON object"), "{system}");

    // One undo for the tutor's word, one for the answer under it.
    s.undo();
    assert_eq!(model::exercises(&store, READY)[8].tutor_grade, None);
    assert_eq!(model::exercises(&store, READY)[8].answer.as_deref(), Some("Ich brauche ein Reisepass."));
    s.undo();
    assert_eq!(model::exercises(&store, READY)[8].answer, None);
}

/// What the quiet line says depends on what the learner said first, and a
/// grade already on the row is never written over.
#[test]
fn the_tutors_line_answers_the_self_grade_and_never_overwrites_one() {
    let graded = |self_grade: Option<i64>| -> (String, Option<i64>, usize) {
        let mut s = tutored();
        let store = s.store().clone();
        // The finished lesson's free answer, with the tutor's own word
        // taken off it so this one is the first to land.
        store
            .write(move |c| {
                c.execute(
                    "UPDATE fluent_exercise SET tutor_grade = NULL, tutor_note = \'\', tutor_fix = \'\',
                            self_grade = ?2
                      WHERE lesson = ?1 AND seq = 8",
                    rusqlite::params![LAST_DONE, self_grade],
                )?;
                Ok(())
            })
            .unwrap();
        let ex = model::exercises(&store, LAST_DONE)[7].clone();
        super::tutor::grade(&mut s, &ex, ex.answer.as_deref().unwrap_or(""));
        let after = model::exercises(&store, LAST_DONE)[7].tutor_grade;
        (said(&s), after, gateway(&s).requests().len())
    };
    assert!(graded(Some(4)).0.contains("tutor checked Q8 — agrees (4/5)"));
    assert!(graded(Some(5)).0.contains("tutor suggests 4/5 for Q8 — you said 5"));
    assert!(graded(None).0.contains("tutor checked Q8 — 4/5"));
    assert_eq!(graded(Some(4)).1, Some(4), "the verdict is on the row");

    // An exercise the tutor has already graded is left alone, and nothing
    // is asked about it at all.
    let mut s = tutored();
    let store = s.store().clone();
    let ex = model::exercises(&store, LAST_DONE)[7].clone();
    assert_eq!(ex.tutor_grade, Some(3), "the seeded lesson carries the tutor\'s word");
    super::tutor::grade(&mut s, &ex, "something else");
    assert_eq!(model::exercises(&store, LAST_DONE)[7].tutor_grade, Some(3));
    assert!(gateway(&s).requests().is_empty(), "a graded answer is not asked about twice");
}

/// The tutor's grade is the last word: the grades the answer filed are
/// rewritten to it and the items follow, and one undo takes both back.
#[test]
fn the_tutors_grade_rewrites_what_the_answer_filed() {
    let mut s = session();
    let store = s.store().clone();
    let ex = model::exercises(&store, LAST_DONE)[7].clone();
    assert_eq!((ex.self_grade, ex.tutor_grade), (Some(4), Some(3)));
    let filed = |store: &Store| -> Vec<i64> {
        store
            .conn()
            .prepare("SELECT quality FROM fluent_review WHERE lesson_uid = ?1 AND seq = ?2 ORDER BY item")
            .unwrap()
            .query_map(rusqlite::params![ex.lesson_uid, ex.seq], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(filed(&store), vec![3, 3]);
    let before = model::item_state(&store, "writing_official");
    assert!(model::tutor_grade(&mut s, ex.id, 1, "Noch nicht: zwei Artikelfehler.", "…"));
    assert_eq!(filed(&store), vec![1, 1]);
    let after = model::item_state(&store, "writing_official");
    assert_eq!(after.reps, 0, "a 1 is a miss: the item starts again");
    assert!(after.due < before.due);
    assert!(same(&after, &replayed(&store, "writing_official")));
    s.undo();
    assert_eq!(filed(&store), vec![3, 3]);
    assert_eq!(model::item_state(&store, "writing_official"), before);
    assert_eq!(model::exercises(&store, LAST_DONE)[7].tutor_grade, Some(3));
    s.redo();
    assert_eq!(filed(&store), vec![1, 1]);
    assert_eq!(model::exercises(&store, LAST_DONE)[7].tutor_grade, Some(1));
}

/// Finishing a lesson feeds the streak by the day it was played on, and
/// undo gives the learner back the streak they had.
#[test]
fn finishing_feeds_the_streak_and_undo_puts_it_back() {
    for (days, streak, want) in [(0.0, 12, 12), (1.0, 12, 13), (4.0, 12, 1)] {
        let mut s = session();
        let store = s.store().clone();
        let last = s.now() - days * DAY;
        store
            .write(move |c| {
                c.execute(
                    "UPDATE fluent_learner SET streak = ?1, last_active = ?2 WHERE id = 1",
                    rusqlite::params![streak, last],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(model::finish(&mut s, READY));
        let fed = model::learner(&store).unwrap();
        assert_eq!((fed.streak, fed.last_active), (want, Some(s.now())), "{days} days on");
        s.undo();
        let back = model::learner(&store).unwrap();
        assert_eq!((back.streak, back.last_active), (streak, Some(last)));
        s.redo();
        assert_eq!(model::learner(&store).unwrap().streak, want);
    }
    // A learner who has never played starts a streak at one.
    assert_eq!(model::streak_after(0, None, 1000.0), 1);
}

/// A lesson is named by its uid everywhere, and the local number every
/// panel takes is kept beside it — whichever of the two rows this device
/// sees first.
#[test]
fn a_lesson_numbers_the_rows_that_name_it_whichever_arrives_first() {
    let s = session();
    let store = s.store().clone();
    // The phone's rows land before the lesson they belong to does.
    store
        .write(|c| {
            c.execute(
                "INSERT INTO fluent_exercise(lesson_uid, seq, prompt) VALUES('from-the-phone', 1, 'Wie geht es dir?')",
                [],
            )?;
            c.execute(
                "INSERT INTO fluent_review(item, at, quality, device, lesson_uid, seq)
                 VALUES('verb_sein', 1, 5, 'phone', 'from-the-phone', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        count(&store, "SELECT lesson FROM fluent_exercise WHERE lesson_uid = 'from-the-phone' AND seq = 1"),
        0,
        "no number here yet"
    );
    store
        .write(|c| {
            c.execute("INSERT INTO fluent_lesson(id, uid, title) VALUES(500, 'from-the-phone', 'Vom Handy')", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        count(&store, "SELECT lesson FROM fluent_exercise WHERE lesson_uid = 'from-the-phone' AND seq = 1"),
        500
    );
    assert_eq!(
        count(&store, "SELECT lesson FROM fluent_review WHERE lesson_uid = 'from-the-phone'"),
        500
    );
    // And the other order: the lesson is here, so the row is numbered as
    // it lands.
    store
        .write(|c| {
            c.execute(
                "INSERT INTO fluent_exercise(lesson_uid, seq, prompt) VALUES('from-the-phone', 2, 'Und dir?')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        count(&store, "SELECT lesson FROM fluent_exercise WHERE lesson_uid = 'from-the-phone' AND seq = 2"),
        500
    );
}

/// The grammar list is in the course's order, not the category slug's, and
/// a topic that changes category moves with it.
#[test]
fn a_topics_rank_follows_its_category() {
    let s = session();
    let store = s.store().clone();
    let rank = |id: &str| model::topic(&store, id).unwrap().rank;
    assert_eq!(rank("artikel-nom-akk-dat"), 0, "Fälle lead");
    assert_eq!(rank("dativ-praepositionen"), 1);
    assert_eq!(rank("adjektiv-endungen"), 2);
    assert_eq!(rank("v2-wortstellung"), 4);
    let order: Vec<String> = store
        .conn()
        .prepare("SELECT category FROM fluent_topic ORDER BY rank, level, title")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        order,
        ["cases", "prepositions", "prepositions", "adjectives", "sentence_structure", "sentence_structure"]
    );
    store
        .write(|c| {
            c.execute("UPDATE fluent_topic SET category = 'verbs' WHERE id = 'adjektiv-endungen'", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(rank("adjektiv-endungen"), 3, "the rank follows the category");
}

// ---------------------------------------------------------------------------
// The tutor
// ---------------------------------------------------------------------------

/// Finishing a lesson calls the tutor: a chat opens beside the summary
/// with the brief as its first turn and the lesson as its chip, and the
/// shelf's placeholder is told which chat it is being built in.
#[test]
fn finishing_a_lesson_opens_the_tutors_chat_beside_the_summary() {
    let mut s = tutored();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| l.finish(s));
    s.settle();

    let shelf = model::shelf(s.store()).unwrap();
    assert_eq!(shelf.status, "building");
    let chat = shelf.chat.expect("the placeholder carries the tutor's chat");

    let open_slots = s.showing(&Chat::id(chat));
    assert_eq!(open_slots.len(), 1, "one chat, open");
    assert_eq!(s.join_parent_of(open_slots[0]), Some(slot), "joined to the summary");

    let turns = agent::turns(s.store(), chat);
    let brief = turns[0].text().to_string();
    assert!(brief.contains("fluent.grade"), "{brief}");
    assert!(brief.contains(&format!("lesson {READY}")), "the lesson just finished: {brief}");
    assert_eq!(turns[0].chips.len(), 1, "the lesson rides in as a chip");
    assert_eq!(turns[0].chips[0]["tag"], "lesson");
    assert!(turns[0].context.as_ref().is_some_and(|c| c.contains("Bei der Ausländerbehörde")));

    // The desk reads the same row: while the tutor is at work its chat is
    // the one thing on the bar beside the course's own links.
    let desk = open(&mut s, Desk::id());
    let verbs: Vec<&str> = s.panel_verbs(desk).iter().map(|v| v.id).collect();
    assert!(verbs.contains(&"fluent.tutor"), "{verbs:?}");
    assert!(!verbs.contains(&"fluent.start"), "there is nothing to start yet");
}

/// A build without the agent app has no tutor, and says so rather than
/// half-working: the lesson still finishes and the shelf still holds its
/// placeholder.
#[test]
fn a_build_with_no_agent_app_says_there_is_no_tutor() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| l.finish(s));
    s.settle();
    assert!(said(&s).contains("no agent app"), "{}", said(&s));
    let shelf = model::shelf(s.store()).unwrap();
    assert_eq!(shelf.status, "building");
    assert_eq!(shelf.chat, None);
}

/// *build* on an empty desk is the same road, with the schedule alone: no
/// learner refuses, and a learner with nothing played gets the first
/// lesson's brief.
#[test]
fn build_asks_the_tutor_for_todays_lesson_and_refuses_without_a_learner() {
    let mut s = tutored();
    let store = s.store().clone();
    // An empty course: no lessons at all, and no learner.
    store
        .write(|c| {
            c.execute("DELETE FROM fluent_lesson", [])?;
            c.execute("DELETE FROM fluent_learner", [])?;
            Ok(())
        })
        .unwrap();
    let desk = open(&mut s, Desk::id());
    let verbs: Vec<&str> = s.panel_verbs(desk).iter().map(|v| v.id).collect();
    assert_eq!(verbs, vec!["fluent.setup"], "nothing is offered before there is a learner");
    super::tutor::author(&mut s, desk, true);
    assert!(said(&s).contains("set the course up first"), "{}", said(&s));
    assert_eq!(model::shelf(&store), None, "and nothing was put on the shelf");

    // With a learner and no history, the brief is the first lesson's.
    assert!(model::save_learner(
        &mut s,
        &model::Learner {
            name: "Andrey".into(),
            native: "Russian".into(),
            target: "German".into(),
            level: "A1".into(),
            goal: "B1".into(),
            daily_minutes: 30,
            streak: 0,
            last_active: None,
        }
    ));
    let desk = open(&mut s, Desk::id());
    let verbs: Vec<&str> = s.panel_verbs(desk).iter().map(|v| v.id).collect();
    assert!(verbs.contains(&"fluent.build"), "{verbs:?}");
    let panel = s.panel(desk).unwrap();
    panel.borrow_mut().run("fluent.build", &mut s);
    s.settle();
    let shelf = model::shelf(&store).expect("a placeholder for today");
    assert_eq!(shelf.status, "building");
    assert_eq!(shelf.for_date, day_start(s.now()), "for today, not tomorrow");
    let chat = shelf.chat.expect("the tutor's chat");
    let brief = agent::turns(&store, chat)[0].text().to_string();
    assert!(brief.contains("no history yet"), "{brief}");
    assert!(brief.contains("fluent.due"), "{brief}");
}

/// *build fresh* over a lesson built for a day long gone: the tutor is
/// asked for today's, and the stale one is left exactly where it stands —
/// still in the history, still playable, until the tutor's own lesson
/// takes the shelf from it by being for a later day.
#[test]
fn build_fresh_leaves_the_stale_lesson_where_it_stands() {
    let mut s = tutored();
    let store = s.store().clone();
    let long_ago = day_start(s.now()) - 5.0 * DAY;
    store
        .write(move |c| {
            c.execute("UPDATE fluent_lesson SET for_date = ?2 WHERE id = ?1", rusqlite::params![READY, long_ago])?;
            Ok(())
        })
        .unwrap();
    let desk = open(&mut s, Desk::id());
    let verbs: Vec<&str> = s.panel_verbs(desk).iter().map(|v| v.id).collect();
    assert_eq!(verbs[..2], ["fluent.build", "fluent.play_anyway"], "stale: {verbs:?}");

    let panel = s.panel(desk).unwrap();
    panel.borrow_mut().run("fluent.build", &mut s);
    s.settle();
    let shelf = model::shelf(&store).unwrap();
    assert_eq!(shelf.status, "building");
    assert_eq!(shelf.for_date, day_start(s.now()), "the placeholder is for today");
    assert!(shelf.chat.is_some(), "and carries the tutor's chat");
    assert_eq!(
        model::lesson(&store, READY).unwrap().status,
        "ready",
        "the stale lesson is untouched"
    );
    let verbs: Vec<&str> = s.panel_verbs(desk).iter().map(|v| v.id).collect();
    assert!(verbs.contains(&"fluent.tutor"), "{verbs:?}");
}

/// The brief is the course's pedagogy in one turn: who the learner is,
/// what the tools are called, and never the workspace's word *session*.
#[test]
fn the_brief_says_who_the_learner_is_and_what_the_tools_are() {
    let s = session();
    let learner = model::learner(s.store()).unwrap();
    let compile = prompt::brief(&learner, Task::Compile { lesson: LAST_DONE, day: s.now() });
    for want in ["Andrey", "Russian", "German", "A2", "B1", "30 minutes"] {
        assert!(compile.contains(want), "the brief says {want}");
    }
    for tool in ["fluent.lesson", "fluent.grade", "fluent.due", "fluent.author"] {
        assert!(compile.contains(tool), "the brief names {tool}");
    }
    assert!(compile.contains("quality of 0 to 5"), "the grading scale");
    assert!(compile.contains("set piece"), "the arc");
    assert!(compile.contains(&model::fmt_iso_day(s.now())), "the day it is for");
    assert!(!compile.contains("session"), "a sitting is a lesson in this book");
    assert!(!compile.contains("no history yet"));
    let first = prompt::brief(&learner, Task::Author { first: true, day: s.now() });
    assert!(first.contains("no history yet"));
    assert!(!first.contains("the one just finished"), "there is no lesson to read");
    assert!(prompt::brief(&learner, Task::Author { first: false, day: s.now() })
        .contains("fluent.author"));
}

/// Everything a lesson leaves behind travels in the one `fluent.author`
/// call, and one `cmd+z` takes it all back — including, cell for cell,
/// the topic the call extended.
#[test]
fn authoring_files_the_cards_topics_notes_and_mistakes_as_one() {
    let mut s = session();
    let apps = kernel::app::Apps::new(APPS);
    let author = apps.tool("fluent.author").unwrap();
    let store = s.store().clone();
    let before = model::topic(&store, "artikel-nom-akk-dat").unwrap();
    let notes_before = model::topic_notes(&store, "artikel-nom-akk-dat").len();
    let mistake_before = count(&store, "SELECT frequency FROM fluent_mistake WHERE id = 'article_gender'");
    let out = (author.run)(
        &mut s,
        &serde_json::json!({
            "title": "Beim Vermieter", "finished": LAST_DONE,
            "exercises": [{"section": "new", "kind": "cloze", "grading": "closed",
                "prompt": "Die ___ beträgt zwei Monatsmieten.", "accepted": ["Kaution"],
                "items": ["vocab_die_hausordnung"]}],
            "cards": [{"item": "vocab_die_hausordnung", "front": "die Hausordnung",
                "back": "правила дома / house rules", "example": "Die Hausordnung hängt im Flur.",
                "audio": "die Hausordnung", "notes": "Feminin."}],
            "topics": [
                {"id": "artikel-nom-akk-dat", "title": "Artikel: Nominativ, Akkusativ, Dativ",
                 "category": "cases", "level": "A1", "summary": "Wie der/die/das sich nach Fall verändern.",
                 "mastery": 4, "items": ["article_gender", "kaution_genus"],
                 "sections": [{"kind": "tip", "body": "Nach mit steht immer der Dativ."}],
                 "related": ["v2-wortstellung"]},
                {"id": "mietvertrag-nomen", "title": "Nomen im Mietvertrag", "category": "nouns",
                 "level": "A2", "summary": "Kaution, Nebenkosten, Hausordnung.",
                 "items": ["vocab_die_hausordnung"],
                 "sections": [{"kind": "text", "body": "Die Wörter, die im Vertrag stehen."}],
                 "related": []}
            ],
            "topic_notes": [{"topic": "artikel-nom-akk-dat", "note": "»der Kaution« → »die Kaution«"}],
            "mistakes": [
                {"id": "article_gender", "category": "grammar", "subcategory": "gender",
                 "wrong": "der Kaution", "right": "die Kaution", "context": "Q8", "notes": "wieder das Genus"},
                {"id": "kaution_genus", "category": "vocabulary", "subcategory": "gender",
                 "wrong": "das Kaution", "right": "die Kaution", "context": "Q8", "notes": ""}
            ]
        }),
    )
    .unwrap();
    assert_eq!(out["filed"], serde_json::json!({"cards": 1, "topics": 2, "topic_notes": 1, "mistakes": 2}));
    assert_eq!(out["finished"], LAST_DONE);

    // The word is an item on the schedule and a card in the deck.
    let card = model::card(&store, "vocab_die_hausordnung").expect("the card");
    assert_eq!(card.front, "die Hausordnung");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_item WHERE id = 'vocab_die_hausordnung' AND kind = 'vocab'"), 1);
    // The new topic is written, the old one extended: its five sections
    // keep their place and the tip is appended, the links are unioned.
    let after = model::topic(&store, "artikel-nom-akk-dat").unwrap();
    let sections = model::sections(&after.sections);
    assert_eq!(sections.len(), 6, "the five it had, and the tip");
    assert!(after.related.contains(&"v2-wortstellung".to_string()));
    assert!(after.related.contains(&"wechselpraepositionen".to_string()), "what it had is kept");
    let items: String = store
        .conn()
        .query_row("SELECT items FROM fluent_topic WHERE id = 'artikel-nom-akk-dat'", [], |r| r.get(0))
        .unwrap();
    assert!(items.contains("article_gender"), "what it had is kept: {items}");
    assert!(items.contains("kaution_genus"), "and what the call added: {items}");
    assert_eq!(after.mastery, Some(4));
    assert_eq!(after.practiced, Some(LAST_DONE));
    let practiced: String = store
        .conn()
        .query_row("SELECT practiced FROM fluent_topic WHERE id = 'artikel-nom-akk-dat'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(practiced, uid(LAST_DONE), "by uid, as every device names it");
    assert_eq!(model::topic(&store, "mietvertrag-nomen").unwrap().rank, 6, "nouns come sixth");
    assert_eq!(model::topic_notes(&store, "artikel-nom-akk-dat").len(), notes_before + 1);
    assert_eq!(
        count(&store, "SELECT frequency FROM fluent_mistake WHERE id = 'article_gender'"),
        mistake_before + 1,
        "a pattern seen before is one row with its count raised"
    );
    assert_eq!(count(&store, "SELECT frequency FROM fluent_mistake WHERE id = 'kaution_genus'"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_item WHERE id = 'kaution_genus' AND kind = 'error'"), 1);

    // One undo takes the lesson and everything filed with it.
    s.undo();
    assert!(model::card(&store, "vocab_die_hausordnung").is_none());
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_item WHERE id = 'vocab_die_hausordnung'"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_item WHERE id = 'kaution_genus'"), 0);
    assert!(model::topic(&store, "mietvertrag-nomen").is_none());
    assert_eq!(model::topic(&store, "artikel-nom-akk-dat"), Some(before.clone()), "the extended topic, cell for cell");
    assert_eq!(model::topic_notes(&store, "artikel-nom-akk-dat").len(), notes_before);
    assert_eq!(
        count(&store, "SELECT frequency FROM fluent_mistake WHERE id = 'article_gender'"),
        mistake_before
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_mistake WHERE id = 'kaution_genus'"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_lesson WHERE title = 'Beim Vermieter'"), 0);

    // And redo files the same rows again, under the same names.
    s.redo();
    assert_eq!(model::topic(&store, "artikel-nom-akk-dat").unwrap().mastery, Some(4));
    assert_eq!(model::topic_notes(&store, "artikel-nom-akk-dat").len(), notes_before + 1);
    assert!(model::card(&store, "vocab_die_hausordnung").is_some());
}

/// The setup form: one row, one action, and the first save stamps the day
/// the course began.
#[test]
fn the_setup_form_writes_the_learner_and_undo_takes_it_back() {
    let mut s = session();
    let store = s.store().clone();
    store.write(|c| { c.execute("DELETE FROM fluent_learner", [])?; Ok(()) }).unwrap();
    let slot = open(&mut s, Setup::id());
    assert_eq!(s.panel_verbs(slot).len(), 1);

    // A form with nothing in it refuses in a line rather than writing.
    setup(&mut s, slot, |p, s| p.save(s));
    assert!(model::learner(&store).is_none());
    setup(&mut s, slot, |p, _| assert!(p.error.contains("a name")));

    setup(&mut s, slot, |p, s| {
        p.name = "Vera".into();
        p.native = "Russian".into();
        p.target = "German".into();
        p.level = "a2".into();
        p.goal = "C1".into();
        p.minutes = "45".into();
        p.save(s);
    });
    let learner = model::learner(&store).expect("the row");
    assert_eq!(learner.name, "Vera");
    assert_eq!((learner.level.as_str(), learner.goal.as_str()), ("A2", "C1"), "the ladder's own spelling");
    assert_eq!(learner.daily_minutes, 45);
    assert!(
        real(&store, "SELECT started FROM fluent_learner WHERE id = 1") > 0.0,
        "the first save stamps the day the course began"
    );
    s.undo();
    assert!(model::learner(&store).is_none(), "a course with no learner again");
    s.redo();
    assert_eq!(model::learner(&store).unwrap().name, "Vera");

    // A second save leaves the streak and the start day where they are.
    store
        .write(|c| {
            c.execute("UPDATE fluent_learner SET streak = 9, started = 1000 WHERE id = 1", [])?;
            Ok(())
        })
        .unwrap();
    setup(&mut s, slot, |p, s| {
        p.minutes = "20".into();
        p.save(s);
    });
    let learner = model::learner(&store).unwrap();
    assert_eq!((learner.daily_minutes, learner.streak), (20, 9));
    assert_eq!(real(&store, "SELECT started FROM fluent_learner WHERE id = 1"), 1000.0);
    s.undo();
    assert_eq!(model::learner(&store).unwrap().daily_minutes, 45);
    // And a number nobody could study refuses.
    setup(&mut s, slot, |p, s| {
        p.minutes = "0".into();
        p.save(s);
        assert!(p.error.contains("5 and 240"));
    });
    assert_eq!(model::learner(&store).unwrap().daily_minutes, 45);
}

#[test]
fn the_full_build_seeds_the_course_too() {
    let apps = kernel::app::Apps::new(crate::APPS);
    let store = Store::open(None, &apps.schemas(), kernel::sync::Device::fake().replicating(apps.replicated())).unwrap();
    apps.seed(&store, kernel::app::Mode::Fake).unwrap();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message"), 81);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_learner"), 1);
}

// ---------------------------------------------------------------------------
// The player follows the rows
// ---------------------------------------------------------------------------

/// Answers every closed exercise right, up to the lesson's free write.
fn answer_closed(l: &mut Lesson, s: &mut Session) {
    while l.current().is_some_and(|e| !e.self_check()) {
        answer_right(l, s);
        l.advance(s);
    }
}

/// Answers the current closed exercise with its first accepted answer.
fn answer_right(l: &mut Lesson, s: &mut Session) {
    let ex = l.current().unwrap();
    let key = ex.accepted.first().cloned().unwrap();
    if ex.typed() {
        l.set_typed(key);
        l.submit(s);
    } else {
        let i = ex.shown_choices().iter().position(|c| *c == key).unwrap();
        l.choose(i, s);
    }
}

/// A lesson closed after its last answer and before **next**, reopened, is
/// its summary — with **finish** on the bar, because its row is still open
/// and nothing else can close it.
#[test]
fn a_lesson_reopened_after_its_last_answer_is_a_summary_that_can_finish() {
    let mut s = session();
    let store = s.store().clone();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        answer_closed(l, s);
        // The model answer itself needs no grade: feedback, and no next.
        let model_answer = l.current().unwrap().model;
        l.set_typed(model_answer);
        l.submit(s);
        assert!(matches!(l.phase(), Phase::Feedback(Closed::Correct)));
    });
    assert_eq!(model::lesson(&store, READY).unwrap().status, "ready");
    let again = open(&mut s, Lesson::id(READY));
    lesson(&mut s, again, |l, _| {
        assert_eq!(l.phase(), Phase::Done);
        assert_eq!(l.outcome().0, 9);
    });
    assert!(verbs(&s, again).contains(&"fluent.end"), "finish on the bar: {:?}", verbs(&s, again));
    lesson(&mut s, again, |l, s| l.run("fluent.end", s));
    let row = model::lesson(&store, READY).unwrap();
    assert_eq!((row.status.as_str(), row.accuracy), ("done", Some(1.0)));
    assert_eq!(model::shelf(&store).unwrap().status, "building");
    assert!(!verbs(&s, again).contains(&"fluent.end"), "finished: nothing left to finish");
}

/// The player follows the rows: an answer undone under its feedback is the
/// exercise to answer again, redone it is the feedback again, and a
/// self-grade undone is the grade pad — however far the player had moved.
#[test]
fn the_player_follows_an_answer_undone_or_redone_beneath_it() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    let now = s.now();
    lesson(&mut s, slot, |l, s| {
        l.choose(0, s);
        assert!(matches!(l.phase(), Phase::Feedback(_)));
    });
    assert!(s.undo());
    lesson(&mut s, slot, |l, _| {
        l.tick(now);
        assert_eq!((l.index(), l.phase()), (0, Phase::Answering));
        assert_eq!(l.choice(), None);
    });
    assert!(s.redo());
    lesson(&mut s, slot, |l, s| {
        l.tick(now);
        assert!(matches!(l.phase(), Phase::Feedback(_)), "redone: its feedback again");
        l.advance(s);
        assert_eq!((l.index(), l.phase()), (1, Phase::Answering));
    });
    // The first answer taken back from the second exercise: the first is
    // the one to answer.
    assert!(s.undo());
    lesson(&mut s, slot, |l, _| {
        l.tick(now);
        assert_eq!((l.index(), l.phase()), (0, Phase::Answering));
    });
    // Through the lesson to the grade pad, a grade and the finish — then
    // back, one step at a time.
    lesson(&mut s, slot, |l, s| {
        l.tick(now);
        answer_closed(l, s);
        l.set_typed("Ich brauche ein Reisepass und Passfoto.".into());
        l.submit(s);
        assert_eq!(l.phase(), Phase::SelfGrade);
        l.grade(3, s);
        assert_eq!(l.phase(), Phase::Done);
    });
    assert!(s.undo(), "the finish");
    lesson(&mut s, slot, |l, _| {
        l.tick(now);
        assert_eq!(l.phase(), Phase::Done, "every answer still in: the summary, open");
    });
    assert!(verbs(&s, slot).contains(&"fluent.end"), "with finish on the bar");
    assert!(s.undo(), "the self-grade");
    lesson(&mut s, slot, |l, _| {
        l.tick(now);
        assert_eq!((l.index(), l.phase()), (8, Phase::SelfGrade), "the grade pad again");
    });
}

/// A self-check answer written and never graded reopens at its grade pad,
/// with the answer on the row and the pad on the bar — never as an empty
/// field asking for the answer again.
#[test]
fn a_self_check_answer_reopens_at_its_grade_pad() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        answer_closed(l, s);
        l.set_typed("Ich brauche ein Reisepass.".into());
        l.submit(s);
        assert_eq!(l.phase(), Phase::SelfGrade);
    });
    let again = open(&mut s, Lesson::id(READY));
    lesson(&mut s, again, |l, _| {
        assert_eq!((l.index(), l.phase()), (8, Phase::SelfGrade));
        assert!(!l.can_submit());
    });
    let bar = verbs(&s, again);
    assert!(bar.contains(&"fluent.grade3"), "{bar:?}");
    assert!(!bar.contains(&"fluent.check"), "{bar:?}");
}

/// The tutor's word, where it has landed before the learner's own grade,
/// is what the items are graded at; the learner's grade stays beside it.
#[test]
fn a_self_grade_after_the_tutors_word_files_the_tutors_quality() {
    let mut s = tutored();
    let store = s.store().clone();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        answer_closed(l, s);
        l.set_typed("Ich brauche ein Reisepass.".into());
        l.submit(s);
    });
    let ex = model::exercises(&store, READY)[8].clone();
    assert_eq!(ex.tutor_grade, Some(4), "the fake tutor was quicker");
    lesson(&mut s, slot, |l, s| l.grade(1, s));
    let after = model::exercises(&store, READY)[8].clone();
    assert_eq!((after.self_grade, after.tutor_grade), (Some(1), Some(4)));
    let filed: Vec<i64> = store
        .conn()
        .prepare("SELECT quality FROM fluent_review WHERE lesson_uid = ?1 AND seq = 9")
        .unwrap()
        .query_map([&ex.lesson_uid], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(filed, vec![4, 4], "the tutor's quality, not the learner's");
}

/// A verdict that comes back for words that are no longer the answer —
/// taken back, or given again as something else — is dropped.
#[test]
fn a_late_verdict_on_words_no_longer_the_answer_is_dropped() {
    let mut s = tutored();
    let store = s.store().clone();
    store
        .write(move |c| {
            c.execute(
                "UPDATE fluent_exercise SET tutor_grade = NULL, tutor_note = '', tutor_fix = '' WHERE lesson = ?1 AND seq = 8",
                [LAST_DONE],
            )?;
            Ok(())
        })
        .unwrap();
    let ex = model::exercises(&store, LAST_DONE)[7].clone();
    super::tutor::grade(&mut s, &ex, "words that were taken back");
    assert_eq!(gateway(&s).requests().len(), 1, "asked");
    assert_eq!(model::exercises(&store, LAST_DONE)[7].tutor_grade, None, "and dropped");
    assert!(!said(&s).contains("tutor"), "{}", said(&s));
}

/// A listening exercise's words stay off the screen while it is answered:
/// play says it is speaking and no more, and the transcript is for after.
#[test]
fn playing_a_listening_exercise_keeps_its_words_off_the_screen() {
    let mut s = session();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        while l.current().is_some_and(|e| e.kind != "listen_mcq") {
            answer_right(l, s);
            l.advance(s);
        }
        let ex = l.current().expect("the listening exercise");
        l.play(s);
        assert!(!said(s).contains(&ex.audio), "{}", said(s));
        assert!(said(s).contains("speaking — the words show once the answer is in"), "{}", said(s));
        l.choose(0, s);
        l.play(s);
        assert!(said(s).contains(&format!("speaking: „{}“", ex.audio)), "answered: the words may show");
    });
}

/// A lesson's minutes are the seconds its exercises took, each one cut at
/// half an hour: a panel left standing overnight is not a night's study.
#[test]
fn minutes_are_the_time_the_exercises_took() {
    let mut s = session();
    let store = s.store().clone();
    let slot = open(&mut s, Lesson::id(READY));
    let mut t = s.now();
    lesson(&mut s, slot, |l, s| {
        for n in 0..8 {
            t += if n == 3 { DAY } else { 40.0 };
            l.tick(t);
            answer_right(l, s);
            l.advance(s);
        }
        t += 40.0;
        l.tick(t);
        let model_answer = l.current().unwrap().model;
        l.set_typed(model_answer);
        l.submit(s);
        l.advance(s);
        assert_eq!(l.phase(), Phase::Done);
    });
    // Eight of forty seconds and one of a day, cut at thirty minutes.
    assert_eq!(model::lesson(&store, READY).unwrap().minutes, Some(35.0));
}

/// Activity is credited to the day a lesson was finished, which is the day
/// the streak was fed — a stale lesson played anyway is today's work.
#[test]
fn activity_counts_a_lesson_on_the_day_it_was_finished() {
    let mut s = session();
    let store = s.store().clone();
    let stale = day_start(s.now()) - 3.0 * DAY;
    store
        .write(move |c| {
            c.execute("UPDATE fluent_lesson SET for_date = ?2 WHERE id = ?1", rusqlite::params![READY, stale])?;
            Ok(())
        })
        .unwrap();
    let before = model::activity(&store, s.now());
    let slot = open(&mut s, Lesson::id(READY));
    let later = s.now() + 60.0;
    lesson(&mut s, slot, |l, s| {
        l.tick(later);
        l.choose(0, s);
        l.advance(s);
        l.finish(s);
    });
    let after = model::activity(&store, s.now());
    assert!(after[55] > before[55], "today: {} → {}", before[55], after[55]);
    assert_eq!(after[52], before[52], "not the day it was written for");
}

// ---------------------------------------------------------------------------
// The schedule's base
// ---------------------------------------------------------------------------

/// Two grades given in one instant on two devices replay in the same order
/// on every device: by the device's name, never by this device's row ids.
#[test]
fn grades_in_one_instant_replay_in_the_devices_order() {
    let stand = |first: &'static str, second: &'static str| -> model::Item {
        let mut s = session();
        s.settle();
        let store = s.store().clone();
        let at = s.now() - 3600.0;
        store
            .write(move |c| {
                for device in [first, second] {
                    let quality = if device == "desk" { 5 } else { 0 };
                    c.execute(
                        "INSERT INTO fluent_review(item, at, quality, device) VALUES('vocab_der_vermieter', ?1, ?2, ?3)",
                        rusqlite::params![at, quality, device],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        s.settle();
        model::item_state(&store, "vocab_der_vermieter")
    };
    let a = stand("desk", "phone");
    let b = stand("phone", "desk");
    assert_eq!(a, b);
    assert_eq!(a.reps, 0, "desk's 5 first, phone's 0 after it: a miss last");
}

/// A grade another device took back leaves at the next poll too, and an
/// item with none left stands where it began — its base — not where the
/// last grade left it.
#[test]
fn a_grade_taken_away_by_sync_puts_the_item_back_where_it_began() {
    let mut s = session();
    s.settle();
    let store = s.store().clone();
    let began = model::item_state(&store, "vocab_der_vermieter");
    assert_eq!(began.reps, 0, "a word yesterday's lesson introduced, never graded");
    let at = s.now() - 3600.0;
    store
        .write(move |c| {
            c.execute(
                "INSERT INTO fluent_review(item, at, quality, device) VALUES('vocab_der_vermieter', ?1, 5, 'phone')",
                [at],
            )?;
            Ok(())
        })
        .unwrap();
    s.settle();
    assert_eq!(model::item_state(&store, "vocab_der_vermieter").reps, 1);
    store
        .write(|c| {
            c.execute("DELETE FROM fluent_review WHERE item = 'vocab_der_vermieter'", [])?;
            Ok(())
        })
        .unwrap();
    s.settle();
    assert_eq!(model::item_state(&store, "vocab_der_vermieter"), began);
}

// ---------------------------------------------------------------------------
// What authoring leaves behind
// ---------------------------------------------------------------------------

/// Every item an exercise grades into is on the schedule once the lesson
/// is: a rule the call names and nothing else in it makes is created as a
/// grammar item, named after the topic that lists it or after its slug.
#[test]
fn authoring_puts_the_rules_it_names_on_the_schedule() {
    let mut s = session();
    let apps = kernel::app::Apps::new(APPS);
    let author = apps.tool("fluent.author").unwrap();
    let store = s.store().clone();
    let out = (author.run)(
        &mut s,
        &serde_json::json!({
            "title": "Weil und dass",
            "exercises": [
                {"section": "new", "kind": "mcq", "grading": "closed", "prompt": "Ich bleibe zu Hause, ___ ich krank bin.",
                 "choices": ["weil", "dass"], "accepted": ["weil"], "items": ["nebensatz_weil", "article_gender"]},
                {"section": "cooldown", "kind": "cloze", "grading": "closed", "prompt": "Er sagt, ___ er kommt.",
                 "accepted": ["dass"], "items": ["nebensatz_dass"]}
            ],
            "topics": [{"id": "nebensaetze-weil", "title": "Nebensätze mit weil", "category": "sentence_structure",
                "summary": "Das Verb steht am Ende.", "items": ["nebensatz_weil"]}]
        }),
    )
    .unwrap();
    assert_eq!(out["exercises"], 2);
    let item = |id: &str| -> Option<(String, String)> {
        store
            .conn()
            .query_row("SELECT kind, content FROM fluent_item WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok()
    };
    assert_eq!(item("nebensatz_weil"), Some(("grammar".into(), "Nebensätze mit weil".into())), "named after its topic");
    assert_eq!(item("nebensatz_dass"), Some(("grammar".into(), "nebensatz dass".into())), "named after its slug");
    assert_eq!(item("article_gender").map(|i| i.0), Some("grammar".into()), "one the schedule had is left alone");
    assert_eq!(model::item_state(&store, "nebensatz_weil").due, day_start(s.now()), "due today");
    s.undo();
    assert_eq!(item("nebensatz_weil"), None);
    assert_eq!(item("nebensatz_dass"), None);
    assert!(item("article_gender").is_some());

    // More choices than the player has rows for is refused whole.
    let too_many = (author.run)(
        &mut s,
        &serde_json::json!({
            "title": "Zu viele",
            "exercises": [{"section": "new", "kind": "mcq", "grading": "closed", "prompt": "?",
                "choices": ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"], "accepted": ["a"], "items": ["article_gender"]}]
        }),
    )
    .unwrap_err();
    assert_eq!(too_many, "exercise 1: at most 9 choices — one per digit key");
}

/// The tutor's notes are on the lesson they are about — the one just
/// played — and one undo puts back what stood there.
#[test]
fn the_tutors_notes_land_on_the_lesson_they_are_about() {
    let mut s = session();
    let apps = kernel::app::Apps::new(APPS);
    let author = apps.tool("fluent.author").unwrap();
    let store = s.store().clone();
    let had = model::lesson(&store, LAST_DONE).unwrap().notes;
    assert!(!had.is_empty(), "the seeded lesson carries notes");
    let notes = "Gut: die Artikel sitzen. Übe den Dativ nach mit.";
    let out = (author.run)(
        &mut s,
        &serde_json::json!({
            "title": "Morgen", "finished": LAST_DONE, "notes": notes,
            "exercises": [{"section": "warmup", "kind": "mcq", "grading": "closed", "prompt": "?",
                "choices": ["a", "b"], "accepted": ["a"], "items": ["article_gender"]}]
        }),
    )
    .unwrap();
    let new = out["lesson"].as_i64().unwrap();
    assert_eq!(model::lesson(&store, LAST_DONE).unwrap().notes, notes);
    assert_eq!(model::lesson(&store, new).unwrap().notes, "", "and not on tomorrow's");
    s.undo();
    assert_eq!(model::lesson(&store, LAST_DONE).unwrap().notes, had);
    s.redo();
    assert_eq!(model::lesson(&store, LAST_DONE).unwrap().notes, notes);
}

/// Extending a topic keeps every section that differs in any part — a
/// second block of examples, a table with new rows — and drops only an
/// exact repeat.
#[test]
fn extending_a_topic_keeps_new_examples_and_drops_only_exact_repeats() {
    let mut s = session();
    let apps = kernel::app::Apps::new(APPS);
    let author = apps.tool("fluent.author").unwrap();
    let store = s.store().clone();
    let sections =
        |store: &Store| model::sections(&model::topic(store, "artikel-nom-akk-dat").unwrap().sections).len();
    let had = sections(&store);
    let extend = |s: &mut Session, items: serde_json::Value| {
        (author.run)(
            s,
            &serde_json::json!({
                "title": "Artikel",
                "exercises": [{"section": "warmup", "kind": "mcq", "grading": "closed", "prompt": "?",
                    "choices": ["a", "b"], "accepted": ["a"], "items": ["article_gender"]}],
                "topics": [{"id": "artikel-nom-akk-dat", "title": "Artikel: Nominativ, Akkusativ, Dativ",
                    "category": "cases", "summary": "Wie der/die/das sich nach Fall verändern.",
                    "sections": [{"kind": "examples", "items": items}]}]
            }),
        )
        .unwrap();
    };
    extend(&mut s, serde_json::json!([{"text": "Ich sehe den Mann.", "note": "Akkusativ"}]));
    assert_eq!(sections(&store), had + 1);
    extend(&mut s, serde_json::json!([{"text": "Ich helfe dem Mann.", "note": "Dativ"}]));
    assert_eq!(sections(&store), had + 2, "a second block of examples is not the first");
    extend(&mut s, serde_json::json!([{"text": "Ich helfe dem Mann.", "note": "Dativ"}]));
    assert_eq!(sections(&store), had + 2, "the same block twice is once");
}

/// A tutor's verdict on a lesson already finished stamps its accuracy
/// again, so the summary and the history say the same thing.
#[test]
fn a_verdict_on_a_finished_lesson_restamps_its_accuracy() {
    let mut s = session();
    let store = s.store().clone();
    let ex = model::exercises(&store, LAST_DONE)[7].clone();
    assert_eq!(model::lesson(&store, LAST_DONE).unwrap().accuracy, Some(0.75));
    assert!(model::tutor_grade(&mut s, ex.id, 1, "Noch nicht.", ""));
    assert_eq!(model::lesson(&store, LAST_DONE).unwrap().accuracy, Some(0.625), "one fewer right of eight");
    s.undo();
    assert_eq!(model::lesson(&store, LAST_DONE).unwrap().accuracy, Some(0.75));
}

// ---------------------------------------------------------------------------
// Undo and redo, in any order, name the same rows
// ---------------------------------------------------------------------------

/// A lesson authored, answered, both undone and both redone: the exercise
/// comes back under the id it had, so the answer finds its row.
#[test]
fn redoing_an_authored_lesson_and_its_answer_keeps_the_answer() {
    let mut s = session();
    let apps = kernel::app::Apps::new(APPS);
    let author = apps.tool("fluent.author").unwrap();
    let store = s.store().clone();
    let out = (author.run)(
        &mut s,
        &serde_json::json!({
            "title": "Nochmal", "for_date": "2026-09-15",
            "exercises": [{"section": "warmup", "kind": "mcq", "grading": "closed", "prompt": "der, die oder das Balkon?",
                "choices": ["der", "die", "das"], "accepted": ["der"], "items": ["article_gender"]}]
        }),
    )
    .unwrap();
    let id = out["lesson"].as_i64().unwrap();
    let ex = model::exercises(&store, id)[0].clone();
    let patch = Patch {
        answer: Some("der".into()),
        result: Some(Closed::Correct),
        quality: Some(5),
        ..Patch::default()
    };
    assert!(model::record(&mut s, &ex, patch, "answer Q1"));
    assert_eq!(model::exercises(&store, id)[0].answer.as_deref(), Some("der"));
    assert!(s.undo(), "the answer");
    assert!(s.undo(), "the lesson");
    assert!(model::lesson(&store, id).is_none());
    assert!(s.redo(), "the lesson");
    assert!(s.redo(), "the answer");
    let again = model::exercises(&store, id)[0].clone();
    assert_eq!(again.id, ex.id, "the same row");
    assert_eq!((again.answer.as_deref(), again.result.as_deref()), (Some("der"), Some("correct")));
    assert_eq!(
        count(&store, &format!("SELECT COUNT(*) FROM fluent_review WHERE lesson_uid = '{}' AND seq = 1", ex.lesson_uid)),
        1,
        "one grade, filed once"
    );
}

/// A finish and the lesson authored over its placeholder, both undone and
/// both redone: the placeholder comes back as itself, the authored lesson
/// takes it off the shelf, and the shelf holds the one ready lesson.
#[test]
fn redoing_a_finish_and_the_next_lesson_leaves_one_lesson_on_the_shelf() {
    let mut s = session();
    let apps = kernel::app::Apps::new(APPS);
    let author = apps.tool("fluent.author").unwrap();
    let store = s.store().clone();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        l.choose(0, s);
        l.advance(s);
        l.finish(s);
    });
    let placeholder = model::shelf(&store).unwrap();
    assert_eq!(placeholder.status, "building");
    let out = (author.run)(
        &mut s,
        &serde_json::json!({
            "title": "Übermorgen",
            "exercises": [{"section": "warmup", "kind": "mcq", "grading": "closed", "prompt": "?",
                "choices": ["a", "b"], "accepted": ["a"], "items": ["article_gender"]}]
        }),
    )
    .unwrap();
    let next = out["lesson"].as_i64().unwrap();
    assert_eq!(model::shelf(&store).unwrap().id, next);
    assert!(s.undo(), "the lesson");
    assert!(s.undo(), "the finish");
    assert!(s.redo(), "the finish");
    assert_eq!(model::shelf(&store).unwrap().id, placeholder.id, "the same placeholder");
    assert!(s.redo(), "the lesson");
    assert_eq!(model::shelf(&store).unwrap().id, next, "the ready lesson, alone");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_lesson WHERE status = 'building'"), 0);
}

/// A self-grade and the tutor's verdict over it, both undone and both
/// redone: the grades come back under their own ids, so the redone verdict
/// still rewrites them.
#[test]
fn redoing_a_self_grade_and_the_tutors_verdict_moves_the_grades() {
    let mut s = session();
    let store = s.store().clone();
    let slot = open(&mut s, Lesson::id(READY));
    lesson(&mut s, slot, |l, s| {
        answer_closed(l, s);
        l.set_typed("Ich brauche ein Reisepass.".into());
        l.submit(s);
        l.grade(1, s);
    });
    let ex = model::exercises(&store, READY)[8].clone();
    let filed = |store: &Store| -> Vec<i64> {
        store
            .conn()
            .prepare("SELECT quality FROM fluent_review WHERE lesson_uid = ?1 AND seq = 9 ORDER BY item")
            .unwrap()
            .query_map([&ex.lesson_uid], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(filed(&store), vec![1, 1]);
    assert!(model::tutor_grade(&mut s, ex.id, 5, "Perfekt.", ""));
    assert_eq!(filed(&store), vec![5, 5]);
    assert!(s.undo(), "the verdict");
    assert!(s.undo(), "the finish");
    assert!(s.undo(), "the self-grade");
    assert_eq!(filed(&store), Vec::<i64>::new());
    assert!(s.redo());
    assert!(s.redo());
    assert!(s.redo());
    assert_eq!(filed(&store), vec![5, 5], "the redone verdict found its rows");
    assert_eq!(model::exercises(&store, READY)[8].tutor_grade, Some(5));
    assert!(same(&model::item_state(&store, "writing_official"), &replayed(&store, "writing_official")));
}

/// An item that arrives by sync with a base and no grades at all is that
/// base at the next poll — not a fresh card overdue since the epoch.
#[test]
fn an_items_base_arriving_by_sync_is_replayed_at_the_next_poll() {
    let mut s = session();
    s.settle();
    let store = s.store().clone();
    let due = day_start(s.now()) + 12.0 * DAY;
    let base = model::base_json(&Replay {
        state: sm2::State { ease: 2.36, interval: 15, reps: 3 },
        due,
        reviewed: Some(due - 15.0 * DAY),
        mastery: 3,
    });
    store
        .write(move |c| {
            c.execute(
                "INSERT INTO fluent_item(id, kind, content, created, base) VALUES('vocab_der_flur', 'vocab', 'der Flur', 0, ?1)",
                [base],
            )?;
            Ok(())
        })
        .unwrap();
    s.settle();
    let it = model::item_state(&store, "vocab_der_flur");
    assert_eq!((it.ease, it.interval, it.reps, it.due, it.mastery), (2.36, 15, 3, due, 3));
}

/// A flashcard grade undone puts the card back in the sitting: the queue
/// stands, the card is the one to grade again, and the score is what the
/// grades still filed add up to.
#[test]
fn undoing_a_flashcard_grade_puts_the_card_back_in_the_sitting() {
    let mut s = session();
    let slot = open(&mut s, Review::id());
    let now = s.now();
    let total = review(&mut s, slot, |r, _| r.total());
    assert!(total >= 3);
    review(&mut s, slot, |r, s| {
        r.flip(s);
        r.grade(5, s);
        r.flip(s);
        r.grade(2, s);
        assert_eq!((r.pos(), r.outcome().1), (2, 1));
    });
    assert!(s.undo(), "the second grade");
    review(&mut s, slot, |r, _| {
        r.tick(now);
        assert_eq!((r.pos(), r.outcome().1, r.flipped()), (1, 1, false), "the second card again");
    });
    assert!(s.undo(), "the first grade");
    review(&mut s, slot, |r, _| {
        r.tick(now);
        assert_eq!((r.pos(), r.outcome().1), (0, 0));
        assert_eq!(r.total(), total, "the queue stands");
    });
    assert!(s.redo());
    review(&mut s, slot, |r, s| {
        r.tick(now);
        assert_eq!((r.pos(), r.outcome().1), (1, 1));
        // To the end, and the last one back: the sitting is open again.
        while r.current().is_some() {
            r.flip(s);
            r.grade(4, s);
        }
        assert!(r.finished());
    });
    assert!(s.undo());
    review(&mut s, slot, |r, _| {
        r.tick(now);
        assert!(!r.finished());
        assert_eq!(r.pos(), total - 1);
    });
}

/// The rows a question's choices are shown in are dealt by the exercise's
/// own name: the same deal on every device and every reopening, a
/// permutation of what was written, and not the written order — whoever
/// writes a question tends to write the right answer first.
#[test]
fn choices_are_dealt_the_same_way_every_time_and_not_as_written() {
    let s = session();
    let store = s.store().clone();
    let mut first = 0;
    let mut asked = 0;
    for ex in model::exercises(&store, READY).iter().filter(|e| !e.choices.is_empty()) {
        let dealt = ex.shown_choices();
        assert_eq!(dealt.len(), ex.choices.len());
        let mut sorted = dealt.clone();
        sorted.sort();
        let mut written = ex.choices.clone();
        written.sort();
        assert_eq!(sorted, written, "a permutation of what was written");
        assert_eq!(dealt, ex.shown_choices(), "the same deal again");
        asked += 1;
        if dealt[0] == ex.accepted[0] {
            first += 1;
        }
    }
    assert!(asked >= 4);
    assert!(first < asked, "the right answer is not always the first row");
    // A different question is a different deal.
    let exs = model::exercises(&store, READY);
    let (a, b) = (&exs[0], &exs[3]);
    assert_ne!(a.order(), b.order());
}
