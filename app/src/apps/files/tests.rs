//! The app driven with no widget in sight: a session over the demo tree,
//! its bars pulled, its verbs run.

use std::sync::{Mutex, MutexGuard};

use kernel::app::App;
use kernel::caps::Disk;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{PanelId, VerbAct};
use kernel::richtable::Completion;
use kernel::session::{Action, Instance, Session};
use kernel::store::Store;

use super::model::{
    crumbs, fmt_size, image_lines, image_size, normalize, preview_of, real_path, stat_in,
    text_lines, Entry, FileKind, Preview, HOME,
};
// The kernel's, beside `FileKind`: what a name claims and how much of it a
// card reads are questions mail asks of a part of a letter too, so the app
// itself no longer names them.
use kernel::caps::{
    image_format, mime_of, ImageFormat, IMAGE_PREVIEW_MAX, TEXT_PREVIEW_MAX,
};
use super::ops::copy_name;
use super::{Card, Dir, Op, FILES};

static APPS: &[&dyn App] = &[&FILES];

/// The clipboard belongs to the app, and an app is a `static`: two tests
/// holding something at once would hold it from each other. The ones that
/// do take this first.
static CLIP: Mutex<()> = Mutex::new(());

/// The lock, and an empty clipboard to start from. A poisoned lock is
/// still a lock: a test that panicked while holding it says so on its own.
fn alone() -> MutexGuard<'static, ()> {
    let g = CLIP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    FILES.clear();
    g
}

/// A session with one files panel on home, and its slot.
fn home() -> (Session, SlotId) {
    let mut s = Session::fake(APPS);
    assert!(open(&mut s, HOME).is_some());
    let slot = s.focus().expect("the listing is focused");
    (s, slot)
}

/// Another files panel, of its own.
fn open(s: &mut Session, dir: &str) -> Option<SlotId> {
    let id = Dir::id(dir);
    s.act(
        Action::new("open", format!("open “{dir}”")).moving(move |wm| {
            wm.open(id, None, false);
        }),
    )?;
    s.settle();
    s.focus()
}

/// A navigation, settled — what the shell does after every event, and what
/// a test does before it looks at the slots.
fn go(s: &mut Session, n: Nav) {
    s.nav(n);
    s.settle();
}

/// A panel on another workspace, opened for its own sake: joined to
/// nothing, previewing nothing, nobody's business but its own.
fn elsewhere(s: &mut Session, id: PanelId) -> SlotId {
    assert!(s.switch(1), "the second workspace");
    s.act(Action::new("open", "open elsewhere").moving(move |wm| {
        wm.open(id, None, false);
    }))
    .expect("the panel opened");
    s.settle();
    let slot = s.focus().expect("the panel it opened");
    assert!(s.switch(0), "back where the verbs run");
    slot
}

/// Hangs a panel under a slot as its joined child — the chain the kernel
/// takes with a slot that closes.
fn join_under(s: &mut Session, slot: SlotId, id: PanelId) -> SlotId {
    s.act(Action::new("open", "open joined").moving(move |wm| {
        wm.open(id, Some(slot), true);
    }))
    .expect("the panel opened");
    s.settle();
    s.joined_child(slot).expect("the joined child")
}

/// The instance, as a widget holds it: an `Rc` of its own, so the session
/// is free while the panel is borrowed.
fn inst(s: &Session, slot: SlotId) -> Instance {
    s.panel(slot).expect("a panel in the slot")
}

/// Reads something off the files panel in a slot.
fn with_dir<R>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Dir) -> R) -> R {
    let i = inst(s, slot);
    let mut p = i.borrow_mut();
    f(p.as_any().downcast_mut::<Dir>().expect("a files panel"))
}

/// The same for a card.
fn with_card<R>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Card) -> R) -> R {
    let i = inst(s, slot);
    let mut p = i.borrow_mut();
    f(p.as_any().downcast_mut::<Card>().expect("a file card"))
}

/// What a slot shows.
fn showing(s: &Session, slot: SlotId) -> PanelId {
    inst(s, slot).borrow().id().clone()
}

/// The rows a listing draws, in order.
fn rows(s: &Session, slot: SlotId) -> Vec<Entry> {
    let store: std::rc::Rc<Store> = s.store().clone();
    with_dir(s, slot, |d| {
        let n = d.list().len(&store);
        (0..n)
            .filter_map(|i| d.list().row(&store, i).map(|r| r.entry))
            .collect()
    })
}

/// One row by name.
fn row(s: &Session, slot: SlotId, name: &str) -> Entry {
    rows(s, slot)
        .into_iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no row “{name}” in the listing"))
}

/// The labels a listing draws.
fn labels(s: &Session, slot: SlotId) -> Vec<String> {
    rows(s, slot).iter().map(Entry::label).collect()
}

/// Runs a verb off a panel's bar exactly as the bar does: the bar is pulled
/// again as it fires, and a verb of the panel's own is its own method, with
/// the instance borrowed for the whole of it.
fn run(s: &mut Session, slot: SlotId, id: &str) {
    let inst = inst(s, slot);
    let act = {
        let verbs = inst.borrow().verbs();
        verbs.into_iter().find(|v| v.id == id).map(|v| v.act)
    };
    match act.unwrap_or_else(|| panic!("no verb “{id}” on the bar")) {
        VerbAct::Run => inst.borrow_mut().run(id, s),
        VerbAct::Call(f) => f(s),
        VerbAct::Go(n) => s.nav(n),
    }
    s.settle();
}

/// `new dir`, as the field's submit calls it: on the instance, with the
/// session beside it.
fn new_dir(s: &mut Session, slot: SlotId, name: &str) {
    let i = inst(s, slot);
    let mut p = i.borrow_mut();
    p.as_any()
        .downcast_mut::<Dir>()
        .expect("a files panel")
        .new_dir(s, name);
    drop(p);
    s.settle();
}

