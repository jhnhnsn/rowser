//! The two `BinarySource` adapters, and the install over a fake `ssh`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use rowser::remote_install::{BinarySource, LocalBuild, ReleaseDownload};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rowser-ri-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serves `body` for `GET <path>` and 404 for any other path, once per request.
fn serve(path: &'static str, body: Vec<u8>) -> String {
    serve_all(vec![(path, body)])
}

/// Serves each `(path, body)` for `GET <path>` and 404 for any other path.
fn serve_all(routes: Vec<(&'static str, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
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
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    format!("http://{addr}/v0.1.0")
}

/// A release-style tarball holding `rowser` with `content`.
fn tarball(dir: &Path, content: &str) -> Vec<u8> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("rowser"), content).unwrap();
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

/// The `sha256sum` line for `body` named `name`, as the release workflow writes it.
fn sums_line(dir: &Path, name: &str, body: &[u8]) -> String {
    let file = dir.join(name);
    std::fs::write(&file, body).unwrap();
    let out = Command::new("sha256sum")
        .arg(name)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

const X86_TGZ: &str = "/v0.1.0/rowser-x86_64-unknown-linux-musl.tar.gz";
const SUMS: &str = "/v0.1.0/SHA256SUMS";

#[test]
fn release_download_fetches_checks_and_unpacks_the_asset() {
    let dir = temp_dir("dl");
    let body = tarball(&dir, "BINARY-x86");
    let other = sums_line(&dir, "rowser-aarch64-unknown-linux-musl.tar.gz", b"other");
    let mine = sums_line(&dir, "rowser-x86_64-unknown-linux-musl.tar.gz", &body);
    let sums = format!("{other}{mine}").into_bytes();
    let base = serve_all(vec![(X86_TGZ, body), (SUMS, sums)]);
    let bin = ReleaseDownload { base_url: base }
        .binary("x86_64-unknown-linux-musl")
        .unwrap();
    assert_eq!(bin, b"BINARY-x86");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_download_refuses_a_hash_mismatch() {
    let dir = temp_dir("dlbad");
    let body = tarball(&dir, "BINARY-x86");
    let sums = sums_line(&dir, "rowser-x86_64-unknown-linux-musl.tar.gz", b"tampered");
    let base = serve_all(vec![(X86_TGZ, body), (SUMS, sums.into_bytes())]);
    let err = ReleaseDownload { base_url: base }
        .binary("x86_64-unknown-linux-musl")
        .unwrap_err();
    assert!(
        err.contains("rowser-x86_64-unknown-linux-musl.tar.gz does not match SHA256SUMS"),
        "{err}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_download_refuses_an_asset_without_an_entry() {
    let dir = temp_dir("dlnoent");
    let body = tarball(&dir, "BINARY-x86");
    let sums = sums_line(&dir, "rowser-aarch64-unknown-linux-musl.tar.gz", &body);
    let base = serve_all(vec![(X86_TGZ, body), (SUMS, sums.into_bytes())]);
    let err = ReleaseDownload { base_url: base }
        .binary("x86_64-unknown-linux-musl")
        .unwrap_err();
    assert!(
        err.contains("SHA256SUMS has no entry for rowser-x86_64-unknown-linux-musl.tar.gz"),
        "{err}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_download_refuses_a_missing_sha256sums() {
    let dir = temp_dir("dlnosums");
    let body = tarball(&dir, "BINARY-x86");
    let base = serve(X86_TGZ, body);
    let err = ReleaseDownload {
        base_url: base.clone(),
    }
    .binary("x86_64-unknown-linux-musl")
    .unwrap_err();
    assert!(
        err.contains(&format!("cannot download {base}/SHA256SUMS")),
        "{err}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_download_names_the_url_on_a_missing_asset() {
    let dir = temp_dir("dl404");
    let base = serve("/v0.1.0/none", Vec::new());
    let err = ReleaseDownload {
        base_url: base.clone(),
    }
    .binary("aarch64-unknown-linux-musl")
    .unwrap_err();
    assert!(
        err.contains(&format!("{base}/rowser-aarch64-unknown-linux-musl.tar.gz")),
        "{err}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn local_build_gives_its_path_and_refuses_a_missing_file() {
    let dir = temp_dir("local");
    let bin = dir.join("rowser");
    std::fs::write(&bin, "x").unwrap();
    assert_eq!(LocalBuild(bin).binary("any").unwrap(), b"x");
    let err = LocalBuild(dir.join("nope")).binary("any").unwrap_err();
    assert!(err.contains("is not a file"), "{err}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A fake `ssh` that runs the remote command locally with `sh`, and
/// answers `uname -sm` with `$FAKE_UNAME`.
fn fake_ssh(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nshift 2\nif [ \"$1\" = \"uname -sm\" ]; then echo \"$FAKE_UNAME\"; exit 0; fi\nexec sh -c \"$1\"\n",
    )
    .unwrap();
    let ok = Command::new("chmod").arg("755").arg(&ssh).status().unwrap();
    assert!(ok.success());
    bin
}

fn run_install(dir: &Path, home: &Path, uname: &str, local: &Path) -> std::process::Output {
    let path = format!(
        "{}:{}",
        fake_ssh(dir).display(),
        std::env::var("PATH").unwrap()
    );
    Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["remote", "install", "myhost", "--local-build"])
        .arg(local)
        .env("PATH", path)
        .env("HOME", home)
        .env("FAKE_UNAME", uname)
        .output()
        .unwrap()
}

#[test]
fn install_copies_the_binary_links_the_names_and_prints_the_browser_line() {
    let dir = temp_dir("inst");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    let bin = home.join(".local/bin");
    assert_eq!(
        std::fs::read_to_string(bin.join("rowser")).unwrap(),
        "LOCAL"
    );
    for name in ["xdg-open", "sensible-browser", "www-browser"] {
        assert_eq!(
            std::fs::read_link(bin.join(name)).unwrap(),
            Path::new("rowser")
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("export BROWSER=rowser"), "{stdout}");

    // A second run replaces the binary and keeps the links.
    std::fs::write(&local, "NEWER").unwrap();
    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(bin.join("rowser")).unwrap(),
        "NEWER"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_does_not_write_through_a_planted_temp_symlink() {
    let dir = temp_dir("link");
    let home = dir.join("home");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let outside = dir.join("outside");
    std::fs::write(&outside, "KEEP").unwrap();
    std::os::unix::fs::symlink(&outside, bin.join(".rowser.tmp")).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "KEEP");
    assert_eq!(
        std::fs::read_to_string(bin.join("rowser")).unwrap(),
        "LOCAL"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_refuses_an_unsupported_remote_and_writes_nothing() {
    let dir = temp_dir("unsup");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Darwin arm64", &local);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported system on the remote host `Darwin arm64`"),
        "{stderr}"
    );
    assert!(!home.join(".local").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
