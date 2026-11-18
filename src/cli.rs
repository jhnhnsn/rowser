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
        /// Matches the Host alias from `rowser ssh`.
        #[arg(long = "confirm-hosts", value_name = "PATTERN")]
        confirm_hosts: Vec<String>,
        /// Also match --confirm-hosts against the untrusted host name that
        /// the remote host reports on the direct socket (ssh_config setups).
        #[arg(long)]
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
    },
    /// Run ssh with a Tunnel to the Daemon.
    Ssh {
        /// Arguments passed to ssh unchanged.
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
    /// Check the remote setup.
    Doctor,
    /// Manage rowser on a remote host.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
    /// Internal: holds a Callback forward for `rowser open` and `rowser forward`.
    #[command(name = "__keeper", hide = true)]
    Keeper {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        forward_id: String,
        #[arg(long = "port", required = true)]
        ports: Vec<u16>,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Write and start the user service.
    Install,
    /// Remove the user service.
    Uninstall,
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
            Command::Remote { .. } => "remote install",
            Command::Keeper { .. } => "__keeper",
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
    match command {
        Command::Daemon {
            action: None,
            runtime_dir,
            open_command,
            confirm_hosts,
            confirm_unverified,
            test_confirm,
            confirm_timeout,
            loopback,
        } => crate::daemon::run(crate::daemon::RunArgs {
            runtime_dir,
            open_command,
            confirm_hosts,
            confirm_unverified,
            test_confirm,
            confirm_timeout,
            loopback,
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
        Command::Ssh { args } => ssh(args),
        Command::Forward { port } => crate::shim::forward_main(port),
        Command::Doctor => crate::doctor::doctor_main(),
        Command::Keeper {
            socket,
            forward_id,
            ports,
        } => crate::shim::keeper_main(socket, forward_id, ports),
        Command::Remote {
            action:
                RemoteAction::Install {
                    host,
                    local_build,
                    release_url,
                },
        } => crate::remote_install::install_main(&host, local_build, release_url),
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
        confirm_unverified,
        test_confirm,
        confirm_timeout,
        loopback,
        ..
    } = cmd
    else {
        return Ok(Vec::new());
    };
    if test_confirm.is_some() || confirm_timeout.is_some() || !loopback.is_empty() {
        return Err(
            "test flags (--test-confirm, --confirm-timeout, --loopback) \
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
    if *confirm_unverified {
        args.push("--confirm-unverified".to_string());
    }
    Ok(args)
}

/// `rowser ssh <args…>`: finds the Daemon's runtime directory and runs the Wrapper.
fn ssh(args: Vec<OsString>) -> ExitCode {
    let env = crate::platform::runtime_dir().map(|dir| crate::wrapper::WrapperEnv {
        ssh: "ssh".into(),
        relay_socket: dir.join(crate::daemon::RELAY_SOCKET),
        session_dir: dir,
    });
    crate::wrapper::run(args, env)
}

/// Parse a full argv. When argv[0]'s file name is a browser name (`xdg-open`, `sensible-browser`, `www-browser`),
/// the arguments are parsed as `rowser open …`; otherwise as normal subcommands.
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
    if as_browser {
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
            "--confirm-unverified",
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
                "--confirm-unverified",
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
        ] {
            let mut argv = vec!["rowser", "daemon"];
            argv.extend(flag);
            argv.push("install");
            let err = args_of(&argv).unwrap_err();
            assert!(err.contains("test flags"), "{flag:?}: {err}");
        }
    }
}
