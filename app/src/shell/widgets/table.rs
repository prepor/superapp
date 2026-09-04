//! The rich table's shell half: one helper that draws and drives any
//! [`ListState`] a panel instance owns.
//!
//! The state is the panel's, never the widget's: the table borrows it from
//! the scope through `as_any` on every draw and event, and everything it
//! changes it changes there. What a panel supplies is four short functions
//! ([`RowSpec`]) — the row template, how to fill a row, what a script calls
//! it, and what it opens.
//!
//! It provides the filter with its error line and completion box, the rows
//! through a `PortalList` with the hidden marks above them, the cursor wash
//! and the mark bar, the keys the grammar gives a list, and the hits that
//! make a row addressable. Presses are answered here, by the row rectangles
//! of the last draw, because portal-list items are rebuilt every draw and a
//! synthesized press must land the way a finger does.
//!
//! A finger is the shell's to arbitrate and the table's to answer: rows are
//! registered as rows ([`Hits::add_row`](super::super::hits::Hits::add_row)),
//! and the three questions a gesture over one raises — which row, what a
//! sweep would run, and run it — arrive through
//! [`Grab`](super::super::hosted::Grab).

use kernel::nav::Nav;
use kernel::panel::PanelId;
use kernel::richtable::{Datasource, ListState, MarkSlot, Table};
use kernel::session::Session;
use kernel::store::Store;
use makepad_widgets::*;

use super::super::hosted::{Ask, PanelProps};
use super::super::keys::Letters;
use super::suggest::Suggest;

/// The children a table expects in its panel's template.
const FILTER: &[LiveId] = ids!(filter_input);
const FILTER_ERR: &[LiveId] = ids!(filter_err_lbl);
const EMPTY: &[LiveId] = ids!(empty_lbl);
/// The rows live in a `PortalList` under this name — public because the
/// shell draws a swipe curtain clipped to it.
pub const LIST: &[LiveId] = ids!(list);

/// The two band templates inside that `PortalList`.
const CAPTION_TPL: LiveId = live_id!(caption);
const BAND_RULE_TPL: LiveId = live_id!(band_rule);

/// The four twins of a row, in the order [`line`] picks them.
const TWINS: [LiveId; 4] = [
    live_id!(line),
    live_id!(line_sel),
    live_id!(line_mark),
    live_id!(line_mark_sel),
];

/// The row of a spec's source, spelled once.
pub type RowOf<S> = <<S as RowSpec>::Src as Datasource>::Row;

/// What a panel's widget tells the table about its rows.
///
/// Implemented by a zero-sized type beside the panel's widget, so the table
/// is generic over it and a list panel writes no draw loop of its own.
pub trait RowSpec: 'static {
    /// Where the rows come from.
    type Src: Datasource;

    /// The instance that owns the [`ListState`].
    type Panel: 'static;

    /// The list state inside that instance.
    fn list(panel: &mut Self::Panel) -> &mut ListState<Self::Src>;

    /// The row template inside the panel's `PortalList`.
    fn row_tpl() -> LiveId;

    /// Fills one row. `row` is the item widget; [`line()`] answers the twin
    /// to write into and puts the cursor wash and the mark bar on it.
    fn populate(cx: &mut Cx, row: &WidgetRef, r: &RowOf<Self>, selected: bool, marked: bool);

    /// What a script — and so the hit table — addresses this row by.
    fn label(r: &RowOf<Self>) -> String;

    /// What the row opens, previews, and is replaced by.
    fn target(r: &RowOf<Self>) -> PanelId;

    /// What the field is seeded with, once, before the first draw. Empty
    /// for most tables; a log opens on a default so that what narrows the
    /// list is on screen and one `cmd+a` clears it.
    fn default_filter() -> &'static str {
        ""
    }

    /// The same, for a panel that computes its own — a list opened *about*
    /// something starts narrowed to it, and what it was opened about is on
    /// the instance rather than in the spec. Defaults to the constant above.
    fn seed_filter(_panel: &Self::Panel) -> String {
        Self::default_filter().to_string()
    }

    /// The line an empty list shows, given the filter it is empty under.
    /// Empty for a table that would rather show nothing.
    fn empty_line(_filter: &str) -> String {
        String::new()
    }

    /// What a sideways sweep across one of this panel's rows runs: the verb
    /// a leftward sweep fires and the verb a rightward one fires, by id.
    /// `None` where that way means nothing here — the curtain then never
    /// appears and the lift does nothing.
    ///
    /// They are the panel's own verbs, run over the swept row alone: a
    /// gesture is a verb like any other, so a finger and a bar can never
    /// offer different ones. The panel answers, not the spec, because
    /// whether a list may file its rows is a fact about that list — an inbox
    /// archives and no other mailbox does.
    fn swipe_verbs(_panel: &Self::Panel) -> [Option<&'static str>; 2] {
        [None, None]
    }
}

