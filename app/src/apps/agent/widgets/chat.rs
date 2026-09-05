//! One conversation, drawn: the transcript above, the composer below.
//!
//! The transcript is a `PortalList` of **items** rather than of turns: a
//! turn, a card for each tool call it asked for, the live tail while the
//! model is still writing, and the sentence a run that came to nothing
//! left. One template draws all of them, with the parts an item is not made
//! of *emptied* rather than merely hidden — a row that scrolls out is
//! reused by the next one in, and an emptied part cannot leave one turn's
//! words on another's.
//!
//! The asymmetry is the whole of who spoke: a person's turn is a washed
//! block on the right, as wide as its longest line and no wider than most
//! of the column; the agent's is plain text on the left, filling it. No
//! avatars, no timestamps, no colour anywhere but on a failure.
//!
//! Presses are answered here, by the rectangles of the last draw, because
//! portal-list items are rebuilt every draw and a synthesized press has to
//! land the way a finger does. The composer is a multi-line field with the
//! chips over it: `enter` sends, `shift+enter` is a newline, and a paste
//! that reads as a panel becomes a chip instead of text.

use std::collections::HashSet;
use std::rc::Rc;

use kernel::nav::Nav;
use kernel::session::Session;
use kernel::store::Store;
use kernel::theme;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::keys::{key_char, Letters};
use crate::shell::widgets::suggest::Suggest;

use super::super::chip::Chip;
use super::super::completion::PanelPick;
use super::super::model::{self, Call, CallId, Run, Turn, TurnId};
use super::super::panels::Chat;
use super::super::run::Tail;
use super::super::wire::Role;
use super::super::{calls, AGENT};

/// How many chips one row shows by name. Past this it says how many more
/// there are: a composer is a composer, and thirty panels must not push the
/// field off the panel.
const CHIP_SLOTS: usize = 5;

/// The slots the DSL lays out for them, in the composer and in a turn.
const CHIP_LBLS: [LiveId; CHIP_SLOTS] = [
    live_id!(k0),
    live_id!(k1),
    live_id!(k2),
    live_id!(k3),
    live_id!(k4),
];

/// How much of the column a person's block may take before it wraps.
const BLOCK_SHARE: f64 = 0.85;

/// The block's own inset, left and right together: what a line costs beyond
/// its own width.
const BLOCK_PAD: f64 = 18.0;

/// How much of a card's output is drawn. A JSON blob is read, not audited.
const OUTPUT_MAX: usize = 2000;

/// The cursor block at the end of a live tail.
const CARET: &str = "\u{258d}";

/// One line of the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    /// The person's turn, by its place in the transcript.
    Mine(usize),
    /// The agent's.
    Theirs(usize),
    /// One tool call the turn above it asked for.
    Card(usize),
    /// What has arrived of an answer still being written.
    Tail,
    /// The sentence a run that came to nothing left.
    Failed,
}

/// What one draw reads out of the instance, in one borrow.
struct Shown {
    turns: Rc<Vec<Turn>>,
    /// Every call of the conversation, in the order the model asked.
    calls: Vec<Call>,
    /// Whether each of those calls was a tool that changes something.
    writes: Vec<bool>,
    items: Vec<Item>,
    /// The muted line under an agent's turn — what the round cost, and the
    /// word it stopped on — by turn.
    foots: Vec<String>,
    run: Option<Run>,
    /// Whether the newest round is still being written.
    streaming: bool,
    tail: Option<Tail>,
    /// What the composer is carrying.
    chips: Vec<Chip>,
    draft: String,
}

/// What a press on one of the last draw's rectangles means.
#[derive(Debug, Clone)]
enum Spot {
    /// A chip: focus the panel it points at, where one still shows it.
    Focus(Chip),
    /// A chip's `×`, in the composer: take it off.
    Drop(usize),
    /// A card's first line: open it, or fold it again.
    Card(CallId),
    /// The agent's folded reasoning.
    Reason(TurnId),
}

/// The widget: the chat read fresh on every draw, so an answer that lands
/// while it is open lands on screen.
#[derive(Script, ScriptHook, Widget)]
pub struct AgentChatPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The mono face, measured rather than drawn: a person's block is as
    /// wide as its longest line, and in a monospaced face that is
    /// arithmetic rather than a second layout pass.
    #[live]
    draw_mono: DrawText,
    /// One character's advance at body size. Nought until the first draw
    /// has asked for it.
    #[rust]
    adv: f64,
    /// Where each pressable thing of the last draw landed.
    #[rust]
    spots: Vec<(Rect, Spot)>,
    /// The cards whose output is open, and the turns whose reasoning is.
    #[rust]
    open_cards: HashSet<CallId>,
    #[rust]
    open_reasons: HashSet<TurnId>,
    /// Whether the field has been offered the keyboard. Once, on the first
    /// event tick after the composer has been drawn — a field that has
    /// never been drawn is nowhere to put a caret.
    #[rust]
    mounted: bool,
    /// Whether this slot was the session's focus at the last event: the
    /// moment it becomes so is when the caret is owed to the field.
    #[rust]
    was_focused: bool,
    /// The caret is owed to the field, and goes there on the first event
    /// that is not a press — a press decides for itself — once the field
    /// has a rectangle.
    #[rust]
    want_caret: bool,
    /// The instance's draft as this widget last wrote it into the field.
    /// Between a keystroke and the action that reports it the field is
    /// ahead of the instance, which is why the comparison is against this
    /// and not against what the field says.
    #[rust]
    shown: String,
    /// The live tail's version as of the last draw: a streaming answer
    /// redraws when a word has arrived, not on every frame.
    #[rust]
    at: u64,
    /// Where the field landed in the last draw. A press inside it is a
    /// press on the caret: makepad's own focus-on-press reads a rectangle
    /// this widget has redrawn since, so the composer takes the keyboard
    /// itself rather than hoping.
    #[rust]
    field: Rect,
    /// A press landed in the composer and the caret is owed. Makepad deals
    /// key focus at the end of an event, and a selectable run that had it
    /// blurs *itself* on a release outside its own rectangle — after the
    /// field has asked. So the field asks again on the release, where it is
    /// the last to speak.
    #[rust]
    caret: bool,
    /// The frame a streaming run asks for.
    #[rust]
    next_frame: NextFrame,
    /// The *add panel* field's own completion box, hung under that field.
    #[live]
    suggest_pick: View,
    /// What it is offering: the panels that are open, matched by title.
    #[rust]
    picker: Suggest<PanelPick>,
    /// Whether the pick row was up at the last look, so the field is seeded
    /// when it opens and not written over as it is typed.
    #[rust]
    pick_up: bool,
    /// A field just raised wants the keyboard, once it has been drawn where
    /// it will stand: focus on a field with no rectangle lands nowhere.
    #[rust]
    pick_land: bool,
    /// The box's rows of the last draw, in the order it offers them: a
    /// press on one is a pick, and it must not reach what it covers.
    #[rust]
    pick_hits: Vec<Rect>,
}

