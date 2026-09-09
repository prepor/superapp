//! A history transition exclusively owns data claims while it runs. Commands
//! arriving behind it queue on the UI; drawing, scrolling and input continue.

use super::*;
use tokio::sync::oneshot;

pub(super) type Command = Box<dyn FnOnce(&mut Session)>;

pub(super) struct PendingWalk {
    pub view: history::View,
    reply: oneshot::Receiver<(History, Result<Option<history::Step>, String>)>,
}


#[derive(Clone, Copy)]
pub(super) enum Direction { Undo, Redo, Travel(NodeId) }

impl Direction {
    fn run(self, tree: &mut History, world: &World) -> Option<history::Step> {
        match self {
            Self::Undo => tree.undo(world),
            Self::Redo => tree.redo(world),
            Self::Travel(node) => tree.travel(world, node),
        }
    }
}

impl Session {
    /// A data reversal is running; external commands must wait until it returns.
    pub fn history_busy(&self) -> bool { self.walk.is_some() }

    pub(super) fn walk_pending(&self) -> bool { self.walk.is_some() }

    pub(crate) fn defer_navigation(&mut self, navigation: Nav) -> bool {
        if !self.walk_pending() { return false; }
        self.commands.push_back(Box::new(move |session| session.nav(navigation)));
        true
    }

    /// Resume an accepted command after an in-flight history transition.
    pub fn after_history(&mut self, complete: impl FnOnce(&mut Session) + 'static) {
        if self.walk_pending() { self.commands.push_back(Box::new(complete)); }
        else { complete(self); }
    }

    /// Apply in-memory UI completion after the current panel borrow ends.
    pub fn after_event(&mut self, complete: impl FnOnce(&mut Session) + 'static) {
        self.events.push_back(Box::new(complete));
        self.redraw();
    }

    pub(super) fn poll_events(&mut self) {
        while let Some(complete) = self.events.pop_front() { complete(self); }
    }

    pub fn claim_ui(&mut self, claim: Box<dyn UiIntent>) {
        if self.walk_pending() {
            self.commands.push_back(Box::new(move |session| session.claim_ui(claim)));
            return;
        }
        self.ui_claims.entry(self.history.head()).or_default().push(claim);
        self.trim_ui_claims();
    }

    pub(crate) fn attach_claims(&mut self, node: NodeId, claims: Vec<Box<dyn Intent>>) {
        self.history.claim_at(node, claims);
    }

    pub(super) fn trim_ui_claims(&mut self) {
        let ids: HashSet<_> = self.history.rows().0.into_iter().map(|row| row.id).collect();
        self.ui_claims.retain(|id, _| ids.contains(id));
    }

