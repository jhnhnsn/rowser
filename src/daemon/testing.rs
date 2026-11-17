//! Test adapters for the Opener and Notifier ports.

use std::io;
use std::sync::{Arc, Mutex};

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
}