/// The twin a row is drawn in: plain, washed by the cursor, wearing the
/// mark bar, or both. Shows that one, stands the other three down, and
/// answers the widget a panel's `populate` writes into.
pub fn line(cx: &mut Cx, row: &WidgetRef, selected: bool, marked: bool) -> WidgetRef {
    let at = usize::from(selected) + 2 * usize::from(marked);
    let mut out = WidgetRef::empty();
    for (i, id) in TWINS.iter().enumerate() {
        let w = row.widget(cx, &[*id]);
        w.set_visible(cx, i == at);
        if i == at {
            out = w;
        }
    }
    out
}

/// The shell half of one rich table.
pub struct TableView<S: RowSpec> {
    ac: Suggest<Table<S::Src>>,
    /// Where each row of the last draw landed: the table index (`None` for
    /// a mark the filter hides, which is outside the table), the rectangle,
    /// and what it opens.
    rows: Vec<(Option<usize>, Rect, PanelId)>,
    /// The open completion's rows, in the same order it offers them.
    picks: Vec<Rect>,
    /// A pick took the press and owes the release: the box is gone by then,
    /// so the rectangle cannot be asked twice.
    picking: bool,
    /// Whether the default filter has been typed in. Once, before the first
    /// draw; after that the field is the operator's, empty included.
    primed: bool,
}

impl<S: RowSpec> Default for TableView<S> {
    fn default() -> Self {
        TableView {
            ac: Suggest::default(),
            rows: Vec::new(),
            picks: Vec::new(),
            picking: false,
            primed: false,
        }
    }
}

/// Runs `f` on the list state the instance owns. The borrow lasts exactly
/// as long as the call: a navigation taken while it stood would find the
/// session walking the same instance.
fn with_list<S: RowSpec, R>(
    props: &PanelProps,
    f: impl FnOnce(&mut ListState<S::Src>) -> R,
) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    let panel = borrow.as_any().downcast_mut::<S::Panel>()?;
    Some(f(S::list(panel)))
}

impl<S: RowSpec> TableView<S> {
    // -- events ---------------------------------------------------------------

