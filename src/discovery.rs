//! The Shim's check of its socket: `ROWSER_SOCKET`, else the Outpost socket.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use tokio::net::UnixStream;

use crate::protocol::{Conn, Error, Hello};

/// How long a socket gets to finish the handshake before the next is tried.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The socket did not answer the handshake, or there is no socket.
#[derive(Debug)]
pub struct NoLink {
    /// The `Error::VersionMismatch` the socket gave, if any.
    pub mismatch: Option<Error>,
}

impl fmt::Display for NoLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no Link")
    }
}

impl std::error::Error for NoLink {}

/// Connects to `path` and completes the handshake. `None`: no socket.
pub async fn find_socket(path: Option<&Path>) -> Result<Conn<UnixStream>, NoLink> {
    let Some(path) = path else {
        return Err(NoLink { mismatch: None });
    };
    probe(path).await.map_err(|mismatch| NoLink { mismatch })
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
