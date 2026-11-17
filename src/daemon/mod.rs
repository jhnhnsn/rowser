//! The Daemon: serves Open requests on the direct and relay sockets.

mod limit;
mod notifier;
mod opener;
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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            rate_max: 5,
            rate_window: Duration::from_secs(10),
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

fn error(text: impl Into<String>) -> Msg {
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
                        .send(&error("the relay socket needs a host stamp first"))
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
                    return conn.send(&error(text)).await;
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
                        error(format!(
                            "rate limit: at most {} URLs per {} seconds",
                            self.config.rate_max,
                            self.config.rate_window.as_secs()
                        ))
                    }
                }
                _ => return conn.send(&error("unexpected message")).await,
            };
            conn.send(&reply).await?;
        }
        Ok(())
    }

    async fn open(&self, url: &str, host: NoticeHost) -> Msg {
        let url = match Url::parse(url) {
            Ok(u) => u,
            Err(e) => return error(format!("invalid URL: {e}")),
        };
        if !matches!(url.scheme(), "http" | "https") {
            return error(format!(
                "URL scheme `{}` is not allowed; only http and https",
                url.scheme()
            ));
        }
        let opener = self.opener.clone();
        let notifier = self.notifier.clone();
        let result = tokio::task::spawn_blocking(move || {
            opener.open(&url)?;
            notifier.notify(Notice { url, host });
            Ok::<_, io::Error>(())
        })
        .await;
        match result {
            Ok(Ok(())) => Msg::OpenResult {
                forward_id: None,
                warnings: vec![],
                error: None,
            },
            Ok(Err(e)) => error(format!("could not open the URL: {e}")),
            Err(e) => error(format!("could not open the URL: {e}")),
        }
    }
}

/// Creates `dir` (mode 0700) and binds the direct and relay sockets in it
/// (mode 0600). A Stale socket file is replaced; a live one is an error.
pub fn bind_sockets(dir: &Path) -> io::Result<(UnixListener, UnixListener)> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok((
        bind(&dir.join(DIRECT_SOCKET))?,
        bind(&dir.join(RELAY_SOCKET))?,
    ))
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
pub fn run(runtime_dir: Option<PathBuf>, open_command: Option<String>) -> ExitCode {
    match run_inner(runtime_dir, open_command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rowser daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(runtime_dir: Option<PathBuf>, open_command: Option<String>) -> Result<(), String> {
    let dir = match runtime_dir {
        Some(d) => d,
        None => crate::platform::runtime_dir()?,
    };
    let template = open_command.unwrap_or_else(|| crate::platform::default_open_command().into());
    let opener = CommandOpener::new(&template)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let (direct, relay) = bind_sockets(&dir).map_err(|e| e.to_string())?;
        eprintln!("rowser daemon: listening in {}", dir.display());
        let daemon = Daemon::new(DaemonConfig::default(), opener, DesktopNotifier);
        daemon.serve(direct, relay).await.map_err(|e| e.to_string())
    })
}
