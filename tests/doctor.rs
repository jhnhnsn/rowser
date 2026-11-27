mod common;
#[path = "common/script.rs"]
mod script;

use std::path::PathBuf;
use std::process::ExitCode;

use common::{test_link, TmpDir, TEST_LINK};
use rowser::browsers::{Os, Roots};
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
use rowser::doctor::{run_doctor, DoctorEnv, FIX};
use rowser::protocol::{Conn, Hello, LinkInfo, LinkState, Msg, Version, CONTROL_SOCKET};
use tokio::net::UnixListener;

/// A home whose PATH and BROWSER checks pass.
fn good_home(dir: &TmpDir) -> PathBuf {
    let home = dir.0.join("home");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("rowser"), "").unwrap();
    std::fs::set_permissions(
        bin.join("rowser"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    for n in rowser::platform::BROWSER_NAMES {
        std::os::unix::fs::symlink("rowser", bin.join(n)).unwrap();
    }
    home
}

fn env(dir: &TmpDir, over: Option<PathBuf>) -> DoctorEnv<Vec<u8>> {
    let home = good_home(dir);
    let bin = home.join(".local/bin");
    let config = home.join(".config");
    let data = home.join(".local/share");
    std::fs::create_dir_all(config.join("environment.d")).unwrap();
    std::fs::write(
        config.join("environment.d/rowser.conf"),
        "BROWSER=rowser\nPATH=$HOME/.local/bin:$PATH\n",
    )
    .unwrap();
    std::fs::write(
        config.join("mimeapps.list"),
        "[Default Applications]\nx-scheme-handler/http=rowser.desktop\n\
         x-scheme-handler/https=rowser.desktop\n",
    )
    .unwrap();
    std::fs::create_dir_all(data.join("applications")).unwrap();
    std::fs::write(data.join("applications/rowser.desktop"), "").unwrap();
    DoctorEnv {
        // Only the temp bin: no real xdg-mime or xdg-open from /usr/bin runs.
        path: Some(bin.clone().into_os_string()),
        browser: Some("xdg-open".into()),
        home: Some(home),
        // No folder: the outpost socket check has nothing to report.
        outpost_socket: Some(dir.0.join("no-outpost/outpost.sock")),
        socket_override: over,
        graphical: false,
        self_exe: Some(bin.join("rowser")),
        // Missing: no Local checks unless a test adds them.
        config_file: Ok(dir.0.join("cfg/rowser/config.toml")),
        runtime_dir: Ok(dir.0.join("rt")),
        // Missing: no installed service unless a test writes one.
        service_file: Ok(dir.0.join("svc/rowser.service")),
        browser_roots: empty_roots(dir),
        ssh: false,
        ssh_timeout: std::time::Duration::from_secs(1),
        stdout: Vec::new(),
    }
}

fn empty_roots(dir: &TmpDir) -> Roots {
    Roots {
        os: Os::Linux,
        app_dirs: vec![],
        data_dirs: vec![dir.0.join("share")],
        home: dir.0.join("bhome"),
        config_dir: dir.0.join("bconfig"),
    }
}

/// Browser roots with `google-chrome.desktop` and Chrome Profile `Work`.
fn chrome_roots(dir: &TmpDir) -> Roots {
    let apps = dir.0.join("share/applications");
    std::fs::create_dir_all(&apps).unwrap();
    std::fs::write(
        apps.join("google-chrome.desktop"),
        "[Desktop Entry]\nName=Google Chrome\nExec=/fake/chrome %U\n\
         MimeType=x-scheme-handler/https;\n",
    )
    .unwrap();
    let state = dir.0.join("bconfig/google-chrome");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("Local State"),
        r#"{"profile":{"info_cache":{"Profile 2":{"name":"Work"}}}}"#,
    )
    .unwrap();
    empty_roots(dir)
}

