//! The Daemon: serves Open requests on the Link to each Saved host, and
//! control commands on the control socket.

mod activity;
mod confirm;
mod control;
mod forward;
mod limit;
mod link;
mod notifier;
mod opener;
mod pending;
#[doc(hidden)]
pub mod testing;

use std::io;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::net::UnixListener;
use url::Url;

use crate::browsers::{Browsers, Roots};
use crate::protocol::{ActivityEntry, AnsweredBy, Conn, Hello, Msg, Outcome, RequestKind};
use crate::routes::{Resolved, Routes};
use limit::Limit;
pub use notifier::{DesktopNotifier, Dismiss, LinkEvent, Notice, NoticeHost, Notifier, Subject};
pub use opener::{CommandOpener, Opener};

pub use crate::protocol::CONTROL_SOCKET;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Most Open requests one Saved host (all streams of its Link) may make
    /// in `rate_window`.
    pub rate_max: usize,
    pub rate_window: Duration,
    /// `--confirm-hosts` glob patterns (`*`, `?`; ASCII case is ignored).
    /// Open requests whose Host alias matches wait for `Notifier::confirm`.
    pub confirm_hosts: Vec<String>,
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
    /// Where browser discovery looks, for browser Targets. `None`: no
    /// browsers, so every browser Target falls back to `[default]`.
    pub browser_roots: Option<Roots>,
    /// The Saved hosts: the Daemon keeps one Link to each.
    pub hosts: Vec<crate::config::SavedHost>,
    /// The `ssh` program a Link runs.
    pub ssh: PathBuf,
    /// Where a Link's control path (`cm-%C`) goes: the runtime directory.
    pub link_dir: PathBuf,
    /// The automatic update copies this binary in place of the release
    /// download (the hidden `ROWSER_TEST_LOCAL_BUILD` hook, and tests).
    pub update_local_build: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            routes: Routes::default(),
            config_file: None,
            rate_max: 5,
            rate_window: Duration::from_secs(10),
            confirm_hosts: vec![],
            confirm_timeout: confirm::CONFIRM_TIMEOUT,
            forward_timeout: crate::callback::FORWARD_TIMEOUT,
            loopback: crate::callback::LOOPBACK.to_vec(),
            browser_roots: None,
            hosts: vec![],
            ssh: PathBuf::from("ssh"),
            // Empty: no shared folder such as /tmp. The default has no
            // Saved hosts, so no Link uses it; a caller with hosts sets it.
            link_dir: PathBuf::new(),
            update_local_build: None,
        }
    }
}

/// Where a request connection came from: a stream on the Link to the Saved
/// host `alias`. All its streams share `limit`. `link` opens new streams on
/// that Link, for Callback forwards; `None` only for a test Link
/// (`Daemon::serve_with_test_links`), which then forwards nothing.
pub(crate) struct Source {
    pub alias: String,
    pub limit: Arc<Mutex<Limit>>,
    pub link: Option<crate::mux::Opener>,
}

/// Any byte stream a request can arrive on: a Unix socket or a Link stream.
pub(crate) trait AsyncStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncStream for T {}
pub(crate) type BoxStream = Box<dyn AsyncStream>;

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
    /// Recent requests, for `rowser activity`.
    activity: Arc<activity::Activity>,
    /// The Routes in use and the file state: set at start, replaced by a
    /// valid `rowser reload`.
    loaded: RwLock<Loaded>,
    /// `rowser pause`: in memory only, so a Daemon restart ends it.
    pause: Mutex<Option<Pause>>,
    /// Requests that wait for an answer, for `rowser pending`.
    pending: Arc<pending::Pending>,
    /// The running Links, by Host alias.
    links: Mutex<std::collections::HashMap<String, RunningLink>>,
    /// This value, so a reload can start Link tasks that hold it.
    me: std::sync::Weak<Inner>,
}

/// One Link task and what it shares with `rowser status`.
struct RunningLink {
    host: crate::config::SavedHost,
    task: tokio::task::AbortHandle,
    status: link::LinkStatus,
}

