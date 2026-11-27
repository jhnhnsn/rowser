//! `rowser doctor`: on the Local machine, checks the config file, its
//! browser Targets and the Daemon; on a Remote host, checks the setup and
//! does a live round trip.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::browsers::{Browsers, Roots};
use crate::config::{Config, Target};
use crate::control::{request_async, RequestError};
use crate::discovery::find_socket;
use crate::platform::{find_on_path, real_xdg_open, BROWSER_NAMES, ENVIRONMENT_D_MARK};
use crate::protocol::{Hello, LinkInfo, LinkState, Msg};

/// The result of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

/// One reported item: a status and a line of detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub status: Status,
    pub name: String,
    pub detail: String,
}

impl Check {
    fn new(status: Status, name: impl Into<String>, detail: impl Into<String>) -> Check {
        Check {
            status,
            name: name.into(),
            detail: detail.into(),
        }
    }
}

/// What `rowser doctor` reads from its environment.
pub struct DoctorEnv<W: Write> {
    /// `PATH`.
    pub path: Option<OsString>,
    /// `BROWSER`.
    pub browser: Option<OsString>,
    /// `HOME`.
    pub home: Option<PathBuf>,
    /// The Outpost socket, used when `ROWSER_SOCKET` is not set.
    pub outpost_socket: Option<PathBuf>,
    /// `ROWSER_SOCKET`, used in place of the Outpost socket.
    pub socket_override: Option<PathBuf>,
    /// `WAYLAND_DISPLAY` or `DISPLAY` is set.
    pub graphical: bool,
    /// This rowser, which the fallback `xdg-open` must not be.
    pub self_exe: Option<PathBuf>,
    /// The config file (`config::path`).
    pub config_file: Result<PathBuf, String>,
    /// The Daemon's runtime directory (`platform::runtime_dir`).
    pub runtime_dir: Result<PathBuf, String>,
    /// The service file `rowser daemon install` writes
    /// (`service::service_file`).
    pub service_file: Result<PathBuf, String>,
    /// Where to look for the browsers that Route Targets name.
    pub browser_roots: Roots,
    /// `SSH_CONNECTION` is set: this is a Remote host.
    pub ssh: bool,
    /// How long each `ssh` login check may take (`SSH_TIMEOUT`).
    pub ssh_timeout: Duration,
    pub stdout: W,
}

impl DoctorEnv<std::io::Stdout> {
    /// The environment of the running process.
    pub fn from_process() -> DoctorEnv<std::io::Stdout> {
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        DoctorEnv {
            path: std::env::var_os("PATH"),
            browser: std::env::var_os("BROWSER"),
            graphical: ["WAYLAND_DISPLAY", "DISPLAY"]
                .iter()
                .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty())),
            self_exe: std::env::current_exe().ok(),
            home,
            outpost_socket: Some(crate::outpost::socket_path(&crate::outpost::outpost_dir())),
            socket_override: std::env::var_os("ROWSER_SOCKET")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            config_file: crate::config::path(),
            runtime_dir: crate::platform::runtime_dir(),
            service_file: crate::service::service_file(),
            browser_roots: Roots::from_env(),
            ssh: std::env::var_os("SSH_CONNECTION").is_some_and(|v| !v.is_empty()),
            ssh_timeout: SSH_TIMEOUT,
            stdout: std::io::stdout(),
        }
    }
}

/// `~/.local/bin` must come before `/usr/bin` on `PATH`, so the symlinks
/// win over the system `xdg-open`.
pub fn check_path(path: Option<&OsStr>, home: &Path) -> Check {
    const NAME: &str = "PATH";
    let local = home.join(".local/bin");
    let entries: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let local_at = entries.iter().position(|e| *e == local);
    let usr_at = entries.iter().position(|e| e == Path::new("/usr/bin"));
    match (local_at, usr_at) {
        (None, _) => Check::new(
            Status::Warn,
            NAME,
            format!("{} is not on PATH", local.display()),
        ),
        (Some(l), Some(u)) if u < l => Check::new(
            Status::Warn,
            NAME,
            format!("{} comes after /usr/bin", local.display()),
        ),
        _ => Check::new(
            Status::Ok,
            NAME,
            format!("{} is before /usr/bin", local.display()),
        ),
    }
}

/// True when `path` resolves to a file named `rowser`.
fn is_rowser(path: &Path) -> bool {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n == "rowser"))
        .unwrap_or(false)
}

/// The browser symlinks in `~/.local/bin` must resolve to rowser.
pub fn check_symlinks(home: &Path) -> Check {
    const NAME: &str = "symlinks";
    let bin = home.join(".local/bin");
    let bad: Vec<&str> = BROWSER_NAMES
        .iter()
        .copied()
        .filter(|n| !is_rowser(&bin.join(n)))
        .collect();
    if bad.is_empty() {
        Check::new(
            Status::Ok,
            NAME,
            format!("{} resolve to rowser", BROWSER_NAMES.join(", ")),
        )
    } else {
        Check::new(
            Status::Warn,
            NAME,
            format!(
                "{} in {} do not resolve to rowser",
                bad.join(", "),
                bin.display()
            ),
        )
    }
}

