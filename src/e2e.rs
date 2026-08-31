//! The e2e harness: a line-based script of user-level steps the shell replays
//! against its real input paths (hit resolution, key handling, text input),
//! with window-layer screenshots along the way.
//!
//! Runs are meant to be headless-feeling: the window sits behind everything,
//! click-through, and keeps presenting (makepad patch 0003), while
//! `screencapture -l` grabs its layer regardless of occlusion.
//!
//! # Script grammar
//!
//! ```text
//! # comment
//! wait 600            — ms; let springs settle / a frame render
//! shot inbox          — e2e/out/inbox.png
//! click "reply"       — click the interactive element whose label matches
//! altclick "subject"  — the same, with alt held (fresh un-joined panel)
//! key cmd+shift+left  — a key chord (cmd/shift/alt + arrows/letters/enter/esc/…)
//! type "hello"        — text into the focused field / panel keys
//! quit                — end the run; non-zero exit if any step failed
//! ```
//!
//! Labels match case-insensitively by substring against links, buttons,
//! fields, rows and panel titles. Steps that mutate the workspace need a
//! `wait` after them: hits refresh on the next drawn frame.

use std::path::PathBuf;
use std::time::Instant;

/// One script step.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Sleep this many milliseconds.
    Wait(u64),
    /// Capture the window to `<out>/<name>.png`.
    Shot(String),
    /// Click the element whose label contains this (case-insensitive).
    Click {
        /// Label substring.
        label: String,
        /// Hold cmd: always a fresh, un-joined panel.
        fresh: bool,
    },
    /// A key chord: `cmd+shift+left`, `enter`, `j`, … with a repeat count
    /// (`key j 5`).
    Key {
        /// The chord.
        chord: String,
        /// How many times to press it.
        times: u32,
    },
    /// Text input into whatever owns the keyboard.
    Type(String),
    /// End the run.
    Quit,
}

/// Parses a script. Errors carry the line number.
pub fn parse(src: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let err = |m: &str| format!("line {}: {m}: {raw}", i + 1);
        let (cmd, rest) = match line.split_once(char::is_whitespace) {
            Some((c, r)) => (c, r.trim()),
            None => (line, ""),
        };
        let quoted = || -> Result<String, String> {
            let a = rest.find('"').ok_or_else(|| err("expected a \"quoted\" argument"))?;
            let b = rest.rfind('"').filter(|&b| b > a).ok_or_else(|| err("unclosed quote"))?;
            Ok(rest[a + 1..b].to_string())
        };
        steps.push(match cmd {
            "wait" => Step::Wait(rest.parse().map_err(|_| err("expected milliseconds"))?),
            "shot" => {
                if rest.is_empty() {
                    return Err(err("expected a name"));
                }
                Step::Shot(rest.to_string())
            }
            "click" => Step::Click {
                label: quoted()?,
                fresh: false,
            },
            // cmd+click: always a fresh, un-joined panel (altclick = old alias).
            "cmdclick" | "altclick" => Step::Click {
                label: quoted()?,
                fresh: true,
            },
            "key" => {
                let mut it = rest.split_whitespace();
                let chord = it.next().ok_or_else(|| err("expected a key chord"))?;
                let times = match it.next() {
                    Some(n) => n.parse().map_err(|_| err("expected a repeat count"))?,
                    None => 1,
                };
                Step::Key {
                    chord: chord.to_lowercase(),
                    times,
                }
            }
            "type" => Step::Type(quoted()?),
            "quit" => Step::Quit,
            _ => return Err(err("unknown command")),
        });
    }
    if steps.last() != Some(&Step::Quit) {
        steps.push(Step::Quit);
    }
    Ok(steps)
}

/// A run in progress.
#[derive(Debug)]
pub struct Runner {
    /// The script.
    pub steps: Vec<Step>,
    /// Next step to execute.
    pub idx: usize,
    /// Set while a `wait` is pending.
    pub resume_at: Option<Instant>,
    /// Where screenshots go.
    pub out: PathBuf,
    /// Failed steps so far (missing labels, failed captures).
    pub failures: u32,
}

impl Runner {
    /// A runner over a parsed script.
    #[must_use]
    pub fn new(steps: Vec<Step>, out: PathBuf) -> Self {
        Runner {
            steps,
            idx: 0,
            resume_at: None,
            out,
            failures: 0,
        }
    }

    /// The next step to run, honouring a pending wait. Advances the cursor.
    pub fn next_step(&mut self) -> Option<Step> {
        if let Some(t) = self.resume_at {
            if Instant::now() < t {
                return None;
            }
            self.resume_at = None;
        }
        let step = self.steps.get(self.idx).cloned()?;
        self.idx += 1;
        if let Step::Wait(ms) = step {
            self.resume_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        }
        Some(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_grammar() {
        let s = parse(
            r#"
            # a comment
            wait 600
            shot inbox
            click "reply"
            altclick "Q3 infra"
            key cmd+shift+left
            type "hello world"
            quit
            "#,
        )
        .unwrap();
        assert_eq!(s[0], Step::Wait(600));
        assert_eq!(s[1], Step::Shot("inbox".into()));
        assert_eq!(
            s[2],
            Step::Click {
                label: "reply".into(),
                fresh: false
            }
        );
        assert_eq!(
            s[3],
            Step::Click {
                label: "Q3 infra".into(),
                fresh: true
            }
        );
        assert_eq!(
            s[4],
            Step::Key {
                chord: "cmd+shift+left".into(),
                times: 1
            }
        );
        assert_eq!(s[5], Step::Type("hello world".into()));
        assert_eq!(s[6], Step::Quit);
    }

    #[test]
    fn key_repeat_counts_parse() {
        let s = parse("key j 5").unwrap();
        assert_eq!(
            s[0],
            Step::Key {
                chord: "j".into(),
                times: 5
            }
        );
    }

    #[test]
    fn quit_is_appended_when_missing() {
        let s = parse("wait 100").unwrap();
        assert_eq!(s.last(), Some(&Step::Quit));
    }

    #[test]
    fn bad_lines_carry_the_line_number() {
        let e = parse("wait ten").unwrap_err();
        assert!(e.starts_with("line 1:"), "{e}");
    }
}
