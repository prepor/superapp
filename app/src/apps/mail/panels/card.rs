//! The `attachment` panel: one part of a letter, on the shared file card.
//!
//! The same card the files app draws a path with, filled from a row instead
//! of a `stat`. The shared viewer renders and measures its content; the kind
//! word, its size, whether a preview is worth attempting — is the kernel's
//! (`caps::preview`), so a part and a file on a disk cannot drift apart.
//!
//! Its source verb is `open`; viewing controls come from the shared viewer.
//! A part has no path, so it is written to the app's scratch directory first
//! and *that* is handed to the OS — one extra step,
//! and then it is a file like any other, browsable with the panel that
//! browses files. There is no copy, no move and no delete: a part is not on a
//! disk, and the letter is not this panel's to edit.

use std::any::Any;
use std::rc::Rc;

use crate::shell::widgets::viewer::{Controller, Measure};
use kernel::caps::{FileKind, OpenPath, WriteFile};
use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::fmt_date;

use super::super::model::{self, MailId};
use super::super::parts::{self, scratch, Attachment};

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
    viewer: Controller,
    pending: Option<tokio::sync::oneshot::Receiver<Result<std::path::PathBuf, String>>>,
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

    pub fn viewer(&self) -> Controller {
        self.viewer.clone()
    }

    /// Reads the row again — the description is a row, so it is there at
    /// once; the bytes are the widget's to ask for off the frame.
    pub fn reread(&mut self) {
        self.row = parts::attachment(&self.store, self.mail, self.at);
        self.with = model::display_head(&self.store, self.mail).map_or_else(String::new, |m| {
            let who = if m.from_name.is_empty() {
                m.from_email.clone()
            } else {
                m.from_name.clone()
            };
            format!("with {who}, {}", fmt_date(m.date))
        });
    }

    /// `open`: the part written out to the app's scratch directory, and that
    /// path handed to whatever the OS opens it with.
    fn open(&mut self, s: &mut Session) {
        if self.pending.is_some() {
            return;
        }
        let Some(a) = self.row.clone() else { return };
        let reader = s.world().with_cap::<parts::Reader, _>(|r| r.clone());
        match reader {
            Ok(reader) if !reader.env.clock.is_virtual() => {
                let db = self.store.db();
                let wake = self.store.ui_waker();
                let (send, receive) = tokio::sync::oneshot::channel();
                self.pending = Some(receive);
                kernel::runtime::spawn_local(move || async move {
                    let result = match reader.world(db) {
                        Ok(world) => Self::open_out(&world, &a).await,
                        Err(e) => Err(e),
                    };
                    let _ = send.send(result);
                    if let Some(wake) = wake {
                        wake();
                    }
                });
                self.status = Some("downloading…".into());
                s.redraw();
            }
            _ => self.finish_open(s, kernel::runtime::block_on(Self::open_out(s.world(), &a))),
        }
    }

    pub fn opening(&self) -> bool {
        self.pending.is_some()
    }

    pub fn poll_open(&mut self, s: &mut Session) {
        let Some(rx) = &mut self.pending else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(_) => Err("attachment download stopped; try again".into()),
        };
        self.pending = None;
        self.finish_open(s, result);
    }

    fn finish_open(&mut self, s: &mut Session, result: Result<std::path::PathBuf, String>) {
        match result {
            Ok(path) => {
                self.status = None;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                s.notify(format!("opened “{name}”"), false);
            }
            Err(e) => {
                self.status = Some(e.clone());
                s.notify(e, true);
            }
        }
    }

    async fn open_out(
        world: &World,
        attachment: &Attachment,
    ) -> Result<std::path::PathBuf, String> {
        let path = Self::write_out(world, attachment).await?;
        if let Some(factory) = world.factory() {
            let target = path.clone();
            kernel::runtime::spawn_blocking(move || {
                let world = factory.build().map_err(|error| error.to_string())?;
                world.run(&OpenPath { path: &target })
            })
            .await
            .map_err(|error| error.to_string())??;
        } else {
            world.run(&OpenPath { path: &path })?;
        }
        Ok(path)
    }

    /// The bytes on the disk, where the OS can reach them. Reads the whole
    /// part rather than the preview's ceiling: what is opened is the file the
    /// sender sent, not as much of it as a card would draw.
    async fn write_out(world: &World, a: &Attachment) -> Result<std::path::PathBuf, String> {
        let bytes = parts::part(world, a).await?;
        let path = scratch(a.message, a.at, &a.name);
        if let Some(factory) = world.factory() {
            let target = path.clone();
            kernel::runtime::spawn_blocking(move || {
                let world = factory.build().map_err(|e| e.to_string())?;
                world.run(&WriteFile {
                    path: &target,
                    bytes: &bytes,
                })
            })
            .await
            .map_err(|e| e.to_string())??;
        } else {
            world.run(&WriteFile {
                path: &path,
                bytes: &bytes,
            })?;
        }
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
             with, and a viewer for text, images, and PDF pages. Its arguments \
             are the letter's `message.id`, {}, and the part's place in it, \
             {} — a part's own row in `attachment` is derived from the \
             content snapshot and local to a device, so the identity is the \
             pair rather than that row's id. Bytes download on demand over \
             IMAP and stay in the device-local file cache. The *open* verb \
             saves a copy with the sender's filename and hands it to the \
             operating system. Use mail.attachment with mail = {} and part = {} \
             to read the file's contents for translation or analysis.",
            self.mail, self.at, self.mail, self.at
        )
    }

    /// The shared viewer reports the loaded content's measurement once,
    /// with a stable reading height throughout a continuous PDF.
    fn wish(&self, cols: usize) -> (u32, u32) {
        let measure = self.viewer.measure();
        if measure == Measure::Empty && self.kind() == FileKind::Pdf {
            Measure::Pdf(595, 842).wish(cols, 7)
        } else {
            measure.wish(cols, 7)
        }
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// Viewer controls and open. A part is not on a disk: nothing to copy,
    /// nothing to move, and the letter is not this panel's to edit.
    fn verbs(&self) -> Vec<Verb> {
        let mut verbs = vec![Verb::run("mail.open", "open", Some('o'))];
        verbs.extend(self.viewer.verbs());
        verbs
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if self.viewer.run(verb) {
            s.redraw();
            return;
        }
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
            viewer: Controller::default(),
            pending: None,
        };
        card.reread();
        Box::new(card)
    }
}