/// `BROWSER` must name rowser (or one of its symlinks), directly or
/// through `PATH`.
pub fn check_browser(browser: Option<&OsStr>, path: Option<&OsStr>) -> Check {
    const NAME: &str = "BROWSER";
    let Some(value) = browser.filter(|v| !v.is_empty()) else {
        return Check::new(Status::Warn, NAME, "BROWSER is not set");
    };
    let shown = value.to_string_lossy();
    let command = Path::new(value);
    let resolves = if command.components().count() > 1 {
        is_rowser(command)
    } else {
        // A shell runs only the first executable match, so test only that one.
        path.and_then(|p| find_on_path(p, &value.to_string_lossy(), |_| true))
            .is_some_and(|f| is_rowser(&f))
    };
    if resolves {
        Check::new(Status::Ok, NAME, format!("BROWSER={shown} is rowser"))
    } else {
        Check::new(
            Status::Warn,
            NAME,
            format!("BROWSER={shown} does not resolve to rowser"),
        )
    }
}

/// The fix for each default-browser warning, appended to its detail.
pub const FIX: &str = "; fix: run `rowser remote install <host>` on the Local machine";

fn warn_fix(name: &'static str, detail: String) -> Check {
    Check::new(Status::Warn, name, format!("{detail}{FIX}"))
}

/// The value of the last `key=` line in an `environment.d` file, unquoted.
fn env_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
        .next_back()
        .map(|v| {
            let v = v.trim();
            v.strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v)
        })
}

/// True when the `PATH` value starts with the `.local/bin` of `home`.
fn path_value_ok(value: &str, home: &Path) -> bool {
    let first = value.split(':').next().unwrap_or("");
    ["$HOME/.local/bin", "${HOME}/.local/bin", "~/.local/bin"].contains(&first)
        || Path::new(first) == home.join(".local/bin")
}

/// `~/.config/environment.d/rowser.conf` must set `BROWSER=rowser` and put
/// `~/.local/bin` first on `PATH`, so systemd user services get rowser too.
pub fn check_environment_d(home: &Path) -> Check {
    const NAME: &str = "environment.d";
    let f = home.join(".config/environment.d/rowser.conf");
    let Ok(text) = std::fs::read_to_string(&f) else {
        return warn_fix(NAME, format!("{} does not exist", f.display()));
    };
    let mut bad = vec![];
    match env_value(&text, "BROWSER") {
        Some("rowser") => {}
        Some(v) => bad.push(format!("sets BROWSER={v}, not rowser")),
        None => bad.push("does not set BROWSER".to_string()),
    }
    match env_value(&text, "PATH") {
        Some(v) if path_value_ok(v, home) => {}
        Some(v) => bad.push(format!("sets PATH={v}, without ~/.local/bin first")),
        None => bad.push("does not set PATH".to_string()),
    }
    if bad.is_empty() {
        return Check::new(
            Status::Ok,
            NAME,
            format!(
                "{} sets BROWSER=rowser and ~/.local/bin first on PATH",
                f.display()
            ),
        );
    }
    let detail = format!("{} {}", f.display(), bad.join(" and "));
    if text.lines().next() == Some(ENVIRONMENT_D_MARK) {
        warn_fix(NAME, detail)
    } else {
        // Install keeps a file it did not write, so tell the user what to add.
        Check::new(
            Status::Warn,
            NAME,
            format!(
                "{detail}; fix: add BROWSER=rowser and PATH=$HOME/.local/bin:$PATH \
                 to it (rowser did not write it and keeps it)"
            ),
        )
    }
}

/// `~/.local/share/applications/rowser.desktop` must exist.
pub fn check_desktop(home: &Path) -> Check {
    const NAME: &str = "rowser.desktop";
    let f = home.join(".local/share/applications/rowser.desktop");
    if f.is_file() {
        Check::new(Status::Ok, NAME, format!("{} exists", f.display()))
    } else {
        warn_fix(NAME, format!("{} does not exist", f.display()))
    }
}

/// The URL types rowser must be the default handler for.
const URL_TYPES: [&str; 2] = ["x-scheme-handler/http", "x-scheme-handler/https"];

/// The first handler for `mime` under `[Default Applications]` in a `mimeapps.list`.
fn mimeapps_default(text: &str, mime: &str) -> Option<String> {
    let mut in_defaults = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_defaults = line == "[Default Applications]";
        } else if in_defaults {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == mime {
                    return v
                        .split(';')
                        .map(str::trim)
                        .find(|s| !s.is_empty())
                        .map(String::from);
                }
            }
        }
    }
    None
}

