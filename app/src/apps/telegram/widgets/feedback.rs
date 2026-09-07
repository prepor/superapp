//! Shared status and recovery controls on every Telegram panel.
use super::super::{
    model,
    operations::{self, Status},
    runtime,
};
use crate::shell::hosted::PanelProps;
use kernel::session::Session;
use makepad_widgets::*;

#[derive(Script, ScriptHook, Widget)]
pub struct TelegramFeedback {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    failure: Option<u64>,
    #[rust]
    retry: Option<Rect>,
    #[rust]
    dismiss: Option<Rect>,
}

impl Widget for TelegramFeedback {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::MouseDown(e) = event else { return };
        let Some(props) = scope.props.get::<PanelProps>() else {
            return;
        };
        if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
            return;
        }
        let Some(id) = self.failure else { return };
        let Some(s) = scope.data.get_mut::<Session>() else {
            return;
        };
        if self.retry.is_some_and(|r| r.contains(e.abs)) {
            operations::retry(s.store(), id);
            s.redraw();
        } else if self.dismiss.is_some_and(|r| r.contains(e.abs)) {
            runtime::of(s.store()).operations.dismiss(id);
            s.redraw();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some(s) = scope.data.get_mut::<Session>() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let ops = runtime::of(s.store()).operations.list();
        let pending: Vec<_> = ops.iter().filter(|o| o.status == Status::Pending).collect();
        let failures: Vec<_> = ops
            .iter()
            .filter(|o| matches!(o.status, Status::Failed { .. }))
            .collect();
        let describe = |o: &operations::Operation| {
            let line = o.line();
            o.chat
                .and_then(|chat| model::peer(s.store(), chat))
                .map_or_else(|| line.clone(), |p| format!("{} · {line}", p.name))
        };
        let mut status = match pending
            .iter()
            .rev()
            .find(|o| o.foreground())
            .copied()
            .or_else(|| pending.last().copied())
        {
            Some(op) => {
                let line = describe(op);
                if pending.len() > 1 {
                    format!("{line} · {} in progress", pending.len())
                } else {
                    line
                }
            }
            None => ops
                .iter()
                .rev()
                .find(|o| o.status == Status::Done && o.foreground())
                .map(&describe)
                .unwrap_or_default(),
        };
        if let Some(connection) = runtime::of(s.store()).connection() {
            status = if status.is_empty() {
                connection
            } else {
                format!("{connection} · {status}")
            };
        }
        let failure = failures.first();
        let error = failure
            .map(|o| {
                let line = describe(o);
                if failures.len() > 1 {
                    format!("{line} · {} more failures", failures.len() - 1)
                } else {
                    line
                }
            })
            .unwrap_or_default();
        self.failure = failure.map(|o| o.id);
        let retryable = failure.is_some_and(|o| o.retryable());
        self.view.label(cx, ids!(status)).set_text(cx, &status);
        self.view
            .label(cx, ids!(status))
            .set_visible(cx, !status.is_empty());
        self.view.label(cx, ids!(error)).set_text(cx, &error);
        self.view
            .label(cx, ids!(error))
            .set_visible(cx, !error.is_empty());
        self.view
            .view(cx, ids!(controls))
            .set_visible(cx, failure.is_some());
        self.view
            .widget(cx, ids!(controls.retry))
            .set_visible(cx, retryable);
        let step = self.view.draw_walk(cx, scope, walk);
        self.retry = None;
        self.dismiss = None;
        for (label, path, visible) in [
            (
                status.as_str(),
                ids!(status) as &[LiveId],
                !status.is_empty(),
            ),
            (error.as_str(), ids!(error), !error.is_empty()),
            ("retry", ids!(controls.retry), retryable),
            ("dismiss", ids!(controls.dismiss), failure.is_some()),
        ] {
            if !visible {
                continue;
            }
            let rect = self.view.widget(cx, path).area().rect(cx);
            props
                .hits
                .add(label.to_string(), rect, MouseCursor::Default, props.slot);
            if label == "retry" {
                self.retry = Some(rect);
            }
            if label == "dismiss" {
                self.dismiss = Some(rect);
            }
        }
        step
    }
}
