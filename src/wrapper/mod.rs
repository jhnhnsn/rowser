//! The Wrapper: `rowser ssh` with a per-session socket and a Host stamp.

mod args;

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};

use crate::protocol::{Conn, Hello, Msg};
pub use args::destination;

/// How long the Daemon's relay socket has to answer the handshake.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// Where the Wrapper finds ssh, the Daemon and a place for its socket.
#[derive(Debug, Clone)]
pub struct WrapperEnv {
    /// The ssh program to run.
    pub ssh: PathBuf,
    /// The Daemon's relay socket.
    pub relay_socket: PathBuf,
    /// The directory for the per-session socket (the Daemon's runtime directory).
    pub session_dir: PathBuf,
}

/// `rowser ssh <args…>` from the command line.
pub fn run(args: Vec<OsString>) -> ExitCode {
    let dir = match crate::platform::runtime_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("rowser ssh: {e}; running ssh without the URL forward");
            return plain_ssh(Path::new("ssh"), &args);
        }
    };
    run_ssh(
        args,
        WrapperEnv {
            ssh: "ssh".into(),
            relay_socket: dir.join(crate::daemon::RELAY_SOCKET),
            session_dir: dir,
        },
    )
}

/// Runs ssh with a Tunnel from a random remote Session socket to a
/// per-session local socket, relays each Tunnel connection to the Daemon's
/// relay socket with the Host stamp, and returns ssh's exit code.
pub fn run_ssh(args: Vec<OsString>, env: WrapperEnv) -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rowser ssh: {e}");
            return ExitCode::from(255);
        }
    };
    runtime.block_on(run_async(args, env))
}

async fn run_async(args: Vec<OsString>, env: WrapperEnv) -> ExitCode {
    if !daemon_answers(&env.relay_socket).await {
        eprintln!(
            "rowser ssh: the daemon is not running (no answer on {}); running ssh without the URL forward",
            env.relay_socket.display()
        );
        return supervise(&env.ssh, &args).await;
    }
    let alias = resolve_alias(&env.ssh, &args).await;
    let token = random_token();
    let local = env.session_dir.join(format!("session-{token}.sock"));
    let listener = match bind_session(&local) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "rowser ssh: could not create {}: {e}; running ssh without the URL forward",
                local.display()
            );
            return supervise(&env.ssh, &args).await;
        }
    };
    let mut full = vec![
        OsString::from("-R"),
        OsString::from(format!("/tmp/rowser-{token}.sock:{}", local.display())),
    ];
    full.extend(args);
    let relay = env.relay_socket.clone();
    let accept = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let relay = relay.clone();
            let alias = alias.clone();
            tokio::spawn(async move {
                if let Err(e) = relay_conn(stream, &relay, alias).await {
                    tracing::debug!("relay ended: {e}");
                }
            });
        }
    });
    let code = supervise(&env.ssh, &full).await;
    accept.abort();
    let _ = std::fs::remove_file(&local);
    code
}

async fn daemon_answers(relay: &Path) -> bool {
    let attempt = async {
        let stream = UnixStream::connect(relay).await.ok()?;
        Conn::connect(stream, Hello::current()).await.ok()
    };
    matches!(
        tokio::time::timeout(ANSWER_TIMEOUT, attempt).await,
        Ok(Some(_))
    )
}

/// The Host alias: the `host` line of `ssh -G <args>`, else the argv destination.
async fn resolve_alias(ssh: &Path, args: &[OsString]) -> String {
    let out = tokio::process::Command::new(ssh)
        .arg("-G")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await;
    if let Ok(out) = out {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(host) = text
                .lines()
                .find_map(|l| l.strip_prefix("host "))
                .map(str::trim)
                .filter(|h| !h.is_empty())
            {
                return host.to_string();
            }
        }
    }
    destination(args).unwrap_or_else(|| "unknown".into())
}

fn bind_session(path: &Path) -> std::io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// 16 hex characters from the OS random source.
fn random_token() -> String {
    let mut bytes = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Completes the Shim's handshake, opens a stamped connection to the Daemon,
/// then splices bytes both ways.
async fn relay_conn(shim: UnixStream, relay: &Path, alias: String) -> crate::protocol::Result<()> {
    let (shim, _) = Conn::accept(shim).await?;
    let daemon = UnixStream::connect(relay).await?;
    let mut daemon = Conn::connect(daemon, Hello::current()).await?;
    daemon.send(&Msg::HostStamp { alias }).await?;
    let (mut shim, shim_extra) = shim.into_raw();
    let (mut daemon, daemon_extra) = daemon.into_raw();
    daemon.write_all(&shim_extra).await?;
    shim.write_all(&daemon_extra).await?;
    tokio::io::copy_bidirectional(&mut shim, &mut daemon).await?;
    Ok(())
}

fn plain_ssh(ssh: &Path, args: &[OsString]) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(r) => r.block_on(supervise(ssh, args)),
        Err(e) => {
            eprintln!("rowser ssh: {e}");
            ExitCode::from(255)
        }
    }
}

/// Runs ssh as a child and returns its exit code (128 + signal if killed).
/// SIGINT and SIGQUIT reach ssh from the terminal, so the Wrapper only
/// survives them; SIGTERM and SIGHUP are passed on to ssh.
async fn supervise(ssh: &Path, args: &[OsString]) -> ExitCode {
    use std::os::unix::process::ExitStatusExt;
    let mut child = match tokio::process::Command::new(ssh).args(args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rowser ssh: could not run {}: {e}", ssh.display());
            return ExitCode::from(255);
        }
    };
    let pid = child.id().map(|p| p as libc::pid_t);
    let mut int = signal(SignalKind::interrupt()).ok();
    let mut quit = signal(SignalKind::quit()).ok();
    let mut term = signal(SignalKind::terminate()).ok();
    let mut hup = signal(SignalKind::hangup()).ok();
    let status = loop {
        tokio::select! {
            s = child.wait() => break s,
            _ = recv(&mut int) => {}
            _ = recv(&mut quit) => {}
            _ = recv(&mut term) => forward(pid, libc::SIGTERM),
            _ = recv(&mut hup) => forward(pid, libc::SIGHUP),
        }
    };
    match status {
        Ok(s) => match (s.code(), s.signal()) {
            (Some(c), _) => ExitCode::from(c as u8),
            (None, Some(sig)) => ExitCode::from((128 + sig) as u8),
            _ => ExitCode::from(255),
        },
        Err(e) => {
            eprintln!("rowser ssh: {e}");
            ExitCode::from(255)
        }
    }
}

async fn recv(s: &mut Option<tokio::signal::unix::Signal>) {
    match s {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

fn forward(pid: Option<libc::pid_t>, sig: libc::c_int) {
    if let Some(pid) = pid {
        // SAFETY: kill has no memory-safety preconditions.
        unsafe {
            libc::kill(pid, sig);
        }
    }
}
