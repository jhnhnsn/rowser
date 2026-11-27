//! `rowser host add|remove|list|connect`: the Saved hosts. The CLI edits
//! the config file itself (ADR 0007) and works without a Daemon; it then
//! asks a running Daemon to reload, or to connect a Link again.
//!
//! `ssh` runs from `PATH`, in the user's terminal, so it can ask for a
//! passphrase or a key touch.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::config::{self, SavedHost};
use crate::control::{self, RequestError};
use crate::protocol::{LinkInfo, Msg};

/// One `rowser host` command.
#[derive(Debug, Clone)]
pub enum HostAction {
    Add { host: String, default_browser: bool },
    Remove { host: String },
    List,
    Connect { host: String },
}

impl HostAction {
    fn name(&self) -> &'static str {
        match self {
            HostAction::Add { .. } => "add",
            HostAction::Remove { .. } => "remove",
            HostAction::List => "list",
            HostAction::Connect { .. } => "connect",
        }
    }
}

/// `rowser host` inputs.
pub struct HostArgs {
    /// The config file (`config::path`).
    pub config_file: Result<PathBuf, String>,
    /// The Daemon's runtime directory: its control socket and the Link
    /// control path.
    pub runtime_dir: Result<PathBuf, String>,
    pub action: HostAction,
}

/// The remote command that finds rowser in the user's login shell.
/// It prints the path, then `rowser <version>`. Only `&&`, which fish has
/// too (fish has no `{ …; }`). The exit status is not read.
pub const FIND_COMMAND: &str = "exec \"$SHELL\" -lc 'command -v rowser && rowser --version'";

