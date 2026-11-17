//! Helpers shared by the integration tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

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
