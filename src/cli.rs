//! clap definitions and argv[0] dispatch.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use url::Url;

pub use crate::platform::BROWSER_NAMES;

#[derive(Debug, Parser)]
#[command(name = "rowser", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the local daemon.
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,
        /// Directory for the daemon's sockets (default: per-OS runtime directory).
        #[arg(long)]
        runtime_dir: Option<PathBuf>,
        /// Command that opens a URL; `{url}` is replaced with the URL as one argument.
        #[arg(long)]
        open_command: Option<String>,
        /// Hosts (glob, may repeat) whose URLs wait until you click "Open".
        /// Matches the Host alias of the Saved host.
        #[arg(long = "confirm-hosts", value_name = "PATTERN")]
        confirm_hosts: Vec<String>,
        /// Removed (v1.0 Links): kept hidden only to fail with a clear error.
        #[arg(long, hide = true)]
        confirm_unverified: bool,
        /// Test hook: answer every confirmation with approve, deny or hang.
        #[arg(long, hide = true)]
        test_confirm: Option<crate::daemon::testing::Answer>,
        /// Test hook: confirmation timeout in seconds (default 60).
        #[arg(long, hide = true)]
        confirm_timeout: Option<u64>,
        /// Test hook: loopback addresses for Callback forwards (default
        /// 127.0.0.1 and ::1). Repeat for more than one.
        #[arg(long, hide = true, value_parser = parse_loopback)]
        loopback: Vec<std::net::IpAddr>,
        /// Test hook: Callback forward timeout in seconds (default 600).
        #[arg(long, hide = true)]
        forward_timeout: Option<u64>,
    },
    /// Removed: prints how to use `rowser host add` and plain ssh.
    #[command(hide = true)]
    Ssh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Open a URL in the local browser.
    Open { url: Url },
    /// Forward a remote port to the local machine.
    Forward {
        /// Remote port, 1024–65535.
        #[arg(value_parser = clap::value_parser!(u16).range(1024..))]
        port: u16,
    },
    /// Check the local and remote setup.
    Doctor,
    /// List the browsers and Profiles found on this machine.
    Browsers {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the running Daemon's state.
    Status {
        /// Print one JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Make the running Daemon read the config file again.
    Reload,
    /// Refuse each Open request and `rowser forward` until `rowser resume`.
    Pause {
        /// End the pause by itself after this long, such as 30m, 1h or 2h30m.
        #[arg(long = "for", value_name = "DURATION", value_parser = crate::control::parse_duration)]
        for_: Option<std::time::Duration>,
    },
    /// End a `rowser pause`.
    Resume,
    /// List the requests that wait for an answer.
    Pending,
    /// Approve a pending request.
    Approve {
        /// The id from `rowser pending`.
        id: u64,
        /// Open it in this Target: system, <browser> or <browser>:<profile>.
        #[arg(long, value_name = "TARGET")]
        open: Option<String>,
    },
    /// Deny a pending request.
    Deny {
        /// The id from `rowser pending`.
        id: u64,
    },
    /// Show the Daemon's last 200 requests, newest last.
    Activity {
        /// Keep printing new requests until Ctrl-C.
        #[arg(long)]
        follow: bool,
        /// Print one JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// List, change and test the Routes in the config file.
    Route {
        #[command(subcommand)]
        action: RouteCmd,
    },
    /// Add, remove, list and log in to the Saved hosts.
    Host {
        #[command(subcommand)]
        action: HostCmd,
    },
    /// Manage rowser on a remote host.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
    /// Internal: the far end of a Link, started by the Daemon over ssh.
    #[command(name = "__outpost", hide = true)]
    Outpost,
}

#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Write and start the user service.
    Install,
    /// Remove the user service.
    Uninstall,
}

#[derive(Debug, Subcommand)]
pub enum RouteCmd {
    /// List the Routes in file order, then [default].
    List,
    /// Append a Route.
    Add {
        /// Glob matched against the Host alias.
        host: String,
        /// Glob matched against the URL's host (may repeat).
        #[arg(long = "match", value_name = "GLOB")]
        matches: Vec<String>,
        /// The Target: system, ask, <browser> or <browser>:<profile>.
        #[arg(long, value_name = "TARGET")]
        open: String,
        /// Use the Target only after you approve.
        #[arg(long)]
        ask: bool,
    },
    /// Remove Route <n> (from `rowser route list`).
    Remove { n: usize },
    /// Move Route <n> to place <to>.
    Move { n: usize, to: usize },
    /// Print the Target and the Route a URL from <host> takes; opens nothing.
    Test { host: String, url: Url },
}

impl From<RouteCmd> for crate::route::RouteAction {
    fn from(c: RouteCmd) -> Self {
        use crate::route::RouteAction as A;
        match c {
            RouteCmd::List => A::List,
            RouteCmd::Add {
                host,
                matches,
                open,
                ask,
            } => A::Add {
                host,
                matches,
                open,
                ask,
            },
            RouteCmd::Remove { n } => A::Remove { n },
            RouteCmd::Move { n, to } => A::Move { n, to },
            RouteCmd::Test { host, url } => A::Test { host, url },
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum HostCmd {
    /// Find rowser on <host> and save it as a Saved host.
    Add {
        /// A Host alias from ssh_config, or user@host.
        host: String,
        /// The host has its own desktop: installs skip the default-browser part.
        #[arg(long)]
        no_default_browser: bool,
    },
    /// Remove a Saved host and close its Link.
    Remove { host: String },
    /// Each Saved host with its Link state.
    List,
    /// Log in to <host> in this terminal, then connect its Link again.
    Connect { host: String },
}

impl From<HostCmd> for crate::host::HostAction {
    fn from(c: HostCmd) -> Self {
        use crate::host::HostAction as A;
        match c {
            HostCmd::Add {
                host,
                no_default_browser,
            } => A::Add {
                host,
                default_browser: !no_default_browser,
            },
            HostCmd::Remove { host } => A::Remove { host },
            HostCmd::List => A::List,
            HostCmd::Connect { host } => A::Connect { host },
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum RemoteAction {
    /// Install rowser on a remote host.
    Install {
        host: String,
        /// Copy this rowser binary instead of the release download.
        #[arg(long, value_name = "PATH")]
        local_build: Option<PathBuf>,
        /// Test hook: base URL for the release asset download.
        #[arg(long, hide = true)]
        release_url: Option<String>,
        /// Install only the binary (a host with its own desktop).
        #[arg(long)]
        no_default_browser: bool,
    },
}

impl Command {
    /// Display label for the command, as a user types it.
    pub fn label(&self) -> &'static str {
        match self {
            Command::Daemon { action: None, .. } => "daemon",
            Command::Daemon {
                action: Some(DaemonAction::Install),
                ..
            } => "daemon install",
            Command::Daemon {
                action: Some(DaemonAction::Uninstall),
                ..
            } => "daemon uninstall",
            Command::Ssh { .. } => "ssh",
            Command::Open { .. } => "open",
            Command::Forward { .. } => "forward",
            Command::Doctor => "doctor",
            Command::Browsers { .. } => "browsers",
            Command::Status { .. } => "status",
            Command::Reload => "reload",
            Command::Pause { .. } => "pause",
            Command::Resume => "resume",
            Command::Pending => "pending",
            Command::Approve { .. } => "approve",
            Command::Deny { .. } => "deny",
            Command::Activity { .. } => "activity",
            Command::Route { .. } => "route",
            Command::Host { .. } => "host",
            Command::Remote { .. } => "remote install",
            Command::Outpost => "__outpost",
        }
    }
}

/// A `--loopback` value: the Daemon binds Callback forwards on loopback only.
fn parse_loopback(text: &str) -> Result<std::net::IpAddr, String> {
    let ip: std::net::IpAddr = text.parse().map_err(|e| format!("{e}"))?;
    if ip.is_loopback() {
        Ok(ip)
    } else {
        Err(format!("{ip} is not a loopback address"))
    }
}

/// Runs a parsed command.
pub fn run(command: Command) -> ExitCode {
    if let Command::Daemon {
        confirm_unverified: true,
        ..
    } = command
    {
        eprintln!("rowser daemon: {CONFIRM_UNVERIFIED_REMOVED}");
        return ExitCode::from(2);
    }
    match command {
        Command::Daemon {
            action: None,
            runtime_dir,
            open_command,
            confirm_hosts,
            confirm_unverified: _,
            test_confirm,
            confirm_timeout,
            loopback,
            forward_timeout,
        } => crate::daemon::run(crate::daemon::RunArgs {
            runtime_dir,
            open_command,
            confirm_hosts,
            test_confirm,
            confirm_timeout,
            loopback,
            forward_timeout,
        }),
        cmd @ Command::Daemon {
            action: Some(DaemonAction::Install),
            ..
        } => match install_args(&cmd) {
            Ok(args) => crate::service::install(&args),
            Err(e) => {
                eprintln!("rowser daemon install: {e}");
                ExitCode::from(2)
            }
        },
        Command::Daemon {
            action: Some(DaemonAction::Uninstall),
            ..
        } => crate::service::uninstall(),
        Command::Open { url } => crate::shim::open_main(&url),
        Command::Ssh { .. } => {
            eprintln!("{SSH_REMOVED}");
            ExitCode::from(2)
        }
        Command::Forward { port } => crate::shim::forward_main(port),
        Command::Doctor => crate::doctor::doctor_main(),
        Command::Browsers { json } => crate::browsers::browsers_main(json),
        Command::Status { json } => crate::control::status_main(json),
        Command::Reload => crate::control::reload_main(),
        Command::Pause { for_ } => crate::control::pause_main(for_),
        Command::Resume => crate::control::resume_main(),
        Command::Pending => crate::control::pending_main(),
        Command::Approve { id, open } => crate::control::approve_main(id, open),
        Command::Deny { id } => crate::control::deny_main(id),
        Command::Activity { follow, json } => crate::control::activity_main(follow, json),
        Command::Route { action } => crate::route::route_main(action.into()),
        Command::Host { action } => crate::host::host_main(action.into()),
        Command::Outpost => crate::outpost::outpost_main(),
        Command::Remote {
            action:
                RemoteAction::Install {
                    host,
                    local_build,
                    release_url,
                    no_default_browser,
                },
        } => crate::remote_install::install_main(
            &host,
            local_build,
            release_url,
            !no_default_browser,
        ),
    }
}

/// The daemon flags of `rowser daemon <flags> install`, as `rowser daemon`
/// arguments for the service, so the installed daemon runs with them
/// (no config file in v1). Refuses the hidden test flags. A relative
/// `--runtime-dir` becomes absolute, as the service does not run in this
/// directory. With no `--runtime-dir`, a set `ROWSER_RUNTIME_DIR` goes
/// into the service as `--runtime-dir`, so one variable sets both sides.
pub fn install_args(cmd: &Command) -> Result<Vec<String>, String> {
    install_args_from(cmd, |k| std::env::var_os(k))
}

fn install_args_from(
    cmd: &Command,
    var: impl Fn(&str) -> Option<OsString>,
) -> Result<Vec<String>, String> {
    let Command::Daemon {
        runtime_dir,
        open_command,
        confirm_hosts,
        test_confirm,
        confirm_timeout,
        loopback,
        forward_timeout,
        ..
    } = cmd
    else {
        return Ok(Vec::new());
    };
    if test_confirm.is_some()
        || confirm_timeout.is_some()
        || !loopback.is_empty()
        || forward_timeout.is_some()
    {
        return Err(
            "test flags (--test-confirm, --confirm-timeout, --loopback, --forward-timeout) \
                    cannot go into the installed service"
                .into(),
        );
    }
    let mut args = Vec::new();
    if let Some(dir) = runtime_dir {
        let dir = std::path::absolute(dir)
            .map_err(|e| format!("cannot resolve --runtime-dir {}: {e}", dir.display()))?;
        let dir = dir
            .to_str()
            .ok_or_else(|| format!("--runtime-dir {} is not UTF-8", dir.display()))?;
        args.extend(["--runtime-dir".to_string(), dir.to_string()]);
    } else if let Some(dir) = crate::platform::rowser_runtime_dir_from(var)? {
        let dir = dir
            .to_str()
            .ok_or_else(|| format!("ROWSER_RUNTIME_DIR {} is not UTF-8", dir.display()))?;
        args.extend(["--runtime-dir".to_string(), dir.to_string()]);
    }
    if let Some(open) = open_command {
        args.extend(["--open-command".to_string(), open.clone()]);
    }
    for host in confirm_hosts {
        args.extend(["--confirm-hosts".to_string(), host.clone()]);
    }
    Ok(args)
}

/// What `rowser ssh …` prints (to stderr) before it exits 2. The Wrapper is
/// removed (v1.0 Links); this stub stays for one release.
pub const SSH_REMOVED: &str =
    "rowser ssh is removed: run \"rowser host add <host>\" once, then use plain ssh";

/// The error of `rowser daemon --confirm-unverified` (exit 2).
pub const CONFIRM_UNVERIFIED_REMOVED: &str =
    "--confirm-unverified is removed: every Open request now comes from a Saved host, \
     so --confirm-hosts matches all of them";

/// Parse a full argv. When argv[0]'s file name is a browser name (`xdg-open`, `sensible-browser`,
/// `www-browser`), or the first argument is a `scheme://` URL, the arguments are parsed as
/// `rowser open …`; otherwise as normal subcommands.
pub fn parse_from<I, T>(argv: I) -> Result<Command, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut args: Vec<OsString> = argv.into_iter().map(Into::into).collect();
    let as_browser = args
        .first()
        .and_then(|a0| Path::new(a0).file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| BROWSER_NAMES.contains(&n));
    // No subcommand name contains `://`. The Daemon still applies its scheme allowlist.
    let url_first = args
        .get(1)
        .and_then(|a| a.to_str())
        .is_some_and(|a| a.contains("://") && Url::parse(a).is_ok());
    if as_browser || url_first {
        args[0] = "rowser".into();
        args.insert(1, "open".into());
    } else if args.get(1).is_some_and(|a| a == "ssh") {
        // Everything after `ssh` belongs to ssh, unchanged: clap would eat a
        // leading `--` and treat `-h`/`--help` as its own.
        return Ok(Command::Ssh {
            args: args.split_off(2),
        });
    }
    Cli::try_parse_from(args).map(|cli| cli.command)
}

#[cfg(test)]
mod install_args_tests {
    use super::*;

    fn args_of(argv: &[&str]) -> Result<Vec<String>, String> {
        install_args_from(&parse_from(argv).unwrap(), |_| None)
    }

    fn args_with_env(argv: &[&str], value: &str) -> Result<Vec<String>, String> {
        let value = OsString::from(value);
        install_args_from(&parse_from(argv).unwrap(), move |k| {
            (k == "ROWSER_RUNTIME_DIR").then(|| value.clone())
        })
    }

    #[test]
    fn install_carries_rowser_runtime_dir_when_no_flag_is_given() {
        let args = args_with_env(&["rowser", "daemon", "install"], "/tmp/rt").unwrap();
        assert_eq!(args, ["--runtime-dir", "/tmp/rt"]);
    }

    #[test]
    fn install_prefers_the_flag_over_rowser_runtime_dir() {
        let argv = ["rowser", "daemon", "--runtime-dir", "/run/r", "install"];
        let args = args_with_env(&argv, "/tmp/rt").unwrap();
        assert_eq!(args, ["--runtime-dir", "/run/r"]);
    }

    #[test]
    fn install_ignores_an_empty_rowser_runtime_dir() {
        let args = args_with_env(&["rowser", "daemon", "install"], "").unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn install_refuses_a_relative_rowser_runtime_dir() {
        let err = args_with_env(&["rowser", "daemon", "install"], "rt").unwrap_err();
        assert!(err.contains("ROWSER_RUNTIME_DIR"), "{err}");
    }

    #[test]
    fn install_carries_the_daemon_flags() {
        let args = args_of(&[
            "rowser",
            "daemon",
            "--runtime-dir",
            "/run/r",
            "--open-command",
            "firefox {url}",
            "--confirm-hosts",
            "a*",
            "--confirm-hosts",
            "b",
            "install",
        ])
        .unwrap();
        assert_eq!(
            args,
            [
                "--runtime-dir",
                "/run/r",
                "--open-command",
                "firefox {url}",
                "--confirm-hosts",
                "a*",
                "--confirm-hosts",
                "b",
            ]
        );
    }

    #[test]
    fn install_with_no_flags_carries_none() {
        assert!(args_of(&["rowser", "daemon", "install"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn install_makes_a_relative_runtime_dir_absolute() {
        let args = args_of(&["rowser", "daemon", "--runtime-dir", "rt", "install"]).unwrap();
        let want = std::env::current_dir().unwrap().join("rt");
        assert_eq!(args, ["--runtime-dir", want.to_str().unwrap()]);
    }

    #[test]
    fn install_refuses_each_test_flag() {
        for flag in [
            &["--test-confirm", "approve"][..],
            &["--confirm-timeout", "5"],
            &["--loopback", "127.0.0.1"],
            &["--forward-timeout", "5"],
        ] {
            let mut argv = vec!["rowser", "daemon"];
            argv.extend(flag);
            argv.push("install");
            let err = args_of(&argv).unwrap_err();
            assert!(err.contains("test flags"), "{flag:?}: {err}");
        }
    }
}

#[cfg(test)]
mod doctor_help_tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn doctor_help_names_the_local_and_remote_sides() {
        let cli = Cli::command();
        let doctor = cli.find_subcommand("doctor").unwrap();
        let about = doctor.get_about().unwrap().to_string();
        assert!(about.contains("local"), "{about}");
        assert!(about.contains("remote"), "{about}");
    }
}
