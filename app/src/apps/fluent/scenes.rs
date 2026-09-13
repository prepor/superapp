//! Fluent's entries for the panels library.
//!
//! Every node is a stage solo on one of the course's panels, over a store
//! with the demo course in it. Where a state is deeper into a lesson than
//! a script should walk, the node's `open` answers the exercises before
//! it on the mount's own store — a picture of the state, not a race to it.

use kernel::panel::PanelId;
use kernel::scene::Scene;
use kernel::store::Store;
use kernel::time::virtual_epoch;

use crate::shell::app_ui::Setup;
use crate::shell::catalog::{panel, panel_fake, workspace_on};

use super::panels::{Card, Cards, Desk, Grammar, History, Import, Lesson, Progress, Review, Topic};
use super::seed::{LAST_DONE, READY};
use super::sm2::DAY;

/// Fluent's scenes, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![desk(), lesson(), review(), cards(), grammar(), progress(), import()]
}

/// Answers every exercise of today's lesson before `seq` correctly, so
/// the player opens on that one.
fn skip_to(store: &Store, seq: i64) {
    let now = virtual_epoch();
    let _ = store.write(move |c| {
        c.execute(
            "UPDATE fluent_exercise SET answer = COALESCE(json_extract(accepted, '$[0]'), model), result = 'correct',
                    answered = ?2, elapsed = 40
              WHERE lesson = ?1 AND seq < ?3",
            rusqlite::params![READY, now, seq],
        )?;
        c.execute("UPDATE fluent_lesson SET started = ?2 WHERE id = ?1", rusqlite::params![READY, now])?;
        Ok(())
    });
}

fn at(seq: i64, script: &str) -> Setup {
    panel(move |store| {
        skip_to(store, seq);
        Lesson::id(READY)
    }, script)
}

fn desk() -> Scene<Setup> {
    let shelf = |sql: &'static str| {
        panel(move |store| {
            let _ = store.write(move |c| c.execute_batch(sql));
            Desk::id()
        }, "")
    };
    Scene::new("fluent desk", (520.0, 520.0))
        .note("The course's desk: the greeting and the streak, the lesson on the shelf, and three tiles — what is due, how the last lessons went, the words.")
        .note("The shelf is one of four things: today's lesson ready, one still being built by the tutor, one built for a day long gone, or nothing at all.")
        .node("ready", panel(|_| Desk::id(), ""))
        .about("today's lesson on the shelf, and start on the bar")
        .node("started", panel(|store| { skip_to(store, 4); Desk::id() }, ""))
        .about("a lesson left half-way resumes: continue on the bar")
        .node("building", shelf("UPDATE fluent_lesson SET status = 'building', title = '' WHERE status = 'ready'"))
        .about("the tutor is at work; nothing to start yet")
        .node("stale", shelf("UPDATE fluent_lesson SET for_date = for_date - 5 * 86400 WHERE status = 'ready'"))
        .about("built for a day five days gone: build fresh, or play it anyway")
        .node("empty", shelf("UPDATE fluent_lesson SET status = 'done' WHERE status = 'ready'"))
        .about("no lesson at all: build is the one wait in fluent")
        .edge("ready", "started", "start, answer three")
        .edge("ready", "building", "the tutor is called")
        .edge("ready", "stale", "five days pass")
}

fn lesson() -> Scene<Setup> {
    Scene::new("fluent lesson", (620.0, 680.0))
        .note("One exercise at a time: the caption says the section and the kind, the hairline says how far, the prompt is the hero. A choice answers to its digit; a field answers to enter; the bar carries the rest.")
        .note("A closed exercise is graded on the spot. A free answer shows the model answer and takes the learner's own grade from the bar — the tutor's word comes later, through fluent.grade.")
        .note("Live — enter a node and play on.")
        .node("choose", at(1, ""))
        .about("a multiple choice: the digit in the box is the key")
        .node("right", at(1, "key 1\nwait 500"))
        .about("Richtig! — the answer marked, the explanation under it, next on the bar")
        .node("cloze", at(2, ""))
        .about("a gap to fill: the caret is in the field")
        .node("hint", at(3, "click \"hint\"\nwait 300\nclick \"hint\"\nwait 400"))
        .about("hints unfold one at a time, from the bar")
        .node("wrong", at(3, "wait 300\ntype \"der Gebühr\"\nwait 300\nkey enter\nwait 500"))
        .about("Nicht ganz. — yours, the arrow, the right one, and why")
        .node("listen", at(5, ""))
        .about("a listening: play on the bar, and the prompt withheld until the answer")
        .node("set piece", at(7, ""))
        .about("a passage in a box, and questions about it under it")
        .node("free write", at(9, ""))
        .about("a free answer: an editor, enter checks, shift+enter breaks a line")
        .node(
            "self-grade",
            at(9, "wait 300\ntype \"Ich brauche mein Reisepass und ein Passfoto.\"\nwait 300\nkey enter\nwait 500"),
        )
        .about("the model answer beside yours, and the six grades on the bar")
        .node("summary", panel(|_| Lesson::id(LAST_DONE), ""))
        .about("after the last exercise the panel is the summary: the numbers, the corrections, the tutor's grade beside yours")
        .node(
            "ask the tutor",
            workspace_on(|_| Lesson::id(LAST_DONE), "click \"ask\"\nwait 900"),
        )
        .sized((1200.0, 700.0))
        .about("ask on the bar: a chat joined to the lesson, carrying it as a chip — the agent app's own panel, not one of this app's")
        .edge("choose", "right", "1")
        .edge("right", "cloze", "enter")
        .edge("cloze", "hint", "hint ×2")
        .edge("cloze", "wrong", "der Gebühr, enter")
        .edge("free write", "self-grade", "enter")
        .edge("self-grade", "summary", "4 good")
        .edge("summary", "ask the tutor", "ask")
}

