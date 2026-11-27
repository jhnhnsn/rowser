//! Links: one supervised `ssh … __outpost` process per Saved host (ADR 0009).

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::limit::Limit;
use super::{Inner, LinkEvent, Notice, NoticeHost, Source, Subject};
use crate::config::SavedHost;
use crate::mux::{self, Side};
use crate::protocol::{Conn, Error, Hello, LinkInfo, LinkState, Msg};

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

/// What the Daemon knows of one Link, shared by its task and `rowser status`.
pub(super) type LinkStatus = Arc<Mutex<LinkInfo>>;

/// A new status for the Link to `host`: connecting, never connected.
pub(super) fn new_status(host: &str) -> LinkStatus {
    Arc::new(Mutex::new(LinkInfo {
        host: host.into(),
        state: LinkState::Connecting,
        since: super::unix_now(),
        last_connected: None,
        outpost_version: None,
        outpost_rowser_version: None,
    }))
}

fn set_state(status: &LinkStatus, state: LinkState) {
    let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
    if s.state != state {
        s.state = state;
        s.since = super::unix_now();
    }
}

/// The state after the `ssh` process ended. `connected`: the Outpost
/// answered on this attempt. `status`: the exit status of `ssh`, if it
/// exited. `stderr`: what `ssh` wrote.
pub(super) fn after_exit(connected: bool, status: Option<i32>, stderr: &str) -> LinkState {
    if !connected && is_login_failure(status, stderr) {
        LinkState::NeedsLogin
    } else {
        LinkState::Connecting
    }
}

/// The exit status `ssh` gives for its own errors, such as a failed login.
const SSH_ERROR_STATUS: i32 = 255;

/// True when `ssh` itself could not authenticate under `BatchMode`: it exits
/// 255 and writes `Permission denied (<methods>)`. A "Permission denied"
/// from the Outpost (`rowser __outpost: …`) or from the remote shell is not
/// a login failure.
fn is_login_failure(status: Option<i32>, stderr: &str) -> bool {
    status == Some(SSH_ERROR_STATUS)
        && stderr
            .lines()
            .filter(|l| !l.starts_with("rowser __outpost:"))
            .any(|l| l.contains("Permission denied ("))
}

/// `path` as one `ssh -o` value: in double quotes, so a space (as in the
/// macOS runtime directory) does not split it.
fn ssh_quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

/// The `ControlPath=…` option value of rowser's own control path in
/// `runtime_dir`. `rowser host connect` starts its master there.
pub fn control_path_option(runtime_dir: &Path) -> String {
    format!("ControlPath={}", ssh_quoted(&runtime_dir.join("cm-%C")))
}

/// The `ssh` arguments of the Link to `host`.
pub(super) fn ssh_args(host: &SavedHost, runtime_dir: &Path) -> Vec<String> {
    vec![
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        control_path_option(runtime_dir),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        host.name.clone(),
        host.remote_path.clone(),
        "__outpost".into(),
    ]
}

/// Keeps the Link to `host` open until it needs a login or waits in
/// version mismatch. The caller aborts the task to close the Link, and
/// starts a new task to end a wait (`Reconnect`, or a changed entry).
pub(super) async fn supervise(inner: Arc<Inner>, host: SavedHost, link: LinkStatus) {
    // One limit for all streams of this Saved host, across reconnections.
    let limit = Arc::new(Mutex::new(Limit::new(
        inner.config.rate_max,
        inner.config.rate_window,
    )));
    let mut backoff = Backoff::default();
    // One update try per task: a task ends in version mismatch, and only a
    // new task (one entry into the state) tries again.
    let mut update_tried = false;
    // Set after an update: notify "updated" when the Link connects to an
    // Outpost of this major version.
    let mut updated: Option<String> = None;
    loop {
        tracing::debug!("link {}: {:?}", host.name, LinkState::Connecting);
        set_state(&link, LinkState::Connecting);
        let (connected, status, stderr) =
            connect_once(&inner, &host, &limit, &mut backoff, &link, &mut updated).await;
        let state = after_exit(connected, status, &stderr);
        if state == LinkState::NeedsLogin {
            set_state(&link, state);
            tracing::warn!("link {}: needs login: {}", host.name, stderr.trim());
            notify(&inner, &host, LinkEvent::NeedsLogin);
            return;
        }
        if !stderr.trim().is_empty() {
            tracing::warn!("link {}: {}", host.name, stderr.trim());
        }
        let (state, outpost) = {
            let s = link.lock().unwrap_or_else(|e| e.into_inner());
            (s.state, s.outpost_rowser_version.clone())
        };
        if state != LinkState::VersionMismatch {
            set_state(&link, LinkState::Connecting);
            let wait = backoff.next();
            tracing::debug!("link {}: dropped; next attempt in {wait:?}", host.name);
            tokio::time::sleep(wait).await;
            continue;
        }
        if host.auto_update && !update_tried {
            update_tried = true;
            match update(&inner, &host).await {
                Ok(()) => {
                    let version = env!("CARGO_PKG_VERSION").to_string();
                    tracing::info!("link {}: installed rowser {version}", host.name);
                    updated = Some(version);
                    backoff.reset();
                    continue;
                }
                Err(e) => tracing::warn!("link {}: update failed: {e}", host.name),
            }
        }
        // Wait: no more attempts, as each one starts an Outpost on the host.
        let after_update = updated.take().is_some();
        notify(
            &inner,
            &host,
            LinkEvent::VersionMismatch {
                outpost,
                after_update,
            },
        );
        return;
    }
}

