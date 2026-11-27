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

/// One `[[host]]` entry: a Saved host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedHost {
    /// The Host alias, given to `ssh` unchanged.
    pub name: String,
    /// Where rowser is on that host; default `~/.local/bin/rowser`.
    pub remote_path: String,
    /// Default `true`.
    pub auto_update: bool,
    /// Default `true`.
    pub default_browser: bool,
}

/// The default `remote_path` of a Saved host.
pub const DEFAULT_REMOTE_PATH: &str = "~/.local/bin/rowser";

/// The parsed config file. `None` entries take the spec's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The Saved hosts, in file order.
    pub hosts: Vec<SavedHost>,
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
    host: Vec<RawHost>,
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
struct RawHost {
    name: Spanned<String>,
    remote_path: Option<String>,
    #[serde(default = "yes")]
    auto_update: bool,
    #[serde(default = "yes")]
    default_browser: bool,
}

fn yes() -> bool {
    true
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
    let mut hosts: Vec<SavedHost> = Vec::new();
    for h in &raw.host {
        let line = line_of(h.name.span().start);
        let name = h.name.get_ref();
        // A name that starts with `-` would be an ssh option.
        if name.trim().is_empty() || name.starts_with('-') {
            return Err(format!("line {line}: invalid host name `{name}`"));
        }
        if hosts.iter().any(|o| o.name == *name) {
            return Err(format!("line {line}: host `{name}` is listed twice"));
        }
        hosts.push(SavedHost {
            name: name.clone(),
            remote_path: h
                .remote_path
                .clone()
                .unwrap_or_else(|| DEFAULT_REMOTE_PATH.into()),
            auto_update: h.auto_update,
            default_browser: h.default_browser,
        });
    }
    Ok(Config {
        hosts,
        routes,
        unverified: raw
            .unverified
            .as_ref()
            .map(|e| target(&e.open))
            .transpose()?,
        default: raw.default.as_ref().map(|e| target(&e.open)).transpose()?,
    })
}

/// Parses `text` for an in-place edit; the text must first be a valid file.
fn edit_doc(text: &str) -> Result<(toml_edit::DocumentMut, usize), String> {
    let count = parse(text)?.routes.len();
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| e.to_string())?;
    Ok((doc, count))
}

/// The `[[route]]` array of `doc`, created when missing.
fn route_array(doc: &mut toml_edit::DocumentMut) -> &mut toml_edit::ArrayOfTables {
    if doc.get("route").is_none() {
        doc.insert(
            "route",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    // `edit_doc` parsed the text as a valid file first, and `parse` refuses
    // a `route` key that is not `[[route]]`.
    doc["route"]
        .as_array_of_tables_mut()
        .expect("a valid file has `route` as [[route]]")
}

/// The table that renders first in the file.
enum Top {
    Route(usize),
    Table(String),
}

/// Finds the table at the top of the file; `None` when key-values come
/// first or the file has no tables.
fn top(doc: &toml_edit::DocumentMut) -> Option<Top> {
    if doc.iter().any(|(_, item)| item.is_value()) {
        return None;
    }
    let mut best: Option<(usize, Top)> = None;
    let mut consider = |pos: Option<usize>, t: Top| {
        let pos = pos.unwrap_or(usize::MAX);
        if best.as_ref().is_none_or(|(p, _)| pos < *p) {
            best = Some((pos, t));
        }
    };
    for (key, item) in doc.iter() {
        match item {
            toml_edit::Item::Table(t) => consider(t.position(), Top::Table(key.to_string())),
            toml_edit::Item::ArrayOfTables(a) if key == "route" => {
                for (i, t) in a.iter().enumerate() {
                    consider(t.position(), Top::Route(i));
                }
            }
            _ => {}
        }
    }
    best.map(|(_, t)| t)
}

fn top_table_mut<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    top: &Top,
) -> Option<&'a mut toml_edit::Table> {
    match top {
        Top::Route(i) => route_array(doc).get_mut(*i),
        Top::Table(k) => doc.get_mut(k)?.as_table_mut(),
    }
}

fn prefix(t: &toml_edit::Table) -> String {
    t.decor()
        .prefix()
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string()
}

/// `text` without its leading blank lines.
fn trim_blank_lines(text: &str) -> &str {
    let mut rest = text;
    while let Some(end) = rest.find('\n') {
        if !rest[..end].trim().is_empty() {
            break;
        }
        rest = &rest[end + 1..];
    }
    rest
}

