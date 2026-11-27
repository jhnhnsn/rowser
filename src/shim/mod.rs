//! The Shim: `rowser open` and `rowser forward` on the Remote host.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use url::Url;

use crate::detect::{callback_ports, Procfs};
use crate::discovery::find_tunnel;
use crate::platform::real_xdg_open;
use crate::protocol::Msg;

pub use crate::platform::TUNNEL_DIR;

/// What the Shim reads from its environment.
pub struct ShimEnv<W: Write> {
    /// Directory searched for `rowser-*.sock`.
    pub tunnel_dir: PathBuf,
    /// `ROWSER_SOCKET`, tried before the directory.
    pub socket_override: Option<PathBuf>,
    /// The Outpost socket, tried after `ROWSER_SOCKET` and before the
    /// directory search.
    pub outpost_socket: Option<PathBuf>,
    /// This host's own name, sent as the Reported host name.
    pub reported_host: String,
    /// Root of `/proc`, for Callback port detection.
    pub procfs: Procfs,
    /// The process whose tree may listen on Callback ports (the Shim's parent).
    pub caller_pid: u32,
    /// `WAYLAND_DISPLAY` or `DISPLAY` is set: with no Tunnel, the URL goes
    /// to the real `xdg-open`.
    pub graphical: bool,
    /// `PATH`, searched for the real `xdg-open`.
    pub search_path: Option<OsString>,
    /// The running rowser executable; an `xdg-open` that resolves to it is
    /// skipped.
    pub self_exe: Option<PathBuf>,
    /// `ROWSER_SHIM_FALLBACK` is set: this Shim runs under a real `xdg-open`
    /// that an earlier Shim started, so it must not run `xdg-open` again.
    pub in_fallback: bool,
    pub stderr: W,
}

impl ShimEnv<std::io::Stderr> {
    /// The environment of the running process.
    pub fn from_process() -> ShimEnv<std::io::Stderr> {
        ShimEnv {
            // ROWSER_TEST_TUNNEL_DIR is a hidden test hook: it keeps a test
            // Shim away from real Session sockets in /tmp.
            tunnel_dir: std::env::var_os("ROWSER_TEST_TUNNEL_DIR")
                .filter(|v| !v.is_empty())
                .map_or_else(|| PathBuf::from(TUNNEL_DIR), PathBuf::from),
            socket_override: std::env::var_os("ROWSER_SOCKET")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            outpost_socket: Some(crate::outpost::socket_path(&crate::outpost::outpost_dir())),
            reported_host: hostname(),
            procfs: Procfs::default(),
            // SAFETY: getppid has no preconditions and cannot fail.
            caller_pid: unsafe { libc::getppid() } as u32,
            graphical: ["WAYLAND_DISPLAY", "DISPLAY"]
                .iter()
                .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty())),
            search_path: std::env::var_os("PATH"),
            self_exe: std::env::current_exe().ok(),
            in_fallback: std::env::var_os(FALLBACK_GUARD).is_some(),
            stderr: std::io::stderr(),
        }
    }
}

/// Sends `url` and its Callback ports to the Daemon through a Tunnel. The
/// Outpost holds any Callback forward. Exits 0 when the Daemon accepts the
/// URL, 1 otherwise, without waiting for the callback.
pub async fn run_open<W: Write>(url: &Url, env: &mut ShimEnv<W>) -> ExitCode {
    let ports: Vec<u16> = callback_ports(url, &env.procfs, env.caller_pid)
        .into_iter()
        .collect();
    let request = Msg::Open {
        url: url.to_string(),
        reported_host: env.reported_host.clone(),
        callback_ports: ports.clone(),
    };
    match request_forward(request, env).await {
        Outcome::Failed => ExitCode::FAILURE,
        Outcome::NoTunnel => {
            if env.graphical && !env.in_fallback {
                if let Some(code) = run_real_xdg_open(url, env) {
                    return code;
                }
            }
            let _ = writeln!(
                env.stderr,
                "rowser: no tunnel; open this URL manually: {url}"
            );
            ExitCode::FAILURE
        }
        Outcome::Accepted | Outcome::Forwarded => ExitCode::SUCCESS,
    }
}

/// Runs the first `xdg-open` on the search path that is not rowser itself
/// with `url`, and returns its exit code. `None` when there is no such
/// program or it cannot start.
fn run_real_xdg_open<W: Write>(url: &Url, env: &mut ShimEnv<W>) -> Option<ExitCode> {
    let program = real_xdg_open(env.search_path.as_deref()?, env.self_exe.as_deref())?;
    let status = std::process::Command::new(&program)
        .arg(url.as_str())
        .env(FALLBACK_GUARD, "1")
        .status();
    match status {
        Ok(s) => Some(match s.code() {
            Some(c) => ExitCode::from(u8::try_from(c).unwrap_or(1)),
            None => ExitCode::FAILURE,
        }),
        Err(e) => {
            let _ = writeln!(env.stderr, "rowser: {}: {e}", program.display());
            None
        }
    }
}

