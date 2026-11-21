//! The control socket handler: `rowser status` and later control commands.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::sync::broadcast::error::RecvError;

use crate::protocol::{Conn, Hello, Msg, SessionInfo};
use crate::routes::Routes;

use super::{open_error, pong, unix_now, Inner, Loaded, Pause};

impl Inner {
    pub(super) async fn handle_control(&self, stream: UnixStream) -> crate::protocol::Result<()> {
        let (mut conn, _) = Conn::accept(stream).await?;
        while let Some(msg) = conn.recv().await? {
            let reply = match msg {
                Msg::Ping => pong(),
                Msg::Status => self.status(),
                Msg::Reload => self.reload(),
                Msg::Pause { for_secs } => self.pause(for_secs),
                Msg::Resume => self.resume(),
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

    /// `rowser pause [--for]`: replaces any pause in effect.
    fn pause(&self, for_secs: Option<u64>) -> Msg {
        let end = for_secs.map(|s| {
            let at = tokio::time::Instant::now() + Duration::from_secs(s);
            (at, unix_now() + s)
        });
        *self.pause.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pause { end });
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
        let was_paused = self.paused().is_some();
        *self.pause.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if was_paused {
            tracing::info!("resumed");
        }
        Msg::Resumed { was_paused }
    }

    /// `Some` while a pause is in effect, with when it ends (Unix seconds)
    /// for `rowser pause --for`. A pause whose time passed ends here.
    pub(super) fn paused(&self) -> Option<Option<u64>> {
        let mut pause = self.pause.lock().unwrap_or_else(|e| e.into_inner());
        match &*pause {
            Some(Pause { end: Some((at, _)) }) if tokio::time::Instant::now() >= *at => {
                *pause = None;
                None
            }
            p => p.as_ref().map(|p| p.end.map(|(_, u)| u)),
        }
    }

    fn status(&self) -> Msg {
        let paused = self.paused();
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
            paused: paused.is_some(),
            paused_until: paused.flatten(),
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
