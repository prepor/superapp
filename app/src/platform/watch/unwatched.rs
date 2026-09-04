//! A platform with no watcher of its own: nothing is ever reported, and
//! the books stand as they are.
//!
//! Not a failure — a build for a platform whose file panels refresh on
//! their own writes and on nothing else, which is the whole app before
//! this module existed.

use super::Watching;

pub struct Thread;

impl Thread {
    pub fn start(_w: Watching) -> Thread {
        Thread
    }

    pub fn wake(&self) {}

    pub fn stop(&mut self) {}
}
