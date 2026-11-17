//! The Shim's search for a working Session socket.

use std::fmt;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::net::UnixStream;

use crate::protocol::{Conn, Hello};

/// How long a socket gets to finish the handshake before the next is tried.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// No socket answered the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoTunnel;

impl fmt::Display for NoTunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no tunnel")
    }
}

impl std::error::Error for NoTunnel {}

/// Finds a working Session socket: `env_override` first, then every
/// `rowser-*.sock` in `dir` owned by the own UID, newest first by mtime.
/// Returns the first connection that completes the handshake. Sockets from
/// `dir` that refuse connections are Stale sockets and are removed.
pub async fn find_tunnel(
    dir: &Path,
    env_override: Option<PathBuf>,
) -> Result<Conn<UnixStream>, NoTunnel> {
    if let Some(path) = env_override {
        if let Probe::Ok(conn) = probe(&path).await {
            return Ok(conn);
        }
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    for path in candidates(dir, uid) {
        match probe(&path).await {
            Probe::Ok(conn) => return Ok(conn),
            Probe::Refused => {
                let _ = std::fs::remove_file(&path);
            }
            Probe::Failed => {}
        }
    }
    Err(NoTunnel)
}

enum Probe {
    Ok(Conn<UnixStream>),
    /// Nothing listens on the socket.
    Refused,
    /// Something listens but did not complete the handshake.
    Failed,
}

async fn probe(path: &Path) -> Probe {
    let stream = match UnixStream::connect(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => return Probe::Refused,
        Err(_) => return Probe::Failed,
    };
    match tokio::time::timeout(PROBE_TIMEOUT, Conn::connect(stream, Hello::current())).await {
        Ok(Ok(conn)) => Probe::Ok(conn),
        _ => Probe::Failed,
    }
}

/// The `rowser-*.sock` sockets in `dir` owned by `uid`, newest first.
fn candidates(dir: &Path, uid: u32) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("rowser-") && name.ends_with(".sock")
        })
        .filter_map(|e| {
            let path = e.path();
            // symlink_metadata: a symlink is not followed to someone else's socket.
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if !meta.file_type().is_socket() || meta.uid() != uid {
                return None;
            }
            Some((meta.modified().ok()?, path))
        })
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_of_another_uid_are_ignored() {
        let dir = PathBuf::from(format!("/tmp/rw.du.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rowser-a.sock");
        let _l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        assert_eq!(candidates(&dir, uid), vec![path]);
        assert!(candidates(&dir, uid.wrapping_add(1)).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
