//! Opener port and the `CommandOpener` adapter.

use std::io;
use std::process::{Command, Stdio};

use url::Url;

use crate::browsers::{Browsers, Roots};
use crate::config::Target;

/// Opens a URL in the browser.
pub trait Opener: Send + Sync + 'static {
    /// Opens `url` in `target`. The Daemon passes `system` or a browser
    /// Target, never `ask`.
    fn open(&self, url: &Url, target: &Target) -> io::Result<()>;
}

/// Runs an `--open-command` template such as `xdg-open {url}`.
///
/// The template is split on whitespace (no shell, no quoting). The first word
/// is the program and may not contain `{url}`; in each later word, `{url}` is
/// replaced with the URL, so the URL always stays inside one argument and is
/// never interpreted by a shell. A template without `{url}` gets the URL as a
/// last argument.
///
/// `open` returns as soon as the command has started; it does not wait for the
/// command to exit (a browser such as `firefox {url}` may run for hours). The
/// child is reaped on a background thread.
#[derive(Debug, Clone)]
pub struct CommandOpener {
    program: String,
    args: Vec<String>,
    /// Browser discovery roots; `None` refuses browser Targets.
    roots: Option<Roots>,
}

impl CommandOpener {
    pub fn new(template: &str) -> Result<CommandOpener, String> {
        let mut words = template.split_whitespace().map(str::to_owned);
        let program = words
            .next()
            .ok_or_else(|| "the open command is empty".to_string())?;
        if program.contains("{url}") {
            return Err("the open command's program may not contain {url}".into());
        }
        let mut args: Vec<String> = words.collect();
        if !args.iter().any(|a| a.contains("{url}")) {
            args.push("{url}".into());
        }
        Ok(CommandOpener {
            program,
            args,
            roots: None,
        })
    }

    /// Opens browser Targets with the browsers found under `roots`.
    pub fn with_browsers(mut self, roots: Roots) -> CommandOpener {
        self.roots = Some(roots);
        self
    }

    /// The argv for one URL: `browsers::command` for a browser Target, else
    /// the template.
    fn argv(&self, url: &Url, target: &Target) -> io::Result<Vec<String>> {
        if let Target::Browser { .. } = target {
            let roots = self
                .roots
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no browser discovery"))?;
            return Browsers::scan(roots)
                .command(roots.os, target, url.as_str())
                .map_err(|e| io::Error::new(io::ErrorKind::NotFound, e));
        }
        let mut v = vec![self.program.clone()];
        v.extend(self.args.iter().map(|a| a.replace("{url}", url.as_str())));
        Ok(v)
    }
}

impl Opener for CommandOpener {
    /// Runs the argv for `target`; never through a shell.
    fn open(&self, url: &Url, target: &Target) -> io::Result<()> {
        let argv = self.argv(url, target)?;
        let program = argv[0].clone();
        let mut child = Command::new(&program)
            .args(&argv[1..])
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("could not start open command `{program}`: {e}"),
                )
            })?;
        std::thread::spawn(move || match child.wait() {
            Ok(status) if !status.success() => {
                tracing::warn!("open command `{program}` failed: {status}")
            }
            Err(e) => tracing::warn!("open command `{program}`: {e}"),
            Ok(_) => {}
        });
        Ok(())
    }
}
