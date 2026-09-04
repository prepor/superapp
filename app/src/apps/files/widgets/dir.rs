//! A directory, drawn: the shared rich table over the panel's own list,
//! with the crumb line above it and the status line under it.
//!
//! Everything a list does — the filter and its completion, the cursor walk
//! that previews, the marks, the band the filter hides them in, the keys —
//! is the table widget's. What files supplies is the four short functions of
//! a [`RowSpec`] and the chrome around the rows: the crumbs, the three
//! fields a verb raises among them, and the line a refused verb leaves.
//!
//! The bar is the instance's: *new dir*, *go to*, the verbs of the directory
//! this panel is the object of — *rename* among them — the batch verbs while
//! there are marks, and `… here` while another panel is holding something.

use kernel::nav::Nav;
use kernel::panel::PanelId;
use kernel::richtable::ListState;
use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::dsl::LinkViewExt;
use crate::shell::hosted::PanelProps;
use crate::shell::keys::Letters;
use crate::shell::widgets::suggest::Suggest;
use crate::shell::widgets::table::{self, RowSpec, TableView};

use super::super::completion::PathCompletion;
use super::super::model::{fmt_size, is_dir_in, normalize, DirRow, DirSource, ROOT};
use super::super::panels::dir::row_target;
use super::super::panels::Dir;
use super::field::Raised;

/// The children a listing's chrome expects in its template.
const CRUMBS: &[LiveId] = ids!(crumbs);
const HERE: &[LiveId] = ids!(crumbs.here_lbl);
const PATH_ROW: &[LiveId] = ids!(path_row);
const PATH: &[LiveId] = ids!(path_row.path_input);
const NAME_ROW: &[LiveId] = ids!(newdir_row);
const NAME: &[LiveId] = ids!(newdir_row.newdir_input);
const RENAME_ROW: &[LiveId] = ids!(rename_row);
const RENAME: &[LiveId] = ids!(rename_row.rename_input);
const STATUS: &[LiveId] = ids!(status_lbl);

/// The crumb slots, and the separator that follows each. Four: a deeper
/// path shows its last four ancestors, which is what a column's width holds.
const CRUMB_SLOTS: [(&[LiveId], &[LiveId]); 4] = [
    (ids!(crumbs.c0), ids!(crumbs.s0)),
    (ids!(crumbs.c1), ids!(crumbs.s1)),
    (ids!(crumbs.c2), ids!(crumbs.s2)),
    (ids!(crumbs.c3), ids!(crumbs.s3)),
];

/// What the table needs to know about a directory's rows.
pub struct DirRows;

impl RowSpec for DirRows {
    type Src = DirSource;
    type Panel = Dir;

    fn list(panel: &mut Dir) -> &mut ListState<DirSource> {
        panel.list_mut()
    }

    fn row_tpl() -> LiveId {
        live_id!(row)
    }

    /// One line: the name, then the size and the date on the columns the
    /// header draws. A directory wears its slash and no size — it is not a
    /// number of bytes — and the source lists directories first.
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &DirRow, selected: bool, marked: bool) {
        let line = table::line(cx, row, selected, marked);
        let e = &r.entry;
        line.label(cx, ids!(body.name_lbl)).set_text(cx, &e.label());
        let size = if e.is_dir {
            "—".to_string()
        } else {
            fmt_size(e.size)
        };
        line.label(cx, ids!(body.size_lbl)).set_text(cx, &size);
        line.label(cx, ids!(body.date_lbl))
            .set_text(cx, &fmt_date(e.modified));
    }

    /// The name as the row draws it, slash and all: what a script addresses
    /// a row by, and unique within the one directory a listing shows.
    fn label(r: &DirRow) -> String {
        r.entry.label()
    }

    /// What the row opens: a directory is a list of its own, a file is a
    /// card.
    fn target(r: &DirRow) -> PanelId {
        row_target(r)
    }

    fn empty_line(_panel: &Self::Panel, filter: &str) -> String {
        if filter.trim().is_empty() {
            "nothing here".to_string()
        } else {
            "nothing under this filter".to_string()
        }
    }
}

/// What the widget reads off its instance at the top of every draw and
/// every event. The three fields are the instance's text, not the widget's:
/// a verb that closes one takes what was typed with it.
struct Fields {
    naming: Option<String>,
    renaming: Option<String>,
    pathing: Option<String>,
    /// The line under the header: [`Dir::note`] — a run in progress, or
    /// what the last verb refused.
    status: Option<String>,
}

/// Which field an esc or a submit is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Path,
    Name,
    Rename,
}