/// `rename` on a card, as the field's submit calls it.
fn rename_card(s: &mut Session, slot: SlotId, name: &str) {
    let i = inst(s, slot);
    let mut p = i.borrow_mut();
    p.as_any()
        .downcast_mut::<Card>()
        .expect("a file card")
        .rename(s, name);
    drop(p);
    s.settle();
}

/// The same on a listing, which renames the directory it shows.
fn rename_dir(s: &mut Session, slot: SlotId, name: &str) {
    let i = inst(s, slot);
    let mut p = i.borrow_mut();
    p.as_any()
        .downcast_mut::<Dir>()
        .expect("a files panel")
        .rename(s, name);
    drop(p);
    s.settle();
}

/// The verb ids a panel's bar wears.
fn bar(s: &Session, slot: SlotId) -> Vec<&'static str> {
    inst(s, slot)
        .borrow()
        .verbs()
        .iter()
        .map(|v| v.id)
        .collect()
}

/// Whether the disk has this path.
fn there(s: &Session, path: &str) -> bool {
    stat_in(s.world(), path).is_some()
}

/// Puts a directory there behind the panels' backs, under exactly the name
/// given: `new dir` trims what it is handed, and a name with a space at
/// either end is the thing being tested.
fn conjure_dir(s: &Session, path: &str) {
    s.world()
        .with_cap::<dyn Disk, _>(|d| d.make_dir(&real_path(path)))
        .expect("a disk")
        .expect("the disk made it");
}

/// Takes a path away behind the panels' backs — another program, while
/// nothing was watching.
fn vanish(s: &Session, path: &str) {
    s.world()
        .with_cap::<dyn Disk, _>(|d| d.trash(&real_path(path)))
        .expect("a disk")
        .expect("the trash took it");
}

/// How many nodes the history has.
fn nodes(s: &Session) -> usize {
    s.history().rows().0.len()
}

// -- the listing ---------------------------------------------------------------

#[test]
fn the_home_listing_is_the_demo_tree() {
    let _alone = alone();
    let (s, slot) = home();
    assert_eq!(
        labels(&s, slot),
        [
            "Desktop/",
            "Documents/",
            "Downloads/",
            "Pictures/",
            "superapp/",
            "notes.md",
        ],
        "directories first, then names, and the dot-files hidden"
    );
    assert_eq!(showing(&s, slot), Dir::id(HOME));
    assert_eq!(inst(&s, slot).borrow().title(), "~");
    assert_eq!(inst(&s, slot).borrow().wish(60), (4, 6));
}

#[test]
fn the_filter_shows_the_dot_files_and_the_directories() {
    let _alone = alone();
    let (s, slot) = home();
    with_dir(&s, slot, |d| {
        assert!(d.list_mut().set_filter("@hidden"));
    });
    assert!(labels(&s, slot).contains(&".zshrc".to_string()));
    with_dir(&s, slot, |d| {
        assert!(d.list_mut().set_filter("@dir"));
    });
    assert_eq!(
        labels(&s, slot),
        [
            "Desktop/",
            "Documents/",
            "Downloads/",
            "Pictures/",
            "superapp/"
        ]
    );
}

#[test]
fn a_preview_opens_a_list_for_a_directory_and_a_card_for_a_file() {
    let _alone = alone();
    let (mut s, slot) = home();

    let e = row(&s, slot, "Downloads");
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    let child = s.joined_child(slot).expect("a joined child");
    assert_eq!(showing(&s, child), Dir::id("~/Downloads"));
    assert_eq!(s.focus(), Some(slot), "a preview leaves focus behind");
    assert!(labels(&s, child).contains(&"README.txt".to_string()));

    // The same walk, one row further: a file is a card in the same slot.
    let e = row(&s, slot, "notes.md");
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    let child = s.joined_child(slot).expect("the same joined child");
    assert_eq!(showing(&s, child), Card::id("~/notes.md"));
    with_card(&s, child, |c| {
        assert_eq!(c.name(), "notes.md");
        assert_eq!(c.kind_word(), "text");
        assert_eq!(c.kind_line(), "text · 2.1 KB");
        assert_eq!(c.path(), "~/notes.md");
        assert!(c.when().starts_with("modified "));
        assert!(c
            .text()
            .is_some_and(|t| t.contains("a directory is a list")));
        assert!(!c.gone());
    });
}

#[test]
fn go_to_replaces_for_a_directory_and_says_so_for_a_path_that_is_not_there() {
    let _alone = alone();
    let (mut s, slot) = home();
    run(&mut s, slot, "files.go_to");
    assert_eq!(
        with_dir(&s, slot, |d| d.pathing().map(str::to_string)),
        Some("~/".to_string()),
        "the field is seeded with where the panel stands"
    );

    let nav = with_dir(&s, slot, |d| d.go_to("~/Downloads/2026"));
    assert_eq!(
        nav,
        Some(Nav::Replace {
            slot,
            id: Dir::id("~/Downloads/2026")
        })
    );
    go(&mut s, nav.expect("a navigation"));
    assert_eq!(showing(&s, slot), Dir::id("~/Downloads/2026"));
    assert_eq!(inst(&s, slot).borrow().title(), "2026");

    // A file is previewed beside the list instead: the list keeps its
    // place, as a cursor walk does.
    let nav = with_dir(&s, slot, |d| d.go_to("~/notes.md"));
    assert_eq!(
        nav,
        Some(Nav::Preview {
            from: slot,
            id: Card::id("~/notes.md")
        })
    );

    // And a path that names nothing is a line where the field is.
    assert_eq!(with_dir(&s, slot, |d| d.go_to("~/nowhere")), None);
    assert_eq!(
        with_dir(&s, slot, |d| d.status().map(str::to_string)),
        Some("“~/nowhere” is not there".to_string())
    );
    assert_eq!(with_dir(&s, slot, |d| d.go_to("Downloads")), None);
    assert_eq!(
        with_dir(&s, slot, |d| d.status().map(str::to_string)),
        Some("“Downloads” is not a path".to_string()),
        "a relative spelling is not a path the browser reads"
    );
}

