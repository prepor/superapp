//! Ghostty owns the VT state; the UI owns this engine and paints snapshots.
//! Only a bounded batch of PTY bytes is parsed per event, so a flooding
//! child cannot monopolize the workspace. No process I/O runs here.

use std::sync::atomic::Ordering;

use kernel::app::Mode;
use libghostty_vt::key;
use libghostty_vt::render::{CellIterator, CursorViewport, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::{CellWide, TrackedGridRef};
use libghostty_vt::selection::{FormatOptions, SelectLineOptions, SelectWordOptions, Selection};
use libghostty_vt::style::{RgbColor, Style};
use libghostty_vt::terminal::{Mode as VtMode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use makepad_widgets::SignalToUI;
use portable_pty::PtySize;

use super::process::{Command, Output, Process};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub(super) const FG: RgbColor = RgbColor {
    r: 220,
    g: 224,
    b: 230,
};
pub(super) const BG: RgbColor = RgbColor {
    r: 24,
    g: 27,
    b: 32,
};

pub(super) struct Cell {
    pub text: String,
    pub fg: RgbColor,
    pub bg: RgbColor,
    pub style: Style,
    pub wide: bool,
    pub spacer: bool,
    pub selected: bool,
}

pub(super) struct Frame {
    pub rows: Vec<Vec<Cell>>,
    pub cursor: Option<CursorViewport>,
    pub cursor_style: CursorVisualStyle,
    pub cursor_color: RgbColor,
    pub background: RgbColor,
}

pub(super) struct Engine {
    pub term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    encoder: key::Encoder<'static>,
    process: Option<Process>,
    size: PtySize,
    state: String,
    command: String,
    done: bool,
    demo: bool,
    demo_line: String,
    anchor: Option<TrackedGridRef>,
}

impl Engine {
    pub fn new(mode: Mode) -> Result<Self> {
        let mut engine = Self::empty(80, 24)?;
        match mode {
            Mode::Real => {
                let process = Process::spawn(engine.size)?;
                let input = process.input.clone();
                engine.term.on_pty_write(move |_, bytes| {
                    let _ = input.send(Command::Write(bytes.to_vec()));
                })?;
                engine.process = Some(process);
            }
            Mode::Fake | Mode::Deny => {
                engine.demo = true;
                engine.command = "demo shell".into();
                engine.term.vt_write(b"\x1b[1msuperapp terminal\x1b[0m\r\n\r\nDemo shell: try echo, clear, or stty size.\r\n\r\n\x1b[32m$\x1b[0m ");
            }
        }
        Ok(engine)
    }

    pub fn empty(cols: u16, rows: u16) -> Result<Self> {
        let mut term = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 4000,
        })?;
        term.set_default_fg_color(Some(FG))?;
        term.set_default_bg_color(Some(BG))?;
        Ok(Self {
            term,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            encoder: key::Encoder::new()?,
            process: None,
            size: PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            },
            state: String::new(),
            command: "shell".into(),
            done: false,
            demo: false,
            demo_line: String::new(),
            anchor: None,
        })
    }

    pub fn title(&self) -> &str {
        self.term
            .title()
            .ok()
            .filter(|title| !title.is_empty())
            .unwrap_or(&self.command)
    }
    pub fn finished(&self) -> bool {
        self.done
    }
    pub fn status(&self) -> Option<&str> {
        self.done.then_some(self.state.as_str())
    }

    pub fn poll(&mut self) -> bool {
        let Some(process) = &self.process else {
            return false;
        };
        // Re-arm before draining: bytes arriving during this pass wake the
        // next one, including the race with an empty queue.
        process.dirty.store(false, Ordering::Release);
        let mut changed = false;
        for _ in 0..16 {
            let Ok(output) = process.output.try_recv() else {
                return changed;
            };
            changed = true;
            match output {
                Output::Ready => self.state.clear(),
                Output::Data(bytes) => self.term.vt_write(&bytes),
                Output::Foreground(name) => self.command = name,
                Output::Error(error) => {
                    self.state = error;
                    self.done = true;
                }
                Output::Exited(status) => {
                    self.state = format!("shell exited: {status}");
                    self.done = true;
                }
            }
        }
        SignalToUI::set_ui_signal();
        changed
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) -> Result<bool> {
        let size = PtySize {
            cols: cols.max(2),
            rows: rows.max(1),
            pixel_width: (u32::from(cols) * cell_w).min(u16::MAX.into()) as u16,
            pixel_height: (u32::from(rows) * cell_h).min(u16::MAX.into()) as u16,
        };
        if size == self.size {
            return Ok(false);
        }
        self.term.resize(size.cols, size.rows, cell_w, cell_h)?;
        self.size = size;
        if !self.done {
            if let Some(process) = &self.process {
                process.input.send(Command::Resize(size))?;
            }
        }
        Ok(true)
    }

    pub fn frame(&mut self) -> Result<Frame> {
        let snapshot = self.render.update(&self.term)?;
        let colors = snapshot.colors()?;
        let mut rows = self.rows.update(&snapshot)?;
        let mut lines = Vec::new();
        while let Some(row) = rows.next() {
            let mut cells = self.cells.update(row)?;
            let mut line = Vec::new();
            while let Some(cell) = cells.next() {
                let style = cell.style()?;
                let (mut fg, mut bg) = (
                    cell.fg_color()?.unwrap_or(colors.foreground),
                    cell.bg_color()?.unwrap_or(colors.background),
                );
                if style.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let wide = cell.raw_cell()?.wide()?;
                let mut text = String::new();
                if !matches!(wide, CellWide::SpacerHead | CellWide::SpacerTail) && !style.invisible
                {
                    cell.graphemes_utf8(&mut text)?;
                }
                line.push(Cell {
                    text,
                    fg,
                    bg,
                    style,
                    wide: wide == CellWide::Wide,
                    spacer: matches!(wide, CellWide::SpacerHead | CellWide::SpacerTail),
                    selected: cell.is_selected()?,
                });
            }
            lines.push(line);
        }
        let cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?
        } else {
            None
        };
        Ok(Frame {
            rows: lines,
            cursor,
            cursor_style: snapshot.cursor_visual_style()?,
            cursor_color: colors.cursor.unwrap_or(colors.foreground),
            background: colors.background,
        })
    }

    pub fn encode_key(
        &mut self,
        key: key::Key,
        mods: key::Mods,
        text: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut event = key::Event::new()?;
        event
            .set_key(key)
            .set_mods(mods)
            .set_action(key::Action::Press)
            .set_utf8(text);
        let mut bytes = Vec::new();
        self.encoder
            .set_options_from_terminal(&self.term)
            .encode_to_vec(&event, &mut bytes)?;
        Ok(bytes)
    }

    pub fn key(&mut self, key: key::Key, mods: key::Mods, text: Option<&str>) -> Result<()> {
        let bytes = self.encode_key(key, mods, text)?;
        self.write(bytes)
    }

    pub fn text(&mut self, text: &str, paste: bool) -> Result<()> {
        if paste {
            let mut bytes = text.as_bytes().to_vec();
            let mut buf = vec![0; bytes.len() + 12];
            let bracketed = self.term.mode(VtMode::BRACKETED_PASTE)?;
            let n = libghostty_vt::paste::encode(&mut bytes, bracketed, &mut buf)?;
            buf.truncate(n);
            self.write(buf)
        } else {
            self.write(text.as_bytes().to_vec())
        }
    }

    fn write(&mut self, bytes: Vec<u8>) -> Result<()> {
        if self.done || bytes.is_empty() {
            return Ok(());
        }
        self.term.set_selection(None)?;
        self.anchor = None;
        self.term.scroll_viewport(ScrollViewport::Bottom);
        if let Some(process) = &self.process {
            process.input.send(Command::Write(bytes))?;
        } else if self.demo {
            self.demo_input(&bytes);
        }
        Ok(())
    }

    pub fn scroll(&mut self, lines: isize) {
        self.term.scroll_viewport(ScrollViewport::Delta(lines));
    }

    pub fn focus(&mut self, focused: bool) -> Result<()> {
        if self.term.mode(VtMode::FOCUS_EVENT)? {
            if let Some(process) = &self.process {
                let mut bytes = [0; 3];
                let event = if focused {
                    libghostty_vt::focus::Event::Gained
                } else {
                    libghostty_vt::focus::Event::Lost
                };
                let n = event.encode(&mut bytes)?;
                process.input.send(Command::Write(bytes[..n].to_vec()))?;
            }
        }
        Ok(())
    }

    pub fn select(&mut self, x: u16, y: u16, start: bool, clicks: u32) -> Result<()> {
        let point = Point::Viewport(PointCoordinate {
            x: x.min(self.size.cols - 1),
            y: u32::from(y.min(self.size.rows - 1)),
        });
        if start {
            self.term.set_selection(None)?;
            self.anchor = Some(self.term.track_grid_ref(point)?);
        }
        let end = self.term.grid_ref(point)?;
        let selection = if clicks >= 3 {
            self.term.select_line(SelectLineOptions::new(end))?
        } else if clicks == 2 {
            self.term.select_word(SelectWordOptions::new(end))?
        } else {
            self.anchor
                .as_ref()
                .map(|anchor| anchor.snapshot(&self.term))
                .transpose()?
                .flatten()
                .map(|start| Selection::new(start, end, false))
        };
        self.term.set_selection(selection.as_ref())?;
        Ok(())
    }

    pub fn select_all(&self) -> Result<()> {
        self.term.set_selection(self.term.select_all()?.as_ref())?;
        Ok(())
    }

    pub fn copy(&self) -> Result<String> {
        let options = || FormatOptions::new().with_trim(true).with_unwrap(true);
        let mut buf = vec![0; 4096];
        let n = match self.term.format_selection_buf(options(), &mut buf) {
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                buf.resize(required, 0);
                self.term.format_selection_buf(options(), &mut buf)?
            }
            other => other?,
        }
        .unwrap_or(0);
        Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    fn demo_input(&mut self, bytes: &[u8]) {
        for ch in String::from_utf8_lossy(bytes).chars() {
            match ch {
                '\r' | '\n' => {
                    self.term.vt_write(b"\r\n");
                    let line = std::mem::take(&mut self.demo_line);
                    let line = line.trim();
                    if line == "exit" {
                        self.done = true;
                        self.state = "shell exited".into();
                        return;
                    } else if line == "clear" {
                        self.term.vt_write(b"\x1b[H\x1b[2J");
                    } else if line == "stty size" {
                        self.term.vt_write(
                            format!("{} {}\r\n", self.size.rows, self.size.cols).as_bytes(),
                        );
                    } else if let Some(text) = line.strip_prefix("echo ") {
                        self.term.vt_write(format!("{text}\r\n").as_bytes());
                    } else if !line.is_empty() {
                        self.term
                            .vt_write(b"demo: try echo, clear, stty size, or exit\r\n");
                    }
                    self.term.vt_write(b"\x1b[32m$\x1b[0m ");
                }
                '\x03' => {
                    self.demo_line.clear();
                    self.term.vt_write(b"^C\r\n$ ");
                }
                '\x7f' | '\x08' => {
                    if self.demo_line.pop().is_some() {
                        self.term.vt_write(b"\x08 \x08");
                    }
                }
                c if !c.is_control() => {
                    self.demo_line.push(c);
                    self.term.vt_write(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                _ => {}
            }
        }
    }
}