fn review() -> Scene<Setup> {
    let one_due = |script: &str| {
        panel(move |store| {
            let later = virtual_epoch() + 3.0 * DAY;
            let _ = store.write(move |c| {
                c.execute(
                    "UPDATE fluent_item SET due = ?1 WHERE kind = 'vocab' AND id <> 'vocab_die_gebuehr'",
                    [later],
                )?;
                Ok(())
            });
            Review::id()
        }, script)
    };
    Scene::new("fluent review", (480.0, 620.0))
        .note("The flashcards due today: the word on the front, show turns it, and the six grades on the bar file a review and move the card's next date.")
        .note("One thumb on a phone: the card is display, everything that acts is on the bar at the foot.")
        .node("front", panel(|_| Review::id(), ""))
        .about("the word alone; play speaks it")
        .node("back", panel(|_| Review::id(), "key enter\nwait 400"))
        .about("the meaning, the example, the note — and the grade pad")
        .node("graded", panel(|_| Review::id(), "key enter\nwait 300\nkey 4\nwait 500"))
        .about("4 good: the next card is up, the last one filed")
        .node("finished", one_due("key enter\nwait 300\nkey 5\nwait 500"))
        .about("Alles erledigt! — the count, the minutes, and where the grades went")
        .node(
            "nothing due",
            panel(|store| {
                let later = virtual_epoch() + 3.0 * DAY;
                let _ = store.write(move |c| {
                    c.execute("UPDATE fluent_item SET due = ?1 WHERE kind = 'vocab'", [later])?;
                    Ok(())
                });
                Review::id()
            }, ""),
        )
        .about("nothing due says when the next card comes")
        .edge("front", "back", "show / enter")
        .edge("back", "graded", "4")
        .edge("graded", "finished", "…the last card")
}

fn cards() -> Scene<Setup> {
    Scene::new("fluent cards", (560.0, 640.0))
        .note("The deck as a rich table: the word and its meaning, when it comes due, its mastery. The cursor previews the card.")
        .node("deck", panel(|_| Cards::id(), ""))
        .about("soonest due first; new cards say so")
        .node("new only", panel(|_| PanelId::new(Cards::TAG, ["@new"]), ""))
        .about("@new — the three words yesterday's lesson introduced")
        .node("card", panel(|_| Card::id("vocab_die_gebuehr"), ""))
        .sized((480.0, 460.0))
        .about("one card whole: the word, the meaning, the example, the note, its schedule, and every grade it was given")
        .edge("deck", "new only", "@new")
        .edge("deck", "card", "↓ / enter")
}

fn grammar() -> Scene<Setup> {
    Scene::new("fluent grammar", (560.0, 640.0))
        .note("The grammar the tutor has written up so far, by category, each rule with its level and a mastery stamp.")
        .node("topics", panel(|_| Grammar::id(), ""))
        .about("grouped by category; @level and @category narrow it")
        .node("topic", panel(|_| Topic::id("artikel-nom-akk-dat"), ""))
        .sized((560.0, 760.0))
        .about("the rule, its tables in the mono face, examples, a tip, the learner's own stumbles, and the related topics as links that replace the panel")
        .node("with a tip", panel(|_| Topic::id("indefinitpronomen-man"), ""))
        .sized((560.0, 700.0))
        .about("the topic the tutor wrote after one stumble: »Die man …«")
        .edge("topics", "topic", "↓ / enter")
        .edge("topic", "with a tip", "related")
}

fn progress() -> Scene<Setup> {
    Scene::new("fluent progress", (560.0, 760.0))
        .note("Where the learner stands: the streak, eight weeks of days, mastery per skill, the last ten lessons' accuracy as bars, the errors that keep coming back, and the words.")
        .node("progress", panel(|_| Progress::id(), ""))
        .about("nothing here is edited: every line is read off the rows")
        .node("history", panel(|_| History::id(), ""))
        .sized((560.0, 520.0))
        .about("every lesson played, newest first; the cursor previews its summary")
        .edge("progress", "history", "history")
}

/// The one panel of the course that reads something outside the store, so
/// its node is mounted on the fake outside: the demo tree has no course
/// folder in it, which is exactly the answer this scene is about.
fn import() -> Scene<Setup> {
    Scene::new("fluent import", (520.0, 400.0))
        .note("The course as it was kept before: a folder of the original's JSON notebooks, read in once. The field opens on where that folder is on this machine; import reads it and writes it as one undoable action.")
        .node("form", panel_fake(|_| Import::id(), ""))
        .about("the path the original keeps, and import on the bar")
        .node("no folder", panel_fake(|_| Import::id(), "click \"import\"\nwait 700"))
        .about("nothing at that path: the reader says so in the shell's red")
        .edge("form", "no folder", "import")
}
