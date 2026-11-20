//! The in-memory ring of the last 200 Open requests and `rowser forward`
//! requests, for `rowser activity`. A Daemon restart clears it.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::protocol::ActivityEntry;

/// How many entries the ring keeps.
pub const CAPACITY: usize = 200;

pub(super) struct Activity {
    inner: Mutex<Ring>,
    /// New and updated entries, for `rowser activity --follow`.
    tx: broadcast::Sender<ActivityEntry>,
}

#[derive(Default)]
struct Ring {
    next: u64,
    /// (forward id, entry), oldest first.
    entries: VecDeque<(Option<String>, ActivityEntry)>,
}

impl Default for Activity {
    fn default() -> Activity {
        Activity {
            inner: Mutex::default(),
            tx: broadcast::channel(CAPACITY).0,
        }
    }
}

impl Activity {
    fn lock(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds `entry` with the next id; entry 201 drops the oldest.
    /// `forward_id` links the entry to its Callback forward.
    pub fn push(&self, mut entry: ActivityEntry, forward_id: Option<String>) {
        let mut ring = self.lock();
        ring.next += 1;
        entry.id = ring.next;
        if ring.entries.len() == CAPACITY {
            ring.entries.pop_front();
        }
        ring.entries.push_back((forward_id, entry.clone()));
        let _ = self.tx.send(entry);
    }

    /// Records how long the Callback forward `forward_id` stayed open.
    pub fn forward_closed(&self, forward_id: &str, open: Duration) {
        let mut ring = self.lock();
        let found = ring
            .entries
            .iter_mut()
            .find(|(id, _)| id.as_deref() == Some(forward_id));
        if let Some((_, entry)) = found {
            entry.open_secs = Some(open.as_secs());
            let _ = self.tx.send(entry.clone());
        }
    }

    /// Every entry, oldest first, and a receiver for later changes. No
    /// change falls between the two.
    pub fn watch(&self) -> (Vec<ActivityEntry>, broadcast::Receiver<ActivityEntry>) {
        let ring = self.lock();
        let rx = self.tx.subscribe();
        (ring.entries.iter().map(|(_, e)| e.clone()).collect(), rx)
    }
}