    pub(super) fn begin_walk(&mut self, direction: Direction) -> bool {
        if !self.walk_pending() {
            for panel in self.instances.values() { panel.borrow_mut().flush(); }
            self.poll_apps();
        }
        if self.walk_pending() || !self.edits.is_empty() || !self.preparations.is_empty() {
            self.commands.push_back(Box::new(move |session| { session.begin_walk(direction); }));
            return true;
        }
        let view = self.history.view();
        match direction {
            Direction::Undo if !view.can_undo() => return false,
            Direction::Redo if !view.can_redo() => return false,
            Direction::Travel(node) if node == view.head() => return false,
            _ => {}
        }
        let Some(factory) = self.world.factory().filter(|_| self.store.ui_attached()) else {
            let step = direction.run(&mut self.history, &self.world);
            return self.walked(step);
        };
        let mut tree = std::mem::take(&mut self.history);
        let wake = self.store.ui_waker();
        let (send, reply) = oneshot::channel();
        self.walk = Some(PendingWalk { view, reply });
        let db = self.store.db();
        let apps = self.apps.list();
        crate::runtime::spawn(async move {
            for app in apps { app.flush(db.clone()).await; }
            if let Err(error) = db.raw_async(|_| Ok(())).await {
                let _ = send.send((tree, Err(error.to_string())));
                if let Some(wake) = wake { wake(); }
                return;
            }
            let (tree, result) = crate::runtime::spawn_blocking(move || {
            let result = factory.build().map_err(|error| error.to_string()).and_then(|world| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| direction.run(&mut tree, &world)))
                    .map_err(|_| {
                        tree.expire_current();
                        "the undo operation panicked; its current claim has expired".to_string()
                    })
            });
            (tree, result)
            }).await.expect("history catches native operation panics");
            let _ = send.send((tree, result));
            if let Some(wake) = wake { wake(); }
        });
        true
    }

    pub(super) fn poll_walk(&mut self) {
        let Some(walk) = self.walk.as_mut() else { return; };
        let result = match walk.reply.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => {
                self.walk = None;
                self.notify("the undo service stopped before returning its history", true);
                return;
            }
        };
        self.walk = None;
        self.history = result.0;
        match result.1 {
            Ok(step) => { self.walked(step); }
            Err(error) => self.notify(error, true),
        }
    }

    pub(super) fn poll_commands(&mut self) {
        while !self.walk_pending() && self.edits.is_empty() && self.preparations.is_empty() {
            let Some(command) = self.commands.pop_front() else { break; };
            command(self);
        }
    }

    pub(super) fn flush_edits(&mut self) {
        while !self.edits.is_empty() || self.walk_pending() || !self.commands.is_empty() {
            if !self.preparations.is_empty() && self.edits.is_empty() && !self.walk_pending() { break; }
            // Writer FIFO is the barrier for already accepted edits. A walk
            // may itself submit several writes, so join it before proceeding.
            if let Some(walk) = self.walk.take() {
                match crate::runtime::block_on(walk.reply) {
                    Ok((tree, result)) => {
                        self.history = tree;
                        match result {
                            Ok(step) => { self.walked(step); }
                            Err(error) => self.notify(error, true),
                        }
                    }
                    Err(error) => self.notify(format!("undo shutdown: {error}"), true),
                }
            }
            if let Err(error) = crate::runtime::block_on(self.store.flush_async()) {
                self.notify(format!("could not flush edits: {error}"), true);
                break;
            }
            self.poll_edits();
            self.poll_commands();
            if !self.preparations.is_empty() { break; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct Slow {
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        reversed: Arc<AtomicBool>,
    }
    impl Intent for Slow {
        fn describe(&self) -> String { "slow native reversal".into() }
        fn reverse(&self, _: &World) -> Result<(), String> {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
            self.reversed.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn reapply(&self, _: &World) -> Result<(), String> { Ok(()) }
    }

    #[test]
    fn a_native_reversal_returns_to_ui_and_orders_navigation_and_edits_after_it() {
        let store = Store::open(None, &[]).unwrap();
        let world = Rc::new(crate::app::world_for(&[], store, Mode::Deny, &Env::default()));
        let workers = Workers::inline(&[], world.clone());
        let mut session = Session::new(Apps::new(&[]), world, workers, Mode::Deny);
        let (entered, started) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let reversed = Arc::new(AtomicBool::new(false));
        session.act(Action::new("slow", "native reversal").claiming(vec![Box::new(Slow {
            entered, release: Mutex::new(held), reversed: reversed.clone(),
        })]));
        let head = session.history().head();
        let (wake, mut woke) = tokio::sync::mpsc::unbounded_channel();
        session.store.attach_ui(move || { let _ = wake.send(()); });

        assert!(session.undo());
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(session.history_busy());
        assert_eq!(session.history().head(), head, "the overlay retains its last snapshot");
        session.nav(Nav::Open { from: 0, id: PanelId::bare(panel::Tag("after-undo")), fresh: true });
        let completed = Rc::new(std::cell::Cell::new(false));
        let result = completed.clone();
        session.act_async(Edit::writing("after", "queued edit", |_| Ok(())), move |_, value| {
            result.set(value.is_some());
        });
        assert!(!completed.get());
        assert!(!reversed.load(Ordering::SeqCst));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !completed.get() {
            assert!(std::time::Instant::now() < deadline);
            crate::runtime::block_on(async {
                tokio::time::timeout(Duration::from_secs(5), woke.recv()).await.unwrap();
            });
            session.settle();
        }
        assert!(reversed.load(Ordering::SeqCst));
        let (rows, _) = session.history().rows();
        assert_eq!(rows[0].state, "undone");
        assert_eq!(rows[1].parent, 0, "navigation branches after the reversal");
        assert_eq!(rows[2].parent, rows[1].id, "the edit follows navigation");
        assert_eq!(session.showing(&PanelId::bare(panel::Tag("after-undo"))).len(), 1);
        session.shutdown();
    }
}
