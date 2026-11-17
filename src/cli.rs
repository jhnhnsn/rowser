//! clap definitions and argv[0] dispatch.

use std::ffi::OsString;
use std::path::Path;

use clap::{Parser, Subcommand};

/// Names under which rowser acts as `rowser open` (installed as symlinks).
pub const BROWSER_NAMES: [&str; 3] = ["xdg-open", "sensible-browser", "www-browser"];

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
    },
    /// Run ssh with a per-session socket forward.
    Ssh {
        /// Arguments passed to ssh.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Open a URL in the local browser.
    Open { url: String },
    /// Forward a remote port to the local machine.
    Forward { port: u16 },
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
    pub fn name(&self) -> &'static str {
        match self {
            Command::Daemon { action: None } => "daemon",
            Command::Daemon {
                action: Some(DaemonAction::Install),
            } => "daemon install",
            Command::Daemon {
                action: Some(DaemonAction::Uninstall),
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

/// Parse a full argv. When argv[0]'s file name is one of [`BROWSER_NAMES`],
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
