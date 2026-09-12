use super::*;
use kernel::app::{Apps, Env, Workers};
use std::{sync::mpsc, time::Duration};

#[test]
fn automatic_coverage_wakes_the_background_worker_by_entity() {
    // Use the production thread mount, with only Calendar's worker and fake
    // capabilities. Inline workers ignore kick addresses and miss this bug.
    static BACKGROUND: &[&dyn App] = &[&CALENDAR];
    let initial = paused_session();
    let env = Env {
        clock: kernel::caps::ClockSource::virtual_from(initial.now()),
        ..Env::default()
    };
    let (sent, passed) = mpsc::channel();
    let workers = Workers::async_io(
        BACKGROUND,
        initial.store().clone(),
        Mode::Fake,
        env,
        move || {
            let _ = sent.send(());
        },
    );
    let mut s = Session::new(
        Apps::new(APPS),
        initial.world().clone(),
        workers,
    );
    assert!(!s.workers().is_inline());
    s.workers().kick_all();
    // Discovery is asynchronous too; a kick does not publish the worker set
    // synchronously. Let startup notifications settle before this request.
    while s.workers().names().is_empty() {
        passed
            .recv_timeout(Duration::from_secs(2))
            .expect("worker discovery");
    }
    assert_eq!(s.workers().names(), ["calendar-sync"]);
    while passed.recv_timeout(Duration::from_millis(25)).is_ok() {}
    let before = model::coverage(s.store()).unwrap();
    assert!(!before.pending());
    let head = s.history().head();
    let end = before.end + 366.0 * 86400.0;
    assert!(model::cover(&mut s, before.start, end));
    assert_eq!(
        s.history().head(),
        head,
        "coverage is not an undoable action"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let after = loop {
        passed.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("date coverage must wake Calendar without waiting for its 60-second poll");
        s.store().poll_external();
        let after = model::coverage(s.store()).unwrap();
        if after.end >= end && after.completed == after.requested { break after; }
    };
    assert_eq!(after.requested, before.requested + 1);
    assert_eq!(after.completed, after.requested);
    assert!(after.end >= end);
    assert!(after.error.is_empty(), "{}", after.error);
    s.shutdown();
}
