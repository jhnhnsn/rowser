//! The Shim's search for a working Session socket.

use std::fmt;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::net::UnixStream;

use crate::protocol::{Conn, Error, Hello};

/// How long a socket gets to finish the handshake before the next is tried.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// No socket answered the handshake.
#[derive(Debug)]
pub struct NoTunnel {
    /// The first `Error::VersionMismatch` a socket gave, if any.
    pub mismatch: Option<Error>,
}

impl fmt::Display for NoTunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no tunnel")
    }
}

/// The UID that owns the sockets we may use. The effective UID, as in
/// `daemon::bind_sockets`: it is the UID that creates and owns our files.
fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
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
    find_tunnel_path(dir, env_override).await.map(|(c, _)| c)
}

/// `find_tunnel`, and also the path of the socket that answered.
pub async fn find_tunnel_path(
    dir: &Path,
    env_override: Option<PathBuf>,
) -> Result<(Conn<UnixStream>, PathBuf), NoTunnel> {
    let mut mismatch = None;
    if let Some(path) = env_override {
        match probe(&path).await {
            Probe::Ok(conn) => return Ok((conn, path)),
            Probe::Mismatch(e) => mismatch = Some(e),
            Probe::Refused | Probe::Failed => {}
        }
    }
    for c in candidates(dir, own_uid()) {
        match probe(&c.path).await {
            Probe::Ok(conn) => return Ok((conn, c.path)),
            Probe::Refused => remove_if_same(&c),
            Probe::Mismatch(e) => {
                mismatch.get_or_insert(e);
            }
            Probe::Failed => {}
        }
    }
    Err(NoTunnel { mismatch })
}

/// Removes a Stale socket only if the path still names the same inode: a new
/// session can bind the same path between the probe and the removal.
fn remove_if_same(c: &Candidate) {
    let Ok(meta) = std::fs::symlink_metadata(&c.path) else {
        return;
    };
    if meta.dev() == c.dev && meta.ino() == c.ino {
        let _ = std::fs::remove_file(&c.path);
    }
}

enum Probe {
    Ok(Conn<UnixStream>),
    /// Nothing listens on the socket.
    Refused,
    /// The peer speaks another major protocol version.
    Mismatch(Error),
    /// Something listens but did not complete the handshake.
    Failed,
}

#[derive(Debug, PartialEq, Eq)]
struct Candidate {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

async fn probe(path: &Path) -> Probe {
    let stream = match UnixStream::connect(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => return Probe::Refused,
        Err(_) => return Probe::Failed,
    };
    match tokio::time::timeout(PROBE_TIMEOUT, Conn::connect(stream, Hello::current())).await {
        Ok(Ok(conn)) => Probe::Ok(conn),
        Ok(Err(e @ Error::VersionMismatch { .. })) => Probe::Mismatch(e),
        _ => Probe::Failed,
    }
}

/// The `rowser-*.sock` sockets in `dir` owned by `uid`, newest first.
fn candidates(dir: &Path, uid: u32) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<(SystemTime, Candidate)> = entries
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
            let candidate = Candidate {
                path,
                dev: meta.dev(),
                ino: meta.ino(),
            };
            Some((meta.modified().ok()?, candidate))
        })
        .collect();
    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    found.into_iter().map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpDir(PathBuf);

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp_dir(tag: &str) -> TmpDir {
        let dir = PathBuf::from(format!("/tmp/rw.d{tag}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }

    fn paths(cs: Vec<Candidate>) -> Vec<PathBuf> {
        cs.into_iter().map(|c| c.path).collect()
    }

    #[test]
    fn sockets_of_another_uid_are_ignored() {
        let dir = tmp_dir("u");
        let path = dir.0.join("rowser-a.sock");
        let _l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let uid = own_uid();
        assert_eq!(paths(candidates(&dir.0, uid)), vec![path]);
        assert!(candidates(&dir.0, uid.wrapping_add(1)).is_empty());
    }

    #[test]
    fn a_socket_rebound_after_the_probe_is_not_removed() {
        let dir = tmp_dir("r");
        let path = dir.0.join("rowser-a.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let old = candidates(&dir.0, own_uid()).remove(0);
        // A new session replaces the socket at the same path. The hard link
        // keeps the old inode alive, so the new socket cannot reuse its number.
        std::fs::hard_link(&path, dir.0.join("keep")).unwrap();
        std::fs::remove_file(&path).unwrap();
        let _l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        remove_if_same(&old);
        assert!(path.exists(), "the new socket stays");
    }
}
