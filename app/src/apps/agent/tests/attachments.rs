use super::*;
use kernel::app::Wake;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;

#[test]
fn a_mail_pdf_reaches_the_models_tool_result_without_a_manual_upload() {
    use crate::apps::mail::{caps::FakeServers, seed, sync};
    use base64::Engine as _;
    static MAIL_BUILD: &[&dyn App] = &[&MAIL, &AGENT];
    let mut s = Session::fake(MAIL_BUILD);
    let bytes = crate::reader::document::test_pdf("Bonjour depuis la piece jointe.");
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let raw = format!("From: sender@example.org\r\nTo: {}\r\nSubject: agent-attachment\r\nMessage-ID: <agent-attachment@example.org>\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nPlease read the attached file.\r\n\
--x\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=letter.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n--x--\r\n", seed::ADDRESS);
    s.world()
        .with_cap::<FakeServers, _>(|server| {
            server.with(seed::ACCOUNT, |server| {
                server.deliver_flagged("INBOX", true, false, &raw);
            })
        })
        .unwrap();
    kernel::runtime::block_on(sync::sync_account(s.world(), seed::ACCOUNT)).unwrap();
    let (mail, part): (i64, i64) = s.store().conn().query_row(
        "SELECT a.message, a.part FROM attachment a JOIN message m ON m.id = a.message WHERE m.subject = 'agent-attachment'",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    plant(
        &s,
        vec![Reply::always(Answer::Call {
            name: "mail.attachment".into(),
            arguments: json!({"mail": mail, "part": part}),
            then: "I can read the PDF.".into(),
        })],
    );
    let chat = send_new(&mut s, "Translate the attached PDF.");
    // The inline scheduler advances only when a test ticks it; production
    // workers follow the immediate wake returned after a tool-call turn.
    s.workers().tick();
    s.settle();
    let run = model::latest_run(s.store(), chat).unwrap();
    assert_eq!(run.status, model::DONE);
    let calls = model::calls(s.store(), run.id);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].status, model::CALL_DONE, "{}", calls[0].said());
    let requests = fake(&s).requests();
    let response = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert!(response.text().contains("Bonjour depuis la piece jointe."));
    assert_eq!(response.tool_call_id.as_deref(), Some("call_1"));
    assert!(s
        .store()
        .conn()
        .query_row("SELECT unread FROM message WHERE id = ?1", [mail], |r| r
            .get::<_, bool>(
            0
        ))
        .unwrap());
}

#[derive(Default)]
struct ReadState {
    ready: AtomicBool,
    polls: AtomicUsize,
    stop: std::sync::Mutex<Option<model::RunId>>,
    changed: tokio::sync::Notify,
}

struct ReadApp;
static READ_APP: ReadApp = ReadApp;
static READ_BUILD: &[&dyn App] = &[&READ_APP, &AGENT];

impl App for ReadApp {
    fn id(&self) -> &'static str {
        "read-test"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        &[]
    }
    fn as_any(&self) -> &dyn StdAny {
        self
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::reading(
                "read-test.file",
                "read",
                json!({
                    "type": "object", "properties": {"id": {"type": "integer"}},
                    "required": ["id"], "additionalProperties": false
                }),
                |_| {
                    Box::new(|world| {
                        Box::pin(async move {
                            let state = world.store().local::<ReadState>();
                            state.polls.fetch_add(1, Ordering::SeqCst);
                            while !state.ready.load(Ordering::SeqCst) {
                                state.changed.notified().await;
                            }
                            let stop = state.stop.lock().unwrap().take();
                            if let Some(run) = stop {
                                world
                                    .store()
                                    .write_async(move |c| {
                                        model::set_run_status_tx(c, run, model::STOPPED, None, 0.0)
                                    })
                                    .await
                                    .unwrap();
                            }
                            Ok(json!({"text": "file contents"}))
                        })
                    })
                },
            ),
            Tool::new("read-test.ask", "ask", json!({}), true, |_, _| {
                Ok(json!({"done": true}))
            })
            .asking(),
        ]
    }
}