/// Takes the file header off the top Route: the part of its comments up to
/// the last blank line. The rest stays with the Route.
fn take_header(doc: &mut toml_edit::DocumentMut) -> String {
    let Some(Top::Route(i)) = top(doc) else {
        return String::new();
    };
    let Some(t) = route_array(doc).get_mut(i) else {
        return String::new();
    };
    let p = prefix(t);
    let Some(cut) = p.rfind("\n\n").or_else(|| p.rfind("\n\r\n")) else {
        return String::new();
    };
    let cut = p[cut..]
        .find(|c: char| !c.is_whitespace())
        .map_or(p.len(), |n| cut + n);
    let (header, rest) = p.split_at(cut);
    t.decor_mut().set_prefix(rest);
    header.to_string()
}

/// Gives each Route one blank line above it and the top table none, then
/// puts `header` back at the top of the file.
fn tidy(doc: &mut toml_edit::DocumentMut, header: &str) {
    for t in route_array(doc).iter_mut() {
        let p = format!("\n{}", trim_blank_lines(&prefix(t)));
        t.decor_mut().set_prefix(p);
    }
    match top(doc) {
        Some(top) => {
            if let Some(t) = top_table_mut(doc, &top) {
                let p = format!("{header}{}", trim_blank_lines(&prefix(t)));
                t.decor_mut().set_prefix(p);
            }
        }
        None => {
            let trailing = doc.trailing().as_str().unwrap_or("").to_string();
            doc.set_trailing(format!("{header}{trailing}"));
        }
    }
}

/// Gives the edited text after it checks that it is a valid file.
fn finish(doc: &toml_edit::DocumentMut) -> Result<String, String> {
    let text = doc.to_string();
    parse(&text)?;
    Ok(text)
}

fn check_number(n: usize, count: usize) -> Result<usize, String> {
    if n == 0 || n > count {
        return Err(format!("no route {n}: the file has {count} routes"));
    }
    Ok(n - 1)
}

/// Appends `route` as a new `[[route]]` to the config text. Comments and
/// layout stay. Gives the new text and the new Route's number. Refuses an
/// invalid text or result. `comment` goes after `open` as `# <comment>`.
pub fn add_route(
    text: &str,
    route: &Route,
    comment: Option<&str>,
) -> Result<(String, usize), String> {
    let (mut doc, count) = edit_doc(text)?;
    let mut t = toml_edit::Table::new();
    t["host"] = toml_edit::value(route.host.as_str());
    if !route.matches.is_empty() {
        let mut a = toml_edit::Array::new();
        for m in &route.matches {
            a.push(m.as_str());
        }
        t["match"] = toml_edit::value(a);
    }
    let mut open = toml_edit::Value::from(route.target.to_string());
    if let Some(c) = comment {
        // One line only, so the comment cannot end the line early.
        let c = c.replace(['\n', '\r'], " ");
        open.decor_mut().set_suffix(format!("  # {c}"));
    }
    t["open"] = toml_edit::Item::Value(open);
    if route.ask {
        t["ask"] = toml_edit::value(true);
    }
    route_array(&mut doc).push(t);
    Ok((finish(&doc)?, count + 1))
}

/// Removes Route `n` (from 1) from the config text. The file header (the
/// comments above a blank line at the top) stays.
pub fn remove_route(text: &str, n: usize) -> Result<String, String> {
    let (mut doc, count) = edit_doc(text)?;
    let i = check_number(n, count)?;
    let header = take_header(&mut doc);
    route_array(&mut doc).remove(i);
    tidy(&mut doc, &header);
    finish(&doc)
}

/// Moves Route `n` to place `to` (both from 1) in the config text. Each
/// Route keeps its comments; the file header stays at the top.
pub fn move_route(text: &str, n: usize, to: usize) -> Result<String, String> {
    let (mut doc, count) = edit_doc(text)?;
    let from = check_number(n, count)?;
    let to = check_number(to, count)?;
    let header = take_header(&mut doc);
    let routes = route_array(&mut doc);
    let positions: Vec<Option<usize>> = routes.iter().map(|t| t.position()).collect();
    let mut tables: Vec<toml_edit::Table> = routes.iter().cloned().collect();
    let moved = tables.remove(from);
    tables.insert(to, moved);
    routes.clear();
    // A table renders at its position: give the file's slots out in order.
    for (mut t, pos) in tables.into_iter().zip(positions) {
        if let Some(p) = pos {
            t.set_position(p);
        }
        routes.push(t);
    }
    tidy(&mut doc, &header);
    finish(&doc)
}

