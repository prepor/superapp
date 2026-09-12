use super::*;
use engine::{Engine, Mark};
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
    let verbs = session.panel_verbs(first);
    crate::shell::bar::check(&verbs);
    let width = verbs.iter().find(|v| v.id == "panel.half_width").unwrap();
    assert_eq!(
        (width.label.as_str(), width.accel),
        ("half width", Some(kernel::session::WIDTH_ACCEL))
    );
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

/// The cells of a frame that carry a mark, as `(row, column, current)`.
fn marks(engine: &mut Engine) -> Vec<(usize, usize, bool)> {
    let frame = engine.frame().unwrap();
    let mut marks = Vec::new();
    for (y, row) in frame.rows.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            match cell.mark {
                Mark::None => {}
                Mark::Match => marks.push((y, x, false)),
                Mark::Current => marks.push((y, x, true)),
            }
        }
    }
    marks
}

#[test]
fn finding_marks_every_match_selects_the_newest_and_walks_around() {
    let mut engine = Engine::empty(20, 5).unwrap();
    engine.term.vt_write(b"alpha beta\r\nBETA gamma\r\nbeta");
    engine.find("beta").unwrap();
    assert_eq!(engine.found(), Some((3, 3)));
    let mut expected: Vec<_> = (6..10).map(|x| (0, x, false)).collect();
    expected.extend((0..4).map(|x| (1, x, false)));
    expected.extend((0..4).map(|x| (2, x, true)));
    assert_eq!(marks(&mut engine), expected);
    // The current match is the selection, so it can be copied.
    assert_eq!(engine.copy().unwrap(), "beta");
    engine.find_step(true).unwrap();
    assert_eq!(engine.found(), Some((2, 3)));
    assert_eq!(engine.copy().unwrap(), "BETA");
    engine.find_step(true).unwrap();
    assert_eq!(engine.found(), Some((1, 3)));
    engine.find_step(true).unwrap();
    assert_eq!(engine.found(), Some((3, 3)), "older wraps to the last");
    engine.find_step(false).unwrap();
    assert_eq!(engine.found(), Some((1, 3)), "newer wraps to the first");
    engine.find("zzz").unwrap();
    assert_eq!(engine.found(), Some((0, 0)));
    assert!(marks(&mut engine).is_empty());
    engine.find("").unwrap();
    assert_eq!(engine.found(), None);
    engine.find_clear();
    assert!(marks(&mut engine).is_empty());
}

#[test]
fn a_match_in_the_scrollback_is_scrolled_into_view_and_kept_through_output() {
    let mut engine = Engine::empty(10, 3).unwrap();
    for i in 0..20 {
        engine.term.vt_write(format!("line {i}\r\n").as_bytes());
    }
    engine.find("line 2").unwrap();
    assert_eq!(engine.found(), Some((1, 1)));
    let frame = engine.frame().unwrap();
    let shown: Vec<String> = frame
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|c| c.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    assert_eq!(shown, vec!["line 1", "line 2", "line 3"]);
    assert_eq!(
        marks(&mut engine),
        (0..6).map(|x| (1, x, true)).collect::<Vec<_>>()
    );
    // More output does not lose the match or move the viewport off it.
    engine.term.vt_write(b"line 20\r\nline 21\r\n");
    engine.term.vt_write(b"\x1b[?1049h\x1b[?1049l");
    assert_eq!(
        marks(&mut engine),
        (0..6).map(|x| (1, x, true)).collect::<Vec<_>>()
    );
    engine.find_step(false).unwrap();
    assert_eq!(engine.found(), Some((1, 1)));
    assert_eq!(engine.copy().unwrap(), "line 2");
    // A query that also matches the new output counts it, and the current
    // match stays put while the query still matches there.
    engine.find("line 2").unwrap();
    engine.find("line").unwrap();
    assert_eq!(engine.found(), Some((3, 22)));
}

#[test]
fn matches_land_on_cells_through_wide_characters_and_graphemes() {
    let mut engine = Engine::empty(12, 3).unwrap();
    engine.term.vt_write("日本 x e\u{301}y".as_bytes());
    engine.find("x").unwrap();
    assert_eq!(marks(&mut engine), vec![(0, 5, true)]);
    engine.find("y").unwrap();
    assert_eq!(marks(&mut engine), vec![(0, 8, true)]);
    assert_eq!(engine.copy().unwrap(), "y");
    engine.find("本").unwrap();
    assert_eq!(marks(&mut engine), vec![(0, 2, true), (0, 3, true)]);
    assert_eq!(engine.copy().unwrap(), "本");
    engine.find("É").unwrap();
    assert_eq!(engine.found(), Some((0, 0)));
    engine.find("e\u{301}Y").unwrap();
    assert_eq!(marks(&mut engine), vec![(0, 7, true), (0, 8, true)]);
}

#[test]
fn refining_the_query_keeps_the_current_match_where_it_still_matches() {
    let mut engine = Engine::empty(20, 5).unwrap();
    engine.term.vt_write(b"ab\r\nab\r\nab");
    engine.find("a").unwrap();
    assert_eq!(engine.found(), Some((3, 3)));
    engine.find_step(true).unwrap();
    assert_eq!(engine.found(), Some((2, 3)));
    engine.find("ab").unwrap();
    assert_eq!(engine.found(), Some((2, 3)));
    assert_eq!(engine.copy().unwrap(), "ab");
    engine.find("b").unwrap();
    assert_eq!(
        engine.found(),
        Some((3, 3)),
        "no match starts where the last did"
    );
}

#[test]
fn the_bar_offers_find_and_running_it_raises_the_field() {
    static APPS: &[&dyn App] = &[&TERMINAL];
    let mut session = Session::fake(APPS);
    session.nav(Nav::Open {
        from: 0,
        id: PanelId::bare(TAG),
        fresh: true,
    });
    session.settle();
    let slot = session.focus().unwrap();
    let instance = session.panel(slot).unwrap();
    {
        let mut panel = instance.borrow_mut();
        let terminal = panel.as_any().downcast_mut::<TerminalPanel>().unwrap();
        assert!(
            terminal.verbs().is_empty(),
            "nothing to find in before the shell starts"
        );
        terminal.start();
        crate::shell::bar::check(&terminal.verbs());
        let find = terminal
            .verbs()
            .into_iter()
            .find(|v| v.id == "terminal.find")
            .unwrap();
        assert_eq!((find.label.as_str(), find.accel), ("find", Some('f')));
        assert!(terminal.find.is_none());
    }
    instance.borrow_mut().run("terminal.find", &mut session);
    let mut panel = instance.borrow_mut();
    let terminal = panel.as_any().downcast_mut::<TerminalPanel>().unwrap();
    let find = terminal.find.as_ref().unwrap();
    assert!(find.land && find.typing);
    terminal.run("terminal.restart", &mut session);
    assert!(
        terminal.find.is_none(),
        "a new shell starts without the bar"
    );
}
