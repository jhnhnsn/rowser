//! Sessions: one per connected Wrapper, kept while its relay connection
//! stays open.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Default)]
pub struct Sessions {
    inner: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    next: u64,
    by_id: HashMap<u64, (String, Instant)>,
}

/// Drops its Session when it goes out of scope.
pub struct Registered {
    sessions: Arc<Sessions>,
    id: u64,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.sessions.lock().by_id.remove(&self.id);
    }
}

impl Sessions {
    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds a Session for `alias`; it lasts as long as the guard.
    pub fn register(self: &Arc<Self>, alias: &str) -> Registered {
        let mut t = self.lock();
        let id = t.next;
        t.next += 1;
        t.by_id.insert(id, (alias.to_string(), Instant::now()));
        Registered {
            sessions: self.clone(),
            id,
        }
    }

    /// (Host alias, start) of each Session, oldest first.
    pub fn list(&self) -> Vec<(String, Instant)> {
        let t = self.lock();
        let mut ids: Vec<_> = t.by_id.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().map(|id| t.by_id[&id].clone()).collect()
    }
}
