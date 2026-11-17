//! Notifier port and the `DesktopNotifier` adapter.

use url::Url;

/// Where the host name in a notice comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeHost {
    /// A Host alias from a Host stamp on the relay socket: trusted.
    Alias(String),
    /// A Reported host name from the Shim: never trusted.
    Reported(String),
}

/// What the Daemon tells the user about an Open request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub url: Url,
    pub host: NoticeHost,
}

impl Notice {
    pub fn summary(&self) -> String {
        "rowser: opened a URL".into()
    }

    pub fn body(&self) -> String {
        let host = match &self.host {
            NoticeHost::Alias(alias) => alias.clone(),
            NoticeHost::Reported(name) if name.is_empty() => "unknown host (unverified)".into(),
            NoticeHost::Reported(name) => format!("{name} (unverified)"),
        };
        format!("From {host}: {}", self.url)
    }
}

/// Shows notices to the user.
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, notice: Notice);
}

/// Desktop notifications through notify-rust.
#[derive(Debug, Clone, Default)]
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify(&self, notice: Notice) {
        let result = notify_rust::Notification::new()
            .summary(&notice.summary())
            .body(&notice.body())
            .show();
        if let Err(e) = result {
            tracing::warn!("desktop notification failed: {e}");
        }
    }
}
