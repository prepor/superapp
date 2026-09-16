//! Immutable conversation readings prepared away from the UI.
//!
//! Domain queries retain immediate truth. A reader owns this separate display
//! snapshot, including quote splitting, previews and line lengths, so drawing
//! and resizing never fetch or repeatedly process entire message bodies.

use std::collections::{BTreeSet, HashMap};

use kernel::store::Store;

use super::{
    html,
    model::{self, MailFull, MailId, Person},
    parts::{self, Attachment},
    reading,
};

#[derive(Default)]
pub struct Conversation {
    pub title: String,
    pub letters: Vec<Letter>,
    /// Who the conversation is with: everyone its letters were addressed to,
    /// one's own accounts left out ([`model::people_with`]). The header at
    /// the top of the reader is this, folded.
    pub people: Vec<Person>,
}

pub struct Letter {
    pub mail: MailFull,
    /// Everyone *this* letter was addressed to, as its own header wrote
    /// them — one's own accounts included, because a letter that reached me
    /// in copy is a thing to know about the letter.
    pub people: Vec<Person>,
    pub preview: (String, bool),
    pub own_text: String,
    pub own_html: String,
    pub quote: Option<String>,
    pub attachments: Vec<Attachment>,
    pub image_scope: String,
    pub has_cids: bool,
    line_lengths: Vec<(usize, usize)>,
}

impl Letter {
    /// The letter's own TO line: everyone its header named, in full, the
    /// copies after the rest behind a `cc:` — one line, under the FROM,
    /// saying who *this* letter went to rather than who the conversation is
    /// with.
    ///
    /// A letter with no recipient rows — one stored before they were kept,
    /// read before the ladder's walk has filled them in — falls back to the
    /// bare TO line the row itself holds, which is what the reader showed
    /// before there was anything better.
    #[must_use]
    pub fn to_line(&self) -> String {
        if self.people.is_empty() {
            return self.mail.to.clone();
        }
        let join = |cc: bool| {
            self.people
                .iter()
                .filter(|p| p.cc == cc)
                .map(Person::full)
                .collect::<Vec<_>>()
                .join(", ")
        };
        match (join(false), join(true)) {
            (to, copies) if copies.is_empty() => to,
            (to, copies) if to.is_empty() => format!("cc: {copies}"),
            (to, copies) => format!("{to} · cc: {copies}"),
        }
    }
}

impl Conversation {
    pub fn read(store: &Store, mail: MailId) -> Self {
        // One read for the whole conversation's headers, handed out per
        // letter below: a reader that asked per open letter would ask again
        // on every fold.
        let mut addressed: HashMap<MailId, Vec<Person>> = HashMap::new();
        for (letter, person) in model::thread_recipients(store, mail) {
            addressed.entry(letter).or_default().push(person);
        }
        let letters = model::thread(store, mail)
            .into_iter()
            .map(|thread| {
                let mail = thread.mail;
                let people = addressed.remove(&mail.head.id).unwrap_or_default();
                let image_scope = parts::image_scope(store, mail.head.id);
                let (own_text, own_html, quote) = match &mail.html {
                    Some(source) => {
                        let scoped = html::scope_cids(source, &image_scope);
                        let (own_html, quote) = reading::split_quote_html(&scoped);
                        (html::plain(&own_html), own_html, quote)
                    }
                    None => {
                        let (own, quote) = reading::split_quote(&mail.body);
                        (own, String::new(), quote)
                    }
                };
                let preview = mail.status.clone().unwrap_or_else(|| {
                    (
                        own_text
                            .lines()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or("")
                            .to_string(),
                        false,
                    )
                });
                let mut line_lengths = HashMap::new();
                for line in own_text.lines() {
                    *line_lengths.entry(line.chars().count()).or_insert(0) += 1;
                }
                let line_lengths = line_lengths.into_iter().collect();
                let has_cids = mail
                    .html
                    .as_deref()
                    .is_some_and(|html| html.contains("src=\"cid:"));
                let attachments = parts::attachments(store, mail.head.id).as_ref().clone();
                Letter {
                    mail,
                    people,
                    preview,
                    own_text,
                    own_html,
                    quote,
                    attachments,
                    image_scope,
                    has_cids,
                    line_lengths,
                }
            })
            .collect::<Vec<Letter>>();
        Self {
            people: model::people_with(letters.iter().flat_map(|l| l.people.iter().cloned())),
            letters,
            title: model::thread_topic(store, mail).unwrap_or_else(|| "message".into()),
        }
    }

    /// How many lines the folded-out list of people costs the header, which
    /// is what the panel's wish adds while it is unfolded.
    #[must_use]
    pub fn people_lines(&self) -> usize {
        self.people.len()
    }

    pub fn initially_open(&self, mail: MailId, unread: &BTreeSet<MailId>) -> BTreeSet<MailId> {
        let first = self
            .letters
            .iter()
            .position(|letter| unread.contains(&letter.mail.head.id))
            .unwrap_or(self.letters.len());
        self.letters[first..]
            .iter()
            .map(|letter| letter.mail.head.id)
            .chain(Some(mail))
            .collect()
    }

    pub fn lines(&self, open: &BTreeSet<MailId>, cols: usize) -> usize {
        let cols = cols.max(1);
        let lines: f64 = self
            .letters
            .iter()
            .map(|letter| {
                if !open.contains(&letter.mail.head.id) {
                    return 1.5;
                }
                let wrapped = letter
                    .line_lengths
                    .iter()
                    .map(|(length, count)| length.div_ceil(cols).max(1) * count)
                    .sum::<usize>()
                    .max(1);
                // Five lines of chrome an open letter costs: its header,
                // the FROM line under it, and the padding around the
                // reading. The TO line under the FROM costs what it wraps
                // to — a letter to a dozen people is a paragraph of header.
                let addressed = letter.to_line().chars().count().div_ceil(cols).max(1);
                5.0 + addressed as f64
                    + wrapped as f64
                    + usize::from(letter.mail.status.is_some()) as f64
                    + usize::from(!letter.attachments.is_empty()) as f64
            })
            .sum();
        (lines.ceil() as usize).max(1)
    }
}
