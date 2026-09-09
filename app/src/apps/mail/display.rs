//! Immutable conversation readings prepared away from the UI.
//!
//! Domain queries retain immediate truth. A reader owns this separate display
//! snapshot, including quote splitting, previews and line lengths, so drawing
//! and resizing never fetch or repeatedly process entire message bodies.

use std::collections::{BTreeSet, HashMap};

use kernel::store::Store;

use super::{
    html,
    model::{self, MailFull, MailId},
    parts::{self, Attachment},
    reading,
};

#[derive(Default)]
pub struct Conversation {
    pub title: String,
    pub letters: Vec<Letter>,
}

pub struct Letter {
    pub mail: MailFull,
    pub preview: (String, bool),
    pub own_text: String,
    pub own_html: String,
    pub quote: Option<String>,
    pub attachments: Vec<Attachment>,
    pub image_scope: String,
    pub has_cids: bool,
    line_lengths: Vec<(usize, usize)>,
}

impl Conversation {
    pub fn read(store: &Store, mail: MailId) -> Self {
        let letters = model::thread(store, mail)
            .into_iter()
            .map(|thread| {
                let mail = thread.mail;
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
            .collect();
        Self {
            letters,
            title: model::thread_topic(store, mail).unwrap_or_else(|| "message".into()),
        }
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
                4.0 + wrapped as f64
                    + usize::from(letter.mail.status.is_some()) as f64
                    + usize::from(!letter.attachments.is_empty()) as f64
            })
            .sum();
        (lines.ceil() as usize).max(1)
    }
}