/// The `[[host]]` array of `doc`, created when missing.
fn host_array(doc: &mut toml_edit::DocumentMut) -> &mut toml_edit::ArrayOfTables {
    if doc.get("host").is_none() {
        doc.insert(
            "host",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    // `parse` refuses a `host` key that is not `[[host]]`.
    doc["host"]
        .as_array_of_tables_mut()
        .expect("a valid file has `host` as [[host]]")
}

/// The index of the Saved host `name` in the `[[host]]` array.
fn host_index(doc: &mut toml_edit::DocumentMut, name: &str) -> Option<usize> {
    host_array(doc)
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
}

/// Adds the Saved host `host` to the config text, or updates the entry of
/// the same name in place (its `remote_path` and `default_browser`; other
/// keys and comments stay). `default_browser = false` is written only when
/// false. So the same `add` twice gives the same text.
pub fn add_host(text: &str, host: &SavedHost) -> Result<String, String> {
    let (mut doc, _) = edit_doc(text)?;
    let was_empty = text.trim().is_empty();
    let found = host_index(&mut doc, &host.name);
    let hosts = host_array(&mut doc);
    let t = match found {
        Some(i) => hosts.get_mut(i).expect("found above"),
        None => {
            let mut t = toml_edit::Table::new();
            t["name"] = toml_edit::value(host.name.as_str());
            if !was_empty {
                t.decor_mut().set_prefix("\n");
            }
            hosts.push(t);
            hosts.iter_mut().last().expect("pushed above")
        }
    };
    // Keep the decor (such as an end-of-line comment) of an old value.
    let mut path = toml_edit::Value::from(host.remote_path.as_str());
    if let Some(old) = t.get("remote_path").and_then(|i| i.as_value()) {
        *path.decor_mut() = old.decor().clone();
    }
    t["remote_path"] = toml_edit::Item::Value(path);
    if host.default_browser {
        t.remove("default_browser");
    } else {
        t["default_browser"] = toml_edit::value(false);
    }
    finish(&doc)
}

/// Removes the Saved host `name` from the config text. Its comments go with
/// it, except a file header (the comments above a blank line), which moves
/// to the table that follows.
pub fn remove_host(text: &str, name: &str) -> Result<String, String> {
    let (mut doc, _) = edit_doc(text)?;
    let i = host_index(&mut doc, name).ok_or_else(|| format!("no Saved host `{name}`"))?;
    let removed = host_array(&mut doc).get(i).expect("found above").clone();
    host_array(&mut doc).remove(i);
    if host_array(&mut doc).is_empty() {
        doc.remove("host");
    }
    let p = prefix(&removed);
    if let Some(cut) = p.rfind("\n\n") {
        let header = &p[..cut + 2];
        let pos = removed.position().unwrap_or(0);
        // The table that renders next after the removed one.
        let mut next: Option<(usize, &mut toml_edit::Table)> = None;
        for (_, item) in doc.iter_mut() {
            let tables: Vec<&mut toml_edit::Table> = match item {
                toml_edit::Item::Table(t) => vec![t],
                toml_edit::Item::ArrayOfTables(a) => a.iter_mut().collect(),
                _ => vec![],
            };
            for t in tables {
                let tp = t.position().unwrap_or(usize::MAX);
                if tp > pos && next.as_ref().is_none_or(|(np, _)| tp < *np) {
                    next = Some((tp, t));
                }
            }
        }
        match next {
            Some((_, t)) => {
                let rest = trim_blank_lines(&prefix(t)).to_string();
                t.decor_mut().set_prefix(format!("{header}{rest}"));
            }
            None => {
                let trailing = doc.trailing().as_str().unwrap_or("").to_string();
                doc.set_trailing(format!("{header}{trailing}"));
            }
        }
    }
    finish(&doc)
}

/// Writes `text` to `path` through a temp file and a rename, so the file is
/// never half written. Creates the directory when missing. A symlinked
/// file stays a symlink (the write goes to its target), and the file keeps
/// its permissions.
pub fn write(path: &Path, text: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let (path, old) = match std::fs::canonicalize(path) {
        Ok(real) => {
            let perms = std::fs::metadata(&real).map_err(fail)?.permissions();
            (real, Some(perms))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (path.to_path_buf(), None),
        Err(e) => return Err(fail(e)),
    };
    let path = path.as_path();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(fail)?;
    if let Some(perms) = old {
        if let Err(e) = std::fs::set_permissions(&tmp, perms) {
            let _ = std::fs::remove_file(&tmp);
            return Err(fail(e));
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
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
    fn saved_hosts_parse_with_defaults_and_bad_names_are_refused() {
        let c = parse(
            "[[host]]\nname = \"g14\"\n\n\
             [[host]]\nname = \"pi\"\nremote_path = \"/opt/rowser\"\nauto_update = false\n",
        )
        .unwrap();
        assert_eq!(
            c.hosts,
            [
                SavedHost {
                    name: "g14".into(),
                    remote_path: "~/.local/bin/rowser".into(),
                    auto_update: true,
                    default_browser: true,
                },
                SavedHost {
                    name: "pi".into(),
                    remote_path: "/opt/rowser".into(),
                    auto_update: false,
                    default_browser: true,
                },
            ]
        );
        for bad in [
            "[[host]]\nname = \"\"\n",
            "[[host]]\nname = \"-oProxyCommand=x\"\n",
            "[[host]]\nname = \"g14\"\n[[host]]\nname = \"g14\"\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn system_and_ask_are_keywords_and_other_names_are_browsers() {
        let target = |s: &str| {
            let c = parse(&format!("[[route]]\nhost = \"g14\"\nopen = \"{s}\"\n")).unwrap();
            c.routes[0].target.clone()
        };
        assert_eq!(target("system"), Target::System);
        assert_eq!(target("ask"), Target::Ask);
        assert_eq!(
            target("firefox"),
            Target::Browser {
                browser: "firefox".into(),
                profile: None
            }
        );
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

    fn saved(name: &str, path: &str, default_browser: bool) -> SavedHost {
        SavedHost {
            name: name.into(),
            remote_path: path.into(),
            auto_update: true,
            default_browser,
        }
    }

    #[test]
    fn add_host_keeps_the_header_and_appends() {
        let text = "# my config\n\n[default]\nopen = \"system\"\n";
        let out = add_host(text, &saved("g14", "/opt/rowser", true)).unwrap();
        assert_eq!(
            out,
            "# my config\n\n[default]\nopen = \"system\"\n\n\
             [[host]]\nname = \"g14\"\nremote_path = \"/opt/rowser\"\n"
        );
    }

    #[test]
    fn add_host_updates_an_entry_in_place_and_keeps_its_comments() {
        let text = "[[host]]\nname = \"g14\" # laptop\nremote_path = \"/a\" # old path\n\
                    auto_update = false\n\n[[host]]\nname = \"pi\"\nremote_path = \"/b\"\n";
        let out = add_host(text, &saved("g14", "/new", false)).unwrap();
        assert_eq!(
            out,
            "[[host]]\nname = \"g14\" # laptop\nremote_path = \"/new\" # old path\n\
             auto_update = false\ndefault_browser = false\n\n\
             [[host]]\nname = \"pi\"\nremote_path = \"/b\"\n"
        );
        let out = add_host(&out, &saved("g14", "/new", true)).unwrap();
        assert!(!out.contains("default_browser"), "{out}");
        assert_eq!(add_host(&out, &saved("g14", "/new", true)).unwrap(), out);
    }

    #[test]
    fn remove_host_keeps_the_header_and_removes_the_last_entry() {
        let text = "# my config\n\n[[host]]\nname = \"g14\"\nremote_path = \"/a\"\n";
        let out = remove_host(text, "g14").unwrap();
        assert_eq!(out, "# my config\n\n");
        assert!(parse(&out).unwrap().hosts.is_empty());
        assert!(remove_host(&out, "g14").is_err());
    }

    #[test]
    fn remove_host_moves_the_header_to_the_next_table() {
        let text = "# my config\n\n# g14 notes\n[[host]]\nname = \"g14\"\nremote_path = \"/a\"\n\n\
                    [[host]]\nname = \"pi\"\nremote_path = \"/b\"\n";
        let out = remove_host(text, "g14").unwrap();
        assert_eq!(
            out,
            "# my config\n\n[[host]]\nname = \"pi\"\nremote_path = \"/b\"\n"
        );
    }
}