    /// Every event the panel's widget sees. `view` is the panel's own tree;
    /// the completion box is only read at draw time, so it stays there.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope, view: &mut View) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let Some(store) = scope.data.get_mut::<Session>().map(|s| s.store().clone()) else {
            return;
        };
        // A finger, arbitrated by the shell and answered here. It is not a
        // press: the carrier event says nothing, the props say everything,
        // and nothing below this line should see it.
        if let Some(ask) = props.grab.ask() {
            self.grab(cx, &props, &store, view, scope, ask);
            return;
        }

        let field = view.text_input(cx, FILTER);
        let focused = field.key_focus(cx);
        filter_keeps(&props, focused);
        self.ac.track(cx, &field);

        // How many rows were marked before this event; see the redraw at the
        // foot of it.
        let marked = with_list::<S, _>(&props, |l| l.marks().len());

        let mut navs: Vec<Nav> = Vec::new();

        // The completion's own rows are drawn over everything else, and they
        // answer first, in both hands. A press on one is a pick and neither
        // half of it reaches what the box hangs over — the release as much
        // as the press, because a focused makepad field blurs *itself* on a
        // release outside its own rectangle, and the caret the pick just
        // parked in the filter would go with it.
        if let Event::MouseDown(e) = event {
            if let Some(i) = self.picks.iter().position(|r| r.contains(e.abs)) {
                let ac = &mut self.ac;
                with_list::<S, _>(&props, |l| ac.pick(cx, l.table(), &field, i));
                self.picking = true;
                view.redraw(cx);
                return;
            }
        }
        if matches!(event, Event::MouseUp(_)) && self.picking {
            self.picking = false;
            return;
        }

        if let Event::KeyDown(k) = event {
            // A live field keeps the chords it needs: `cmd+a` stays
            // select-all rather than firing a verb on the bar.
            if focused && k.modifiers.logo {
                props.chord.take();
            }
            // The completion owns the arrows, enter, tab and esc while it
            // is open; the field never sees them.
            let ac = &mut self.ac;
            let took =
                with_list::<S, _>(&props, |l| ac.key(cx, l.table(), &field, k)).unwrap_or(false);
            if took {
                view.redraw(cx);
                return;
            }
            // `↓` out of the filter and onto the first row: the field is
            // one line, so down has nothing else to mean, and the walk
            // starts where the eye already is.
            if focused && leaves_filter_down(k) {
                let text = field.text();
                let landed = with_list::<S, _>(&props, |l| {
                    l.set_filter(&text);
                    Some((l.set_cursor(&store, 0)?, l.list_index(0)))
                })
                .flatten();
                if let Some((row, li)) = landed {
                    leave_field(cx, view);
                    self.follow(cx, view, li);
                    navs.push(Nav::Preview {
                        from: props.slot,
                        id: S::target(&row),
                    });
                    view.redraw(cx);
                }
                self.apply(scope, navs);
                return;
            }
        }

        view.handle_event(cx, event, scope);

        // `/` focuses the filter and space marks the cursor's row — the two
        // plain keys the grammar keeps, arriving as text the way a letter
        // does. In a live field they are a slash and a space.
        if let Event::TextInput(t) = event {
            if !focused && t.input == "/" {
                focus_field(cx, &field);
            } else if !focused && t.input == " " {
                with_list::<S, _>(&props, |l| l.toggle_mark(&store));
                view.redraw(cx);
            }
        }

        if let Event::KeyDown(k) = event {
            if !focused {
                self.row_key(cx, &props, &store, view, k, &mut navs);
            }
        }

        if let Event::MouseDown(e) = event {
            self.press(cx, &props, &store, view, e, &mut navs);
        }

        if let Event::Actions(actions) = event {
            self.actions(cx, &props, &store, view, actions, &mut navs);
        }

        // The marked set feeds the panel's batch verbs, and a bar is drawn by
        // the stage rather than by this view: a mark that only redrew the
        // panel would put its verbs on the bar a frame late. Asked once, at
        // the foot, so every path that touches the set is covered by it.
        if with_list::<S, _>(&props, |l| l.marks().len()) != marked {
            if let Some(session) = scope.data.get_mut::<Session>() {
                session.redraw();
            }
        }

        self.apply(scope, navs);
    }

    /// The keys a list answers to while the rows have the keyboard.
    fn row_key(
        &mut self,
        cx: &mut Cx,
        props: &PanelProps,
        store: &Store,
        view: &mut View,
        k: &KeyEvent,
        navs: &mut Vec<Nav>,
    ) {
        match k.key_code {
            // Enter *goes*: unlike the walk's preview it hands focus to
            // what it opened, which is the solid-link rule.
            KeyCode::ReturnKey => {
                let target = with_list::<S, _>(props, |l| {
                    let i = l.cursor_index(store).unwrap_or(0);
                    l.row(store, i).map(|r| S::target(&r))
                })
                .flatten();
                if let Some(id) = target {
                    navs.push(Nav::Open {
                        from: props.slot,
                        id,
                        fresh: k.modifiers.logo || k.modifiers.alt,
                    });
                }
            }
            // The row walk, with scroll-follow: each step previews what it
            // lands on and keeps the keyboard. Shift marks the row it
            // leaves and the row it lands on — a range, by the walk's own
            // keys.
            KeyCode::ArrowDown | KeyCode::ArrowUp => {
                let d: isize = if k.key_code == KeyCode::ArrowDown {
                    1
                } else {
                    -1
                };
                let landed = with_list::<S, _>(props, |l| {
                    let row = if k.modifiers.shift {
                        l.mark_range(store, d)
                    } else {
                        l.move_cursor(store, d)
                    }?;
                    Some((S::target(&row), l.list_index(l.cursor_index(store)?)))
                })
                .flatten();
                if let Some((id, li)) = landed {
                    self.follow(cx, view, li);
                    navs.push(Nav::Preview {
                        from: props.slot,
                        id,
                    });
                }
                view.redraw(cx);
            }
            // Esc empties the marks; a live field keeps its own esc.
            KeyCode::Escape => {
                with_list::<S, _>(props, ListState::clear_marks);
                view.redraw(cx);
            }
            // A list's one-stop tab ring: the filter.
            KeyCode::Tab => {
                let field = view.text_input(cx, FILTER);
                focus_field(cx, &field);
            }
            _ => {}
        }
    }

    /// The three questions a finger over a row raises, answered by the
    /// rectangles of the last draw — the same ones a press resolves against.
    fn grab(
        &mut self,
        cx: &mut Cx,
        props: &PanelProps,
        store: &Store,
        view: &mut View,
        scope: &mut Scope,
        ask: Ask,
    ) {
        match ask {
            // The phone's way to a mark: space and shift belong to a
            // keyboard, and a finger has neither.
            Ask::Mark(p) => {
                let Some(i) = self.row_at(p) else {
                    return;
                };
                with_list::<S, _>(props, |l| {
                    let Some(row) = l.row(store, i) else {
                        return;
                    };
                    let key = l.table().key(&row);
                    l.marks_mut().toggle(key);
                });
                view.redraw(cx);
                // The marked set feeds the bar, which the stage draws.
                if let Some(session) = scope.data.get_mut::<Session>() {
                    session.redraw();
                }
            }

            Ask::Verbs(p) => {
                if self.row_at(p).is_none() {
                    return;
                }
                props.grab.answer(swipe_verbs::<S>(props));
            }

            // The committed sweep. The row is marked alone, the panel's own
            // verb runs over that set, and whatever was marked before goes
            // back on: a gesture borrows the batch machinery rather than
            // asking for a second door into the same action.
            Ask::Run { at, left } => {
                let Some(i) = self.row_at(at) else {
                    return;
                };
                let Some(id) = swipe_verbs::<S>(props)[usize::from(!left)] else {
                    return;
                };
                let saved = with_list::<S, _>(props, |l| {
                    let saved = l.marks_mut().take();
                    if let Some(row) = l.row(store, i) {
                        let key = l.table().key(&row);
                        l.marks_mut().add(key);
                    }
                    saved
                });
                if let Some(session) = scope.data.get_mut::<Session>() {
                    props.panel.borrow_mut().run(id, session);
                }
                if let Some(saved) = saved {
                    with_list::<S, _>(props, |l| {
                        l.clear_marks();
                        l.marks_mut().extend(saved);
                    });
                }
                view.redraw(cx);
            }
        }
    }

    /// The table index of the row a point is on: `None` off the rows, and
    /// `None` on a mark the filter hides, which is outside the table.
    fn row_at(&self, p: DVec2) -> Option<usize> {
        self.rows
            .iter()
            .rev()
            .find(|(_, r, _)| r.contains(p))
            .and_then(|(at, _, _)| *at)
    }

    /// A press, answered by the rectangles of the last draw.
    fn press(
        &mut self,
        cx: &mut Cx,
        props: &PanelProps,
        store: &Store,
        view: &mut View,
        e: &MouseDownEvent,
        navs: &mut Vec<Nav>,
    ) {
        let p = e.abs;
        // Only this panel's own rows, and only where nothing was drawn over
        // them: the hit table settles that, as it does for a human.
        if props.hits.at(p).map(|h| h.slot) != Some(Some(props.slot)) {
            return;
        }
        let Some((at, _, target)) = self.rows.iter().rev().find(|(_, r, _)| r.contains(p)).cloned()
        else {
            return;
        };
        // The keyboard belongs to the rows now, not to the filter.
        leave_field(cx, view);
        navs.push(Nav::Focus(props.slot));
        // A mark the filter hides is outside the table: opening it moves no
        // cursor.
        if let Some(i) = at {
            with_list::<S, _>(props, |l| l.set_cursor(store, i));
        }
        view.redraw(cx);
        // cmd (alt as a quiet alias) always opens a fresh, un-joined panel.
        navs.push(if e.modifiers.logo || e.modifiers.alt {
            Nav::Open {
                from: props.slot,
                id: target,
                fresh: true,
            }
        } else {
            Nav::Preview {
                from: props.slot,
                id: target,
            }
        });
    }

    /// What the field and the list report after the fact.
    fn actions(
        &mut self,
        cx: &mut Cx,
        props: &PanelProps,
        store: &Store,
        view: &mut View,
        actions: &Actions,
        navs: &mut Vec<Nav>,
    ) {
        let field = view.text_input(cx, FILTER);
        if field.key_focus_lost(actions) {
            field.set_cursor(cx, field.cursor(), false);
        }
        if field.returned(actions).is_some() || field.escaped(actions) {
            leave_field(cx, view);
            if field.returned(actions).is_some() {
                let text = field.text();
                let landed = with_list::<S, _>(props, |l| {
                    l.set_filter(&text);
                    l.set_cursor(store, 0)
                })
                .flatten();
                if let Some(row) = landed {
                    navs.push(Nav::Preview {
                        from: props.slot,
                        id: S::target(&row),
                    });
                }
            }
            view.redraw(cx);
        }
        if field.changed(actions).is_some() {
            let text = field.text();
            with_list::<S, _>(props, |l| l.set_filter(&text));
            view.redraw(cx);
        }
        // The end of the list came on screen: a source without a count
        // loads its next page here.
        if view
            .widget(cx, LIST)
            .as_portal_list()
            .reached_end(actions)
            && with_list::<S, _>(props, |l| l.table_mut().extend(store)).unwrap_or(false)
        {
            view.redraw(cx);
        }
    }

    /// Applies what the event decided, once the instance is no longer
    /// borrowed.
    fn apply(&self, scope: &mut Scope, navs: Vec<Nav>) {
        if navs.is_empty() {
            return;
        }
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        for n in navs {
            session.nav(n);
        }
    }

    /// Keeps the cursor's row on screen as the walk moves it.
    fn follow(&self, cx: &mut Cx, view: &View, li: usize) {
        let list = view.widget(cx, LIST).as_portal_list();
        let visible = list
            .borrow()
            .is_some_and(|l| l.items().iter().any(|(i, _)| *i == li));
        if !visible {
            list.smooth_scroll_to(cx, li, 90.0, None, 0.0);
        }
    }

    // -- the draw --------------------------------------------------------------

    /// The whole panel: the filter, the band of hidden marks, the rows, the
    /// completion box, and the hits that make all three addressable.
    pub fn draw(
        &mut self,
        cx: &mut Cx2d,
        scope: &mut Scope,
        walk: Walk,
        view: &mut View,
        suggest: &mut View,
    ) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return view.draw_walk(cx, scope, walk);
        };
        let Some(store) = scope.data.get_mut::<Session>().map(|s| s.store().clone()) else {
            return view.draw_walk(cx, scope, walk);
        };

        let field = view.text_input(cx, FILTER);
        if !self.primed {
            self.primed = true;
            // Asked of the instance, so a list that opens narrowed to what
            // it is about says so in its own field.
            let seed = {
                let mut borrow = props.panel.borrow_mut();
                borrow
                    .as_any()
                    .downcast_mut::<S::Panel>()
                    .map(|p| S::seed_filter(p))
                    .unwrap_or_default()
            };
            if !seed.is_empty() {
                field.set_text(cx, &seed);
            }
        }
        let text = field.text();
        let focused = field.key_focus(cx);
        filter_keeps(&props, focused);

        // The instance's list, borrowed for the length of the draw. Nothing
        // reached from here runs a verb, so nothing else wants it.
        let instance = props.panel.clone();
        let mut borrow = instance.borrow_mut();
        let Some(panel) = borrow.as_any().downcast_mut::<S::Panel>() else {
            drop(borrow);
            return view.draw_walk(cx, scope, walk);
        };
        let list = S::list(panel);
        list.set_filter(&text);

        // What the filter could not read — minus the tag still being typed,
        // which is not wrong yet.
        let err = if focused {
            list.table()
                .errors_while_typing()
                .first()
                .map(|e| e.message.clone())
        } else {
            list.table().errors().first().map(|e| e.message.clone())
        };
        let err_lbl = view.label(cx, FILTER_ERR);
        err_lbl.set_text(cx, err.as_deref().unwrap_or(""));
        err_lbl.set_visible(cx, err.is_some());

        // The marks: what the filter shows and what it hides, read fresh by
        // key each draw. A mark whose row is gone goes with it.
        list.sync(&store);
        let n = list.len(&store);
        let pre = list.prefix();
        let cursor = list.cursor_index(&store);

        let empty_lbl = view.label(cx, EMPTY);
        let said = S::empty_line(&text);
        empty_lbl.set_text(cx, &said);
        empty_lbl.set_visible(cx, n == 0 && err.is_none() && !said.is_empty());

        let mut drawn: Vec<(Option<usize>, WidgetRef, String, PanelId)> = Vec::new();
        while let Some(item) = view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut pl) = list_ref.borrow_mut() else {
                continue;
            };
            pl.set_item_range(cx, 0, n + pre);
            while let Some(idx) = pl.next_visible_item(cx) {
                let (row, marked, at) = match list.slot(idx) {
                    // The band above the rows: its caption, and the rule
                    // that closes it.
                    MarkSlot::Caption | MarkSlot::Rule => {
                        let tpl = if matches!(list.slot(idx), MarkSlot::Caption) {
                            CAPTION_TPL
                        } else {
                            BAND_RULE_TPL
                        };
                        pl.item(cx, idx, tpl).draw_all(cx, scope);
                        continue;
                    }
                    MarkSlot::Hidden(row) => (row, true, None),
                    MarkSlot::Row(i) => {
                        let Some(row) = list.row(&store, i) else {
                            continue;
                        };
                        let marked = list.marks().has(&list.table().key(&row));
                        (row, marked, Some(i))
                    }
                };
                let w = pl.item(cx, idx, S::row_tpl());
                S::populate(cx, &w, &row, at.is_some() && at == cursor, marked);
                w.draw_all(cx, scope);
                drawn.push((at, w, S::label(&row), S::target(&row)));
            }
        }

        // The hits: the filter, then every row by the label the panel gives
        // it. Later hits win where they overlap, so the completion's rows —
        // registered last — take a press over the rows they cover.
        let fr = field.area().rect(cx);
        if fr.size.x > 0.0 {
            props.hits.add("filter", fr, MouseCursor::Text, props.slot);
        }
        // Clipped to the rows' own rectangle: a `PortalList` item reports the
        // whole of itself, so the one half-scrolled at either end reaches
        // past the list — over the bar at the panel's foot, where it would
        // take a click meant for a verb, and under the filter. What is
        // hittable is what is visible.
        let clip = view.widget(cx, LIST).area().rect(cx);
        self.rows.clear();
        for (at, w, label, target) in drawn {
            let Some(r) = visible(w.area().rect(cx), clip) else {
                continue;
            };
            props.hits.add_row(label, r, MouseCursor::Hand, props.slot);
            self.rows.push((at, r, target));
        }

        self.ac
            .draw(cx, scope, &store, list.table(), &field, suggest);
        let picks = self.ac.hits(cx, suggest);
        self.picks = picks.iter().map(|(_, r)| *r).collect();
        for (label, r) in picks {
            props.hits.add(label, r, MouseCursor::Hand, props.slot);
        }

        drop(borrow);
        DrawStep::done()
    }
}

