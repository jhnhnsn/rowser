//! The `install.sh` installer, run against a local HTTP server with a temp HOME.

#[path = "common/release.rs"]
mod release;
#[path = "common/script.rs"]
mod script;

use release::sums_line;
use script::write_script;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ASSET: &str = "rowser-x86_64-unknown-linux-musl.tar.gz";
const SUMS: &str = "/rel/SHA256SUMS";
const ALL_ASSETS: [&str; 4] = [
    "rowser-x86_64-unknown-linux-musl.tar.gz",
    "rowser-aarch64-unknown-linux-musl.tar.gz",
    "rowser-aarch64-apple-darwin.tar.gz",
    "rowser-x86_64-apple-darwin.tar.gz",
];

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rowser-inst-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The server path of `asset`.
fn route(asset: &str) -> &'static str {
    Box::leak(format!("/rel/{asset}").into_boxed_str())
}

fn serve_all(routes: Vec<(&'static str, Vec<u8>)>) -> String {
    release::serve_all("/rel", routes)
}

/// A release-style tarball holding a fake `rowser` that prints `version`.
fn tarball(dir: &Path, version: &str) -> Vec<u8> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_script(&src.join("rowser"), &format!("echo 'rowser {version}'"));
    let out = dir.join("a.tar.gz");
    let ok = Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(&src)
        .arg("rowser")
        .status()
        .unwrap();
    assert!(ok.success());
    std::fs::read(out).unwrap()
}

/// A Release that serves only `asset` (a real tarball) plus a four-line
/// SHA256SUMS in which the other entries hash junk.
fn release_with(dir: &Path, version: &str, asset: &str) -> String {
    let body = tarball(dir, version);
    let sums: String = ALL_ASSETS
        .iter()
        .map(|name| {
            if *name == asset {
                sums_line(name, &body)
            } else {
                sums_line(name, name.as_bytes())
            }
        })
        .collect();
    serve_all(vec![(route(asset), body), (SUMS, sums.into_bytes())])
}

/// A good Release for Linux x86_64.
fn good_release(dir: &Path, version: &str) -> String {
    release_with(dir, version, ASSET)
}

