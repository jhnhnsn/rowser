//! The Daemon: serves Open requests on the direct and relay sockets, and
//! control commands on the control socket.

mod activity;
mod confirm;
mod control;
mod forward;
mod limit;
mod notifier;
mod opener;
mod session;
#[doc(hidden)]
pub mod testing;

use std::io;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use url::Url;

use crate::protocol::{ActivityEntry, Conn, Hello, Msg, Outcome, RequestKind};
use crate::routes::{RequestHost, Resolved, Routes};
use limit::Limit;
pub use notifier::{DesktopNotifier, Notice, NoticeHost, Notifier, Subject};
pub use opener::{CommandOpener, Opener};

/// File name of the direct socket in the runtime directory.
pub const DIRECT_SOCKET: &str = "rowserd.sock";
/// File name of the relay socket in the runtime directory.
pub const RELAY_SOCKET: &str = "rowserd-relay.sock";
pub use crate::protocol::CONTROL_SOCKET;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Most Open requests one connection may make in `rate_window`.
    pub rate_max: usize,
    pub rate_window: Duration,
    /// `--confirm-hosts` glob patterns (`*`, `?`; ASCII case is ignored).
    /// Open requests whose Host alias matches wait for `Notifier::confirm`.
    pub confirm_hosts: Vec<String>,
    /// `--confirm-unverified`: on the direct socket, also match the patterns
    /// against the Reported host name. Without it, requests on the direct
    /// socket never wait for confirmation.
    pub confirm_unverified: bool,
    /// How long a confirmation may take before the request is refused.
    pub confirm_timeout: Duration,
    /// How long a Callback forward lives at most.
    pub forward_timeout: Duration,
    /// Addresses the local side of a Callback forward binds. Only loopback
    /// addresses are used; `Daemon::new` drops any other.
    pub loopback: Vec<IpAddr>,
    /// The Routes from the config file (the spec's defaults without one).
    pub routes: Routes,
    /// The config file path the Daemon resolved, for `rowser status`.
    /// `None` when no path resolved.
    pub config_file: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            routes: Routes::default(),
            config_file: None,
            rate_max: 5,
            rate_window: Duration::from_secs(10),
            confirm_hosts: vec![],
            confirm_unverified: false,
            confirm_timeout: confirm::CONFIRM_TIMEOUT,
            forward_timeout: crate::callback::FORWARD_TIMEOUT,
            loopback: crate::callback::LOOPBACK.to_vec(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Socket {
    Direct,
    Relay,
    /// Local control commands only (ADR 0006).
    Control,
}

pub struct Daemon {
    inner: Arc<Inner>,
}

/// What a valid `rowser reload` replaces, as one value (ADR 0007).
struct Loaded {
    routes: Arc<Routes>,
    /// True when a file was at `config.config_file` at the last load.
    config_file_exists: bool,
}

struct Inner {
    /// The start-up config. `Daemon::new` moves its `routes` into `loaded`
    /// (leaving the default there), so no reader sees stale Routes: read
    /// `current_routes()` only.
    config: DaemonConfig,
    opener: Arc<dyn Opener>,
    notifier: Arc<dyn Notifier>,
    forwards: Arc<forward::Forwards>,
    sessions: Arc<session::Sessions>,
    /// Recent requests, for `rowser activity`.
    activity: Arc<activity::Activity>,
    /// The Routes in use and the file state: set at start, replaced by a
    /// valid `rowser reload`.
    loaded: RwLock<Loaded>,
    /// `rowser pause`: in memory only, so a Daemon restart ends it.
    pause: Mutex<Option<Pause>>,
}

/// A pause in effect.
struct Pause {
    /// For `rowser pause --for`: when it ends, on the tokio clock and in
    /// Unix seconds.
    end: Option<(tokio::time::Instant, u64)>,
}

impl Daemon {
    pub fn new(mut config: DaemonConfig, opener: impl Opener, notifier: impl Notifier) -> Daemon {
        // The spec binds the local side on loopback only.
        config.loopback.retain(IpAddr::is_loopback);
        let config_file_exists = config.config_file.as_deref().is_some_and(Path::is_file);
        Daemon {
            inner: Arc::new(Inner {
                loaded: RwLock::new(Loaded {
                    routes: Arc::new(std::mem::take(&mut config.routes)),
                    config_file_exists,
                }),
                config,
                opener: Arc::new(opener),
                notifier: Arc::new(notifier),
                forwards: Arc::default(),
                sessions: Arc::default(),
                activity: Arc::default(),
                pause: Mutex::default(),
            }),
        }
    }

    /// Accepts connections on the three sockets until an accept fails.
    pub async fn serve(
        self,
        direct: UnixListener,
        relay: UnixListener,
        control: UnixListener,
    ) -> io::Result<()> {
        loop {
            let (stream, socket) = tokio::select! {
                r = direct.accept() => (r?.0, Socket::Direct),
                r = relay.accept() => (r?.0, Socket::Relay),
                r = control.accept() => (r?.0, Socket::Control),
            };
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let result = match socket {
                    Socket::Control => inner.handle_control(stream).await,
                    socket => inner.handle(stream, socket).await,
                };
                if let Err(e) = result {
                    tracing::debug!("connection ended: {e}");
                }
            });
        }
    }
}

