//! The session's half of device sync: how the service is mounted, and what
//! the *device sync* panel asks of it.
//!
//! Device sync is not an app — it carries every app's declared tables and
//! the roster the kernel owns — so the session holds it, as it held the
//! lease driver before it. Everything here is a message to one task: the
//! session neither dials nor listens, and never blocks on either.

use super::Session;
use crate::sync::{Mount, Pairing, Service, SyncStatus};

impl Session {
    /// Starts the service on this session's store. Said once at boot, and
    /// only by a run that has an outside: a scripted run that asked for no
    /// endpoint mounts nothing, and the panel says so.
    ///
    /// `notify` wakes the window after the snapshot moves, the way a
    /// worker's commit does.
    pub fn mount_sync(&mut self, mount: Mount, notify: impl Fn() + Send + Sync + 'static) {
        self.sync = Some(Service::spawn(self.store.db(), mount, notify));
    }

    /// Whether this run has a service at all.
    #[must_use]
    pub fn syncing(&self) -> bool {
        self.sync.is_some()
    }

    /// What sync is doing, or `None` on a run with no service.
    #[must_use]
    pub fn sync_status(&self) -> Option<SyncStatus> {
        self.sync.as_ref().map(Service::status)
    }

    /// Whether the snapshot has moved since this was last asked — what the
    /// shell polls, so a connection coming up redraws the panel.
    pub fn poll_sync(&mut self) -> bool {
        self.sync.as_mut().is_some_and(Service::moved)
    }

    /// Pairs with a ticket somebody pasted.
    ///
    /// # Errors
    ///
    /// If the string is not a ticket, is this device's own, or there is no
    /// service to pair with.
    pub fn sync_pair(&self, ticket: &str) -> Result<(), String> {
        match &self.sync {
            Some(service) => service.pair(ticket),
            None => Err("device sync is not running".into()),
        }
    }

    /// Drops a device from the roster: its row says removed, its session
    /// ends, and it is refused from then on.
    pub fn sync_forget(&self, device: &str) {
        if let Some(service) = &self.sync {
            service.forget(device);
        }
    }

    /// Opens a pairing window and hands back the guard that closes it —
    /// which is what the panel holds, because a ticket is worthless once
    /// the panel that showed it is gone.
    #[must_use]
    pub fn sync_begin_pairing(&self) -> Option<Pairing> {
        self.sync.as_ref().map(Service::begin_pairing)
    }

    /// Closes the window now, for a caller that kept no guard.
    pub fn sync_end_pairing(&self) {
        if let Some(service) = &self.sync {
            service.end_pairing();
        }
    }

    /// What this device calls itself, written to its own roster row.
    pub fn sync_rename(&self, name: &str) {
        if let Some(service) = &self.sync {
            service.rename(name);
        }
    }

    /// Tries every peer that has no connection, now: a foreground, a
    /// resume, or a pairing.
    pub fn sync_kick(&self) {
        if let Some(service) = &self.sync {
            service.kick();
        }
    }
}
