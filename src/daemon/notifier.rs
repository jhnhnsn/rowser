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
        self.body_with(true)
    }

    /// The body without markup escapes, for a dialog that shows plain text.
    /// Control characters are still removed.
    pub fn plain_body(&self) -> String {
        self.body_with(false)
    }

    fn body_with(&self, escape: bool) -> String {
        let host = match &self.host {
            NoticeHost::Alias(alias) => clean(alias, escape),
            NoticeHost::Reported(name) => match clean(name, escape) {
                n if n.trim().is_empty() => "unknown host (unverified)".into(),
                n => format!("{n} (unverified)"),
            },
        };
        format!("From {host}: {}", clean(self.url.as_str(), escape))
    }
}

/// Removes control characters and, when `escape` is set, escapes
/// notification markup.
fn clean(text: &str, escape: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let bidi = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    for c in text.chars().filter(|&c| !c.is_control() && !bidi(c)) {
        match c {
            '&' if escape => out.push_str("&amp;"),
            '<' if escape => out.push_str("&lt;"),
            '>' if escape => out.push_str("&gt;"),
            '"' if escape => out.push_str("&quot;"),
            '\'' if escape => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Shows notices to the user.
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, notice: Notice);

    /// Asks the user to approve opening `notice.url`. Blocks until the user
    /// answers; `true` means "Open". The Daemon applies its own timeout and
    /// ignores a late answer.
    fn confirm(&self, notice: Notice) -> bool;
}

/// Seconds the confirmation notification or dialog stays up.
const CONFIRM_SECS: u32 = 60;

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

    fn confirm(&self, notice: Notice) -> bool {
        match confirm_desktop(&notice) {
            Ok(answer) => answer,
            Err(e) => {
                tracing::warn!("confirmation failed, URL not opened: {e}");
                false
            }
        }
    }
}

/// Linux: a notification with an "Open" action.
#[cfg(not(target_os = "macos"))]
fn confirm_desktop(notice: &Notice) -> Result<bool, String> {
    let handle = notify_rust::Notification::new()
        .summary("rowser: open this URL?")
        .body(&notice.body())
        .action("open", "Open")
        .action("default", "Open")
        .action("deny", "Deny")
        .timeout(notify_rust::Timeout::Milliseconds(CONFIRM_SECS * 1000))
        .show()
        .map_err(|e| e.to_string())?;
    let mut approved = false;
    handle.wait_for_action(|action| approved = matches!(action, "open" | "default"));
    Ok(approved)
}

/// macOS: notify-rust has no notification actions here, so ask with a dialog.
/// The text goes in as an argument, never into the script source.
#[cfg(target_os = "macos")]
fn confirm_desktop(notice: &Notice) -> Result<bool, String> {
    let script = format!(
        "on run argv\n\
         set r to display dialog (item 1 of argv) with title \"rowser\" \
         buttons {{\"Deny\", \"Open\"}} default button \"Deny\" giving up after {CONFIRM_SECS}\n\
         if gave up of r then return \"deny\"\n\
         return button returned of r\n\
         end run"
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg(format!("Open this URL?\n\n{}", notice.plain_body()))
        .output()
        .map_err(|e| e.to_string())?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "Open")
}
