//! `rowser daemon install` over a fake `systemctl`, in a temp home.
#![cfg(not(target_os = "macos"))]

#[path = "common/script.rs"]
mod script;

use script::write_script;
use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rowser-svc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fake `systemctl` that logs each call to `$FAKE_LOG` and answers
/// `is-active graphical-session.target` with `$FAKE_ACTIVE`.
fn fake_systemctl(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("systemctl");
    write_script(
        &exe,
        "echo \"$*\" >> \"$FAKE_LOG\"\n\
         if [ \"$2\" = is-active ]; then echo \"$FAKE_ACTIVE\"; [ \"$FAKE_ACTIVE\" = active ]; exit $?; fi\n\
         exit 0",
    );
    bin
}

fn install(tag: &str, active: &str) -> (std::process::Output, String, PathBuf) {
    let dir = temp_dir(tag);
    let path = format!(
        "{}:{}",
        fake_systemctl(&dir).display(),
        std::env::var("PATH").unwrap()
    );
    let log = dir.join("log");
    let out = Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["daemon", "install"])
        .env("PATH", path)
        .env("HOME", &dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("FAKE_LOG", &log)
        .env("FAKE_ACTIVE", active)
        .output()
        .unwrap();
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    (out, calls, dir)
}

#[test]
fn install_reenables_and_restarts_in_a_graphical_session() {
    let (out, calls, dir) = install("active", "active");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        calls,
        "--user daemon-reload\n\
         --user reenable rowser.service\n\
         --user is-active graphical-session.target\n\
         --user restart rowser.service\n"
    );
    assert!(dir.join("config/systemd/user/rowser.service").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_without_a_graphical_session_does_not_start_the_daemon() {
    let (out, calls, dir) = install("inactive", "inactive");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        calls,
        "--user daemon-reload\n\
         --user reenable rowser.service\n\
         --user is-active graphical-session.target\n"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("starts with the next graphical session"),
        "{text}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
