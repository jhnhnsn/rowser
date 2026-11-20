//! Route matching: (Host alias or unverified host, URL) -> (Route index,
//! Target). Pure: no ports, no I/O.

use url::Url;

use crate::config::{Config, Route, Target};

/// The host an Open request comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestHost {
    /// A Host alias from a Host stamp: matched against `[[route]]` hosts.
    Alias(String),
    /// A Reported host name: never matches a `[[route]]`.
    Unverified(String),
}

/// The result of `Routes::pick`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The index of the matching `[[route]]`; `None` for `[unverified]` or
    /// `[default]`.
    pub route: Option<usize>,
    pub target: Target,
    /// The Route has `ask = true`.
    pub ask: bool,
}

/// The Routes of one config file, with the spec's defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routes {
    routes: Vec<Route>,
    unverified: Target,
    default: Target,
}

impl Default for Routes {
    fn default() -> Routes {
        Routes::from_config(&Config::default())
    }
}

impl Routes {
    pub fn from_config(config: &Config) -> Routes {
        Routes {
            routes: config.routes.clone(),
            unverified: config.unverified.clone().unwrap_or(Target::Ask),
            default: config.default.clone().unwrap_or(Target::System),
        }
    }

    /// The first matching Route in file order; else `[unverified]` for a
    /// Reported host name, or `[default]`.
    pub fn pick(&self, host: &RequestHost, url: &Url) -> Pick {
        let alias = match host {
            RequestHost::Alias(a) => a,
            RequestHost::Unverified(_) => {
                return Pick {
                    route: None,
                    target: self.unverified.clone(),
                    ask: false,
                }
            }
        };
        let url_host = url.host_str().unwrap_or("");
        for (i, r) in self.routes.iter().enumerate() {
            if glob_match(&r.host, alias)
                && (r.matches.is_empty() || r.matches.iter().any(|m| glob_match(m, url_host)))
            {
                return Pick {
                    route: Some(i),
                    target: r.target.clone(),
                    ask: r.ask,
                };
            }
        }
        Pick {
            route: None,
            target: self.default.clone(),
            ask: false,
        }
    }

    /// The `[default]` Target, or `system` when that is `ask`: what "Open"
    /// uses for an `ask` Target.
    pub fn fallback(&self) -> Target {
        match self.default {
            Target::Ask => Target::System,
            ref t => t.clone(),
        }
    }
}

/// Matches `name` against a glob `pattern` with `*` (any run) and `?` (one
/// character), ignoring ASCII case. The `--confirm-hosts` rules.
pub fn glob_match(pattern: &str, name: &str) -> bool {
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
    use super::*;
    use crate::config::parse;

    fn routes(text: &str) -> Routes {
        Routes::from_config(&parse(text).unwrap())
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn alias(a: &str) -> RequestHost {
        RequestHost::Alias(a.into())
    }

    const TWO: &str =
        "[[route]]\nhost = \"g14\"\nmatch = [\"*.google.com\"]\nopen = \"chrome:Work\"\n\
                       [[route]]\nhost = \"g1?\"\nopen = \"safari\"\n";

    #[test]
    fn first_matching_route_wins() {
        let p = routes(TWO).pick(&alias("g14"), &url("https://mail.google.com/x"));
        assert_eq!(p.route, Some(0));
        assert_eq!(p.target.to_string(), "chrome:Work");
    }

    #[test]
    fn match_globs_test_the_url_host() {
        let r = routes(TWO);
        let p = r.pick(&alias("G14"), &url("https://example.com/google.com"));
        assert_eq!(p.route, Some(1));
        assert_eq!(p.target.to_string(), "safari");
    }

    #[test]
    fn no_match_goes_to_default_which_defaults_to_system() {
        let p = routes(TWO).pick(&alias("db1"), &url("https://example.com/"));
        assert_eq!(
            p,
            Pick {
                route: None,
                target: Target::System,
                ask: false
            }
        );
        let r = routes("[default]\nopen = \"firefox:Dev\"\n");
        assert_eq!(
            r.pick(&alias("db1"), &url("https://e.com/"))
                .target
                .to_string(),
            "firefox:Dev"
        );
    }

    #[test]
    fn a_reported_host_never_matches_a_route_and_goes_to_unverified() {
        let r = routes(TWO);
        let p = r.pick(
            &RequestHost::Unverified("g14".into()),
            &url("https://mail.google.com/"),
        );
        assert_eq!(p.route, None);
        assert_eq!(p.target, Target::Ask);
        let r = routes("[unverified]\nopen = \"system\"\n");
        let p = r.pick(&RequestHost::Unverified("x".into()), &url("https://e.com/"));
        assert_eq!(p.target, Target::System);
    }

    #[test]
    fn ask_true_is_reported() {
        let r = routes("[[route]]\nhost = \"*\"\nopen = \"system\"\nask = true\n");
        assert!(r.pick(&alias("any"), &url("https://e.com/")).ask);
    }

    #[test]
    fn fallback_is_system_when_default_is_ask() {
        assert_eq!(
            routes("[default]\nopen = \"ask\"\n").fallback(),
            Target::System
        );
        assert_eq!(routes("").fallback(), Target::System);
    }

    #[test]
    fn glob_rules() {
        assert!(glob_match("db?", "db1"));
        assert!(!glob_match("db?", "db12"));
        assert!(glob_match("*.prod", "a.prod.b.prod"));
        assert!(glob_match("DB.Prod", "db.PROD"));
    }
}
