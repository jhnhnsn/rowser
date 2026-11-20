//! The control socket handler: `rowser status` and later control commands.

use tokio::net::UnixStream;

use super::{open_error, pong, Inner};
use crate::protocol::{Conn, Hello, Msg};

impl Inner {
    pub(super) async fn handle_control(&self, stream: UnixStream) -> crate::protocol::Result<()> {
        let (mut conn, _) = Conn::accept(stream).await?;
        while let Some(msg) = conn.recv().await? {
            let reply = match msg {
                Msg::Ping => pong(),
                Msg::Status => self.status(),
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
        }
    }
}