/// The error when `Notifier::confirm` gives `false` for an Open request.
pub const NOT_APPROVED: &str =
    "the URL was not approved: the user denied it, or the confirmation could not be shown (see the daemon log)";

/// The error when `Notifier::confirm` gives `false` for `rowser forward`.
pub const FORWARD_NOT_APPROVED: &str =
    "the forward was not approved: the user denied it, or the confirmation could not be shown (see the daemon log)";

/// The error for a control message on the direct or relay socket.
pub const CONTROL_ONLY: &str = "control messages are accepted only on the control socket";

/// The answer to `Ping` on every socket.
fn pong() -> Msg {
    Msg::Pong {
        version: Hello::current().version(),
    }
}

/// A failed `OpenResult` carrying `text`.
fn open_error(text: impl Into<String>) -> Msg {
    Msg::OpenResult {
        forward_id: None,
        warnings: vec![],
        error: Some(text.into()),
    }
}

impl Inner {
    async fn handle(&self, stream: UnixStream, socket: Socket) -> crate::protocol::Result<()> {
        let (mut conn, _) = Conn::accept(stream).await?;
        let stamp = match socket {
            Socket::Direct | Socket::Control => None,
            Socket::Relay => match conn.recv().await? {
                Some(Msg::HostStamp { alias }) => Some(alias),
                Some(_) => {
                    return conn
                        .send(&open_error("the relay socket needs a host stamp first"))
                        .await
                }
                None => return Ok(()),
            },
        };
        let mut limit = Limit::new(self.config.rate_max, self.config.rate_window);
        while let Some(msg) = conn.recv().await? {
            if msg.is_control() {
                // ADR 0006: a Remote host never reaches the control socket.
                return conn.send(&open_error(CONTROL_ONLY)).await;
            }
            let reply = match msg {
                Msg::Ping => pong(),
                Msg::HostStamp { .. } => {
                    let text = match socket {
                        Socket::Relay => "only one host stamp is allowed per connection",
                        _ => "host stamps are not accepted on the direct socket",
                    };
                    return conn.send(&open_error(text)).await;
                }
                Msg::Open {
                    url,
                    reported_host,
                    callback_ports,
                } => {
                    let host = notice_host(&stamp, reported_host);
                    if self.paused().is_some() {
                        let mut e = new_entry(RequestKind::Open, &host, Outcome::Paused);
                        e.url = Some(url.clone());
                        self.record(e, callback_ports);
                        open_error(format!("paused; open this URL manually: {url}"))
                    } else if limit.allow() {
                        self.open(&url, host, &callback_ports).await
                    } else {
                        let mut e = new_entry(RequestKind::Open, &host, Outcome::RateLimited);
                        e.url = Some(url);
                        self.record(e, callback_ports);
                        self.rate_error()
                    }
                }
                Msg::Forward {
                    reported_host,
                    callback_ports,
                } => {
                    let host = notice_host(&stamp, reported_host);
                    if self.paused().is_some() {
                        let ports = callback_ports
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(", ");
                        let e = new_entry(RequestKind::Forward, &host, Outcome::Paused);
                        self.record(e, callback_ports);
                        open_error(format!("paused; port {ports} was not forwarded"))
                    } else if limit.allow() {
                        self.manual_forward(host, callback_ports).await
                    } else {
                        let e = new_entry(RequestKind::Forward, &host, Outcome::RateLimited);
                        self.record(e, callback_ports);
                        self.rate_error()
                    }
                }
                Msg::AttachForward { forward_id } => {
                    return match self.forwards.attach(&forward_id, conn) {
                        Ok(()) => Ok(()),
                        Err(mut conn) => conn.send(&open_error("unknown forward id")).await,
                    };
                }
                Msg::DataConn {
                    forward_id,
                    conn_id,
                } => {
                    return match self.forwards.take_pending(&forward_id, conn_id) {
                        Some(mut browser) => Ok(conn.splice(&mut browser).await?),
                        None => conn.send(&open_error("unknown connection id")).await,
                    };
                }
                _ => return conn.send(&open_error("unexpected message")).await,
            };
            conn.send(&reply).await?;
        }
        Ok(())
    }

