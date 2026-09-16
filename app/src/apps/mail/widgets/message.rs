//! The reader: one conversation, as rows that open in place.
//!
//! A closed row is one line — who wrote, the first line they wrote, the date
//! — and a press on it unfolds the letter under the same header. What is
//! open is the instance's, so the panel asks for as many rows as its
//! conversation reads as, and unfolding one asks the layout again.
//!
//! The header fields — who the conversation is with and its SUBJECT at the
//! top, an open letter's FROM and TO under its row — are selectable runs and
//! not labels: an address and a subject are what a person carries out of a
//! letter. The letter's own `From` sits inside the letter rather than on the
//! row that names it, because that row is the toggle and a drag across it
//! would fold the letter it was selecting from.
//!
//! The TO at the top is folded: three first names, and a count that is the
//! press unfolding everyone, one a line. That press is the one the header
//! answers, and it sits outside the list the rows are in, so its rectangle
//! is kept on the widget the way the rows' are.
//!
//! A letter with an HTML reading is drawn through Makepad's `Html` widget,
//! and a plain one through a selectable run; both readings are written on
//! every populate — the hidden one *emptied* rather than merely hidden, so no
//! mail can leave its text behind for the next one to show. Its pictures come
//! from [`pictures`], never from the frame that draws them.
//!
//! Presses are answered here, by the row rectangles of the last draw: items
//! of a portal list are rebuilt every draw, and a synthesized press has to
//! land the way a finger does.

use std::collections::{HashMap, HashSet};

use kernel::nav::Nav;
use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::dsl::SLinkWidgetRefExt;
use crate::shell::hosted::PanelProps;

use super::super::display::Letter;
use super::super::model::{self, MailFull, MailId};
use super::super::panels::Message;
use super::super::parts::Attachment;
use super::pictures;
use crate::reader::{link_runs, HtmlContent};

/// How many parts one open message lists by name. Past this the line says how
/// many more there are: a row is a row, and a letter with thirty attachments
/// must not push the next message off the panel.
const ATT_SLOTS: usize = 5;

/// The slots the DSL lays out for them.
const ATT_LINKS: [LiveId; ATT_SLOTS] = [
    live_id!(a0),
    live_id!(a1),
    live_id!(a2),
    live_id!(a3),
    live_id!(a4),
];

/// Where one row of the last draw landed, and what a press on it means.
struct RowHit {
    mail: MailId,
    /// The header: a press toggles the letter.
    head: Rect,
    /// The fold under an open letter, while its quote is folded.
    quote: Option<Rect>,
}

/// What a press on the reader landed on, by the rectangles of the last
/// draw: the header's fold, or one of a row's two.
enum Press {
    /// The fold on the TO line: everyone the conversation is with unfolds,
    /// or folds back to the three names.
    People,
    /// A row's header: the letter under it folds or unfolds.
    Letter(MailId),
    /// The fold under an open letter: its quoted tail unfolds.
    Quote(MailId),
}

/// The widget: the thread read fresh on every draw, so a letter that lands
/// while it is open lands on screen.
#[derive(Script, ScriptHook, Widget)]
pub struct MessagePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    rows: Vec<RowHit>,
    #[rust]
    html: HashMap<MailId, (HtmlContent, HtmlContent)>,
    /// The conversation's rectangle of the last draw: what a clip in a
    /// letter is told it must be inside to be on the screen.
    #[rust]
    viewport: Option<Rect>,
    /// The fold on the TO line, of the last draw, while it was drawn: the
    /// one press the header answers, outside the list the rows are in.
    #[rust]
    people_fold: Option<Rect>,
}