/// A Local-machine env: config file with `text`, no Outpost socket.
fn local_env(dir: &TmpDir, text: &str) -> DoctorEnv<Vec<u8>> {
    let mut e = env(dir, None);
    let f = dir.0.join("cfg/rowser/config.toml");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, text).unwrap();
    e.browser_roots = chrome_roots(dir);
    e
}

#[tokio::test]
async fn local_good_config_passes_and_a_stopped_daemon_only_warns() {
    let dir = TmpDir::new("dl");
    let mut e = local_env(
        &dir,
        "[[route]]\nhost = \"g14\"\nopen = \"chrome:Work\"\n[default]\nopen = \"system\"\n",
    );
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let f = dir.0.join("cfg/rowser/config.toml");
    let out = lines(e);
    assert_eq!(
        out,
        vec![
            format!("ok   config: {} parses; 1 route", f.display()),
            "ok   routes: every browser Target is found".to_string(),
            "warn daemon: the daemon is not running; start it with `rowser daemon`".to_string(),
        ],
        "no Remote checks without an Outpost socket or SSH"
    );
}

#[tokio::test]
async fn local_route_to_a_missing_browser_or_profile_warns() {
    let dir = TmpDir::new("dm");
    let mut e = local_env(
        &dir,
        "[[route]]\nhost = \"a\"\nopen = \"firefox\"\n\
         [[route]]\nhost = \"b\"\nopen = \"chrome:Nope\"\n\
         [default]\nopen = \"chrome:Work\"\n",
    );
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    let warns: Vec<&String> = out
        .iter()
        .filter(|l| l.starts_with("warn routes: "))
        .collect();
    assert_eq!(warns.len(), 2, "{out:?}");
    assert!(
        warns[0].starts_with("warn routes: route 1 (host a): firefox: "),
        "{out:?}"
    );
    assert!(
        warns[1].starts_with("warn routes: route 2 (host b): chrome:Nope: "),
        "{out:?}"
    );
}

#[tokio::test]
async fn local_missing_fallback_targets_say_what_they_fall_back_to() {
    let dir = TmpDir::new("df");
    let mut e = local_env(&dir, "[default]\nopen = \"chrome:Nope\"\n");
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    let warns: Vec<&String> = out
        .iter()
        .filter(|l| l.starts_with("warn routes: "))
        .collect();
    assert_eq!(
        warns,
        vec![
            "warn routes: [default]: chrome:Nope: browser `chrome` has no profile `Nope`; \
             the request falls back to `system`",
        ],
        "{out:?}"
    );
}

#[tokio::test]
async fn local_unverified_entry_warns_once_that_it_is_ignored() {
    let dir = TmpDir::new("du");
    let mut e = local_env(&dir, "[unverified]\nopen = \"firefox\"\n");
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    let warns: Vec<&String> = out.iter().filter(|l| l.contains("unverified")).collect();
    assert_eq!(
        warns,
        vec![
            "warn config: the [unverified] entry is removed and ignored: every Open request \
             now comes from a Saved host; remove [unverified] from the config file"
        ],
        "{out:?}"
    );
    assert!(!out.iter().any(|l| l.contains("firefox")), "{out:?}");
}

#[tokio::test]
async fn local_invalid_config_fails_with_the_file_and_line() {
    let dir = TmpDir::new("di");
    let mut e = local_env(&dir, "[[route]]\nhost = \"a\"\nopen = \"chrome:\"\n");
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let f = dir.0.join("cfg/rowser/config.toml");
    let out = lines(e);
    assert!(
        out[0].starts_with(&format!("fail config: {}: line 3: ", f.display())),
        "{out:?}"
    );
    assert!(!out.iter().any(|l| l.contains("routes:")), "{out:?}");
}

