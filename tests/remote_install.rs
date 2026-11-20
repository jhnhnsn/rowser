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
/// Computed with `sha2`, so the tests need no `sha256sum` (macOS has none).
fn sums_line(name: &str, body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{hash}  {name}\n")
}

const X86_TGZ: &str = "/v0.1.0/rowser-x86_64-unknown-linux-musl.tar.gz";
const SUMS: &str = "/v0.1.0/SHA256SUMS";

#[test]
fn release_download_fetches_checks_and_unpacks_the_asset() {
    let dir = temp_dir("dl");
    let body = tarball(&dir, "BINARY-x86");
    let other = sums_line("rowser-aarch64-unknown-linux-musl.tar.gz", b"other");
    let mine = sums_line("rowser-x86_64-unknown-linux-musl.tar.gz", &body);
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
    let sums = sums_line("rowser-x86_64-unknown-linux-musl.tar.gz", b"tampered");
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
    let sums = sums_line("rowser-aarch64-unknown-linux-musl.tar.gz", &body);
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
    let local = local.to_str().unwrap();
    run_install_with(dir, home, uname, &["--local-build", local])
}

/// `rowser remote install myhost <extra>` over the fake `ssh`, with `HOME` at `home`.
fn run_install_with(dir: &Path, home: &Path, uname: &str, extra: &[&str]) -> std::process::Output {
    let path = format!(
        "{}:{}",
        fake_ssh(dir).display(),
        std::env::var("PATH").unwrap()
    );
    Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["remote", "install", "myhost"])
        .args(extra)
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

const ENV_D: &str = ".config/environment.d/rowser.conf";
use rowser::remote_install::ENVIRONMENT_D_CONTENT as ENV_D_CONTENT;

#[test]
fn install_writes_environment_d_for_user_services_and_a_rerun_is_idempotent() {
    let dir = temp_dir("envd");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(ENV_D)).unwrap(),
        ENV_D_CONTENT
    );
    assert!(!home.join(".config/environment.d/rowser.conf.tmp").exists());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("~/.config/environment.d/rowser.conf") && stdout.contains("next login"),
        "{stdout}"
    );

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(ENV_D)).unwrap(),
        ENV_D_CONTENT
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("environment.d"), "{stderr}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_keeps_a_users_own_environment_d_file_and_warns() {
    let dir = temp_dir("envdown");
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".config/environment.d")).unwrap();
    std::fs::write(home.join(ENV_D), "BROWSER=firefox\n").unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(ENV_D)).unwrap(),
        "BROWSER=firefox\n"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("~/.config/environment.d/rowser.conf")
            && stderr.contains("not written by rowser"),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("next login"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_replaces_an_old_environment_d_file_rowser_wrote() {
    let dir = temp_dir("envdold");
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".config/environment.d")).unwrap();
    let old = "# Written by rowser remote install; rowser overwrites this file.\nBROWSER=old\n";
    std::fs::write(home.join(ENV_D), old).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(ENV_D)).unwrap(),
        ENV_D_CONTENT
    );
    assert!(!home.join(".config/environment.d/rowser.conf.tmp").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_does_not_write_through_a_planted_environment_d_temp_symlink() {
    let dir = temp_dir("envdlink");
    let home = dir.join("home");
    let envd = home.join(".config/environment.d");
    std::fs::create_dir_all(&envd).unwrap();
    let outside = dir.join("outside");
    std::fs::write(&outside, "KEEP").unwrap();
    std::os::unix::fs::symlink(&outside, envd.join("rowser.conf.tmp")).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "KEEP");
    assert_eq!(
        std::fs::read_to_string(home.join(ENV_D)).unwrap(),
        ENV_D_CONTENT
    );
    assert!(std::fs::symlink_metadata(envd.join("rowser.conf.tmp")).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_works_with_a_space_in_home() {
    let dir = temp_dir("envdspace");
    let home = dir.join("env d");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(".local/bin/rowser")).unwrap(),
        "LOCAL"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(ENV_D)).unwrap(),
        ENV_D_CONTENT
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_keeps_a_foreign_or_dangling_environment_d_symlink_and_warns() {
    let dir = temp_dir("envdsym");
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();
    let foreign = dir.join("foreign.conf");
    std::fs::write(&foreign, "BROWSER=firefox\n").unwrap();

    for (tag, target) in [("foreign", foreign.clone()), ("dangling", dir.join("gone"))] {
        let home = dir.join(tag);
        let envd = home.join(".config/environment.d");
        std::fs::create_dir_all(&envd).unwrap();
        std::os::unix::fs::symlink(&target, envd.join("rowser.conf")).unwrap();

        let out = run_install(&dir, &home, "Linux x86_64", &local);
        assert!(out.status.success(), "{tag}: {out:?}");
        assert_eq!(
            std::fs::read_link(envd.join("rowser.conf")).unwrap(),
            target,
            "{tag}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("not written by rowser"), "{tag}: {stderr}");
    }
    assert_eq!(
        std::fs::read_to_string(&foreign).unwrap(),
        "BROWSER=firefox\n"
    );
    assert!(!dir.join("gone").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_warns_and_succeeds_when_environment_d_cannot_be_written() {
    let dir = temp_dir("envdfail");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // `~/.config` is a file, so `mkdir -p ~/.config/environment.d` fails.
    std::fs::write(home.join(".config"), "").unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(".local/bin/rowser")).unwrap(),
        "LOCAL"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: cannot write ~/.config/environment.d/rowser.conf"),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("next login"), "{stdout}");
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

#[test]
fn install_refuses_a_download_that_does_not_match_sha256sums_and_writes_nothing() {
    let dir = temp_dir("instbad");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let body = tarball(&dir, "BINARY-x86");
    let sums = sums_line("rowser-x86_64-unknown-linux-musl.tar.gz", b"tampered");
    let base = serve_all(vec![(X86_TGZ, body), (SUMS, sums.into_bytes())]);

    let out = run_install_with(&dir, &home, "Linux x86_64", &["--release-url", &base]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not match SHA256SUMS"), "{stderr}");
    assert!(!home.join(".local").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_with_a_local_build_skips_the_sha256sums_check() {
    let dir = temp_dir("instskip");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();
    // The release URL serves no SHA256SUMS: a check would refuse the install.
    let base = serve("/v0.1.0/none", Vec::new());

    let local = local.to_str().unwrap();
    let args = ["--local-build", local, "--release-url", &base];
    let out = run_install_with(&dir, &home, "Linux x86_64", &args);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(".local/bin/rowser")).unwrap(),
        "LOCAL"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
