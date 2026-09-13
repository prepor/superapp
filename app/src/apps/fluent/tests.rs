use super::model::{self, Closed, Patch};
use super::panels::{Card, Cards, Desk, Grammar, History, Lesson, Phase, Progress, Review, Topic};
use super::seed::{uid, FIRST_DUE, LAST_DONE, READY};
use super::sm2::{self, Replay, DAY};
use super::{tools, FLUENT};
use kernel::app::App;
use kernel::layout::SlotId;
use kernel::panel::PanelId;
use kernel::richtable::Datasource;
use kernel::session::{Action, Session};
use kernel::store::Store;

static APPS: &[&dyn App] = &[&FLUENT];

fn session() -> Session {
    Session::fake(APPS)
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

fn review<R>(s: &mut Session, slot: SlotId, f: impl FnOnce(&mut Review, &mut Session) -> R) -> R {
    let panel = s.panel(slot).unwrap();
    let mut borrow = panel.borrow_mut();
    f(borrow.as_any().downcast_mut::<Review>().unwrap(), s)
}

fn count(store: &Store, sql: &str) -> i64 {
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
                                    let i = ex.choices.iter().position(|c| *c == key).unwrap();
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
        l.choose(1, s);
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
                let i = ex.choices.iter().position(|c| *c == key).unwrap();
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

#[test]
fn the_full_build_seeds_the_course_too() {
    let apps = kernel::app::Apps::new(crate::APPS);
    let store = Store::open(None, &apps.schemas(), kernel::sync::Device::fake().replicating(apps.replicated())).unwrap();
    apps.seed(&store, kernel::app::Mode::Fake).unwrap();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message"), 81);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM fluent_learner"), 1);
}
