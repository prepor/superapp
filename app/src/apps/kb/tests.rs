use super::markdown::{self, LinkKind};
use super::model::{self, Target, Where};
use super::panels::{Catalogue, Edit, File, History, Import, Page, Revision};
use super::seed::{self, hash_of, uid_of};
use super::KB;
use crate::apps::agent::{Chat, AGENT};
use kernel::app::{App, Apps, Mode};
use kernel::layout::SlotId;
use kernel::panel::PanelId;
use kernel::richtable::ListState;
use kernel::session::{Action, Session};
use kernel::store::Store;

static APPS: &[&dyn App] = &[&KB, &AGENT];
static ALONE: &[&dyn App] = &[&KB];

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

/// Presses one of a panel's own verbs, as the bar would: with the panel
/// borrowed for the length of the call, which is what a verb that reaches
/// back into its panel trips over.
fn run(s: &mut Session, slot: SlotId, verb: &str) {
    let panel = s.panel(slot).unwrap();
    panel.borrow_mut().run(verb, s);
    s.settle();
}

fn count(store: &Store, sql: &str) -> i64 {
    store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn rows(store: &Store, filter: &str) -> Vec<String> {
    let mut list = ListState::new(&model::PAGES, 50);
    list.set_filter(filter);
    let n = list.len(store);
    list.rows(store, 0, n).into_iter().map(|r| r.slug).collect()
}

/// A page's `kb_link` rows: target, kind, and what the row says it resolved to.
fn link_rows(store: &Store, uid: &str) -> Vec<(String, String, String)> {
    store
        .conn()
        .prepare("SELECT target, kind, resolved FROM kb_link WHERE page = ?1 ORDER BY rowid")
        .unwrap()
        .query_map([uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn body(store: &Store, uid: &str) -> String {
    model::page_by_uid(store, uid).map(|p| p.body).unwrap_or_default()
}

fn revisions(store: &Store, uid: &str) -> usize {
    model::revisions(store, uid).len()
}

/// Every bar in the app over the seeded wiki: no reserved letter, no letter
/// twice, and the letters CR-022 names on each.
#[test]
fn every_bar_wears_distinct_unreserved_letters() {
    let mut s = session();
    let ids = vec![
        Catalogue::id(),
        Catalogue::filtered("@kind:skill"),
        Page::id("porto-lume"),
        Page::id("filing-a-source"),
        Edit::id("lamp-build"),
        Edit::new_id(),
        History::id("lamp-build"),
        Revision::id(&seed::restorable_revision()),
        File::id(seed::PDF),
        File::id(seed::TEXT),
        Import::id(),
    ];
    for id in ids {
        let slot = open(&mut s, id.clone());
        let verbs = s.panel_verbs(slot);
        let mut seen = Vec::new();
        for v in &verbs {
            if let Some(c) = v.accel {
                assert!(!crate::shell::keys::is_reserved(c), "{}: {c} is reserved", v.id);
                assert!(!seen.contains(&c), "{}: {c} twice on {id}", v.id);
                // CR-022 gives lint `k`, since `l` is the shell's: the one
                // letter on these bars its label does not carry.
                assert!(v.label.contains(c) || v.id == "kb.lint", "{}: the letter is in the label", v.id);
                seen.push(c);
            }
        }
    }
    let letters = |id: PanelId| -> Vec<(String, Option<char>)> {
        let mut s2 = session();
        let slot = open(&mut s2, id);
        s2.panel_verbs(slot).iter().map(|v| (v.label.clone(), v.accel)).collect()
    };
    assert_eq!(
        letters(Catalogue::id()),
        [("ask", Some('a')), ("new page", Some('n')), ("lint", Some('k')), ("import", Some('m'))]
            .map(|(l, a)| (l.to_string(), a))
    );
    assert_eq!(
        letters(Page::id("porto-lume")),
        [("ask", Some('a')), ("edit", Some('e')), ("history", Some('h')), ("rename", Some('r')), ("delete", Some('d'))]
            .map(|(l, a)| (l.to_string(), a))
    );
    assert_eq!(letters(Page::id("filing-a-source"))[0], ("use".to_string(), Some('s')));
    assert_eq!(letters(Revision::id(&seed::restorable_revision())), [("restore".to_string(), Some('r'))]);
    assert_eq!(letters(Import::id()), [("browse".to_string(), Some('b')), ("import".to_string(), Some('m'))]);
    let cached = letters(File::id(seed::PDF));
    assert_eq!(cached[0], ("ask".to_string(), Some('a')));
    assert_eq!(cached[1], ("open".to_string(), Some('o')));
}

/// Review 1: ask, lint and use ran with their own panel borrowed and
/// reached back into it — `RefCell already borrowed`. Each is pressed the
/// way the bar presses it, and each opens its chat.
#[test]
fn ask_lint_and_use_open_a_chat_from_a_borrowed_panel() {
    let chat_focused = |s: &Session| {
        let slot = s.focus().unwrap();
        s.panel(slot).unwrap().borrow().id().tag == Chat::TAG
    };
    // ask on the catalogue: a blank chat joined to it, with its chip.
    let mut s = session();
    let slot = open(&mut s, Catalogue::id());
    run(&mut s, slot, "kb.ask");
    assert!(chat_focused(&s), "ask opens a chat");
    assert!(!s.showing(&Chat::new_id()).is_empty());
    // lint on the catalogue: a chat with the first turn written.
    let mut s = session();
    let slot = open(&mut s, Catalogue::id());
    run(&mut s, slot, "kb.lint");
    assert!(chat_focused(&s), "lint opens a chat");
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM agent_turn WHERE role = 'user'"), 1);
    assert!(count(s.store(), "SELECT COUNT(*) FROM agent_chat WHERE title LIKE 'run kb.lint%'") == 1);
    // use on a skill page.
    let mut s = session();
    let slot = open(&mut s, Page::id("filing-a-source"));
    run(&mut s, slot, "kb.use");
    assert!(chat_focused(&s), "use opens a chat");
    assert_eq!(count(s.store(), "SELECT COUNT(*) FROM agent_chat WHERE title = 'follow this skill'"), 1);
    // ask on a page and on a file card.
    let mut s = session();
    let slot = open(&mut s, Page::id("porto-lume"));
    run(&mut s, slot, "kb.ask");
    assert!(chat_focused(&s));
    let mut s = session();
    let slot = open(&mut s, File::id(seed::PDF));
    run(&mut s, slot, "kb.ask");
    assert!(chat_focused(&s));
}

/// Review 2 and 3: a real install gets the six tables of the plan and
/// nothing of the prototype's, and every table is the app's typed writes'
/// alone.
#[test]
fn a_real_install_gets_the_plans_tables_and_all_are_protected() {
    let apps = Apps::new(ALONE);
    let store = Store::open(None, &apps.schemas(), kernel::sync::Device::fake().replicating(apps.replicated())).unwrap();
    apps.seed(&store, Mode::Real).unwrap();
    let tables: Vec<String> = store
        .conn()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'kb%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(tables, ["kb_alias", "kb_draft", "kb_file", "kb_link", "kb_page", "kb_revision"]);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM kb_page"), 0);
    // The cache's states live beside the store, and a real store has none.
    assert_eq!(model::where_is(&store, &hash_of(seed::PDF)), Where::Missing);
    let mut protected: Vec<&str> = KB.protected_sql_tables().to_vec();
    protected.sort_unstable();
    assert_eq!(protected, ["kb_alias", "kb_draft", "kb_file", "kb_link", "kb_page", "kb_revision"]);
}

#[test]
fn the_seed_is_eight_pages_across_the_seven_kinds_with_three_files() {
    let s = session();
    let store = s.store();
    assert_eq!(count(store, "SELECT COUNT(*) FROM kb_page"), 8);
    assert_eq!(count(store, "SELECT COUNT(DISTINCT kind) FROM kb_page"), 7);
    assert_eq!(count(store, "SELECT COUNT(*) FROM kb_file"), 3);
    assert!(count(store, "SELECT COUNT(*) FROM kb_revision") >= 12);
    assert_eq!(rows(store, ""), [
        "sailing-trip-2026", "lamp-build", "joule-heating", "porto-lume",
        "mooring-fees-2026", "filing-a-source", "remembered", "harbour-radio-channels",
    ]);
    assert_eq!(rows(store, "@kind:skill"), ["filing-a-source"]);
    assert_eq!(rows(store, "@orphan"), ["harbour-radio-channels"]);
    assert_eq!(rows(store, "@dangling"), ["sailing-trip-2026"]);
    assert_eq!(rows(store, "harbour"), ["sailing-trip-2026", "porto-lume", "harbour-radio-channels"]);
    // An alias resolves a page, case-folded.
    assert_eq!(model::page(store, "lume").map(|p| p.slug), Some("porto-lume".into()));
    assert_eq!(model::page(store, "LUME").map(|p| p.slug), Some("porto-lume".into()));
    let p = model::page(store, "porto-lume").unwrap();
    let links = model::links(store, &p.uid);
    assert!(links.iter().any(|l| l.kind == LinkKind::Image && matches!(&l.resolved, Target::File { path, .. } if path == seed::PICTURE)));
    assert!(links.iter().any(|l| matches!(&l.resolved, Target::Page { slug, .. } if slug == "mooring-fees-2026")));
    let back: Vec<String> = model::backlinks(store, &p).iter().map(|(s, _)| s.clone()).collect();
    assert_eq!(back, ["mooring-fees-2026", "sailing-trip-2026"]);
    assert_eq!(model::where_is(store, &hash_of(seed::PDF)), Where::Cached);
    assert_eq!(model::where_is(store, &hash_of(seed::TEXT)), Where::Outbox);
    assert!(seed::bytes_of(&hash_of(seed::PDF)).unwrap().starts_with(b"%PDF-1.4"));
    assert!(seed::bytes_of(&hash_of(seed::PICTURE)).unwrap().starts_with(b"\x89PNG"));
}

/// Review 6: the log only grows. A save, its undo and its redo are three
/// revisions, and the page is live with all of them after the redo; a
/// restore is a fourth.
#[test]
fn revisions_are_append_only_through_undo_and_redo() {
    let mut s = session();
    let store = s.store().clone();
    let doc = "---\ntype: concept\ntitle: Anchoring in a blow\nsummary: What holds.\n---\n\nSee [[porto-lume]].\n";
    let uid = model::save(&mut s, None, doc.into(), "new page".into()).unwrap();
    s.settle();
    assert_eq!(revisions(&store, &uid), 1);
    s.undo();
    s.settle();
    assert!(model::page_by_uid(&store, &uid).is_none(), "undone: the page is away");
    assert_eq!(revisions(&store, &uid), 2);
    assert!(model::revisions(&store, &uid)[0].message.starts_with("undone:"));
    s.redo();
    s.settle();
    let page = model::page_by_uid(&store, &uid).expect("redone: the page is back");
    assert_eq!(page.slug, "anchoring-in-a-blow");
    assert_eq!(revisions(&store, &uid), 3, "a live page with its whole history");
    // A restore on the lamp page: the body moves and the log grows by one;
    // undo grows it again and puts the body back exactly.
    let lamp = uid_of("lamp-build");
    let before = body(&store, &lamp);
    let n = revisions(&store, &lamp);
    let slot = open(&mut s, Revision::id(&seed::restorable_revision()));
    run(&mut s, slot, "kb.restore");
    assert_ne!(body(&store, &lamp), before);
    assert_eq!(revisions(&store, &lamp), n + 1);
    assert!(model::revisions(&store, &lamp)[0].message.starts_with("restored from 20 Aug 2026"));
    s.undo();
    s.settle();
    assert_eq!(body(&store, &lamp), before);
    assert_eq!(revisions(&store, &lamp), n + 2);
    assert!(model::revisions(&store, &lamp).iter().any(|r| r.chat() == Some(2)), "the chat's revision is still there");
}

/// Review 4 and 5: a rename rewrites the parsed addresses of the links
/// that resolve to the page — not code, not a link that already spelled
/// the new word — and its undo puts every body back byte for byte, with
/// the rows re-derived from the bodies and a revision on every page moved.
#[test]
fn a_rename_rewrites_addresses_and_its_undo_is_exact() {
    let mut s = session();
    let store = s.store().clone();
    let berlin = model::save(&mut s, None, "---\ntype: entity\ntitle: Berlin\n---\n\nA city.\n".into(), "new page".into()).unwrap();
    // `bonn` is nobody's page yet: the link dangles, and stays as written.
    let original = "---\ntype: concept\ntitle: Trips\n---\n\n[[berlin]] [[bonn]] [[ berlin | the capital ]] [b](berlin.md \"title\") `[[berlin]]`\n\n```\n[[berlin]]\n```\n";
    let trips = model::save(&mut s, None, original.into(), "new page".into()).unwrap();
    s.settle();
    let before = body(&store, &trips);
    let n_trips = revisions(&store, &trips);
    let n_berlin = revisions(&store, &berlin);
    let slug = model::rename(&mut s, &berlin, "Bonn").unwrap();
    s.settle();
    assert_eq!(slug, "bonn");
    assert_eq!(
        body(&store, &trips),
        "[[bonn]] [[bonn]] [[ bonn | the capital ]] [b](bonn.md \"title\") `[[berlin]]`\n\n```\n[[berlin]]\n```\n"
    );
    // The rows say what the bodies say — one row per distinct target and
    // kind, and every one resolves to the page.
    let resolved: Vec<String> = link_rows(&store, &trips).into_iter().map(|(_, _, r)| r).collect();
    assert_eq!(resolved, vec![format!("page:{berlin}"); 2]);
    assert_eq!(revisions(&store, &trips), n_trips + 1);
    assert_eq!(revisions(&store, &berlin), n_berlin + 1);
    assert!(model::revisions(&store, &berlin)[0].message.starts_with("renamed berlin to bonn"));
    assert!(model::revisions(&store, &trips)[0].message.starts_with("links to berlin renamed to bonn"));
    // Undo: byte-exact, the dangling `[[bonn]]` dangling again.
    s.undo();
    s.settle();
    assert_eq!(body(&store, &trips), before);
    assert_eq!(model::page_by_uid(&store, &berlin).unwrap().slug, "berlin");
    let resolved: Vec<String> = link_rows(&store, &trips).into_iter().map(|(_, _, r)| r).collect();
    assert_eq!(resolved, vec![format!("page:{berlin}"), String::new(), format!("page:{berlin}")]);
    assert_eq!(rows(&store, "@dangling"), ["sailing-trip-2026", "trips"]);
    assert_eq!(revisions(&store, &trips), n_trips + 2, "the undo is a write too");
    // Redo: the same again.
    s.redo();
    s.settle();
    assert!(body(&store, &trips).starts_with("[[bonn]] [[bonn]]"));
    assert_eq!(revisions(&store, &trips), n_trips + 3);
}

/// Review 7: a name another live page answers to is refused, as a slug
/// and as an alias, and a deleted page's names are free.
#[test]
fn a_name_another_page_has_is_refused() {
    let mut s = session();
    let doc = |title: &str, aliases: &str| format!("---\ntype: concept\ntitle: {title}\naliases: [{aliases}]\n---\n\nwords\n");
    let why = model::save(&mut s, None, doc("Fresh", "Lume"), "new page".into()).unwrap_err();
    assert_eq!(why, "the alias Lume is already porto-lume's");
    let why = model::save(&mut s, None, doc("Fresh", "joule-heating"), "new page".into()).unwrap_err();
    assert_eq!(why, "the alias joule-heating is already joule-heating's");
    let why = model::save(&mut s, None, doc("Porto Lume", ""), "new page".into()).unwrap_err();
    assert_eq!(why, "the slug porto-lume is already porto-lume's");
    let lamp = uid_of("lamp-build");
    assert_eq!(model::rename(&mut s, &lamp, "Lume").unwrap_err(), "the slug lume is already porto-lume's");
    // The page's own names are not a conflict with itself.
    let porto = model::page(s.store(), "porto-lume").unwrap();
    assert!(model::save(&mut s, Some(porto.uid.clone()), porto.document(), "edited".into()).is_ok());
    // Once porto-lume is deleted its alias is free, and the alias table
    // still says whose it was until the row is written again.
    assert!(model::delete(&mut s, vec!["porto-lume".into()]));
    s.settle();
    assert!(model::save(&mut s, None, doc("Fresh", "Lume"), "new page".into()).is_ok());
    assert_eq!(model::page(s.store(), "lume").map(|p| p.slug), Some("fresh".into()));
}

/// Review 10: one resolver. The reading's links, the page's `kb_link` rows
/// and the catalogue's filters agree on one fixture — a `.md` link, an
/// alias in another case, an imported path, a `%20` path, a `%2F` literal,
/// a file's stem as a wikilink, and a deleted page's alias.
#[test]
fn the_reading_the_rows_and_the_filters_read_one_resolution() {
    let mut s = session();
    let store = s.store().clone();
    let fixture = "---\ntype: concept\ntitle: Fixture\n---\n\n\
        [city](porto-lume.md) [[LUME]] [inbox](inbox/harbour-radio-channels.md) \
        [fees](sources/mooring-fees-2026.pdf) [space](sources/mooring%20fees.pdf) [literal](sources/a%2Fb.pdf) \
        [[porto-lume-harbour]] [[nowhere]] [[Porto-Lume]] [typed](wiki:porto-lume)\n";
    let uid = model::save(&mut s, None, fixture.into(), "new page".into()).unwrap();
    s.settle();
    let resolver = model::Resolver::new(&store);
    let rows = link_rows(&store, &uid);
    let expect = |target: &str| rows.iter().find(|(t, _, _)| t == target).map(|(_, _, r)| r.clone()).unwrap();
    let porto = format!("page:{}", uid_of("porto-lume"));
    assert_eq!(expect("porto-lume.md"), porto);
    assert_eq!(expect("LUME"), porto);
    assert_eq!(expect("inbox/harbour-radio-channels.md"), format!("page:{}", uid_of("harbour-radio-channels")));
    assert_eq!(expect("sources/mooring-fees-2026.pdf"), format!("file:{}", seed::PDF));
    assert_eq!(expect("sources/mooring%20fees.pdf"), "", "a %20 path with no file is dangling");
    assert_eq!(expect("sources/a%2Fb.pdf"), "", "%2F is a literal, never a slash");
    assert_eq!(expect("porto-lume-harbour"), format!("file:{}", seed::PICTURE), "a wikilink names a file by its stem");
    assert_eq!(expect("nowhere"), "");
    // Review 2, #10: a slug in another case resolves; a typed `wiki:`
    // address is an inline link like any other and dangles.
    assert_eq!(expect("Porto-Lume"), porto);
    assert_eq!(expect("wiki:porto-lume"), "");
    // The rows are what the resolver answers, row for row.
    for (target, kind, resolved) in &rows {
        assert_eq!(resolver.resolve(target, LinkKind::of(kind)).key(), *resolved, "{target}");
    }
    // The reading draws the same answers.
    let html = model::page_by_uid(&store, &uid).map(|p| markdown::html(&p.body, &|t, k| resolver.resolve(t, k))).unwrap();
    assert!(html.contains("<a href=\"kb:page/porto-lume\">city</a>"), "{html}");
    assert!(html.contains("<a href=\"kb:page/porto-lume\">LUME</a>"), "{html}");
    assert!(html.contains("<a href=\"kb:page/harbour-radio-channels\">inbox</a>"), "{html}");
    assert!(html.contains(&format!("<a href=\"kb:file/{}\">porto-lume-harbour</a>", seed::PICTURE)), "{html}");
    assert!(html.contains("<dangling>space</dangling>") && html.contains("<dangling>literal</dangling>") && html.contains("<dangling>nowhere</dangling>"), "{html}");
    assert!(html.contains("<a href=\"kb:page/porto-lume\">Porto-Lume</a>"), "{html}");
    assert!(html.contains("<dangling>typed</dangling>"), "a typed wiki: address is drawn as its row says: {html}");
    assert_eq!(model::page(&store, "PORTO-LUME").map(|p| p.slug), Some("porto-lume".into()), "the page lookup folds the same way");
    // And the filters: the fixture dangles (three of its links do), the
    // inbox note is no orphan now that a `.md` link names it, and the
    // backlinks of the town count the fixture once.
    assert_eq!(rows_of(&store, "@dangling"), ["sailing-trip-2026", "fixture"]);
    assert_eq!(rows_of(&store, "@orphan"), ["fixture"], "the inbox note is no orphan now that a .md link names it");
    let porto_page = model::page(&store, "porto-lume").unwrap();
    let back: Vec<String> = model::backlinks(&store, &porto_page).iter().map(|(s, _)| s.clone()).collect();
    assert_eq!(back, ["fixture", "mooring-fees-2026", "sailing-trip-2026"]);
    // A deleted page's alias answers nothing: `[[LUME]]` dangles, and the
    // undo brings the answer back.
    assert!(model::delete(&mut s, vec!["porto-lume".into()]));
    s.settle();
    assert_eq!(link_rows(&store, &uid).iter().find(|(t, _, _)| t == "LUME").unwrap().2, "");
    s.undo();
    s.settle();
    assert_eq!(link_rows(&store, &uid).iter().find(|(t, _, _)| t == "LUME").unwrap().2, porto);
}

/// Review 2, #7: a restore was validated on the title's word while it kept
/// the deleted row's slug, so it took a word another page had meanwhile.
#[test]
fn a_restore_is_validated_as_the_row_it_would_leave() {
    let mut s = session();
    let store = s.store().clone();
    let berlin = model::save(&mut s, None, "---\ntype: entity\ntitle: Berlin\n---\n\nA city.\n".into(), "new page".into()).unwrap();
    s.settle();
    let first = model::revisions(&store, &berlin).last().unwrap().uid.clone();
    assert_eq!(model::rename(&mut s, &berlin, "Bonn").unwrap(), "bonn");
    s.settle();
    assert!(model::delete(&mut s, vec!["bonn".into()]));
    s.settle();
    let other = model::save(&mut s, None, "---\ntype: concept\ntitle: Other\naliases: [bonn]\n---\n\nwords\n".into(), "new page".into()).unwrap();
    s.settle();
    // The original revision's document would come back on the deleted
    // row, whose slug is `bonn` now — and `bonn` is the other page's.
    let (_, document) = model::revision(&store, &first).unwrap();
    let why = model::save(&mut s, Some(berlin.clone()), document.clone(), "restored".into()).unwrap_err();
    assert_eq!(why, "the slug bonn is already other's");
    s.settle();
    assert!(model::page_by_uid(&store, &berlin).is_none(), "the page stays away");
    assert_eq!(model::page(&store, "bonn").map(|p| p.uid), Some(other.clone()), "the other page keeps its alias");
    // The revision panel says the same and writes nothing.
    let n = count(&store, "SELECT COUNT(*) FROM kb_revision");
    let slot = open(&mut s, Revision::id(&first));
    run(&mut s, slot, "kb.restore");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM kb_revision"), n);
    assert!(model::page_by_uid(&store, &berlin).is_none());
    // Once the other page lets the word go, the restore lands, as `bonn`.
    let other_page = model::page_by_uid(&store, &other).unwrap();
    let freed = other_page.document().replace("aliases: [bonn]\n", "");
    model::save(&mut s, Some(other), freed, "edited".into()).unwrap();
    s.settle();
    model::save(&mut s, Some(berlin.clone()), document, "restored".into()).unwrap();
    s.settle();
    assert_eq!(model::page_by_uid(&store, &berlin).map(|p| (p.slug, p.title)), Some(("bonn".into(), "Berlin".into())));
}

fn rows_of(store: &Store, filter: &str) -> Vec<String> {
    rows(store, filter)
}

#[test]
fn a_new_page_takes_its_slug_from_the_title_and_a_delete_is_soft() {
    let mut s = session();
    let store = s.store().clone();
    let doc = "---\ntype: concept\ntitle: Anchoring in a blow\nsummary: What holds.\n---\n\nSee [[porto-lume]].\n";
    let uid = model::save(&mut s, None, doc.into(), "new page".into()).unwrap();
    s.settle();
    let page = model::page_by_uid(&store, &uid).unwrap();
    assert_eq!(page.slug, "anchoring-in-a-blow");
    assert_eq!(model::links(&store, &uid).len(), 1);
    assert_eq!(model::save(&mut s, None, "---\ntitle: \n---\n".into(), "new page".into()).unwrap_err(), "a page needs a title");
    assert!(model::delete(&mut s, vec!["joule-heating".into()]));
    s.settle();
    assert!(model::page(&store, "joule-heating").is_none());
    assert_eq!(count(&store, "SELECT COUNT(*) FROM kb_page WHERE deleted = 1"), 1);
    // The lamp page's link to the concept dangles while it is away.
    assert_eq!(rows(&store, "@dangling"), ["sailing-trip-2026", "lamp-build"]);
    s.undo();
    s.settle();
    assert!(model::page(&store, "joule-heating").is_some());
    assert_eq!(rows(&store, "@dangling"), ["sailing-trip-2026"]);
}

#[test]
fn the_history_names_the_chat() {
    let s = session();
    let revs = model::revisions(s.store(), &uid_of("lamp-build"));
    assert_eq!(revs.len(), 4);
    assert_eq!(revs[1].by(), "by the agent in \u{201c}file the tax letter\u{201d}");
    assert_eq!(revs[1].chat(), Some(2));
}