impl Widget for AgentChatPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };

        // A paste that reads as a panel is a chip and never text — decided
        // before the field sees the event, because a field that has taken a
        // document in cannot give it back.
        if let Event::TextInput(t) = event {
            if t.was_paste && self.pasted(cx, &props, scope, &t.input) {
                return;
            }
        }

        let field = self.view.text_input(cx, ids!(ask_input));
        let pick = self.view.text_input(cx, ids!(pick_input));
        self.raise(cx, &props, &pick);
        self.picker.track(cx, &pick);
        let picking = self.pick_up && pick.key_focus(cx);
        let focused = field.key_focus(cx);
        if focused || picking {
            // Exactly the text chords: a caret owns `cmd+x/c/v/a` wherever
            // it blinks, and this bar's own letters stay bold and stay
            // firing — `s`, `k`, `r` and `n` are what the panel is for.
            props.chord.field(Letters::NONE);
        }
        if let Event::KeyDown(k) = event {
            if (focused || picking)
                && k.modifiers.logo
                && key_char(k.key_code).is_some_and(|c| Letters::TEXT.has(c))
            {
                props.chord.take();
            }
            // The pick field owns its own keys while it has the keyboard:
            // enter takes what the offer is showing, esc puts the field
            // away whether the offer is up or not — a picker is one gesture
            // and one way out of it.
            if picking {
                if k.key_code == KeyCode::Escape {
                    self.close_pick(cx, &props, scope);
                    return;
                }
                if let Some(c) = self.offer(&props, scope) {
                    let took = self.picker.key(cx, &c, &pick, k);
                    if k.key_code == KeyCode::ReturnKey {
                        self.take_pick(cx, &props, scope, &pick.text());
                        return;
                    }
                    if took {
                        self.mirror(&props, &pick.text());
                        self.view.redraw(cx);
                        return;
                    }
                }
            }
            // Enter sends; shift+enter is the field's own newline. Taken
            // before the field, which would otherwise put a line break in
            // and leave the words behind.
            if !picking && k.key_code == KeyCode::ReturnKey && !k.modifiers.shift {
                self.send(cx, &props, scope);
                return;
            }
        }

        // The box is drawn over the composer, so its rows answer a press
        // first: what is under them is not what was pressed.
        if let Event::MouseDown(e) = event {
            if self.pick_hits.iter().any(|r| r.contains(e.abs)) {
                if let Some(label) = self.pick_hit(cx, e.abs) {
                    self.take_pick(cx, &props, scope, &label);
                }
                return;
            }
        }

        self.view.handle_event(cx, event, scope);
        self.mount(cx, &props, scope);
        self.follow_focus(cx, &props, scope, event);
        if matches!(event, Event::MouseUp(_)) && std::mem::take(&mut self.caret) {
            field.set_key_focus(cx);
        }

        if let Event::Actions(actions) = event {
            if field.changed(actions).is_some() {
                self.edited(cx, &props, scope);
            }
            if field.key_focus_lost(actions) {
                field.set_cursor(cx, field.cursor(), false);
            }
            if pick.changed(actions).is_some() {
                self.mirror(&props, &pick.text());
                self.view.redraw(cx);
            }
            if pick.returned(actions).is_some() {
                self.take_pick(cx, &props, scope, &pick.text());
            }
            if pick.escaped(actions) {
                self.close_pick(cx, &props, scope);
            }
            if pick.key_focus_lost(actions) {
                pick.set_cursor(cx, pick.cursor(), false);
            }
        }

        if let Event::MouseDown(e) = event {
            self.press(cx, &props, scope, e);
        }

        // The run's hands: a call the model asked for runs here, with the
        // session, and the answer kicks the worker awake. A chat with
        // nothing waiting pays one cached query for the asking.
        if matches!(
            event,
            Event::Actions(_)
                | Event::Signal
                | Event::NextFrame(_)
                | Event::Timer(_)
                | Event::MouseUp(_)
        ) {
            self.serve(cx, &props, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        self.measure(cx);
        let Some(shown) = read(&props, scope) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        self.at = shown.tail.as_ref().map_or(0, |t| t.version);
        self.compose(cx, &shown);
        // Said on the draw as well as on the event: a verb is run while the
        // bar is being drawn, and its field is up in that very frame.
        let pick = self.view.text_input(cx, ids!(pick_input));
        self.raise(cx, &props, &pick);

        // The widest a person's block may be: most of the column, and the
        // block takes as much of that as its longest line asks for. The
        // column is the turtle this panel was handed — its own area is not
        // its own until the draw it is in has ended.
        let cap = (cx.turtle().rect().size.x * BLOCK_SHARE).max(120.0);

        let mut drawn: Vec<(Item, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, shown.items.len());
            while let Some(idx) = list.next_visible_item(cx) {
                let Some(it) = shown.items.get(idx).copied() else {
                    continue;
                };
                let row = list.item(cx, idx, live_id!(turn));
                self.populate(cx, &row, &shown, it, cap);
                row.draw_all(cx, scope);
                drawn.push((it, row));
            }
        }

        self.hits(cx, &props, &shown, drawn);
        self.draw_pick(cx, &props, scope, &pick);
        // A live answer asks for the frame that draws its next word; a
        // finished one asks for nothing, and the panel sits still.
        if shown.streaming {
            self.next_frame = cx.new_next_frame();
        }
        DrawStep::done()
    }
}

