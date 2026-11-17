//! The Shim: `rowser open` on the Remote host.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use url::Url;

use crate::discovery::find_tunnel;
use crate::protocol::Msg;

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
            stderr: std::io::stderr(),
        }
    }
}

/// Sends `url` to the Daemon through a Tunnel. Exits 0 when the Daemon
/// accepts it, 1 otherwise.
pub async fn run_open<W: Write>(url: &Url, env: &mut ShimEnv<W>) -> ExitCode {
    let no_tunnel = |env: &mut ShimEnv<W>| {
        let _ = writeln!(
            env.stderr,
            "rowser: no tunnel; open this URL manually: {url}"
        );
        ExitCode::FAILURE
    };
    let Ok(mut conn) = find_tunnel(&env.tunnel_dir, env.socket_override.clone()).await else {
        return no_tunnel(env);
    };
    let request = Msg::Open {
        url: url.to_string(),
        reported_host: env.reported_host.clone(),
        callback_ports: vec![],
    };
    if let Err(e) = conn.send(&request).await {
        let _ = writeln!(env.stderr, "rowser: {e}");
        return no_tunnel(env);
    }
    match conn.recv().await {
        Ok(Some(Msg::OpenResult {
            warnings, error, ..
        })) => {
            for w in warnings {
                let _ = writeln!(env.stderr, "rowser: warning: {w}");
            }
            match error {
                None => ExitCode::SUCCESS,
                Some(e) => {
                    let _ = writeln!(env.stderr, "rowser: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(Some(other)) => {
            let _ = writeln!(
                env.stderr,
                "rowser: unexpected reply from daemon: {other:?}"
            );
            ExitCode::FAILURE
        }
        Ok(None) => {
            let _ = writeln!(env.stderr, "rowser: daemon closed the connection");
            ExitCode::FAILURE
        }
        Err(e) => {
            let _ = writeln!(env.stderr, "rowser: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `rowser open <url>` for the binary.
pub fn open_main(url: &Url) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(run_open(url, &mut ShimEnv::from_process())),
        Err(e) => {
            eprintln!("rowser: {e}");
            ExitCode::FAILURE
        }
    }
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
