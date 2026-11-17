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
/// The template is split on whitespace (no shell, no quoting). In each word,
/// `{url}` is replaced with the URL, so the URL always stays inside one
/// argument and is never interpreted by a shell.
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
        let mut args: Vec<String> = words.collect();
        if !program.contains("{url}") && !args.iter().any(|a| a.contains("{url}")) {
            args.push("{url}".into());
        }
        Ok(CommandOpener { program, args })
    }
}

impl Opener for CommandOpener {
    fn open(&self, url: &Url) -> io::Result<()> {
        let fill = |w: &str| w.replace("{url}", url.as_str());
        let status = Command::new(fill(&self.program))
            .args(self.args.iter().map(|a| fill(a)))
            .stdin(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "open command `{}` failed: {status}",
                self.program
            )))
        }
    }
}
