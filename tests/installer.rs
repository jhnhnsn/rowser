//! The `install.sh` installer, run against a local HTTP server with a temp HOME.

#[path = "common/script.rs"]
mod script;

use script::write_script;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TGZ: &str = "/rel/rowser-x86_64-unknown-linux-musl.tar.gz";
const SUMS: &str = "/rel/SHA256SUMS";
const ASSET: &str = "rowser-x86_64-unknown-linux-musl.tar.gz";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rowser-inst-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serves each `(path, body)` for `GET <path>` and 404 for any other path.
fn serve_all(routes: Vec<(&'static str, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let hit = routes
                .iter()
                .find(|(path, _)| req.starts_with(&format!("GET {path} ")));
            let (status, body) = match hit {
                Some((_, body)) => ("200 OK", body.clone()),
                None => ("404 Not Found", Vec::new()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://{addr}/rel")
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

fn sums_line(name: &str, body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{hash}  {name}\n")
}

/// A good Release: the tarball plus four-line SHA256SUMS.
fn good_release(dir: &Path, version: &str) -> String {
    let body = tarball(dir, version);
    let sums = format!(
        "{}{}{}{}",
        sums_line("rowser-aarch64-unknown-linux-musl.tar.gz", b"a"),
        sums_line(ASSET, &body),
        sums_line("rowser-aarch64-apple-darwin.tar.gz", b"b"),
        sums_line("rowser-x86_64-apple-darwin.tar.gz", b"c"),
    );
    serve_all(vec![(TGZ, body), (SUMS, sums.into_bytes())])
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
    let entries: Vec<_> = std::fs::read_dir(bin.parent().unwrap()).unwrap().collect();
    assert_eq!(entries.len(), 1, "only the binary is installed");
    std::fs::remove_dir_all(dir).unwrap();
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
        "Darwin arm64",
        &[("ROWSER_INSTALL_DIR", &target)],
    );
    // Darwin arm64 asks for the Apple asset, which this server lacks.
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("rowser-aarch64-apple-darwin.tar.gz"),
        "{}",
        stderr(&out)
    );
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

/// Runs a failing install over an existing binary and checks it is unchanged.
fn assert_refused(tag: &str, routes: Vec<(&'static str, Vec<u8>)>, want: &str) {
    let dir = temp_dir(tag);
    let good = good_release(&dir.join("r0"), "1.0.0");
    assert!(run(&dir, &good, "Linux x86_64", &[]).status.success());
    let before = std::fs::read(installed(&dir)).unwrap();
    let base = serve_all(routes);
    let out = run(&dir, &base, "Linux x86_64", &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains(want), "{}", stderr(&out));
    assert_eq!(std::fs::read(installed(&dir)).unwrap(), before);
    let entries: Vec<_> = std::fs::read_dir(installed(&dir).parent().unwrap())
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1, "the install folder is unchanged");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_checksum_mismatch_stops_the_install() {
    let dir = temp_dir("bad-src");
    let body = tarball(&dir, "6.6.6");
    let sums = sums_line(ASSET, b"tampered").into_bytes();
    assert_refused(
        "bad",
        vec![(TGZ, body), (SUMS, sums)],
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
        vec![(TGZ, body), (SUMS, sums)],
        &format!("SHA256SUMS has no entry for {ASSET}"),
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_missing_sha256sums_stops_the_install() {
    let dir = temp_dir("nosums-src");
    let body = tarball(&dir, "6.6.6");
    assert_refused("nosums", vec![(TGZ, body)], "/rel/SHA256SUMS");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unknown_system_stops_the_install() {
    let dir = temp_dir("unknown");
    let base = good_release(&dir, "9.9.9");
    let out = run(&dir, &base, "FreeBSD amd64", &[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("unsupported system: FreeBSD amd64"),
        "{}",
        stderr(&out)
    );
    assert!(!dir.join("home/.local").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
