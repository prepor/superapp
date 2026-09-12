use super::*;
use engine::Engine;
use kernel::layout::{Grid, LayoutOpts};
use kernel::nav::Nav;
use libghostty_vt::key::{Key, Mods};

fn text(engine: &mut Engine) -> String {
    engine
        .frame()
        .unwrap()
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    if cell.text.is_empty() {
                        " "
                    } else {
                        cell.text.as_str()
                    }
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn terminal_titles_use_program_titles_with_a_shell_fallback() {
    let mut engine = Engine::new(Mode::Fake).unwrap();
    assert_eq!(engine.title(), "demo shell");
    engine.term.vt_write(b"\x1b]2;nvim main.rs\x07");
    assert_eq!(engine.title(), "nvim main.rs");
    engine.term.vt_write(b"\x1b]2;\x07");
    assert_eq!(engine.title(), "demo shell");
}

#[test]
fn parses_colors_unicode_and_alternate_screen_then_reflows() {
    let mut engine = Engine::empty(20, 5).unwrap();
    engine
        .term
        .vt_write("\x1b[38;2;10;20;30m日e\u{301}\x1b[0m hello terminal".as_bytes());
    let frame = engine.frame().unwrap();
    assert_eq!(frame.rows[0][0].text, "日");
    assert!(frame.rows[0][0].wide);
    assert_eq!(frame.rows[0][2].text, "e\u{301}");
    assert_eq!(
        frame.rows[0][0].fg,
        libghostty_vt::style::RgbColor {
            r: 10,
            g: 20,
            b: 30
        }
    );
    engine.term.vt_write(b"\x1b[?1049h\x1b[Hscreen two");
    assert!(text(&mut engine).starts_with("screen two"));
    engine.term.vt_write(b"\x1b[?1049l");
    engine.resize(10, 5, 8, 16).unwrap();
    let output = text(&mut engine);
    assert!(output.contains("terminal"), "{output}");
    assert_eq!(engine.term.cols().unwrap(), 10);
    assert_eq!(engine.term.rows().unwrap(), 5);
}

#[test]
fn cursor_keys_follow_application_mode_and_ctrl_c_interrupts() {
    let mut engine = Engine::empty(80, 24).unwrap();
    assert_eq!(
        engine
            .encode_key(Key::ArrowUp, Mods::empty(), None)
            .unwrap(),
        b"\x1b[A"
    );
    engine.term.vt_write(b"\x1b[?1h");
    assert_eq!(
        engine
            .encode_key(Key::ArrowUp, Mods::empty(), None)
            .unwrap(),
        b"\x1bOA"
    );
    assert_eq!(
        engine.encode_key(Key::C, Mods::CTRL, Some("c")).unwrap(),
        b"\x03"
    );
    assert_eq!(
        engine.encode_key(Key::Enter, Mods::empty(), None).unwrap(),
        b"\r"
    );
}

#[test]
fn selections_copy_graphemes_and_wrapped_text() {
    let mut engine = Engine::empty(8, 4).unwrap();
    engine.term.vt_write("hello world!\r\n日本語".as_bytes());
    engine.select(0, 0, true, 1).unwrap();
    engine.select(3, 1, false, 1).unwrap();
    assert_eq!(engine.copy().unwrap(), "hello world!");
    engine.select(0, 2, true, 2).unwrap();
    assert_eq!(engine.copy().unwrap(), "日");
    engine.select(0, 2, true, 1).unwrap();
    engine.select(5, 2, false, 1).unwrap();
    assert_eq!(engine.copy().unwrap(), "日本語");
}

#[test]
fn output_replies_and_bracketed_paste_are_not_plain_text() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut engine = Engine::empty(80, 24).unwrap();
    let replies = Rc::new(RefCell::new(Vec::new()));
    let captured = replies.clone();
    engine
        .term
        .on_pty_write(move |_, bytes| captured.borrow_mut().extend_from_slice(bytes))
        .unwrap();
    engine.term.vt_write(b"\x1b[6n");
    assert_eq!(*replies.borrow(), b"\x1b[1;1R");
    let mut input = b"one\ntwo\x1b[201~".to_vec();
    let mut output = vec![0; input.len() + 12];
    let n = libghostty_vt::paste::encode(&mut input, true, &mut output).unwrap();
    assert_eq!(&output[..n], b"\x1b[200~one\ntwo [201~\x1b[201~");
}

