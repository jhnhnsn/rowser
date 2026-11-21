//! Notifier port and the `DesktopNotifier` adapter.

use std::sync::{Arc, Mutex};

use url::Url;

use crate::config::Target;

/// Where the host name in a notice comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeHost {
    /// A Host alias from a Host stamp on the relay socket: trusted.
    Alias(String),
    /// A Reported host name from the Shim: never trusted.
    Reported(String),
}

impl NoticeHost {
    /// The host name, and `true` when it is an unverified Reported host name.
    pub fn parts(&self) -> (&str, bool) {
        match self {
            NoticeHost::Alias(a) => (a, false),
            NoticeHost::Reported(r) => (r, true),
        }
    }
}

/// What a notice is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// An Open request for this URL.
    Url(Url),
    /// A `rowser forward` request for these Callback ports.
    Forward(Vec<u16>),
}

/// What the Daemon tells the user about an Open request or a `rowser
/// forward` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub subject: Subject,
    pub host: NoticeHost,
    /// The Target an Open request opens in; `None` for a `rowser forward`
    /// request. The body names it when it is not `system`.
    pub target: Option<Target>,
    /// Problems with the request that did not stop it, such as a busy
    /// Callback port.
    pub warnings: Vec<String>,
}

impl Notice {
    pub fn summary(&self) -> String {
        match self.subject {
            Subject::Url(_) => "rowser: opened a URL".into(),
            Subject::Forward(_) => "rowser: forwarded a port".into(),
        }
    }

    /// The question of a confirmation, without the "rowser: " prefix.
    pub fn question(&self) -> &'static str {
        match self.subject {
            Subject::Url(_) => "Open this URL?",
            Subject::Forward(_) => "Forward this port?",
        }
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
        let what = match &self.subject {
            Subject::Url(url) => clean(url.as_str(), escape),
            Subject::Forward(ports) => {
                let ports: Vec<String> = ports.iter().map(u16::to_string).collect();
                format!("forward local port {}", ports.join(", "))
            }
        };
        let mut body = format!("From {host}: {what}");
        if let Some(target) = self.target.as_ref().filter(|t| **t != Target::System) {
            body.push_str("\nOpens in: ");
            body.push_str(&clean(&target.to_string(), escape));
        }
        for w in &self.warnings {
            body.push_str("\nWarning: ");
            body.push_str(&clean(w, escape));
        }
        body
    }
}

/// Removes control characters and, when `escape` is set, escapes
/// notification markup.
fn clean(text: &str, escape: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in crate::protocol::printable(text).chars() {
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

/// Lets the Daemon close a confirmation that the terminal answered first
/// (`rowser approve` or `rowser deny`). The Notifier registers how to close
/// its notification or dialog with `on_dismiss`.
#[derive(Clone, Default)]
pub struct Dismiss(Arc<Mutex<DismissState>>);

#[derive(Default)]
struct DismissState {
    done: bool,
    close: Option<Box<dyn FnOnce() + Send>>,
}

impl Dismiss {
    /// Runs `close` when the Daemon dismisses the confirmation, or now when
    /// it did already.
    pub fn on_dismiss(&self, close: impl FnOnce() + Send + 'static) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if s.done {
            drop(s);
            close();
        } else {
            s.close = Some(Box::new(close));
        }
    }

    /// True after the Daemon dismissed the confirmation.
    pub fn is_dismissed(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).done
    }

    /// Closes the confirmation; its later answer is ignored.
    pub(crate) fn dismiss(&self) {
        let close = {
            let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
            s.done = true;
            s.close.take()
        };
        if let Some(close) = close {
            close();
        }
    }
}

/// Shows notices to the user.
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, notice: Notice);

    /// Asks the user to approve `notice.subject`. Blocks until the user
    /// answers; `true` means "Open". The Daemon applies its own timeout and
    /// ignores a late answer. When the terminal answers first, the Daemon
    /// calls `dismiss`, and the Notifier closes its notification or dialog
    /// where the platform allows.
    fn confirm(&self, notice: Notice, dismiss: &Dismiss) -> bool;
}

/// Seconds the confirmation notification or dialog stays up.
const CONFIRM_SECS: u64 = super::confirm::CONFIRM_TIMEOUT.as_secs();

/// Desktop notifications through notify-rust.
#[derive(Debug, Clone, Default)]
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify(&self, notice: Notice) {
        if let Err(e) = notify_desktop(&notice) {
            tracing::warn!("desktop notification failed: {e}");
        }
    }

    fn confirm(&self, notice: Notice, dismiss: &Dismiss) -> bool {
        match confirm_desktop(&notice, dismiss) {
            Ok(answer) => answer,
            Err(e) => {
                tracing::warn!("confirmation failed, request refused: {e}");
                false
            }
        }
    }
}

