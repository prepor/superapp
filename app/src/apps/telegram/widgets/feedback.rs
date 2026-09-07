//! Shared status and recovery controls on every Telegram panel.
use super::super::{
    model,
    operations::{self, Status},
    runtime,
};
use crate::shell::hosted::PanelProps;
use kernel::session::Session;
use makepad_widgets::*;

#[derive(Debug, PartialEq, Eq)]
struct Feedback {
    status: String,
    error: String,
    failure: Option<u64>,
    retryable: bool,
}

fn feedback(
    ops: &[operations::Operation],
    connection: Option<String>,
    describe: impl Fn(&operations::Operation) -> String,
) -> Feedback {
    // History pages and other background work must not insert/remove a row
    // on every panel as each request settles. Chats already show loading for
    // the whole history walk in their header. Keep background failures below.
    let pending: Vec<_> = ops
        .iter()
        .filter(|o| o.status == Status::Pending && o.foreground())
        .collect();
    let failures: Vec<_> = ops
        .iter()
        .filter(|o| matches!(o.status, Status::Failed { .. }))
        .collect();
    let mut status = match pending.last() {
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
    if let Some(connection) = connection {
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
    Feedback {
        status,
        error,
        failure: failure.map(|o| o.id),
        retryable: failure.is_some_and(|o| o.retryable()),
    }
}

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
        let rt = runtime::of(s.store());
        let Feedback {
            status,
            error,
            failure,
            retryable,
        } = feedback(&rt.operations.list(), rt.connection(), |o| {
            let line = o.line();
            o.chat
                .and_then(|chat| model::peer(s.store(), chat))
                .map_or_else(|| line.clone(), |p| format!("{} · {line}", p.name))
        });
        self.failure = failure;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::telegram::{model::Carried, requests};
    use kernel::store::Store;
    use serde_json::{json, Value};

    fn tracked(tracker: &operations::Tracker, request: String) -> Value {
        serde_json::from_str(&tracker.track(&request)).unwrap()
    }

    fn snapshot(tracker: &operations::Tracker, connection: Option<&str>) -> Feedback {
        feedback(
            &tracker.list(),
            connection.map(str::to_string),
            operations::Operation::line,
        )
    }

    #[test]
    fn history_pages_do_not_change_shared_feedback() {
        let store = Store::open(None, &[]).unwrap();
        for connection in [None, Some("connecting to Telegram")] {
            let tracker = operations::Tracker::default();
            let idle = snapshot(&tracker, connection);
            assert_eq!(idle.status, connection.unwrap_or_default());
            // Regular chats and multiple selected topics can load together.
            for from in [0, 300, 200] {
                let pages: Vec<_> = [0, 2, 3]
                    .into_iter()
                    .map(|topic| {
                        tracked(
                            &tracker,
                            requests::get_history_in(7, topic, from, requests::Walk::Fill),
                        )
                    })
                    .collect();
                assert_eq!(snapshot(&tracker, connection), idle);
                for page in pages.iter().rev() {
                    tracker.reply(
                        &store,
                        &json!({"@type": "messages", "messages": [],
                        "@extra": page["@extra"]}),
                    );
                    assert_eq!(snapshot(&tracker, connection), idle);
                }
            }
        }
    }

    #[test]
    fn background_work_preserves_command_progress_and_completion() {
        let store = Store::open(None, &[]).unwrap();
        let tracker = operations::Tracker::default();
        let photo = tracked(
            &tracker,
            requests::send_file(
                7,
                None,
                &Carried {
                    path: "/tmp/photo.png".into(),
                },
                "",
            ),
        );
        tracker.reply(
            &store,
            &json!({"@type": "message", "chat_id": 7, "id": 100,
            "sending_state": {"@type": "messageSendingStatePending"},
            "content": {"file": {"@type": "file", "id": 5, "size": 1000,
                "remote": {"uploaded_size": 0}}}, "@extra": photo["@extra"]}),
        );
        let page = tracked(
            &tracker,
            requests::get_history_in(7, 2, 0, requests::Walk::Fill),
        );
        tracker.file_progress(&json!({"id": 5, "size": 1000,
            "remote": {"uploaded_size": 650}}));
        assert_eq!(snapshot(&tracker, None).status, "sending photo.png… 65%");

        let message = tracked(&tracker, requests::send_message(8, "hello", None));
        assert_eq!(
            snapshot(&tracker, None).status,
            "sending message… · 2 in progress"
        );
        tracker.reply(
            &store,
            &json!({"@type": "message", "chat_id": 8, "id": 200,
            "@extra": message["@extra"]}),
        );
        assert_eq!(snapshot(&tracker, None).status, "sending photo.png… 65%");
        tracker.sent(
            &store,
            &json!({"@type": "updateMessageSendSucceeded",
            "old_message_id": 100, "message": {"chat_id": 7, "id": 101}}),
        );
        let completed = snapshot(&tracker, None);
        assert_eq!(completed.status, "sending message — done");
        tracker.reply(
            &store,
            &json!({"@type": "messages", "messages": [],
            "@extra": page["@extra"]}),
        );
        assert_eq!(snapshot(&tracker, None), completed);
        tracked(&tracker, requests::get_forum_topic(7, 2));
        assert_eq!(snapshot(&tracker, None), completed);
    }

    #[test]
    fn background_history_errors_still_offer_recovery() {
        let store = Store::open(None, &[]).unwrap();
        for topic in [0, 2] {
            let tracker = operations::Tracker::default();
            let page = tracked(
                &tracker,
                requests::get_history_in(7, topic, 0, requests::Walk::Fill),
            );
            let id = page["@extra"]["operation"].as_u64().unwrap();
            let idle = snapshot(&tracker, None);
            tracker.reply(
                &store,
                &json!({"@type": "error", "code": 500,
                "message": "History unavailable", "@extra": page["@extra"]}),
            );
            let failed = snapshot(&tracker, None);
            assert_eq!(failed.status, "");
            assert_eq!(
                failed.error,
                "loading messages failed: History unavailable (500)"
            );
            assert_eq!(failed.failure, Some(id));
            assert!(failed.retryable);

            let retry: Value = serde_json::from_str(&tracker.retry(id).unwrap()).unwrap();
            assert_eq!(snapshot(&tracker, None), idle);
            tracker.reply(
                &store,
                &json!({"@type": "messages", "messages": [],
                "@extra": retry["@extra"]}),
            );
            assert_eq!(snapshot(&tracker, None), idle);
        }
    }
}
