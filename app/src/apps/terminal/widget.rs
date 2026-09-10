use crate::shell::draw::DrawFlat;
use crate::shell::hits::Hit as ShellHit;
use crate::shell::hosted::PanelProps;
use kernel::session::Session;
use libghostty_vt::key::{Key, Mods};
use libghostty_vt::render::CursorVisualStyle;
use libghostty_vt::style::{RgbColor, Underline};
use makepad_widgets::*;

use super::{engine, TerminalPanel};

const PAD: f64 = 10.0;
const STATUS_H: f64 = 24.0;

#[derive(Script, ScriptHook, WidgetRef, WidgetSet, WidgetRegister)]
pub struct TerminalView {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_fill: DrawFlat,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_bold: DrawText,
    #[live]
    draw_italic: DrawText,
    #[live]
    draw_status: DrawText,
    #[rust]
    area: Area,
    #[rust]
    cell: DVec2,
    #[rust]
    selecting: bool,
    #[rust]
    scroll: f64,
    #[rust]
    frame: Option<engine::Frame>,
    #[rust]
    focused: bool,
    #[rust]
    focus_next_frame: NextFrame,
}

impl WidgetNode for TerminalView {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
}

impl TerminalView {
    fn owns(&self, cx: &Cx, props: &PanelProps, point: DVec2) -> bool {
        self.area.clipped_rect(cx).contains(point)
            && props
                .hits
                .at(point)
                .is_some_and(|hit| hit.slot == Some(props.slot))
    }

    fn point(&self, cx: &Cx, abs: DVec2) -> (u16, u16) {
        let pos = abs - self.area.rect(cx).pos - dvec2(PAD, PAD);
        (
            (pos.x / self.cell.x.max(1.0)).max(0.0) as u16,
            (pos.y / self.cell.y.max(1.0)).max(0.0) as u16,
        )
    }

    fn fill(&mut self, cx: &mut Cx2d, rect: Rect, color: RgbColor) {
        self.draw_fill.color = rgba(color);
        self.draw_fill.draw_abs(cx, rect);
    }

    fn show_text_input(&self, cx: &mut Cx) {
        let rect = self.area.rect(cx);
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
            return;
        }
        let cursor = self.frame.as_ref().and_then(|frame| frame.cursor);
        let pos = dvec2(PAD, PAD)
            + cursor.map_or(DVec2::default(), |c| {
                dvec2(f64::from(c.x) * self.cell.x, f64::from(c.y) * self.cell.y)
            });
        cx.show_text_ime_with_config(
            self.area,
            Rect {
                pos,
                size: self.cell,
            },
            TextInputConfig {
                is_multiline: true,
                ..Default::default()
            },
        );
    }

    fn focus_input(&self, cx: &mut Cx) {
        cx.set_key_focus(self.area);
        self.show_text_input(cx);
    }
}

