//! The reader: one conversation, as rows that open in place.
//!
//! A closed row is one line — who wrote, the first line they wrote, the date
//! — and a press on it unfolds the letter under the same header. What is
//! open is the instance's, so the panel asks for as many rows as its
//! conversation reads as, and unfolding one asks the layout again.
//!
//! Presses are answered here, by the row rectangles of the last draw: items
//! of a portal list are rebuilt every draw, and a synthesized press has to
//! land the way a finger does.

use kernel::session::Session;
use kernel::time::fmt_date;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::model::{self, MailFull, MailId, ThreadMail};
use super::super::panels::Message;

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
}

impl Widget for MessagePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::MouseDown(e) = event else { return };
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
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
        // The borrow ends before anything is asked of the session: a
        // relayout walks every instance, this one included.
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
        let Some((msgs, open, quoted)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Message>().map(|m| {
                let msgs = m.thread();
                let open: Vec<bool> = msgs.iter().map(|t| m.is_open(t.mail.head.id)).collect();
                let quoted: Vec<bool> = msgs.iter().map(|t| m.quoted(t.mail.head.id)).collect();
                (msgs, open, quoted)
            })
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };

        // The account this conversation came to, said once at the top.
        self.view.label(cx, ids!(to_lbl)).set_text(
            cx,
            msgs.first().map_or("", |t| t.mail.to.as_str()),
        );

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
                populate(cx, &row, t, open[idx], quoted[idx]);
                row.draw_all(cx, scope);
                drawn.push((idx, row));
            }
        }

        // The hits, once the rows have landed: the header a press toggles,
        // and — while a letter is open — its text as a selectable run and
        // the fold over its quote.
        self.rows.clear();
        for (idx, row) in drawn {
            let Some(t) = msgs.get(idx) else { continue };
            let (mail, is_open) = (t.mail.head.id, open[idx]);
            let rect = |path: &[LiveId]| {
                let r = row.widget(cx, path).area().rect(cx);
                (r.size.x > 0.0 && r.size.y > 0.0).then_some(r)
            };
            let Some(head) = rect(ids!(head)) else { continue };
            props
                .hits
                .add(head_label(t, is_open), head, MouseCursor::Hand, props.slot);
            let quote = if is_open && row.widget(cx, ids!(body.quote_fold)).visible() {
                rect(ids!(body.quote_fold))
            } else {
                None
            };
            if let Some(q) = quote {
                props.hits.add("› quoted", q, MouseCursor::Hand, props.slot);
            }
            if is_open {
                if let Some(r) = rect(ids!(body.text_wrap.body_txt)) {
                    props.hits.add("mail body", r, MouseCursor::Text, props.slot);
                }
            }
            self.rows.push(RowHit { mail, head, quote });
        }
        DrawStep::done()
    }
}

/// What a script addresses a row by: sender and the line it previews while
/// it is closed, sender and date while it is open — so "this message opened
/// in place" is checked by the run and not by a human reading a screenshot.
fn head_label(t: &ThreadMail, open: bool) -> String {
    let name = writer(&t.mail);
    if open {
        format!("{name} · {}", fmt_date(t.mail.head.date))
    } else {
        format!("{name}: {}", preview(&t.mail).0)
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

/// The line a closed row shows: the status where there is one — that is what
/// the letter is about — else the first line its author wrote. `true` marks
/// it as an error, which is the one thing colour is spent on.
fn preview(m: &MailFull) -> (String, bool) {
    match &m.status {
        Some((s, err)) => (s.clone(), *err),
        None => (
            model::own_text(m)
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .to_string(),
            false,
        ),
    }
}

/// One message of the conversation. The header is the same row open or
/// closed, so it toggles in place; everything below it belongs to the open
/// state and is emptied when it goes, rather than merely hidden.
fn populate(cx: &mut Cx, row: &WidgetRef, t: &ThreadMail, open: bool, quoted: bool) {
    let m = &t.mail;
    let (line, err) = preview(m);
    row.label(cx, ids!(head.name_lbl)).set_text(cx, &writer(m));
    row.label(cx, ids!(head.date_lbl))
        .set_text(cx, &fmt_date(m.head.date));
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
    // The status line, where the letter carries one: said in the header
    // while closed, and again under the text while open.
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
    let (own, quote) = if open {
        model::split_quote(&m.body)
    } else {
        (String::new(), None)
    };
    row.text_input(cx, ids!(body.text_wrap.body_txt))
        .set_text(cx, &own);
    let show_quote = quote.is_some() && quoted;
    let tail = quote.unwrap_or_default();
    row.widget(cx, ids!(body.quote_fold))
        .set_visible(cx, !tail.is_empty() && !quoted);
    row.text_input(cx, ids!(body.quote_wrap.quote_txt))
        .set_text(cx, if show_quote { tail.as_str() } else { "" });
    row.widget(cx, ids!(body.quote_wrap))
        .set_visible(cx, show_quote);
}