impl AgentChatPanel {
    /// One character's advance in the mono face, measured once. It is what
    /// a person's block is measured with.
    fn measure(&mut self, cx: &mut Cx2d) {
        if self.adv > 0.0 {
            return;
        }
        // Said here rather than taken from the DSL, as the chrome's own
        // measure is: a run prepared before anything has been drawn at this
        // size answers for whatever size the style was left at.
        self.draw_mono.text_style.font_size = theme::FONT_SIZE as f32;
        if let Some(run) = self
            .draw_mono
            .prepare_single_line_run(cx, "MMMMMMMMMMMMMMMM")
        {
            self.adv = f64::from(run.width_in_lpxs) / 16.0;
        }
    }

    /// The composer: the chips it is carrying, and the field.
    fn compose(&mut self, cx: &mut Cx2d, shown: &Shown) {
        let labels: Vec<String> = shown.chips.iter().map(Chip::label).collect();
        let row = self.view.widget(cx, ids!(chips));
        pills(cx, &row, &labels, true);
        // The instance's text, put back when it moved without a keystroke
        // of this widget's — which is what a send looks like from here.
        if shown.draft != self.shown {
            let field = self.view.text_input(cx, ids!(ask_input));
            if field.text() != shown.draft {
                field.set_text(cx, &shown.draft);
            }
            self.shown.clone_from(&shown.draft);
        }
    }

