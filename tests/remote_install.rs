//! The two `BinarySource` adapters, and the install over a fake `ssh`.

#[path = "common/release.rs"]
mod release;
#[path = "common/script.rs"]
mod script;

use release::sums_line;
use script::write_script;
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

/// Serves each `(path, body)` under the `/v0.1.0` base.
fn serve_all(routes: Vec<(&'static str, Vec<u8>)>) -> String {
    release::serve_all("/v0.1.0", routes)
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

/// A fake `ssh` that runs the remote command locally with `/bin/sh`, with
/// `PATH` set to `$FAKE_REMOTE_PATH`, and answers `uname -sm` with `$FAKE_UNAME`.
fn fake_ssh(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ssh = bin.join("ssh");
    write_script(
        &ssh,
        "shift 2\nif [ \"$1\" = \"uname -sm\" ]; then echo \"$FAKE_UNAME\"; exit 0; fi\n\
         export PATH=\"$FAKE_REMOTE_PATH\"\nexec /bin/sh -c \"$1\"",
    );
    bin
}

/// The remote `PATH` for the fake `ssh`: only the tools the remote script
/// needs, linked from the real `PATH`. It never holds the real `xdg-mime`.
fn remote_toolbox(dir: &Path) -> PathBuf {
    let tools = dir.join("toolbox");
    std::fs::create_dir_all(&tools).unwrap();
    let path = std::env::var("PATH").unwrap();
    for tool in [
        "sh", "mkdir", "rm", "cat", "chmod", "mv", "ln", "head", "awk",
    ] {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on PATH"));
        let link = tools.join(tool);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(found, link).unwrap();
        }
    }
    tools
}

/// A fake `xdg-mime` in `<dir>/xdg` that writes its arguments to `<dir>/xdg-mime.args`.
fn fake_xdg_mime(dir: &Path) -> PathBuf {
    let bin = dir.join("xdg");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("xdg-mime");
    let args = dir.join("xdg-mime.args");
    write_script(&tool, &format!("echo \"$@\" >>\"{}\"", args.display()));
    bin
}

fn run_install(dir: &Path, home: &Path, uname: &str, local: &Path) -> std::process::Output {
    let local = local.to_str().unwrap();
    run_install_with(dir, home, uname, &["--local-build", local])
}

/// `rowser remote install myhost <extra>` over the fake `ssh`, with `HOME` at `home`.
fn run_install_with(dir: &Path, home: &Path, uname: &str, extra: &[&str]) -> std::process::Output {
    run_install_remote(dir, home, uname, extra, None)
}

/// As [`run_install_with`], with `remote_bin` (if any) first on the remote `PATH`.
fn run_install_remote(
    dir: &Path,
    home: &Path,
    uname: &str,
    extra: &[&str],
    remote_bin: Option<&Path>,
) -> std::process::Output {
    let path = format!(
        "{}:{}",
        fake_ssh(dir).display(),
        std::env::var("PATH").unwrap()
    );
    let toolbox = remote_toolbox(dir);
    let remote_path = match remote_bin {
        Some(bin) => format!("{}:{}", bin.display(), toolbox.display()),
        None => toolbox.display().to_string(),
    };
    Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["remote", "install", "myhost"])
        .args(extra)
        .env("PATH", path)
        .env("FAKE_REMOTE_PATH", remote_path)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/rowser-test",
        )
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

const DESKTOP: &str = ".local/share/applications/rowser.desktop";
const MIMEAPPS: &str = ".config/mimeapps.list";

fn desktop_content(exec: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=rowser\n\
         Comment=Open URLs in the browser on your local machine\n\
         Exec={exec} open %u\nNoDisplay=true\n\
         MimeType=x-scheme-handler/http;x-scheme-handler/https;\n"
    )
}

