//! Client side of the control socket: `rowser status` and the helper that
//! every later control command uses.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde::Serialize;
use tokio::net::UnixStream;

use crate::protocol::{
    ActivityEntry, Conn, ForwardInfo, Hello, Msg, Outcome, RequestKind, SessionInfo, CONTROL_SOCKET,
};

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
    runtime.block_on(request_async(runtime_dir, msg))
}

/// `request` for a caller that already runs in a tokio runtime.
pub async fn request_async(runtime_dir: &Path, msg: &Msg) -> Result<Msg, RequestError> {
    let exchange = async {
        let mut conn = open_control(runtime_dir).await?;
        conn.send(msg).await.map_err(|_| RequestError::NotRunning)?;
        match conn.recv().await {
            Ok(Some(reply)) => Ok(reply),
            _ => Err(RequestError::NotRunning),
        }
    };
    tokio::time::timeout(ANSWER_TIMEOUT, exchange)
        .await
        .unwrap_or(Err(RequestError::NotRunning))
}

/// Connects to the control socket in `runtime_dir` and does the handshake.
async fn open_control(runtime_dir: &Path) -> Result<Conn<UnixStream>, RequestError> {
    let stream = UnixStream::connect(runtime_dir.join(CONTROL_SOCKET))
        .await
        .map_err(|_| RequestError::NotRunning)?;
    Conn::connect(stream, Hello::current())
        .await
        .map_err(|e| match e {
            crate::protocol::Error::VersionMismatch { .. } => RequestError::Other(e.to_string()),
            _ => RequestError::NotRunning,
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
        paused_until,
        forwards,
        sessions,
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
        paused_until,
        sessions,
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
    /// When a `rowser pause --for` ends, in seconds since the Unix epoch.
    paused_until: Option<u64>,
    sessions: Vec<SessionInfo>,
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
    let paused = match (s.paused, s.paused_until) {
        (false, _) => "no".to_string(),
        (true, None) => "yes".to_string(),
        (true, Some(u)) => format!("yes, until {}", local_time(u)),
    };
    writeln!(out, "paused:            {paused}")?;
    if s.sessions.is_empty() {
        writeln!(out, "sessions:          none")?;
    } else {
        writeln!(out, "sessions:")?;
        for x in &s.sessions {
            writeln!(out, "  {}  {}  {}", x.host, age(x.age_secs), x.target)?;
        }
    }
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

/// Asks the Daemon in `runtime_dir` to read the config file again, and
/// gives the number of Routes it read. An invalid file is `Other` with the
/// file, line and reason; the Daemon then keeps its old Routes. The
/// `rowser route` commands call this after they change the file.
pub fn reload(runtime_dir: &Path) -> Result<usize, RequestError> {
    match request(runtime_dir, &Msg::Reload)? {
        Msg::Reloaded { routes } => Ok(routes),
        Msg::OpenResult { error: Some(e), .. } => Err(RequestError::Other(e)),
        other => Err(RequestError::Other(format!("unexpected reply {other:?}"))),
    }
}

/// The line `rowser reload` and the `rowser route` commands print after a
/// reload: `rowser: config reloaded (<n> routes)`, `(1 route)` for one.
pub fn reloaded_line(routes: usize) -> String {
    let s = if routes == 1 { "" } else { "s" };
    format!("rowser: config reloaded ({routes} route{s})")
}

/// `rowser reload`: prints `reloaded_line`.
pub fn run_reload(
    runtime_dir: Result<PathBuf, String>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    let result = runtime_dir
        .map_err(RequestError::Other)
        .and_then(|dir| reload(&dir));
    match result {
        Ok(n) => match writeln!(out, "{}", reloaded_line(n)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Err(RequestError::Other(e)) => {
            let _ = writeln!(err, "rowser reload: {e}");
            ExitCode::FAILURE
        }
        Err(e) => {
            let _ = writeln!(err, "{e}");
            ExitCode::FAILURE
        }
    }
}

/// `rowser reload` from the command line.
pub fn reload_main() -> ExitCode {
    run_reload(
        crate::platform::runtime_dir(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// Reads a `rowser pause --for` duration: hours, minutes and seconds in
/// that order, each once, such as `30m`, `1h`, `2h30m` or `45s`.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("invalid duration `{text}`: use a value such as 30m, 1h or 2h30m");
    let mut secs: u64 = 0;
    let mut digits = String::new();
    let mut last_unit = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let (rank, unit) = match c {
            'h' => (1, 3600),
            'm' => (2, 60),
            's' => (3, 1),
            _ => return Err(bad()),
        };
        if digits.is_empty() || rank <= last_unit {
            return Err(bad());
        }
        let n: u64 = digits.parse().map_err(|_| bad())?;
        secs = n
            .checked_mul(unit)
            .and_then(|v| secs.checked_add(v))
            .ok_or_else(bad)?;
        digits.clear();
        last_unit = rank;
    }
    if !digits.is_empty() || secs == 0 {
        return Err(bad());
    }
    Ok(Duration::from_secs(secs))
}

/// Prints the line in `result`, or reports its error: a Daemon error as
/// `rowser <name>: …`, other errors as they are.
fn finish(
    name: &str,
    result: Result<String, RequestError>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    match result {
        Ok(line) => match writeln!(out, "{line}") {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Err(RequestError::Other(e)) => {
            let _ = writeln!(err, "rowser {name}: {e}");
            ExitCode::FAILURE
        }
        Err(e) => {
            let _ = writeln!(err, "{e}");
            ExitCode::FAILURE
        }
    }
}

/// `rowser pause [--for]`: prints `rowser: paused` or
/// `rowser: paused until <local time>`.
pub fn run_pause(
    runtime_dir: Result<PathBuf, String>,
    for_: Option<Duration>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    let msg = Msg::Pause {
        for_secs: for_.map(|d| d.as_secs()),
    };
    let result =
        runtime_dir
            .map_err(RequestError::Other)
            .and_then(|dir| match request(&dir, &msg)? {
                Msg::Paused { until: None } => Ok("rowser: paused".to_string()),
                Msg::Paused { until: Some(u) } => {
                    Ok(format!("rowser: paused until {}", local_time(u)))
                }
                other => Err(unexpected_reply(other)),
            });
    finish("pause", result, out, err)
}

/// `rowser resume`: prints `rowser: resumed`, or `rowser: not paused`.
pub fn run_resume(
    runtime_dir: Result<PathBuf, String>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    let result = runtime_dir.map_err(RequestError::Other).and_then(|dir| {
        match request(&dir, &Msg::Resume)? {
            Msg::Resumed { was_paused: true } => Ok("rowser: resumed".to_string()),
            Msg::Resumed { was_paused: false } => Ok("rowser: not paused".to_string()),
            other => Err(unexpected_reply(other)),
        }
    });
    finish("resume", result, out, err)
}

fn unexpected_reply(reply: Msg) -> RequestError {
    RequestError::Other(match reply {
        Msg::OpenResult { error: Some(e), .. } => e,
        other => format!("unexpected reply {other:?}"),
    })
}

/// `rowser pause` from the command line.
pub fn pause_main(for_: Option<Duration>) -> ExitCode {
    run_pause(
        crate::platform::runtime_dir(),
        for_,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// `rowser resume` from the command line.
pub fn resume_main() -> ExitCode {
    run_resume(
        crate::platform::runtime_dir(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// `rowser activity` inputs.
pub struct ActivityArgs {
    /// The runtime directory, found as `rowser ssh` finds it.
    pub runtime_dir: Result<PathBuf, String>,
    pub json: bool,
    /// Keep printing new and changed entries until the process ends.
    pub follow: bool,
}

/// `rowser activity`: prints the Daemon's recent requests, newest last,
/// one line (or one JSON object) each.
pub fn run_activity(args: ActivityArgs, out: &mut impl Write, err: &mut impl Write) -> ExitCode {
    let dir = match args.runtime_dir {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(err, "rowser activity: {e}");
            return ExitCode::FAILURE;
        }
    };
    let print = |out: &mut dyn Write, e: &ActivityEntry| -> std::io::Result<()> {
        if args.json {
            let json = serde_json::to_string(e).map_err(std::io::Error::other)?;
            writeln!(out, "{json}")?;
        } else {
            writeln!(out, "{}", activity_line(e))?;
        }
        out.flush()
    };
    let msg = Msg::Activity {
        follow: args.follow,
    };
    if !args.follow {
        let entries = match request(&dir, &msg) {
            Ok(Msg::ActivityReply { entries }) => entries,
            Ok(other) => return unexpected(err, other),
            Err(e) => {
                let _ = writeln!(err, "{e}");
                return ExitCode::FAILURE;
            }
        };
        for e in &entries {
            if print(out, e).is_err() {
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "rowser: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let first = tokio::time::timeout(ANSWER_TIMEOUT, async {
            let mut conn = open_control(&dir).await?;
            conn.send(&msg)
                .await
                .map_err(|_| RequestError::NotRunning)?;
            match conn.recv().await {
                Ok(Some(reply)) => Ok((conn, reply)),
                _ => Err(RequestError::NotRunning),
            }
        })
        .await
        .unwrap_or(Err(RequestError::NotRunning));
        let (mut conn, entries) = match first {
            Ok((conn, Msg::ActivityReply { entries })) => (conn, entries),
            Ok((_, other)) => return unexpected(err, other),
            Err(e) => {
                let _ = writeln!(err, "{e}");
                return ExitCode::FAILURE;
            }
        };
        for e in &entries {
            if print(out, e).is_err() {
                return ExitCode::SUCCESS;
            }
        }
        loop {
            match conn.recv().await {
                Ok(Some(Msg::ActivityUpdate { entry })) => {
                    // A closed stdout (`| head`) ends the command quietly.
                    if print(out, &entry).is_err() {
                        return ExitCode::SUCCESS;
                    }
                }
                Ok(Some(_)) => continue,
                _ => {
                    let _ = writeln!(err, "{NOT_RUNNING}");
                    return ExitCode::FAILURE;
                }
            }
        }
    })
}

fn unexpected(err: &mut impl Write, reply: Msg) -> ExitCode {
    let text = match reply {
        Msg::OpenResult { error: Some(e), .. } => e,
        other => format!("unexpected reply {other:?}"),
    };
    let _ = writeln!(err, "rowser activity: {text}");
    ExitCode::FAILURE
}

/// One `rowser activity` text line.
fn activity_line(e: &ActivityEntry) -> String {
    let mut parts = vec![local_time(e.time)];
    parts.push(if e.unverified {
        format!("{} (unverified)", e.host)
    } else {
        e.host.clone()
    });
    let ports = || {
        e.ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    match e.kind {
        RequestKind::Open => parts.push(e.url.clone().unwrap_or_default()),
        RequestKind::Forward => parts.push(format!("forward {}", ports())),
    }
    parts.extend(e.route.clone());
    parts.extend(e.target.clone());
    parts.push(e.result.label().to_string());
    if e.result == Outcome::Opened && !e.ports.is_empty() {
        let open = match e.open_secs {
            None => "still open".to_string(),
            Some(s) => format!("open {}", age(s)),
        };
        parts.push(match e.kind {
            RequestKind::Open => format!("callback {} {open}", ports()),
            RequestKind::Forward => open,
        });
    }
    parts.extend(e.warnings.iter().map(|w| format!("warning: {w}")));
    // Host names, URLs and warnings come from the Remote host.
    crate::protocol::printable(&parts.join("  "))
}

/// Unix seconds as local `YYYY-MM-DD HH:MM:SS`.
fn local_time(secs: u64) -> String {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is plain data; localtime_r only writes into it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return secs.to_string();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// `rowser activity` from the command line.
pub fn activity_main(follow: bool, json: bool) -> ExitCode {
    run_activity(
        ActivityArgs {
            runtime_dir: crate::platform::runtime_dir(),
            json,
            follow,
        },
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
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
