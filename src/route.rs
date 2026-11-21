//! `rowser route list|add|remove|move|test`: the CLI edits the config file
//! itself (ADR 0007), then asks a running Daemon to reload it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use url::Url;

use crate::config::{self, Route, Target};
use crate::control::{self, RequestError};
use crate::routes::{RequestHost, Routes};

/// One `rowser route` command.
#[derive(Debug, Clone)]
pub enum RouteAction {
    List,
    Add {
        host: String,
        matches: Vec<String>,
        open: String,
        ask: bool,
    },
    Remove {
        n: usize,
    },
    Move {
        n: usize,
        to: usize,
    },
    /// Which Route an Open request from `host` for `url` takes.
    Test {
        host: String,
        url: Url,
        /// `host` is a Reported host name, not a Host alias.
        unverified: bool,
    },
}

impl RouteAction {
    fn name(&self) -> &'static str {
        match self {
            RouteAction::List => "list",
            RouteAction::Add { .. } => "add",
            RouteAction::Remove { .. } => "remove",
            RouteAction::Move { .. } => "move",
            RouteAction::Test { .. } => "test",
        }
    }
}

/// `rowser route` inputs.
pub struct RouteArgs {
    /// The config file (`config::path`).
    pub config_file: Result<PathBuf, String>,
    /// The runtime directory of the Daemon to reload.
    pub runtime_dir: Result<PathBuf, String>,
    pub action: RouteAction,
}

/// Runs one `rowser route` command.
pub fn run_route(args: RouteArgs, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    let name = args.action.name();
    match run_inner(args, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "rowser route {name}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}

/// `rowser route list`: one row per Route, then the two fallbacks.
fn list(file: &Path, out: &mut impl Write) -> Result<(), String> {
    let c = config::load(file)?;
    let routes = Routes::from_config(&c);
    for (i, r) in c.routes.iter().enumerate() {
        let mut parts = vec![(i + 1).to_string(), r.host.clone()];
        if !r.matches.is_empty() {
            parts.push(format!("match {}", r.matches.join(",")));
        }
        parts.push(r.target.to_string());
        if r.ask {
            parts.push("ask".into());
        }
        writeln!(out, "{}", parts.join("  ")).map_err(io)?;
    }
    let (unverified, default) = routes.fallbacks();
    writeln!(out, "[unverified]  {unverified}").map_err(io)?;
    writeln!(out, "[default]  {default}").map_err(io)
}

/// `rowser route test`: the Target and the Route that matched.
fn test(file: &Path, host: RequestHost, url: &Url, out: &mut impl Write) -> Result<(), String> {
    let routes = Routes::from_config(&config::load(file)?);
    let pick = routes.pick(&host, url);
    let label = match (pick.route, &host) {
        (Some(i), _) => format!("route {}", i + 1),
        (None, RequestHost::Unverified(_)) => "[unverified]".into(),
        (None, RequestHost::Alias(_)) => "[default]".into(),
    };
    let ask = if pick.ask { "  ask" } else { "" };
    writeln!(out, "{}  {label}{ask}", pick.target).map_err(io)
}

fn run_inner(args: RouteArgs, out: &mut impl Write) -> Result<(), String> {
    let file = args.config_file?;
    let read = || match std::fs::read_to_string(&file) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    };
    let in_file = |e: String| format!("{}: {e}", file.display());
    let (text, done) = match args.action {
        RouteAction::List => return list(&file, out),
        RouteAction::Test {
            host,
            url,
            unverified,
        } => {
            let host = if unverified {
                RequestHost::Unverified(host)
            } else {
                RequestHost::Alias(host)
            };
            return test(&file, host, &url, out);
        }
        RouteAction::Add {
            host,
            matches,
            open,
            ask,
        } => {
            let target: Target = open.parse()?;
            let route = Route {
                host,
                matches,
                target,
                ask,
            };
            let (text, n) = config::add_route(&read()?, &route).map_err(in_file)?;
            (text, format!("added route {n}"))
        }
        RouteAction::Remove { n } => (
            config::remove_route(&read()?, n).map_err(in_file)?,
            format!("removed route {n}"),
        ),
        RouteAction::Move { n, to } => (
            config::move_route(&read()?, n, to).map_err(in_file)?,
            format!("moved route {n} to {to}"),
        ),
    };
    config::write(&file, &text)?;
    writeln!(out, "rowser: {done}").map_err(io)?;
    // The file change is done, so a reload the Daemon refuses is only a
    // note: the command still exits 0.
    let note = reload_note(args.runtime_dir.as_deref().map_err(String::clone));
    writeln!(out, "{note}").map_err(io)
}

/// What to say about the Daemon after a change.
fn reload_note(runtime_dir: Result<&Path, String>) -> String {
    match runtime_dir
        .map_err(RequestError::Other)
        .and_then(control::reload)
    {
        Ok(n) => control::reloaded_line(n),
        Err(RequestError::NotRunning) => {
            "rowser: the daemon is not running; the change applies when it starts".into()
        }
        Err(RequestError::Other(e)) => format!("rowser: the daemon did not reload: {e}"),
    }
}

/// `rowser route …` from the command line.
pub fn route_main(action: RouteAction) -> ExitCode {
    run_route(
        RouteArgs {
            config_file: config::path(),
            runtime_dir: crate::platform::runtime_dir(),
            action,
        },
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}