/// `xdg-mime query default <mime>`, or `None` when it fails or prints nothing.
fn query_xdg_mime(prog: &Path, path: Option<&OsStr>, mime: &str) -> Option<String> {
    let mut cmd = std::process::Command::new(prog);
    cmd.args(["query", "default", mime]);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    cmd.stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The default handler for `http` and `https` must be `rowser.desktop`:
/// from `xdg-mime query default` when `xdg-mime` is on `path`, else from
/// `~/.config/mimeapps.list`. One `ok` line, or one warning for each type
/// that fails.
pub fn check_default_handler(path: Option<&OsStr>, home: &Path) -> Vec<Check> {
    const NAME: &str = "default browser";
    let xdg_mime = path.and_then(|p| find_on_path(p, "xdg-mime", |_| true));
    let m = home.join(".config/mimeapps.list");
    let symlink = xdg_mime.is_none() && m.is_symlink();
    let (source, found): (String, Vec<Option<String>>) = match &xdg_mime {
        Some(prog) => (
            "xdg-mime".to_string(),
            URL_TYPES
                .iter()
                .map(|t| query_xdg_mime(prog, path, t))
                .collect(),
        ),
        None => {
            let text = std::fs::read_to_string(&m).unwrap_or_default();
            (
                m.display().to_string(),
                URL_TYPES
                    .iter()
                    .map(|t| mimeapps_default(&text, t))
                    .collect(),
            )
        }
    };
    let warn = |detail: String| {
        if symlink {
            // Install keeps a symlinked mimeapps.list, so tell the user what to add.
            Check::new(
                Status::Warn,
                NAME,
                format!(
                    "{detail}; fix: add x-scheme-handler/http=rowser.desktop and \
                     x-scheme-handler/https=rowser.desktop under [Default Applications] \
                     in {} (a symlink, which rowser keeps)",
                    m.display()
                ),
            )
        } else {
            warn_fix(NAME, detail)
        }
    };
    let warnings: Vec<Check> = URL_TYPES
        .iter()
        .zip(&found)
        .filter_map(|(t, f)| match f.as_deref() {
            Some("rowser.desktop") => None,
            Some(other) => Some(warn(format!(
                "{t} opens {other}, not rowser.desktop ({source})"
            ))),
            None => Some(warn(format!("no default for {t} ({source})"))),
        })
        .collect();
    if warnings.is_empty() {
        vec![Check::new(
            Status::Ok,
            NAME,
            format!("http and https open rowser.desktop ({source})"),
        )]
    } else {
        warnings
    }
}

/// With a graphical session, a real `xdg-open` (not rowser) must be on
/// `path`, for the Shim's local fallback.
pub fn check_fallback(graphical: bool, path: Option<&OsStr>, self_exe: Option<&Path>) -> Check {
    const NAME: &str = "fallback";
    if !graphical {
        return Check::new(Status::Ok, NAME, "no graphical session; no fallback needed");
    }
    match path.and_then(|p| real_xdg_open(p, self_exe)) {
        Some(real) => Check::new(
            Status::Ok,
            NAME,
            format!("{} opens URLs when no Link answers", real.display()),
        ),
        None => Check::new(
            Status::Warn,
            NAME,
            "graphical session but no real xdg-open on PATH; \
             fix: install the `xdg-utils` package (e.g. `sudo apt install xdg-utils`)",
        ),
    }
}

/// Connects to `socket` (the Shim's socket) and sends `Ping` through the
/// Link; reports the Daemon's version and any version skew.
pub async fn check_link(socket: Option<PathBuf>) -> Vec<Check> {
    const NAME: &str = "link";
    let mut conn = match find_socket(socket.as_deref()).await {
        Ok(conn) => conn,
        Err(e) => {
            // A mismatch means a socket answers; its message names both
            // versions and the update command, so the "no Link" hint is wrong.
            return match e.mismatch {
                Some(m) => vec![Check::new(Status::Fail, "version", m.to_string())],
                None => vec![Check::new(
                    Status::Fail,
                    NAME,
                    "no Link to your Local machine; run \"rowser host add <host>\" there, \
                     or check \"rowser host list\"",
                )],
            };
        }
    };
    // find_socket succeeded, so there is a path.
    let socket = socket.unwrap_or_default();
    let fail = |detail: String| vec![Check::new(Status::Fail, NAME, detail)];
    if let Err(e) = conn.send(&Msg::Ping).await {
        return fail(format!("{}: {e}", socket.display()));
    }
    let (version, rowser_version) = match conn.recv().await {
        Ok(Some(Msg::Pong {
            version,
            rowser_version,
        })) => (version, rowser_version),
        Ok(Some(other)) => return fail(format!("unexpected reply from daemon: {other:?}")),
        Ok(None) => return fail("daemon closed the connection".into()),
        Err(e) => return fail(format!("{}: {e}", socket.display())),
    };
    let ours = Hello::current().version();
    let daemon = match rowser_version {
        Some(v) => format!("daemon rowser {v} (protocol {version})"),
        None => format!("daemon protocol {version}"),
    };
    let mut out = vec![Check::new(
        Status::Ok,
        NAME,
        format!("{}: {daemon}", socket.display()),
    )];
    if version == ours {
        out.push(Check::new(
            Status::Ok,
            "version",
            format!("daemon and this rowser speak {ours}"),
        ));
    } else {
        out.push(Check::new(
            Status::Warn,
            "version",
            format!(
                "version skew: daemon speaks {version}, this rowser speaks {ours}; \
                 update with `rowser remote install <host>` or reinstall rowser locally"
            ),
        ));
    }
    out
}

/// The config file must parse. Gives the Config (empty when there is no
/// file) when it does.
pub fn check_config(file: &Result<PathBuf, String>) -> (Check, Option<Config>) {
    const NAME: &str = "config";
    let f = match file {
        Ok(f) => f,
        Err(e) => return (Check::new(Status::Fail, NAME, e.clone()), None),
    };
    if !f.exists() {
        return (
            Check::new(
                Status::Ok,
                NAME,
                format!("no file at {}; every request opens in system", f.display()),
            ),
            Some(Config::default()),
        );
    }
    match crate::config::load(f) {
        Ok(c) => {
            let n = c.routes.len();
            let detail = format!(
                "{} parses; {n} route{}",
                f.display(),
                if n == 1 { "" } else { "s" }
            );
            (Check::new(Status::Ok, NAME, detail), Some(c))
        }
        Err(e) => (Check::new(Status::Fail, NAME, e), None),
    }
}

/// Each browser Target in `config` must name a browser and Profile that
/// `rowser browsers` finds. One warning for each Target that does not.
pub fn check_routes(config: &Config, roots: &Roots) -> Vec<Check> {
    const NAME: &str = "routes";
    let mut targets: Vec<(String, &Target)> = config
        .routes
        .iter()
        .enumerate()
        .map(|(i, r)| (format!("route {} (host {})", i + 1, r.host), &r.target))
        .collect();
    if let Some(t) = &config.default {
        targets.push(("[default]".into(), t));
    }
    targets.retain(|(_, t)| matches!(t, Target::Browser { .. }));
    if targets.is_empty() {
        return vec![Check::new(Status::Ok, NAME, "no browser Targets")];
    }
    let browsers = Browsers::scan(roots);
    let warnings: Vec<Check> = targets
        .iter()
        .filter_map(|(label, t)| {
            browsers.normalize(t).err().map(|e| {
                // A missing [default] opens with `system` (Routes::resolve).
                let next = if label == "[default]" {
                    "`system`"
                } else {
                    "[default]"
                };
                Check::new(
                    Status::Warn,
                    NAME,
                    format!("{label}: {t}: {e}; the request falls back to {next}"),
                )
            })
        })
        .collect();
    if warnings.is_empty() {
        vec![Check::new(
            Status::Ok,
            NAME,
            "every browser Target is found",
        )]
    } else {
        warnings
    }
}

/// The Daemon must answer `status` on the control socket. Gives its Links
/// when it does.
pub async fn check_daemon(runtime_dir: &Result<PathBuf, String>) -> (Check, Option<Vec<LinkInfo>>) {
    const NAME: &str = "daemon";
    let dir = match runtime_dir {
        Ok(d) => d,
        Err(e) => return (Check::new(Status::Warn, NAME, e.clone()), None),
    };
    match request_async(dir, &Msg::Status).await {
        Ok(Msg::StatusReply {
            daemon_version,
            protocol_version,
            links,
            ..
        }) => (
            Check::new(
                Status::Ok,
                NAME,
                format!(
                    "rowser {daemon_version} answers on {} (protocol {protocol_version})",
                    dir.display()
                ),
            ),
            Some(links),
        ),
        Ok(other) => (
            Check::new(
                Status::Fail,
                NAME,
                format!("unexpected reply from daemon: {other:?}"),
            ),
            None,
        ),
        Err(RequestError::NotRunning) => (
            Check::new(
                Status::Warn,
                NAME,
                "the daemon is not running; start it with `rowser daemon`",
            ),
            None,
        ),
        Err(RequestError::Other(e)) => (Check::new(Status::Fail, NAME, e), None),
    }
}

/// The Link state of the Saved host `host`, from the Daemon's `links`;
/// `None` when the Daemon did not answer.
pub fn check_link_state(host: &str, links: Option<&[LinkInfo]>) -> Check {
    let Some(links) = links else {
        return Check::new(
            Status::Warn,
            format!("host {host}"),
            "no Link; the daemon is not running",
        );
    };
    let (status, detail) = match links.iter().find(|l| l.host == host).map(|l| l.state) {
        Some(LinkState::Connected) => (Status::Ok, "the Link is connected".to_string()),
        Some(LinkState::Connecting) => (Status::Warn, "the Link is connecting".to_string()),
        Some(LinkState::NeedsLogin) => (
            Status::Warn,
            format!("the Link needs login; run `rowser host connect {host}`"),
        ),
        Some(LinkState::VersionMismatch) => (
            Status::Warn,
            format!("the Link has a version mismatch; run `rowser remote install {host}`"),
        ),
        None => (
            Status::Warn,
            format!("the daemon has no Link to {host}; run `rowser reload`"),
        ),
    };
    Check::new(status, format!("host {host}"), detail)
}

/// `ssh -o BatchMode=yes <host> true` must succeed: the Link logs in the
/// same way. `ssh` comes from `path`, with `-o ConnectTimeout=10` too; it
/// fails with "no answer" when it runs longer than `timeout`.
pub fn check_ssh_login(host: &str, path: Option<&OsStr>, timeout: Duration) -> Check {
    let name = format!("host {host}");
    let shown = format!("`ssh -o BatchMode=yes {host} true`");
    let Some(ssh) = path.and_then(|p| find_on_path(p, "ssh", |_| true)) else {
        return Check::new(
            Status::Fail,
            name,
            format!("cannot run {shown}: ssh is not on PATH"),
        );
    };
    let child = std::process::Command::new(ssh)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            host,
            "true",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Check::new(Status::Fail, name, format!("cannot run {shown}: {e}")),
    };
    // Read stderr on its own thread, so a full pipe cannot stop ssh.
    let stderr = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
            buf
        })
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Check::new(
                    Status::Fail,
                    name,
                    format!("{shown} failed: no answer in {}s", timeout.as_secs()),
                );
            }
            Err(e) => return Check::new(Status::Fail, name, format!("cannot run {shown}: {e}")),
        }
    };
    if status.success() {
        return Check::new(Status::Ok, name, format!("{shown} succeeds"));
    }
    let err = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    let err = String::from_utf8_lossy(&err);
    let last = err.lines().map(str::trim).rfind(|l| !l.is_empty());
    let detail = match last {
        Some(l) => format!("{shown} failed ({status}): {l}"),
        None => format!("{shown} failed ({status})"),
    };
    Check::new(Status::Fail, name, crate::protocol::printable(&detail))
}