impl Widget for MessagePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        match event {
            // A picture that arrived off the frame has to be placed, and the
            // item that wants it may be anywhere in the tree.
            Event::Actions(actions) => {
                if pictures::landed(cx, actions) || crate::reader::html_landed(actions) {
                    self.view.redraw(cx);
                }
                self.opened_links(cx, actions, scope);
            }
            Event::NetworkResponses(responses) if pictures::arrived(cx, responses) => {
                self.view.redraw(cx);
            }
            _ => {}
        }
        let Event::MouseDown(e) = event else { return };
        // Only this panel's own rows, and only where nothing was drawn over
        // them: the hit table settles that, as it does for a human.
        if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
            return;
        }
        // The header's fold first, since it is outside the list the rows
        // are in; then the rows.
        let Some(press) = self
            .people_fold
            .filter(|r| r.contains(e.abs))
            .map(|_| Press::People)
            .or_else(|| {
                self.rows.iter().rev().find_map(|r| {
                    if r.head.contains(e.abs) {
                        Some(Press::Letter(r.mail))
                    } else if r.quote.is_some_and(|q| q.contains(e.abs)) {
                        Some(Press::Quote(r.mail))
                    } else {
                        None
                    }
                })
            })
        else {
            return;
        };
        // The borrow ends before anything is asked of the session: a relayout
        // walks every instance, this one included.
        let relayout = {
            let mut borrow = props.panel.borrow_mut();
            let Some(m) = borrow.as_any().downcast_mut::<Message>() else {
                return;
            };
            // The wish changes with what is open and with what the header
            // unfolds, so either asks for the rows the panel now reads as;
            // a quote is inside the letter's own scroll and asks for nothing.
            match press {
                Press::People => {
                    m.toggle_people();
                    true
                }
                Press::Letter(mail) => {
                    m.toggle(mail);
                    true
                }
                Press::Quote(mail) => {
                    m.toggle_quote(mail);
                    false
                }
            }
        };
        self.view.redraw(cx);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        if relayout {
            session.relayout();
        } else {
            session.redraw();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        // Cloned out of the instance: the row loop hands `scope` on to each
        // item, so nothing may still be borrowing it by then.
        let Some((reading, open, quoted, people_open)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Message>().map(|m| {
                let reading = m.reading();
                let open: Vec<bool> = reading
                    .letters
                    .iter()
                    .map(|t| m.is_open(t.mail.head.id))
                    .collect();
                let quoted: Vec<bool> = reading
                    .letters
                    .iter()
                    .map(|t| m.quoted(t.mail.head.id))
                    .collect();
                (reading, open, quoted, m.people_open())
            })
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let msgs = &reading.letters;
        let present: HashSet<_> = msgs.iter().map(|letter| letter.mail.head.id).collect();
        self.html.retain(|mail, _| present.contains(mail));

        // Who the conversation is with, said once at the top and folded:
        // three first names and how many people there are, or one person in
        // full. The count is the press that unfolds everyone — one a line,
        // the copies marked — and it says the whole count rather than what
        // the line left out, because a control says what it is. A store
        // whose recipient rows are not filled in yet has nobody to fold, and
        // reads as it did: the first letter's bare TO line. Under it the
        // subject, which the chrome also wears — truncated there, and here
        // whole and selectable, because a subject is a thing people quote.
        let people = &reading.people;
        let bare = msgs.first().map_or("", |t| t.mail.to.as_str());
        let line = model::people_line(people).0;
        let shown = if people.is_empty() {
            bare
        } else {
            line.as_str()
        };
        self.view
            .text_input(cx, ids!(people_txt))
            .set_text(cx, shown);
        // One person is already named in full on the line and has nothing
        // to unfold; from two on, the fold is there.
        let n = people.len();
        let fold = if people_open {
            "fewer".to_string()
        } else {
            format!("{n} people")
        };
        self.view
            .label(cx, ids!(people_fold.people_lbl))
            .set_text(cx, &fold);
        self.view
            .widget(cx, ids!(people_fold))
            .set_visible(cx, n >= 2);
        // Emptied when it folds, not merely hidden — for the reason the two
        // readings of a letter are, below.
        let everyone = people_open && !people.is_empty();
        let block = if everyone {
            model::people_block(people)
        } else {
            String::new()
        };
        self.view
            .text_input(cx, ids!(everyone_wrap.everyone_txt))
            .set_text(cx, &block);
        self.view
            .widget(cx, ids!(everyone_wrap))
            .set_visible(cx, everyone);
        self.view
            .text_input(cx, ids!(subject_txt))
            .set_text(cx, &reading.title);

        // Opening a reading requests its inline files from the cache or
        // IMAP. Pictures deduplicates these asks and retries failed downloads.
        for (i, t) in msgs.iter().enumerate() {
            let mid = t.mail.head.id;
            if !open[i] || !t.has_cids {
                continue;
            }
            if let Some(s) = scope.data.get_mut::<Session>() {
                pictures::want_cid_parts(cx, s.world(), mid, t.image_scope.clone());
            }
        }

        let n = msgs.len();
        let mut drawn: Vec<(usize, WidgetRef)> = Vec::new();
        // Where the reading is, so a clip in a letter knows whether it is
        // on the screen; the rectangle of the last draw is what there is.
        crate::reader::pictures::set_viewport(cx, self.viewport);
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, n);
            while let Some(idx) = list.next_visible_item(cx) {
                let Some(t) = msgs.get(idx) else { continue };
                let row = list.item(cx, idx, live_id!(msg));
                populate(
                    cx,
                    &row,
                    t,
                    open[idx],
                    quoted[idx],
                    props.slot,
                    self.html.entry(t.mail.head.id).or_default(),
                );
                row.draw_all(cx, scope);
                drawn.push((idx, row));
            }
        }

        // The hits, once the rows have landed: the header a press toggles,
        // and — while a letter is open — its reading as a selectable run and
        // the fold over its quote. A part's link registers its own.
        //
        // The pictures that stood in a link left their rectangles behind as
        // they drew; which letter each belongs to is what its rows say.
        let pics = pictures::link_rects(cx);
        self.rows.clear();
        for (idx, row) in drawn {
            let Some(t) = msgs.get(idx) else { continue };
            let (mail, is_open) = (t.mail.head.id, open[idx]);
            let rect = |path: &[LiveId]| {
                let r = row.widget(cx, path).area().rect(cx);
                (r.size.x > 0.0 && r.size.y > 0.0).then_some(r)
            };
            let Some(head) = rect(ids!(head)) else {
                continue;
            };
            props
                .hits
                .add(head_label(t, is_open), head, MouseCursor::Hand, props.slot);
            let quote = if is_open && row.widget(cx, ids!(body.quote_fold)).visible() {
                rect(ids!(body.quote_fold))
            } else {
                None
            };
            // The quoted tail, while it is unfolded and is an HTML reading:
            // its links are the letter's too.
            let tail = if is_open && row.widget(cx, ids!(body.quote_html)).visible() {
                rect(ids!(body.quote_html.quote_body))
            } else {
                None
            };
            if let Some(q) = quote {
                props.hits.add("› quoted", q, MouseCursor::Hand, props.slot);
            }
            let mut letter = None;
            if is_open {
                // The `From` of an open letter, under its header: the name
                // is on the row above, which is a press, so the address
                // people copy lives here, where there is nothing to press.
                if let Some(r) = rect(ids!(body.from_wrap.from_txt)) {
                    props
                        .hits
                        .add("mail from", r, MouseCursor::Text, props.slot);
                }
                // And who it went to, under that — while the letter names
                // anyone. The row is hidden when it does not, and a hidden
                // run keeps the rectangle of the last time it was drawn.
                let to = if row.widget(cx, ids!(body.to_wrap)).visible() {
                    rect(ids!(body.to_wrap.to_txt))
                } else {
                    None
                };
                if let Some(r) = to {
                    props.hits.add("mail to", r, MouseCursor::Text, props.slot);
                }
                let path = if t.mail.html.is_some() {
                    ids!(body.html_wrap.body_html)
                } else {
                    ids!(body.text_wrap.body_txt)
                };
                let label = if t.mail.html.is_some() {
                    "mail html"
                } else {
                    "mail body"
                };
                if let Some(r) = rect(path) {
                    props.hits.add(label, r, MouseCursor::Text, props.slot);
                    letter = t.mail.html.is_some().then_some(r);
                }
            }
            // The `$Forwarded` mark is a fact about the letter and not a
            // thing to press — but it is addressable, so a script can assert
            // that a letter was passed on rather than photograph an arrow.
            if t.mail.forwarded {
                if let Some(r) = rect(ids!(head.fwd_lbl)) {
                    props
                        .hits
                        .add("passed on", r, MouseCursor::Default, props.slot);
                }
            }
            // What a reading carries that answers a press — a link, the
            // summary line of a fold, a picture that is a link — as
            // rectangles of its own over the reading's. The pointer is
            // painted from the hit table, so without them a link would wear
            // the I-beam the letter around it is read with.
            for (path, area) in [
                (ids!(body.html_wrap.body_html), letter),
                (ids!(body.quote_html.quote_body), tail),
            ] {
                let Some(area) = area else { continue };
                for r in link_runs(cx, &row, path, area, &pics) {
                    props.hits.add("link", r, MouseCursor::Hand, props.slot);
                }
            }
            self.rows.push(RowHit { mail, head, quote });
        }
        // The conversation's own fields: a run answers a press itself, and
        // the hit is what puts the I-beam over it and lets a script name it.
        // Named rather than addressed by what they say, so a subject cannot
        // outrank the mailbox row that carries the same words. The list of
        // everyone is emptied while it is folded, and an empty run is not
        // registered.
        let fields: [(&str, &[LiveId]); 3] = [
            ("mail people", ids!(people_txt)),
            ("mail people list", ids!(everyone_wrap.everyone_txt)),
            ("mail subject", ids!(subject_txt)),
        ];
        for (label, path) in fields {
            let run = self.view.text_input(cx, path);
            let r = run.area().rect(cx);
            if r.size.x > 0.0 && !run.text().is_empty() {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }
        // The fold on the TO line is addressed by what it says — `6 people`,
        // `fewer` — as a control is. Its rectangle is kept for the press,
        // which lands here rather than in the list the rows are in.
        let fold_view = self.view.widget(cx, ids!(people_fold));
        self.people_fold = fold_view
            .visible()
            .then(|| fold_view.area().rect(cx))
            .filter(|r| r.size.x > 0.0 && r.size.y > 0.0);
        if let Some(r) = self.people_fold {
            props.hits.add(fold, r, MouseCursor::Hand, props.slot);
        }

        // The clips' controls, wherever in the conversation they drew.
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        crate::reader::control_hits(cx, &props, clip);
        self.viewport = Some(clip).filter(|r| r.size.y > 0.0);
        crate::reader::pictures::set_viewport(cx, None);
        DrawStep::done()
    }
}