    /// Records a request that started no Callback forward, with the ports
    /// it asked for.
    fn record(&self, mut entry: ActivityEntry, ports: Vec<u16>) {
        entry.ports = ports;
        self.activity.push(entry, None);
    }

    /// Records a request that went on to `Forwards::create`. `entry.ports`
    /// holds the requested ports; the bound ones replace them when any bound.
    /// When none bound, no forward starts, so the entry is closed at once.
    fn record_forward(&self, mut entry: ActivityEntry, created: &forward::Created) {
        entry.warnings.extend(created.warnings.iter().cloned());
        if !created.ports.is_empty() {
            entry.ports = created.ports.clone();
        }
        if created.forward_id.is_none() && !entry.ports.is_empty() {
            entry.open_secs = Some(0);
        }
        self.activity.push(entry, created.forward_id.clone());
    }

    fn rate_error(&self) -> Msg {
        open_error(format!(
            "rate limit: at most {} URLs per {} seconds",
            self.config.rate_max,
            self.config.rate_window.as_secs()
        ))
    }

    fn forward(&self, ports: &[u16], host: &NoticeHost) -> forward::Created {
        self.forwards.create(
            ports,
            host,
            &self.config.loopback,
            self.config.forward_timeout,
            {
                let activity = self.activity.clone();
                move |id: &str, open| activity.forward_closed(id, open)
            },
        )
    }

    /// `rowser forward`: the same `--confirm-hosts` rules as an Open
    /// request. A matching host waits for "Open"; other hosts get a notice.
    async fn manual_forward(&self, host: NoticeHost, ports: Vec<u16>) -> Msg {
        let confirm = self.config.needs_confirm(&host);
        let mut notice = Notice {
            subject: Subject::Forward(ports.clone()),
            target: None,
            host,
            warnings: vec![],
        };
        if confirm {
            if let Err((error, outcome)) = self.confirm(notice.clone()).await {
                let e = new_entry(RequestKind::Forward, &notice.host, outcome);
                self.record(e, ports);
                return open_error(error);
            }
        }
        let created = self.forward(&ports, &notice.host);
        let mut e = new_entry(RequestKind::Forward, &notice.host, Outcome::Opened);
        e.ports = ports.clone();
        self.record_forward(e, &created);
        if !confirm || !created.warnings.is_empty() {
            notice.warnings = created.warnings.clone();
            let notifier = self.notifier.clone();
            tokio::task::spawn_blocking(move || notifier.notify(notice));
        }
        Msg::OpenResult {
            forward_id: created.forward_id,
            warnings: created.warnings,
            error: None,
        }
    }

