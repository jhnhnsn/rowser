//! The control socket handler: `rowser status` and later control commands.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::sync::broadcast::error::RecvError;

use crate::protocol::{Conn, Hello, Msg, SessionInfo};
use crate::routes::Routes;

use super::pending::TerminalAnswer;
use super::{open_error, pong, unix_now, Inner, Loaded, Pause, Subject};
use crate::config::Target;

/// Whether `rowser pause` is in effect now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PauseState {
    NotPaused,
    /// `until`: when `rowser pause --for` ends it, in Unix seconds.
    Paused {
        until: Option<u64>,
    },
}

impl PauseState {
    pub(super) fn is_paused(self) -> bool {
        matches!(self, PauseState::Paused { .. })
    }

    pub(super) fn until(self) -> Option<u64> {
        match self {
            PauseState::Paused { until } => until,
            PauseState::NotPaused => None,
        }
    }
}

impl Inner {
    pub(super) async fn handle_control(&self, stream: UnixStream) -> crate::protocol::Result<()> {
        let (mut conn, _) = Conn::accept(stream).await?;
        while let Some(msg) = conn.recv().await? {
            let reply = match msg {
                Msg::Ping => pong(),
                Msg::Status => self.status(),
                Msg::Reload => self.reload(),
                Msg::Pause { for_secs } => self.start_pause(for_secs),
                Msg::Resume => self.resume(),
                Msg::Pending => Msg::PendingReply {
                    requests: self.pending.list(),
                },
                Msg::AnswerPending { id, approve, open } => self.answer_pending(id, approve, open),
                Msg::Activity { follow } => {
                    let (entries, mut rx) = self.activity.watch();
                    conn.send(&Msg::ActivityReply { entries }).await?;
                    if !follow {
                        continue;
                    }
                    // Streams until the client closes the connection.
                    loop {
                        tokio::select! {
                            r = rx.recv() => match r {
                                Ok(entry) => {
                                    let entry = Box::new(entry);
                                    conn.send(&Msg::ActivityUpdate { entry }).await?;
                                }
                                Err(RecvError::Lagged(_)) => continue,
                                Err(RecvError::Closed) => return Ok(()),
                            },
                            r = conn.recv() => match r {
                                Ok(Some(_)) => continue,
                                _ => return Ok(()),
                            },
                        }
                    }
                }
                Msg::Session { host } => {
                    let _session = self.sessions.register(&host);
                    conn.send(&Msg::OpenResult {
                        forward_id: None,
                        warnings: vec![],
                        error: None,
                    })
                    .await?;
                    // The Session lasts until the Wrapper closes the
                    // connection (or the kernel does, for a killed
                    // Wrapper); the guard drops it then.
                    while let Ok(Some(_)) = conn.recv().await {}
                    return Ok(());
                }
                _ => {
                    return conn
                        .send(&open_error(
                            "the control socket accepts only control messages",
                        ))
                        .await
                }
            };
            conn.send(&reply).await?;
        }
        Ok(())
    }

    /// The Routes in use now.
    pub(super) fn current_routes(&self) -> Arc<Routes> {
        self.loaded
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .clone()
    }

    /// `rowser reload`: reads the config file again. An invalid file is
    /// refused with the file, line and reason, and the old Routes stay.
    fn reload(&self) -> Msg {
        let Some(path) = &self.config.config_file else {
            return open_error("the daemon has no config file path");
        };
        match crate::config::load(path) {
            Ok(file) => {
                let count = file.routes.len();
                // A valid load means a file is there now.
                *self.loaded.write().unwrap_or_else(|e| e.into_inner()) = Loaded {
                    routes: Arc::new(Routes::from_config(&file)),
                    config_file_exists: true,
                };
                tracing::info!("config reloaded from {} ({count} routes)", path.display());
                Msg::Reloaded { routes: count }
            }
            Err(e) => open_error(e),
        }
    }

    /// `rowser pause [--for]`: replaces any pause in effect. A duration
    /// past what the clocks can hold is refused.
    fn start_pause(&self, for_secs: Option<u64>) -> Msg {
        let end = match for_secs {
            None => None,
            Some(s) => {
                let at = tokio::time::Instant::now().checked_add(Duration::from_secs(s));
                match at.zip(unix_now().checked_add(s)) {
                    Some(end) => Some(end),
                    None => return open_error("duration too long"),
                }
            }
        };
        *self.lock_pause() = Some(Pause { end });
        tracing::info!(
            "paused{}",
            for_secs.map_or(String::new(), |s| format!(" for {s} seconds"))
        );
        Msg::Paused {
            until: end.map(|(_, u)| u),
        }
    }

    /// `rowser resume`.
    fn resume(&self) -> Msg {
        let was_paused = self.pause_state().is_paused();
        *self.lock_pause() = None;
        if was_paused {
            tracing::info!("resumed");
        }
        Msg::Resumed { was_paused }
    }

    /// `rowser approve <id> [--open <target>]` and `rowser deny <id>`. The
    /// first answer wins; an unknown or answered id is an error. An `open`
    /// Target that cannot be used is refused, and the request still waits.
    fn answer_pending(&self, id: u64, approve: bool, open: Option<String>) -> Msg {
        let no_request = || open_error(format!("no pending request {id}"));
        let Some(subject) = self.pending.subject(id) else {
            return no_request();
        };
        let answer = match (approve, open) {
            (false, _) => TerminalAnswer::Deny,
            (true, None) => TerminalAnswer::Approve(None),
            (true, Some(text)) => {
                if matches!(subject, Subject::Forward(_)) {
                    return open_error("--open applies only to a URL, not to rowser forward");
                }
                let target: Target = match text.parse() {
                    Ok(t) => t,
                    Err(e) => return open_error(e),
                };
                let target = match target {
                    Target::Ask => return open_error("--open needs system or a browser, not ask"),
                    Target::System => Target::System,
                    t => match self.browsers().normalize(&t) {
                        Ok(found) => found,
                        Err(why) => return open_error(why),
                    },
                };
                TerminalAnswer::Approve(Some(target))
            }
        };
        match self.pending.take(id) {
            Some(taken) => {
                taken.answer(answer);
                Msg::Answered { id }
            }
            None => no_request(),
        }
    }

    /// The pause field; a poisoned lock still holds a usable value.
    fn lock_pause(&self) -> std::sync::MutexGuard<'_, Option<Pause>> {
        self.pause.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The pause in effect now. A pause whose time passed ends here.
    pub(super) fn pause_state(&self) -> PauseState {
        let mut pause = self.lock_pause();
        match &*pause {
            None => PauseState::NotPaused,
            Some(Pause { end: Some((at, _)) }) if tokio::time::Instant::now() >= *at => {
                *pause = None;
                PauseState::NotPaused
            }
            Some(p) => PauseState::Paused {
                until: p.end.map(|(_, u)| u),
            },
        }
    }

    fn status(&self) -> Msg {
        let paused = self.pause_state();
        Msg::StatusReply {
            daemon_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: Hello::current().version(),
            config_file: self
                .config
                .config_file
                .as_ref()
                .map(|p| p.display().to_string()),
            config_file_exists: self
                .loaded
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .config_file_exists,
            paused: paused.is_paused(),
            paused_until: paused.until(),
            forwards: self.forwards.list(),
            sessions: self
                .sessions
                .list()
                .into_iter()
                .map(|(host, start)| SessionInfo {
                    target: self.current_routes().session_target(&host).to_string(),
                    age_secs: start.elapsed().as_secs(),
                    host,
                })
                .collect(),
        }
    }
}