    /// One item, into the row template. Every part is written every time —
    /// the ones this item is not made of emptied rather than merely stood
    /// down, so a reused row cannot show the turn before it.
    fn populate(&self, cx: &mut Cx, row: &WidgetRef, shown: &Shown, item: Item, cap: f64) {
        self.fill_mine(cx, row, shown, item, cap);
        self.fill_theirs(cx, row, shown, item);
        self.fill_card(cx, row, shown, item);

        let why = match item {
            Item::Failed => shown
                .run
                .as_ref()
                .and_then(|r| r.error.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        row.widget(cx, ids!(err)).set_visible(cx, !why.is_empty());
        row.text_input(cx, ids!(err.err_txt)).set_text(cx, &why);
    }

    /// The person's block: the chips it carried over the words it said, in
    /// a wash, on the right.
    fn fill_mine(&self, cx: &mut Cx, row: &WidgetRef, shown: &Shown, item: Item, cap: f64) {
        let turn = match item {
            Item::Mine(i) => shown.turns.get(i),
            _ => None,
        };
        row.widget(cx, ids!(mine)).set_visible(cx, turn.is_some());
        let said = turn.map_or("", |t| t.text());
        let chips: Vec<String> = turn
            .map(|t| chips_of(t).iter().map(Chip::label).collect())
            .unwrap_or_default();
        row.text_input(cx, ids!(mine.wash.mine_txt))
            .set_text(cx, said);
        let pill_row = row.widget(cx, ids!(mine.wash.mine_chips));
        pills(cx, &pill_row, &chips, false);
        if turn.is_some() {
            // As wide as its longest line, and no wider than the cap. A
            // `Fit` would not wrap — a multi-line field lays its text out
            // against the width it is given — so the width is arithmetic
            // and the field inside it fills.
            let want = self.width_of(said, &chips).min(cap);
            if let Some(mut w) = row.widget(cx, ids!(mine.wash)).borrow_mut::<View>() {
                w.walk.width = Size::Fixed(want);
            }
        }
    }

    /// The agent's block: its reasoning folded over what it said, and the
    /// muted line under it.
    fn fill_theirs(&self, cx: &mut Cx, row: &WidgetRef, shown: &Shown, item: Item) {
        let theirs = matches!(item, Item::Theirs(_) | Item::Tail);
        row.widget(cx, ids!(theirs)).set_visible(cx, theirs);
        let turn = match item {
            Item::Theirs(i) => shown.turns.get(i),
            _ => None,
        };
        let (text, reasoning) = match item {
            Item::Theirs(_) => turn.map_or((String::new(), String::new()), |t| {
                (
                    t.text().to_string(),
                    t.message.reasoning_content.clone().unwrap_or_default(),
                )
            }),
            // The live tail wears the cursor block, which is the whole of
            // how one sees that it is still being written.
            Item::Tail => shown.tail.as_ref().map_or_else(
                || (CARET.to_string(), String::new()),
                |t| (format!("{}{CARET}", t.text), t.reasoning.clone()),
            ),
            _ => (String::new(), String::new()),
        };
        let has = !reasoning.trim().is_empty();
        let open = turn.is_some_and(|t| self.open_reasons.contains(&t.id));
        row.widget(cx, ids!(theirs.reason_fold))
            .set_visible(cx, has && !open);
        row.widget(cx, ids!(theirs.reason_wrap))
            .set_visible(cx, has && open);
        row.text_input(cx, ids!(theirs.reason_wrap.reason_txt))
            .set_text(cx, if has && open { reasoning.trim() } else { "" });
        row.text_input(cx, ids!(theirs.theirs_txt))
            .set_text(cx, &text);
        let foot = match item {
            Item::Theirs(i) => shown.foots.get(i).cloned().unwrap_or_default(),
            _ => String::new(),
        };
        let foot_lbl = row.label(cx, ids!(theirs.foot_lbl));
        foot_lbl.set_text(cx, &foot);
        foot_lbl.set_visible(cx, !foot.is_empty());
    }

    /// One tool call's card: what it did on the first line, and behind it
    /// what it came to — or, where it failed, why, in the colour errors
    /// get.
    fn fill_card(&self, cx: &mut Cx, row: &WidgetRef, shown: &Shown, item: Item) {
        let call = match item {
            Item::Card(i) => shown.calls.get(i).map(|c| (i, c)),
            _ => None,
        };
        row.widget(cx, ids!(card)).set_visible(cx, call.is_some());
        let folded = call.is_some_and(|(i, c)| self.folded(i, c, shown));
        row.label(cx, ids!(card.card_line.card_lbl))
            .set_text(cx, &call.map_or(String::new(), |(_, c)| line_of(c, folded)));
        let output = call
            .map(|(_, c)| c)
            .filter(|c| c.status == model::CALL_DONE && !folded)
            .map(|c| clip(&c.said(), OUTPUT_MAX))
            .unwrap_or_default();
        row.widget(cx, ids!(card.card_out))
            .set_visible(cx, !output.is_empty());
        row.text_input(cx, ids!(card.card_out.card_out_txt))
            .set_text(cx, &output);
        let failed = call
            .map(|(_, c)| c)
            .filter(|c| c.status == model::CALL_FAILED)
            .map(|c| clip(&c.said(), OUTPUT_MAX))
            .unwrap_or_default();
        row.widget(cx, ids!(card.card_err))
            .set_visible(cx, !failed.is_empty());
        row.text_input(cx, ids!(card.card_err.card_err_txt))
            .set_text(cx, &failed);
    }

    /// How wide a person's block wants to be: its longest line, its widest
    /// chip, and the inset around them.
    fn width_of(&self, text: &str, chips: &[String]) -> f64 {
        let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
        // A pill is its label in the section register — four fifths of the
        // body advance — inside its own border and padding.
        let widest = chips
            .iter()
            .map(|c| c.chars().count() * 4 / 5 + 4)
            .max()
            .unwrap_or(0);
        let cols = longest.max(widest);
        #[allow(clippy::cast_precision_loss)]
        self.adv.mul_add(cols as f64, BLOCK_PAD)
    }

    /// Whether this card is folded: a reading tool's output is a page of
    /// JSON nobody asked to see, and one press shows it. A writing tool
    /// says what it did on its line and has nothing behind it.
    fn folded(&self, i: usize, call: &Call, shown: &Shown) -> bool {
        call.status == model::CALL_DONE
            && !call.said().is_empty()
            && !shown.writes.get(i).copied().unwrap_or(false)
            && !self.open_cards.contains(&call.id)
    }

    /// The hits: every turn by its first line, every card by its own, and
    /// every chip by the panel it points at. What a script addresses, and
    /// what the shell puts a cursor on.
    fn hits(
        &mut self,
        cx: &mut Cx2d,
        props: &PanelProps,
        shown: &Shown,
        drawn: Vec<(Item, WidgetRef)>,
    ) {
        self.spots.clear();
        let clip_rect = self.view.widget(cx, ids!(list)).area().rect(cx);
        for (item, row) in drawn {
            let at = |cx: &mut Cx2d, path: &[LiveId]| {
                visible(row.widget(cx, path).area().rect(cx), clip_rect)
            };
            match item {
                Item::Mine(i) => {
                    let Some(turn) = shown.turns.get(i) else {
                        continue;
                    };
                    if let (Some(r), Some(line)) =
                        (at(cx, ids!(mine.wash)), first_line(turn.text()))
                    {
                        props.hits.add(line, r, MouseCursor::Text, props.slot);
                    }
                    let pill_row = row.widget(cx, ids!(mine.wash.mine_chips));
                    chip_hits(
                        cx,
                        props,
                        &mut self.spots,
                        &pill_row,
                        &chips_of(turn),
                        false,
                    );
                }
                Item::Theirs(i) => {
                    let Some(turn) = shown.turns.get(i) else {
                        continue;
                    };
                    if let (Some(r), Some(line)) =
                        (at(cx, ids!(theirs.theirs_txt)), first_line(turn.text()))
                    {
                        props.hits.add(line, r, MouseCursor::Text, props.slot);
                    }
                    if row.widget(cx, ids!(theirs.reason_fold)).visible() {
                        if let Some(r) = at(cx, ids!(theirs.reason_fold)) {
                            props
                                .hits
                                .add("› reasoning", r, MouseCursor::Hand, props.slot);
                            self.spots.push((r, Spot::Reason(turn.id)));
                        }
                    }
                    // What the round cost and how it ended is a fact about
                    // the turn and not a thing to press — but it is
                    // addressable, so a run can assert that an answer was
                    // cut short rather than photograph a grey line.
                    let foot = shown.foots.get(i).cloned().unwrap_or_default();
                    if let Some(r) = at(cx, ids!(theirs.foot_lbl)) {
                        if !foot.is_empty() {
                            props.hits.add(foot, r, MouseCursor::Default, props.slot);
                        }
                    }
                }
                Item::Card(i) => {
                    let Some(call) = shown.calls.get(i) else {
                        continue;
                    };
                    if let Some(r) = at(cx, ids!(card.card_line)) {
                        let label = line_of(call, self.folded(i, call, shown));
                        props.hits.add(label, r, MouseCursor::Hand, props.slot);
                        self.spots.push((r, Spot::Card(call.id)));
                    }
                    if call.status == model::CALL_FAILED {
                        let said = call.said();
                        if let (Some(r), Some(line)) =
                            (at(cx, ids!(card.card_err.card_err_txt)), first_line(&said))
                        {
                            props.hits.add(line, r, MouseCursor::Text, props.slot);
                        }
                    }
                }
                Item::Tail => {
                    let text = shown
                        .tail
                        .as_ref()
                        .map_or(String::new(), |t| t.text.clone());
                    if let (Some(r), Some(line)) =
                        (at(cx, ids!(theirs.theirs_txt)), first_line(&text))
                    {
                        props.hits.add(line, r, MouseCursor::Text, props.slot);
                    }
                }
                Item::Failed => {
                    let why = shown
                        .run
                        .as_ref()
                        .and_then(|r| r.error.clone())
                        .unwrap_or_default();
                    if let (Some(r), Some(line)) = (at(cx, ids!(err.err_txt)), first_line(&why)) {
                        props.hits.add(line, r, MouseCursor::Text, props.slot);
                    }
                }
            }
        }
        // The composer, last: its chips take a press over anything the list
        // left under them, and the field is what a script puts a caret in
        // by name.
        let pill_row = self.view.widget(cx, ids!(chips));
        chip_hits(cx, props, &mut self.spots, &pill_row, &shown.chips, true);
        self.field = self.view.widget(cx, ids!(ask_input)).area().rect(cx);
        if self.field.size.x > 0.0 {
            props
                .hits
                .add("ask", self.field, MouseCursor::Text, props.slot);
        }
    }

    /// The first look at a live panel: the caret goes in the field, so a
    /// chat one has just opened is a chat one can type in.
    ///
    /// Held until the field has a rectangle — focus on a field that has
    /// never been drawn is focus on nothing — and only where this panel is
    /// the focused one: a chat previewed beside the agents list is being
    /// read, not written in.
    fn mount(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        if self.mounted {
            return;
        }
        let field = self.view.text_input(cx, ids!(ask_input));
        if field.area().rect(cx).size.x <= 0.0 {
            return;
        }
        self.mounted = true;
        let focused = scope
            .data
            .get_mut::<Session>()
            .and_then(|s| s.focus())
            .is_some_and(|f| f == props.slot);
        if focused {
            field.set_key_focus(cx);
        }
    }

    /// Focus follows the panel: the moment this slot becomes the session's
    /// focus — a chord, the launcher, a fresh open — the caret goes into the
    /// field, or into the picker's while that is up, so a chat one has just
    /// reached is a chat one can type in. Applied on the first event that is
    /// not a press, because a press decides for itself where the caret goes
    /// ([`press`](Self::press)), and held until the field has a rectangle.
    fn follow_focus(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, event: &Event) {
        let focused = scope
            .data
            .get_mut::<Session>()
            .and_then(|s| s.focus())
            .is_some_and(|f| f == props.slot);
        if focused && !self.was_focused {
            self.want_caret = true;
        }
        self.was_focused = focused;
        if !focused {
            self.want_caret = false;
            return;
        }
        let pressing = matches!(
            event,
            Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseMove(_) | Event::Scroll(_)
        );
        if !self.want_caret || pressing {
            return;
        }
        let target = if self.pick_up {
            self.view.text_input(cx, ids!(pick_input))
        } else {
            self.view.text_input(cx, ids!(ask_input))
        };
        if target.area().rect(cx).size.x <= 0.0 {
            return;
        }
        target.set_key_focus(cx);
        self.want_caret = false;
    }

    /// The field changed: the panel keeps the text, and the bar is drawn
    /// again — *send* comes and goes with it.
    fn edited(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        let text = self.view.text_input(cx, ids!(ask_input)).text();
        self.shown.clone_from(&text);
        {
            let mut borrow = props.panel.borrow_mut();
            if let Some(c) = borrow.as_any().downcast_mut::<Chat>() {
                c.set_draft(&text);
            }
        }
        if let Some(session) = scope.data.get_mut::<Session>() {
            session.redraw();
        }
    }

    /// `enter`: what is in the composer goes, unless something is already
    /// going — while a run is live the bar wears *stop*, and the key that
    /// would send has nowhere to send.
    fn send(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        self.edited(cx, props, scope);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        {
            let mut borrow = props.panel.borrow_mut();
            let Some(chat) = borrow.as_any().downcast_mut::<Chat>() else {
                return;
            };
            if chat.latest_run().as_ref().is_some_and(Run::live) {
                return;
            }
            chat.send(session);
        }
        self.shown.clear();
        self.view.text_input(cx, ids!(ask_input)).set_text(cx, "");
        self.view.redraw(cx);
    }

    /// Raises and lowers the *add panel* row with the instance's own state,
    /// seeds the field on the look it opens, and hands it the keyboard once
    /// it has a rectangle to take it in — focus on a field that has never
    /// been drawn lands nowhere. Called from the draw as well as from the
    /// event, so the verb's field is up in the very frame it asked for.
    fn raise(&mut self, cx: &mut Cx, props: &PanelProps, pick: &TextInputRef) {
        let up = {
            let mut borrow = props.panel.borrow_mut();
            match borrow.as_any().downcast_mut::<Chat>() {
                Some(c) => c.picking().is_some(),
                None => return,
            }
        };
        if up != self.pick_up {
            self.pick_up = up;
            self.view.widget(cx, ids!(pick_row)).set_visible(cx, up);
            if up {
                pick.set_text(cx, "");
                // A fresh field, a fresh offer: nothing of the last pick.
                self.picker = Suggest::default();
                self.pick_land = true;
            }
        }
        if self.pick_land && up && pick.area().rect(cx).size.y > 0.0 {
            self.pick_land = false;
            pick.set_key_focus(cx);
        }
    }

    /// What the pick field is offering: the panels that are open, by title,
    /// this chat left out. Read fresh — a panel opened while the field is up
    /// is one more thing to pick.
    fn offer(&self, props: &PanelProps, scope: &mut Scope) -> Option<PanelPick> {
        let session = scope.data.get_mut::<Session>()?;
        let mut borrow = props.panel.borrow_mut();
        let chat = borrow.as_any().downcast_mut::<Chat>()?;
        let open = chat
            .pickable(session)
            .into_iter()
            .map(|(slot, title)| {
                let at = session
                    .ws()
                    .ws_of(slot)
                    .map_or_else(String::new, |k| format!("ws {}", k + 1));
                (title, at)
            })
            .collect();
        Some(PanelPick { open })
    }

    /// The field's text into the instance, so the row it draws and the row
    /// it is are one thing.
    fn mirror(&self, props: &PanelProps, text: &str) {
        let mut borrow = props.panel.borrow_mut();
        if let Some(c) = borrow.as_any().downcast_mut::<Chat>() {
            c.set_picking(Some(text));
        }
    }

    /// A pick taken: the panel's chip into the composer and the field away,
    /// with the keyboard back where this panel's work is. A spelling that
    /// names no open panel leaves the field where it is.
    fn take_pick(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, typed: &str) {
        let took = match scope.data.get_mut::<Session>() {
            Some(session) => {
                let mut borrow = props.panel.borrow_mut();
                match borrow.as_any().downcast_mut::<Chat>() {
                    Some(c) => c.add_panel(session, typed),
                    None => false,
                }
            }
            None => false,
        };
        if took {
            self.view.text_input(cx, ids!(ask_input)).set_key_focus(cx);
        }
        if let Some(session) = scope.data.get_mut::<Session>() {
            session.redraw();
        }
        self.view.redraw(cx);
    }

    /// `esc`: the field away, nothing added, the keyboard back in the
    /// composer.
    fn close_pick(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        {
            let mut borrow = props.panel.borrow_mut();
            if let Some(c) = borrow.as_any().downcast_mut::<Chat>() {
                c.set_picking(None);
            }
        }
        self.view.text_input(cx, ids!(ask_input)).set_key_focus(cx);
        if let Some(session) = scope.data.get_mut::<Session>() {
            session.redraw();
        }
        self.view.redraw(cx);
    }

    /// The offer, drawn last of all — after the transcript and the composer
    /// — so it covers what it hangs over, and its rows registered last, so a
    /// press on one wins over what is underneath.
    fn draw_pick(
        &mut self,
        cx: &mut Cx2d,
        props: &PanelProps,
        scope: &mut Scope,
        pick: &TextInputRef,
    ) {
        self.pick_hits.clear();
        if self.pick_up {
            // Addressable by name: that is all a script needs to put a
            // caret in it. Only while the row is up — a hidden widget keeps
            // its last rectangle.
            let r = self.view.widget(cx, ids!(pick_input)).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add("panel", r, MouseCursor::Text, props.slot);
            }
        }
        let store = scope.data.get_mut::<Session>().map(|s| s.store().clone());
        let offer = self.offer(props, scope);
        let (Some(store), Some(c)) = (store, offer.filter(|_| self.pick_up)) else {
            self.suggest_pick.set_visible(cx, false);
            return;
        };
        let Self {
            suggest_pick,
            picker,
            pick_hits,
            ..
        } = self;
        picker.draw(cx, scope, &store, &c, pick, suggest_pick);
        for (label, r) in picker.hits(cx, suggest_pick) {
            pick_hits.push(r);
            props.hits.add(label, r, MouseCursor::Hand, props.slot);
        }
    }

    /// Which row of the open box a press landed on, by the title it wears.
    fn pick_hit(&mut self, cx: &mut Cx, at: DVec2) -> Option<String> {
        let Self {
            suggest_pick,
            picker,
            ..
        } = self;
        picker
            .hits(cx, suggest_pick)
            .into_iter()
            .find(|(_, r)| r.contains(at))
            .map(|(label, _)| label)
    }

    /// A paste that reads as a panel. Answers whether it was one, which is
    /// whether the field is to be kept out of it.
    fn pasted(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, text: &str) -> bool {
        let Some(session) = scope.data.get_mut::<Session>() else {
            return false;
        };
        // Made before the chat is borrowed: a chip reads the panel it
        // points at, and that panel is any of the open ones.
        let Some(chip) = Chip::from_paste(session, text) else {
            return false;
        };
        {
            let mut borrow = props.panel.borrow_mut();
            if let Some(c) = borrow.as_any().downcast_mut::<Chat>() {
                c.add_chip(chip);
            }
        }
        self.view.redraw(cx);
        true
    }

    /// A press, answered by the rectangles of the last draw.
    fn press(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope, e: &MouseDownEvent) {
        // Only this panel's own, and only where nothing was drawn over
        // them: the hit table settles that, as it does for a human.
        if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
            return;
        }
        let spot = self
            .spots
            .iter()
            .rev()
            .find(|(r, _)| r.contains(e.abs))
            .map(|(_, s)| s.clone());
        match spot {
            Some(Spot::Card(id)) => {
                if !self.open_cards.remove(&id) {
                    self.open_cards.insert(id);
                }
            }
            Some(Spot::Reason(id)) => {
                if !self.open_reasons.remove(&id) {
                    self.open_reasons.insert(id);
                }
            }
            Some(Spot::Drop(i)) => {
                let mut borrow = props.panel.borrow_mut();
                if let Some(c) = borrow.as_any().downcast_mut::<Chat>() {
                    c.remove_chip(i);
                }
            }
            // A chip goes to the panel it points at, where one is still
            // showing it; a chip whose panel has closed points at something
            // that is not on screen, and says nothing.
            Some(Spot::Focus(chip)) => {
                if let Some(session) = scope.data.get_mut::<Session>() {
                    if let Some(slot) = chip.open_slot(session) {
                        session.nav(Nav::Focus(slot));
                    }
                }
            }
            // Anywhere else in the panel: the keyboard is this chat's, so
            // the next `enter` sends into it, and the caret goes in the
            // field — except on a run of text, where a press is a selection
            // starting, which must not have the caret taken from it a frame
            // later.
            None => {
                let on_text = props
                    .hits
                    .at(e.abs)
                    .is_some_and(|h| matches!(h.cursor, MouseCursor::Text))
                    && !self.field.contains(e.abs);
                if on_text {
                    self.want_caret = false;
                } else {
                    self.caret = true;
                    if self.field.contains(e.abs) {
                        self.view.text_input(cx, ids!(ask_input)).set_key_focus(cx);
                    }
                }
                if let Some(session) = scope.data.get_mut::<Session>() {
                    session.nav(Nav::Focus(props.slot));
                }
                return;
            }
        }
        self.view.redraw(cx);
    }

