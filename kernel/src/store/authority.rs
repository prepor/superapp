//! Local execution authority, separate from the remote ownership record.
//!
//! Closing admission and accepting a transaction/service use the same mutex.
//! A revoked generation never becomes valid again. Services retain an Activity
//! until their descendants and native resources have actually stopped.

use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityState {
    pub generation: u64,
    pub writable: bool,
}

struct Inner {
    state: AuthorityState,
    active: usize,
    fault: Option<String>,
}

#[derive(Clone)]
pub struct Authority {
    inner: Arc<Mutex<Inner>>,
    changed: watch::Sender<AuthorityState>,
    active: watch::Sender<usize>,
}

impl Authority {
    pub(super) fn new(writable: bool) -> Self {
        let state = AuthorityState {
            generation: 1,
            writable,
        };
        Self {
            inner: Arc::new(Mutex::new(Inner { state, active: 0, fault: None })),
            changed: watch::channel(state).0,
            active: watch::channel(0).0,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<AuthorityState> {
        self.changed.subscribe()
    }
    pub fn state(&self) -> AuthorityState {
        self.inner.lock().expect("authority").state
    }
    pub fn permits(&self, generation: Option<u64>) -> bool {
        let state = self.state();
        state.writable && generation.is_none_or(|g| g == state.generation)
    }

    /// Failed native cleanup cannot be repaired by granting a new generation.
    /// Keep the reason for the life of this Db so sync can report a fault after
    /// ordinary accepted activities drain, without publishing or releasing.
    pub fn poison(&self, reason: impl Into<String>) {
        let mut inner = self.inner.lock().expect("authority");
        if inner.fault.is_none() { inner.fault = Some(reason.into()); }
        inner.state.writable = false;
        self.changed.send_replace(inner.state);
    }

    pub fn fault(&self) -> Option<String> {
        self.inner.lock().expect("authority").fault.clone()
    }

    /// Seal immediately. Reopening is allowed only after retirement completes.
    pub(super) fn set(&self, writable: bool) {
        if writable {
            self.grant_if(|| true);
            return;
        }
        let mut inner = self.inner.lock().expect("authority");
        if !inner.state.writable {
            return;
        }
        inner.state.writable = false;
        self.changed.send_replace(inner.state);
    }

    pub(super) fn grant_if(&self, allowed: impl FnOnce() -> bool) {
        let mut inner = self.inner.lock().expect("authority");
        if inner.state.writable || inner.active != 0 || inner.fault.is_some() || !allowed() {
            return;
        }
        inner.state.generation += 1;
        inner.state.writable = true;
        self.changed.send_replace(inner.state);
    }

    /// Serialize write acceptance with revocation. The callback only enqueues;
    /// it must never wait for the transaction or invoke arbitrary app code.
    pub(super) fn admit<T>(
        &self,
        generation: Option<u64>,
        enqueue: impl FnOnce() -> T,
    ) -> Option<T> {
        let inner = self.inner.lock().expect("authority");
        if !inner.state.writable || generation.is_some_and(|g| g != inner.state.generation) {
            return None;
        }
        Some(enqueue())
    }

    pub fn enter(&self) -> Option<Activity> {
        let mut inner = self.inner.lock().expect("authority");
        if !inner.state.writable {
            return None;
        }
        inner.active += 1;
        self.active.send_replace(inner.active);
        Some(Activity {
            authority: self.clone(),
            generation: inner.state.generation,
        })
    }

    /// Do not hold a service Activity while awaiting this barrier.
    pub async fn quiesce(&self) {
        self.set(false);
        let mut active = self.active.subscribe();
        let _ = active.wait_for(|n| *n == 0).await;
    }
}

pub struct Activity {
    authority: Authority,
    generation: u64,
}
impl Activity {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Extend already accepted work through its cleanup. This cannot grant
    /// write permission; it only retains the retirement barrier. Requiring a
    /// live parent prevents a cleanup from appearing after quiescence ended.
    pub fn fork(&self) -> Self {
        let mut inner = self.authority.inner.lock().expect("authority");
        inner.active += 1;
        self.authority.active.send_replace(inner.active);
        Self {
            authority: self.authority.clone(),
            generation: self.generation,
        }
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        let mut inner = self.authority.inner.lock().expect("authority");
        inner.active -= 1;
        self.authority.active.send_replace(inner.active);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoked_generation_never_reopens_and_quiesce_joins_descendants() {
        crate::runtime::block_on(async {
            let a = Authority::new(true);
            let task = a.enter().unwrap();
            let old = task.generation();
            a.set(false);
            assert!(!a.permits(Some(old)));
            a.set(true);
            assert!(!a.state().writable, "cannot overlap a retired service");
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), a.quiesce())
                    .await
                    .is_err()
            );
            drop(task);
            a.quiesce().await;
            a.set(true);
            assert!(a.permits(None));
            assert!(!a.permits(Some(old)), "late completions stay fenced");
        });
    }
}