#[test]
fn half_and_full_width_keep_the_instance_and_resize_independently() {
    static APPS: &[&dyn App] = &[&TERMINAL];
    let mut session = Session::fake(APPS);
    session.set_viewport((1440.0, 900.0));
    session.nav(Nav::Open {
        from: 0,
        id: PanelId::bare(TAG),
        fresh: true,
    });
    session.settle();
    let first = session.focus().unwrap();
    session.nav(Nav::Open {
        from: first,
        id: PanelId::bare(TAG),
        fresh: true,
    });
    session.settle();
    let second = session.focus().unwrap();
    let instance = session.panel(first).unwrap();
    instance
        .borrow_mut()
        .as_any()
        .downcast_mut::<TerminalPanel>()
        .unwrap()
        .start();
    {
        let mut panel = instance.borrow_mut();
        panel
            .as_any()
            .downcast_mut::<TerminalPanel>()
            .unwrap()
            .engine
            .as_mut()
            .unwrap()
            .text("echo keep me", false)
            .unwrap();
    }
    session.set_panel_width(first, PanelWidth::Full);
    assert!(std::rc::Rc::ptr_eq(
        &instance,
        &session.panel(first).unwrap()
    ));
    assert_eq!(session.ws().widths[&first], PanelWidth::Full);
    assert_eq!(session.ws().widths[&second], PanelWidth::Half);
    assert!(session
        .panel_verbs(first)
        .iter()
        .any(|v| v.id == "panel.half_width"));
    let scene = session
        .ws()
        .clone()
        .scene((1440.0, 900.0), LayoutOpts::default());
    let full = scene
        .slots
        .iter()
        .find(|slot| slot.id == first)
        .unwrap()
        .rect;
    let half = scene
        .slots
        .iter()
        .find(|slot| slot.id == second)
        .unwrap()
        .rect;
    assert!(full.w > half.w * 1.9);
    assert_eq!(full.h, half.h);
    assert!(full.h > 850.0);
    assert_eq!(PanelWidth::Half.units(Grid { w: 8, h: 4 }), 4);
    assert_eq!(PanelWidth::Full.units(Grid { w: 8, h: 4 }), 8);
    let mut panel = instance.borrow_mut();
    let terminal = panel.as_any().downcast_mut::<TerminalPanel>().unwrap();
    assert!(text(terminal.engine.as_mut().unwrap()).contains("echo keep me"));
    assert_eq!(terminal.persist().arg(0), Some("full"));
}

#[test]
fn a_new_terminal_gets_a_column_when_the_next_one_is_half_used() {
    static APPS: &[&dyn App] = &[&TERMINAL];
    let mut session = Session::fake(APPS);
    session.nav(Nav::Open {
        from: 0,
        id: PanelId::bare(Tag("left")),
        fresh: true,
    });
    session.settle();
    let left = session.focus().unwrap();
    session.nav(Nav::Open {
        from: left,
        id: PanelId::bare(Tag("right")),
        fresh: true,
    });
    session.settle();
    let right = session.focus().unwrap();
    session.nav(Nav::Open {
        from: left,
        id: PanelId::bare(TAG),
        fresh: true,
    });
    session.settle();
    let terminal = session.focus().unwrap();
    assert_eq!(session.ws().columns.len(), 3);
    assert_eq!(session.ws().columns[1].slots, vec![terminal]);
    assert_eq!(session.ws().columns[2].slots, vec![right]);
}

#[test]
fn shared_sessions_keep_input_and_scrollback_without_a_panel_and_stay_in_one_store() {
    static APPS: &[&dyn App] = &[&TERMINAL];
    let first = Session::fake(APPS);
    let second = Session::fake(APPS);
    let handle = create_session(
        first.store(),
        Mode::Fake,
        std::path::Path::new("/workspace/one"),
    )
    .unwrap();
    handle.input("echo persistent output\r").unwrap();
    let output = kernel::runtime::block_on(handle.read()).unwrap();
    assert!(output.contains("persistent output"), "{output}");
    assert!(get_session(second.store(), &handle.id).is_none());
    let id = handle.id.clone();
    drop(handle);
    let retained = get_session(first.store(), &id).unwrap();
    assert!(kernel::runtime::block_on(retained.read())
        .unwrap()
        .contains("persistent output"));
    assert_eq!(retained.cwd, std::path::Path::new("/workspace/one"));
    assert!(close_session(first.store(), &id));
    assert!(get_session(first.store(), &id).is_none());
}

#[test]
fn a_shared_session_moves_into_a_panel_and_closing_only_stops_that_session() {
    static APPS: &[&dyn App] = &[&TERMINAL];
    let mut session = Session::fake(APPS);
    let moved = create_session(
        session.store(),
        Mode::Fake,
        std::path::Path::new("/workspace/project"),
    )
    .unwrap();
    moved.input("echo moved intact\r").unwrap();
    let replacement = create_session(session.store(), Mode::Fake, &moved.cwd).unwrap();
    session.nav(Nav::Open {
        from: 0,
        id: session_panel_id(&moved),
        fresh: false,
    });
    session.settle();
    let slot = session.focus().unwrap();
    {
        let panel = session.panel(slot).unwrap();
        let mut panel = panel.borrow_mut();
        let terminal = panel.as_any().downcast_mut::<TerminalPanel>().unwrap();
        assert_eq!(terminal.shared.as_ref().unwrap().id, moved.id);
        assert_eq!(terminal.persist().arg(2), Some("/workspace/project"));
        assert!(
            terminal.engine.is_none(),
            "Moving cannot spawn a second panel-owned shell"
        );
    }
    assert!(kernel::runtime::block_on(moved.read())
        .unwrap()
        .contains("moved intact"));
    assert!(!kernel::runtime::block_on(replacement.read())
        .unwrap()
        .contains("moved intact"));
    session.nav(Nav::Close { slot, label: None });
    session.settle();
    assert!(get_session(session.store(), &moved.id).is_none());
    assert!(get_session(session.store(), &replacement.id).is_some());
    replacement.input("echo still embedded\r").unwrap();
    assert!(kernel::runtime::block_on(replacement.read())
        .unwrap()
        .contains("still embedded"));
    close_session(session.store(), &replacement.id);
}
