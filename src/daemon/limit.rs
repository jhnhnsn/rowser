//! Per-connection rate limit: at most `max` events in any `window`.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

pub(crate) struct Limit {
    max: usize,
    window: Duration,
    recent: VecDeque<Instant>,
}

impl Limit {
    pub(crate) fn new(max: usize, window: Duration) -> Limit {
        Limit {
            max,
            window,
            recent: VecDeque::new(),
        }
    }

    /// Records an event now and says whether it is within the limit.
    /// A rejected event does not count against later ones.
    pub(crate) fn allow(&mut self) -> bool {
        let now = Instant::now();
        while self
            .recent
            .front()
            .is_some_and(|&t| now.duration_since(t) >= self.window)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= self.max {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn allows_max_per_window_then_frees_slots_as_they_age() {
        let mut limit = Limit::new(5, Duration::from_secs(10));
        for _ in 0..3 {
            assert!(limit.allow());
        }
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(limit.allow());
        assert!(limit.allow());
        assert!(!limit.allow());
        // The first three age out at t=10, the other two at t=14.
        tokio::time::advance(Duration::from_secs(6)).await;
        for _ in 0..3 {
            assert!(limit.allow());
        }
        assert!(!limit.allow());
        tokio::time::advance(Duration::from_millis(3999)).await;
        assert!(!limit.allow());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(limit.allow());
    }
}