/// Hidden variable set on the real `xdg-open`. The real `xdg-open` can start
/// rowser again (`$BROWSER`, `www-browser`), and a hard link or copy of
/// rowser on `PATH` passes the canonical-path check below. A Shim that sees
/// this variable does not run `xdg-open`, so neither case loops.
const FALLBACK_GUARD: &str = "ROWSER_SHIM_FALLBACK";

/// What `request_forward` did.
enum Outcome {
    /// No Tunnel answered; the caller prints the no-tunnel message.
    NoTunnel,
    /// The request failed; the error is on stderr.
    Failed,
    /// The Daemon accepted the request, with no Callback forward.
    Accepted,
    /// The Daemon accepted the request and started a Callback forward.
    Forwarded,
}

/// `rowser forward <port>`: a Callback forward without a URL. Exits 0 when
/// the forward started, 1 otherwise.
pub async fn run_forward<W: Write>(port: u16, env: &mut ShimEnv<W>) -> ExitCode {
    let request = Msg::Forward {
        reported_host: env.reported_host.clone(),
        callback_ports: vec![port],
    };
    match request_forward(request, env).await {
        Outcome::NoTunnel => {
            let _ = writeln!(env.stderr, "rowser: no tunnel; cannot forward port {port}");
            ExitCode::FAILURE
        }
        Outcome::Forwarded => {
            let _ = writeln!(
                env.stderr,
                "rowser: forwarding local port {port} to port {port} on this host"
            );
            ExitCode::SUCCESS
        }
        _ => ExitCode::FAILURE,
    }
}

/// Sends `request` and prints the reply's warnings and errors.
async fn request_forward<W: Write>(request: Msg, env: &mut ShimEnv<W>) -> Outcome {
    // ROWSER_SOCKET, else the Outpost socket, else the old directory search.
    // The owner check applies to the Outpost socket only: ROWSER_SOCKET is
    // the user's own choice.
    let first = match env.socket_override.clone() {
        Some(p) => Some(p),
        None => match env.outpost_socket.clone() {
            Some(p) if p.symlink_metadata().is_ok() => match crate::outpost::check_socket(&p) {
                Ok(()) => Some(p),
                Err(e) => {
                    let _ = writeln!(
                        env.stderr,
                        "rowser: warning: not using the Outpost socket: {e}"
                    );
                    None
                }
            },
            _ => None,
        },
    };
    let mut conn = match find_tunnel(&env.tunnel_dir, first).await {
        Ok(found) => found,
        Err(e) => {
            if let Some(mismatch) = e.mismatch {
                let _ = writeln!(env.stderr, "rowser: {mismatch}");
            }
            return Outcome::NoTunnel;
        }
    };
    if let Err(e) = conn.send(&request).await {
        let _ = writeln!(env.stderr, "rowser: {e}");
        return Outcome::Failed;
    }
    let (forward_id, warnings, error) = match conn.recv().await {
        Ok(Some(Msg::OpenResult {
            forward_id,
            warnings,
            error,
        })) => (forward_id, warnings, error),
        Ok(Some(other)) => {
            let _ = writeln!(
                env.stderr,
                "rowser: unexpected reply from daemon: {other:?}"
            );
            return Outcome::Failed;
        }
        Ok(None) => {
            let _ = writeln!(env.stderr, "rowser: daemon closed the connection");
            return Outcome::Failed;
        }
        Err(e) => {
            let _ = writeln!(env.stderr, "rowser: {e}");
            return Outcome::Failed;
        }
    };
    for w in warnings {
        let _ = writeln!(env.stderr, "rowser: warning: {w}");
    }
    if let Some(e) = error {
        let _ = writeln!(env.stderr, "rowser: {e}");
        return Outcome::Failed;
    }
    match forward_id {
        Some(_) => Outcome::Forwarded,
        None => Outcome::Accepted,
    }
}

fn block_on(f: impl std::future::Future<Output = ExitCode>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(f),
        Err(e) => {
            eprintln!("rowser: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `rowser open <url>` for the binary.
pub fn open_main(url: &Url) -> ExitCode {
    block_on(run_open(url, &mut ShimEnv::from_process()))
}

/// `rowser forward <port>` for the binary.
pub fn forward_main(port: u16) -> ExitCode {
    block_on(run_forward(port, &mut ShimEnv::from_process()))
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for buf.len() bytes; gethostname writes at most that.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "unknown".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}
