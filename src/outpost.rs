//! The Outpost (`rowser __outpost`): the far end of a Link on a Saved host.
//!
//! It speaks the protocol with the Daemon over its stdin and stdout
//! (`yamux`, ADR 0010), listens on the Outpost socket, and relays each Shim
//! connection to the Daemon as a new stream. It exits when stdio closes.

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;

use crate::mux::{self, Side};
use crate::protocol::{Conn, Hello, Msg};

/// File name of the Outpost socket in its folder.
pub const OUTPOST_SOCKET: &str = "outpost.sock";

/// Hidden test hook: replaces the Outpost socket folder
/// `/tmp/rowser-<uid>`, so a test never touches the real one.
pub const TEST_DIR_VAR: &str = "ROWSER_TEST_OUTPOST_DIR";

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The Outpost socket folder: `/tmp/rowser-<uid>`, from the UID alone
/// (only the hidden test hook replaces it).
pub fn outpost_dir() -> PathBuf {
    match std::env::var_os(TEST_DIR_VAR).filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(format!("/tmp/rowser-{}", euid())),
    }
}

/// The Outpost socket path in `dir`.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join(OUTPOST_SOCKET)
}

/// Creates `dir` with mode 0700 when it is missing. An existing `dir` must
/// belong to the current user and have mode 0700, else it is refused.
pub fn prepare_dir(dir: &Path) -> Result<(), String> {
    if let Err(e) = std::fs::DirBuilder::new().mode(0o700).create(dir) {
        if e.kind() != io::ErrorKind::AlreadyExists {
            return Err(format!("cannot create {}: {e}", dir.display()));
        }
    }
    // symlink_metadata: a symlink to another folder is refused too.
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    if meta.uid() != euid() {
        return Err(format!(
            "{} does not belong to the current user; remove it and connect again",
            dir.display()
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(format!(
            "{} has mode {mode:03o}; it must be 0700",
            dir.display()
        ));
    }
    Ok(())
}

/// The Shim's check before it connects to the Outpost socket at `path`: its
/// folder must belong to the current user with mode 0700, and the socket
/// must belong to the current user. Another user who made the folder first
/// gets no URLs (ADR 0010).
pub fn check_socket(path: &Path) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    if meta.uid() != euid() {
        return Err(format!(
            "{} does not belong to the current user",
            dir.display()
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(format!(
            "{} has mode {mode:03o}; it must be 0700",
            dir.display()
        ));
    }
    let sock = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if sock.uid() != euid() {
        return Err(format!(
            "{} does not belong to the current user",
            path.display()
        ));
    }
    Ok(())
}

/// Runs the Outpost over `io` with its socket in `dir` until `io` closes.
/// The socket is removed at the end.
pub async fn serve<T>(io: T, dir: &Path) -> Result<(), String>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    prepare_dir(dir)?;
    let path = socket_path(dir);
    // The Link that connected last takes the Open requests.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    let listener = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let ino = std::fs::symlink_metadata(&path).map(|m| m.ino()).ok();

    let (opener, mut inbound, driver) = mux::start(io, Side::Outpost);
    let accept = async {
        loop {
            match listener.accept().await {
                Ok((mut shim, _)) => {
                    let opener = opener.clone();
                    tokio::spawn(async move {
                        match opener.open().await {
                            Ok(mut stream) => {
                                let _ = tokio::io::copy_bidirectional(&mut shim, &mut stream).await;
                            }
                            Err(e) => tracing::debug!("no stream for a Shim: {e}"),
                        }
                    });
                }
                Err(e) => tracing::warn!("accept on {}: {e}", path.display()),
            }
        }
    };
    let control = async {
        while let Some(stream) = inbound.recv().await {
            tokio::spawn(async move {
                if let Err(e) = answer_control(stream).await {
                    tracing::debug!("control stream ended: {e}");
                }
            });
        }
    };
    tokio::select! {
        _ = accept => {}
        _ = control => {}
        _ = driver => {}
    }
    // Remove the socket only when it is still ours: a newer Link's Outpost
    // may have bound the same path.
    if std::fs::symlink_metadata(&path).map(|m| m.ino()).ok() == ino {
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}

/// A Daemon-to-Outpost control exchange: answers `Ping`.
async fn answer_control(stream: mux::Stream) -> crate::protocol::Result<()> {
    let (mut conn, _) = Conn::accept(stream).await?;
    while let Some(msg) = conn.recv().await? {
        let reply = match msg {
            Msg::Ping => Msg::Pong {
                version: Hello::current().version(),
                rowser_version: crate::protocol::rowser_version(),
            },
            _ => Msg::OpenResult {
                forward_id: None,
                warnings: vec![],
                error: Some("unexpected message".into()),
            },
        };
        conn.send(&reply).await?;
    }
    Ok(())
}

/// `rowser __outpost` for the binary.
pub fn outpost_main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rowser __outpost: {e}");
            return ExitCode::FAILURE;
        }
    };
    let io = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    let result = runtime.block_on(serve(io, &outpost_dir()));
    // A blocking stdin read must not hold the exit.
    runtime.shutdown_timeout(std::time::Duration::from_millis(100));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rowser __outpost: {e}");
            ExitCode::FAILURE
        }
    }
}