#[test]
fn the_crumbs_name_the_panels_they_go_to() {
    let _alone = alone();
    let (mut s, slot) = home();
    let nav = with_dir(&s, slot, |d| d.go_to("~/Downloads/2026"));
    go(&mut s, nav.expect("a navigation"));
    assert_eq!(
        with_dir(&s, slot, |d| d.crumbs()),
        vec![
            ("~".to_string(), Dir::id("~")),
            ("Downloads".to_string(), Dir::id("~/Downloads")),
            ("2026".to_string(), Dir::id("~/Downloads/2026")),
        ]
    );
}

// -- the verbs -----------------------------------------------------------------

#[test]
fn new_dir_is_one_action_and_undo_trashes_it() {
    let _alone = alone();
    let (mut s, slot) = home();
    run(&mut s, slot, "files.new_dir");
    assert_eq!(
        with_dir(&s, slot, |d| d.naming().map(str::to_string)),
        Some(String::new()),
        "the verb opens the field the widget draws"
    );

    let was = nodes(&s);
    new_dir(&mut s, slot, "reports");
    assert!(there(&s, "~/reports"));
    assert_eq!(nodes(&s), was + 1, "one node");
    assert!(labels(&s, slot).contains(&"reports/".to_string()));
    assert!(
        with_dir(&s, slot, |d| d.naming().is_none()),
        "the field closes behind it"
    );

    assert!(s.undo());
    assert!(!there(&s, "~/reports"), "undo trashed it");
    assert!(there(&s, "~/.Trash/reports"));
    with_dir(&s, slot, |d| d.observe(&s));
    assert!(!labels(&s, slot).contains(&"reports/".to_string()));

    assert!(s.redo());
    assert!(there(&s, "~/reports"), "redo made it again");

    // A name the directory already has is refused where the field was.
    new_dir(&mut s, slot, "reports");
    assert_eq!(
        with_dir(&s, slot, |d| d.status().map(str::to_string)),
        Some("~/reports is already there".to_string())
    );
}

#[test]
fn copy_holds_a_file_and_copy_here_lays_it_down() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(&mut s, Nav::Preview {
        from: slot,
        id: Card::id("~/notes.md"),
    });
    let card = s.joined_child(slot).expect("the card");

    run(&mut s, card, "files.copy");
    assert_eq!(
        FILES.clipboard().paths,
        vec!["~/notes.md".to_string()],
        "the card's own file"
    );
    assert_eq!(FILES.clipboard().verb, Op::Copy);

    // Another directory, and the verb the clipboard put on its bar.
    go(&mut s, Nav::Open {
        from: slot,
        id: Dir::id("~/Desktop"),
        fresh: true,
    });
    let desk = s.focus().expect("the new panel");
    assert!(bar(&s, desk).contains(&"files.here"));
    let was = nodes(&s);
    run(&mut s, desk, "files.here");
    assert!(there(&s, "~/Desktop/notes.md"));
    assert_eq!(nodes(&s), was + 1, "one node for the batch");
    assert!(labels(&s, desk).contains(&"notes.md".to_string()));
    assert!(
        !FILES.clipboard().is_empty(),
        "a copy keeps what it laid down, so it can be laid down again"
    );

    assert!(s.undo());
    assert!(!there(&s, "~/Desktop/notes.md"), "undo trashed the copy");
    assert!(there(&s, "~/notes.md"), "and left the original alone");
}

#[test]
fn a_copy_into_the_same_directory_takes_the_next_free_name() {
    let _alone = alone();
    let (mut s, slot) = home();
    FILES.set(Op::Copy, vec!["~/notes.md".to_string()]);
    run(&mut s, slot, "files.here");
    assert!(there(&s, "~/notes copy.md"));
    run(&mut s, slot, "files.here");
    assert!(there(&s, "~/notes copy 2.md"));
}

#[test]
fn move_here_moves_and_lets_go() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(&mut s, Nav::Preview {
        from: slot,
        id: Card::id("~/notes.md"),
    });
    let card = s.joined_child(slot).expect("the card");

    run(&mut s, card, "files.move");
    assert_eq!(FILES.clipboard().verb, Op::Move);

    go(&mut s, Nav::Open {
        from: slot,
        id: Dir::id("~/Desktop"),
        fresh: true,
    });
    let desk = s.focus().expect("the new panel");
    run(&mut s, desk, "files.here");
    assert!(there(&s, "~/Desktop/notes.md"));
    assert!(
        !there(&s, "~/notes.md"),
        "a move empties where it came from"
    );
    assert!(
        FILES.clipboard().is_empty(),
        "a move consumes what it carried"
    );
    assert!(
        !labels(&s, slot).contains(&"notes.md".to_string()),
        "the panel it came from lists again too"
    );
    assert!(
        s.panel(card).is_some(),
        "a move closes nothing: the card is still where someone put it"
    );
    assert!(
        with_card(&s, card, |c| c.gone()),
        "and it says what it shows is not there any more"
    );

    assert!(s.undo());
    assert!(there(&s, "~/notes.md"), "undo moved it back");
    assert!(!there(&s, "~/Desktop/notes.md"));
    with_card(&s, card, |c| c.observe(&s));
    assert!(
        !with_card(&s, card, |c| c.gone()),
        "and the card has its file again"
    );
}

#[test]
fn delete_trashes_and_undo_puts_it_back() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(&mut s, Nav::Preview {
        from: slot,
        id: Card::id("~/notes.md"),
    });
    let card = s.joined_child(slot).expect("the card");

    let was = nodes(&s);
    run(&mut s, card, "files.delete");
    assert!(!there(&s, "~/notes.md"));
    assert!(there(&s, "~/.Trash/notes.md"), "never an rm");
    assert_eq!(nodes(&s), was + 1);
    assert!(s.panel(card).is_none(), "the card had nothing left to show");
    assert!(!labels(&s, slot).contains(&"notes.md".to_string()));

    assert!(s.undo());
    assert!(there(&s, "~/notes.md"));
    with_dir(&s, slot, |d| d.observe(&s));
    assert!(labels(&s, slot).contains(&"notes.md".to_string()));
    assert!(s.panel(card).is_some());
}

