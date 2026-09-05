//! The `attachment` panel: one part of a letter, on the shared file card.
//!
//! The same card the files app draws a path with, filled from a row instead
//! of a `stat`. That is the whole of the sharing: what a file *is* — its kind
//! word, its size, whether a preview is worth attempting — is the kernel's
//! (`caps::preview`), so a part and a file on a disk cannot drift apart.
//!
//! Its one verb is `open`. A part has no path, so it is written to the app's
//! scratch directory first and *that* is handed to the OS — one extra step,
//! and then it is a file like any other, browsable with the panel that
//! browses files. There is no copy, no move and no delete: a part is not on a
//! disk, and the letter is not this panel's to edit.

use std::any::Any;
use std::rc::Rc;

use kernel::caps::{FileKind, OpenPath, WriteFile};
use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::fmt_date;

use super::super::model::{self, MailId};
use super::super::parts::{self, scratch, Attachment};

/// What the card spends on everything that is not the preview: the name, the
/// kind line, the date, the media type, the rule and the padding around them,
/// in lines.
const CHROME_LINES: usize = 7;

/// How many lines of text one grid row holds, near enough for a wish.
const ROW_LINES: usize = 6;

/// The rows a card asks for at its shortest, and the most it will ask for.
const ROWS: (u32, u32) = (3, 6);

/// One part of a letter, shown.
pub struct Card {
    id: PanelId,
    mail: MailId,
    at: u32,
    slot: SlotId,
    store: Rc<Store>,
    /// The row, if the letter still yields that part.
    row: Option<Attachment>,
    /// Who wrote the letter, and when — the card's *when* line, since a part
    /// has no date of its own.
    with: String,
    /// The line under the header: what a verb refused, until the next one.
    status: Option<String>,
}

impl Card {
    /// The persisted spelling. Two arguments: the letter, and the part's
    /// place in it — a row's own id is derived and local to a device.
    pub const TAG: Tag = Tag("attachment");

    /// The card over one part.
    #[must_use]
    pub fn id(mail: MailId, at: u32) -> PanelId {
        PanelId::new(Self::TAG, [mail.to_string(), at.to_string()])
    }

    /// The part an `attachment` panel names; `None` for any other tag, or for
    /// arguments this build cannot read.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<(MailId, u32)> {
        if id.tag != Self::TAG {
            return None;
        }
        Some((id.arg(0)?.parse().ok()?, id.arg(1)?.parse().ok()?))
    }

    /// Which letter it came in, and where in it — what the widget asks the
    /// picture reader for.
    #[must_use]
    pub fn part(&self) -> (MailId, u32) {
        (self.mail, self.at)
    }

    /// The store the part is read back out of.
    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }

    /// The big line: what the sender called it.
    #[must_use]
    pub fn name(&self) -> String {
        self.row
            .as_ref()
            .map_or_else(|| "attachment".to_string(), |a| a.name.clone())
    }

    #[must_use]
    pub fn kind(&self) -> FileKind {
        self.row.as_ref().map_or(FileKind::Other, Attachment::kind)
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        self.row.as_ref().map_or(0, |a| a.size)
    }

    /// The muted line: which letter it arrived in. A part has no date of its
    /// own, so it wears its letter's.
    #[must_use]
    pub fn when(&self) -> String {
        if self.row.is_none() {
            return "not in the letter any more".to_string();
        }
        self.with.clone()
    }

    /// The selectable line under the three. A disk card's is the path; a
    /// part has none, so it is the media type — which is also what tells the
    /// two cards apart in a script.
    #[must_use]
    pub fn detail(&self) -> String {
        self.row
            .as_ref()
            .map_or_else(String::new, |a| a.mime.clone())
    }

    /// Whether the letter still yields it.
    #[must_use]
    pub fn gone(&self) -> bool {
        self.row.is_none()
    }

    /// What the last verb refused, until the next one.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Reads the row again — the description is a row, so it is there at
    /// once; the bytes are the widget's to ask for off the frame.
    pub fn reread(&mut self) {
        self.row = parts::attachment(&self.store, self.mail, self.at);
        self.with = model::mail(&self.store, self.mail).map_or_else(String::new, |m| {
            let who = if m.head.from_name.is_empty() {
                m.head.from_email.clone()
            } else {
                m.head.from_name.clone()
            };
            format!("with {who}, {}", fmt_date(m.head.date))
        });
    }

    /// `open`: the part written out to the app's scratch directory, and that
    /// path handed to whatever the OS opens it with.
    fn open(&mut self, s: &mut Session) {
        match self.write_out(s.world()) {
            Ok(path) => {
                self.status = None;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match s.world().run(&OpenPath { path: &path }) {
                    Ok(()) => s.notify(format!("opened “{name}”"), false),
                    Err(e) => {
                        self.status = Some(e.clone());
                        s.notify(e, true);
                    }
                }
            }
            Err(e) => {
                self.status = Some(e.clone());
                s.notify(e, true);
            }
        }
    }

    /// The bytes on the disk, where the OS can reach them. Reads the whole
    /// part rather than the preview's ceiling: what is opened is the file the
    /// sender sent, not as much of it as a card would draw.
    fn write_out(&self, world: &World) -> Result<std::path::PathBuf, String> {
        let a = self
            .row
            .clone()
            .ok_or("that part is no longer in the letter")?;
        let bytes = parts::part(&self.store, &a)
            .ok_or_else(|| format!("“{}” is not there any more", a.name))?;
        let path = scratch(a.message, a.at, &a.name);
        world.run(&WriteFile {
            path: &path,
            bytes: &bytes,
        })?;
        Ok(path)
    }
}

impl Panel for Card {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        self.name()
    }

    /// One part of a letter, and why it has no path.
    fn about(&self) -> String {
        format!(
            "One part of a letter, on the same card the files app draws a path \
             with: the name, the media type, the size, the letter it came \
             with, and a preview when it is text or a picture. Its arguments \
             are the letter's `message.id`, {}, and the part's place in it, \
             {} — a part's own row in `attachment` is derived from the \
             letter's raw MIME and local to a device, so the identity is the \
             pair rather than that row's id. The bytes stay in `message.raw` \
             and are never stored twice; there is no path either, so the one \
             verb is *open*, which writes the part to a scratch directory and \
             hands that to the operating system.",
            self.mail, self.at
        )
    }

    /// Three rows as the floor, more when the preview needs them. The bytes
    /// are not here to measure — they come off a thread — so a text part is
    /// wished at its size and a picture at the box a card gives one.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        let lines = match self.kind() {
            FileKind::Text => (self.size() as usize / 60).min(120),
            FileKind::Image => 12,
            _ => 0,
        };
        let rows = (CHROME_LINES + lines).div_ceil(ROW_LINES) as u32;
        (4, rows.clamp(ROWS.0, ROWS.1))
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// One verb. A part is not on a disk: there is nothing to copy, nothing
    /// to move, and the letter is not this panel's to edit.
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("mail.open", "open", Some('o'))]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "mail.open" {
            self.open(s);
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct CardKind;

impl PanelKind for CardKind {
    fn tag(&self) -> Tag {
        Card::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let (mail, at) = Card::of(id).unwrap_or_default();
        let mut card = Card {
            id: id.clone(),
            mail,
            at,
            slot: 0,
            store: cx.session().store().clone(),
            row: None,
            with: String::new(),
            status: None,
        };
        card.reread();
        Box::new(card)
    }
}