    /// Runs the calls the chat's run is waiting on, and draws the answer a
    /// live one has added since the last frame.
    fn serve(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        let (chat, run) = {
            let mut borrow = props.panel.borrow_mut();
            match borrow.as_any().downcast_mut::<Chat>() {
                Some(c) => (c.chat(), c.latest_run()),
                None => return,
            }
        };
        // A word that has arrived since the last draw is a reason to draw;
        // a frame with nothing new in it is not.
        if let Some(r) = run.filter(|r| r.status == model::STREAMING) {
            if AGENT.tail(r.id).is_some_and(|t| t.version != self.at) {
                self.view.redraw(cx);
            }
        }
        let (Some(chat), Some(session)) = (chat, scope.data.get_mut::<Session>()) else {
            return;
        };
        if calls::run_pending_calls(session, chat) > 0 {
            session.redraw();
            self.view.redraw(cx);
        }
    }
}

/// One row of pills: the pill itself, and — in the composer — the `×` that
/// takes it off. Beyond the slots the row has, a count.
fn pills(cx: &mut Cx, row: &WidgetRef, labels: &[String], composing: bool) {
    row.set_visible(cx, !labels.is_empty());
    for (i, name) in CHIP_LBLS.iter().enumerate() {
        let pill = row.widget(cx, &[*name]);
        let shown = labels.get(i);
        pill.label(cx, ids!(chip_lbl))
            .set_text(cx, shown.map_or("", String::as_str));
        pill.widget(cx, ids!(chip_x))
            .set_visible(cx, composing && shown.is_some());
        pill.set_visible(cx, shown.is_some());
    }
    let rest = labels.len().saturating_sub(CHIP_SLOTS);
    let more = row.label(cx, ids!(chip_more));
    more.set_text(cx, &format!("+{rest}"));
    more.set_visible(cx, rest > 0);
}

