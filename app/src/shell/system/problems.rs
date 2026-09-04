//! Every standing problem as a row.
//!
//! Problems are not a table: they are derived from rows on every poll, and
//! fixing the source condition removes one. The panel draws whatever the
//! apps' sources list, controls included — a [`Problem`] carries its own
//! verbs as data, so a source this build has never heard of still draws.
//!
//! No chords: a panel with a control per row would have to invent a letter
//! per row, so its verbs are clicked and tabbed, never chorded.

use std::any::Any;

use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, VerbAct};
use kernel::problems::Problem;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::dsl::SLinkWidgetRefExt;
use crate::shell::hosted::PanelProps;

/// The problems panel. It owns nothing: what stands is asked of the session
/// on every draw, which is what makes a fixed condition disappear by
/// itself.
pub struct Problems {
    id: PanelId,
}

impl Problems {
    pub const TAG: Tag = Tag("problems");

    /// The identity of the one problems panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}

impl Panel for Problems {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "problems".into()
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct ProblemsKind;

impl PanelKind for ProblemsKind {
    fn tag(&self) -> Tag {
        Problems::TAG
    }

    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Problems { id: id.clone() })
    }
}

/// How many controls one row draws. A source with more than this says the
/// rest in its detail line; nothing in the build has two.
const SLOTS: usize = 2;
const BTNS: [&[LiveId]; SLOTS] = [ids!(head.b0), ids!(head.b1)];
const BTN_LBLS: [&[LiveId]; SLOTS] = [ids!(head.b0.lbl), ids!(head.b1.lbl)];
const LINKS: [&[LiveId]; SLOTS] = [ids!(foot.l0), ids!(foot.l1)];

/// The widget. Its buttons are answered here, by the rectangles of the last
/// draw, because a portal-list item's own area goes stale the moment a
/// mid-gesture redraw lands; its links are `SLink`s, which carry the
/// navigation they mean and answer for themselves.
#[derive(Script, ScriptHook, Widget)]
pub struct ProblemsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Which button is where, as the last draw left it: the problem's key,
    /// the verb's id, and the rectangle.
    #[rust]
    btns: Vec<(String, &'static str, Rect)>,
}

impl Widget for ProblemsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::MouseDown(e) = event else { return };
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        // Only where nothing was drawn over the button: the hit table
        // settles that, as it does for a human.
        if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
            return;
        }
        let Some((key, id, _)) = self.btns.iter().rev().find(|(_, _, r)| r.contains(e.abs)) else {
            return;
        };
        let (key, id) = (key.clone(), *id);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        // The verbs are pulled again as one fires: the row is a view of
        // what stands, never a copy of it.
        let act = session
            .problems()
            .into_iter()
            .find(|p| p.key == key)
            .and_then(|p| p.verbs.into_iter().find(|v| v.id == id))
            .map(|v| v.act);
        match act {
            // A problem row belongs to no panel, so its buttons carry their
            // own behaviour: there is no instance for a `Run` to reach.
            Some(VerbAct::Call(f)) => f(session),
            Some(VerbAct::Go(nav)) => session.nav(nav),
            Some(VerbAct::Run) | None => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let problems = scope
            .data
            .get_mut::<Session>()
            .map(|s| s.problems())
            .unwrap_or_default();
        self.view
            .label(cx, ids!(none_lbl))
            .set_visible(cx, problems.is_empty());

        self.btns.clear();
        let mut drawn: Vec<(String, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, problems.len());
            while let Some(idx) = list.next_visible_item(cx) {
                let Some(p) = problems.get(idx) else { continue };
                let row = list.item(cx, idx, live_id!(problem_row));
                populate(cx, &row, p);
                row.draw_all(cx, scope);
                drawn.push((p.key.clone(), row));
            }
        }
        // The controls' rectangles, once the rows have landed. The buttons
        // are answered here; a link answers its own press, but the hit it
        // registered while it drew points at the wrong place — the row's
        // `Fill` detail line defers, and the links only reach the right
        // edge after that. Registered again here, where the row has landed:
        // a later hit wins, and a script's press lands where the human's
        // does.
        for (key, row) in drawn {
            let Some(p) = problems.iter().find(|p| p.key == key) else {
                continue;
            };
            for (v, path) in p
                .verbs
                .iter()
                .filter(|v| v.act.button())
                .zip(BTNS)
            {
                let r = row.widget(cx, path).area().rect(cx);
                if r.size.x <= 0.0 {
                    continue;
                }
                props
                    .hits
                    .add(v.label.clone(), r, MouseCursor::Hand, props.slot);
                self.btns.push((key.clone(), v.id, r));
            }
            for (v, path) in p
                .verbs
                .iter()
                .filter(|v| !v.act.button())
                .zip(LINKS)
            {
                let r = row.widget(cx, path).area().rect(cx);
                if r.size.x > 0.0 {
                    props
                        .hits
                        .add(v.label.clone(), r, MouseCursor::Hand, props.slot);
                }
            }
        }
        DrawStep::done()
    }
}

/// One row: what it concerns, what is wrong, the muted line under it, and
/// the controls the source gave it.
fn populate(cx: &mut Cx, row: &WidgetRef, p: &Problem) {
    row.label(cx, ids!(head.label_lbl)).set_text(cx, &p.label);
    row.label(cx, ids!(line_lbl)).set_text(cx, &p.line);
    row.label(cx, ids!(foot.detail_lbl)).set_text(cx, &p.detail);

    let mut buttons = p.verbs.iter().filter(|v| v.act.button());
    for (i, path) in BTNS.into_iter().enumerate() {
        match buttons.next() {
            Some(v) => {
                row.widget(cx, path).set_visible(cx, true);
                row.label(cx, BTN_LBLS[i]).set_text(cx, &v.label);
            }
            None => row.widget(cx, path).set_visible(cx, false),
        }
    }
    let mut links = p.verbs.iter().filter_map(|v| match &v.act {
        VerbAct::Go(nav) => Some((v.label.clone(), nav.clone())),
        VerbAct::Run | VerbAct::Call(_) => None,
    });
    for path in LINKS {
        match links.next() {
            Some((label, nav)) => {
                row.widget(cx, path).set_visible(cx, true);
                row.widget(cx, path)
                    .as_slink()
                    .set(cx, &label, nav, false, None);
            }
            None => row.widget(cx, path).set_visible(cx, false),
        }
    }
}
