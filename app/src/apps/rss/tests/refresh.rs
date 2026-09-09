use super::{session, sync, APPS, RSS};
use kernel::app::{App, Env, Wake};
use kernel::session::Session;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

struct RecordingFetch(Rc<RefCell<Vec<i64>>>);

#[async_trait::async_trait(?Send)]

impl sync::Fetch for RecordingFetch {
    async fn get(&mut self, request: &sync::Request) -> Result<sync::Response, String> {
        self.0.borrow_mut().push(request.id);
        if request.id == 100 {
            return Err("feed unavailable".into());
        }
        Ok(sync::Response::Unchanged)
    }
}

fn record(s: &Session) -> Rc<RefCell<Vec<i64>>> {
    let calls = Rc::new(RefCell::new(Vec::new()));
    s.world().caps(|caps| {
        caps.insert::<dyn sync::Fetch>(Box::new(RecordingFetch(calls.clone())));
    });
    calls
}

#[test]
fn hundreds_of_feeds_share_one_worker_and_each_refreshes_once() {
    let s = session();
    s.store()
        .write(|c| {
            for id in 100..612 {
                c.execute(
                    "INSERT INTO rss_feed(id,url,title) VALUES(?1,?2,'Imported')",
                    rusqlite::params![id, format!("https://example.com/{id}")],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let calls = record(&s);
    let mut workers = RSS.workers(s.store());
    assert_eq!(
        workers.len(),
        1,
        "each worker owns a thread and database reader"
    );
    let names: Vec<_> = workers.iter().map(|worker| worker.name()).collect();

    // Run more rounds than needed: waiting workers must not fetch again.
    for _ in 0..514 {
        for worker in &mut workers {
            kernel::runtime::block_on(worker.pass(s.world()));
        }
    }
    let mut fetched = calls.borrow().clone();
    fetched.sort_unstable();
    assert_eq!(fetched, (100..612).collect::<Vec<_>>());
    assert_eq!(
        s.store()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM rss_feed WHERE requested != completed",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        s.store()
            .conn()
            .query_row("SELECT error FROM rss_feed WHERE id=100", [], |r| r
                .get::<_, String>(0),)
            .unwrap(),
        "feed unavailable"
    );

    // The retained worker discovers new subscriptions and skips removed ones.
    calls.borrow_mut().clear();
    s.store()
        .write(|c| {
            c.execute_batch(
                "INSERT INTO rss_feed(id,url,title) VALUES(1000,'https://example.com/new','New');
         UPDATE rss_feed SET subscribed=0,requested=requested+1 WHERE id=100;",
            )
        })
        .unwrap();
    assert_eq!(
        RSS.workers(s.store())
            .iter()
            .map(|worker| worker.name())
            .collect::<Vec<_>>(),
        names
    );
    for worker in &mut workers {
        kernel::runtime::block_on(worker.pass(s.world()));
    }
    assert_eq!(*calls.borrow(), [1000]);
}

#[test]
fn manual_refresh_precedes_sleeping_feeds_and_periodic_deadlines_are_preserved() {
    let env = Env::default();
    let s = Session::fake_with(APPS, &env);
    let now = s.now();
    s.store().write(move |c| {
        // The worker must find this request past the up-to-date feeds.
        c.execute(
            "INSERT INTO rss_feed(id,url,title,checked,completed) VALUES(5,'https://example.com/five','Five',?1,0)",
            [now],
        )?;
        Ok(())
    }).unwrap();
    let calls = record(&s);
    let mut workers = RSS.workers(s.store());
    for worker in &mut workers {
        assert_eq!(
            kernel::runtime::block_on(worker.pass(s.world())),
            Wake::After(Duration::from_secs(900))
        );
    }
    assert!(calls.borrow().is_empty());

    env.clock.advance(30.0);
    s.store()
        .write(|c| c.execute("UPDATE rss_feed SET requested=requested+1 WHERE id=5", []))
        .unwrap();
    for worker in &mut workers {
        kernel::runtime::block_on(worker.pass(s.world()));
    }
    assert_eq!(*calls.borrow(), [5]);

    env.clock.advance(870.0);
    for _ in 0..2 {
        for worker in &mut workers {
            assert_eq!(
                kernel::runtime::block_on(worker.pass(s.world())),
                Wake::After(Duration::ZERO)
            );
        }
    }
    assert_eq!(*calls.borrow(), [5, 1, 2]);
    let wait = workers
        .iter_mut()
        .filter_map(
            |worker| match kernel::runtime::block_on(worker.pass(s.world())) {
                Wake::After(wait) => Some(wait),
                Wake::OnKick => None,
            },
        )
        .min();
    assert_eq!(wait, Some(Duration::from_secs(30)));

    env.clock.advance(30.0);
    for worker in &mut workers {
        kernel::runtime::block_on(worker.pass(s.world()));
    }
    assert_eq!(*calls.borrow(), [5, 1, 2, 5]);
}

#[test]
fn an_empty_worker_waits_and_resumes_without_being_replaced() {
    let s = session();
    let calls = record(&s);
    let mut workers = RSS.workers(s.store());
    s.store()
        .write(|c| c.execute("UPDATE rss_feed SET subscribed=0", []))
        .unwrap();
    let empty = RSS.workers(s.store());
    assert_eq!(empty.len(), 1);
    assert_eq!(empty[0].name(), workers[0].name());
    assert_eq!(
        kernel::runtime::block_on(workers[0].pass(s.world())),
        Wake::OnKick
    );
    assert!(calls.borrow().is_empty());
    s.store()
        .write(|c| {
            c.execute(
                "UPDATE rss_feed SET subscribed=1,requested=requested+1 WHERE id=1",
                [],
            )
        })
        .unwrap();
    kernel::runtime::block_on(workers[0].pass(s.world()));
    assert_eq!(*calls.borrow(), [1]);
}

#[test]
fn a_failed_store_write_does_not_spin_on_the_same_feed() {
    let s = session();
    s.store()
        .write(|c| c.execute("UPDATE rss_feed SET requested=requested+1 WHERE id=1", []))
        .unwrap();
    let calls = record(&s);
    let mut workers = RSS.workers(s.store());
    s.store().db().set_writable(false);
    for worker in &mut workers {
        assert!(
            matches!(kernel::runtime::block_on(worker.pass(s.world())), Wake::After(wait) if wait >= Duration::from_secs(1))
        );
    }
    assert_eq!(*calls.borrow(), [1]);
}
