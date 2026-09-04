//! A file, drawn: the shell's own card over what the instance read.
//!
//! The card does no reading. What a file *is* — its size, its date, whether
//! there is a preview worth attempting and what those bytes are — the
//! [`Card`] instance worked out through the disk when it opened, and again
//! whenever anybody wrote one; this hands the answer to
//! [`card::fill`](crate::shell::widgets::card::fill).
//!
//! Filled when the reading changes and not once a frame: a picture is
//! decoded into a texture of its own, so the widget remembers which reading
//! of which file is on the card and writes again only when that moves.
//!
//! The bar is the instance's: *open*, *copy*, *move*, the *rename* that
//! raises a field where the name is drawn, the *delete* that takes the card
//! with the file, and the *copy path* that takes nothing at all.

use kernel::panel::PanelId;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::keys::{key_char, Letters};
use crate::shell::widgets::card::{self, CardData, Preview};

use super::super::model::{fmt_size, FileKind, Preview as Read};
use super::super::panels::Card;
use super::field::Raised;

/// The children the card's template adds to the shell's own.
const STATUS: &[LiveId] = ids!(status_lbl);

/// The name, and the `rename` field that stands in its place: the shell's
/// card carries the row, this app is what raises it.
const NAME: &[LiveId] = ids!(name_lbl);
const RENAME_ROW: &[LiveId] = ids!(rename_row);
const RENAME: &[LiveId] = ids!(rename_row.rename_input);

/// The selectable line under the three: the path, and the run the preview
/// is. Both are addressed by a script — the path by its own text, which is
/// the one thing two cards can never both be right about.
const DETAIL: &[LiveId] = ids!(detail_txt);
const PREVIEW: &[LiveId] = ids!(text_box.text_prev);
/// The picture, when there is one: addressed by one word, since a card
/// draws at most one and its bytes are nothing a script can name.
const PICTURE: &[LiveId] = ids!(img_box.img_prev);

/// Which reading of which file is on the card. A picture is decoded once
/// per reading, so a second draw of the same one writes nothing.
#[derive(Clone, PartialEq, Eq)]
struct Shown {
    id: PanelId,
    /// Which reading the instance is holding. Not *when* it last looked: a
    /// run writing elsewhere brings the card back to the disk on every
    /// path it performs, and the picture on it has not changed for any of
    /// them.
    at: u64,
}

/// What the card draws around the file: the line a refused verb leaves, and
/// the `rename` field's text while that field is up. Both are the
/// instance's — a verb that closes the field takes what was typed with it.
struct Chrome {
    status: Option<String>,
    renaming: Option<String>,
}

/// The widget: the card, the field it raises over the name, and the line a
/// refused verb leaves.
#[derive(Script, ScriptHook, Widget)]
pub struct CardPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// What the card was last filled for.
    #[rust]
    shown: Option<Shown>,
    /// The `rename` field: up while the instance holds a name for it.
    #[rust]
    rename_field: Raised,
}