/// One row of pills' hits: a chip's label is a panel's title, so the hit
/// says which it is — `chip inbox`, never `inbox`, which is the panel.
fn chip_hits(
    cx: &mut Cx2d,
    props: &PanelProps,
    spots: &mut Vec<(Rect, Spot)>,
    row: &WidgetRef,
    chips: &[Chip],
    composing: bool,
) {
    for (i, chip) in chips.iter().take(CHIP_SLOTS).enumerate() {
        let pill = row.widget(cx, &[CHIP_LBLS[i]]);
        let r = pill.area().rect(cx);
        if r.size.x <= 0.0 {
            continue;
        }
        let label = chip.label();
        props
            .hits
            .add(format!("chip {label}"), r, MouseCursor::Hand, props.slot);
        spots.push((r, Spot::Focus(chip.clone())));
        if !composing {
            continue;
        }
        let x = pill.widget(cx, ids!(chip_x)).area().rect(cx);
        if x.size.x > 0.0 {
            props
                .hits
                .add(format!("× {label}"), x, MouseCursor::Hand, props.slot);
            spots.push((x, Spot::Drop(i)));
        }
    }
}

/// Everything one draw needs, read out of the instance and the store in one
/// pass.
fn read(props: &PanelProps, scope: &mut Scope) -> Option<Shown> {
    let store: Rc<Store> = scope.data.get_mut::<Session>()?.store().clone();
    let (turns, run, streaming, foots, chips, draft, id) = {
        let mut borrow = props.panel.borrow_mut();
        let chat = borrow.as_any().downcast_mut::<Chat>()?;
        let turns = chat.turns();
        let foots = turns.iter().map(|t| foot_line(chat, t)).collect();
        (
            turns,
            chat.latest_run(),
            chat.status().as_deref() == Some(model::STREAMING),
            foots,
            chat.chips().to_vec(),
            chat.draft().to_string(),
            chat.chat(),
        )
    };
    // Every call of the conversation, in the order the model asked: a card
    // hangs off the turn that asked for it, and a run goes round as many
    // times as the model wants tools.
    let mut calls: Vec<Call> = Vec::new();
    if let Some(id) = id {
        for r in model::runs(&store, id).iter() {
            calls.extend(model::calls(&store, r.id).iter().cloned());
        }
    }
    // Which of them changed something — the one thing a card asks of the
    // registry, and a tool no app in this build offers changes nothing.
    let writes: Vec<bool> = match scope.data.get_mut::<Session>() {
        Some(s) => calls
            .iter()
            .map(|c| s.apps().tool(&c.tool).is_some_and(|t| t.writes))
            .collect(),
        None => vec![false; calls.len()],
    };
    let tail = run
        .as_ref()
        .filter(|_| streaming)
        .and_then(|r| AGENT.tail(r.id));
    let items = items(&turns, &calls, run.as_ref(), tail.is_some());
    Some(Shown {
        turns,
        calls,
        writes,
        items,
        foots,
        run,
        streaming,
        tail,
        chips,
        draft,
    })
}