/// How long `rowser doctor` waits for each `ssh` login check.
pub const SSH_TIMEOUT: Duration = Duration::from_secs(15);

/// An installed service file must not have the removed
/// `--confirm-unverified`. `None` when no service is installed.
pub fn check_service(file: &Result<PathBuf, String>) -> Option<Check> {
    const NAME: &str = "service";
    let f = file.as_ref().ok()?;
    let text = std::fs::read_to_string(f).ok()?;
    Some(if text.contains(crate::service::REMOVED_FLAG) {
        Check::new(
            Status::Warn,
            NAME,
            format!(
                "{} still has {}, which is removed; the daemon does not start with it; \
                 run `rowser daemon install` again",
                f.display(),
                crate::service::REMOVED_FLAG
            ),
        )
    } else {
        Check::new(Status::Ok, NAME, f.display().to_string())
    })
}

/// The socket the Shim uses: `ROWSER_SOCKET`, else the Outpost socket.
fn shim_socket<W: Write>(env: &DoctorEnv<W>) -> Option<PathBuf> {
    env.socket_override
        .clone()
        .or_else(|| env.outpost_socket.clone())
}

/// Runs the Local machine checks, then the Remote host checks, as this
/// machine needs; prints one line each, and exits 1 when any fails.
///
/// Local: a config file exists, or the Daemon runtime directory exists.
/// Remote: the Shim's socket exists or `SSH_CONNECTION` is set. Neither:
/// Remote.
pub async fn run_doctor<W: Write>(env: &mut DoctorEnv<W>) -> ExitCode {
    let local = env.config_file.as_ref().is_ok_and(|f| f.exists())
        || env.runtime_dir.as_ref().is_ok_and(|d| d.is_dir());
    let remote = env.ssh || shim_socket(env).is_some_and(|p| p.exists()) || !local;
    let mut checks = vec![];
    if local {
        let (c, config) = check_config(&env.config_file);
        checks.push(c);
        if let Some(config) = &config {
            if config.has_unverified {
                checks.push(Check::new(
                    Status::Warn,
                    "config",
                    crate::config::UNVERIFIED_IGNORED,
                ));
            }
            checks.extend(check_routes(config, &env.browser_roots));
        }
        let (c, links) = check_daemon(&env.runtime_dir).await;
        checks.push(c);
        checks.extend(check_service(&env.service_file));
        // One thread for each Saved host: each ssh check can take up to
        // `ssh_timeout`.
        let (path, timeout) = (env.path.as_deref(), env.ssh_timeout);
        let logins: Vec<Check> = std::thread::scope(|s| {
            let threads: Vec<_> = config
                .iter()
                .flat_map(|c| &c.hosts)
                .map(|h| s.spawn(move || check_ssh_login(&h.name, path, timeout)))
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().expect("ssh check thread"))
                .collect()
        });
        for (h, login) in config.iter().flat_map(|c| &c.hosts).zip(logins) {
            checks.push(check_link_state(&h.name, links.as_deref()));
            checks.push(login);
        }
    }
    if remote {
        remote_checks(env, &mut checks).await;
    }
    for c in &checks {
        let _ = writeln!(
            env.stdout,
            "{:<4} {}: {}",
            c.status.label(),
            c.name,
            c.detail
        );
    }
    if checks.iter().any(|c| c.status == Status::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Runs the Remote host checks: PATH, Shim symlinks, `BROWSER` and the rest.
async fn remote_checks<W: Write>(env: &DoctorEnv<W>, checks: &mut Vec<Check>) {
    let marker = env
        .home
        .as_ref()
        .map(|h| h.join(crate::remote_install::NO_DEFAULT_BROWSER_MARKER))
        .filter(|m| m.exists());
    match marker {
        Some(m) => checks.push(Check::new(
            Status::Ok,
            "default browser",
            format!(
                "not checked: {} says rowser was installed with --no-default-browser",
                m.display()
            ),
        )),
        None => default_browser_checks(env, checks),
    }
    checks.push(check_fallback(
        env.graphical,
        env.path.as_deref(),
        env.self_exe.as_deref(),
    ));
    // The Outpost socket path comes from the UID alone, so check it even
    // when ROWSER_SOCKET is set.
    if let Some(c) = env.outpost_socket.as_deref().and_then(check_outpost_socket) {
        checks.push(c);
    }
    // Like the Shim: an existing Outpost socket that fails its owner check
    // is not used, so the `link` check must not connect to it.
    let refused = match (&env.socket_override, &env.outpost_socket) {
        (None, Some(p)) if p.symlink_metadata().is_ok() => crate::outpost::check_socket(p)
            .err()
            .map(|e| format!("{e}; the Shim does not use {}", p.display())),
        _ => None,
    };
    match refused {
        Some(detail) => checks.push(Check::new(Status::Fail, "link", detail)),
        None => checks.extend(check_link(shim_socket(env)).await),
    }
}

/// The default-browser part: PATH, the browser symlinks, `BROWSER`, and
/// each part of Default browser on a Remote host.
fn default_browser_checks<W: Write>(env: &DoctorEnv<W>, checks: &mut Vec<Check>) {
    match &env.home {
        Some(home) => {
            checks.push(check_path(env.path.as_deref(), home));
            checks.push(check_symlinks(home));
        }
        None => checks.push(Check::new(
            Status::Warn,
            "PATH",
            "HOME is not set; cannot check ~/.local/bin",
        )),
    }
    checks.push(check_browser(env.browser.as_deref(), env.path.as_deref()));
    match &env.home {
        Some(home) => {
            checks.push(check_environment_d(home));
            checks.push(check_desktop(home));
            checks.extend(check_default_handler(env.path.as_deref(), home));
        }
        None => checks.push(Check::new(
            Status::Warn,
            "default browser",
            "HOME is not set; cannot check the default browser",
        )),
    }
}

/// The Outpost socket at `path`, when its folder exists: the folder must
/// belong to this user with mode 0700 (as the Shim checks), and the socket
/// must have mode 0600. `None` for a missing folder, or a good folder with
/// no socket: the `link` check reports those.
pub fn check_outpost_socket(path: &Path) -> Option<Check> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    const NAME: &str = "outpost socket";
    let dir = path.parent()?;
    let dir_meta = std::fs::symlink_metadata(dir).ok()?;
    let sock_meta = std::fs::symlink_metadata(path).ok();
    let checked = match sock_meta {
        Some(_) => crate::outpost::check_socket(path),
        None => crate::outpost::check_socket_dir(dir),
    };
    if let Err(e) = checked {
        let mut detail = e;
        if sock_meta.is_some() {
            detail.push_str("; the Shim does not use this socket");
        }
        let dir_mode = dir_meta.permissions().mode() & 0o777;
        if dir_meta.is_dir() && dir_meta.uid() == crate::outpost::euid() && dir_mode != 0o700 {
            detail.push_str(&format!("; fix: chmod 700 {}", dir.display()));
        }
        return Some(Check::new(Status::Fail, NAME, detail));
    }
    let meta = sock_meta?;
    let mode = meta.permissions().mode() & 0o777;
    let dir_mode = path
        .parent()
        .and_then(|d| std::fs::metadata(d).ok())
        .map_or(0, |m| m.permissions().mode() & 0o777);
    Some(if mode == 0o600 {
        Check::new(
            Status::Ok,
            NAME,
            format!(
                "{} belongs to you, mode 0600, in a folder with mode {dir_mode:04o}",
                path.display()
            ),
        )
    } else {
        Check::new(
            Status::Fail,
            NAME,
            format!("{} has mode {mode:04o}, not 0600", path.display()),
        )
    })
}

