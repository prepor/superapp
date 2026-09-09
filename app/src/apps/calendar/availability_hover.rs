//! Prepared with participant tracks off the UI. Pointer moves only search spans;
//! they never load a snapshot, scan events, or format dates.
use super::{availability, dates};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

pub(super) struct Content {
    pub text: String,
    line_lengths: Box<[usize]>,
}
impl Content {
    fn new(text: String, truncated: bool) -> Self {
        let mut line_lengths: Vec<_> = text.lines().map(|line| line.chars().count()).collect();
        // Cutting before a hidden thirteenth line can leave the visible
        // twelfth line empty. Keep its height even though str::lines omits
        // a trailing empty line when the original text ends there.
        if truncated && text.ends_with('\n') {
            line_lengths.push(0);
        }
        let line_lengths = line_lengths.into_boxed_slice();
        Self { text, line_lengths }
    }

    pub fn lines(&self, width: f64) -> f64 {
        let mut lines = 0.0;
        for length in &self.line_lengths {
            lines += (*length as f64 * 6.5 / (width - 24.0).max(1.0))
                .ceil()
                .max(1.0);
            if lines >= 12.0 {
                return 12.0;
            }
        }
        lines
    }
}

struct Span {
    start: f64,
    end: f64,
    content: Arc<Content>,
}

#[derive(Default)]
pub(super) struct Timeline(Vec<Span>);

#[derive(Clone, Copy)]
enum Kind {
    Busy(usize),
    Event(usize),
}
struct Edge {
    at: f64,
    person: usize,
    kind: Kind,
    enter: bool,
}
/// The popup paints at most twelve lines. Preparing the first twelve logical
/// lines preserves that output at every width: wrapping can only show fewer.
const LINES: usize = 12;

struct Text {
    value: Arc<str>,
    breaks: Box<[usize]>,
}
#[derive(Default)]
struct Texts {
    values: Vec<Text>,
    ids: HashMap<Arc<str>, usize>,
}
impl Texts {
    fn add(&mut self, value: String) -> usize {
        if let Some(id) = self.ids.get(value.as_str()) {
            return *id;
        }
        let value: Arc<str> = value.into();
        let breaks = value
            .match_indices('\n')
            .take(LINES)
            .map(|(at, _)| at)
            .collect();
        let id = self.values.len();
        self.values.push(Text {
            value: value.clone(),
            breaks,
        });
        self.ids.insert(value, id);
        id
    }
}

/// A bounded description of the visible prefix. Hidden event boundaries can
/// compare these small identities and extend the existing span, without
/// copying text or scanning the rest of a large overlapping event set.
#[derive(Default)]
struct Prefix {
    parts: Vec<(usize, usize)>,
    breaks: usize,
    truncated: bool,
}
impl Prefix {
    fn push(&mut self, texts: &Texts, id: usize) -> bool {
        let text = &texts.values[id];
        let remaining = LINES - 1 - self.breaks;
        let end = text.breaks.get(remaining).copied();
        let through = end.unwrap_or(text.value.len());
        if through > 0 {
            self.parts.push((id, through));
        }
        if end.is_some() {
            self.truncated = true;
            return false;
        }
        self.breaks += text.breaks.len();
        true
    }

    fn render(&self, texts: &Texts) -> String {
        let mut text = String::with_capacity(self.parts.iter().map(|(_, end)| end).sum());
        for (id, end) in &self.parts {
            text.push_str(&texts.values[*id].value[..*end]);
        }
        text
    }
}

struct Active {
    busy: BTreeSet<usize>,
    events: BTreeSet<usize>,
    calendar: usize,
    busy_text: Vec<usize>,
    event_text: Vec<usize>,
}

