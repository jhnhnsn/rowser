//! The Shim: `rowser open`, `rowser forward` and the `__keeper` process on
//! the Remote host.

mod keeper;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use url::Url;

use crate::detect::{callback_ports, Procfs};
use crate::discovery::find_tunnel_path;
use crate::protocol::Msg;
pub use keeper::{keeper_main, run_keeper, KeeperArgs};

pub use crate::platform::TUNNEL_DIR;

/// What the Shim reads from its environment.
pub struct ShimEnv<W: Write> {
    /// Directory searched for `rowser-*.sock`.
    pub tunnel_dir: PathBuf,
    /// `ROWSER_SOCKET`, tried before the directory.
    pub socket_override: Option<PathBuf>,
    /// This host's own name, sent as the Reported host name.
    pub reported_host: String,
    /// Root of `/proc`, for Callback port detection.
    pub procfs: Procfs,
    /// The process whose tree may listen on Callback ports (the Shim's parent).
    pub caller_pid: u32,
    /// The rowser executable, re-executed as `rowser __keeper`. `None`
    /// starts no keeper.
    pub keeper_exe: Option<PathBuf>,
    /// `WAYLAND_DISPLAY` or `DISPLAY` is set: with no Tunnel, the URL goes
    /// to the real `xdg-open`.
    pub graphical: bool,
    /// `PATH`, searched for the real `xdg-open`.
    pub search_path: Option<OsString>,
    /// The running rowser executable; an `xdg-open` that resolves to it is
    /// skipped. It holds the same path as `keeper_exe`, but a test sets each
    /// alone: `None` here does not stop the keeper.
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
            reported_host: hostname(),
            procfs: Procfs::default(),
            // SAFETY: getppid has no preconditions and cannot fail.
            caller_pid: unsafe { libc::getppid() } as u32,
            keeper_exe: std::env::current_exe().ok(),
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

/// Sends `url` and its Callback ports to the Daemon through a Tunnel. When
/// the Daemon returns a forward id, starts a detached keeper. Exits 0 when the
/// Daemon accepts the URL, 1 otherwise, without waiting for the callback.
pub async fn run_open<W: Write>(url: &Url, env: &mut ShimEnv<W>) -> ExitCode {
    let ports: Vec<u16> = callback_ports(url, &env.procfs, env.caller_pid)
        .into_iter()
        .collect();
    let request = Msg::Open {
        url: url.to_string(),
        reported_host: env.reported_host.clone(),
        callback_ports: ports.clone(),
    };
    match request_forward(request, &ports, env).await {
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
        Outcome::Accepted | Outcome::KeeperStarted => ExitCode::SUCCESS,
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

/// The first executable `xdg-open` in `search_path` whose canonical path is
/// not the canonical path of `self_exe`.
pub(crate) fn real_xdg_open(search_path: &OsStr, self_exe: Option<&Path>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let own = self_exe.and_then(|p| std::fs::canonicalize(p).ok());
    std::env::split_paths(search_path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("xdg-open"))
        .find(|candidate| {
            let Ok(real) = std::fs::canonicalize(candidate) else {
                return false;
            };
            let executable = std::fs::metadata(&real)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
            executable && Some(&real) != own.as_ref()
        })
}

/// What `request_forward` did.
enum Outcome {
    /// No Tunnel answered; the caller prints the no-tunnel message.
    NoTunnel,
    /// The request failed; the error is on stderr.
    Failed,
    /// The Daemon accepted the request, but no keeper runs.
    Accepted,
    /// The Daemon accepted the request and a keeper holds the forward.
    KeeperStarted,
}

/// `rowser forward <port>`: a Callback forward without a URL. Exits 0 when
/// the forward started, 1 otherwise.
pub async fn run_forward<W: Write>(port: u16, env: &mut ShimEnv<W>) -> ExitCode {
    let request = Msg::Forward {
        reported_host: env.reported_host.clone(),
        callback_ports: vec![port],
    };
    match request_forward(request, &[port], env).await {
        Outcome::NoTunnel => {
            let _ = writeln!(env.stderr, "rowser: no tunnel; cannot forward port {port}");
            ExitCode::FAILURE
        }
        Outcome::KeeperStarted => {
            let _ = writeln!(
                env.stderr,
                "rowser: forwarding local port {port} to port {port} on this host"
            );
            ExitCode::SUCCESS
        }
        _ => ExitCode::FAILURE,
    }
}

/// Sends `request`, prints warnings and errors, and starts a keeper when the
/// reply has a forward id.
async fn request_forward<W: Write>(request: Msg, ports: &[u16], env: &mut ShimEnv<W>) -> Outcome {
    let (mut conn, socket) =
        match find_tunnel_path(&env.tunnel_dir, env.socket_override.clone()).await {
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
    let (Some(id), Some(exe)) = (forward_id, env.keeper_exe.as_deref()) else {
        return Outcome::Accepted;
    };
    match spawn_keeper(exe, &socket, &id, ports) {
        Ok(()) => Outcome::KeeperStarted,
        Err(e) => {
            let _ = writeln!(
                env.stderr,
                "rowser: warning: could not start the callback forward: {e}"
            );
            Outcome::Accepted
        }
    }
}

/// Re-executes `exe` as `rowser __keeper` in a new session, detached from
/// the caller's terminal and stdio, and does not wait for it.
fn spawn_keeper(exe: &Path, socket: &Path, forward_id: &str, ports: &[u16]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe);
    cmd.arg("__keeper")
        .arg("--socket")
        .arg(socket)
        .arg("--forward-id")
        .arg(forward_id);
    for p in ports {
        cmd.arg("--port").arg(p.to_string());
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // `spawn` forks while tokio threads run. The spec's "no fork inside
    // tokio" means no forked child that keeps running tokio code: here the
    // child only calls setsid and then execs at once, which is safe.
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map(drop)
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