/// The transcript as lines: a turn, the cards it asked for under it, the
/// live tail at the foot, and the sentence of a run that came to nothing.
///
/// A `tool` turn draws nothing of its own — what a call came to is on the
/// card — and an agent's turn that only asked for tools draws nothing
/// either, since the cards are what it said.
fn items(turns: &[Turn], calls: &[Call], run: Option<&Run>, tailing: bool) -> Vec<Item> {
    let mut items = Vec::new();
    for (i, t) in turns.iter().enumerate() {
        match t.message.role {
            Role::User => items.push(Item::Mine(i)),
            Role::Assistant => {
                let said = !t.text().trim().is_empty()
                    || t.message
                        .reasoning_content
                        .as_ref()
                        .is_some_and(|r| !r.trim().is_empty());
                if said {
                    items.push(Item::Theirs(i));
                }
                items.extend(
                    calls
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.turn == t.id)
                        .map(|(j, _)| Item::Card(j)),
                );
            }
            Role::System | Role::Tool => {}
        }
    }
    if tailing {
        items.push(Item::Tail);
    }
    if run.is_some_and(|r| r.error.is_some()) {
        items.push(Item::Failed);
    }
    items
}

/// The chips a turn carried, as this build reads them: one from another
/// build's is not guessed at, and simply does not draw.
fn chips_of(turn: &Turn) -> Vec<Chip> {
    turn.chips.iter().filter_map(Chip::from_json).collect()
}

