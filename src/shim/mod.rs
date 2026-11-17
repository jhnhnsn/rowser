//! The Shim: `rowser open`, `rowser forward` and the `__keeper` process on
//! the Remote host.

mod keeper;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use url::Url;

use crate::detect::{callback_ports, Procfs};
use crate::discovery::find_tunnel_path;
use crate::protocol::Msg;
pub use keeper::{keeper_main, run_keeper, KeeperArgs};

/// Where the Shim looks for Session sockets in production.
pub const TUNNEL_DIR: &str = "/tmp";

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
    pub stderr: W,
}

impl ShimEnv<std::io::Stderr> {
    /// The environment of the running process.
    pub fn from_process() -> ShimEnv<std::io::Stderr> {
        ShimEnv {
            tunnel_dir: PathBuf::from(TUNNEL_DIR),
            socket_override: std::env::var_os("ROWSER_SOCKET")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            reported_host: hostname(),
            procfs: Procfs::default(),
            // SAFETY: getppid has no preconditions and cannot fail.
            caller_pid: unsafe { libc::getppid() } as u32,
            keeper_exe: std::env::current_exe().ok(),
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
    let manual = format!("open this URL manually: {url}");
    match request_forward(request, &ports, &manual, env).await {
        Some(_) => ExitCode::SUCCESS,
        None => ExitCode::FAILURE,
    }
}

/// `rowser forward <port>`: a Callback forward without a URL. Exits 0 when
/// the forward started, 1 otherwise.
pub async fn run_forward<W: Write>(port: u16, env: &mut ShimEnv<W>) -> ExitCode {
    let request = Msg::Forward {
        reported_host: env.reported_host.clone(),
        callback_ports: vec![port],
    };
    let manual = format!("cannot forward port {port}");
    match request_forward(request, &[port], &manual, env).await {
        Some(true) => {
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
/// reply has a forward id. Returns `None` on failure, else whether a keeper
/// started.
async fn request_forward<W: Write>(
    request: Msg,
    ports: &[u16],
    no_tunnel: &str,
    env: &mut ShimEnv<W>,
) -> Option<bool> {
    let (mut conn, socket) =
        match find_tunnel_path(&env.tunnel_dir, env.socket_override.clone()).await {
            Ok(found) => found,
            Err(e) => {
                if let Some(mismatch) = e.mismatch {
                    let _ = writeln!(env.stderr, "rowser: {mismatch}");
                }
                let _ = writeln!(env.stderr, "rowser: no tunnel; {no_tunnel}");
                return None;
            }
        };
    if let Err(e) = conn.send(&request).await {
        let _ = writeln!(env.stderr, "rowser: {e}");
        return None;
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
            return None;
        }
        Ok(None) => {
            let _ = writeln!(env.stderr, "rowser: daemon closed the connection");
            return None;
        }
        Err(e) => {
            let _ = writeln!(env.stderr, "rowser: {e}");
            return None;
        }
    };
    for w in warnings {
        let _ = writeln!(env.stderr, "rowser: warning: {w}");
    }
    if let Some(e) = error {
        let _ = writeln!(env.stderr, "rowser: {e}");
        return None;
    }
    let (Some(id), Some(exe)) = (forward_id, env.keeper_exe.as_deref()) else {
        return Some(false);
    };
    match spawn_keeper(exe, &socket, &id, ports) {
        Ok(()) => Some(true),
        Err(e) => {
            let _ = writeln!(
                env.stderr,
                "rowser: warning: could not start the callback forward: {e}"
            );
            Some(false)
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
