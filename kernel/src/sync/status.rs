//! What the *device sync* panel draws: one line per peer, and this device
//! above them.
//!
//! Nothing here is in the store. The service keeps one snapshot in memory
//! and replaces it whenever a connection, a pass or an error moves; the
//! panel reads it and never asks the network anything itself.

/// One peer, as this device last saw it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeerStatus {
    /// Its endpoint id.
    pub device: String,
    /// What it calls itself, from the roster.
    pub name: String,
    /// Whether a connection is open right now.
    pub connected: bool,
    /// When it was last heard from, in the world's seconds. Zero for a peer
    /// this run has never reached.
    pub last_seen: f64,
    /// Why the last attempt failed, in one line, or empty.
    pub last_error: String,
    /// How many of our ops it has not acknowledged holding. Zero while a
    /// connection is open, because everything that lands goes out on it;
    /// zero too for a peer that is away, because what it holds is only ever
    /// said in a `Have` on a connection there is not.
    pub behind: i64,
}

impl PeerStatus {
    /// The one line the panel draws under a peer's name: connected, when it
    /// was last heard from, or why the last attempt failed.
    #[must_use]
    pub fn line(&self, now: f64) -> String {
        if self.connected {
            return "connected".into();
        }
        if !self.last_error.is_empty() {
            return self.last_error.clone();
        }
        if self.last_seen <= 0.0 {
            return "not reached yet".into();
        }
        format!("seen {}", ago(now - self.last_seen))
    }
}

/// How long ago, in the coarsest unit that still says something.
fn ago(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    match s {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// The whole of what sync is doing, as one snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncStatus {
    /// This device's id.
    pub device: String,
    /// This device's name.
    pub name: String,
    /// Whether the endpoint is bound. A scripted run never binds.
    pub online: bool,
    /// The roster, in the order the panel lists it.
    pub peers: Vec<PeerStatus>,
    /// Ops kept but refused by a constraint, since this process started —
    /// what the problem source reports, once.
    pub refused: u64,
    /// The first such refusal, which is what that problem says.
    pub refusal: String,
    /// The ticket this device is showing while the panel that asked for one
    /// is open. Empty before the endpoint is reachable, which is what the
    /// panel says *connecting…* for.
    pub ticket: String,
    /// The endpoint's own line: why it could not bind, or what the last
    /// *pair with* said. Empty when there is nothing to say.
    pub note: String,
}

impl SyncStatus {
    /// The first eight characters of a device id — enough to tell two
    /// devices apart, and short enough to sit beside a name.
    #[must_use]
    pub fn short(device: &str) -> String {
        device.chars().take(8).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer's line says the one thing that matters most: that it is here,
    /// or why it is not, or how long it has been gone.
    #[test]
    fn a_peer_says_the_nearest_thing_to_now() {
        let now = 10_000.0;
        let connected = PeerStatus {
            connected: true,
            last_error: "cannot reach the device".into(),
            ..PeerStatus::default()
        };
        assert_eq!(connected.line(now), "connected");

        let failing = PeerStatus {
            last_error: "cannot reach the device".into(),
            last_seen: now - 30.0,
            ..PeerStatus::default()
        };
        assert_eq!(failing.line(now), "cannot reach the device");

        let away = PeerStatus {
            last_seen: now - 200.0,
            ..PeerStatus::default()
        };
        assert_eq!(away.line(now), "seen 3 min ago");
        assert_eq!(PeerStatus::default().line(now), "not reached yet");
    }

    #[test]
    fn how_long_ago_reads_in_one_unit() {
        assert_eq!(ago(0.0), "just now");
        assert_eq!(ago(59.0), "just now");
        assert_eq!(ago(60.0), "1 min ago");
        assert_eq!(ago(7200.0), "2 h ago");
        assert_eq!(ago(200_000.0), "2 days ago");
    }
}