/// The widget: the shared table, and the chrome around it.
#[derive(Script, ScriptHook, Widget)]
pub struct DirPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The filter's completion box, drawn over the rows after everything
    /// else.
    #[live]
    suggest: View,
    /// The `go to` field's own box, hung under that field.
    #[live]
    suggest_path: View,
    #[rust]
    table: TableView<DirRows>,
    /// The path field's completion: one segment at a time, like a shell.
    #[rust]
    pac: Suggest<PathCompletion>,
    /// The three fields the panel raises, each with its own memory of
    /// whether it is up and whether it is still waiting for the keyboard.
    #[rust]
    path_field: Raised,
    #[rust]
    name_field: Raised,
    #[rust]
    rename_field: Raised,
    /// The path box's rows of the last draw, in the order it offers them: a
    /// press on one is a pick, and it must not reach the row underneath.
    #[rust]
    picks: Vec<Rect>,
}

impl Widget for DirPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let Some(f) = observe(&props, scope) else {
            return;
        };
        let path = self.view.text_input(cx, PATH);
        let name = self.view.text_input(cx, NAME);
        let rename = self.view.text_input(cx, RENAME);
        self.sync(cx, &f, &path, &name, &rename);
        self.land(cx, &path, &name, &rename);
        let path_live = self.path_field.live(cx, &path);
        let field_live =
            path_live || self.name_field.live(cx, &name) || self.rename_field.live(cx, &rename);
        keeps(&props, field_live);
        self.pac.track(cx, &path);

        // The path box is drawn over the rows, so its rows answer first: the
        // table would otherwise find a row under the same point.
        if let Event::MouseDown(e) = event {
            if let Some(i) = self.picks.iter().position(|r| r.contains(e.abs)) {
                if let Some(c) = completion(&props) {
                    self.pac.pick(cx, &c, &path, i);
                    edit(&props, |d| d.set_pathing(Some(path.text())));
                    self.view.redraw(cx);
                }
                return;
            }
        }

        if let Event::KeyDown(k) = event {
            // A live field keeps the chords it needs: `cmd+a` is select-all
            // here, not a verb on the bar.
            if field_live && k.modifiers.logo {
                props.chord.take();
            }
            if path_live {
                if let Some(c) = completion(&props) {
                    // Enter goes to what is typed when that is a directory
                    // the disk has, offer open or not; otherwise the offer
                    // owns enter, and tab takes it either way.
                    if k.key_code == KeyCode::ReturnKey {
                        let typed = path.text();
                        let goes = normalize(&typed).is_some_and(|p| is_dir_in(&c.world, &p));
                        if goes || !self.pac.open() {
                            self.go_to(cx, &props, scope, &typed);
                            return;
                        }
                    }
                    if self.pac.key(cx, &c, &path, k) {
                        self.view.redraw(cx);
                        return;
                    }
                }
            }
        }

        if field_live && matches!(event, Event::KeyDown(_) | Event::TextInput(_)) {
            // While one of the three fields has the keyboard the rows' keys
            // are not the panel's to take: `/` is a slash, space is a space,
            // and the arrows walk the text.
            self.view.handle_event(cx, event, scope);
        } else {
            let Self { view, table, .. } = self;
            table.handle_event(cx, event, scope, view);
        }

        let Event::Actions(actions) = event else {
            return;
        };
        for t in [&path, &name, &rename] {
            if t.key_focus_lost(actions) {
                t.set_cursor(cx, t.cursor(), false);
            }
        }
        if path.changed(actions).is_some() {
            edit(&props, |d| d.set_pathing(Some(path.text())));
        }
        if name.changed(actions).is_some() {
            edit(&props, |d| d.set_naming(Some(name.text())));
        }
        if rename.changed(actions).is_some() {
            edit(&props, |d| d.set_renaming(Some(rename.text())));
        }
        if path.returned(actions).is_some() {
            self.go_to(cx, &props, scope, &path.text());
        }
        if path.escaped(actions) {
            self.close(cx, &props, scope, Field::Path);
        }
        if name.returned(actions).is_some() {
            self.new_dir(cx, &props, scope, &name.text());
        }
        if name.escaped(actions) {
            self.close(cx, &props, scope, Field::Name);
        }
        if rename.returned(actions).is_some() {
            self.rename(cx, &props, scope, &rename.text());
        }
        if rename.escaped(actions) {
            self.close(cx, &props, scope, Field::Rename);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some(f) = observe(&props, scope) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some(store) = scope.data.get_mut::<Session>().map(|s| s.store().clone()) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let path = self.view.text_input(cx, PATH);
        let name = self.view.text_input(cx, NAME);
        let rename = self.view.text_input(cx, RENAME);
        self.sync(cx, &f, &path, &name, &rename);
        // Said on the draw as well as on the event: a bar is drawn before
        // the body that reports, and the promise a bold letter makes is
        // about now.
        keeps(
            &props,
            self.path_field.live(cx, &path)
                || self.name_field.live(cx, &name)
                || self.rename_field.live(cx, &rename),
        );
        self.crumbs(cx, &props, &f);
        // Which run that line is about, remembered on the draw and nowhere
        // else: *cancel* is a button about the run whose line was on
        // screen when it was pressed, and the frame is where that is
        // decided.
        drew(&props);
        // The status line: what a run is doing, or what the last verb
        // refused until the next one.
        let lbl = self.view.label(cx, STATUS);
        lbl.set_text(cx, f.status.as_deref().unwrap_or(""));
        lbl.set_visible(cx, f.status.is_some());

        let Self {
            view,
            suggest,
            suggest_path,
            table,
            pac,
            picks,
            ..
        } = self;
        let step = table.draw(cx, scope, walk, view, suggest);

        // The two fields, addressable by name — that is all a script needs
        // to put a caret in one. Only while their rows are up: a hidden
        // widget keeps its last rectangle.
        for (up, path_ids, label) in [
            (f.pathing.is_some(), PATH, "path"),
            (f.naming.is_some(), NAME, "new dir name"),
            (f.renaming.is_some(), RENAME, "new name"),
        ] {
            if !up {
                continue;
            }
            let r = view.widget(cx, path_ids).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }

        // The path field's offer, drawn last of all — after the table's own
        // box — so it covers what it hangs over, and registered last, so a
        // press on it wins over the rows underneath.
        picks.clear();
        match (f.pathing.is_some(), completion(&props)) {
            (true, Some(c)) => {
                pac.draw(cx, scope, &store, &c, &path, suggest_path);
                for (label, r) in pac.hits(cx, suggest_path) {
                    picks.push(r);
                    props.hits.add(label, r, MouseCursor::Hand, props.slot);
                }
            }
            _ => suggest_path.set_visible(cx, false),
        }
        step
    }
}

