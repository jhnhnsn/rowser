//! The tracing subscriber: log lines go to stderr only, so stdout stays clean
//! for commands that print to it. `ROWSER_LOG` sets the level (an
//! `EnvFilter` directive such as `debug` or `rowser=trace`); default `warn`.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// The environment variable that sets the log level.
pub const LOG_ENV: &str = "ROWSER_LOG";

/// Starts the subscriber for this process. Call once, first in `main`. An
/// invalid `ROWSER_LOG` falls back to `warn`.
pub fn init() {
    let filter = EnvFilter::builder()
        .with_default_directive(tracing::Level::WARN.into())
        .with_env_var(LOG_ENV)
        .from_env_lossy();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}
