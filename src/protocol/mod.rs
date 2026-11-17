//! Wire messages, NDJSON framing and the version handshake.

use std::fmt;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Longest accepted line, in bytes, without the trailing newline.
const MAX_LINE: usize = 64 * 1024;

/// The protocol version this build speaks.
const VERSION: Version = Version { major: 1, minor: 0 };

/// A protocol version `major.minor`. Peers with the same major interoperate.
///
/// On the wire it is a JSON object, `{"major":1,"minor":0}`, not a `"1.0"`
/// string: this is deliberate, so peers compare numbers and never parse text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    major: u32,
    minor: u32,
}

impl Version {
    pub fn new(major: u32, minor: u32) -> Version {
        Version { major, minor }
    }

    pub fn major(self) -> u32 {
        self.major
    }

    pub fn minor(self) -> u32 {
        self.minor
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The first message on every connection, sent by both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    version: Version,
}

impl Hello {
    /// The hello for the protocol version this build speaks.
    pub fn current() -> Hello {
        Hello { version: VERSION }
    }

    /// A hello that claims `version`. Use it only to test version mismatches.
    pub fn with_version(version: Version) -> Hello {
        Hello { version }
    }

    pub fn version(&self) -> Version {
        self.version
    }
}

/// Every message after the handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Msg {
    HostStamp {
        alias: String,
    },
    Open {
        url: String,
        reported_host: String,
        callback_ports: Vec<u16>,
    },
    /// `rowser forward`: a Callback forward without a URL. The reply is an
    /// `OpenResult`.
    Forward {
        reported_host: String,
        callback_ports: Vec<u16>,
    },
    OpenResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forward_id: Option<String>,
        #[serde(default)]
        warnings: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    AttachForward {
        forward_id: String,
    },
    Dial {
        conn_id: u64,
        port: u16,
    },
    DataConn {
        conn_id: u64,
    },
    Ping,
    Pong {
        version: Version,
    },
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// A line exceeded the maximum line length. The rest of that line is
    /// still unread, so the connection is unusable: every later `send` and
    /// `recv` returns this error too.
    LineTooLong,
    /// A line was not a valid message.
    Malformed(serde_json::Error),
    /// The peer closed the connection before the handshake finished.
    Closed,
    /// The peers speak different major versions.
    VersionMismatch {
        local: Version,
        remote: Version,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "connection error: {e}"),
            Error::LineTooLong => write!(f, "message longer than {MAX_LINE} bytes"),
            Error::Malformed(e) => write!(f, "malformed message: {e}"),
            Error::Closed => write!(f, "connection closed during handshake"),
            Error::VersionMismatch { local, remote } => write!(
                f,
                "protocol version mismatch: this rowser speaks {local}, the peer speaks {remote}; \
                 update the remote with `rowser remote install <host>` or reinstall rowser locally"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A framed connection after a successful handshake.
pub struct Conn<S> {
    stream: BufReader<S>,
    /// Set after `LineTooLong`: the stream is mid-line and cannot be framed.
    broken: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    fn new(stream: S) -> Conn<S> {
        Conn {
            stream: BufReader::new(stream),
            broken: false,
        }
    }

    /// Sends `hello`, then checks the peer's reply.
    pub async fn connect(stream: S, hello: Hello) -> Result<Conn<S>> {
        let mut conn = Conn::new(stream);
        conn.write(&hello).await?;
        let peer: Hello = conn.read().await?.ok_or(Error::Closed)?;
        require_same_major(hello.version, peer.version)?;
        Ok(conn)
    }

    /// Reads the peer's hello and replies with ours, even on a major mismatch
    /// so that the peer can report it too.
    pub async fn accept(stream: S) -> Result<(Conn<S>, Hello)> {
        let mut conn = Conn::new(stream);
        let peer: Hello = conn.read().await?.ok_or(Error::Closed)?;
        conn.write(&Hello::current()).await?;
        require_same_major(VERSION, peer.version)?;
        Ok((conn, peer))
    }

    pub async fn send(&mut self, msg: &Msg) -> Result<()> {
        self.write(msg).await
    }

    /// Returns `None` when the peer closed the connection.
    pub async fn recv(&mut self) -> Result<Option<Msg>> {
        self.read().await
    }

    /// Hands back the stream and any bytes already read past the last message.
    pub fn into_raw(self) -> (S, Vec<u8>) {
        let buffered = self.stream.buffer().to_vec();
        (self.stream.into_inner(), buffered)
    }

    async fn write<T: Serialize>(&mut self, value: &T) -> Result<()> {
        if self.broken {
            return Err(Error::LineTooLong);
        }
        let mut line = serde_json::to_vec(value).map_err(Error::Malformed)?;
        line.push(b'\n');
        self.stream.write_all(&line).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn read<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        if self.broken {
            return Err(Error::LineTooLong);
        }
        let mut line = Vec::new();
        (&mut self.stream)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if line.is_empty() {
            return Ok(None);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.len() > MAX_LINE {
            self.broken = true;
            return Err(Error::LineTooLong);
        }
        serde_json::from_slice(&line)
            .map(Some)
            .map_err(Error::Malformed)
    }
}

fn require_same_major(local: Version, remote: Version) -> Result<()> {
    if local.major == remote.major {
        Ok(())
    } else {
        Err(Error::VersionMismatch { local, remote })
    }
}