impl DirPanel {
    /// Raises and lowers the three fields with the instance's own state, and
    /// seeds each one when it opens. Called from the draw as well as from
    /// the event, so a verb's field is up in the very frame it asked for.
    fn sync(
        &mut self,
        cx: &mut Cx,
        f: &Fields,
        path: &TextInputRef,
        name: &TextInputRef,
        rename: &TextInputRef,
    ) {
        if self
            .path_field
            .sync(cx, &self.view, PATH_ROW, path, f.pathing.as_deref())
        {
            // A fresh field, a fresh offer: nothing of the last walk.
            self.pac = Suggest::default();
        }
        self.name_field
            .sync(cx, &self.view, NAME_ROW, name, f.naming.as_deref());
        self.rename_field
            .sync(cx, &self.view, RENAME_ROW, rename, f.renaming.as_deref());
    }

    /// The deferred focus: a field takes the keyboard once it has been drawn
    /// where it will stand. The path field keeps its seed and puts the caret
    /// at the end; a name is a value to type over, so it lands selected.
    fn land(
        &mut self,
        cx: &mut Cx,
        path: &TextInputRef,
        name: &TextInputRef,
        rename: &TextInputRef,
    ) {
        self.path_field.land(cx, path, false);
        self.name_field.land(cx, name, true);
        self.rename_field.land(cx, rename, true);
    }

    /// The crumb line: the last four ancestors as dotted links — each
    /// replaces this panel with that directory, in place, which is the same
    /// walk one directory up — and the directory itself plain, last. While
    /// the `go to` field is up it stands in their place.
    fn crumbs(&mut self, cx: &mut Cx2d, props: &PanelProps, f: &Fields) {
        let crumbs = crumbs_of(props);
        let n = crumbs.len();
        let ancestors: Vec<&(String, PanelId)> = crumbs[..n.saturating_sub(1)]
            .iter()
            .rev()
            .take(CRUMB_SLOTS.len())
            .rev()
            .collect();
        for (i, (slot, sep)) in CRUMB_SLOTS.iter().enumerate() {
            let at = ancestors.get(i);
            if let Some((label, id)) = at {
                self.view.link(cx, slot).set(
                    cx,
                    label,
                    Nav::Replace {
                        slot: props.slot,
                        id: id.clone(),
                    },
                    true,
                    None,
                );
            }
            self.view.widget(cx, slot).set_visible(cx, at.is_some());
            // The disk's root is its own separator: `/ tmp`, not `/ / tmp`.
            self.view
                .widget(cx, sep)
                .set_visible(cx, at.is_some_and(|(l, _)| l != ROOT));
        }
        let here = crumbs.last().map(|(l, _)| l.clone()).unwrap_or_default();
        self.view.label(cx, HERE).set_text(cx, &here);
        self.view
            .widget(cx, CRUMBS)
            .set_visible(cx, f.pathing.is_none());
    }

