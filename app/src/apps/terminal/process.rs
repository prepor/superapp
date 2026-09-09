//! PTY I/O stays off the UI thread. The bounded output queue applies
//! backpressure to noisy children; a coalesced signal wakes the renderer.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use makepad_widgets::SignalToUI;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

pub(super) enum Command {
    Write(Vec<u8>),
    Resize(PtySize),
}

pub(super) enum Output {
    Ready,
    Data(Vec<u8>),
    Error(String),
    Exited(String),
}

pub(super) struct Process {
    pub input: mpsc::Sender<Command>,
    pub output: mpsc::Receiver<Output>,
    pub dirty: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
}

#[derive(Clone)]
struct Sink {
    output: mpsc::SyncSender<Output>,
    dirty: Arc<AtomicBool>,
}

impl Sink {
    fn send(&self, event: Output) -> bool {
        if self.output.send(event).is_err() {
            return false;
        }
        if !self.dirty.swap(true, Ordering::AcqRel) {
            SignalToUI::set_ui_signal();
        }
        true
    }
}

impl Process {
    pub fn spawn(size: PtySize) -> std::io::Result<Self> {
        let mut cmd = CommandBuilder::new_default_prog();
        if let Some(home) = std::env::var_os("HOME") {
            cmd.cwd(home);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "superapp");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        Self::command(cmd, size)
    }

    fn command(cmd: CommandBuilder, size: PtySize) -> std::io::Result<Self> {
        let (input, commands) = mpsc::channel();
        let (sender, output) = mpsc::sync_channel(16);
        let (stop, stopped) = mpsc::channel();
        let dirty = Arc::new(AtomicBool::new(false));
        let sink = Sink {
            output: sender,
            dirty: dirty.clone(),
        };
        std::thread::Builder::new()
            .name("terminal shell".into())
            .spawn(move || {
                if let Err(error) = run(cmd, size, commands, stopped, &sink) {
                    sink.send(Output::Error(error));
                }
            })?;
        Ok(Self {
            input,
            output,
            dirty,
            stop,
        })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // The supervisor kills and reaps the child even if the writer is
        // blocked, or this panel closed before spawning finished.
        let _ = self.stop.send(());
    }
}

fn run(
    cmd: CommandBuilder,
    size: PtySize,
    commands: mpsc::Receiver<Command>,
    stopped: mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<(), String> {
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| e.to_string())?;
    // Prepare all fallible handles before creating a child.
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let output = sink.clone();
    let read_thread = std::thread::Builder::new()
        .name("terminal output".into())
        .spawn(move || {
            let mut bytes = [0; 16 * 1024];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => {
                        if !output.send(Output::Data(bytes[..n].to_vec())) {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // A closed Unix PTY can report EIO instead of EOF.
                    Err(e) if e.raw_os_error() == Some(5) => break,
                    Err(e) => {
                        output.send(Output::Error(format!("reading shell: {e}")));
                        break;
                    }
                }
            }
        });
    if let Err(error) = read_thread {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.to_string());
    }
    let output = sink.clone();
    let write_thread = std::thread::Builder::new()
        .name("terminal input".into())
        .spawn(move || {
            while let Ok(command) = commands.recv() {
                let result = match command {
                    Command::Write(bytes) => writer.write_all(&bytes).map_err(|e| e.to_string()),
                    Command::Resize(size) => pair.master.resize(size).map_err(|e| e.to_string()),
                };
                if let Err(error) = result {
                    output.send(Output::Error(format!("writing shell: {error}")));
                    break;
                }
            }
        });
    if let Err(error) = write_thread {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.to_string());
    }
    sink.send(Output::Ready);
    loop {
        match stopped.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Final bytes precede the exit notification.
                if let Ok(thread) = read_thread {
                    let _ = thread.join();
                }
                sink.send(Output::Exited(status.to_string()));
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    fn until(process: &Process, output: &mut String, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !output.contains(needle) {
            assert!(
                Instant::now() < deadline,
                "shell did not produce {needle:?}: {output}"
            );
            match process.output.recv_timeout(Duration::from_millis(100)) {
                Ok(Output::Data(bytes)) => output.push_str(&String::from_utf8_lossy(&bytes)),
                Ok(Output::Error(error)) => panic!("{error}"),
                Ok(Output::Exited(status)) => panic!("shell exited early: {status}: {output}"),
                _ => {}
            }
        }
    }

    #[test]
    fn real_pty_accepts_input_and_reports_the_changed_size() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "printf 'READY\\n'; read line; stty size; printf 'DONE:%s\\n' \"$line\"",
        ]);
        let process = Process::command(
            cmd,
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        )
        .unwrap();
        let mut output = String::new();
        until(&process, &mut output, "READY");
        process
            .input
            .send(Command::Resize(PtySize {
                rows: 40,
                cols: 132,
                pixel_width: 0,
                pixel_height: 0,
            }))
            .unwrap();
        process
            .input
            .send(Command::Write(b"hello\r".to_vec()))
            .unwrap();
        until(&process, &mut output, "DONE:hello");
        assert!(output.contains("40 132"), "{output}");
        loop {
            match process.output.recv_timeout(Duration::from_secs(5)).unwrap() {
                Output::Exited(_) => break,
                Output::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
    }

    #[test]
    fn closing_a_flooding_panel_does_not_wait_on_its_output_queue() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "printf 'READY:%s\\n' \"$$\"; exec /usr/bin/yes"]);
        let process = Process::command(cmd, PtySize::default()).unwrap();
        let mut output = String::new();
        until(&process, &mut output, "\r\n");
        let pid = output
            .split("READY:")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .trim()
            .to_string();
        assert!(pid.chars().all(|c| c.is_ascii_digit()));
        // Allow the reader to fill its bounded queue, then close it.
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        drop(process);
        assert!(start.elapsed() < Duration::from_millis(100));
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(
                Instant::now() < deadline,
                "closed terminal left its child alive"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