impl MessagePanel {
    /// A link in a letter goes to the system browser.
    ///
    /// **One** handler, not one per panel: every hosted widget is handed the
    /// same action list, so two open readers would open two browser windows
    /// for one click. The list's group uid is what settles which reader the
    /// link was in — a portal list stamps its items' actions with its own —
    /// and only that one acts.
    fn opened_links(&mut self, cx: &mut Cx, actions: &Actions, scope: &mut Scope) {
        let mine = self.view.widget(cx, ids!(list)).widget_uid();
        for a in actions {
            let Some(wa) = a.as_widget_action() else {
                continue;
            };
            if wa.group.as_ref().map(|g| g.group_uid) != Some(mine) {
                continue;
            }
            if let HtmlLinkAction::Clicked { url, .. } = wa.cast() {
                // Vetted first, as every other reading vets one: a letter
                // is the one place a `javascript:` or a bare fragment
                // really turns up, and handing that to the system browser
                // is at best a failure the person is told about.
                if let Some(url) = crate::reader::html::link_target(&url) {
                    crate::platform::browser::open_or_notify(cx, &url, scope);
                }
            }
        }
    }
}

/// What a script addresses a row by: sender and the line it previews while it
/// is closed, sender and date while it is open — so "this message opened in
/// place" is checked by the run and not by a human reading a screenshot.
fn head_label(t: &Letter, open: bool) -> String {
    let name = writer(&t.mail);
    if open {
        format!("{name} · {}", fmt_date(t.mail.head.date))
    } else {
        format!("{name}: {}", t.preview.0)
    }
}