pub use link::control_path_option;

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
            inner: Arc::new_cyclic(|me| Inner {
                me: me.clone(),
                links: Mutex::default(),
                loaded: RwLock::new(Loaded {
                    routes: Arc::new(std::mem::take(&mut config.routes)),
                    config_file_exists,
                }),
                config,
                opener: Arc::new(opener),
                notifier: Arc::new(notifier),
                forwards: Arc::default(),
                activity: Arc::default(),
                pause: Mutex::default(),
                pending: Arc::default(),
            }),
        }
    }

    /// Opens a Link to each Saved host, and accepts connections on the
    /// control socket until an accept fails.
    pub async fn serve(self, control: UnixListener) -> io::Result<()> {
        self.serve_with_test_links(control, vec![]).await
    }

    /// `serve`, and also takes each connection on a test listener as a
    /// stream of a Link to its Host alias, with no Link under it: a
    /// Callback forward gives a warning and forwards nothing. A test hook:
    /// the binary never binds one.
    #[doc(hidden)]
    pub async fn serve_with_test_links(
        self,
        control: UnixListener,
        test_links: Vec<(String, UnixListener)>,
    ) -> io::Result<()> {
        self.inner.sync_links(self.inner.config.hosts.clone());
        for (alias, listener) in test_links {
            let inner = self.inner.clone();
            let limit = inner.new_limit();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let source = Source {
                        alias: alias.clone(),
                        limit: limit.clone(),
                        link: None,
                    };
                    let inner = inner.clone();
                    tokio::spawn(async move {
                        if let Err(e) = inner.handle(stream, source).await {
                            tracing::debug!("connection ended: {e}");
                        }
                    });
                }
            });
        }
        loop {
            let (stream, _) = control.accept().await?;
            let inner = self.inner.clone();
            tokio::spawn(async move {
                if let Err(e) = inner.handle_control(stream).await {
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

/// The error for a control message on a Link.
pub const CONTROL_ONLY: &str = "control messages are accepted only on the control socket";

/// The answer to `Ping` on every socket.
fn pong() -> Msg {
    Msg::Pong {
        version: Hello::current().version(),
        rowser_version: crate::protocol::rowser_version(),
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
    /// Starts a Link for each Saved host in `hosts` that has none (or whose
    /// entry changed), and closes the Links of hosts no longer listed.
    fn sync_links(&self, hosts: Vec<crate::config::SavedHost>) {
        let Some(me) = self.me.upgrade() else { return };
        let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        links.retain(|name, l| {
            let keep = hosts.iter().any(|h| *h == l.host && h.name == *name);
            if !keep {
                tracing::info!("link {name}: closed");
                l.task.abort();
            }
            keep
        });
        for host in hosts {
            if links.contains_key(&host.name) {
                continue;
            }
            let status = link::new_status(&host.name);
            let task = tokio::spawn(link::supervise(me.clone(), host.clone(), status.clone()));
            links.insert(
                host.name.clone(),
                RunningLink {
                    host,
                    task: task.abort_handle(),
                    status,
                },
            );
        }
    }

    /// `rowser host connect`: closes the Link to `name` and starts it again
    /// at once, with a fresh backoff. False when `name` has no Link.
    fn reconnect_link(&self, name: &str) -> bool {
        let Some(me) = self.me.upgrade() else {
            return false;
        };
        let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        let Some(l) = links.get_mut(name) else {
            return false;
        };
        l.task.abort();
        tracing::info!("link {name}: connecting again");
        let task = tokio::spawn(link::supervise(me, l.host.clone(), l.status.clone()));
        l.task = task.abort_handle();
        true
    }

    /// Each Link's status, in Host alias order.
    fn link_infos(&self) -> Vec<crate::protocol::LinkInfo> {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        let mut infos: Vec<_> = links
            .values()
            .map(|l| l.status.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .collect();
        infos.sort_by(|a, b| a.host.cmp(&b.host));
        infos
    }

    async fn handle(
        &self,
        stream: impl AsyncStream + 'static,
        source: Source,
    ) -> crate::protocol::Result<()> {
        let (mut conn, _) = Conn::accept(Box::new(stream) as BoxStream).await?;
        let Source { alias, limit, link } = source;
        let link = link.as_ref();
        let allow = || limit.lock().unwrap_or_else(|e| e.into_inner()).allow();
        while let Some(msg) = conn.recv().await? {
            if msg.is_control() {
                // ADR 0006: a Remote host never reaches the control socket.
                return conn.send(&open_error(CONTROL_ONLY)).await;
            }
            let host = NoticeHost::Alias(alias.clone());
            let reply = match msg {
                Msg::Ping => pong(),
                Msg::Open {
                    url,
                    callback_ports,
                } => {
                    if self.pause_state().is_paused() {
                        let mut e = new_entry(RequestKind::Open, &host, Outcome::Paused);
                        e.url = Some(url.clone());
                        self.record(e, callback_ports);
                        open_error(format!("paused; open this URL manually: {url}"))
                    } else if allow() {
                        self.open(&url, host, &callback_ports, link).await
                    } else {
                        let mut e = new_entry(RequestKind::Open, &host, Outcome::RateLimited);
                        e.url = Some(url);
                        self.record(e, callback_ports);
                        self.rate_error()
                    }
                }
                Msg::Forward { callback_ports } => {
                    if self.pause_state().is_paused() {
                        let text = paused_text(&Subject::Forward(callback_ports.clone()));
                        let e = new_entry(RequestKind::Forward, &host, Outcome::Paused);
                        self.record(e, callback_ports);
                        open_error(text)
                    } else if allow() {
                        self.manual_forward(host, callback_ports, link).await
                    } else {
                        let e = new_entry(RequestKind::Forward, &host, Outcome::RateLimited);
                        self.record(e, callback_ports);
                        self.rate_error()
                    }
                }
                _ => return conn.send(&open_error("unexpected message")).await,
            };
            conn.send(&reply).await?;
        }
        Ok(())
    }

    /// A fresh rate limit, for all the streams of one Link.
    pub(crate) fn new_limit(&self) -> Arc<Mutex<Limit>> {
        Arc::new(Mutex::new(Limit::new(
            self.config.rate_max,
            self.config.rate_window,
        )))
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

    async fn forward(
        &self,
        ports: &[u16],
        host: &NoticeHost,
        link: Option<&crate::mux::Opener>,
    ) -> forward::Created {
        self.forwards
            .create(
                ports,
                host,
                link,
                &self.config.loopback,
                self.config.forward_timeout,
                {
                    let activity = self.activity.clone();
                    move |id: &str, open| activity.forward_closed(id, open)
                },
            )
            .await
    }

    /// `rowser forward`: the same `--confirm-hosts` rules as an Open
    /// request. A matching host waits for "Open"; other hosts get a notice.
    async fn manual_forward(
        &self,
        host: NoticeHost,
        ports: Vec<u16>,
        link: Option<&crate::mux::Opener>,
    ) -> Msg {
        let confirm = self.config.needs_confirm(&host);
        let mut notice = Notice {
            subject: Subject::Forward(ports.clone()),
            target: None,
            host,
            warnings: vec![],
        };
        let mut answered_by = None;
        if confirm {
            match self.confirm(notice.clone()).await {
                Ok(c) => answered_by = Some(c.by),
                Err(refused) => {
                    let mut e = new_entry(RequestKind::Forward, &notice.host, refused.outcome);
                    e.answered_by = refused.by;
                    self.record(e, ports);
                    return open_error(refused.error);
                }
            }
        }
        let created = self.forward(&ports, &notice.host, link).await;
        let mut e = new_entry(RequestKind::Forward, &notice.host, Outcome::Opened);
        e.answered_by = answered_by;
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

    async fn open(
        &self,
        raw: &str,
        host: NoticeHost,
        ports: &[u16],
        link: Option<&crate::mux::Opener>,
    ) -> Msg {
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
        let Resolved {
            route,
            mut target,
            ask,
            mut warnings,
        } = self
            .current_routes()
            .resolve(host.alias(), &url, &self.browsers());
        entry.route = Some(route_label(route));
        entry.target = Some(target.to_string());
        entry.warnings = warnings.clone();
        let confirm = ask || self.config.needs_confirm(&host);
        if confirm {
            match self
                .confirm(Notice {
                    subject: Subject::Url(url.clone()),
                    host: host.clone(),
                    target: Some(target.clone()),
                    warnings: warnings.clone(),
                })
                .await
            {
                Ok(c) => {
                    entry.answered_by = Some(c.by);
                    if let Some(t) = c.target {
                        entry.target = Some(t.to_string());
                        target = t;
                    }
                }
                Err(refused) => {
                    entry.result = refused.outcome;
                    entry.answered_by = refused.by;
                    self.activity.push(entry, None);
                    return open_error(refused.error);
                }
            }
        }
        // Bind the Callback forward only after approval, so a denied URL
        // never holds a local port.
        let created = self.forward(ports, &host, link).await;
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
                // The URL did not open, so no callback comes: free the
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
    ///
    /// The request is listed in `rowser pending` while it waits. The first
    /// answer wins: the Notifier's, or `rowser approve` / `rowser deny`,
    /// which also closes the notification or dialog.
    async fn confirm(&self, notice: Notice) -> Result<Confirmed, Refused> {
        let (not_approved, not_done) = match notice.subject {
            Subject::Url(_) => (NOT_APPROVED, "the URL was not opened"),
            Subject::Forward(_) => (FORWARD_NOT_APPROVED, "the port was not forwarded"),
            Subject::Link(_) => {
                unreachable!("a Link notice is sent with Notifier::notify, never confirmed")
            }
        };
        let paused = paused_text(&notice.subject);
        let dismiss = Dismiss::default();
        let (guard, mut terminal) = self.pending.add(notice.clone(), dismiss.clone());
        let notifier = self.notifier.clone();
        let mut desktop = tokio::task::spawn_blocking(move || notifier.confirm(notice, &dismiss));
        let timeout = self.config.confirm_timeout;
        let waited = tokio::time::timeout(timeout, async {
            tokio::select! {
                answer = &mut terminal => answer.ok().map(FirstAnswer::Terminal),
                answer = &mut desktop => {
                    // The desktop answered; the terminal may have taken the
                    // request just before, and then its answer wins.
                    match self.pending.take(guard.id) {
                        Some(_) => Some(match answer {
                            Ok(a) => FirstAnswer::Desktop(a),
                            Err(e) => FirstAnswer::Failed(e.to_string()),
                        }),
                        None => terminal.await.ok().map(FirstAnswer::Terminal),
                    }
                }
            }
        })
        .await;
        drop(guard);
        let refused = |error: String, outcome, by| Refused { error, outcome, by };
        let answer = match waited {
            Err(_) => {
                return Err(refused(
                    format!(
                        "no confirmation within {} seconds; {not_done}",
                        timeout.as_secs()
                    ),
                    Outcome::TimedOut,
                    None,
                ))
            }
            Ok(a) => a,
        };
        let (approved, target, by) = match answer {
            Some(FirstAnswer::Terminal(pending::TerminalAnswer::Approve(t))) => {
                (true, t, AnsweredBy::Terminal)
            }
            // `None`: the terminal took the request but dropped its sender
            // without an answer. Not reached today (`Taken::answer` sends
            // before it dismisses); deny is the safe result if it ever is.
            Some(FirstAnswer::Terminal(pending::TerminalAnswer::Deny)) | None => {
                (false, None, AnsweredBy::Terminal)
            }
            Some(FirstAnswer::Desktop(a)) => (a, None, AnsweredBy::Desktop),
            Some(FirstAnswer::Failed(e)) => {
                return Err(refused(
                    format!("confirmation failed: {e}"),
                    Outcome::Denied,
                    None,
                ))
            }
        };
        if !approved {
            // `confirm` gives only a bool, so a denial and a confirmation
            // that could not be shown look the same here; the Notifier logs
            // the second.
            return Err(refused(not_approved.to_string(), Outcome::Denied, Some(by)));
        }
        // A pause that started while the user looked still wins.
        if self.pause_state().is_paused() {
            return Err(refused(paused, Outcome::Paused, Some(by)));
        }
        Ok(Confirmed { target, by })
    }

    /// The browsers the Daemon can open in now.
    fn browsers(&self) -> Browsers {
        self.config
            .browser_roots
            .as_ref()
            .map(Browsers::scan)
            .unwrap_or_default()
    }
}

/// The first answer to a confirmation.
enum FirstAnswer {
    Terminal(pending::TerminalAnswer),
    Desktop(bool),
    /// The Notifier task failed.
    Failed(String),
}

/// An approved confirmation.
struct Confirmed {
    /// The `rowser approve --open` Target, which replaces the request's.
    target: Option<crate::config::Target>,
    by: AnsweredBy,
}

/// A refused confirmation: the error for the Shim, and what activity shows.
struct Refused {
    error: String,
    outcome: Outcome,
    by: Option<AnsweredBy>,
}

/// The error for a request that a pause refused.
fn paused_text(subject: &Subject) -> String {
    match subject {
        Subject::Url(url) => format!("paused; open this URL manually: {url}"),
        Subject::Forward(ports) => {
            let ports = ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("paused; port {ports} was not forwarded")
        }
        Subject::Link(_) => {
            unreachable!("a Link notice is never a request, so no pause refuses it")
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
    ActivityEntry {
        id: 0,
        time: unix_now(),
        kind,
        host: host.alias().to_string(),
        url: None,
        route: None,
        target: None,
        result,
        ports: vec![],
        open_secs: None,
        warnings: vec![],
        answered_by: None,
    }
}

/// The Route that matched, as `rowser activity` shows it.
fn route_label(route: Option<usize>) -> String {
    match route {
        Some(i) => format!("route {}", i + 1),
        None => "[default]".into(),
    }
}

/// Binds the control socket (mode 0600) in the runtime directory `dir`. A
/// missing `dir` is created with mode 0700. An existing `dir` must be owned
/// by the current user and have no group or other permission bits; it is
/// never changed. A socket nothing listens on is replaced; a live one is an
/// error.
pub fn bind_sockets(dir: &Path) -> io::Result<UnixListener> {
    prepare_dir(dir)?;
    bind(&dir.join(CONTROL_SOCKET))
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
    /// Test hook: a scripted Notifier answer instead of the desktop.
    pub test_confirm: Option<testing::Answer>,
    /// Test hook: confirmation timeout in seconds.
    pub confirm_timeout: Option<u64>,
    /// Test hook: loopback addresses for Callback forwards.
    pub loopback: Vec<IpAddr>,
    /// Test hook: Callback forward timeout in seconds.
    pub forward_timeout: Option<u64>,
}

fn run_inner(args: RunArgs) -> Result<(), String> {
    let dir = match args.runtime_dir {
        Some(d) => d,
        None => crate::platform::runtime_dir()?,
    };
    let template = args
        .open_command
        .unwrap_or_else(|| crate::platform::default_open_command().into());
    let roots = Roots::from_env();
    let opener = CommandOpener::new(&template)?.with_browsers(roots.clone());
    let config_path = crate::config::path()?;
    let file = crate::config::load(&config_path)?;
    let mut config = DaemonConfig {
        browser_roots: Some(roots),
        hosts: file.hosts.clone(),
        link_dir: dir.clone(),
        update_local_build: crate::remote_install::test_local_build(),
        routes: Routes::from_config(&file),
        config_file: Some(config_path),
        confirm_hosts: args.confirm_hosts,
        ..DaemonConfig::default()
    };
    let unverified = file.has_unverified;
    if let Some(secs) = args.confirm_timeout {
        config.confirm_timeout = Duration::from_secs(secs);
    }
    if let Some(secs) = args.forward_timeout {
        config.forward_timeout = Duration::from_secs(secs);
    }
    if !args.loopback.is_empty() {
        config.loopback = args.loopback;
    }
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let control = bind_sockets(&dir).map_err(|e| e.to_string())?;
        eprintln!("rowser daemon: listening in {}", dir.display());
        if unverified {
            tracing::warn!("{}", crate::config::UNVERIFIED_IGNORED);
        }
        let daemon = match args.test_confirm {
            Some(answer) => Daemon::new(config, opener, testing::ScriptedNotifier::new(answer)),
            None => Daemon::new(config, opener, DesktopNotifier),
        };
        daemon.serve(control).await.map_err(|e| e.to_string())
    })
}