    async fn open(&self, raw: &str, host: NoticeHost, ports: &[u16]) -> Msg {
        let mut entry = new_entry(RequestKind::Open, &host, Outcome::SchemeRefused);
        entry.url = Some(raw.to_string());
        entry.ports = ports.to_vec();
        let url = match Url::parse(raw) {
            Ok(u) => u,
            Err(e) => {
                // An unparsable URL has no allowed scheme either.
                self.activity.push(entry, None);
                return open_error(format!("invalid URL: {e}"));
            }
        };
        if !matches!(url.scheme(), "http" | "https") {
            self.activity.push(entry, None);
            return open_error(format!(
                "URL scheme `{}` is not allowed; only http and https",
                url.scheme()
            ));
        }
        let request_host = match &host {
            NoticeHost::Alias(a) => RequestHost::Alias(a.clone()),
            NoticeHost::Reported(n) => RequestHost::Unverified(n.clone()),
        };
        let Resolved {
            route,
            target,
            ask,
            mut warnings,
        } = self.current_routes().resolve(&request_host, &url);
        entry.route = Some(route_label(route, &host));
        entry.target = Some(target.to_string());
        entry.warnings = warnings.clone();
        let confirm = ask || self.config.needs_confirm(&host);
        if confirm {
            if let Err((error, outcome)) = self
                .confirm(Notice {
                    subject: Subject::Url(url.clone()),
                    host: host.clone(),
                    target: Some(target.clone()),
                    warnings: warnings.clone(),
                })
                .await
            {
                entry.result = outcome;
                self.activity.push(entry, None);
                return open_error(error);
            }
        }
        // Bind the Callback forward only after approval, so a denied URL
        // never holds a local port.
        let created = self.forward(ports, &host);
        let opener = self.opener.clone();
        let notifier = self.notifier.clone();
        let forward_warned = !created.warnings.is_empty();
        warnings.extend(created.warnings.iter().cloned());
        let result = tokio::task::spawn_blocking(move || {
            opener.open(&url, &target)?;
            // The user saw the confirmation; a second notice adds nothing
            // unless it carries a forward warning.
            if !confirm || forward_warned {
                notifier.notify(Notice {
                    subject: Subject::Url(url),
                    host,
                    target: Some(target),
                    warnings,
                });
            }
            Ok::<_, io::Error>(())
        })
        .await;
        match result.map_err(io::Error::other).and_then(|r| r) {
            Ok(()) => {
                entry.result = Outcome::Opened;
                self.record_forward(entry, &created);
                Msg::OpenResult {
                    forward_id: created.forward_id,
                    warnings: created.warnings,
                    error: None,
                }
            }
            Err(e) => {
                entry.result = Outcome::BrowserMissing;
                entry.warnings.extend(created.warnings.iter().cloned());
                self.record(entry, vec![]);
                // No keeper attaches when the Shim gets an error: free the
                // ports now, not after the forward timeout.
                if let Some(id) = &created.forward_id {
                    self.forwards.close(id);
                }
                open_error(format!("could not open the URL: {e}"))
            }
        }
    }

    /// Waits for the user's answer. A denial, a timeout or a failed
    /// Notifier gives the error text for the Shim.
    async fn confirm(&self, notice: Notice) -> Result<(), (String, Outcome)> {
        let (not_approved, not_done) = match notice.subject {
            Subject::Url(_) => (NOT_APPROVED, "the URL was not opened"),
            Subject::Forward(_) => (FORWARD_NOT_APPROVED, "the port was not forwarded"),
        };
        let notifier = self.notifier.clone();
        let answer = tokio::task::spawn_blocking(move || notifier.confirm(notice));
        let timeout = self.config.confirm_timeout;
        match tokio::time::timeout(timeout, answer).await {
            Ok(Ok(true)) => Ok(()),
            // `confirm` gives only a bool, so a denial and a confirmation
            // that could not be shown look the same here; the Notifier logs
            // the second.
            Ok(Ok(false)) => Err((not_approved.to_string(), Outcome::Denied)),
            Ok(Err(e)) => Err((format!("confirmation failed: {e}"), Outcome::Denied)),
            Err(_) => Err((
                format!(
                    "no confirmation within {} seconds; {not_done}",
                    timeout.as_secs()
                ),
                Outcome::TimedOut,
            )),
        }
    }
}