/// `rowser doctor` for the binary.
pub fn doctor_main() -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(run_doctor(&mut DoctorEnv::from_process())),
        Err(e) => {
            eprintln!("rowser: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/u";

    fn path(p: &str) -> Check {
        check_path(Some(p.as_ref()), Path::new(HOME))
    }

    #[test]
    fn path_with_local_bin_first_is_ok() {
        assert_eq!(path("/home/u/.local/bin:/usr/bin").status, Status::Ok);
    }

    #[test]
    fn path_without_usr_bin_is_ok() {
        assert_eq!(path("/bin:/home/u/.local/bin").status, Status::Ok);
    }

    #[test]
    fn path_with_local_bin_after_usr_bin_warns() {
        let c = path("/usr/bin:/home/u/.local/bin");
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.detail, "/home/u/.local/bin comes after /usr/bin");
    }

    #[test]
    fn path_without_local_bin_warns() {
        let c = path("/usr/local/bin:/usr/bin");
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.detail, "/home/u/.local/bin is not on PATH");
        assert_eq!(check_path(None, Path::new(HOME)).status, Status::Warn);
    }

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A home with `~/.local/bin/rowser` and, when `links`, its symlinks.
    /// A counter makes each path unique, so tags that differ only in case
    /// (which a case-insensitive file system folds) never share a folder.
    fn home(tag: &str, links: bool) -> Tmp {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/rw.doc{tag}-{n}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("rowser"), "").unwrap();
        std::fs::set_permissions(
            bin.join("rowser"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        if links {
            for n in BROWSER_NAMES {
                std::os::unix::fs::symlink("rowser", bin.join(n)).unwrap();
            }
        }
        Tmp(dir)
    }

    #[test]
    fn homes_do_not_collide_on_a_case_insensitive_fs() {
        // macOS folds case: "/tmp/rw.docE.1" and "/tmp/rw.doce.1" are one folder.
        let a = home("q", false);
        let b = home("Q", false);
        let fold = |t: &Tmp| t.0.to_string_lossy().to_lowercase();
        assert_ne!(fold(&a), fold(&b));
    }

    #[test]
    fn symlinks_to_rowser_are_ok() {
        let h = home("s", true);
        assert_eq!(check_symlinks(&h.0).status, Status::Ok);
    }

    #[test]
    fn missing_symlinks_warn_by_name() {
        let h = home("m", false);
        let bin = h.0.join(".local/bin");
        std::os::unix::fs::symlink("rowser", bin.join("xdg-open")).unwrap();
        std::os::unix::fs::symlink("/bin/true", bin.join("www-browser")).unwrap();
        let c = check_symlinks(&h.0);
        assert_eq!(c.status, Status::Warn);
        assert!(
            c.detail.starts_with("sensible-browser, www-browser in "),
            "{}",
            c.detail
        );
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn env_d(h: &Tmp, text: &str) -> Check {
        write(&h.0.join(".config/environment.d/rowser.conf"), text);
        check_environment_d(&h.0)
    }

    const MARK: &str = "# Written by rowser remote install; rowser overwrites this file.\n";

    #[test]
    fn environment_d_setting_browser_and_path_is_ok() {
        let h = home("e", false);
        let ok = |text: &str| assert_eq!(env_d(&h, text).status, Status::Ok, "{text}");
        ok("# x\nBROWSER=rowser\nPATH=$HOME/.local/bin:$PATH\n");
        ok("BROWSER=\"rowser\"\nPATH=~/.local/bin\n");
        ok(&format!(
            "BROWSER=rowser\nPATH={}/.local/bin:/usr/bin\n",
            h.0.display()
        ));
    }

    #[test]
    fn environment_d_missing_or_incomplete_warns_with_the_fix() {
        let h = home("E", false);
        let c = check_environment_d(&h.0);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.ends_with(FIX), "{}", c.detail);
        let c = env_d(&h, &format!("{MARK}PATH=$HOME/.local/bin:$PATH\n"));
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("does not set BROWSER"), "{}", c.detail);
        assert!(c.detail.ends_with(FIX), "{}", c.detail);
        let c = env_d(&h, &format!("{MARK}BROWSER=rowser\n"));
        assert!(c.detail.contains("does not set PATH"), "{}", c.detail);
    }

    #[test]
    fn environment_d_with_wrong_values_warns() {
        let h = home("w", false);
        let c = env_d(
            &h,
            &format!("{MARK}BROWSER=firefox\nPATH=$HOME/.local/bin\n"),
        );
        assert_eq!(c.status, Status::Warn);
        assert!(
            c.detail.contains("sets BROWSER=firefox, not rowser"),
            "{}",
            c.detail
        );
        let c = env_d(
            &h,
            &format!("{MARK}BROWSER=rowser\nPATH=$PATH:$HOME/.local/bin\n"),
        );
        assert_eq!(c.status, Status::Warn);
        assert!(
            c.detail.contains("without ~/.local/bin first"),
            "{}",
            c.detail
        );
        let c = env_d(
            &h,
            &format!("{MARK}BROWSER=rowser\nPATH=/other/.local/bin\n"),
        );
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
    }

    #[test]
    fn environment_d_the_user_wrote_names_the_lines_to_add() {
        let h = home("u", false);
        let c = env_d(&h, "BROWSER=firefox\n");
        assert_eq!(c.status, Status::Warn);
        assert!(!c.detail.ends_with(FIX), "{}", c.detail);
        assert!(
            c.detail
                .contains("fix: add BROWSER=rowser and PATH=$HOME/.local/bin:$PATH to it"),
            "{}",
            c.detail
        );
    }

    #[test]
    fn desktop_file_present_is_ok_and_missing_warns() {
        let h = home("d", false);
        let c = check_desktop(&h.0);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.ends_with(FIX), "{}", c.detail);
        write(
            &h.0.join(".local/share/applications/rowser.desktop"),
            "[Desktop Entry]\n",
        );
        assert_eq!(check_desktop(&h.0).status, Status::Ok);
    }

    /// A `bin` dir in `h` with a fake `xdg-mime` whose body is `body`. The
    /// script cannot be busy when it runs (see #55, ETXTBSY).
    fn fake_xdg_mime(h: &Tmp, body: &str) -> OsString {
        let bin = h.0.join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();
        crate::test_support::write_script(&bin.join("xdg-mime"), body);
        bin.into_os_string()
    }

    fn details(checks: &[Check]) -> Vec<(Status, &str)> {
        checks
            .iter()
            .map(|c| (c.status, c.detail.as_str()))
            .collect()
    }

    #[test]
    fn default_handler_from_xdg_mime() {
        let h = home("x", false);
        let p = fake_xdg_mime(&h, "echo rowser.desktop");
        let c = check_default_handler(Some(&p), &h.0);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].status, Status::Ok, "{}", c[0].detail);
        let h = home("X", false);
        let p = fake_xdg_mime(&h, "echo firefox.desktop");
        let c = check_default_handler(Some(&p), &h.0);
        assert_eq!(c.len(), 2, "{:?}", details(&c));
        for (c, t) in c.iter().zip(URL_TYPES) {
            assert_eq!(c.status, Status::Warn);
            assert!(
                c.detail.starts_with(&format!("{t} opens firefox.desktop")),
                "{}",
                c.detail
            );
            assert!(c.detail.ends_with(FIX), "{}", c.detail);
        }
    }

    #[test]
    fn xdg_mime_with_http_on_another_handler_warns_for_http_only() {
        let h = home("H", false);
        let p = fake_xdg_mime(
            &h,
            "case \"$3\" in *https) echo rowser.desktop;; *) echo firefox.desktop;; esac",
        );
        let c = check_default_handler(Some(&p), &h.0);
        assert_eq!(c.len(), 1, "{:?}", details(&c));
        assert_eq!(c[0].status, Status::Warn);
        assert!(
            c[0].detail
                .starts_with("x-scheme-handler/http opens firefox.desktop"),
            "{}",
            c[0].detail
        );
    }

    #[test]
    fn xdg_mime_that_fails_or_prints_nothing_warns() {
        for (tag, body) in [("1", "exit 1"), ("0", "true")] {
            let h = home(tag, false);
            let p = fake_xdg_mime(&h, body);
            let c = check_default_handler(Some(&p), &h.0);
            assert_eq!(c.len(), 2, "{:?}", details(&c));
            assert!(
                c[1].detail
                    .starts_with("no default for x-scheme-handler/https (xdg-mime)"),
                "{}",
                c[1].detail
            );
        }
    }

    #[test]
    fn default_handler_from_mimeapps_list_without_xdg_mime() {
        let h = home("l", false);
        let empty = h.0.join("nobin").into_os_string();
        let c = check_default_handler(Some(&empty), &h.0);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].status, Status::Warn);
        let m = h.0.join(".config/mimeapps.list");
        write(
            &m,
            "[Added Associations]\nx-scheme-handler/https=rowser.desktop;\n\
             [Default Applications]\nx-scheme-handler/http=rowser.desktop\n\
             x-scheme-handler/https=firefox.desktop;\n",
        );
        let c = check_default_handler(Some(&empty), &h.0);
        assert_eq!(c.len(), 1, "{:?}", details(&c));
        assert!(c[0].detail.contains("firefox.desktop"), "{}", c[0].detail);
        write(
            &m,
            "[Default Applications]\nx-scheme-handler/http=rowser.desktop\n\
             x-scheme-handler/https = rowser.desktop;other.desktop;\n",
        );
        let c = check_default_handler(Some(&empty), &h.0);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].status, Status::Ok, "{}", c[0].detail);
    }

    #[test]
    fn mimeapps_list_with_no_section_warns() {
        let h = home("n", false);
        let empty = h.0.join("nobin").into_os_string();
        write(
            &h.0.join(".config/mimeapps.list"),
            "x-scheme-handler/http=rowser.desktop\nx-scheme-handler/https=rowser.desktop\n",
        );
        let c = check_default_handler(Some(&empty), &h.0);
        assert_eq!(c.len(), 2, "{:?}", details(&c));
        assert!(c.iter().all(|c| c.status == Status::Warn));
    }

    #[test]
    fn symlinked_mimeapps_list_names_the_lines_to_add() {
        let h = home("y", false);
        let empty = h.0.join("nobin").into_os_string();
        let real = h.0.join("dotfiles/mimeapps.list");
        write(&real, "[Default Applications]\n");
        std::fs::create_dir_all(h.0.join(".config")).unwrap();
        std::os::unix::fs::symlink(&real, h.0.join(".config/mimeapps.list")).unwrap();
        let c = check_default_handler(Some(&empty), &h.0);
        assert_eq!(c.len(), 2, "{:?}", details(&c));
        for c in &c {
            assert!(!c.detail.ends_with(FIX), "{}", c.detail);
            assert!(
                c.detail.contains(
                    "fix: add x-scheme-handler/http=rowser.desktop and \
                     x-scheme-handler/https=rowser.desktop under [Default Applications]"
                ),
                "{}",
                c.detail
            );
        }
    }

    #[test]
    fn fallback_needs_a_real_xdg_open_only_with_a_graphical_session() {
        let h = home("g", true);
        let rowser = h.0.join(".local/bin/rowser");
        let p = h.0.join(".local/bin").into_os_string();
        assert_eq!(
            check_fallback(false, Some(&p), Some(&rowser)).status,
            Status::Ok
        );
        let c = check_fallback(true, Some(&p), Some(&rowser));
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("no real xdg-open"), "{}", c.detail);
        let real = h.0.join("usr/bin/xdg-open");
        write(&real, "");
        std::fs::set_permissions(&real, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let p = std::env::join_paths([h.0.join(".local/bin"), h.0.join("usr/bin")]).unwrap();
        let c = check_fallback(true, Some(&p), Some(&rowser));
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        assert!(c.detail.contains(&*real.to_string_lossy()), "{}", c.detail);
    }

    #[test]
    fn unset_or_empty_browser_warns() {
        let c = check_browser(None, None);
        assert_eq!(
            (c.status, c.detail.as_str()),
            (Status::Warn, "BROWSER is not set")
        );
        assert_eq!(check_browser(Some("".as_ref()), None).status, Status::Warn);
    }

    #[test]
    fn browser_found_on_path_is_ok() {
        let h = home("b", true);
        let p = h.0.join(".local/bin").into_os_string();
        assert_eq!(
            check_browser(Some("rowser".as_ref()), Some(&p)).status,
            Status::Ok
        );
        assert_eq!(
            check_browser(Some("xdg-open".as_ref()), Some(&p)).status,
            Status::Ok
        );
    }

    #[test]
    fn browser_shadowed_by_an_earlier_path_entry_warns() {
        let h = home("f", true);
        let other = h.0.join("usr/bin");
        std::fs::create_dir_all(&other).unwrap();
        let fake = other.join("xdg-open");
        std::fs::write(&fake, "").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let p = std::env::join_paths([other, h.0.join(".local/bin")]).unwrap();
        let c = check_browser(Some("xdg-open".as_ref()), Some(&p));
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
    }

    #[test]
    fn browser_as_a_path_to_rowser_is_ok() {
        let h = home("a", true);
        let b = h.0.join(".local/bin/rowser").into_os_string();
        assert_eq!(check_browser(Some(&b), None).status, Status::Ok);
    }

    #[test]
    fn browser_naming_another_program_warns() {
        let h = home("o", true);
        let p = h.0.join(".local/bin").into_os_string();
        let c = check_browser(Some("firefox".as_ref()), Some(&p));
        assert_eq!(c.status, Status::Warn);
        assert_eq!(c.detail, "BROWSER=firefox does not resolve to rowser");
    }
}