#[tokio::test]
async fn local_daemon_answers_with_its_version() {
    let dir = TmpDir::new("dd");
    let run = dir.0.join("rt");
    let control = bind_sockets(&run).unwrap();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        RecordingOpener::default(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(control));
    // No config file: the runtime directory alone makes this the Local machine.
    let mut e = env(&dir, None);
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(
        line(&out, "daemon"),
        format!(
            "ok   daemon: rowser {} answers on {} (protocol 2.0)",
            env!("CARGO_PKG_VERSION"),
            run.display()
        )
    );
    assert!(line(&out, "config").starts_with("ok   config: no file at "));
    assert!(!out.iter().any(|l| l.contains("tunnel")), "{out:?}");
    assert!(!out.iter().any(|l| l.contains("ok   link:")), "{out:?}");
}

#[tokio::test]
async fn ssh_runs_the_remote_checks_too() {
    let dir = TmpDir::new("ds");
    let mut e = local_env(&dir, "");
    e.ssh = true;
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert!(line(&out, "config").starts_with("ok   "), "{out:?}");
    assert!(line(&out, "link").starts_with("fail "), "{out:?}");
}

fn lines(env: DoctorEnv<Vec<u8>>) -> Vec<String> {
    String::from_utf8(env.stdout)
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn round_trip_against_a_daemon_is_ok_and_exits_0() {
    let dir = TmpDir::new("doc");
    let run = dir.0.join("run");
    let control = bind_sockets(&run).unwrap();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        RecordingOpener::default(),
        RecordingNotifier::default(),
    );
    let links = vec![test_link(&run, "devbox")];
    tokio::spawn(daemon.serve_with_test_links(control, links));
    let socket = run.join(TEST_LINK);
    let mut e = env(&dir, Some(socket.clone()));
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(out.len(), 9, "{out:?}");
    assert!(out.iter().all(|l| l.starts_with("ok ")), "{out:?}");
    assert_eq!(
        line(&out, "link"),
        format!(
            "ok   link: {}: daemon rowser {} (protocol 2.0)",
            socket.display(),
            env!("CARGO_PKG_VERSION")
        )
    );
    assert!(!out.iter().any(|l| l.contains("tunnel")), "{out:?}");
    assert_eq!(
        line(&out, "version"),
        "ok   version: daemon and this rowser speak 2.0"
    );
}

#[tokio::test]
async fn a_missing_default_browser_setup_warns_with_the_fix() {
    let dir = TmpDir::new("doc");
    let sock = dir.0.join("ok.sock");
    fake_daemon(sock.clone(), Version::new(1, 1));
    let mut e = env(&dir, Some(sock));
    let home = e.home.clone().unwrap();
    std::fs::remove_dir_all(home.join(".config")).unwrap();
    std::fs::remove_dir_all(home.join(".local/share")).unwrap();
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    for name in ["environment.d", "rowser.desktop", "default browser"] {
        let found: Vec<&String> = out
            .iter()
            .filter(|l| l.starts_with(&format!("warn {name}: ")))
            .collect();
        assert!(!found.is_empty(), "{name}: {out:?}");
        assert!(found.iter().all(|l| l.ends_with(FIX)), "{out:?}");
    }
    // One warning for each URL type.
    assert_eq!(
        out.iter()
            .filter(|l| l.starts_with("warn default browser: "))
            .count(),
        2,
        "{out:?}"
    );
}

/// The line for check `name`.
fn line<'a>(out: &'a [String], name: &str) -> &'a str {
    out.iter()
        .find(|l| l[5..].starts_with(&format!("{name}: ")))
        .unwrap_or_else(|| panic!("no {name} line: {out:?}"))
}

#[tokio::test]
async fn no_home_warns_and_skips_the_home_checks() {
    let dir = TmpDir::new("doc");
    let sock = dir.0.join("ok.sock");
    fake_daemon(sock.clone(), Version::new(1, 1));
    let mut e = env(&dir, Some(sock));
    e.home = None;
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(
        line(&out, "PATH"),
        "warn PATH: HOME is not set; cannot check ~/.local/bin"
    );
    assert_eq!(
        line(&out, "default browser"),
        "warn default browser: HOME is not set; cannot check the default browser"
    );
    assert!(!out.iter().any(|l| l.contains("environment.d")), "{out:?}");
}

