mod common;

use std::path::PathBuf;
use std::process::ExitCode;

use common::{test_link, TmpDir, TEST_LINK};
use rowser::browsers::{Os, Roots};
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
use rowser::doctor::{run_doctor, DoctorEnv, FIX};
use rowser::protocol::{Conn, Hello, Msg, Version};
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
        outpost_socket: Some(dir.0.join("no-outpost.sock")),
        socket_override: over,
        graphical: false,
        self_exe: Some(bin.join("rowser")),
        // Missing: no Local checks unless a test adds them.
        config_file: Ok(dir.0.join("cfg/rowser/config.toml")),
        runtime_dir: Ok(dir.0.join("rt")),
        browser_roots: empty_roots(dir),
        ssh: false,
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
}

#[tokio::test]
async fn ssh_runs_the_remote_checks_too() {
    let dir = TmpDir::new("ds");
    let mut e = local_env(&dir, "");
    e.ssh = true;
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    let out = lines(e);
    assert!(line(&out, "config").starts_with("ok   "), "{out:?}");
    assert!(line(&out, "tunnel").starts_with("fail "), "{out:?}");
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
        line(&out, "tunnel"),
        format!("ok   tunnel: {}: daemon protocol 2.0", socket.display())
    );
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
async fn no_tunnel_fails_clearly_and_exits_1() {
    let dir = TmpDir::new("doc");
    let mut e = env(&dir, Some(dir.0.join("missing.sock")));
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    assert_eq!(
        lines(e).last().unwrap(),
        "fail tunnel: no Link to your Local machine; run \"rowser host add <host>\" there, \
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
    assert!(!out.iter().any(|l| l.contains("no tunnel")), "{out:?}");
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