    /// Enter in the `go to` field: the instance reads the path and answers
    /// where it leads, and the widget applies it.
    fn go_to(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, typed: &str) {
        let nav = {
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<Dir>()
                .and_then(|d| d.go_to(typed))
        };
        // A spelling that names nothing leaves the field where it is, with
        // the status line saying so.
        if nav.is_some() {
            cx.set_key_focus(self.view.area());
        }
        self.view.redraw(cx);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        match nav {
            Some(n) => session.nav(n),
            None => session.redraw(),
        }
    }

    /// Enter in the `new dir` field: the instance's own verb, on the
    /// instance the widget is holding — the same `&mut self` the bar's
    /// button reaches it with.
    fn new_dir(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, name: &str) {
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let closed = {
            let mut borrow = props.panel.borrow_mut();
            let Some(d) = borrow.as_any().downcast_mut::<Dir>() else {
                return;
            };
            d.new_dir(session, name);
            // The field closes itself when the directory was made; a refusal
            // keeps it, with the name still in it.
            d.naming().is_none()
        };
        if closed {
            cx.set_key_focus(self.view.area());
        }
        self.view.redraw(cx);
    }

    /// Enter in the `rename` field: the instance's own verb, on the instance
    /// the widget is holding. The panel is replaced with the new name in the
    /// layout half of the same action, so this one is on its way out — what
    /// it does with the keyboard is only for the frames in between.
    fn rename(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, name: &str) {
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let closed = {
            let mut borrow = props.panel.borrow_mut();
            let Some(d) = borrow.as_any().downcast_mut::<Dir>() else {
                return;
            };
            d.rename(session, name);
            // The field closes itself where the rename went through; a
            // refusal keeps it, with the name still in it.
            d.renaming().is_none()
        };
        if closed {
            cx.set_key_focus(self.view.area());
        }
        self.view.redraw(cx);
    }

    /// Esc: the field goes away and what stood in its place comes back, with
    /// nothing created, nothing gone to, and nothing renamed.
    fn close(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, which: Field) {
        edit(props, |d| {
            match which {
                Field::Path => d.set_pathing(None),
                Field::Name => d.set_naming(None),
                Field::Rename => d.set_renaming(None),
            }
            d.set_status(None);
        });
        cx.set_key_focus(self.view.area());
        self.view.redraw(cx);
        if let Some(session) = scope.data.get_mut::<Session>() {
            session.redraw();
        }
    }
}

/// Hands the instance the facts it cannot ask for itself — where it stands
/// in the join chain, and whether anybody has written the disk — and answers
/// what the widget draws around the rows.
///
/// Read-only on the session, and both borrows end with the call: nothing
/// here runs a verb.
fn observe(props: &PanelProps, scope: &mut Scope) -> Option<Fields> {
    let session = scope.data.get_mut::<Session>()?;
    let session: &Session = session;
    let mut borrow = props.panel.borrow_mut();
    let d = borrow.as_any().downcast_mut::<Dir>()?;
    d.observe(session);
    Some(Fields {
        naming: d.naming().map(str::to_string),
        renaming: d.renaming().map(str::to_string),
        pathing: d.pathing().map(str::to_string),
        status: d.note(),
    })
}

/// Remembers which run the line under the header is about, at the moment
/// it is drawn. Called from the draw pass alone — an event that refreshed
/// it would put the answer back to *now*, which is the very thing a stale
/// *cancel* must not be able to do.
fn drew(props: &PanelProps) {
    edit(props, super::super::panels::Dir::drawn);
}

/// What the three fields keep from the bars while one of them has the
/// keyboard: every letter, because the keydown above answers *any* cmd
/// chord while a caret blinks in `go to`, `new dir` or `rename` — so no
/// bar's letter would fire and none may be drawn as if it would.
///
/// The table says the same of its filter; this is the other three, which
/// are files' own. Said on every draw and every event, and said nothing at
/// all when none is live: the frame after the caret leaves must not still
/// be drawn as if it were there.
fn keeps(props: &PanelProps, live: bool) {
    if live {
        props.chord.field(Letters::ALL);
    }
}

/// Runs `f` on the instance, for the length of the call.
fn with_dir<R>(props: &PanelProps, f: impl FnOnce(&mut Dir) -> R) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    borrow.as_any().downcast_mut::<Dir>().map(f)
}

/// The same, where there is nothing to answer.
fn edit(props: &PanelProps, f: impl FnOnce(&mut Dir)) {
    with_dir(props, f);
}

/// The completion the `go to` field offers, off the instance's own world.
fn completion(props: &PanelProps) -> Option<PathCompletion> {
    with_dir(props, |d| d.completion())
}

/// The crumb line, read fresh: it is where the panel stands, and a panel
/// replaced in place stands somewhere else.
fn crumbs_of(props: &PanelProps) -> Vec<(String, PanelId)> {
    with_dir(props, |d| d.crumbs()).unwrap_or_default()
}