#[tokio::test]
async fn a_graphical_session_without_a_real_xdg_open_warns() {
    let dir = TmpDir::new("doc");
    let sock = dir.0.join("ok.sock");
    fake_daemon(sock.clone(), Version::new(1, 1));
    let mut e = env(&dir, Some(sock));
    e.graphical = true;
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(
        line(&out, "fallback"),
        "warn fallback: graphical session but no real xdg-open on PATH; \
         fix: install the `xdg-utils` package (e.g. `sudo apt install xdg-utils`)"
    );
}

#[tokio::test]
async fn no_link_fails_clearly_and_exits_1() {
    let dir = TmpDir::new("doc");
    let mut e = env(&dir, Some(dir.0.join("missing.sock")));
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    assert_eq!(
        lines(e).last().unwrap(),
        "fail link: no Link to your Local machine; run \"rowser host add <host>\" there, \
         or check \"rowser host list\""
    );
}

/// A fake Daemon that answers `Ping` with `Pong { version }`.
fn fake_daemon(path: PathBuf, version: Version) {
    let listener = UnixListener::bind(path).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let Ok((mut conn, _)) = Conn::accept(stream).await else {
                    return;
                };
                if let Ok(Some(Msg::Ping)) = conn.recv().await {
                    let _ = conn
                        .send(&Msg::Pong {
                            version,
                            rowser_version: None,
                        })
                        .await;
                }
            });
        }
    });
}

#[tokio::test]
async fn minor_version_skew_warns_and_exits_0() {
    let dir = TmpDir::new("doc");
    let sock = dir.0.join("skew.sock");
    fake_daemon(sock.clone(), Version::new(2, 7));
    let mut e = env(&dir, Some(sock));
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    assert_eq!(
        lines(e).last().unwrap(),
        "warn version: version skew: daemon speaks 2.7, this rowser speaks 2.0; \
         update with `rowser remote install <host>` or reinstall rowser locally"
    );
}

#[tokio::test]
async fn major_version_mismatch_fails_and_exits_1() {
    let dir = TmpDir::new("doc");
    let sock = dir.0.join("old.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = Conn::connect(stream, Hello::with_version(Version::new(99, 0))).await;
        }
    });
    let mut e = env(&dir, Some(sock));
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert!(
        out[out.len() - 1].starts_with(
            "fail version: protocol version mismatch: this rowser speaks 2.0, the peer speaks 99.0"
        ),
        "{out:?}"
    );
    assert!(!out.iter().any(|l| l.starts_with("fail link")), "{out:?}");
}

#[tokio::test]
async fn the_no_default_browser_marker_skips_the_default_browser_checks() {
    let dir = TmpDir::new("dn");
    let sock = dir.0.join("ok.sock");
    fake_daemon(sock.clone(), Version::new(2, 0));
    let mut e = env(&dir, Some(sock));
    let home = e.home.clone().unwrap();
    // A host added with --no-default-browser: no symlinks, no setup files.
    std::fs::remove_dir_all(home.join(".config")).unwrap();
    std::fs::remove_dir_all(home.join(".local/share")).unwrap();
    for n in rowser::platform::BROWSER_NAMES {
        std::fs::remove_file(home.join(".local/bin").join(n)).unwrap();
    }
    e.browser = None;
    let marker = home.join(rowser::remote_install::NO_DEFAULT_BROWSER_MARKER);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "").unwrap();
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert!(!out.iter().any(|l| l.starts_with("warn")), "{out:?}");
    for name in [
        "PATH",
        "symlinks",
        "BROWSER",
        "environment.d",
        "rowser.desktop",
    ] {
        assert!(
            !out.iter().any(|l| l[5..].starts_with(&format!("{name}: "))),
            "{name}: {out:?}"
        );
    }
    assert_eq!(
        line(&out, "default browser"),
        format!(
            "ok   default browser: not checked: {} says rowser was installed \
             with --no-default-browser",
            marker.display()
        )
    );
    assert!(line(&out, "link").starts_with("ok   "), "{out:?}");
}

