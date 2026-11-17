//! Test adapters for the Opener and Notifier ports.

use std::io;
use std::sync::{Arc, Condvar, Mutex};

use url::Url;

use super::{Notice, Notifier, Opener};

/// Records every URL it is asked to open, and never opens a browser.
#[derive(Debug, Clone, Default)]
pub struct RecordingOpener {
    urls: Arc<Mutex<Vec<Url>>>,
}

impl RecordingOpener {
    pub fn urls(&self) -> Vec<Url> {
        self.urls.lock().unwrap().clone()
    }
}

impl Opener for RecordingOpener {
    fn open(&self, url: &Url) -> io::Result<()> {
        self.urls.lock().unwrap().push(url.clone());
        Ok(())
    }
}

/// Records every notice, and never shows a desktop notification.
#[derive(Debug, Clone, Default)]
pub struct RecordingNotifier {
    notices: Arc<Mutex<Vec<Notice>>>,
}

impl RecordingNotifier {
    pub fn notices(&self) -> Vec<Notice> {
        self.notices.lock().unwrap().clone()
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, notice: Notice) {
        self.notices.lock().unwrap().push(notice);
    }

    /// Approves every confirmation (and records it as a notice).
    fn confirm(&self, notice: Notice) -> bool {
        self.notices.lock().unwrap().push(notice);
        true
    }
}

/// The scripted answer of a `ScriptedNotifier` to every confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Approve,
    Deny,
    /// Never answers until `ScriptedNotifier::release`, so the Daemon times out.
    Hang,
}

impl std::str::FromStr for Answer {
    type Err = String;
    fn from_str(s: &str) -> Result<Answer, String> {
        match s {
            "approve" => Ok(Answer::Approve),
            "deny" => Ok(Answer::Deny),
            "hang" => Ok(Answer::Hang),
            other => Err(format!(
                "unknown answer `{other}`; use approve, deny or hang"
            )),
        }
    }
}

#[derive(Debug, Default)]
struct Script {
    notices: Vec<Notice>,
    confirms: Vec<Notice>,
    released: Option<bool>,
}

/// Records notices and answers confirmations from a script. Never shows a
/// desktop notification.
#[derive(Debug, Clone)]
pub struct ScriptedNotifier {
    answer: Answer,
    state: Arc<(Mutex<Script>, Condvar)>,
}

impl ScriptedNotifier {
    pub fn new(answer: Answer) -> ScriptedNotifier {
        ScriptedNotifier {
            answer,
            state: Arc::default(),
        }
    }

    pub fn notices(&self) -> Vec<Notice> {
        self.state.0.lock().unwrap().notices.clone()
    }

    /// Every confirmation the Daemon asked for.
    pub fn confirms(&self) -> Vec<Notice> {
        self.state.0.lock().unwrap().confirms.clone()
    }

    /// Lets every hung confirmation return `answer`.
    pub fn release(&self, answer: bool) {
        self.state.0.lock().unwrap().released = Some(answer);
        self.state.1.notify_all();
    }
}

impl Notifier for ScriptedNotifier {
    fn notify(&self, notice: Notice) {
        self.state.0.lock().unwrap().notices.push(notice);
    }

    fn confirm(&self, notice: Notice) -> bool {
        let (lock, cvar) = &*self.state;
        let mut script = lock.lock().unwrap();
        script.confirms.push(notice);
        match self.answer {
            Answer::Approve => true,
            Answer::Deny => false,
            Answer::Hang => loop {
                if let Some(answer) = script.released {
                    return answer;
                }
                script = cvar.wait(script).unwrap();
            },
        }
    }
}
