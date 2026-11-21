//! Route matching: (Host alias or unverified host, URL) -> (Route index,
//! Target). Pure: no ports, no I/O.

use url::Url;

use crate::browsers::Browsers;
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

    /// The `[unverified]` and `[default]` Targets, with the spec's defaults.
    pub fn fallbacks(&self) -> (&Target, &Target) {
        (&self.unverified, &self.default)
    }

    /// The Target `rowser status` shows for a Session: the first Route of
    /// `alias` without a `match` list, else `[default]`. It is what an
    /// `https` URL that no `match` list names opens in.
    pub fn session_target(&self, alias: &str) -> Target {
        self.routes
            .iter()
            .find(|r| r.matches.is_empty() && glob_match(&r.host, alias))
            .map_or_else(|| self.default.clone(), |r| r.target.clone())
    }

    /// The Target an Open request opens in, whether it waits for the user
    /// first, and the notice warnings. An `ask` Target waits, then opens in
    /// `[default]`. A browser Target found in `browsers` opens with its
    /// profile as a folder name; a missing one falls back to `[default]`
    /// with a warning, and a `[default]` of `ask` then waits too (ADR 0008).
    /// A `[default]` that is missing too opens with `system`.
    pub fn resolve(&self, host: &RequestHost, url: &Url, browsers: &Browsers) -> Resolved {
        let pick = self.pick(host, url);
        let mut resolved = Resolved {
            route: pick.route,
            target: Target::System,
            ask: pick.ask,
            warnings: vec![],
        };
        let mut chain = vec![&pick.target];
        if pick.target != self.default {
            chain.push(&self.default);
        }
        for (i, target) in chain.iter().enumerate() {
            match target {
                Target::System => return resolved,
                Target::Ask => resolved.ask = true,
                Target::Browser { .. } => match browsers.normalize(target) {
                    Ok(found) => {
                        resolved.target = found;
                        return resolved;
                    }
                    Err(why) => {
                        let next = chain
                            .get(i + 1)
                            .map_or("`system`".to_string(), |t| format!("[default] `{t}`"));
                        resolved.warnings.push(format!(
                            "target `{target}` is not available ({why}); fell back to {next}"
                        ));
                    }
                },
            }
        }
        resolved
    }
}

/// The result of `Routes::resolve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The index of the matching `[[route]]`, as in `Pick`.
    pub route: Option<usize>,
    /// The Target the URL opens in.
    pub target: Target,
    /// The user approves the URL before it opens.
    pub ask: bool,
    /// Notice warnings, such as a browser Target that fell back.
    pub warnings: Vec<String>,
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
    fn session_target_is_the_first_route_without_match_else_default() {
        let r = routes(TWO);
        assert_eq!(r.session_target("g14").to_string(), "safari");
        assert_eq!(r.session_target("db1").to_string(), "system");
    }

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

    fn resolve(text: &str, host: RequestHost) -> Resolved {
        routes(text).resolve(&host, &url("https://e.com/"), &Browsers::default())
    }

    fn chrome() -> Browsers {
        use crate::browsers::{Browser, Profile};
        Browsers {
            all: vec![Browser {
                name: "chrome".into(),
                display_name: "Google Chrome".into(),
                bundle_id: None,
                desktop_id: None,
                profiles: vec![Profile {
                    name: "Work".into(),
                    dir: "Profile 2".into(),
                }],
                exec: None,
            }],
            os: None,
        }
    }

    #[test]
    fn a_found_browser_target_opens_with_the_profile_folder() {
        let r = routes(
            "[[route]]\nhost = \"g14\"\nopen = \"chrome:Work\"\n[default]\nopen = \"ask\"\n",
        )
        .resolve(&alias("g14"), &url("https://e.com/"), &chrome());
        assert_eq!(r.target.to_string(), "chrome:Profile 2");
        assert!(!r.ask);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn a_missing_route_target_falls_back_to_a_found_browser_default() {
        let r =
            routes("[[route]]\nhost = \"g14\"\nopen = \"safari\"\n[default]\nopen = \"chrome\"\n")
                .resolve(&alias("g14"), &url("https://e.com/"), &chrome());
        assert_eq!(r.target.to_string(), "chrome");
        assert_eq!(
            r.warnings,
            ["target `safari` is not available (browser `safari` is not installed); fell back to [default] `chrome`"]
        );
    }

    #[test]
    fn an_ask_target_waits_and_opens_in_system() {
        let r = resolve("[default]\nopen = \"ask\"\n", alias("x"));
        assert_eq!(r.target, Target::System);
        assert!(r.ask);
        assert!(r.warnings.is_empty());
        assert!(resolve("", RequestHost::Unverified("x".into())).ask);
        assert!(!resolve("", alias("x")).ask);
    }

    #[test]
    fn a_browser_target_falls_back_to_an_ask_default_and_waits() {
        let text = "[unverified]\nopen = \"safari\"\n[default]\nopen = \"ask\"\n";
        let r = resolve(text, RequestHost::Unverified("x".into()));
        assert_eq!(r.target, Target::System);
        assert!(r.ask);
        assert_eq!(
            r.warnings,
            ["target `safari` is not available (browser `safari` is not installed); fell back to [default] `ask`"]
        );
        let text = "[[route]]\nhost = \"g14\"\nopen = \"chrome:Work\"\n[default]\nopen = \"ask\"\n";
        assert!(resolve(text, alias("g14")).ask);
    }

    #[test]
    fn a_browser_target_falls_back_to_a_system_default_without_waiting() {
        let r = resolve("[[route]]\nhost = \"*\"\nopen = \"safari\"\n", alias("g14"));
        assert_eq!(r.target, Target::System);
        assert!(!r.ask);
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn a_browser_default_warns_once() {
        let r = resolve("[default]\nopen = \"chrome:Work\"\n", alias("db1"));
        assert_eq!(
            r.warnings,
            ["target `chrome:Work` is not available (browser `chrome` is not installed); fell back to `system`"]
        );
        let text = "[[route]]\nhost = \"*\"\nopen = \"safari\"\n[default]\nopen = \"firefox\"\n";
        assert_eq!(resolve(text, alias("g14")).warnings.len(), 2);
    }

    #[test]
    fn glob_rules() {
        assert!(glob_match("db?", "db1"));
        assert!(!glob_match("db?", "db12"));
        assert!(glob_match("*.prod", "a.prod.b.prod"));
        assert!(glob_match("DB.Prod", "db.PROD"));
    }
}