/// Linux: a notification through notify-rust. Notification servers render
/// body markup, so the body is escaped.
#[cfg(not(target_os = "macos"))]
fn notify_desktop(notice: &Notice) -> Result<(), String> {
    notify_rust::Notification::new()
        .summary(&notice.summary())
        .body(&notice.body())
        .show()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The AppleScript for a macOS notification. The title and body go in as
/// arguments (`item 1` and `item 2 of argv`), never into the script source.
const MACOS_NOTIFY_SCRIPT: &str = "on run argv\n\
     display notification (item 2 of argv) with title (item 1 of argv)\n\
     end run";

/// The `osascript` arguments for a macOS notification: plain text, since
/// macOS shows notification text as it is (no markup).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_notify_args(notice: &Notice) -> Vec<String> {
    vec![
        "-e".into(),
        MACOS_NOTIFY_SCRIPT.into(),
        notice.summary(),
        notice.plain_body(),
    ]
}

/// macOS: notify-rust's notifications do not show on current macOS (seen on
/// macOS 26 from both a LaunchAgent and a terminal), while
/// `display notification` does, so use `osascript` as the dialog does.
#[cfg(target_os = "macos")]
fn notify_desktop(notice: &Notice) -> Result<(), String> {
    let out = std::process::Command::new("osascript")
        .args(macos_notify_args(notice))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Linux: a notification with an "Open" action.
#[cfg(not(target_os = "macos"))]
fn confirm_desktop(notice: &Notice, dismiss: &Dismiss) -> Result<bool, String> {
    let handle = notify_rust::Notification::new()
        .summary(&format!("rowser: {}", notice.question().to_lowercase()))
        .body(&notice.body())
        .action("open", "Open")
        .action("deny", "Deny")
        .timeout(notify_rust::Timeout::Milliseconds(
            (CONFIRM_SECS * 1000) as u32,
        ))
        .show()
        .map_err(|e| e.to_string())?;
    // The handle waits below, so close the notification by its id: a
    // replacement with the same id, closed at once. The close also ends the
    // wait.
    let id = handle.id();
    dismiss.on_dismiss(move || {
        match notify_rust::Notification::new()
            .id(id)
            .summary("rowser")
            .show()
        {
            Ok(h) => h.close(),
            Err(e) => tracing::warn!("could not close the confirmation: {e}"),
        }
    });
    // Only the "Open" button approves. A click on the notification body (the
    // "default" action on many servers), a close or "Deny" all deny.
    let mut approved = false;
    handle.wait_for_action(|action| approved = action == "open");
    Ok(approved && !dismiss.is_dismissed())
}

/// macOS: notify-rust has no notification actions here, so ask with a dialog.
/// The text goes in as an argument, never into the script source.
#[cfg(target_os = "macos")]
fn confirm_desktop(notice: &Notice, dismiss: &Dismiss) -> Result<bool, String> {
    let script = format!(
        "on run argv\n\
         set r to display dialog (item 1 of argv) with title \"rowser\" \
         buttons {{\"Deny\", \"Open\"}} default button \"Deny\" giving up after {CONFIRM_SECS}\n\
         if gave up of r then return \"deny\"\n\
         return button returned of r\n\
         end run"
    );
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    let mut child = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg(format!("{}\n\n{}", notice.question(), notice.plain_body()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().ok_or("no osascript stdout")?;
    // The child is reaped only under this lock, so the dismiss below never
    // signals a pid that the OS already gave to another process.
    let child = Arc::new(Mutex::new(child));
    // A terminal answer ends osascript, which closes its dialog.
    dismiss.on_dismiss({
        let child = child.clone();
        move || {
            let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(None) = c.try_wait() {
                // SAFETY: kill has no memory preconditions; the pid is our
                // child, not reaped while we hold the lock.
                unsafe { libc::kill(c.id() as libc::pid_t, libc::SIGTERM) };
            }
        }
    });
    let mut text = String::new();
    let _ = stdout.read_to_string(&mut text);
    let status = child
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .wait()
        .map_err(|e| e.to_string())?;
    Ok(!dismiss.is_dismissed() && status.success() && text.trim() == "Open")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_notification_text_is_plain_and_passed_as_arguments() {
        let notice = Notice {
            subject: Subject::Url(Url::parse("https://example.com/?a=1&b=2").unwrap()),
            host: NoticeHost::Alias("g14".into()),
            target: None,
            warnings: vec![],
        };
        let args = macos_notify_args(&notice);
        assert_eq!(args[0], "-e");
        assert!(
            !args[1].contains("example.com"),
            "text must not be in the script"
        );
        assert_eq!(args[2], "rowser: opened a URL");
        assert_eq!(args[3], "From g14: https://example.com/?a=1&b=2");
    }
}