#[test]
fn a_delete_closes_its_own_slot_and_the_chain_under_it_and_nothing_else() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(&mut s, Nav::Preview {
        from: slot,
        id: Card::id("~/notes.md"),
    });
    let card = s.joined_child(slot).expect("the card");
    // Something joined under the card: the chain that is context derived
    // from what the card shows, and goes where it goes.
    let under = join_under(&mut s, card, Dir::id("~/Desktop"));
    // And another card on the very same file, opened for its own sake on
    // another workspace. No verb goes looking for it.
    let other = elsewhere(&mut s, Card::id("~/notes.md"));

    run(&mut s, card, "files.delete");
    assert!(!there(&s, "~/notes.md"));
    assert!(s.panel(card).is_none(), "the card had nothing left to show");
    assert!(
        s.panel(under).is_none(),
        "and the kernel took the chain under it"
    );
    assert!(
        s.panel(slot).is_some(),
        "the list that previewed the card is nobody's descendant"
    );
    assert!(
        s.panel(other).is_some(),
        "and the card elsewhere keeps showing what it shows"
    );
    assert!(
        with_card(&s, other, |c| c.gone()),
        "— and says the file is not there any more"
    );

    // One node: the disk and the panels come back together.
    assert!(s.undo());
    assert!(there(&s, "~/notes.md"));
    assert!(s.panel(card).is_some(), "the card is back");
    assert!(s.panel(under).is_some(), "with the chain under it");
}

#[test]
fn a_root_is_nobody_s_object() {
    let _alone = alone();
    let (mut s, slot) = home();
    // Home is where the browser starts: it wears the two field verbs and
    // nothing that would take it away.
    assert_eq!(bar(&s, slot), ["files.new_dir", "files.go_to"]);

    // A directory previewed beside it is the object under the cursor, and
    // wears the three that act on what it shows.
    let e = row(&s, slot, "Downloads");
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    let child = s.joined_child(slot).expect("a joined child");
    with_dir(&s, child, |d| d.observe(&s));
    assert_eq!(
        bar(&s, child),
        [
            "files.new_dir",
            "files.go_to",
            "files.copy",
            "files.move",
            "files.rename",
            "files.delete"
        ]
    );

    // …until it drives a preview of its own: then the chord means the row
    // under *its* cursor, and its own directory is nobody's object.
    let e = row(&s, child, "README.txt");
    let nav = with_dir(&s, child, |d| d.preview(&e));
    go(&mut s, nav);
    with_dir(&s, child, |d| d.observe(&s));
    assert_eq!(bar(&s, child), ["files.new_dir", "files.go_to"]);
}

// -- rename --------------------------------------------------------------------

#[test]
fn rename_is_one_action_and_undo_puts_the_name_back() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(&mut s, Nav::Preview {
        from: slot,
        id: Card::id("~/notes.md"),
    });
    let card = s.joined_child(slot).expect("the card");

    run(&mut s, card, "files.rename");
    assert_eq!(
        with_card(&s, card, |c| c.renaming().map(str::to_string)),
        Some("notes.md".to_string()),
        "the verb opens the field, seeded with the name it has"
    );
    assert_eq!(
        s.focus(),
        Some(card),
        "and focus follows it: this verb arrives through the list above, \
         and a caret on an unfocused panel would never see a letter"
    );

    let was = nodes(&s);
    rename_card(&mut s, card, "reading.md");
    assert!(there(&s, "~/reading.md"));
    assert!(!there(&s, "~/notes.md"), "a rename moves, it does not copy");
    assert!(
        !there(&s, "~/.Trash/notes.md"),
        "and nothing went to the trash"
    );
    assert_eq!(nodes(&s), was + 1, "one node");
    assert_eq!(
        showing(&s, card),
        Card::id("~/reading.md"),
        "the card is on the file, not on the spelling"
    );
    with_dir(&s, slot, |d| d.observe(&s));
    assert!(labels(&s, slot).contains(&"reading.md".to_string()));

    assert!(s.undo());
    assert!(there(&s, "~/notes.md"), "undo put the old name back");
    assert!(!there(&s, "~/reading.md"));
    assert_eq!(
        showing(&s, card),
        Card::id("~/notes.md"),
        "and the card came back with it — one node, both halves"
    );

    assert!(s.redo());
    assert!(there(&s, "~/reading.md"), "redo renamed it again");
}

#[test]
fn rename_refuses_a_path_a_taken_name_and_a_file_that_has_gone() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(&mut s, Nav::Preview {
        from: slot,
        id: Card::id("~/notes.md"),
    });
    let card = s.joined_child(slot).expect("the card");
    run(&mut s, card, "files.rename");

    // A rename renames a thing where it already is. A path in the field
    // would carry it off somewhere else under the word.
    rename_card(&mut s, card, "Desktop/notes.md");
    assert_eq!(
        with_card(&s, card, |c| c.status().map(str::to_string)),
        Some("a name is not a path".to_string())
    );
    assert!(there(&s, "~/notes.md"), "and nothing was written");
    assert!(
        with_card(&s, card, |c| c.renaming().is_some()),
        "a refusal keeps the field, with the name still in it"
    );

    // A name the directory already has: the same sentence a `… here` gives.
    rename_card(&mut s, card, "Desktop");
    assert_eq!(
        with_card(&s, card, |c| c.status().map(str::to_string)),
        Some("“Desktop” is already here".to_string())
    );

    // The name it already has is the field's work done, and nothing at all:
    // no disk is asked, and no node is made.
    let was = nodes(&s);
    rename_card(&mut s, card, "notes.md");
    assert_eq!(nodes(&s), was, "no node");
    assert!(
        with_card(&s, card, |c| c.renaming().is_none()),
        "and the field closes behind it"
    );

    // The file may have gone while the field stood: nothing watches a disk,
    // so the write is the first look since the verb opened it.
    run(&mut s, card, "files.rename");
    vanish(&s, "~/notes.md");
    rename_card(&mut s, card, "reading.md");
    assert_eq!(
        with_card(&s, card, |c| c.status().map(str::to_string)),
        Some("“notes.md” is no longer there".to_string())
    );
    assert!(!there(&s, "~/reading.md"));
}

