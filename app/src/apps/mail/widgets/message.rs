//! The reader: one conversation, as rows that open in place.
//!
//! A closed row is one line — who wrote, the first line they wrote, the date
//! — and a press on it unfolds the letter under the same header. What is
//! open is the instance's, so the panel asks for as many rows as its
//! conversation reads as, and unfolding one asks the layout again.
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
use super::super::model::{MailFull, MailId};
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
        let Some((mail, folded)) = self.rows.iter().rev().find_map(|r| {
            if r.head.contains(e.abs) {
                Some((r.mail, false))
            } else if r.quote.is_some_and(|q| q.contains(e.abs)) {
                Some((r.mail, true))
            } else {
                None
            }
        }) else {
            return;
        };
        // The borrow ends before anything is asked of the session: a relayout
        // walks every instance, this one included.
        let toggled = {
            let mut borrow = props.panel.borrow_mut();
            let Some(m) = borrow.as_any().downcast_mut::<Message>() else {
                return;
            };
            if folded {
                m.toggle_quote(mail);
            } else {
                m.toggle(mail);
            }
            !folded
        };
        self.view.redraw(cx);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        if toggled {
            // The wish changed with what is open, so the panel asks for the
            // rows it now reads as.
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
        let Some((reading, open, quoted)) = ({
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
                (reading, open, quoted)
            })
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let msgs = &reading.letters;
        let present: HashSet<_> = msgs.iter().map(|letter| letter.mail.head.id).collect();
        self.html.retain(|mail, _| present.contains(mail));

        // Who the conversation was addressed to, off its first letter, said
        // once at the top: the account, for one that came in — the person it
        // went to, for one this mailbox started.
        self.view
            .label(cx, ids!(to_lbl))
            .set_text(cx, msgs.first().map_or("", |t| t.mail.to.as_str()));

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
                crate::platform::browser::open_or_notify(cx, &url, scope);
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
