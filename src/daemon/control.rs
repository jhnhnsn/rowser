//! The control socket handler: `rowser status` and later control commands.

use std::sync::Arc;

use tokio::net::UnixStream;
use tokio::sync::broadcast::error::RecvError;

use crate::routes::Routes;

use super::{open_error, pong, Inner};
use crate::protocol::{Conn, Hello, Msg, SessionInfo};

impl Inner {
    pub(super) async fn handle_control(&self, stream: UnixStream) -> crate::protocol::Result<()> {
        let (mut conn, _) = Conn::accept(stream).await?;
        while let Some(msg) = conn.recv().await? {
            let reply = match msg {
                Msg::Ping => pong(),
                Msg::Status => self.status(),
                Msg::Reload => self.reload(),
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
                                Ok(entry) => conn.send(&Msg::ActivityUpdate { entry: Box::new(entry) }).await?,
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
    pub(super) fn routes(&self) -> Arc<Routes> {
        self.routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
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
                *self.routes.write().unwrap_or_else(|e| e.into_inner()) =
                    Arc::new(Routes::from_config(&file));
                tracing::info!("config reloaded from {} ({count} routes)", path.display());
                Msg::Reloaded { routes: count }
            }
            Err(e) => open_error(e),
        }
    }

    fn status(&self) -> Msg {
        Msg::StatusReply {
            daemon_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: Hello::current().version(),
            config_file: self
                .config
                .config_file
                .as_ref()
                .map(|p| p.display().to_string()),
            config_file_exists: self.config_file_exists,
            // `rowser pause` (#49) sets this; until then the Daemon never pauses.
            paused: false,
            forwards: self.forwards.list(),
            sessions: self
                .sessions
                .list()
                .into_iter()
                .map(|(host, start)| SessionInfo {
                    target: self.routes().session_target(&host).to_string(),
                    age_secs: start.elapsed().as_secs(),
                    host,
                })
                .collect(),
        }
    }
}
