//! Opt-in native GPU frames for background recordings. Animation uses real time.
use makepad_widgets::*;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    time::Instant,
};

pub(super) struct Capture {
    dir: PathBuf,
    timer: Timer,
    started: Option<Instant>,
    seconds: f64,
    index: usize,
    log: BufWriter<File>,
    done: bool,
    bmp: bool,
}

impl Capture {
    pub(super) fn from_env(cx: &mut Cx) -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("SUPERAPP_CAPTURE_DIR")?);
        // A fresh directory is required; a second run never overwrites a take.
        std::fs::create_dir(&dir).ok()?;
        let file = File::create(dir.join("frames.csv")).ok()?;
        let seconds = std::env::var("SUPERAPP_CAPTURE_SECONDS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 120.0)
            .unwrap_or(12.0);
        Some(Self {
            dir,
            timer: cx.start_interval(1.0 / 120.0),
            started: None,
            seconds,
            index: 0,
            log: BufWriter::new(file),
            done: false,
            bmp: std::env::var_os("MAKEPAD_CAPTURE_BMP").is_some(),
        })
    }

    pub(super) fn handle(&mut self, cx: &mut Cx, event: &Event) {
        if self.done {
            return;
        }
        if self.timer.is_event(event).is_some() {
            if self.started.is_none() && self.dir.join("start").exists() {
                self.started = Some(Instant::now());
            }
            if self.started.is_some() {
                cx.redraw_all();
            }
        }
        let Some(started) = self.started else { return };
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed >= self.seconds {
            cx.stop_timer(self.timer);
            let _ = self.log.flush();
            let _ = std::fs::write(self.dir.join("complete"), self.index.to_string());
            self.done = true;
            return;
        }
        if matches!(event, Event::Draw(_)) {
            let extension = if self.bmp { "bmp" } else { "png" };
            let path = self.dir.join(format!("{:06}.{extension}", self.index));
            cx.capture_next_frame_to_file(path);
            let _ = writeln!(self.log, "{},{elapsed:.9}", self.index);
            if self.index == 0 {
                let _ = std::fs::write(self.dir.join("ready"), "native GPU capture");
            }
            self.index += 1;
        }
    }
}