/// An Outpost socket folder `dir/o` with `dir_mode`, and a fake Daemon on
/// its `outpost.sock` with `sock_mode`.
fn outpost_socket(dir: &TmpDir, dir_mode: u32, sock_mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let o = dir.0.join("o");
    std::fs::create_dir_all(&o).unwrap();
    std::fs::set_permissions(&o, std::fs::Permissions::from_mode(dir_mode)).unwrap();
    let sock = o.join("outpost.sock");
    fake_daemon(sock.clone(), Version::new(2, 0));
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(sock_mode)).unwrap();
    sock
}

#[tokio::test]
async fn an_outpost_socket_of_this_user_with_private_modes_is_ok() {
    let dir = TmpDir::new("do");
    let sock = outpost_socket(&dir, 0o700, 0o600);
    let mut e = env(&dir, None);
    e.outpost_socket = Some(sock.clone());
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(
        line(&out, "outpost socket"),
        format!(
            "ok   outpost socket: {} belongs to you, mode 0600, in a folder with mode 0700",
            sock.display()
        )
    );
    assert!(line(&out, "link").starts_with("ok   "), "{out:?}");
}

#[tokio::test]
async fn an_outpost_socket_in_a_folder_others_can_read_fails() {
    let dir = TmpDir::new("dO");
    let sock = outpost_socket(&dir, 0o755, 0o600);
    let mut e = env(&dir, None);
    e.outpost_socket = Some(sock.clone());
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert_eq!(
        line(&out, "outpost socket"),
        format!(
            "fail outpost socket: {} has mode 755; it must be 0700; the Shim does not use \
             this socket; fix: chmod 700 {}",
            sock.parent().unwrap().display(),
            sock.parent().unwrap().display()
        )
    );
}

#[tokio::test]
async fn an_outpost_socket_that_others_can_use_fails() {
    let dir = TmpDir::new("dp");
    let sock = outpost_socket(&dir, 0o700, 0o666);
    let mut e = env(&dir, None);
    e.outpost_socket = Some(sock.clone());
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert_eq!(
        line(&out, "outpost socket"),
        format!(
            "fail outpost socket: {} has mode 0666, not 0600",
            sock.display()
        )
    );
}

#[tokio::test]
async fn a_bad_outpost_folder_fails_the_link_check_with_the_same_cause() {
    let dir = TmpDir::new("dL");
    let sock = outpost_socket(&dir, 0o755, 0o600);
    let mut e = env(&dir, None);
    e.outpost_socket = Some(sock.clone());
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert_eq!(
        line(&out, "link"),
        format!(
            "fail link: {} has mode 755; it must be 0700; the Shim does not use {}",
            sock.parent().unwrap().display(),
            sock.display()
        )
    );
}

#[tokio::test]
async fn an_outpost_folder_without_a_socket_is_still_checked() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TmpDir::new("dF");
    let o = dir.0.join("o");
    std::fs::create_dir_all(&o).unwrap();
    std::fs::set_permissions(&o, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut e = env(&dir, None);
    e.outpost_socket = Some(o.join("outpost.sock"));
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert_eq!(
        line(&out, "outpost socket"),
        format!(
            "fail outpost socket: {} has mode 755; it must be 0700; fix: chmod 700 {}",
            o.display(),
            o.display()
        )
    );
}

#[tokio::test]
async fn rowser_socket_still_checks_the_outpost_socket() {
    let dir = TmpDir::new("dr");
    let sock = dir.0.join("ok.sock");
    fake_daemon(sock.clone(), Version::new(2, 0));
    let mut e = env(&dir, Some(sock));
    // An Outpost socket folder others can read: the Shim uses ROWSER_SOCKET.
    e.outpost_socket = Some(outpost_socket(&dir, 0o755, 0o666));
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert!(line(&out, "outpost socket").starts_with("fail "), "{out:?}");
    assert!(line(&out, "link").starts_with("ok   "), "{out:?}");
}