/// The part of a row that is on screen: `None` for one scrolled entirely
/// out. A zero-sized clip means the list has not drawn yet, and the row
/// stands as it is.
fn visible(r: Rect, clip: Rect) -> Option<Rect> {
    if r.size.x <= 0.0 {
        return None;
    }
    if clip.size.y <= 0.0 {
        return Some(r);
    }
    let top = r.pos.y.max(clip.pos.y);
    let bot = (r.pos.y + r.size.y).min(clip.pos.y + clip.size.y);
    (bot > top).then(|| Rect {
        pos: dvec2(r.pos.x, top),
        size: dvec2(r.size.x, bot - top),
    })
}

/// What a sweep across this panel's rows would run, asked of the instance.
fn swipe_verbs<S: RowSpec>(props: &PanelProps) -> [Option<&'static str>; 2] {
    let mut borrow = props.panel.borrow_mut();
    borrow
        .as_any()
        .downcast_mut::<S::Panel>()
        .map_or([None, None], |p| S::swipe_verbs(p))
}

/// What the filter keeps from the bars while it has the keyboard, said on
/// every draw and every event — the promise a bold letter makes is about
/// now, and the bar is drawn before this widget is.
///
/// Every letter, not only the text chords: the keydown above answers *any*
/// cmd chord while the caret is in the filter, so no bar's letter would fire
/// and none may be drawn as if it would.
fn filter_keeps(props: &PanelProps, focused: bool) {
    if focused {
        props.chord.field(Letters::ALL);
    }
}

/// Hands the keyboard from the filter back to the rows.
///
/// The panel's own view, never `Area::Empty`: a makepad text field that has
/// never been drawn — a run a job panel folds away, say — reads an
/// `Area::Empty` key focus as its own, and takes the keyboard back on the
/// next release. Any real area settles it, and this one is always drawn.
fn leave_field(cx: &mut Cx, view: &View) {
    cx.set_key_focus(view.area());
}

/// Puts the caret in a field and selects what is in it, so typing replaces
/// rather than appends.
fn focus_field(cx: &mut Cx, field: &TextInputRef) {
    field.set_key_focus(cx);
    if let Some(mut t) = field.borrow_mut() {
        t.select_all(cx);
    }
}

/// Whether this key is the plain `↓` that leaves a filter for the rows
/// under it. Modified downs are not: `cmd+↓` is the shell's focus walk and
/// `shift+↓` is the field's own selection.
fn leaves_filter_down(k: &KeyEvent) -> bool {
    k.key_code == KeyCode::ArrowDown
        && !k.modifiers.shift
        && !k.modifiers.control
        && !k.modifiers.alt
        && !k.modifiers.logo
}
