//! `--confirm-hosts` matching and the confirmation timeout.

use std::time::Duration;

use super::DaemonConfig;
use crate::routes::glob_match;

/// How long a confirmation waits for the user by default. The Daemon's
/// timeout and the desktop notification or dialog both use it.
pub(crate) const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

impl DaemonConfig {
    /// Whether an Open request from `host` must wait for confirmation.
    pub(crate) fn needs_confirm(&self, host: &str) -> bool {
        self.confirm_hosts.iter().any(|p| glob_match(p, host))
    }
}

#[cfg(test)]
mod tests {
    use crate::routes::glob_match;

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(glob_match("db?", "db1"));
        assert!(!glob_match("db?", "db"));
        assert!(!glob_match("db?", "db12"));
    }

    #[test]
    fn star_in_the_middle_matches_any_run() {
        assert!(glob_match("db*.prod", "db.prod"));
        assert!(glob_match("db*.prod", "db-eu-1.prod"));
        assert!(!glob_match("db*.prod", "db-eu-1.prod.x"));
    }

    #[test]
    fn star_backtracks_past_an_early_partial_match() {
        assert!(glob_match("*.prod", "a.prod.b.prod"));
        assert!(glob_match("a*b*c", "aXbYbZc"));
        assert!(!glob_match("a*b*c", "aXbYbZ"));
    }

    #[test]
    fn ascii_case_is_ignored() {
        assert!(glob_match("DB.Prod", "db.PROD"));
        assert!(!glob_match("db.prod", "db.prodx"));
    }
}
