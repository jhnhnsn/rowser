//! `rowser daemon` reads `ROWSER_RUNTIME_DIR`, and `--runtime-dir` wins.

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use common::TmpDir;

const EXE: &str = env!("CARGO_BIN_EXE_rowser");

/// Starts `rowser daemon` with `ROWSER_RUNTIME_DIR=var` and `flag` as extra
/// arguments, and returns the directory of its "listening in" line.
fn listening_dir(var: &Path, flag: &[&Path]) -> String {
    let mut cmd = Command::new(EXE);
    cmd.arg("daemon");
    for dir in flag {
        cmd.arg("--runtime-dir").arg(dir);
    }
    let mut child = cmd
        .args(["--open-command", "true {url}"])
        .env("ROWSER_RUNTIME_DIR", var)
        // Never read the real config: this directory has no config file.
        .env("XDG_CONFIG_HOME", var.join("no-config"))
        .env_remove("ROWSER_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let line = lines.next().unwrap().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    line.strip_prefix("rowser daemon: listening in ")
        .unwrap_or_else(|| panic!("{line}"))
        .to_string()
}

fn private_dir(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn rowser_daemon_listens_in_rowser_runtime_dir() {
    let dir = TmpDir::new("rtv");
    let var = dir.0.join("v");
    private_dir(&var);
    assert_eq!(listening_dir(&var, &[]), var.display().to_string());
}

#[test]
fn the_runtime_dir_flag_wins_over_rowser_runtime_dir() {
    let dir = TmpDir::new("rtf");
    let (var, flag) = (dir.0.join("v"), dir.0.join("f"));
    private_dir(&var);
    private_dir(&flag);
    assert_eq!(listening_dir(&var, &[&flag]), flag.display().to_string());
    assert!(!var.join("rowserd.sock").exists());
}