/// Runs one `rowser host` command.
pub fn run_host(args: HostArgs, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    let name = args.action.name();
    match run_inner(args, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "rowser host {name}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}

/// A host name that `ssh` would read as an option is refused.
fn check_name(host: &str) -> Result<(), String> {
    if host.trim().is_empty() || host.starts_with('-') {
        return Err(format!("invalid host name `{host}`"));
    }
    Ok(())
}

fn read(file: &Path) -> Result<String, String> {
    match std::fs::read_to_string(file) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

fn run_inner(args: HostArgs, out: &mut impl Write) -> Result<(), String> {
    let file = args.config_file?;
    let in_file = |e: String| format!("{}: {e}", file.display());
    match args.action {
        HostAction::Add {
            host,
            default_browser,
        } => {
            check_name(&host)?;
            let text = read(&file)?;
            // Refuse an invalid file before ssh runs.
            config::parse(&text).map_err(in_file)?;
            let remote_path = ensure_rowser(&host, default_browser, out)?;
            let saved = SavedHost {
                name: host.clone(),
                remote_path: remote_path.clone(),
                auto_update: true,
                default_browser,
            };
            let text = config::add_host(&text, &saved).map_err(in_file)?;
            config::write(&file, &text)?;
            writeln!(out, "rowser: saved host {host} (rowser at {remote_path})").map_err(io)?;
            let note = control::reload_note(args.runtime_dir.as_deref().map_err(String::clone));
            writeln!(out, "{note}").map_err(io)
        }
        HostAction::Remove { host } => {
            let text = config::remove_host(&read(&file)?, &host).map_err(in_file)?;
            config::write(&file, &text)?;
            writeln!(out, "rowser: removed host {host}").map_err(io)?;
            let note = control::reload_note(args.runtime_dir.as_deref().map_err(String::clone));
            writeln!(out, "{note}").map_err(io)
        }
        HostAction::List => {
            let c = config::load(&file)?;
            if c.hosts.is_empty() {
                return writeln!(out, "no Saved hosts").map_err(io);
            }
            let links = args
                .runtime_dir
                .map_err(RequestError::Other)
                .and_then(|d| links(&d));
            for h in &c.hosts {
                let line = match &links {
                    Ok(links) => match links.iter().find(|l| l.host == h.name) {
                        Some(l) => link_line(l),
                        None => format!("{}  not loaded (run rowser reload)", h.name),
                    },
                    Err(RequestError::NotRunning) => format!("{}  daemon not running", h.name),
                    Err(RequestError::Other(e)) => format!("{}  unknown: {e}", h.name),
                };
                writeln!(out, "{line}").map_err(io)?;
            }
            Ok(())
        }
        HostAction::Connect { host } => {
            let c = config::load(&file)?;
            if !c.hosts.iter().any(|h| h.name == host) {
                return Err(format!(
                    "`{host}` is not a Saved host; run \"rowser host add {host}\" first"
                ));
            }
            let dir = args.runtime_dir?;
            let status = Command::new("ssh")
                .args(master_args(&host, &dir))
                .status()
                .map_err(|e| format!("cannot run ssh: {e}"))?;
            if !status.success() {
                return Err(format!("ssh could not log in to {host} ({status})"));
            }
            writeln!(out, "rowser: logged in to {host}").map_err(io)?;
            let note = match control::request(&dir, &Msg::Reconnect { host: host.clone() }) {
                Ok(Msg::Reconnected { .. }) => format!("rowser: the Link to {host} connects again"),
                // The only error: the Daemon has not loaded this Saved host.
                Ok(Msg::OpenResult { error: Some(e), .. }) => {
                    return Err(format!(
                        "{e}: the daemon has not loaded {host}; run \"rowser reload\", then connect again"
                    ))
                }
                Ok(other) => format!("rowser: unexpected reply {other:?}"),
                Err(RequestError::NotRunning) => {
                    "rowser: the daemon is not running; the Link connects when it starts".into()
                }
                Err(e) => e.to_string(),
            };
            writeln!(out, "{note}").map_err(io)
        }
    }
}

/// The `ssh` arguments of `rowser host connect`: a background master on
/// rowser's own control path.
pub fn master_args(host: &str, runtime_dir: &Path) -> Vec<String> {
    vec![
        "-o".into(),
        "ControlMaster=yes".into(),
        "-o".into(),
        "ControlPersist=yes".into(),
        "-o".into(),
        crate::daemon::control_path_option(runtime_dir),
        "-N".into(),
        "-f".into(),
        host.into(),
    ]
}

/// Where an install puts rowser on the Remote host.
const INSTALLED_PATH: &str = "~/.local/bin/rowser";

/// rowser on a Remote host, as [`FIND_COMMAND`] found it.
#[derive(Debug, PartialEq, Eq)]
struct Found {
    path: String,
    /// What `rowser --version` gave (`None`: no answer).
    version: Option<String>,
}

/// Reads the output of [`FIND_COMMAND`]: the path line, then the
/// `rowser <version>` line. A login-shell profile can print lines before
/// them. `None` when there is no path: `command -v` gives a name, not a
/// path, for an alias or a function.
fn parse_found(stdout: &str) -> Option<Found> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let (version, rest) = match lines.split_last() {
        Some((last, rest)) if last.starts_with("rowser ") => {
            (Some(last["rowser ".len()..].trim().to_string()), rest)
        }
        _ => (None, &lines[..]),
    };
    let path = rest.last()?;
    (path.starts_with('/') || path.starts_with('~')).then(|| Found {
        path: path.to_string(),
        version,
    })
}

/// Runs [`FIND_COMMAND`] on `host`. `ssh` keeps the terminal for its
/// prompts; only stdout is read. `Ok(None)`: rowser is not there.
fn find_rowser(host: &str) -> Result<Option<Found>, String> {
    let out = Command::new("ssh")
        .arg(host)
        .arg(FIND_COMMAND)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    if out.status.code() == Some(255) {
        return Err(format!("ssh to {host} failed ({})", out.status));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `command -v` fails when rowser is missing; a failed `--version` still
    // leaves the path.
    Ok(parse_found(&stdout))
}

/// The hidden test hook: a local build for the install of `host add`, in
/// place of the release download.
pub const TEST_LOCAL_BUILD_VAR: &str = "ROWSER_TEST_LOCAL_BUILD";

/// The binary source of an install from this machine: the release of this
/// version (or the test hook's local build).
pub fn install_source() -> Box<dyn crate::remote_install::BinarySource + Send + Sync> {
    let local = std::env::var_os(TEST_LOCAL_BUILD_VAR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    crate::remote_install::source_for(local, None)
}

/// The `remote_path` for `host`: the found rowser when its version is this
/// one, else the path of a new install.
fn ensure_rowser(
    host: &str,
    default_browser: bool,
    out: &mut impl Write,
) -> Result<String, String> {
    let found = find_rowser(host)?;
    let local = env!("CARGO_PKG_VERSION");
    if let Some(f) = &found {
        if f.version.as_deref() == Some(local) {
            return Ok(f.path.clone());
        }
    }
    let why = match &found {
        None => "rowser is not installed".to_string(),
        Some(Found {
            version: Some(v), ..
        }) => format!("rowser {v} is installed, not {local}"),
        Some(_) => "the installed rowser gives no version".to_string(),
    };
    writeln!(out, "rowser: {why} on {host}; installing rowser {local}").map_err(io)?;
    let source = install_source();
    let installed = crate::remote_install::install(host, source.as_ref(), default_browser)
        .map_err(|e| format!("install on {host} failed: {e}"))?;
    crate::remote_install::write_report(out, host, default_browser, &installed).map_err(io)?;
    Ok(INSTALLED_PATH.into())
}

/// `g14  connected  since <time>  last connected <time>  outpost 1.0.0`: the
/// Outpost's rowser version.
fn link_line(l: &LinkInfo) -> String {
    let last = l.last_connected.map_or("never".into(), control::local_time);
    let version = l.outpost_rowser_version.as_deref().unwrap_or("-");
    format!(
        "{}  {}  since {}  last connected {last}  outpost {version}",
        l.host,
        l.state.label(),
        control::local_time(l.since)
    )
}

/// The Links of the Daemon in `runtime_dir`.
fn links(runtime_dir: &Path) -> Result<Vec<LinkInfo>, RequestError> {
    match control::request(runtime_dir, &Msg::Status)? {
        Msg::StatusReply { links, .. } => Ok(links),
        Msg::OpenResult { error: Some(e), .. } => Err(RequestError::Other(e)),
        other => Err(RequestError::Other(format!("unexpected reply {other:?}"))),
    }
}

/// `rowser host …` from the command line.
pub fn host_main(action: HostAction) -> ExitCode {
    run_host(
        HostArgs {
            config_file: config::path(),
            runtime_dir: crate::platform::runtime_dir(),
            action,
        },
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}
