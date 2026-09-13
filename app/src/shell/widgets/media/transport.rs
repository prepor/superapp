//! The transport: the wish to run or hold, what the platform last said, and
//! the rule that one thing plays at a time.
//!
//! A verb has no `Cx` to reach a player through, so a host keeps the wish
//! here and the draw is where it is made so (see [`Clip`](super::Clip)).
//! What the platform reports comes back the same way, so a card's verbs
//! and its strip read one state. A host with no player at all — a demo
//! line, a voice note this build cannot decode — runs a [`Timeline`]
//! instead: a clock-driven stand-in the same strip draws.
//!
//! Who plays now is kept per store, weakly: a store is one screen's worth
//! of panels, a closed panel never keeps playing, and two sessions in one
//! process — the tests — never pause each other.

use std::sync::{Arc, Mutex, Weak};

use kernel::store::Store;

use super::PlayerState;

/// The transport that last took the slot, if it still exists.
#[derive(Default)]
pub struct Registry(Mutex<Weak<Mutex<Inner>>>);

#[derive(Default)]
struct Inner {
    /// The wish: run or hold.
    running: bool,
    /// What the platform last said of the clip this transports.
    native: PlayerState,
    /// The clock-driven stand-in, where the host has no player.
    timeline: Option<Timeline>,
}

impl Inner {
    fn pause(&mut self, now: f64) {
        self.running = false;
        if let Some(t) = self.timeline.as_mut().filter(|t| t.state(now).playing) {
            t.toggle(now);
        }
    }
}

/// Where one thing a host can play stands, as the host and the platform
/// agree. A host keeps one per thing it can play.
pub struct Transport {
    inner: Arc<Mutex<Inner>>,
    /// The store's registry, where playing takes the slot from whatever
    /// played before; `None` for a transport outside the rule — a muted
    /// clip that plays on sight is a moving picture, not a sound: it pauses
    /// nothing and nothing pauses it.
    registry: Option<Arc<Registry>>,
}

impl Transport {
    /// A transport at rest, over the store's registry.
    #[must_use]
    pub fn new(store: &Store) -> Transport {
        Transport { inner: Arc::default(), registry: Some(store.local()) }
    }

    /// A transport that stands outside the one-at-a-time rule.
    #[must_use]
    pub fn animation() -> Transport {
        Transport { inner: Arc::default(), registry: None }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("playback transport")
    }

    /// Whether this transport holds the slot — or stands outside the rule.
    #[must_use]
    pub fn owns(&self) -> bool {
        let Some(registry) = &self.registry else {
            return true;
        };
        registry.0.lock().expect("playback registry").upgrade()
            .is_some_and(|active| Arc::ptr_eq(&active, &self.inner))
    }

    /// Takes the slot: whatever played before pauses now.
    fn take(&self, now: f64) {
        let Some(registry) = &self.registry else {
            return;
        };
        let mut active = registry.0.lock().expect("playback registry");
        if let Some(previous) = active.upgrade().filter(|p| !Arc::ptr_eq(p, &self.inner)) {
            previous.lock().expect("playback transport").pause(now);
        }
        *active = Arc::downgrade(&self.inner);
    }

    /// Runs a clock-driven timeline over `length` seconds instead of a
    /// player, from where it stands if there is one already.
    pub fn run_timeline(&mut self, length: f64) {
        let mut inner = self.lock();
        if inner.timeline.is_none() {
            inner.timeline = Some(Timeline::over(length));
        }
    }

    /// Whether the transport runs a timeline rather than a player.
    #[must_use]
    pub fn on_timeline(&self) -> bool {
        self.lock().timeline.is_some()
    }

    /// Play: takes the slot and sets the wish, or starts the timeline.
    pub fn play(&mut self, now: f64) {
        self.take(now);
        let mut inner = self.lock();
        match inner.timeline.as_mut() {
            Some(t) => {
                if !t.state(now).playing {
                    t.toggle(now);
                }
            }
            None => inner.running = true,
        }
    }

    /// Pause: the wish falls and the timeline stops where it is.
    pub fn pause(&mut self, now: f64) {
        self.lock().pause(now);
    }

    /// Play or pause.
    pub fn toggle(&mut self, now: f64) {
        if self.playing(now) {
            self.pause(now);
        } else {
            self.play(now);
        }
    }

    /// Whether anything runs — what asks for the next frame.
    #[must_use]
    pub fn playing(&self, now: f64) -> bool {
        let inner = self.lock();
        inner.running || inner.timeline.is_some_and(|t| t.state(now).playing)
    }

    /// The wish the draw carries out over the clip's player.
    #[must_use]
    pub fn running(&self) -> bool {
        self.lock().running
    }

    /// What the draw found the player at afterwards — `false` where the
    /// clip has run to its end, which is what puts the button back to
    /// *play*. A late report cannot reclaim another transport's turn.
    pub fn set_running(&mut self, running: bool) {
        let owns = self.owns();
        self.lock().running = running && owns;
    }

