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

    /// The notification body. Host names and the URL have control characters
    /// removed and markup escaped, so a forged name cannot move or hide
    /// "(unverified)".
    pub fn body(&self) -> String {
        let host = match &self.host {
            NoticeHost::Alias(alias) => clean(alias),
            NoticeHost::Reported(name) => match clean(name) {
                n if n.trim().is_empty() => "unknown host (unverified)".into(),
                n => format!("{n} (unverified)"),
            },
        };
        format!("From {host}: {}", clean(self.url.as_str()))
    }
}

/// Removes control characters and escapes notification markup.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bidi = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    for c in text.chars().filter(|&c| !c.is_control() && !bidi(c)) {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
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