#[tokio::test]
async fn an_installed_service_with_confirm_unverified_warns() {
    let dir = TmpDir::new("dv");
    let mut e = local_env(&dir, "");
    let unit = dir.0.join("svc/rowser.service");
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(
        &unit,
        "[Service]\nExecStart=\"/usr/bin/rowser\" daemon \"--confirm-unverified\"\n",
    )
    .unwrap();
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(
        line(&out, "service"),
        format!(
            "warn service: {} still has --confirm-unverified, which is removed; \
             the daemon does not start with it; run `rowser daemon install` again",
            unit.display()
        )
    );
}

#[tokio::test]
async fn an_installed_service_without_confirm_unverified_is_ok() {
    let dir = TmpDir::new("dV");
    let mut e = local_env(&dir, "");
    let unit = dir.0.join("svc/rowser.service");
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(&unit, "[Service]\nExecStart=\"/usr/bin/rowser\" daemon\n").unwrap();
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(
        line(&out, "service"),
        format!("ok   service: {}", unit.display())
    );
    // No service file: no line.
    let dir = TmpDir::new("dW");
    let mut e = local_env(&dir, "");
    let _ = run_doctor(&mut e).await;
    assert!(!lines(e).iter().any(|l| l.contains("service:")));
}

/// A fake Daemon on the control socket in `run` that answers `Status` with
/// `links`.
fn fake_control(run: &std::path::Path, links: Vec<LinkInfo>) {
    std::fs::create_dir_all(run).unwrap();
    let listener = UnixListener::bind(run.join(CONTROL_SOCKET)).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let links = links.clone();
            tokio::spawn(async move {
                let Ok((mut conn, _)) = Conn::accept(stream).await else {
                    return;
                };
                if let Ok(Some(Msg::Status)) = conn.recv().await {
                    let _ = conn
                        .send(&Msg::StatusReply {
                            daemon_version: "9.9.9".into(),
                            protocol_version: Version::new(2, 0),
                            config_file: None,
                            config_file_exists: false,
                            paused: false,
                            paused_until: None,
                            forwards: vec![],
                            links,
                        })
                        .await;
                }
            });
        }
    });
}

fn link(host: &str, state: LinkState) -> LinkInfo {
    LinkInfo {
        host: host.into(),
        state,
        since: 1,
        last_connected: None,
        outpost_version: None,
        outpost_rowser_version: None,
        target: None,
    }
}

/// A fake `ssh` in `dir/fakebin` that never reaches a host: it logs its
/// arguments to `dir/ssh-args`, fails (exit 255) for the host `bad`, and
/// does not answer for the host `slow`.
fn fake_ssh(dir: &TmpDir) -> std::ffi::OsString {
    let bin = dir.0.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    script::write_script(
        &bin.join("ssh"),
        &format!(
            "printf '%s\\n' \"$*\" >> {}\n\
             case \"$5\" in bad) echo 'bad: Permission denied (publickey).' >&2; exit 255;;\n\
             slow) exec sleep 30;; esac\n\
             exit 0",
            dir.0.join("ssh-args").display()
        ),
    );
    bin.into_os_string()
}

const HOSTS: &str = "[[host]]\nname = \"bad\"\n[[host]]\nname = \"g14\"\n\
                     [[host]]\nname = \"new\"\n[[host]]\nname = \"old\"\n\
                     [[host]]\nname = \"pi\"\n";