/// The muted line under an agent's turn: what the round cost, and the word
/// it stopped on where that word is worth saying. Empty for a round that
/// has said neither.
fn foot_line(chat: &Chat, turn: &Turn) -> String {
    if turn.message.role != Role::Assistant {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(usage) = chat.usage_line(turn) {
        parts.push(usage);
    }
    if let Some(mark) = finish_mark(turn.finish.as_deref()) {
        parts.push(mark.to_string());
    }
    parts.join(" · ")
}

/// What a finish reason reads as, where it is worth reading: an answer that
/// simply ended says nothing at all.
fn finish_mark(finish: Option<&str>) -> Option<&'static str> {
    match finish {
        Some("length") => Some("cut short"),
        Some("stopped") => Some("stopped"),
        Some("content_filter") => Some("filtered"),
        _ => None,
    }
}

/// A card's first line: what the call did, in one line.
///
/// A call that **wrote** says the sentence its own node wears — *rename
/// “README.txt” to “readme-renamed.txt”* — which is the same words the undo
/// tree offers to take back, so the card and the history agree without
/// either quoting the other. Everything else — a reading tool, a call that
/// refused before it did anything — is the tool by name with a compact
/// reading of the arguments the model wrote for it. A folded card wears the
/// same mark a folded quote does.
#[must_use]
pub fn card_line(call: &Call) -> String {
    if let Some(label) = call.label.as_ref().filter(|l| !l.trim().is_empty()) {
        return label.clone();
    }
    let args = summarize(&call.input());
    if args.is_empty() {
        call.tool.clone()
    } else {
        format!("{} {args}", call.tool)
    }
}

/// The same, said as this card stands: still running, folded, or plain.
fn line_of(call: &Call, folded: bool) -> String {
    let line = card_line(call);
    if call.status == model::CALL_PENDING {
        return format!("{line} … running");
    }
    if folded {
        return format!("› {line}");
    }
    line
}

/// The arguments of a call on one line: the values the model wrote, each
/// clipped to what a line can hold.
fn summarize(input: &serde_json::Value) -> String {
    let parts: Vec<String> = match input {
        serde_json::Value::Object(map) => map.values().map(scalar).collect(),
        other => vec![scalar(other)],
    };
    let line = parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    clip(&line, 120)
}

/// One value as a line says it: a string bare, anything else as its JSON.
fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => clip(s, 60),
        serde_json::Value::Null => String::new(),
        other => clip(&other.to_string(), 60),
    }
}

/// A text cut to `max` characters, with the cut said rather than hidden.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// The first line of a run of text, as a hit's label: what a script
/// addresses a turn by. `None` where there is nothing to address.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    (!line.is_empty()).then(|| clip(line, 120))
}

/// The part of a row that is on screen: `None` for one scrolled entirely
/// out. A zero-sized clip means the list has not drawn yet, and the row
/// stands as it is.
fn visible(r: Rect, clip: Rect) -> Option<Rect> {
    if r.size.x <= 0.0 || r.size.y <= 0.0 {
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
