//! The Shim's check of its socket: `ROWSER_SOCKET`, else the Outpost socket.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
        write!(f, "no Link")
    }
}

impl std::error::Error for NoTunnel {}

/// Connects to the first of `paths` that completes the handshake, and
/// returns the connection and its path.
pub async fn find_socket(paths: &[PathBuf]) -> Result<(Conn<UnixStream>, PathBuf), NoTunnel> {
    let mut mismatch = None;
    for path in paths {
        match probe(path).await {
            Ok(conn) => return Ok((conn, path.clone())),
            Err(Some(e)) => {
                mismatch.get_or_insert(e);
            }
            Err(None) => {}
        }
    }
    Err(NoTunnel { mismatch })
}

/// `Err(Some(..))`: the peer speaks another major protocol version.
async fn probe(path: &Path) -> Result<Conn<UnixStream>, Option<Error>> {
    let stream = UnixStream::connect(path).await.map_err(|_| None)?;
    match tokio::time::timeout(PROBE_TIMEOUT, Conn::connect(stream, Hello::current())).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e @ Error::VersionMismatch { .. })) => Err(Some(e)),
        _ => Err(None),
    }
}
