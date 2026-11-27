//! Links: one supervised `ssh … __outpost` process per Saved host (ADR 0009).

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::limit::Limit;
use super::{Inner, Notice, NoticeHost, Source, Subject};
use crate::config::SavedHost;
use crate::mux::{self, Side};
use crate::protocol::{Conn, Hello, Msg};

/// The first wait after a drop.
pub(super) const BACKOFF_START: Duration = Duration::from_secs(1);
/// The longest wait between two connection attempts.
pub(super) const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long the Outpost gets to answer the first `Ping`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);

/// The wait before the next connection attempt: 1 s, doubling, at most 60 s.
#[derive(Debug)]
pub(super) struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            next: BACKOFF_START,
        }
    }
}

impl Backoff {
    /// The wait now; the one after is twice as long, up to the maximum.
    pub(super) fn next(&mut self) -> Duration {
        let now = self.next;
        self.next = (self.next * 2).min(BACKOFF_MAX);
        now
    }

    /// After a connection: the next wait is the first one again.
    pub(super) fn reset(&mut self) {
        self.next = BACKOFF_START;
    }
}

/// The state of a Link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkState {
    Connecting,
    Connected,
    /// `BatchMode` could not log in: no more attempts.
    NeedsLogin,
}

impl LinkState {
    /// The state after the `ssh` process ended. `connected`: the Outpost
    /// answered on this attempt. `stderr`: what `ssh` wrote.
    pub(super) fn after_exit(connected: bool, stderr: &str) -> LinkState {
        if !connected && is_login_failure(stderr) {
            LinkState::NeedsLogin
        } else {
            LinkState::Connecting
        }
    }
}

/// True when `ssh` stderr shows that `BatchMode` could not authenticate.
fn is_login_failure(stderr: &str) -> bool {
    stderr.contains("Permission denied")
}

/// The `ssh` arguments of the Link to `host`.
pub(super) fn ssh_args(host: &SavedHost, runtime_dir: &Path) -> Vec<String> {
    let cm = runtime_dir.join("cm-%C");
    vec![
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        format!("ControlPath={}", cm.display()),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        host.name.clone(),
        host.remote_path.clone(),
        "__outpost".into(),
    ]
}

/// Keeps the Link to `host` open until it needs a login. The caller
/// aborts the task to close the Link.
pub(super) async fn supervise(inner: Arc<Inner>, host: SavedHost) {
    // One limit for all streams of this Saved host, across reconnections.
    let limit = Arc::new(Mutex::new(Limit::new(
        inner.config.rate_max,
        inner.config.rate_window,
    )));
    let mut backoff = Backoff::default();
    loop {
        tracing::debug!("link {}: {:?}", host.name, LinkState::Connecting);
        let (connected, stderr) = connect_once(&inner, &host, &limit, &mut backoff).await;
        match LinkState::after_exit(connected, &stderr) {
            LinkState::NeedsLogin => {
                tracing::warn!("link {}: needs login: {}", host.name, stderr.trim());
                let notifier = inner.notifier.clone();
                let notice = Notice {
                    subject: Subject::NeedsLogin,
                    host: NoticeHost::Alias(host.name.clone()),
                    target: None,
                    warnings: vec![],
                };
                tokio::task::spawn_blocking(move || notifier.notify(notice));
                return;
            }
            _ => {
                if !stderr.trim().is_empty() {
                    tracing::warn!("link {}: {}", host.name, stderr.trim());
                }
                let wait = backoff.next();
                tracing::debug!("link {}: dropped; next attempt in {wait:?}", host.name);
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Runs one `ssh` process until it ends. Returns whether the Outpost
/// answered, and the stderr of `ssh`.
async fn connect_once(
    inner: &Arc<Inner>,
    host: &SavedHost,
    limit: &Arc<Mutex<Limit>>,
    backoff: &mut Backoff,
) -> (bool, String) {
    let mut child = match Command::new(&inner.config.ssh)
        .args(ssh_args(host, &inner.config.link_dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                format!("cannot run {}: {e}", inner.config.ssh.display()),
            )
        }
    };
    let (Some(stdin), Some(stdout), Some(mut err)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return (false, "ssh has no stdio".into());
    };
    let stderr = tokio::spawn(async move {
        let mut text = String::new();
        let _ = err.read_to_string(&mut text).await;
        text
    });
    let (opener, mut inbound, driver) = mux::start(tokio::io::join(stdout, stdin), Side::Daemon);
    let mut connected = false;
    let hello = async {
        let stream = opener.open().await.ok()?;
        let mut conn = Conn::connect(stream, Hello::current()).await.ok()?;
        conn.send(&Msg::Ping).await.ok()?;
        match conn.recv().await.ok()? {
            Some(Msg::Pong { version }) => Some((conn, version)),
            _ => None,
        }
    };
    let serve = async {
        while let Some(stream) = inbound.recv().await {
            let inner = inner.clone();
            let source = Source::Link {
                alias: host.name.clone(),
                limit: limit.clone(),
            };
            tokio::spawn(async move {
                if let Err(e) = inner.handle(stream, source).await {
                    tracing::debug!("link stream ended: {e}");
                }
            });
        }
    };
    tokio::pin!(serve);
    tokio::select! {
        r = tokio::time::timeout(HELLO_TIMEOUT, hello) => {
            if let Ok(Some((_control, version))) = r {
                connected = true;
                backoff.reset();
                tracing::info!("link {}: {:?} (outpost {version})", host.name, LinkState::Connected);
                (&mut serve).await;
            }
        }
        _ = &mut serve => {}
    }
    driver.abort();
    let _ = child.kill().await;
    let text = stderr.await.unwrap_or_default();
    (connected, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_starts_at_one_second_and_doubles_to_sixty() {
        let mut b = Backoff::default();
        let waits: Vec<u64> = (0..9).map(|_| b.next().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        b.reset();
        assert_eq!(b.next(), Duration::from_secs(1));
    }

    #[test]
    fn a_login_failure_before_the_outpost_answers_needs_login() {
        let err = "g14: Permission denied (publickey).\n";
        assert_eq!(LinkState::after_exit(false, err), LinkState::NeedsLogin);
    }

    #[test]
    fn a_drop_after_a_connection_retries() {
        let err = "Connection to g14 closed by remote host.\n";
        assert_eq!(LinkState::after_exit(true, err), LinkState::Connecting);
        assert_eq!(LinkState::after_exit(false, err), LinkState::Connecting);
    }

    #[test]
    fn the_ssh_command_matches_the_spec() {
        let host = SavedHost {
            name: "g14".into(),
            remote_path: "~/.local/bin/rowser".into(),
            auto_update: true,
            default_browser: true,
        };
        assert_eq!(
            ssh_args(&host, Path::new("/run/u/rowser")).join(" "),
            "-T -o BatchMode=yes -o ControlMaster=no -o ControlPath=/run/u/rowser/cm-%C \
             -o ServerAliveInterval=15 -o ServerAliveCountMax=3 g14 ~/.local/bin/rowser __outpost"
        );
    }
}
