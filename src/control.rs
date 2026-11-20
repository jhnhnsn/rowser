//! Client side of the control socket: `rowser status` and the helper that
//! every later control command uses.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde::Serialize;
use tokio::net::UnixStream;

use crate::protocol::{Conn, ForwardInfo, Hello, Msg, CONTROL_SOCKET};

/// The line every control command prints when the Daemon does not answer.
pub const NOT_RUNNING: &str = "rowser: the daemon is not running";

/// How long a control command waits for the Daemon.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a control request failed.
#[derive(Debug)]
pub enum RequestError {
    /// No control socket, or no answer on it.
    NotRunning,
    /// The Daemon answered, but not with a reply the command can use.
    Other(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::NotRunning => f.write_str(NOT_RUNNING),
            RequestError::Other(e) => write!(f, "rowser: {e}"),
        }
    }
}

/// Sends `msg` on the control socket in `runtime_dir` and gives the reply.
/// A protocol major mismatch is `Other`, so the user sees both versions.
pub fn request(runtime_dir: &Path, msg: &Msg) -> Result<Msg, RequestError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| RequestError::Other(e.to_string()))?;
    let path = runtime_dir.join(CONTROL_SOCKET);
    runtime.block_on(async {
        let exchange = async {
            let stream = UnixStream::connect(&path)
                .await
                .map_err(|_| RequestError::NotRunning)?;
            let mut conn = Conn::connect(stream, Hello::current())
                .await
                .map_err(|e| match e {
                    crate::protocol::Error::VersionMismatch { .. } => {
                        RequestError::Other(e.to_string())
                    }
                    _ => RequestError::NotRunning,
                })?;
            conn.send(msg).await.map_err(|_| RequestError::NotRunning)?;
            match conn.recv().await {
                Ok(Some(reply)) => Ok(reply),
                _ => Err(RequestError::NotRunning),
            }
        };
        tokio::time::timeout(ANSWER_TIMEOUT, exchange)
            .await
            .unwrap_or(Err(RequestError::NotRunning))
    })
}

/// `rowser status` inputs.
pub struct StatusArgs {
    /// The runtime directory, found as `rowser ssh` finds it.
    pub runtime_dir: Result<PathBuf, String>,
    pub json: bool,
}

/// `rowser status`: prints the Daemon's state.
pub fn run_status(args: StatusArgs, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    let dir = match args.runtime_dir {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(err, "rowser status: {e}");
            return ExitCode::FAILURE;
        }
    };
    let reply = match request(&dir, &Msg::Status) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return ExitCode::FAILURE;
        }
    };
    let Msg::StatusReply {
        daemon_version,
        protocol_version,
        config_file,
        config_file_exists,
        paused,
        forwards,
    } = reply
    else {
        let text = match reply {
            Msg::OpenResult { error: Some(e), .. } => e,
            other => format!("unexpected reply {other:?}"),
        };
        let _ = writeln!(err, "rowser status: {text}");
        return ExitCode::FAILURE;
    };
    let view = StatusView {
        daemon_version,
        protocol_version: protocol_version.to_string(),
        runtime_dir: dir,
        config_file,
        config_file_exists,
        paused,
        forwards,
    };
    let written = if args.json {
        serde_json::to_string(&view)
            .map_err(std::io::Error::other)
            .and_then(|json| writeln!(out, "{json}"))
    } else {
        write_text(out, &view)
    };
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// What `rowser status` prints, as text or as one JSON object.
#[derive(Serialize)]
struct StatusView {
    daemon_version: String,
    protocol_version: String,
    runtime_dir: PathBuf,
    config_file: Option<String>,
    config_file_exists: bool,
    paused: bool,
    forwards: Vec<ForwardInfo>,
}

fn write_text(out: &mut impl Write, s: &StatusView) -> std::io::Result<()> {
    let config_file = match (&s.config_file, s.config_file_exists) {
        (None, _) => "none".to_string(),
        (Some(p), true) => p.clone(),
        (Some(p), false) => format!("{p} (no file)"),
    };
    writeln!(out, "daemon version:    {}", s.daemon_version)?;
    writeln!(out, "protocol version:  {}", s.protocol_version)?;
    writeln!(out, "runtime directory: {}", s.runtime_dir.display())?;
    writeln!(out, "config file:       {config_file}")?;
    writeln!(
        out,
        "paused:            {}",
        if s.paused { "yes" } else { "no" }
    )?;
    if s.forwards.is_empty() {
        return writeln!(out, "callback forwards: none");
    }
    writeln!(out, "callback forwards:")?;
    for f in &s.forwards {
        let host = if f.unverified {
            format!("{} (unverified)", f.host)
        } else {
            f.host.clone()
        };
        writeln!(out, "  {}  {host}  {}", f.port, age(f.age_secs))?;
    }
    Ok(())
}

/// An age as `42s`, `3m 05s` or `1h 02m`.
fn age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// `rowser status` from the command line.
pub fn status_main(json: bool) -> ExitCode {
    run_status(
        StatusArgs {
            runtime_dir: crate::platform::runtime_dir(),
            json,
        },
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn ages_read_as_seconds_minutes_or_hours() {
        assert_eq!(super::age(42), "42s");
        assert_eq!(super::age(185), "3m 05s");
        assert_eq!(super::age(3720), "1h 02m");
    }
}
