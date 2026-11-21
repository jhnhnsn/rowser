//! Helpers shared by the unit tests.

#[path = "../tests/common/script.rs"]
mod script;
pub use script::write_script;

#[cfg(test)]
mod tests {
    use super::*;

    /// Many threads write and run scripts at once, as parallel tests do.
    /// A fork in one thread must not leave another thread's script busy
    /// (ETXTBSY, #55).
    #[test]
    fn scripts_written_while_other_threads_fork_run() {
        let dir = std::env::temp_dir().join(format!("rowser-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        let p = dir.join(format!("s{t}-{i}"));
                        write_script(&p, "exit 0");
                        let s = std::process::Command::new(&p)
                            .status()
                            .unwrap_or_else(|e| panic!("cannot run {}: {e}", p.display()));
                        assert!(s.success());
                    }
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join()).collect();
        let _ = std::fs::remove_dir_all(&dir);
        for r in results {
            r.unwrap();
        }
    }
}
