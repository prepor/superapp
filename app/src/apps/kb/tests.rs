use super::model::{self, Where};
use super::panels::{Catalogue, Edit, File, History, Import, Page, Revision};
use super::seed::{self, hash_of, uid_of};
use super::KB;
use crate::apps::agent::AGENT;
use kernel::app::App;
use kernel::layout::SlotId;
use kernel::panel::PanelId;
use kernel::richtable::ListState;
use kernel::session::{Action, Session};
use kernel::store::Store;

static APPS: &[&dyn App] = &[&KB, &AGENT];

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

/// Presses one of a panel's own verbs, as the bar would.
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
    let letters = |_: &Session, id: PanelId| -> Vec<(String, Option<char>)> {
        let mut s2 = session();
        let slot = open(&mut s2, id);
        s2.panel_verbs(slot).iter().map(|v| (v.label.clone(), v.accel)).collect()
    };
    assert_eq!(
        letters(&s, Catalogue::id()),
        [("ask", Some('a')), ("new page", Some('n')), ("lint", Some('k')), ("import", Some('m'))]
            .map(|(l, a)| (l.to_string(), a))
    );
    assert_eq!(
        letters(&s, Page::id("porto-lume")),
        [("ask", Some('a')), ("edit", Some('e')), ("history", Some('h')), ("rename", Some('r')), ("delete", Some('d'))]
            .map(|(l, a)| (l.to_string(), a))
    );
    assert_eq!(letters(&s, Page::id("filing-a-source"))[0], ("use".to_string(), Some('s')));
    assert_eq!(letters(&s, Revision::id(&seed::restorable_revision())), [("restore".to_string(), Some('r'))]);
    assert_eq!(letters(&s, Import::id()), [("browse".to_string(), Some('b')), ("import".to_string(), Some('m'))]);
    let missing = letters(&s, File::id(seed::PDF));
    assert_eq!(missing[0], ("ask".to_string(), Some('a')));
    assert_eq!(missing[1], ("open".to_string(), Some('o')));
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
    // The entity page names a picture, two pages and, through the fees, is named back.
    let p = model::page(store, "porto-lume").unwrap();
    let links = model::links(store, &p.uid);
    assert!(links.iter().any(|l| l.kind == "image" && matches!(&l.resolved, model::Target::File { path, .. } if path == seed::PICTURE)));
    assert!(links.iter().any(|l| matches!(&l.resolved, model::Target::Page { slug, .. } if slug == "mooring-fees-2026")));
    let back: Vec<String> = model::backlinks(store, &p).iter().map(|(s, _)| s.clone()).collect();
    assert_eq!(back, ["mooring-fees-2026", "sailing-trip-2026"]);
    // The files know where their bytes are, and the seed can hand them over.
    assert_eq!(model::where_is(store, &hash_of(seed::PDF)), Where::Cached);
    assert_eq!(model::where_is(store, &hash_of(seed::TEXT)), Where::Outbox);
    assert!(seed::bytes_of(&hash_of(seed::PDF)).unwrap().starts_with(b"%PDF-1.4"));
    assert!(seed::bytes_of(&hash_of(seed::PICTURE)).unwrap().starts_with(b"\x89PNG"));
}

#[test]
fn the_history_names_the_chat_and_a_restore_is_a_new_revision() {
    let mut s = session();
    let store = s.store().clone();
    let revs = model::revisions(&store, &uid_of("lamp-build"));
    assert_eq!(revs.len(), 4);
    assert_eq!(revs[1].by(), "by the agent in \u{201c}file the tax letter\u{201d}");
    assert_eq!(revs[1].chat(), Some(2));
    let slot = open(&mut s, Revision::id(&seed::restorable_revision()));
    let before = model::page(&store, "lamp-build").unwrap();
    run(&mut s, slot, "kb.restore");
    let after = model::page(&store, "lamp-build").unwrap();
    assert_ne!(before.body, after.body);
    assert!(!after.body.contains("48.20"));
    let revs = model::revisions(&store, &uid_of("lamp-build"));
    assert_eq!(revs.len(), 5);
    assert!(revs[0].message.starts_with("restored from 20 Aug 2026"));
    // Undo takes the row and the revision back together.
    s.undo();
    s.settle();
    assert_eq!(model::page(&store, "lamp-build").unwrap().body, before.body);
    assert_eq!(model::revisions(&store, &uid_of("lamp-build")).len(), 4);
}

#[test]
fn a_new_page_takes_its_slug_from_the_title_and_a_rename_rewrites_links() {
    let mut s = session();
    let store = s.store().clone();
    let doc = "---\ntype: concept\ntitle: Anchoring in a blow\nsummary: What holds.\n---\n\nSee [[porto-lume]].\n";
    let uid = model::save(&mut s, None, doc.into(), "new page".into()).unwrap();
    let page = model::page_by_uid(&store, &uid).unwrap();
    assert_eq!(page.slug, "anchoring-in-a-blow");
    assert_eq!(model::links(&store, &uid).len(), 1);
    assert_eq!(rows(&store, "@dangling"), ["sailing-trip-2026"]);
    // A rename of the town rewrites the new page's body and its link row.
    let town = model::page(&store, "porto-lume").unwrap();
    let slug = model::rename(&mut s, &town.uid, "Porto Lume Marina").unwrap();
    assert_eq!(slug, "porto-lume-marina");
    assert!(model::page_by_uid(&store, &uid).unwrap().body.contains("[[porto-lume-marina]]"));
    assert_eq!(rows(&store, "@dangling"), ["sailing-trip-2026"]);
    s.undo();
    s.settle();
    assert_eq!(model::page(&store, "porto-lume").unwrap().slug, "porto-lume");
    assert!(model::page_by_uid(&store, &uid).unwrap().body.contains("[[porto-lume]]"));
}

#[test]
fn a_delete_is_soft_and_undoable() {
    let mut s = session();
    let store = s.store().clone();
    assert!(model::delete(&mut s, vec!["joule-heating".into()]));
    s.settle();
    assert!(model::page(&store, "joule-heating").is_none());
    assert_eq!(count(&store, "SELECT COUNT(*) FROM kb_page WHERE deleted = 1"), 1);
    s.undo();
    s.settle();
    assert!(model::page(&store, "joule-heating").is_some());
}

#[test]
fn the_page_and_the_filter_read_the_same_resolution() {
    let s = session();
    let resolver = model::Resolver::new(s.store());
    assert!(matches!(resolver.resolve("tide-tables"), model::Target::Dangling(_)));
    assert!(matches!(resolver.resolve("joule-heating.md"), model::Target::Page { .. }));
    assert!(matches!(resolver.resolve("sources/mooring-fees-2026.pdf"), model::Target::File { .. }));
    assert!(matches!(resolver.resolve("sources/mooring-fees-2026%20copy.pdf"), model::Target::Dangling(_)));
    // The seed's spec is a total order with a key, as the table wants.
    assert_eq!(model::PAGES.spec.key, "p.slug");
}