/// Shows one notification about the Link to `host`.
fn notify(inner: &Arc<Inner>, host: &SavedHost, event: LinkEvent) {
    let notifier = inner.notifier.clone();
    let notice = Notice {
        subject: Subject::Link(event),
        host: NoticeHost::Alias(host.name.clone()),
        target: None,
        warnings: vec![],
    };
    tokio::task::spawn_blocking(move || notifier.notify(notice));
}

/// The major part of a rowser version (`1` of `1.0.0-rc.2`).
fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// True when the Outpost's rowser version has another major version than
/// this build. An Outpost that gives no version is not a mismatch.
pub(super) fn is_version_mismatch(outpost: Option<&str>) -> bool {
    outpost.is_some_and(|v| major(v.trim()) != major(env!("CARGO_PKG_VERSION")))
}

/// The automatic update: the remote install of this version on `host`,
/// with or without the default-browser part as the host was added. It runs
/// the Link's `ssh` with `BatchMode` on rowser's own control path.
async fn update(inner: &Arc<Inner>, host: &SavedHost) -> Result<(), String> {
    let ssh = crate::remote_install::Ssh {
        program: inner.config.ssh.clone(),
        options: vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            control_path_option(&inner.config.link_dir),
        ],
    };
    let source = crate::remote_install::source_for(inner.config.update_local_build.clone(), None);
    let name = host.name.clone();
    let default_browser = host.default_browser;
    tokio::task::spawn_blocking(move || {
        crate::remote_install::install_with(&ssh, &name, source.as_ref(), default_browser)
            .map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Runs one `ssh` process until it ends. Returns whether the Outpost
/// answered, the exit status of `ssh` (`None` when it was killed), and its
/// stderr. When the Link connects and `updated` holds a version, it shows
/// the "updated" notification and clears `updated`.
async fn connect_once(
    inner: &Arc<Inner>,
    host: &SavedHost,
    limit: &Arc<Mutex<Limit>>,
    backoff: &mut Backoff,
    link: &LinkStatus,
    updated: &mut Option<String>,
) -> (bool, Option<i32>, String) {
    if inner.config.link_dir.as_os_str().is_empty() {
        return (false, None, "no runtime directory for the Link".into());
    }
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
                None,
                format!("cannot run {}: {e}", inner.config.ssh.display()),
            )
        }
    };
    let (Some(stdin), Some(stdout), Some(mut err)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return (false, None, "ssh has no stdio".into());
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
        let mut conn = match Conn::connect(stream, Hello::current()).await {
            Ok(c) => c,
            Err(e @ Error::VersionMismatch { .. }) => {
                tracing::warn!("link {}: {e}", host.name);
                set_state(link, LinkState::VersionMismatch);
                return None;
            }
            Err(_) => return None,
        };
        conn.send(&Msg::Ping).await.ok()?;
        match conn.recv().await.ok()? {
            Some(Msg::Pong {
                version,
                rowser_version,
            }) => Some((conn, version, rowser_version)),
            _ => None,
        }
    };
    let serve = async {
        while let Some(stream) = inbound.recv().await {
            let inner = inner.clone();
            let source = Source::Link {
                alias: host.name.clone(),
                limit: limit.clone(),
                link: opener.clone(),
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
            match r {
            Ok(Some((_, _, rowser_version))) if is_version_mismatch(rowser_version.as_deref()) => {
                tracing::warn!(
                    "link {}: version mismatch: outpost {}, local {}",
                    host.name,
                    rowser_version.as_deref().unwrap_or("-"),
                    env!("CARGO_PKG_VERSION")
                );
                link.lock().unwrap_or_else(|e| e.into_inner()).outpost_rowser_version =
                    rowser_version;
                set_state(link, LinkState::VersionMismatch);
            }
            Ok(Some((_control, version, rowser_version))) => {
                connected = true;
                backoff.reset();
                {
                    let mut s = link.lock().unwrap_or_else(|e| e.into_inner());
                    s.last_connected = Some(super::unix_now());
                    s.outpost_version = Some(version);
                    s.outpost_rowser_version = rowser_version;
                }
                set_state(link, LinkState::Connected);
                tracing::info!("link {}: {:?} (outpost {version})", host.name, LinkState::Connected);
                if let Some(version) = updated.take() {
                    tracing::info!("link {}: updated rowser to {version}", host.name);
                    notify(inner, host, LinkEvent::Updated { version });
                }
                (&mut serve).await;
            }
            _ => {}
            }
        }
        _ = &mut serve => {}
    }
    driver.abort();
    // When `ssh` ended by itself, its stdout closed first: give it a moment
    // to exit, so its status shows a login failure.
    let status = match tokio::time::timeout(EXIT_WAIT, child.wait()).await {
        Ok(Ok(s)) => s.code(),
        _ => {
            let _ = child.kill().await;
            None
        }
    };
    let text = stderr.await.unwrap_or_default();
    (connected, status, text)
}

