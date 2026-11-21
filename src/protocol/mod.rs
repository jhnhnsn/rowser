//! Wire messages, NDJSON framing and the version handshake.

use std::fmt;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Longest accepted line, in bytes, without the trailing newline.
const MAX_LINE: usize = 64 * 1024;

/// File name of the control socket in the runtime directory (ADR 0006).
/// It is here, not in `daemon`, so that the client side needs only `protocol`.
pub const CONTROL_SOCKET: &str = "rowserd-control.sock";

/// `text` without control characters and bidi controls, so a forged host
/// name or URL cannot move or hide text it is shown next to.
pub fn printable(text: &str) -> String {
    let bidi = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    text.chars()
        .filter(|&c| !c.is_control() && !bidi(c))
        .collect()
}

/// The protocol version this build speaks.
const VERSION: Version = Version { major: 1, minor: 1 };

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

    /// True for 1.1 and later of the same major: the peer knows Sessions.
    pub fn has_sessions(self) -> bool {
        self.major == VERSION.major && self.minor >= 1
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
    /// `OpenResult`. `reported_host` plays the same part as in `Open`: the
    /// notice shows it, and `--confirm-unverified` matches it.
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
    /// A keeper's data connection for the browser connection `conn_id` of
    /// the forward `forward_id`. The Daemon accepts it only when both match.
    DataConn {
        forward_id: String,
        conn_id: u64,
    },
    /// Control socket only (1.1): the Wrapper registers its Session for the
    /// Host alias `host` it got from `ssh -G`. The reply is an `OpenResult`.
    /// The Session lasts until the connection closes; no other message
    /// follows. The relay socket refuses it, so a Remote host cannot add one.
    Session {
        host: String,
    },
    Ping,
    Pong {
        version: Version,
    },
    /// Control socket only (1.1): `rowser status`. The reply is a `StatusReply`.
    Status,
    /// Control socket only (1.1): `rowser reload`, the Daemon reads the
    /// config file again. The reply is `Reloaded`, or an `OpenResult` with
    /// the error when the file is invalid; the Daemon then keeps its Routes.
    Reload,
    /// The Daemon's answer to a valid `Reload`: the number of Routes it read.
    Reloaded {
        routes: usize,
    },
    /// The Daemon's answer to `Status`.
    StatusReply {
        daemon_version: String,
        protocol_version: Version,
        /// The config file path the Daemon resolved; `None` when no path
        /// resolved.
        #[serde(default)]
        config_file: Option<String>,
        /// True when a file was at `config_file` when the Daemon started.
        #[serde(default)]
        config_file_exists: bool,
        paused: bool,
        /// When a `rowser pause --for` ends, in seconds since the Unix epoch.
        #[serde(default)]
        paused_until: Option<u64>,
        #[serde(default)]
        forwards: Vec<ForwardInfo>,
        #[serde(default)]
        sessions: Vec<SessionInfo>,
    },
    /// Control socket only (1.1): `rowser pause [--for]`. The Daemon refuses
    /// each Open request and `rowser forward` until `Resume`, until
    /// `for_secs` pass, or until it restarts. The reply is `Paused`.
    Pause {
        #[serde(default)]
        for_secs: Option<u64>,
    },
    /// The Daemon's answer to `Pause`: when the pause ends, in seconds since
    /// the Unix epoch; `None` until `Resume`.
    Paused {
        until: Option<u64>,
    },
    /// Control socket only (1.1): `rowser resume`. The reply is `Resumed`.
    Resume,
    /// The Daemon's answer to `Resume`: false when it was not paused.
    Resumed {
        was_paused: bool,
    },
    /// Control socket only (1.1): `rowser activity`. The reply is an
    /// `ActivityReply`; with `follow`, an `ActivityUpdate` follows for each
    /// new or changed entry until the client closes the connection.
    Activity {
        #[serde(default)]
        follow: bool,
    },
    /// The Daemon's answer to `Activity`: the kept entries, oldest first.
    ActivityReply {
        entries: Vec<ActivityEntry>,
    },
    /// A new entry, or a changed one (same `id`) when its Callback forward
    /// closes.
    ActivityUpdate {
        entry: Box<ActivityEntry>,
    },
    /// Control socket only (1.1): `rowser pending`. The reply is
    /// `PendingReply`.
    Pending,
    /// The Daemon's answer to `Pending`: the requests that wait for an
    /// answer, oldest first.
    PendingReply {
        requests: Vec<PendingInfo>,
    },
    /// Control socket only (1.1): `rowser approve <id> [--open <target>]`
    /// (`approve`) or `rowser deny <id>`. The reply is `Answered`, or an
    /// `OpenResult` error for an unknown or already answered id, or an
    /// `open` Target that cannot be used.
    AnswerPending {
        id: u64,
        approve: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<String>,
    },
    /// The Daemon's answer to a valid `AnswerPending`.
    Answered {
        id: u64,
    },
}