/// Who wrote a letter, as a row names them: the name, or the address for a
/// sender who gave none.
fn writer(m: &MailFull) -> String {
    if m.head.from_name.is_empty() {
        m.head.from_email.clone()
    } else {
        m.head.from_name.clone()
    }
}

/// The `From` an open letter says in full: the name and the address it
/// arrived with, or the address alone for a sender who gave no name. The row
/// above says the name — this is the header field, which is what a person
/// carries out to a filter or an address book.
fn from_line(m: &MailFull) -> String {
    if m.head.from_name.is_empty() {
        m.head.from_email.clone()
    } else {
        format!("{} <{}>", m.head.from_name, m.head.from_email)
    }
}

/// One message of the conversation. The header is the same row open or
/// closed, so it toggles in place; everything below it belongs to the open
/// state and is emptied when it goes, rather than merely hidden.
fn populate(
    cx: &mut Cx,
    row: &WidgetRef,
    t: &Letter,
    open: bool,
    quoted: bool,
    slot: kernel::layout::SlotId,
    html: &mut (HtmlContent, HtmlContent),
) {
    let atts = &t.attachments;
    let m = &t.mail;
    let (line, err) = (&t.preview.0, t.preview.1);
    row.label(cx, ids!(head.name_lbl)).set_text(cx, &writer(m));
    row.text_input(cx, ids!(body.from_wrap.from_txt))
        .set_text(cx, &from_line(m));
    // Who this one went to, and who was only in copy. The header at the top
    // says who the conversation is with, everyone once; this says whom the
    // letter itself named, as its own header wrote them — one's own address
    // included, because reaching me in copy is a thing to know about a
    // letter. One that names nobody, sent in blind copy alone, loses the
    // row rather than wear a bare TO.
    let to = t.to_line();
    row.text_input(cx, ids!(body.to_wrap.to_txt))
        .set_text(cx, &to);
    row.widget(cx, ids!(body.to_wrap))
        .set_visible(cx, !to.is_empty());
    row.label(cx, ids!(head.date_lbl))
        .set_text(cx, &fmt_date(m.head.date));
    // Passed on: the one mark every other client draws for `$Forwarded`,
    // muted, beside the date.
    row.label(cx, ids!(head.fwd_lbl))
        .set_visible(cx, m.forwarded);
    for (path, on) in [
        (ids!(head.preview_wrap.preview_lbl), !err),
        (ids!(head.preview_wrap.preview_err), err),
    ] {
        let lbl = row.label(cx, path);
        let show = on && !open;
        lbl.set_text(cx, if show { line.as_str() } else { "" });
        lbl.set_visible(cx, show);
    }
    // Open, the preview gives its width to the date at the right edge.
    row.widget(cx, ids!(head.preview_wrap))
        .set_visible(cx, !open);
    row.widget(cx, ids!(head.spacer)).set_visible(cx, open);

    row.widget(cx, ids!(body)).set_visible(cx, open);
    // The status line, where the letter carries one: said in the header while
    // closed, and again under the text while open.
    for (path, on) in [
        (ids!(body.status_lbl), open && !err),
        (ids!(body.status_err_lbl), open && err),
    ] {
        let lbl = row.label(cx, path);
        let show = on && m.status.is_some();
        lbl.set_text(cx, if show { line.as_str() } else { "" });
        lbl.set_visible(cx, show);
    }

    // What the author wrote, and the quoted tail they wrote it over, folded
    // behind one line: in a conversation that tail is the message above.
    //
    // Both readings are written every time — the hidden one emptied rather
    // than merely hidden, so no mail can leave its text behind for the next
    // one to show. A letter's images are filed under its own name, so two
    // open letters cannot answer for each other's parts.
    let is_html = open && m.html.is_some();
    let own_text = if open && !is_html {
        t.own_text.as_str()
    } else {
        ""
    };
    let own_html = if is_html { t.own_html.as_str() } else { "" };
    let quote = if open { t.quote.as_deref() } else { None };
    row.text_input(cx, ids!(body.text_wrap.body_txt))
        .set_text(cx, own_text);
    let body_html = row.html(cx, ids!(body.html_wrap.body_html));
    html.0.set(cx, body_html, own_html);
    row.widget(cx, ids!(body.text_wrap))
        .set_visible(cx, open && !is_html);
    row.widget(cx, ids!(body.html_wrap))
        .set_visible(cx, is_html);

    let show_quote = quote.is_some() && quoted;
    let tail = quote.unwrap_or_default();
    row.widget(cx, ids!(body.quote_fold))
        .set_visible(cx, !tail.is_empty() && !quoted);
    row.text_input(cx, ids!(body.quote_wrap.quote_txt))
        .set_text(cx, if show_quote && !is_html { tail } else { "" });
    let quote_html = row.html(cx, ids!(body.quote_html.quote_body));
    html.1.set(
        cx,
        quote_html,
        if show_quote && is_html { tail } else { "" },
    );
    row.widget(cx, ids!(body.quote_wrap))
        .set_visible(cx, show_quote && !is_html);
    row.widget(cx, ids!(body.quote_html))
        .set_visible(cx, show_quote && is_html);

    // What the letter carries, under its reading: one link a part, each
    // opening the card over it — a solid link, so it opens joined to the
    // right like anything else the panel names.
    let shown: Vec<&Attachment> = if open {
        atts.iter().take(ATT_SLOTS).collect()
    } else {
        Vec::new()
    };
    row.widget(cx, ids!(atts))
        .set_visible(cx, !shown.is_empty());
    for (i, name) in ATT_LINKS.iter().enumerate() {
        let link = row.widget(cx, &[live_id!(atts), *name]).as_slink();
        match shown.get(i) {
            Some(a) => {
                link.set(
                    cx,
                    &a.label(),
                    Nav::Open {
                        from: slot,
                        id: a.panel(),
                        fresh: false,
                    },
                    false,
                    None,
                );
                link.set_visible(cx, true);
            }
            None => link.set_visible(cx, false),
        }
    }
    let rest = atts.len().saturating_sub(ATT_SLOTS);
    let more = row.label(cx, ids!(atts.more_lbl));
    more.set_text(cx, &format!("+{rest} more"));
    more.set_visible(cx, !shown.is_empty() && rest > 0);
}