/// How long `connect_once` waits for `ssh` to exit before it kills it.
const EXIT_WAIT: Duration = Duration::from_secs(1);

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
    fn only_another_major_version_is_a_mismatch() {
        // This build is 1.x.
        assert!(is_version_mismatch(Some("0.9.0")));
        assert!(is_version_mismatch(Some("2.0.0")));
        assert!(!is_version_mismatch(Some("1.0.0")));
        assert!(!is_version_mismatch(Some("1.4.2")));
        assert!(!is_version_mismatch(None));
    }

    #[test]
    fn a_login_failure_before_the_outpost_answers_needs_login() {
        let err = "g14: Permission denied (publickey).\n";
        assert_eq!(after_exit(false, Some(255), err), LinkState::NeedsLogin);
        let err = "user@g14: Permission denied (publickey,password).\n";
        assert_eq!(after_exit(false, Some(255), err), LinkState::NeedsLogin);
    }

    #[test]
    fn a_drop_after_a_connection_retries() {
        let err = "Connection to g14 closed by remote host.\n";
        assert_eq!(after_exit(true, Some(255), err), LinkState::Connecting);
        assert_eq!(after_exit(false, Some(255), err), LinkState::Connecting);
    }

    #[test]
    fn a_permission_error_from_the_outpost_retries() {
        let err =
            "rowser __outpost: /tmp/rowser-1000/outpost.sock: Permission denied (os error 13)\n";
        assert_eq!(after_exit(false, Some(1), err), LinkState::Connecting);
        assert_eq!(after_exit(false, Some(255), err), LinkState::Connecting);
    }

    #[test]
    fn a_remote_shell_permission_error_retries() {
        let err = "bash: line 1: /home/u/.local/bin/rowser: Permission denied\n";
        assert_eq!(after_exit(false, Some(126), err), LinkState::Connecting);
        assert_eq!(after_exit(false, Some(255), err), LinkState::Connecting);
    }

    #[test]
    fn a_login_text_without_the_ssh_error_status_retries() {
        let err = "g14: Permission denied (publickey).\n";
        assert_eq!(after_exit(false, Some(1), err), LinkState::Connecting);
        assert_eq!(after_exit(false, None, err), LinkState::Connecting);
    }

    #[test]
    fn a_runtime_dir_with_a_space_is_quoted_for_ssh() {
        let host = SavedHost {
            name: "g14".into(),
            remote_path: "~/.local/bin/rowser".into(),
            auto_update: true,
            default_browser: true,
        };
        let dir = Path::new("/Users/u/Library/Application Support/rowser");
        let args = ssh_args(&host, dir);
        assert!(
            args.contains(
                &"ControlPath=\"/Users/u/Library/Application Support/rowser/cm-%C\"".to_string()
            ),
            "{args:?}"
        );
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
            "-T -o BatchMode=yes -o ControlMaster=no -o ControlPath=\"/run/u/rowser/cm-%C\" \
             -o ServerAliveInterval=15 -o ServerAliveCountMax=3 g14 ~/.local/bin/rowser __outpost"
        );
    }
}