impl Timeline {
    pub fn new(people: &[availability::Person], zone: &str, range: (f64, f64)) -> Self {
        let mut edges = Vec::new();
        let mut active = Vec::new();
        let mut texts = Texts::default();
        let newline = texts.add("\n".into());
        let separator = texts.add("\n\n".into());
        let time = |a, b| format!("{} – {}", dates::label(a, zone), dates::label(b, zone));
        for (person, p) in people.iter().enumerate() {
            let mut state = Active {
                busy: BTreeSet::new(),
                events: BTreeSet::new(),
                calendar: texts.add(p.calendar.clone()),
                busy_text: Vec::new(),
                event_text: Vec::new(),
            };
            let mut add = |start: f64, end: f64, kind| {
                if !start.is_finite() || !end.is_finite() {
                    return;
                }
                let (start, end) = (start.max(range.0), end.min(range.1));
                if end <= start {
                    return;
                }
                edges.push(Edge {
                    at: start,
                    person,
                    kind,
                    enter: true,
                });
                edges.push(Edge {
                    at: end,
                    person,
                    kind,
                    enter: false,
                });
            };
            for (i, (start, end)) in p.busy.iter().copied().enumerate() {
                state.busy_text.push(texts.add(format!(
                    "Busy\n{}\n{}",
                    time(start, end),
                    match p.details.state {
                        availability::DetailState::Pending => "Loading event details…",
                        availability::DetailState::Ready
                        | availability::DetailState::Unavailable => "Event details unavailable",
                    }
                )));
                add(start, end, Kind::Busy(i));
            }
            for (i, event) in p.details.events.iter().enumerate() {
                state.event_text.push(texts.add(format!(
                    "{}\n{}{}{}",
                    if event.title.is_empty() {
                        "Busy · details unavailable"
                    } else {
                        &event.title
                    },
                    if event.all_day { "All day · " } else { "" },
                    time(event.start, event.end),
                    if event.location.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", event.location)
                    },
                )));
                add(event.start, event.end, Kind::Event(i));
            }
            active.push(state);
        }
        edges.sort_unstable_by(|a, b| a.at.total_cmp(&b.at));
        let mut spans: Vec<Span> = Vec::new();
        let mut occupied = BTreeSet::new();
        let mut previous = (Vec::new(), false);
        let mut i = 0;
        while i < edges.len() {
            let start = edges[i].at;
            while i < edges.len() && edges[i].at == start {
                let edge = &edges[i];
                let state = &mut active[edge.person];
                let (set, index) = match edge.kind {
                    Kind::Busy(index) => (&mut state.busy, index),
                    Kind::Event(index) => (&mut state.events, index),
                };
                if edge.enter {
                    set.insert(index);
                } else {
                    set.remove(&index);
                }
                if state.busy.is_empty() {
                    occupied.remove(&edge.person);
                } else {
                    occupied.insert(edge.person);
                }
                i += 1;
            }
            let Some(next) = edges.get(i) else {
                break;
            };
            let mut prefix = Prefix::default();
            'people: for (n, person) in occupied.iter().enumerate() {
                let state = &active[*person];
                if n > 0 && !prefix.push(&texts, separator) {
                    break;
                }
                if !prefix.push(&texts, state.calendar) || !prefix.push(&texts, newline) {
                    break;
                }
                // Free/busy remains authoritative, including when optional
                // event details extend beyond it. Original order is retained.
                if state.events.is_empty() {
                    if !prefix.push(&texts, state.busy_text[*state.busy.first().unwrap()]) {
                        break;
                    }
                } else {
                    for (n, event) in state.events.iter().enumerate() {
                        if n > 0 && !prefix.push(&texts, separator) {
                            break 'people;
                        }
                        if !prefix.push(&texts, state.event_text[*event]) {
                            break 'people;
                        }
                    }
                }
            }
            if prefix.parts.is_empty() {
                continue;
            }
            if previous.0 == prefix.parts && previous.1 == prefix.truncated {
                if let Some(last) = spans.last_mut() {
                    if last.end == start {
                        last.end = next.at;
                    } else {
                        let content = last.content.clone();
                        spans.push(Span {
                            start,
                            end: next.at,
                            content,
                        });
                    }
                    continue;
                }
            }
            let content = Content::new(prefix.render(&texts), prefix.truncated);
            previous = (prefix.parts, prefix.truncated);
            if let Some(last) = spans.last_mut().filter(|last| {
                last.end == start
                    && last.content.text == content.text
                    && last.content.line_lengths == content.line_lengths
            }) {
                last.end = next.at;
            } else {
                spans.push(Span {
                    start,
                    end: next.at,
                    content: Arc::new(content),
                });
            }
        }
        Self(spans)
    }

    pub fn at(&self, time: f64) -> Option<Arc<Content>> {
        let i = self.0.partition_point(|span| span.start <= time);
        let span = self.0.get(i.checked_sub(1)?)?;
        (time < span.end).then(|| span.content.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(
        calendar: &str,
        busy: Vec<(f64, f64)>,
        events: Vec<(&str, f64, f64)>,
    ) -> availability::Person {
        availability::Person {
            calendar: calendar.into(),
            known: true,
            busy,
            error: String::new(),
            checks: vec![],
            details: availability::Details {
                state: availability::DetailState::Ready,
                error: String::new(),
                events: events
                    .into_iter()
                    .map(|(title, start, end)| availability::BusyEvent {
                        id: title.into(),
                        title: title.into(),
                        location: String::new(),
                        start,
                        end,
                        all_day: false,
                        account: String::new(),
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn boundaries_preserve_busy_coverage_private_details_and_calendar_order() {
        let people = [
            person(
                "first",
                vec![(10.0, 30.0), (20.0, 50.0), (70.0, 80.0)],
                vec![
                    ("Review", 0.0, 25.0),
                    ("", 25.0, 40.0),
                    ("Outside busy", 50.0, 70.0),
                ],
            ),
            person("second", vec![(22.0, 27.0)], vec![("Lunch", 22.0, 27.0)]),
        ];
        let timeline = Timeline::new(&people, "UTC", (0.0, 100.0));
        assert!(timeline.at(9.0).is_none());
        assert!(timeline.at(10.0).unwrap().text.contains("Review"));
        let overlap = &timeline.at(24.0).unwrap().text;
        assert!(overlap.find("Review").unwrap() < overlap.find("Lunch").unwrap());
        let boundary = &timeline.at(25.0).unwrap().text;
        assert!(boundary.contains("Busy · details unavailable") && boundary.contains("Lunch"));
        assert!(!boundary.contains("Review"));
        let fallback = &timeline.at(40.0).unwrap().text;
        assert!(fallback.contains("Busy\n") && fallback.contains("Event details unavailable"));
        assert!(timeline.at(50.0).is_none());
        assert!(timeline.at(70.0).unwrap().text.contains("Busy\n"));
        assert!(!timeline.at(70.0).unwrap().text.contains("Outside busy"));
        assert!(timeline.at(80.0).is_none());
    }

    #[test]
    fn movement_reuses_prepared_content_and_pending_details_remain_visible() {
        let mut p = person("team", vec![(10.0, 50.0)], vec![]);
        p.details.state = availability::DetailState::Pending;
        let timeline = Timeline::new(&[p], "UTC", (20.0, 40.0));
        assert!(timeline.at(19.0).is_none());
        let content = timeline.at(20.0).unwrap();
        assert!(content.text.contains("Loading event details…"));
        assert!(Arc::ptr_eq(&content, &timeline.at(39.0).unwrap()));
        assert!(timeline.at(40.0).is_none());
        assert!(timeline.at(f64::NAN).is_none());
    }

    #[test]
    fn overlapping_details_keep_only_the_visible_prefix_and_reuse_hidden_boundaries() {
        let count = 10_000;
        let titles: Vec<_> = (0..count)
            .map(|i| format!("Planning meeting {i:04}"))
            .collect();
        let mut p = person(
            "team",
            vec![(0.0, 2.0 * count as f64)],
            titles
                .iter()
                .enumerate()
                .map(|(i, title)| (title.as_str(), i as f64, (count + i) as f64))
                .collect(),
        );
        for event in &mut p.details.events {
            event.location = "Room 4".into();
        }
        let expected = format!(
            "team\n{}",
            p.details.events[..3]
                .iter()
                .map(|event| format!(
                    "{}\n{} – {}\nRoom 4",
                    event.title,
                    dates::label(event.start, "UTC"),
                    dates::label(event.end, "UTC")
                ))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
        assert_eq!(expected.lines().count(), LINES);
        let timeline = Timeline::new(&[p], "UTC", (0.0, 2.0 * count as f64));
        let content = timeline.at(count as f64 - 0.5).unwrap();
        assert_eq!(
            content.text, expected,
            "the visible text is unchanged, including its final line"
        );
        assert!(
            Arc::ptr_eq(&content, &timeline.at(3.0).unwrap()),
            "thousands of hidden event boundaries retain the same prepared content"
        );
        assert!(
            timeline.0.len() <= count + LINES,
            "hidden boundaries do not create spans"
        );
        let retained: usize = timeline
            .0
            .iter()
            .map(|span| {
                assert!(span.content.text.lines().count() <= LINES);
                span.content.text.len()
                    + span.content.line_lengths.len() * std::mem::size_of::<usize>()
            })
            .sum();
        assert!(
            retained < count * 1024,
            "prepared display grew beyond its linear line budget: {retained} bytes"
        );
        assert!(timeline.at(2.0 * count as f64).is_none());
    }

    #[test]
    fn line_budget_preserves_newlines_and_unicode_across_text_fragments() {
        let title = (0..20)
            .map(|i| format!("東京 · line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let p = person("team", vec![(10.0, 50.0)], vec![(&title, 10.0, 50.0)]);
        let expected = std::iter::once("team")
            .chain(title.lines())
            .take(LINES)
            .collect::<Vec<_>>()
            .join("\n");
        let timeline = Timeline::new(&[p], "UTC", (0.0, 100.0));
        assert_eq!(timeline.at(20.0).unwrap().text, expected);

        let p = person("team", vec![(10.0, 50.0)], vec![("Review\n", 10.0, 50.0)]);
        let timeline = Timeline::new(&[p], "UTC", (0.0, 100.0));
        assert_eq!(
            timeline.at(20.0).unwrap().text,
            format!(
                "team\nReview\n\n{} – {}",
                dates::label(10.0, "UTC"),
                dates::label(50.0, "UTC")
            )
        );

        let title = (0..9)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let p = person(
            "team",
            vec![(10.0, 50.0)],
            vec![(&title, 10.0, 50.0), ("Hidden", 10.0, 50.0)],
        );
        let timeline = Timeline::new(&[p], "UTC", (0.0, 100.0));
        let content = timeline.at(20.0).unwrap();
        assert!(content.text.ends_with('\n'));
        assert_eq!(
            content.lines(10_000.0),
            LINES as f64,
            "the final visible blank line retains its height"
        );
    }
}