    /// Keeps the platform's position for the strip and the verbs between
    /// draws.
    pub fn set_native(&mut self, state: PlayerState) {
        self.lock().native = state;
        self.set_running(state.playing);
    }

    /// Moves the timeline; a player is sought through its clip, which needs
    /// a `Cx`.
    pub fn seek_timeline(&mut self, position: f64, now: f64) {
        if let Some(t) = self.lock().timeline.as_mut() {
            t.seek(position, now);
        }
    }

    /// Where it stands: the timeline's word where there is one, else the
    /// wish over what the platform last said — its length, or `length`
    /// until it has reported one.
    #[must_use]
    pub fn state(&self, now: f64, length: f64) -> PlayerState {
        let inner = self.lock();
        if let Some(t) = inner.timeline {
            return t.state(now);
        }
        PlayerState {
            playing: inner.running,
            position: inner.native.position,
            length: if inner.native.length > 0.0 { inner.native.length } else { length },
        }
    }
}

/// A timeline over a recording, ticked against the clock: where it stands
/// is what it was at when it last started plus the time since.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timeline {
    pub length: f64,
    /// Where it stood when it last started, or was paused.
    pub offset: f64,
    /// When it last started, while it runs.
    pub started: Option<f64>,
}

impl Timeline {
    /// A timeline at the start of a recording, not running.
    #[must_use]
    pub fn over(length: f64) -> Timeline {
        Timeline { length, offset: 0.0, started: None }
    }

    /// Where it stands now.
    #[must_use]
    pub fn state(&self, now: f64) -> PlayerState {
        let position = match self.started {
            Some(t) => (self.offset + (now - t)).min(self.length),
            None => self.offset,
        };
        PlayerState {
            playing: self.started.is_some() && position < self.length,
            position,
            length: self.length,
        }
    }

    /// Play or pause. Played again at its end, it starts over.
    pub fn toggle(&mut self, now: f64) {
        let st = self.state(now);
        if st.playing {
            self.offset = st.position;
            self.started = None;
        } else {
            self.offset = if st.position >= self.length { 0.0 } else { st.position };
            self.started = Some(now);
        }
    }

    /// Move along the timeline, preserving whether it is currently running.
    pub fn seek(&mut self, position: f64, now: f64) {
        if !position.is_finite() {
            return;
        }
        let playing = self.state(now).playing;
        self.offset = position.clamp(0.0, self.length.max(0.0));
        self.started = playing.then_some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> Store {
        Store::open(None, &[], kernel::sync::Device::fake()).unwrap()
    }

    #[test]
    fn a_timeline_runs_against_the_clock_and_starts_over_at_its_end() {
        let mut p = Timeline::over(42.0);
        assert_eq!(p.state(100.0).position, 0.0);
        assert!(!p.state(100.0).playing);
        p.toggle(100.0);
        assert!(p.state(117.0).playing);
        assert_eq!(p.state(117.0).position, 17.0);
        p.toggle(117.0);
        assert!(!p.state(130.0).playing);
        assert_eq!(p.state(130.0).position, 17.0);
        p.toggle(130.0);
        let end = p.state(200.0);
        assert!(!end.playing && end.position == 42.0, "ran out");
        p.toggle(200.0);
        assert_eq!(p.state(201.0).position, 1.0, "played again, from the start");
    }

    #[test]
    fn one_transport_plays_at_a_time_per_store() {
        let store = open_store();
        let mut a = Transport::new(&store);
        let mut b = Transport::new(&store);
        a.play(0.0);
        assert!(a.running() && a.owns());
        b.play(1.0);
        assert!(!a.running(), "the one playing before pauses");
        assert!(b.running() && b.owns() && !a.owns());
        // A late report from the platform cannot give a the slot back.
        a.set_native(PlayerState { playing: true, position: 3.5, length: 14.0 });
        assert!(!a.running());
        assert_eq!(a.state(0.0, 0.0).position, 3.5, "but its position is kept");
        // Another store is another screen.
        let other = open_store();
        let mut c = Transport::new(&other);
        c.play(2.0);
        assert!(b.running());
        // A moving picture without a sound takes nothing from anyone.
        let mut anim = Transport::animation();
        anim.play(3.0);
        assert!(b.running() && anim.running() && anim.owns());
    }

    #[test]
    fn a_timeline_pauses_when_another_takes_the_slot() {
        let store = open_store();
        let mut voice = Transport::new(&store);
        voice.run_timeline(30.0);
        voice.toggle(10.0);
        assert!(voice.playing(12.0));
        assert_eq!(voice.state(12.0, 0.0).position, 2.0);
        let mut clip = Transport::new(&store);
        clip.play(15.0);
        assert!(!voice.playing(16.0));
        assert_eq!(voice.state(16.0, 0.0).position, 5.0, "paused where the other started");
        voice.seek_timeline(9.0, 17.0);
        assert_eq!(voice.state(18.0, 0.0).position, 9.0);
        clip.set_running(false);
        assert!(!clip.playing(18.0));
    }
}