impl Widget for TerminalView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>() else {
            return;
        };
        if self.focus_next_frame.is_event(event).is_some() {
            self.focus_next_frame = NextFrame::default();
            if props.has_keyboard {
                self.focus_input(cx);
            }
        }
        let mut panel = props.panel.borrow_mut();
        let Some(panel) = panel.as_any().downcast_mut::<TerminalPanel>() else {
            return;
        };
        let Some(engine) = &mut panel.engine else {
            return;
        };
        let mut changed = engine.poll();
        let result = match event {
            Event::KeyDown(key) => {
                if props.has_keyboard {
                    self.focus_input(cx);
                }
                if key.modifiers.logo {
                    match key.key_code {
                        KeyCode::KeyA => engine.select_all(),
                        KeyCode::KeyC => engine.copy().map(|text| cx.copy_to_clipboard(&text)),
                        _ => Ok(()),
                    }
                } else if let Some((physical, text)) = physical_key(key) {
                    let mut mods = Mods::empty();
                    if key.modifiers.shift {
                        mods |= Mods::SHIFT;
                    }
                    if key.modifiers.control {
                        mods |= Mods::CTRL;
                    }
                    if key.modifiers.alt {
                        mods |= Mods::ALT;
                    }
                    changed = true;
                    engine.key(physical, mods, text.as_deref())
                } else {
                    Ok(())
                }
            }
            Event::TextInput(text) => {
                if props.has_keyboard {
                    self.focus_input(cx);
                }
                changed = true;
                engine.text(&text.input, text.was_paste)
            }
            Event::TextCopy(copy) if props.has_keyboard && cx.key_focus() == self.area => engine
                .copy()
                .map(|text| *copy.response.borrow_mut() = Some(text)),
            Event::Signal => Ok(()),
            Event::KeyFocus(_) => {
                if props.has_keyboard && cx.has_key_focus(self.area) {
                    self.show_text_input(cx);
                }
                Ok(())
            }
            Event::Scroll(e) if self.owns(cx, props, e.abs) && !e.handled_y.get() => {
                e.handled_y.set(true);
                self.scroll += e.scroll.y / self.cell.y.max(1.0);
                let lines = self.scroll.trunc() as isize;
                self.scroll -= lines as f64;
                if lines != 0 {
                    engine.scroll(lines);
                    changed = true;
                }
                Ok(())
            }
            Event::MouseDown(e) if !self.owns(cx, props, e.abs) => Ok(()),
            Event::WindowLostFocus(_) | Event::Background => {
                self.selecting = false;
                Ok(())
            }
            _ => match event.hits(cx, self.area) {
                Hit::FingerDown(e) => {
                    if props.has_keyboard {
                        self.focus_input(cx);
                    }
                    self.selecting = true;
                    let (x, y) = self.point(cx, e.abs);
                    changed = true;
                    engine.select(x, y, true, e.tap_count)
                }
                Hit::FingerMove(e) if self.selecting => {
                    let (x, y) = self.point(cx, e.abs);
                    changed = true;
                    engine.select(x, y, false, 1)
                }
                Hit::FingerUp(_) => {
                    self.selecting = false;
                    Ok(())
                }
                _ => Ok(()),
            },
        };
        if let Err(error) = result {
            panel.error = Some(error.to_string());
        }
        // Copy and select-all can change the selection too.
        if changed || matches!(event, Event::KeyDown(_) | Event::KeyFocus(_)) {
            self.frame = None;
            self.area.redraw(cx);
            if let Some(s) = scope.data.get_mut::<Session>() {
                s.redraw();
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return DrawStep::done();
        };
        let focused = props.has_keyboard;
        let mut panel = props.panel.borrow_mut();
        let Some(panel) = panel.as_any().downcast_mut::<TerminalPanel>() else {
            return DrawStep::done();
        };
        if panel.engine.is_none() {
            self.frame = None;
        }
        panel.start();
        let rect = cx.peek_walk_turtle(walk);
        cx.begin_turtle(
            walk,
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        if let Some(run) = self.draw_text.prepare_single_line_run(cx, "M") {
            self.cell = dvec2(
                f64::from(run.width_in_lpxs).max(1.0),
                f64::from(run.ascender_in_lpxs - run.descender_in_lpxs)
                    .ceil()
                    .max(1.0),
            );
        }
        let cols = ((rect.size.x - PAD * 2.0) / self.cell.x.max(1.0))
            .floor()
            .clamp(2.0, 1000.0) as u16;
        if let Some(engine) = &mut panel.engine {
            if engine.poll() {
                self.frame = None;
            }
        }
        let status_h = if panel.error.is_some()
            || panel
                .engine
                .as_ref()
                .and_then(|engine| engine.status())
                .is_some()
        {
            STATUS_H
        } else {
            0.0
        };
        let rows = ((rect.size.y - PAD * 2.0 - status_h) / self.cell.y.max(1.0))
            .floor()
            .clamp(1.0, 1000.0) as u16;
        if let Some(engine) = &mut panel.engine {
            match engine.resize(
                cols,
                rows,
                self.cell.x.ceil() as u32,
                self.cell.y.ceil() as u32,
            ) {
                Ok(true) => self.frame = None,
                Ok(false) => {}
                Err(error) => panel.error = Some(error.to_string()),
            }
            // Resizing can reflow even when there was no PTY output.
            if self.frame.is_none() {
                match engine.frame() {
                    Ok(frame) => self.frame = Some(frame),
                    Err(error) => panel.error = Some(error.to_string()),
                }
            }
            if focused != self.focused {
                let _ = engine.focus(focused);
                self.focused = focused;
            }
        }
        let background = self
            .frame
            .as_ref()
            .map_or(engine::BG, |frame| frame.background);
        self.fill(cx, rect, background);
        if let Some(frame) = self.frame.take() {
            for (y, row) in frame.rows.iter().enumerate() {
                let mut line = String::new();
                // Paint every background before any glyph, so a wide
                // character's trailing cell cannot cover half its ink.
                for (x, cell) in row.iter().enumerate() {
                    let pos = rect.pos
                        + dvec2(PAD + x as f64 * self.cell.x, PAD + y as f64 * self.cell.y);
                    let cursor = focused
                        && frame
                            .cursor
                            .is_some_and(|c| usize::from(c.x) == x && usize::from(c.y) == y);
                    let mut bg = cell.bg;
                    if cell.selected {
                        bg = RgbColor {
                            r: 61,
                            g: 80,
                            b: 111,
                        };
                    }
                    if cursor && frame.cursor_style == CursorVisualStyle::Block {
                        bg = frame.cursor_color;
                    }
                    if bg != background {
                        self.fill(
                            cx,
                            Rect {
                                pos,
                                size: self.cell,
                            },
                            bg,
                        );
                    }
                }
                for (x, cell) in row.iter().enumerate() {
                    let pos = rect.pos
                        + dvec2(PAD + x as f64 * self.cell.x, PAD + y as f64 * self.cell.y);
                    let size = dvec2(self.cell.x * if cell.wide { 2.0 } else { 1.0 }, self.cell.y);
                    let cursor = focused
                        && frame
                            .cursor
                            .is_some_and(|c| usize::from(c.x) == x && usize::from(c.y) == y);
                    let mut fg = cell.fg;
                    if cursor && frame.cursor_style == CursorVisualStyle::Block {
                        fg = background;
                    }
                    let mut color = rgba(fg);
                    if cell.style.faint {
                        color.w = 0.5;
                    }
                    let text = if cell.style.bold {
                        &mut self.draw_bold
                    } else if cell.style.italic {
                        &mut self.draw_italic
                    } else {
                        &mut self.draw_text
                    };
                    text.color = color;
                    text.draw_abs(cx, pos, &cell.text);
                    if cell.style.underline != Underline::None {
                        self.fill(
                            cx,
                            Rect {
                                pos: pos + dvec2(0.0, self.cell.y - 2.0),
                                size: dvec2(size.x, 1.0),
                            },
                            fg,
                        );
                    }
                    if cell.style.strikethrough {
                        self.fill(
                            cx,
                            Rect {
                                pos: pos + dvec2(0.0, self.cell.y * 0.55),
                                size: dvec2(size.x, 1.0),
                            },
                            fg,
                        );
                    }
                    if cursor && frame.cursor_style != CursorVisualStyle::Block {
                        let (offset, size) = if frame.cursor_style == CursorVisualStyle::Underline {
                            (dvec2(0.0, self.cell.y - 2.0), dvec2(size.x, 2.0))
                        } else {
                            (dvec2(0.0, 0.0), dvec2(2.0, self.cell.y))
                        };
                        self.fill(
                            cx,
                            Rect {
                                pos: pos + offset,
                                size,
                            },
                            frame.cursor_color,
                        );
                    }
                    if !cell.spacer {
                        if cell.text.is_empty() {
                            line.push(' ');
                        } else {
                            line.push_str(&cell.text);
                        }
                    }
                }
                if !line.trim().is_empty() {
                    props.hits.add_clipped(
                        format!("terminal output: {}", line.trim_end()),
                        Rect {
                            pos: rect.pos + dvec2(PAD, PAD + y as f64 * self.cell.y),
                            size: dvec2(rect.size.x - PAD * 2.0, self.cell.y),
                        },
                        rect,
                        MouseCursor::Text,
                        props.slot,
                    );
                }
            }
            self.frame = Some(frame);
        }
        let status = panel
            .error
            .as_deref()
            .or_else(|| panel.engine.as_ref().and_then(|engine| engine.status()));
        if let Some(status) = status {
            self.draw_status.draw_abs(
                cx,
                rect.pos + dvec2(PAD, rect.size.y - STATUS_H + 5.0),
                status,
            );
            // A resize or render error can first appear during this draw.
            if status_h == 0.0 {
                cx.redraw_all();
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        if focused {
            // Cocoa only emits TextInput while its input context is active.
            // Register on draw: enabling it in KeyDown loses the first key.
            self.show_text_input(cx);
            // Makepad commits key focus after events, not after drawing.
            if !cx.has_key_focus(self.area) && self.focus_next_frame == NextFrame::default() {
                self.focus_next_frame = cx.new_next_frame();
            }
        }
        props.hits.push(ShellHit::new(
            "terminal input",
            // Rect-area clipping is filled in when the parent turtle ends.
            // Reading clipped_rect here would register an empty input hit.
            rect,
            MouseCursor::Text,
            props.slot,
        ));
        DrawStep::done()
    }
}

fn rgba(c: RgbColor) -> Vec4f {
    vec4(
        f32::from(c.r) / 255.0,
        f32::from(c.g) / 255.0,
        f32::from(c.b) / 255.0,
        1.0,
    )
}

/// Printable text arrives through TextInput, including IME and Option
/// characters. KeyDown handles controls and keys without text exactly once.
fn physical_key(event: &KeyEvent) -> Option<(Key, Option<String>)> {
    let key = match event.key_code {
        KeyCode::ReturnKey => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab => Key::Tab,
        KeyCode::Escape => Key::Escape,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        KeyCode::Space if event.modifiers.control => Key::Space,
        KeyCode::LBracket if event.modifiers.control => Key::BracketLeft,
        KeyCode::RBracket if event.modifiers.control => Key::BracketRight,
        KeyCode::Backslash if event.modifiers.control => Key::Backslash,
        KeyCode::Minus if event.modifiers.control => Key::Minus,
        _ if event.modifiers.control => {
            let c = crate::shell::keys::key_char(event.key_code)?;
            // Ghostty uses the physical key for C0 controls, and the text
            // for the logical character on the user's keyboard layout.
            let key = match c {
                'a' => Key::A,
                'b' => Key::B,
                'c' => Key::C,
                'd' => Key::D,
                'e' => Key::E,
                'f' => Key::F,
                'g' => Key::G,
                'h' => Key::H,
                'i' => Key::I,
                'j' => Key::J,
                'k' => Key::K,
                'l' => Key::L,
                'm' => Key::M,
                'n' => Key::N,
                'o' => Key::O,
                'p' => Key::P,
                'q' => Key::Q,
                'r' => Key::R,
                's' => Key::S,
                't' => Key::T,
                'u' => Key::U,
                'v' => Key::V,
                'w' => Key::W,
                'x' => Key::X,
                'y' => Key::Y,
                'z' => Key::Z,
                _ => return None,
            };
            return Some((key, Some(c.to_string())));
        }
        _ => return None,
    };
    Some((key, None))
}
