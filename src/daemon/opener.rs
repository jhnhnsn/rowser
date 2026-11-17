//! Opener port and the `CommandOpener` adapter.

use std::io;
use std::process::{Command, Stdio};

use url::Url;

/// Opens a URL in the browser.
pub trait Opener: Send + Sync + 'static {
    fn open(&self, url: &Url) -> io::Result<()>;
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
        Ok(CommandOpener { program, args })
    }
}

impl Opener for CommandOpener {
    fn open(&self, url: &Url) -> io::Result<()> {
        let mut child = Command::new(&self.program)
            .args(self.args.iter().map(|a| a.replace("{url}", url.as_str())))
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("could not start open command `{}`: {e}", self.program),
                )
            })?;
        let program = self.program.clone();
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