/// The field is seeded with the name the file has, so enter on that seed
/// means *nothing*, whatever the name holds. A file whose name really does
/// end in a space would otherwise be quietly shortened by a submit that
/// changed nothing — and a stray space either side of a name is not a
/// rename either.
#[test]
fn a_submit_that_changed_nothing_renames_nothing() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(
        &mut s,
        Nav::Preview {
            from: slot,
            id: Card::id("~/notes.md"),
        },
    );
    let card = s.joined_child(slot).expect("the card");
    let was = nodes(&s);

    run(&mut s, card, "files.rename");
    rename_card(&mut s, card, "  notes.md  ");
    assert_eq!(nodes(&s), was, "a stray space is not a new name");
    assert!(there(&s, "~/notes.md"));
    assert!(!there(&s, "~/notes.md  "));

    // And the same for a name that is itself spaced — made behind the
    // panels' backs, since `new dir` trims what it is given. The seed comes
    // back exactly as the disk spells it, and enter on it writes nothing.
    let spaced = "~/notes .md ";
    conjure_dir(&s, spaced);
    assert!(there(&s, spaced));
    go(
        &mut s,
        Nav::Preview {
            from: slot,
            id: Card::id(spaced),
        },
    );
    let spaced_card = s.joined_child(slot).expect("the card");
    run(&mut s, spaced_card, "files.rename");
    let seed = with_card(&s, spaced_card, |c| c.renaming().map(str::to_string));
    assert_eq!(seed, Some("notes .md ".to_string()), "the name as it is");
    let was = nodes(&s);
    rename_card(&mut s, spaced_card, "notes .md ");
    assert_eq!(nodes(&s), was, "no node");
    assert!(there(&s, spaced), "and the trailing space is still there");
}

#[test]
fn renaming_a_directory_carries_its_panel_to_the_new_name() {
    let _alone = alone();
    let (mut s, slot) = home();
    // The directory previewed beside the list is the object under the
    // cursor, and wears the verbs that act on what it shows.
    let e = row(&s, slot, "Downloads");
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    let child = s.joined_child(slot).expect("a joined child");
    with_dir(&s, child, |d| d.observe(&s));

    run(&mut s, child, "files.rename");
    assert_eq!(
        with_dir(&s, child, |d| d.renaming().map(str::to_string)),
        Some("Downloads".to_string())
    );

    rename_dir(&mut s, child, "Inbox");
    assert!(there(&s, "~/Inbox"));
    assert!(!there(&s, "~/Downloads"));
    assert!(
        there(&s, "~/Inbox/README.txt"),
        "with everything that was under it"
    );
    assert_eq!(
        showing(&s, child),
        Dir::id("~/Inbox"),
        "the listing is on the directory, not on the spelling"
    );

    assert!(s.undo());
    assert!(there(&s, "~/Downloads"));
    assert_eq!(showing(&s, child), Dir::id("~/Downloads"));
}

// -- the batch -----------------------------------------------------------------

/// Marks two rows of a listing, by name.
fn mark(s: &Session, slot: SlotId, names: [&str; 2]) {
    with_dir(s, slot, |d| {
        for n in names {
            d.list_mut().marks_mut().add(n.to_string());
        }
    });
}

/// What is marked, in key order.
fn marks(s: &Session, slot: SlotId) -> Vec<String> {
    with_dir(s, slot, |d| d.list().marks().keys())
}

/// Where a name sits in the listing as it stands.
fn index_of(s: &Session, slot: SlotId, name: &str) -> usize {
    rows(s, slot)
        .iter()
        .position(|e| e.name == name)
        .unwrap_or_else(|| panic!("no row “{name}” in the listing"))
}

/// The row the cursor is on, by the list's own three rules.
fn under_cursor(s: &Session, slot: SlotId) -> Entry {
    let store: std::rc::Rc<Store> = s.store().clone();
    with_dir(s, slot, |d| {
        let i = d.list().cursor_index(&store).expect("a cursor");
        d.list().row(&store, i).map(|r| r.entry)
    })
    .expect("a row under the cursor")
}

/// `rename` is the one object verb a marked set does not wear: a name is a
/// name, and two things cannot both take it.
#[test]
fn a_marked_set_wears_no_rename() {
    let _alone = alone();
    let (mut s, _) = home();
    let slot = open(&mut s, "~/Downloads").expect("the listing");
    let e = row(&s, slot, "2026");
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    let child = s.joined_child(slot).expect("a joined child");
    with_dir(&s, child, |d| d.observe(&s));
    assert!(bar(&s, child).contains(&"files.rename"), "one thing, one name");

    mark(&s, child, ["README.txt", "report-q3.pdf"]);
    let batch = bar(&s, child);
    assert!(batch.contains(&"files.delete"), "the batch verbs are there");
    assert!(!batch.contains(&"files.rename"), "and rename is not one of them");
}