/// One request that waits for an answer, as `rowser pending` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInfo {
    pub id: u64,
    pub kind: RequestKind,
    /// The Host alias, or the Reported host name when `unverified`.
    pub host: String,
    pub unverified: bool,
    #[serde(default)]
    pub url: Option<String>,
    /// The Callback ports of a `rowser forward` request.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// The Target an Open request opens in; `None` for a forward.
    #[serde(default)]
    pub target: Option<String>,
    /// Seconds since the request started to wait.
    pub age_secs: u64,
}

/// Who answered a request that waited for an answer, in `rowser activity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsweredBy {
    /// `rowser approve` or `rowser deny`.
    Terminal,
    /// The desktop notification or dialog.
    Desktop,
}

/// What a request is, in `rowser activity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Open,
    Forward,
}

/// How the Daemon answered a request, in `rowser activity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The URL opened, or the Callback forward started.
    Opened,
    Denied,
    TimedOut,
    RateLimited,
    SchemeRefused,
    /// The Opener failed.
    BrowserMissing,
    /// Refused because of `rowser pause`.
    Paused,
}

impl Outcome {
    /// The words `rowser activity` prints.
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Opened => "opened",
            Outcome::Denied => "denied",
            Outcome::TimedOut => "timed out",
            Outcome::RateLimited => "rate limited",
            Outcome::SchemeRefused => "scheme refused",
            Outcome::BrowserMissing => "browser missing",
            Outcome::Paused => "paused",
        }
    }
}

/// One Open request or `rowser forward` request, as `rowser activity`
/// shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Counts up from 1 for each request since the Daemon started.
    pub id: u64,
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub kind: RequestKind,
    /// The Host alias, or the Reported host name when `unverified`.
    pub host: String,
    pub unverified: bool,
    #[serde(default)]
    pub url: Option<String>,
    /// The Route that matched: `route <n>` (from 1, in file order),
    /// `[unverified]` or `[default]`. `None` for a forward, or a request
    /// refused before Route matching.
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    pub result: Outcome,
    /// The local ports of its Callback forward.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// How long the Callback forward stayed open; `None` while it is open.
    #[serde(default)]
    pub open_secs: Option<u64>,
    /// Notice warnings, such as a Target that fell back to `[default]`.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Who answered, for a request that waited for an answer; `None` when
    /// none answered (no wait, or a timeout).
    #[serde(default)]
    pub answered_by: Option<AnsweredBy>,
}

/// One connected Wrapper, as `rowser status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The Host alias the Wrapper got from `ssh -G`.
    pub host: String,
    /// Seconds since the Session registered.
    pub age_secs: u64,
    /// The Target the Host alias's Routes give an `https` URL that no
    /// `match` list names.
    pub target: String,
}

impl Msg {
    /// True for the messages that only the control socket accepts.
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            Msg::Status
                | Msg::StatusReply { .. }
                | Msg::Session { .. }
                | Msg::Reload
                | Msg::Reloaded { .. }
                | Msg::Activity { .. }
                | Msg::ActivityReply { .. }
                | Msg::ActivityUpdate { .. }
                | Msg::Pause { .. }
                | Msg::Paused { .. }
                | Msg::Resume
                | Msg::Resumed { .. }
                | Msg::Pending
                | Msg::PendingReply { .. }
                | Msg::AnswerPending { .. }
                | Msg::Answered { .. }
        )
    }
}

/// One local port of an open Callback forward, as `rowser status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardInfo {
    pub port: u16,
    /// The Host alias, or the Reported host name when `unverified`.
    pub host: String,
    pub unverified: bool,
    pub age_secs: u64,
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
    /// The peer's hello version, set by the handshake.
    peer: Version,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    fn new(stream: S) -> Conn<S> {
        Conn {
            stream: BufReader::new(stream),
            broken: false,
            peer: VERSION,
        }
    }

    /// Sends `hello`, then checks the peer's reply.
    pub async fn connect(stream: S, hello: Hello) -> Result<Conn<S>> {
        let mut conn = Conn::new(stream);
        conn.write(&hello).await?;
        let peer: Hello = conn.read().await?.ok_or(Error::Closed)?;
        require_same_major(hello.version, peer.version)?;
        conn.peer = peer.version;
        Ok(conn)
    }

    /// The protocol version the peer sent in its hello.
    pub fn peer_version(&self) -> Version {
        self.peer
    }

    /// Reads the peer's hello and replies with ours, even on a major mismatch
    /// so that the peer can report it too.
    pub async fn accept(stream: S) -> Result<(Conn<S>, Hello)> {
        let mut conn = Conn::new(stream);
        let peer: Hello = conn.read().await?.ok_or(Error::Closed)?;
        conn.write(&Hello::current()).await?;
        require_same_major(VERSION, peer.version)?;
        conn.peer = peer.version;
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

    /// Ends the framing and relays bytes both ways between this stream and
    /// `other` until one side closes. Bytes already read past the last
    /// message go to `other` first.
    pub async fn splice<T: AsyncRead + AsyncWrite + Unpin>(
        self,
        other: &mut T,
    ) -> std::io::Result<()> {
        let (mut stream, extra) = self.into_raw();
        other.write_all(&extra).await?;
        tokio::io::copy_bidirectional(&mut stream, other).await?;
        Ok(())
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