/// Seconds since the Unix epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A new `rowser activity` entry, stamped with the time now. The ring
/// sets its id.
fn new_entry(kind: RequestKind, host: &NoticeHost, result: Outcome) -> ActivityEntry {
    let (host, unverified) = host.parts();
    ActivityEntry {
        id: 0,
        time: unix_now(),
        kind,
        host: host.to_string(),
        unverified,
        url: None,
        route: None,
        target: None,
        result,
        ports: vec![],
        open_secs: None,
        warnings: vec![],
    }
}

/// The Route that matched, as `rowser activity` shows it.
fn route_label(route: Option<usize>, host: &NoticeHost) -> String {
    match (route, host) {
        (Some(i), _) => format!("route {}", i + 1),
        (None, NoticeHost::Reported(_)) => "[unverified]".into(),
        (None, NoticeHost::Alias(_)) => "[default]".into(),
    }
}

/// The host name of a request: the Host stamp's alias on the relay socket,
/// else the Reported host name.
fn notice_host(stamp: &Option<String>, reported_host: String) -> NoticeHost {
    match stamp {
        Some(alias) => NoticeHost::Alias(alias.clone()),
        None => NoticeHost::Reported(reported_host),
    }
}

/// Binds the direct, relay and control sockets (mode 0600) in the runtime
/// directory `dir`. A missing `dir` is created with mode 0700. An existing `dir` must be
/// owned by the current user and have no group or other permission bits; it is
/// never changed. A Stale socket is replaced; a live one is an error.
pub fn bind_sockets(dir: &Path) -> io::Result<(UnixListener, UnixListener, UnixListener)> {
    prepare_dir(dir)?;
    Ok((
        bind(&dir.join(DIRECT_SOCKET))?,
        bind(&dir.join(RELAY_SOCKET))?,
        bind(&dir.join(CONTROL_SOCKET))?,
    ))
}

fn prepare_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if !dir.exists() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    }
    let meta = std::fs::metadata(dir)?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "runtime directory {} is not owned by the current user",
                dir.display()
            ),
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "runtime directory {} has mode {mode:03o}; it must give no access to group or others (0700)",
                dir.display()
            ),
        ));
    }
    Ok(())
}

fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a daemon already listens on {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// `rowser daemon`: runs the Daemon in the foreground.
pub fn run(args: RunArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rowser daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The `rowser daemon` flags.
#[derive(Debug, Default)]
pub struct RunArgs {
    pub runtime_dir: Option<PathBuf>,
    pub open_command: Option<String>,
    pub confirm_hosts: Vec<String>,
    pub confirm_unverified: bool,
    /// Test hook: a scripted Notifier answer instead of the desktop.
    pub test_confirm: Option<testing::Answer>,
    /// Test hook: confirmation timeout in seconds.
    pub confirm_timeout: Option<u64>,
    /// Test hook: loopback addresses for Callback forwards.
    pub loopback: Vec<IpAddr>,
}

fn run_inner(args: RunArgs) -> Result<(), String> {
    let dir = match args.runtime_dir {
        Some(d) => d,
        None => crate::platform::runtime_dir()?,
    };
    let template = args
        .open_command
        .unwrap_or_else(|| crate::platform::default_open_command().into());
    let opener = CommandOpener::new(&template)?;
    let config_path = crate::config::path()?;
    let file = crate::config::load(&config_path)?;
    let mut config = DaemonConfig {
        routes: Routes::from_config(&file),
        config_file: Some(config_path),
        confirm_hosts: args.confirm_hosts,
        confirm_unverified: args.confirm_unverified,
        ..DaemonConfig::default()
    };
    if let Some(secs) = args.confirm_timeout {
        config.confirm_timeout = Duration::from_secs(secs);
    }
    if !args.loopback.is_empty() {
        config.loopback = args.loopback;
    }
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let (direct, relay, control) = bind_sockets(&dir).map_err(|e| e.to_string())?;
        eprintln!("rowser daemon: listening in {}", dir.display());
        let daemon = match args.test_confirm {
            Some(answer) => Daemon::new(config, opener, testing::ScriptedNotifier::new(answer)),
            None => Daemon::new(config, opener, DesktopNotifier),
        };
        daemon
            .serve(direct, relay, control)
            .await
            .map_err(|e| e.to_string())
    })
}
