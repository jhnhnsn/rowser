//! `rowser route list|add|remove|move|test`: the CLI edits the config file
//! itself (ADR 0007), then asks a running Daemon to reload it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use url::Url;

use crate::browsers::{Browsers, Roots};
use crate::config::{self, Route, Target};
use crate::control;
use crate::routes::Routes;

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
    /// The browsers on the Local machine, for `add` and `test`.
    pub browsers: Browsers,
}

/// Runs one `rowser route` command.
pub fn run_route(args: RouteArgs, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    let name = args.action.name();
    match run_inner(args, out, err) {
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

/// `rowser route list`: one row per Route, then `[default]`.
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
    writeln!(out, "[default]  {}", routes.default_target()).map_err(io)
}

/// `rowser route test`: the Target and the Route that matched. A browser
/// Target shows as resolved (profile folder name), or as its fallback with
/// a `warning:` line.
fn test(
    file: &Path,
    host: &str,
    url: &Url,
    browsers: &Browsers,
    out: &mut impl Write,
) -> Result<(), String> {
    let routes = Routes::from_config(&config::load(file)?);
    let pick = routes.pick(host, url);
    let label = crate::routes::route_label(pick.route);
    let (target, ask, warnings) = match pick.target {
        Target::Browser { .. } => {
            let r = routes.resolve(host, url, browsers);
            (r.target, r.ask, r.warnings)
        }
        // `system` and `ask` show as picked, as before.
        t => (t, pick.ask, vec![]),
    };
    let ask = if ask { "  ask" } else { "" };
    writeln!(out, "{target}  {label}{ask}").map_err(io)?;
    for w in warnings {
        writeln!(out, "warning: {w}").map_err(io)?;
    }
    Ok(())
}

fn run_inner(args: RouteArgs, out: &mut impl Write, err: &mut impl Write) -> Result<(), String> {
    let file = args.config_file?;
    let read = || match std::fs::read_to_string(&file) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    };
    let in_file = |e: String| format!("{}: {e}", file.display());
    let (text, done) = match args.action {
        RouteAction::List => return list(&file, out),
        RouteAction::Test { host, url } => {
            return test(&file, &host, &url, &args.browsers, out);
        }
        RouteAction::Add {
            host,
            matches,
            open,
            ask,
        } => {
            let mut target: Target = open.parse()?;
            let mut comment = None;
            let mut note = None;
            if let Target::Browser { .. } = target {
                match args.browsers.find(&target) {
                    Ok((_, Some(p))) => {
                        comment = Some(p.name.clone());
                        target = args.browsers.normalize(&target)?;
                    }
                    Ok((_, None)) => {}
                    Err(why) => {
                        note = Some(format!(
                            "rowser: warning: {why}; the route falls back to [default] until it is found"
                        ))
                    }
                }
            }
            let route = Route {
                host,
                matches,
                target,
                ask,
            };
            let (text, n) =
                config::add_route(&read()?, &route, comment.as_deref()).map_err(in_file)?;
            if let Some(note) = note {
                writeln!(err, "{note}").map_err(io)?;
            }
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
    let note = control::reload_note(args.runtime_dir.as_deref().map_err(String::clone));
    writeln!(out, "{note}").map_err(io)
}

/// `rowser route …` from the command line.
pub fn route_main(action: RouteAction) -> ExitCode {
    run_route(
        RouteArgs {
            config_file: config::path(),
            runtime_dir: crate::platform::runtime_dir(),
            action,
            browsers: Browsers::scan(&Roots::from_env()),
        },
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}
