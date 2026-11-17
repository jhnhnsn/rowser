//! clap definitions and argv[0] dispatch.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use url::Url;

/// Names under which rowser acts as `rowser open` (installed as symlinks).
const BROWSER_NAMES: [&str; 3] = ["xdg-open", "sensible-browser", "www-browser"];

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
        #[arg(long, hide = true)]
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
    Install { host: String },
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
        Command::Open { url } => crate::shim::open_main(&url),
        Command::Ssh { args } => ssh(args),
        Command::Forward { port } => crate::shim::forward_main(port),
        Command::Keeper {
            socket,
            forward_id,
            ports,
        } => crate::shim::keeper_main(socket, forward_id, ports),
        other => {
            eprintln!("rowser {}: not implemented yet", other.label());
            ExitCode::from(2)
        }
    }
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
