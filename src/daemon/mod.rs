//! The Daemon: serves Open requests on the direct and relay sockets.

mod confirm;
mod limit;
mod notifier;
mod opener;
#[doc(hidden)]
pub mod testing;

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use url::Url;

use crate::protocol::{Conn, Hello, Msg};
use limit::Limit;
pub use notifier::{DesktopNotifier, Notice, NoticeHost, Notifier};
pub use opener::{CommandOpener, Opener};

/// File name of the direct socket in the runtime directory.
pub const DIRECT_SOCKET: &str = "rowserd.sock";
/// File name of the relay socket in the runtime directory.
pub const RELAY_SOCKET: &str = "rowserd-relay.sock";

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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            rate_max: 5,
            rate_window: Duration::from_secs(10),
            confirm_hosts: vec![],
            confirm_unverified: false,
            confirm_timeout: confirm::CONFIRM_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Socket {
    Direct,
    Relay,
}

pub struct Daemon {
    inner: Arc<Inner>,
}

struct Inner {
    config: DaemonConfig,
    opener: Arc<dyn Opener>,
    notifier: Arc<dyn Notifier>,
}

impl Daemon {
    pub fn new(config: DaemonConfig, opener: impl Opener, notifier: impl Notifier) -> Daemon {
        Daemon {
            inner: Arc::new(Inner {
                config,
                opener: Arc::new(opener),
                notifier: Arc::new(notifier),
            }),
        }
    }

    /// Accepts connections on both sockets until an accept fails.
    pub async fn serve(self, direct: UnixListener, relay: UnixListener) -> io::Result<()> {
        loop {
            let (stream, socket) = tokio::select! {
                r = direct.accept() => (r?.0, Socket::Direct),
                r = relay.accept() => (r?.0, Socket::Relay),
            };
            let inner = self.inner.clone();
            tokio::spawn(async move {
                if let Err(e) = inner.handle(stream, socket).await {
                    tracing::debug!("connection ended: {e}");
                }
            });
        }
    }
}

/// A successful `OpenResult`.
fn open_ok() -> Msg {
    Msg::OpenResult {
        forward_id: None,
        warnings: vec![],
        error: None,
    }
}

/// The error when `Notifier::confirm` gives `false`.
pub const NOT_APPROVED: &str =
    "the URL was not approved: the user denied it, or the confirmation could not be shown (see the daemon log)";

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
            Socket::Direct => None,
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
            let reply = match msg {
                Msg::Ping => Msg::Pong {
                    version: Hello::current().version(),
                },
                Msg::HostStamp { .. } => {
                    let text = match socket {
                        Socket::Direct => "host stamps are not accepted on the direct socket",
                        Socket::Relay => "only one host stamp is allowed per connection",
                    };
                    return conn.send(&open_error(text)).await;
                }
                Msg::Open {
                    url, reported_host, ..
                } => {
                    if limit.allow() {
                        let host = match &stamp {
                            Some(alias) => NoticeHost::Alias(alias.clone()),
                            None => NoticeHost::Reported(reported_host),
                        };
                        self.open(&url, host).await
                    } else {
                        open_error(format!(
                            "rate limit: at most {} URLs per {} seconds",
                            self.config.rate_max,
                            self.config.rate_window.as_secs()
                        ))
                    }
                }
                _ => return conn.send(&open_error("unexpected message")).await,
            };
            conn.send(&reply).await?;
        }
        Ok(())
    }

    async fn open(&self, url: &str, host: NoticeHost) -> Msg {
        let url = match Url::parse(url) {
            Ok(u) => u,
            Err(e) => return open_error(format!("invalid URL: {e}")),
        };
        if !matches!(url.scheme(), "http" | "https") {
            return open_error(format!(
                "URL scheme `{}` is not allowed; only http and https",
                url.scheme()
            ));
        }
        let confirm = self.config.needs_confirm(&host);
        if confirm {
            if let Err(error) = self
                .confirm(Notice {
                    url: url.clone(),
                    host: host.clone(),
                })
                .await
            {
                return error;
            }
        }
        let opener = self.opener.clone();
        let notifier = self.notifier.clone();
        let result = tokio::task::spawn_blocking(move || {
            opener.open(&url)?;
            // The user saw the confirmation; a second notice adds nothing.
            if !confirm {
                notifier.notify(Notice { url, host });
            }
            Ok::<_, io::Error>(())
        })
        .await;
        match result.map_err(io::Error::other).and_then(|r| r) {
            Ok(()) => open_ok(),
            Err(e) => open_error(format!("could not open the URL: {e}")),
        }
    }

    /// Waits for the user's answer. A denial, a timeout or a failed
    /// Notifier gives the error reply for the Shim.
    async fn confirm(&self, notice: Notice) -> Result<(), Msg> {
        let notifier = self.notifier.clone();
        let answer = tokio::task::spawn_blocking(move || notifier.confirm(notice));
        let timeout = self.config.confirm_timeout;
        match tokio::time::timeout(timeout, answer).await {
            Ok(Ok(true)) => Ok(()),
            // `confirm` gives only a bool, so a denial and a confirmation
            // that could not be shown look the same here; the Notifier logs
            // the second.
            Ok(Ok(false)) => Err(open_error(NOT_APPROVED)),
            Ok(Err(e)) => Err(open_error(format!("confirmation failed: {e}"))),
            Err(_) => Err(open_error(format!(
                "no confirmation within {} seconds; the URL was not opened",
                timeout.as_secs()
            ))),
        }
    }
}

/// Binds the direct and relay sockets (mode 0600) in the runtime directory
/// `dir`. A missing `dir` is created with mode 0700. An existing `dir` must be
/// owned by the current user and have no group or other permission bits; it is
/// never changed. A Stale socket is replaced; a live one is an error.
pub fn bind_sockets(dir: &Path) -> io::Result<(UnixListener, UnixListener)> {
    prepare_dir(dir)?;
    Ok((
        bind(&dir.join(DIRECT_SOCKET))?,
        bind(&dir.join(RELAY_SOCKET))?,
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
    let mut config = DaemonConfig {
        confirm_hosts: args.confirm_hosts,
        confirm_unverified: args.confirm_unverified,
        ..DaemonConfig::default()
    };
    if let Some(secs) = args.confirm_timeout {
        config.confirm_timeout = Duration::from_secs(secs);
    }
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let (direct, relay) = bind_sockets(&dir).map_err(|e| e.to_string())?;
        eprintln!("rowser daemon: listening in {}", dir.display());
        let daemon = match args.test_confirm {
            Some(answer) => Daemon::new(config, opener, testing::ScriptedNotifier::new(answer)),
            None => Daemon::new(config, opener, DesktopNotifier),
        };
        daemon.serve(direct, relay).await.map_err(|e| e.to_string())
    })
}
