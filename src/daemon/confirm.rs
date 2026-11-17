//! `--confirm-hosts` matching and the confirmation timeout.

use std::time::Duration;

use super::{DaemonConfig, NoticeHost};

/// How long a confirmation waits for the user by default. The Daemon's
/// timeout and the desktop notification or dialog both use it.
pub(crate) const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

impl DaemonConfig {
    /// Whether an Open request from `host` must wait for confirmation.
    pub(crate) fn needs_confirm(&self, host: &NoticeHost) -> bool {
        let name = match host {
            NoticeHost::Alias(alias) => alias,
            NoticeHost::Reported(name) if self.confirm_unverified => name,
            NoticeHost::Reported(_) => return false,
        };
        self.confirm_hosts.iter().any(|p| glob_match(p, name))
    }
}

/// Matches `name` against a glob `pattern` with `*` (any run) and `?` (one
/// character), ignoring ASCII case.
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let n: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::glob_match;

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