#[test]
fn install_writes_the_desktop_file_and_mimeapps_list_without_xdg_mime() {
    let dir = temp_dir("desk");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    let exec = format!("\"{}/.local/bin/rowser\"", home.display());
    assert_eq!(
        std::fs::read_to_string(home.join(DESKTOP)).unwrap(),
        desktop_content(&exec)
    );
    assert_eq!(
        std::fs::read_to_string(home.join(MIMEAPPS)).unwrap(),
        "[Default Applications]\nx-scheme-handler/http=rowser.desktop\n\
         x-scheme-handler/https=rowser.desktop\n"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("~/.local/share/applications/rowser.desktop")
            && stdout.contains("~/.config/mimeapps.list"),
        "{stdout}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_edits_mimeapps_list_keeping_other_lines_and_a_rerun_is_idempotent() {
    let dir = temp_dir("mimeedit");
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let before = "[Added Associations]\nx-scheme-handler/http=firefox.desktop;\n\n\
                  [Default Applications]\ntext/html=firefox.desktop\n\
                  x-scheme-handler/http=firefox.desktop\nx-scheme-handler/https = firefox.desktop\n\
                  x-scheme-handler/mailto=thunderbird.desktop\n\n\
                  [Removed Associations]\nimage/png=gimp.desktop\n";
    std::fs::write(home.join(MIMEAPPS), before).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let want = "[Added Associations]\nx-scheme-handler/http=firefox.desktop;\n\n\
                [Default Applications]\ntext/html=firefox.desktop\n\
                x-scheme-handler/mailto=thunderbird.desktop\n\n\
                x-scheme-handler/http=rowser.desktop\nx-scheme-handler/https=rowser.desktop\n\
                [Removed Associations]\nimage/png=gimp.desktop\n";
    for _ in 0..2 {
        let out = run_install(&dir, &home, "Linux x86_64", &local);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(std::fs::read_to_string(home.join(MIMEAPPS)).unwrap(), want);
    }
    assert!(!home.join(".config/mimeapps.list.tmp").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_adds_a_default_applications_section_to_a_mimeapps_list_without_one() {
    let dir = temp_dir("mimeadd");
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::fs::write(
        home.join(MIMEAPPS),
        "[Added Associations]\ntext/plain=gedit.desktop;\n",
    )
    .unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(MIMEAPPS)).unwrap(),
        "[Added Associations]\ntext/plain=gedit.desktop;\n\n[Default Applications]\n\
         x-scheme-handler/http=rowser.desktop\nx-scheme-handler/https=rowser.desktop\n"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_keeps_a_mimeapps_list_symlink_and_warns() {
    let dir = temp_dir("mimesym");
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let target = dir.join("dotfiles-mimeapps.list");
    std::fs::write(&target, "[Default Applications]\n").unwrap();
    std::os::unix::fs::symlink(&target, home.join(MIMEAPPS)).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_link(home.join(MIMEAPPS)).unwrap(), target);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "[Default Applications]\n"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning") && stderr.contains("~/.config/mimeapps.list"),
        "{stderr}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_calls_xdg_mime_when_the_remote_has_it_and_leaves_mimeapps_list() {
    let dir = temp_dir("xdgmime");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();
    let xdg = fake_xdg_mime(&dir);

    let args = ["--local-build", local.to_str().unwrap()];
    let out = run_install_remote(&dir, &home, "Linux x86_64", &args, Some(&xdg));
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("xdg-mime.args")).unwrap(),
        "default rowser.desktop x-scheme-handler/http x-scheme-handler/https\n"
    );
    assert!(home.join(DESKTOP).is_file());
    assert!(!home.join(MIMEAPPS).exists());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("xdg-mime"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_quotes_the_desktop_exec_path_with_a_space_in_home() {
    let dir = temp_dir("deskspace");
    let home = dir.join("my home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    let exec = format!("\"{}/.local/bin/rowser\"", home.display());
    assert_eq!(
        std::fs::read_to_string(home.join(DESKTOP)).unwrap(),
        desktop_content(&exec)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_escapes_reserved_characters_in_the_desktop_exec_path() {
    let dir = temp_dir("deskesc");
    let home = dir.join("h\"o`m$e%x\\y");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    // Desktop Entry spec: `"`, `` ` ``, `$` and `\` get a backslash inside the
    // quotes, `%` becomes `%%`, and then every backslash is doubled by the
    // string escape rule.
    let base = dir.display().to_string();
    let exec = format!("\"{base}/h\\\\\"o\\\\`m\\\\$e%%x\\\\\\\\y/.local/bin/rowser\"");
    assert_eq!(
        std::fs::read_to_string(home.join(DESKTOP)).unwrap(),
        desktop_content(&exec)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_warns_and_succeeds_when_the_applications_dir_cannot_be_written() {
    let dir = temp_dir("deskfail");
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".local/share")).unwrap();
    // `applications` is a file, so `mkdir -p` fails.
    std::fs::write(home.join(".local/share/applications"), "").unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();

    let out = run_install(&dir, &home, "Linux x86_64", &local);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(home.join(".local/bin/rowser")).unwrap(),
        "LOCAL"
    );
    assert!(!home.join(MIMEAPPS).exists());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "warning: cannot write ~/.local/share/applications/rowser.desktop: \
             rowser is not the default browser."
        ),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("rowser.desktop"), "{stdout}");
    assert!(stdout.contains("export BROWSER=rowser"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn install_warns_and_succeeds_when_xdg_mime_fails() {
    let dir = temp_dir("xdgfail");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let local = dir.join("rowser-built");
    std::fs::write(&local, "LOCAL").unwrap();
    let bin = dir.join("xdg");
    std::fs::create_dir_all(&bin).unwrap();
    write_script(&bin.join("xdg-mime"), "exit 1");

    let args = ["--local-build", local.to_str().unwrap()];
    let out = run_install_remote(&dir, &home, "Linux x86_64", &args, Some(&bin));
    assert!(out.status.success(), "{out:?}");
    assert!(home.join(DESKTOP).is_file());
    assert!(!home.join(MIMEAPPS).exists());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: xdg-mime failed: rowser is not the default browser."),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Wrote ~/.local/share/applications/rowser.desktop"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("Made rowser.desktop the default"),
        "{stdout}"
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