#[test]
fn a_batch_delete_closes_nothing_and_the_cursor_walks_on() {
    let _alone = alone();
    let (mut s, _) = home();
    let slot = open(&mut s, "~/Downloads").expect("the listing");

    // A walk: the cursor on a row, and that row previewed beside the list.
    let store = s.store().clone();
    let i = index_of(&s, slot, "README.txt");
    let e = with_dir(&s, slot, |d| d.list_mut().set_cursor(&store, i))
        .expect("the row")
        .entry;
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    let child = s.joined_child(slot).expect("the row's card");
    assert_eq!(showing(&s, child), Card::id("~/Downloads/README.txt"));

    mark(&s, slot, ["README.txt", "report-q3.pdf"]);
    run(&mut s, slot, "files.delete");
    assert!(!there(&s, "~/Downloads/README.txt"));
    assert!(
        s.panel(slot).is_some(),
        "the rows went, not what the list shows"
    );
    assert_eq!(
        s.joined_child(slot),
        Some(child),
        "and the child it previewed is untouched — a verb closes nothing it did not run on"
    );

    // The cursor is on the nearest row left, and previewing that row
    // replaces the child in place, as any cursor step does.
    let e = under_cursor(&s, slot);
    assert_eq!(e.name, "screenshot-2026-08-30.png");
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    assert_eq!(s.joined_child(slot), Some(child), "the same slot");
    assert_eq!(
        showing(&s, child),
        Card::id("~/Downloads/screenshot-2026-08-30.png")
    );
}

#[test]
fn a_batch_delete_is_one_node_and_undo_restores_the_marks() {
    let _alone = alone();
    let (mut s, _) = home();
    let slot = open(&mut s, "~/Downloads").expect("the listing");
    mark(&s, slot, ["README.txt", "report-q3.pdf"]);

    // The batch verb is the single-row verb over more than one thing, so
    // it wears the same id — and says how many in its label.
    let labelled = inst(&s, slot)
        .borrow()
        .verbs()
        .into_iter()
        .find(|v| v.id == "files.delete")
        .map(|v| (v.label, v.accel));
    assert_eq!(labelled, Some(("delete 2".to_string(), Some('d'))));

    let was = nodes(&s);
    run(&mut s, slot, "files.delete");
    assert!(!there(&s, "~/Downloads/README.txt"));
    assert!(!there(&s, "~/Downloads/report-q3.pdf"));
    assert_eq!(nodes(&s), was + 1, "one node for the whole set");
    assert!(marks(&s, slot).is_empty(), "the rows went, and the marks");

    assert!(s.undo());
    assert!(there(&s, "~/Downloads/README.txt"));
    assert!(there(&s, "~/Downloads/report-q3.pdf"));
    assert_eq!(
        marks(&s, slot),
        ["README.txt".to_string(), "report-q3.pdf".to_string()],
        "and the marks came back with them"
    );

    assert!(s.redo());
    assert!(!there(&s, "~/Downloads/README.txt"));
    assert!(marks(&s, slot).is_empty(), "redo takes exactly these again");
}

#[test]
fn a_batch_keeps_the_mark_it_could_not_take() {
    let _alone = alone();
    let (mut s, _) = home();
    let slot = open(&mut s, "~/Downloads").expect("the listing");
    mark(&s, slot, ["README.txt", "report-q3.pdf"]);
    // Another program took one of them while nothing was watching.
    vanish(&s, "~/Downloads/README.txt");

    run(&mut s, slot, "files.delete");
    assert!(
        !there(&s, "~/Downloads/report-q3.pdf"),
        "the one that was there"
    );
    assert_eq!(
        marks(&s, slot),
        ["README.txt".to_string()],
        "what a verb leaves marked is exactly what it could not do"
    );
    assert!(
        s.notes()
            .iter()
            .any(|n| n.msg.contains("1 of 2 files to the trash")),
        "and the toast says how many of how many: {:?}",
        s.notes()
    );
}

#[test]
fn a_batch_holds_every_marked_row_and_keeps_the_marks() {
    let _alone = alone();
    let (mut s, _) = home();
    let slot = open(&mut s, "~/Downloads").expect("the listing");
    mark(&s, slot, ["README.txt", "report-q3.pdf"]);

    run(&mut s, slot, "files.copy");
    assert_eq!(
        FILES.clipboard().paths,
        [
            "~/Downloads/README.txt".to_string(),
            "~/Downloads/report-q3.pdf".to_string()
        ]
    );
    assert_eq!(
        marks(&s, slot).len(),
        2,
        "nothing was consumed: the destination is still to be walked to"
    );
}

#[test]
fn a_here_that_can_do_nothing_says_so_and_holds_on() {
    let _alone = alone();
    let (mut s, slot) = home();
    FILES.set(Op::Move, vec!["~/notes.md".to_string()]);
    let was = nodes(&s);
    run(&mut s, slot, "files.here");
    assert_eq!(nodes(&s), was, "nothing happened, so there is no node");
    assert_eq!(
        with_dir(&s, slot, |d| d.status().map(str::to_string)),
        Some("“notes.md” is already here".to_string())
    );
    assert!(!FILES.clipboard().is_empty(), "and the clipboard stands");
}

// -- the spellings a path travels in -------------------------------------------

#[test]
fn a_typed_path_is_read_the_way_a_shell_reads_one() {
    let _alone = alone();
    assert_eq!(
        normalize("~/Downloads/2026"),
        Some("~/Downloads/2026".into())
    );
    assert_eq!(normalize("  ~/Downloads/  "), Some("~/Downloads".into()));
    assert_eq!(
        normalize("~/Downloads/../notes.md"),
        Some("~/notes.md".into())
    );
    assert_eq!(normalize("~"), Some("~".into()));
    // A second root restarts the path, so a typed absolute one wins over
    // the seed without the field having to be cleared first.
    assert_eq!(normalize("~/Downloads//tmp"), Some("/tmp".into()));
    assert_eq!(normalize("~/Downloads/~/x"), Some("~/x".into()));
    // What the browser does not read.
    assert_eq!(normalize("Downloads"), None);
    assert_eq!(normalize(""), None);

    assert_eq!(
        crumbs("/tmp"),
        [
            ("/".to_string(), "/".to_string()),
            ("tmp".to_string(), "/tmp".to_string())
        ]
    );
    assert_eq!(fmt_size(640), "640 B");
    assert_eq!(fmt_size(84 * 1024), "84 KB");
    assert_eq!(fmt_size(1024 * 1024 + 200 * 1024), "1.2 MB");
    assert_eq!(text_lines("a\nb\nc", 40), 3);
    assert_eq!(text_lines(&"x".repeat(100), 40), 3, "a long line wraps");

    // The one clash a copy is allowed to make, under a name that is free.
    assert_eq!(copy_name("notes.txt", 1), "notes copy.txt");
    assert_eq!(copy_name("notes.txt", 2), "notes copy 2.txt");
    assert_eq!(copy_name("2026", 1), "2026 copy");
    assert_eq!(
        copy_name(".zshrc", 1),
        ".zshrc copy",
        "a dot-file is all extension"
    );
}