impl Widget for CardPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // The card asks again once anybody has written the disk — on an
        // event as well as on a draw, since a verb's write lands between
        // the two.
        observe(scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            self.view.handle_event(cx, event, scope);
            return;
        };
        let rename = self.view.text_input(cx, RENAME);
        self.sync(cx, &props, &rename);
        self.rename_field.land(cx, &rename, true);
        let live = self.rename_field.live(cx, &rename);
        let in_run = !live && selecting(cx, &self.view);
        keeps(&props, live, in_run);
        if let Event::KeyDown(k) = event {
            // A live field keeps the chords it needs: `cmd+a` is select-all
            // here, not a verb on the bar. A caret in one of the selectable
            // runs keeps only the four text chords — `cmd+c` copies what is
            // selected rather than running the bar's `copy path`, and every
            // other letter is still the bar's.
            if k.modifiers.logo && (live || (in_run && text_chord(k))) {
                props.chord.take();
            }
        }
        self.view.handle_event(cx, event, scope);

        let Event::Actions(actions) = event else {
            return;
        };
        if rename.key_focus_lost(actions) {
            rename.set_cursor(cx, rename.cursor(), false);
        }
        if rename.changed(actions).is_some() {
            edit(&props, |c| c.set_renaming(Some(rename.text())));
        }
        if rename.returned(actions).is_some() {
            self.rename(cx, &props, scope, &rename.text());
        }
        if rename.escaped(actions) {
            self.close(cx, &props, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        observe(scope);
        // The line under the header and the run it is about, both read
        // here and before anything asks for either: as a listing's.
        drew(&props);
        let Some(shown) = shown(&props) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let fresh = self.shown.as_ref() != Some(&shown);
        // Cloned out of the instance: `fill` writes the whole tree, and
        // nothing may still be borrowing the panel by then. The preview
        // comes along only when it is going to be written — a reading is up
        // to 64 KiB and a picture rather more.
        let Some((data, chrome)) = read(&props, fresh) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if fresh {
            card::fill(cx, &self.view, &data);
            self.shown = Some(shown);
        }
        let lbl = self.view.label(cx, STATUS);
        lbl.set_text(cx, chrome.status.as_deref().unwrap_or(""));
        lbl.set_visible(cx, chrome.status.is_some());
        // The field stands where the name is drawn, so exactly one of the
        // two is up. Said on the draw as well as on the event: a bar is
        // drawn before the body that reports.
        let rename = self.view.text_input(cx, RENAME);
        self.rename_field.sync(
            cx,
            &self.view,
            RENAME_ROW,
            &rename,
            chrome.renaming.as_deref(),
        );
        self.view
            .widget(cx, NAME)
            .set_visible(cx, !self.rename_field.up());
        let live = self.rename_field.live(cx, &rename);
        keeps(&props, live, !live && selecting(cx, &self.view));

        let step = self.view.draw_walk(cx, scope, walk);

        // The field, addressable by name — that is all a script needs to put
        // a caret in one. Only while its row is up: a hidden widget keeps
        // its last rectangle.
        if self.rename_field.up() {
            let r = self.view.widget(cx, RENAME).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add("new name", r, MouseCursor::Text, props.slot);
            }
        }

        // The path line carries its own text as its label, the way a row
        // does, so a script can say which file this card is on.
        for (label, path, cursor) in [
            (data.detail.as_str(), DETAIL, MouseCursor::Text),
            ("preview", PREVIEW, MouseCursor::Text),
            ("picture", PICTURE, MouseCursor::Default),
        ] {
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 && !label.is_empty() {
                props.hits.add(label, r, cursor, props.slot);
            }
        }
        step
    }
}

impl CardPanel {
    /// Raises and lowers the field with the instance's own state, seeding it
    /// the once. Called from the draw as well as from the event, so the verb's
    /// field is up in the very frame it asked for.
    fn sync(&mut self, cx: &mut Cx, props: &PanelProps, rename: &TextInputRef) {
        let text = renaming(props);
        self.rename_field
            .sync(cx, &self.view, RENAME_ROW, rename, text.as_deref());
    }

    /// Enter in the field: the instance's own verb, on the instance the
    /// widget is holding. The card is pointed at the new name in the layout
    /// half of that same action, so this instance is on its way out — what it
    /// does with the keyboard is only for the frames in between.
    fn rename(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, name: &str) {
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let closed = {
            let mut borrow = props.panel.borrow_mut();
            let Some(c) = borrow.as_any().downcast_mut::<Card>() else {
                return;
            };
            c.rename(session, name);
            // The field closes itself where the rename went through; a
            // refusal keeps it, with the name still in it.
            c.renaming().is_none()
        };
        if closed {
            cx.set_key_focus(self.view.area());
        }
        self.view.redraw(cx);
    }

    /// Esc: the field goes away and the name comes back, with nothing
    /// renamed.
    fn close(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        edit(props, |c| {
            c.set_renaming(None);
            c.set_status(None);
        });
        cx.set_key_focus(self.view.area());
        self.view.redraw(cx);
        if let Some(session) = scope.data.get_mut::<Session>() {
            session.redraw();
        }
    }
}

/// The `rename` field's text, off the instance; `None` while it is down.
fn renaming(props: &PanelProps) -> Option<String> {
    let mut borrow = props.panel.borrow_mut();
    borrow
        .as_any()
        .downcast_mut::<Card>()?
        .renaming()
        .map(str::to_string)
}

