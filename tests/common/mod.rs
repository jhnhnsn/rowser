//! Helpers shared by the integration tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The file name of the test Link socket in a run directory.
#[allow(dead_code)]
pub const TEST_LINK: &str = "test-link.sock";

/// Binds `run/TEST_LINK`: each connection to it acts as a stream of a
/// Link to the Saved host `alias` (see `Daemon::serve_with_test_links`).
/// Call it after `bind_sockets`, which makes `run`.
#[allow(dead_code)]
pub fn test_link(run: &Path, alias: &str) -> (String, tokio::net::UnixListener) {
    // A Daemon started again on the same run directory replaces it.
    let _ = std::fs::remove_file(run.join(TEST_LINK));
    let l = tokio::net::UnixListener::bind(run.join(TEST_LINK)).unwrap();
    (alias.to_string(), l)
}

/// A short temp dir under /tmp (Unix socket paths are limited to ~104
/// bytes), removed on drop. `tag` keeps the dirs of test binaries apart.
pub struct TmpDir(pub PathBuf);

impl TmpDir {
    pub fn new(tag: &str) -> TmpDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = PathBuf::from(format!("/tmp/rw.{tag}{}.{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