#[test]
fn the_path_field_completes_one_segment_at_a_time() {
    let _alone = alone();
    let (s, slot) = home();
    let c = with_dir(&s, slot, |d| d.completion());

    // Before a slash: the two roots.
    let ctx = c.context("", 0).expect("a context");
    assert_eq!(ctx.dir, None);
    assert_eq!(values(&c.offer(s.store(), &ctx)), ["~/", "/"]);

    // After one: the entries of the directory the segments before it name,
    // matched as a prefix — a directory with its slash, so the next offer
    // opens at once.
    let ctx = c.context("~/Down", 6).expect("a context");
    assert_eq!(ctx.dir.as_deref(), Some("~"));
    assert_eq!(ctx.prefix, "Down");
    let offer = c.offer(s.store(), &ctx);
    assert_eq!(values(&offer), ["Downloads/"]);
    assert_eq!(
        c.splice("~/Down", 6, &ctx, &offer[0]),
        ("~/Downloads/".to_string(), 12)
    );

    // A dot-file is offered only once the dot is typed.
    let ctx = c.context("~/", 2).expect("a context");
    assert!(!values(&c.offer(s.store(), &ctx)).contains(&".zshrc".to_string()));
    let ctx = c.context("~/.z", 4).expect("a context");
    assert_eq!(values(&c.offer(s.store(), &ctx)), [".zshrc"]);
}

/// What an offer would put into the field.
fn values(offer: &[kernel::richtable::Suggestion]) -> Vec<String> {
    offer.iter().map(|s| s.value.clone()).collect()
}

// -- what a card shows ---------------------------------------------------------

/// The kind decides whether anything is read at all; the caps decide how
/// much. A card over a 38 MB disk image costs one `stat`, and a picture
/// past the cap is the card's lines alone rather than a pause.
#[test]
fn a_card_reads_what_its_kind_is_worth_and_no_more() {
    let _alone = alone();
    let never = |_: usize| -> Option<Vec<u8>> { panic!("read for a kind with no preview") };
    assert_eq!(preview_of(FileKind::Pdf, "a.pdf", 96, never), Preview::None);
    assert_eq!(
        preview_of(FileKind::Archive, "a.zip", 400, never),
        Preview::None
    );
    assert_eq!(preview_of(FileKind::Dir, "d", 0, never), Preview::None);
    // A `.gif` is an image the card cannot decode: not read either.
    assert_eq!(preview_of(FileKind::Image, "a.gif", 12, never), Preview::None);
    // Nor is one past the cap: 24 MB of picture nobody would see.
    assert_eq!(
        preview_of(FileKind::Image, "big.png", 24 * 1024 * 1024, never),
        Preview::None
    );

    // What is read is read once, and only as far as its own cap.
    let asked = std::cell::Cell::new((0, 0));
    let read = |max: usize| {
        asked.set((asked.get().0 + 1, max));
        Some(b"hello".to_vec())
    };
    assert_eq!(
        preview_of(FileKind::Text, "a.txt", 5, read),
        Preview::Text("hello".into())
    );
    assert_eq!(asked.get(), (1, TEXT_PREVIEW_MAX));
    let read = |max: usize| {
        asked.set((asked.get().0 + 1, max));
        Some(b"\x89PNG".to_vec())
    };
    assert_eq!(
        preview_of(FileKind::Image, "a.png", 4, read),
        Preview::Image(b"\x89PNG".to_vec())
    );
    assert_eq!(asked.get(), (2, IMAGE_PREVIEW_MAX));
    // Bytes that could not be had: no preview, and nothing decided.
    assert_eq!(preview_of(FileKind::Text, "a.txt", 5, |_| None), Preview::None);
}

/// The card's wish reads a picture's size off its header alone, so the rows
/// are asked for before anything is decoded.
#[test]
fn a_picture_is_measured_by_its_header() {
    let _alone = alone();
    let icon = kernel::caps::demo::bytes_of("~/Pictures/fold-cover.png").expect("the fixture");
    assert_eq!(image_size(&icon), Some((32, 32)));
    // A JPEG's size sits in its first frame marker, past the tables.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
    jpeg.extend_from_slice(&[
        0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x01, 0x90, 0x02, 0x80, 0x01, 0x01, 0x11, 0x00,
    ]);
    assert_eq!(image_size(&jpeg), Some((640, 400)));
    assert_eq!(image_size(b"not a picture"), None);
    assert_eq!(image_size(&[]), None);
    assert_eq!(image_format("a.jpeg"), Some(ImageFormat::Jpeg));
    assert_eq!(image_format("a.png"), Some(ImageFormat::Png));
    assert_eq!(image_format("a.gif"), None, "an image the card cannot draw");

    // Drawn at the text's width, a square picture is that width tall: at 60
    // characters, 60 · 0.8 / 2.0 = 24 lines. Half as tall, half as many.
    assert_eq!(image_lines(60, 32, 32), 24.0);
    assert_eq!(image_lines(60, 64, 32), 12.0);
    assert_eq!(image_lines(60, 32, 64), 48.0);
}