fn pending_round(s: &mut Session, order: &[(&'static str, Value)]) -> (ChatId, model::RunId) {
    let chat = send_new(s, "hello");
    let order = order.to_vec();
    let run = s
        .store()
        .write(move |tx| {
            let run = model::new_run_tx(tx, chat, 0.0)?;
            let (turn, _) = model::add_turn_tx(
                tx,
                chat,
                &Turn::new(Message::of(Role::Assistant)).by(run),
                0.0,
            )?;
            for (i, (tool, input)) in order.iter().enumerate() {
                model::add_call_tx(
                    tx,
                    run,
                    turn,
                    &ToolCall {
                        id: format!("call_{i}"),
                        r#type: "function".into(),
                        function: FunctionCall {
                            name: tool.to_string(),
                            arguments: input.to_string(),
                        },
                    },
                    0.0,
                )?;
            }
            model::set_run_status_tx(tx, run, model::WAITING, None, 0.0)?;
            Ok(run)
        })
        .unwrap();
    (chat, run)
}

#[test]
fn background_reads_preserve_call_order_and_the_approval_gate() {
    let mut s = Session::fake(READ_BUILD);
    let (chat, run) = pending_round(
        &mut s,
        &[
            ("read-test.file", json!({"id": 1})),
            ("read-test.ask", json!({})),
            ("read-test.file", json!({"id": 2})),
        ],
    );
    let state = s.store().local::<ReadState>();
    assert_eq!(calls::run_pending_calls(&mut s, chat), 0);
    assert_eq!(
        state.polls.load(Ordering::SeqCst),
        0,
        "UI does not start the read"
    );
    let mut worker = worker::RunWorker::new(run, chat);
    kernel::runtime::block_on(async {
        let mut pass = Box::pin(worker.pass(s.world()));
        assert!(matches!(futures_util::poll!(&mut pass), Poll::Pending));
        assert_eq!(state.polls.load(Ordering::SeqCst), 1);
        assert_eq!(model::calls(s.store(), run)[1].status, model::CALL_PENDING);
        state.ready.store(true, Ordering::SeqCst);
        state.changed.notify_waiters();
        pass.await;
    });
    let before_ask = state.polls.load(Ordering::SeqCst);
    assert_eq!(calls::run_pending_calls(&mut s, chat), 1);
    let rows = model::calls(s.store(), run);
    assert_eq!(rows[0].status, model::CALL_DONE);
    assert_eq!(rows[1].status, model::CALL_ASKED);
    assert_eq!(rows[2].status, model::CALL_PENDING);
    kernel::runtime::block_on(worker.pass(s.world()));
    assert_eq!(
        state.polls.load(Ordering::SeqCst),
        before_ask,
        "read behind an approval remains stopped"
    );
    assert!(calls::refuse(&mut s, chat, rows[1].id));
    s.settle();
    assert_eq!(
        model::latest_run(s.store(), chat).unwrap().status,
        model::DONE
    );
    assert_eq!(model::calls(s.store(), run)[2].status, model::CALL_DONE);
}

#[test]
fn a_stop_during_an_async_read_prevents_the_next_request() {
    let mut s = Session::fake(READ_BUILD);
    let (chat, run) = pending_round(&mut s, &[("read-test.file", json!({"id": 1}))]);
    let state = s.store().local::<ReadState>();
    state.ready.store(true, Ordering::SeqCst);
    *state.stop.lock().unwrap() = Some(run);
    let requests = fake(&s).requests().len();
    let turns = model::turns(s.store(), chat).len();

    assert!(matches!(
        kernel::runtime::block_on(worker::RunWorker::new(run, chat).pass(s.world())),
        Wake::OnKick
    ));
    assert_eq!(state.polls.load(Ordering::SeqCst), 1);
    assert_eq!(model::calls(s.store(), run)[0].status, model::CALL_DONE);
    assert_eq!(
        fake(&s).requests().len(),
        requests,
        "a stop during the read must prevent another billed request"
    );
    assert_eq!(model::run(s.store(), run).unwrap().status, model::STOPPED);
    assert_eq!(
        model::turns(s.store(), chat).len(),
        turns,
        "the stopped run must not append tool or assistant turns"
    );
}

#[test]
fn invalid_read_arguments_and_stopped_runs_never_start_or_resume_io() {
    let mut s = Session::fake(READ_BUILD);
    let (chat, run) = pending_round(&mut s, &[("read-test.file", json!({"id": "wrong"}))]);
    let state = s.store().local::<ReadState>();
    kernel::runtime::block_on(worker::RunWorker::new(run, chat).pass(s.world()));
    assert_eq!(state.polls.load(Ordering::SeqCst), 0);
    assert_eq!(model::calls(s.store(), run)[0].status, model::CALL_FAILED);
    let (chat, run) = pending_round(&mut s, &[("read-test.file", json!({"id": 1}))]);
    let mut worker = worker::RunWorker::new(run, chat);
    kernel::runtime::block_on(async {
        let mut pass = Box::pin(worker.pass(s.world()));
        assert!(matches!(futures_util::poll!(&mut pass), Poll::Pending));
        assert_eq!(state.polls.load(Ordering::SeqCst), 1);
        s.store()
            .write_async(move |c| model::set_run_status_tx(c, run, model::STOPPED, None, 0.0))
            .await
            .unwrap();
        assert!(matches!(pass.await, Wake::OnKick));
    });
    state.ready.store(true, Ordering::SeqCst);
    kernel::runtime::block_on(worker.pass(s.world()));
    assert_eq!(state.polls.load(Ordering::SeqCst), 1);
    assert_eq!(
        model::latest_run(s.store(), chat).unwrap().status,
        model::STOPPED
    );
}
