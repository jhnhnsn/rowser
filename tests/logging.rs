//! `ROWSER_LOG` starts a tracing subscriber on stderr in each process.

mod common;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::TmpDir;

const EXE: &str = env!("CARGO_BIN_EXE_rowser");

/// Starts `rowser daemon` in `dir`, with stderr piped and lines sent to the
/// returned channel. Waits for the "listening" line and returns the lines
/// before it.
fn daemon(
    dir: &Path,
    config_text: &str,
    log: Option<&str>,
) -> (Child, mpsc::Receiver<String>, Vec<String>) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Never read the real config.
    let config = dir.join("config");
    std::fs::create_dir_all(config.join("rowser")).unwrap();
    std::fs::write(config.join("rowser").join("config.toml"), config_text).unwrap();
    let open_command = "true {url}";
    let mut cmd = Command::new(EXE);
    cmd.arg("daemon")
        .env("XDG_CONFIG_HOME", &config)
        .arg("--runtime-dir")
        .arg(dir)
        .arg("--open-command")
        .arg(open_command)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/nobus", dir.display()),
        )
        .env_remove("ROWSER_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(level) = log {
        cmd.env("ROWSER_LOG", level);
    }
    let mut child = cmd.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut early = Vec::new();
    loop {
        let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        if line.contains("listening in") {
            break;
        }
        early.push(line);
    }
    (child, rx, early)
}

/// Lines that arrive within `wait`.
fn drain(rx: &mpsc::Receiver<String>, wait: Duration) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(line) = rx.recv_timeout(wait) {
        out.push(line);
    }
    out
}

#[test]
fn rowser_log_debug_shows_debug_lines_on_stderr() {
    let dir = TmpDir::new("logd");
    let (mut child, _rx, early) = daemon(&dir.0, "", Some("debug"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(early.iter().any(|l| l.contains("DEBUG")), "{early:?}");
}

#[test]
fn without_rowser_log_an_unverified_entry_shows_one_warning() {
    let dir = TmpDir::new("logw");
    let (mut child, rx, early) = daemon(&dir.0, "[unverified]\nopen = \"system\"\n", None);
    let mut lines = early;
    lines.extend(drain(&rx, Duration::from_millis(300)));
    child.kill().unwrap();
    child.wait().unwrap();
    let warns: Vec<_> = lines.iter().filter(|l| l.contains("WARN")).collect();
    assert_eq!(warns.len(), 1, "{lines:?}");
    assert!(
        warns[0].contains("the [unverified] entry is removed and ignored"),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.contains("DEBUG")), "{lines:?}");
}

/// Runs `rowser reload` against the Daemon in `dir`.
fn reload(dir: &Path) {
    let out = Command::new(EXE)
        .arg("reload")
        .env("ROWSER_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_reload_warns_about_unverified_only_when_the_entry_is_new() {
    let dir = TmpDir::new("logr");
    let (mut child, rx, early) = daemon(&dir.0, "[unverified]\nopen = \"system\"\n", None);
    let mut lines = early;
    reload(&dir.0);
    reload(&dir.0);
    lines.extend(drain(&rx, Duration::from_millis(300)));
    let file = dir.0.join("config").join("rowser").join("config.toml");
    std::fs::write(&file, "").unwrap();
    reload(&dir.0);
    std::fs::write(&file, "[unverified]\nopen = \"system\"\n").unwrap();
    reload(&dir.0);
    lines.extend(drain(&rx, Duration::from_millis(300)));
    child.kill().unwrap();
    child.wait().unwrap();
    let warns = lines.iter().filter(|l| l.contains("[unverified]")).count();
    // One at start, none for the unchanged reloads, one when it comes back.
    assert_eq!(warns, 2, "{lines:?}");
}

#[test]
fn log_lines_never_go_to_stdout() {
    let out = Command::new(EXE)
        .arg("--version")
        .env("ROWSER_LOG", "trace")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DEBUG"), "{stderr}");
    assert!(
        !stdout.contains("DEBUG") && !stdout.contains("TRACE"),
        "{stdout}"
    );
    assert!(stdout.contains("rowser"), "{stdout}");
}