#[tokio::test]
async fn local_doctor_reports_each_saved_hosts_link_and_ssh_login() {
    let dir = TmpDir::new("dh");
    fake_control(
        &dir.0.join("rt"),
        vec![
            link("bad", LinkState::NeedsLogin),
            link("g14", LinkState::Connected),
            link("old", LinkState::VersionMismatch),
            link("pi", LinkState::Connecting),
        ],
    );
    let mut e = local_env(&dir, HOSTS);
    e.path = Some(fake_ssh(&dir));
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    let hosts: Vec<&String> = out.iter().filter(|l| l[5..].starts_with("host ")).collect();
    assert_eq!(
        hosts,
        vec![
            "warn host bad: the Link needs login; run `rowser host connect bad`",
            "fail host bad: `ssh -o BatchMode=yes bad true` failed (exit status: 255): \
             bad: Permission denied (publickey).",
            "ok   host g14: the Link is connected",
            "ok   host g14: `ssh -o BatchMode=yes g14 true` succeeds",
            "warn host new: the daemon has no Link to new; run `rowser reload`",
            "ok   host new: `ssh -o BatchMode=yes new true` succeeds",
            "warn host old: the Link has a version mismatch; run `rowser remote install old`",
            "ok   host old: `ssh -o BatchMode=yes old true` succeeds",
            "warn host pi: the Link is connecting",
            "ok   host pi: `ssh -o BatchMode=yes pi true` succeeds",
        ],
        "{out:?}"
    );
    // The hosts run at the same time, so the log order can change.
    let args = std::fs::read_to_string(dir.0.join("ssh-args")).unwrap();
    let mut args: Vec<&str> = args.lines().collect();
    args.sort();
    assert_eq!(
        args,
        ["bad", "g14", "new", "old", "pi"]
            .map(|h| format!("-o BatchMode=yes -o ConnectTimeout=10 {h} true"))
    );
}

#[tokio::test]
async fn local_doctor_without_a_daemon_says_each_host_has_no_link() {
    let dir = TmpDir::new("dH");
    let mut e = local_env(&dir, "[[host]]\nname = \"g14\"\n");
    e.path = Some(fake_ssh(&dir));
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    let hosts: Vec<&String> = out.iter().filter(|l| l[5..].starts_with("host ")).collect();
    assert_eq!(
        hosts,
        vec![
            "warn host g14: no Link; the daemon is not running",
            "ok   host g14: `ssh -o BatchMode=yes g14 true` succeeds",
        ],
        "{out:?}"
    );
}

#[tokio::test]
async fn an_ssh_login_that_does_not_answer_fails_after_the_time_limit() {
    let dir = TmpDir::new("dT");
    let mut e = local_env(&dir, "[[host]]\nname = \"slow\"\n");
    e.path = Some(fake_ssh(&dir));
    let start = std::time::Instant::now();
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    let out = lines(e);
    assert!(
        out.contains(
            &"fail host slow: `ssh -o BatchMode=yes slow true` failed: no answer in 1s".to_string()
        ),
        "{out:?}"
    );
}

#[tokio::test]
async fn local_doctor_without_ssh_on_path_fails_for_each_saved_host() {
    let dir = TmpDir::new("dS");
    let mut e = local_env(&dir, "[[host]]\nname = \"g14\"\n");
    e.path = Some(dir.0.join("empty-bin").into_os_string());
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert!(
        out.contains(
            &"fail host g14: cannot run `ssh -o BatchMode=yes g14 true`: ssh is not on PATH"
                .to_string()
        ),
        "{out:?}"
    );
}

#[tokio::test]
async fn warnings_alone_do_not_fail() {
    let dir = TmpDir::new("doc");
    let sock = dir.0.join("ok.sock");
    fake_daemon(sock.clone(), Version::new(1, 0));
    let mut e = env(&dir, Some(sock));
    e.browser = None;
    // Not /usr/bin: the real xdg-mime there reads the user's own mimeapps.list.
    e.path = Some(dir.0.join("empty-bin").into_os_string());
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert!(line(&out, "PATH").starts_with("warn PATH: "), "{out:?}");
    assert_eq!(line(&out, "BROWSER"), "warn BROWSER: BROWSER is not set");
}
