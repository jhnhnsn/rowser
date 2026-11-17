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
    #[command(name = "__keeper", hide = true)]
    Keeper,
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
            Command::Keeper => "__keeper",
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
        } => crate::daemon::run(runtime_dir, open_command),
        Command::Open { url } => crate::shim::open_main(&url),
        Command::Ssh { args } => crate::wrapper::run(args),
        other => {
            eprintln!("rowser {}: not implemented yet", other.label());
            ExitCode::from(2)
        }
    }
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
    }
    Cli::try_parse_from(args).map(|cli| cli.command)
}