/// The card over each of the three previews, on the demo tree: what it
/// says, what it holds, and the rows it asks for.
#[test]
fn a_card_previews_a_text_file_a_picture_or_neither() {
    let _alone = alone();
    let (mut s, slot) = home();

    let card = |s: &mut Session, path: &str| {
        go(s, Nav::Preview {
            from: slot,
            id: Card::id(path),
        });
        s.joined_child(slot).expect("the card")
    };

    // A text file: its first 64 KiB, and the rows the reading needs.
    let c = card(&mut s, "~/notes.md");
    with_card(&s, c, |c| {
        assert!(matches!(c.preview(), Preview::Text(t) if t.starts_with("# notes")));
        assert_eq!(c.pixels(), None);
        assert_eq!(c.kind_line(), "text · 2.1 KB");
    });
    assert_eq!(inst(&s, c).borrow().wish(60), (4, 3), "seven lines of chrome");

    // A picture: the bytes, decoded by whoever draws them, and a wish off
    // its aspect at the text's width — 32 × 32 at 60 characters is 24
    // lines, which with the chrome is more than a card's floor.
    let c = card(&mut s, "~/Pictures/fold-cover.png");
    with_card(&s, c, |c| {
        assert_eq!(c.kind_word(), "image");
        assert_eq!(c.pixels(), Some((32, 32)));
        assert!(
            c.image().is_some_and(|b| b.starts_with(b"\x89PNG\r\n\x1a\n")),
            "the fixture's own bytes, whatever the name says"
        );
    });
    assert_eq!(inst(&s, c).borrow().wish(60), (4, 6));
    assert_eq!(inst(&s, c).borrow().wish(20), (4, 3), "a narrow column, a small picture");

    // A `.jpg` the fixture wrote as a PNG is read all the same: the name
    // says whether to read, the bytes say how to decode.
    let c = card(&mut s, "~/Downloads/2026/photo-lisbon.jpg");
    with_card(&s, c, |c| {
        assert!(c.image().is_some_and(|b| b.starts_with(b"\x89PNG\r\n\x1a\n")));
    });

    // Neither: the card's lines, and the *open* that shows it.
    let c = card(&mut s, "~/Downloads/report-q3.pdf");
    with_card(&s, c, |c| {
        assert_eq!(c.preview(), &Preview::None);
        assert_eq!(c.kind_line(), "pdf · 1.2 MB");
    });
    assert_eq!(inst(&s, c).borrow().wish(60), (4, 3));
}

/// `open` hands the path to the OS and changes nothing of ours, so no
/// listing goes stale behind it.
#[test]
fn open_hands_the_path_to_the_os() {
    let _alone = alone();
    let (mut s, slot) = home();
    go(&mut s, Nav::Preview {
        from: slot,
        id: Card::id("~/notes.md"),
    });
    let card = s.joined_child(slot).expect("the card");
    let was = FILES.writes();
    s.take_notes();
    run(&mut s, card, "files.open");
    assert_eq!(
        s.notes().iter().map(|n| n.msg.clone()).collect::<Vec<_>>(),
        ["opened “notes.md”"],
        "the disk took it, so the card says so"
    );
    assert_eq!(FILES.writes(), was, "nothing of ours changed");
    assert_eq!(
        with_card(&s, card, |c| c.status().map(str::to_string)),
        None,
        "and nothing was refused"
    );
    // A directory panel wears no `open`: what a directory opens is itself,
    // as a list, and that is a row's business.
    assert!(!bar(&s, slot).contains(&"files.open"));
}

/// A name claims a media type: a short table, and the honest answer past
/// it. Nothing in this build attaches yet; the card and a part will read
/// the same table when one does.
#[test]
fn a_name_claims_a_media_type() {
    let _alone = alone();
    assert_eq!(mime_of("q3.CSV"), "text/csv");
    assert_eq!(mime_of("report-q3.pdf"), "application/pdf");
    assert_eq!(mime_of("photo.jpeg"), "image/jpeg");
    assert_eq!(mime_of("fold-cover.png"), "image/png");
    assert_eq!(mime_of("logs.tar.gz"), "application/gzip");
    assert_eq!(mime_of("superapp.db"), "application/octet-stream");
    assert_eq!(mime_of("noextension"), "application/octet-stream");
}

// -- the app itself ------------------------------------------------------------

#[test]
fn the_app_owns_two_tags_and_one_root() {
    let _alone = alone();
    let apps = kernel::app::Apps::new(APPS);
    assert!(apps.kind(Dir::TAG).is_some());
    assert!(apps.kind(Card::TAG).is_some());
    let roots = apps.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].id, Dir::id(HOME));
    // The public API, as another app reaches it: the registry by type,
    // then the clipboard, and `None` where this build has no files app.
    let files = apps.get_as::<super::Files>().expect("the files app");
    assert!(files.clipboard().is_empty());
    files.set(Op::Move, vec!["~/notes.md".to_string()]);
    assert_eq!(files.clipboard().verb, Op::Move);
    assert_eq!(files.clipboard().what(), "“notes.md”");
    files.clear();
    assert!(apps.get("files").is_some());
    assert!(apps.get("mail").is_none());
}

#[test]
fn a_bar_wears_no_letter_twice() {
    let _alone = alone();
    let (mut s, slot) = home();
    FILES.set(Op::Copy, vec!["~/notes.md".to_string()]);
    let e = row(&s, slot, "Downloads");
    let nav = with_dir(&s, slot, |d| d.preview(&e));
    go(&mut s, nav);
    let child = s.joined_child(slot).expect("a joined child");
    with_dir(&s, child, |d| d.observe(&s));
    for slot in [slot, child] {
        let verbs = inst(&s, slot).borrow().verbs();
        let mut seen: Vec<char> = Vec::new();
        for v in &verbs {
            let Some(c) = v.accel else { continue };
            assert!(!seen.contains(&c), "two verbs wear cmd+{c}");
            seen.push(c);
        }
    }
    FILES.clear();
}