/// Whether one of the card's selectable runs has the keyboard: the path
/// line, or a text preview. Either is a makepad field, read-only.
///
/// The keyboard has to be *somewhere* first: a field that has never been
/// drawn — the preview box a card without one folds away — has an empty
/// area, and an empty key focus reads as that field's own. Without this a
/// pdf's card would answer yes with nothing focused at all, and swallow
/// the `cmd+c` its bar was drawing.
fn selecting(cx: &mut Cx, view: &View) -> bool {
    cx.key_focus() != Area::Empty
        && (view.text_input(cx, DETAIL).key_focus(cx) || view.text_input(cx, PREVIEW).key_focus(cx))
}

/// Whether a chord is one of the four a caret keeps wherever one blinks.
fn text_chord(k: &KeyEvent) -> bool {
    key_char(k.key_code).is_some_and(|c| Letters::TEXT.has(c))
}

/// Which reading of which file the instance is holding right now.
fn shown(props: &PanelProps) -> Option<Shown> {
    // The identity off the instance as a panel, the reading off it as a
    // card: `Card::id` is the constructor of one, not the accessor.
    let id = props.panel.borrow().id().clone();
    let mut borrow = props.panel.borrow_mut();
    let c = borrow.as_any().downcast_mut::<Card>()?;
    Some(Shown {
        id,
        at: c.read_at(),
    })
}

/// What the card shows, off the instance: the name, what it is and how big,
/// when it changed, where it lives, and — when this draw is going to write
/// it — whatever preview there is.
///
/// A file that has gone says so in its own line rather than reading as a
/// nought-byte one, and the date line says the same thing again.
fn read(props: &PanelProps, preview: bool) -> Option<(CardData, Chrome)> {
    let mut borrow = props.panel.borrow_mut();
    let c = borrow.as_any().downcast_mut::<Card>()?;
    let (kind_word, size) = if c.gone() {
        ("gone".to_string(), String::new())
    } else if c.kind() == FileKind::Dir {
        (c.kind_word().to_string(), String::new())
    } else {
        (c.kind_word().to_string(), fmt_size(c.size()))
    };
    Some((
        CardData {
            name: c.name(),
            kind_word,
            size,
            modified: c.when(),
            detail: c.path().to_string(),
            // The instance's reading, in the card's own words. The two
            // enums are the same three cases on either side of the seam:
            // the app decides what is worth reading, the card decodes.
            preview: match (preview, c.preview()) {
                (true, Read::Text(t)) => Preview::Text(t.clone()),
                (true, Read::Image(b)) => Preview::Image(b.clone()),
                _ => Preview::None,
            },
        },
        Chrome {
            status: c.note(),
            renaming: c.renaming().map(str::to_string),
        },
    ))
}

/// Runs `f` on the instance, where there is nothing to answer.
fn edit(props: &PanelProps, f: impl FnOnce(&mut Card)) {
    let mut borrow = props.panel.borrow_mut();
    if let Some(c) = borrow.as_any().downcast_mut::<Card>() {
        f(c);
    }
}

/// Remembers which run the line under the header is about, at the moment it
/// is drawn — the frame being where a *cancel* decides what it is about.
fn drew(props: &PanelProps) {
    edit(props, Card::drawn);
}

/// What this widget's keyboard keeps from the bars, said on every draw and
/// every event.
///
/// Every letter while the `rename` field is live, because the keydown above
/// answers *any* cmd chord while a caret blinks in it — so no bar's letter
/// would fire and none may be drawn as if it would. Only the four text
/// chords while a caret sits in one of the selectable runs instead: those
/// are read-only, and answer nothing else. Nothing at all when the keyboard
/// is elsewhere.
fn keeps(props: &PanelProps, live: bool, in_run: bool) {
    if live {
        props.chord.field(Letters::ALL);
    } else if in_run {
        props.chord.field(Letters::NONE);
    }
}

/// Hands the instance the one fact it cannot ask for itself: that the disk
/// has moved under it. Read-only on the session, and both borrows end with
/// the call.
fn observe(scope: &mut Scope) {
    let Some(props) = scope.props.get::<PanelProps>().cloned() else {
        return;
    };
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let session: &Session = session;
    let mut borrow = props.panel.borrow_mut();
    if let Some(c) = borrow.as_any().downcast_mut::<Card>() {
        c.observe(session);
    }
}