/// Runs install.sh with HOME and XDG_CONFIG_HOME under `dir`, a fake `uname`
/// that prints `system`, and `extra` env vars.
fn run(dir: &Path, base: &str, system: &str, extra: &[(&str, &Path)]) -> Output {
    let tools = dir.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    write_script(&tools.join("uname"), &format!("echo '{system}'"));
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let path = format!("{}:{}", tools.display(), std::env::var("PATH").unwrap());
    let mut cmd = Command::new("sh");
    cmd.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("PATH", path)
        .env("ROWSER_INSTALL_BASE_URL", base)
        .env_remove("ROWSER_INSTALL_DIR");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn installed(dir: &Path) -> PathBuf {
    dir.join("home/.local/bin/rowser")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn installs_the_binary_and_prints_next_steps() {
    let dir = temp_dir("good");
    let base = good_release(&dir, "9.9.9");
    let out = run(&dir, &base, "Linux x86_64", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let bin = installed(&dir);
    let ver = Command::new(&bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ver.stdout), "rowser 9.9.9\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Installed rowser 9.9.9"), "{stdout}");
    assert!(stdout.contains("rowser daemon install"), "{stdout}");
    assert!(stdout.contains("rowser remote install <host>"), "{stdout}");
    assert!(stdout.contains("is not on your PATH"), "{stdout}");
    assert_eq!(entries(bin.parent().unwrap()), ["rowser"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn each_system_gets_its_own_asset() {
    let cases = [
        ("Linux x86_64", "rowser-x86_64-unknown-linux-musl.tar.gz"),
        ("Linux aarch64", "rowser-aarch64-unknown-linux-musl.tar.gz"),
        ("Linux arm64", "rowser-aarch64-unknown-linux-musl.tar.gz"),
        ("Darwin arm64", "rowser-aarch64-apple-darwin.tar.gz"),
        ("Darwin x86_64", "rowser-x86_64-apple-darwin.tar.gz"),
    ];
    for (n, (system, asset)) in cases.iter().enumerate() {
        let dir = temp_dir(&format!("map{n}"));
        let base = release_with(&dir, "9.9.9", asset);
        let out = run(&dir, &base, system, &[]);
        assert!(out.status.success(), "{system}: {}", stderr(&out));
        assert!(installed(&dir).exists(), "{system}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn a_second_run_replaces_the_binary() {
    let dir = temp_dir("again");
    let first = good_release(&dir.join("r1"), "1.0.0");
    assert!(run(&dir, &first, "Linux x86_64", &[]).status.success());
    let second = good_release(&dir.join("r2"), "2.0.0");
    let out = run(&dir, &second, "Linux x86_64", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let ver = Command::new(installed(&dir)).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ver.stdout), "rowser 2.0.0\n");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rowser_install_dir_overrides_the_folder() {
    let dir = temp_dir("dir");
    let base = good_release(&dir, "9.9.9");
    let target = dir.join("custom/bin");
    let out = run(
        &dir,
        &base,
        "Linux x86_64",
        &[("ROWSER_INSTALL_DIR", &target)],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(target.join("rowser").exists());
    assert!(!installed(&dir).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_uppercase_hash_in_sha256sums_is_accepted() {
    let dir = temp_dir("upper");
    let body = tarball(&dir, "9.9.9");
    let line = sums_line(ASSET, &body);
    let (hash, name) = line.split_once("  ").unwrap();
    let sums = format!("{}  {name}", hash.to_uppercase());
    let base = serve_all(vec![(route(ASSET), body), (SUMS, sums.into_bytes())]);
    let out = run(&dir, &base, "Linux x86_64", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(installed(&dir).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Runs a failing install over an existing binary and checks it is unchanged.
fn assert_refused(tag: &str, routes: Vec<(&'static str, Vec<u8>)>, want: &str) {
    let dir = temp_dir(tag);
    let good = good_release(&dir.join("r0"), "1.0.0");
    assert!(run(&dir, &good, "Linux x86_64", &[]).status.success());
    let before = std::fs::read(installed(&dir)).unwrap();
    let base = serve_all(routes);
    let out = run(&dir, &base, "Linux x86_64", &[]);
    assert!(!out.status.success());
    assert_eq!(stderr(&out), format!("rowser-installer: {want}\n"));
    assert_eq!(std::fs::read(installed(&dir)).unwrap(), before);
    assert_eq!(entries(installed(&dir).parent().unwrap()), ["rowser"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_checksum_mismatch_stops_the_install() {
    let dir = temp_dir("bad-src");
    let body = tarball(&dir, "6.6.6");
    let sums = sums_line(ASSET, b"tampered").into_bytes();
    assert_refused(
        "bad",
        vec![(route(ASSET), body), (SUMS, sums)],
        &format!("{ASSET} does not match SHA256SUMS"),
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_missing_entry_stops_the_install() {
    let dir = temp_dir("noent-src");
    let body = tarball(&dir, "6.6.6");
    let sums = sums_line("rowser-x86_64-apple-darwin.tar.gz", &body).into_bytes();
    assert_refused(
        "noent",
        vec![(route(ASSET), body), (SUMS, sums)],
        &format!("SHA256SUMS has no entry for {ASSET}"),
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_missing_sha256sums_stops_the_install() {
    // The message names the server URL, so this test does not use assert_refused.
    let dir = temp_dir("nosums");
    let good = good_release(&dir.join("r0"), "1.0.0");
    assert!(run(&dir, &good, "Linux x86_64", &[]).status.success());
    let body = tarball(&dir.join("r1"), "6.6.6");
    let base = serve_all(vec![(route(ASSET), body)]);
    let out = run(&dir, &base, "Linux x86_64", &[]);
    assert!(!out.status.success());
    // curl prints its own error line first.
    let want = format!("rowser-installer: cannot download {base}/SHA256SUMS\n");
    assert!(stderr(&out).ends_with(&want), "{}", stderr(&out));
    assert_eq!(entries(installed(&dir).parent().unwrap()), ["rowser"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_failed_copy_leaves_no_part_file() {
    let dir = temp_dir("part");
    let good = good_release(&dir.join("r0"), "1.0.0");
    assert!(run(&dir, &good, "Linux x86_64", &[]).status.success());
    let before = std::fs::read(installed(&dir)).unwrap();
    // A fake chmod that fails, after cp has written the part file.
    write_script(&dir.join("tools/chmod"), "exit 1");
    let base = good_release(&dir.join("r1"), "2.0.0");
    let out = run(&dir, &base, "Linux x86_64", &[]);
    assert!(!out.status.success());
    let part = installed(&dir).with_file_name(".rowser.part.");
    let want = format!("rowser-installer: cannot make {}", part.display());
    assert!(stderr(&out).starts_with(&want), "{}", stderr(&out));
    assert!(stderr(&out).ends_with(" executable\n"), "{}", stderr(&out));
    assert_eq!(std::fs::read(installed(&dir)).unwrap(), before);
    assert_eq!(entries(installed(&dir).parent().unwrap()), ["rowser"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unknown_system_stops_the_install() {
    let dir = temp_dir("unknown");
    let base = good_release(&dir, "9.9.9");
    let out = run(&dir, &base, "FreeBSD amd64", &[]);
    assert!(!out.status.success());
    assert_eq!(
        stderr(&out),
        "rowser-installer: unsupported system: FreeBSD amd64\n"
    );
    assert!(!dir.join("home/.local").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
