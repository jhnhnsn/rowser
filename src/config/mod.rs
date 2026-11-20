//! The config file: path rules, load and validate.
//!
//! `$XDG_CONFIG_HOME/rowser/config.toml` (default `~/.config/rowser/config.toml`)
//! holds the Routes. The Daemon only reads it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;
use toml::Spanned;

/// Where a Route sends a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The `--open-command`.
    System,
    /// Wait for the user's answer; "Open" uses the `[default]` Target.
    Ask,
    /// A browser short name, optionally with a Profile.
    Browser {
        browser: String,
        profile: Option<String>,
    },
}

impl FromStr for Target {
    type Err = String;
    fn from_str(s: &str) -> Result<Target, String> {
        match s {
            "system" => return Ok(Target::System),
            "ask" => return Ok(Target::Ask),
            _ => {}
        }
        let (browser, profile) = match s.split_once(':') {
            Some((b, p)) => (b, Some(p)),
            None => (s, None),
        };
        if browser.trim().is_empty() {
            return Err(format!(
                "invalid target `{s}`: use system, ask, <browser> or <browser>:<profile>"
            ));
        }
        if profile.is_some_and(|p| p.trim().is_empty()) {
            return Err(format!("invalid target `{s}`: the profile is empty"));
        }
        Ok(Target::Browser {
            browser: browser.into(),
            profile: profile.map(str::to_owned),
        })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::System => f.write_str("system"),
            Target::Ask => f.write_str("ask"),
            Target::Browser {
                browser,
                profile: None,
            } => f.write_str(browser),
            Target::Browser {
                browser,
                profile: Some(p),
            } => write!(f, "{browser}:{p}"),
        }
    }
}

/// One `[[route]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Glob matched against the Host alias.
    pub host: String,
    /// Globs matched against the URL's host; empty means every URL.
    pub matches: Vec<String>,
    pub target: Target,
    /// Use the Target only after the user approves.
    pub ask: bool,
}

/// The parsed config file. `None` entries take the spec's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub routes: Vec<Route>,
    /// `[unverified] open`; default `ask`.
    pub unverified: Option<Target>,
    /// `[default] open`; default `system`.
    pub default: Option<Target>,
}

/// The config file path: `$XDG_CONFIG_HOME/rowser/config.toml`, else
/// `$HOME/.config/rowser/config.toml`.
pub fn path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .ok_or("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(base.join("rowser").join("config.toml"))
}

/// Reads and validates the file at `path`. A missing file is an empty
/// `Config` (no Routes). Errors name the file, the line and the reason.
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    route: Vec<RawRoute>,
    unverified: Option<RawEntry>,
    default: Option<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    host: Spanned<String>,
    #[serde(default, rename = "match")]
    matches: Vec<String>,
    open: Spanned<String>,
    #[serde(default)]
    ask: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    open: Spanned<String>,
}

/// Parses config text. Errors start with `line <n>: `.
pub fn parse(text: &str) -> Result<Config, String> {
    let line_of = |offset: usize| text[..offset.min(text.len())].matches('\n').count() + 1;
    let raw: RawFile = toml::from_str(text).map_err(|e| {
        let line = e.span().map(|s| line_of(s.start)).unwrap_or(1);
        format!("line {line}: {}", e.message().trim())
    })?;
    let target = |open: &Spanned<String>| {
        open.get_ref()
            .parse::<Target>()
            .map_err(|e| format!("line {}: {e}", line_of(open.span().start)))
    };
    let mut routes = Vec::new();
    for r in &raw.route {
        if r.host.get_ref().trim().is_empty() {
            return Err(format!(
                "line {}: a route's host is empty",
                line_of(r.host.span().start)
            ));
        }
        routes.push(Route {
            host: r.host.get_ref().clone(),
            matches: r.matches.clone(),
            target: target(&r.open)?,
            ask: r.ask,
        });
    }
    Ok(Config {
        routes,
        unverified: raw
            .unverified
            .as_ref()
            .map(|e| target(&e.open))
            .transpose()?,
        default: raw.default.as_ref().map(|e| target(&e.open)).transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_example() {
        let c = parse(
            "[[route]]\nhost = \"g14\"\nmatch = [\"*.google.com\"]\nopen = \"chrome:Profile 2\"\n\n\
             [[route]]\nhost = \"g14\"\nopen = \"safari\"\nask = true\n\n\
             [unverified]\nopen = \"ask\"\n\n[default]\nopen = \"system\"\n",
        )
        .unwrap();
        assert_eq!(c.routes.len(), 2);
        assert_eq!(c.routes[0].matches, ["*.google.com"]);
        assert_eq!(c.routes[0].target.to_string(), "chrome:Profile 2");
        assert!(c.routes[1].ask);
        assert_eq!(c.unverified, Some(Target::Ask));
        assert_eq!(c.default, Some(Target::System));
    }

    #[test]
    fn a_bad_target_names_its_line() {
        let e = parse("[[route]]\nhost = \"g14\"\nopen = \":x\"\n").unwrap_err();
        assert!(e.starts_with("line 3: invalid target"), "{e}");
    }

    #[test]
    fn a_syntax_error_and_an_unknown_key_name_their_line() {
        let e = parse("[default]\nopen = \"system\"\n\n[[route]]\nhost = \n").unwrap_err();
        assert!(e.starts_with("line 5:"), "{e}");
        let e = parse("[default]\nopen = \"system\"\nbrowser = \"x\"\n").unwrap_err();
        assert!(e.starts_with("line 3:"), "{e}");
    }
}
