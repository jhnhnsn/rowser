mod common;

use std::path::PathBuf;
use std::process::ExitCode;

use common::TmpDir;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET};
use rowser::doctor::{run_doctor, DoctorEnv};
use rowser::protocol::{Conn, Hello, Msg, Version};
use tokio::net::UnixListener;

/// A home whose PATH and BROWSER checks pass.
fn good_home(dir: &TmpDir) -> PathBuf {
    let home = dir.0.join("home");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("rowser"), "").unwrap();
    for n in rowser::cli::BROWSER_NAMES {
        std::os::unix::fs::symlink("rowser", bin.join(n)).unwrap();
    }
    home
}

fn env(dir: &TmpDir, over: Option<PathBuf>) -> DoctorEnv<Vec<u8>> {
    let home = good_home(dir);
    let bin = home.join(".local/bin");
    DoctorEnv {
        path: Some(format!("{}:/usr/bin", bin.display()).into()),
        browser: Some("xdg-open".into()),
        home: Some(home),
        tunnel_dir: dir.0.join("tunnels"),
        socket_override: over,
        stdout: Vec::new(),
    }
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
    let (direct, relay) = bind_sockets(&run).unwrap();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        RecordingOpener::default(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(direct, relay));
    let socket = run.join(DIRECT_SOCKET);
    let mut e = env(&dir, Some(socket.clone()));
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert_eq!(out.len(), 5, "{out:?}");
    assert!(out[..3].iter().all(|l| l.starts_with("ok ")), "{out:?}");
    assert_eq!(
        out[3],
        format!("ok   tunnel: {}: daemon protocol 1.0", socket.display())
    );
    assert_eq!(out[4], "ok   version: daemon and this rowser speak 1.0");
}

#[tokio::test]
async fn no_tunnel_fails_clearly_and_exits_1() {
    let dir = TmpDir::new("doc");
    let mut e = env(&dir, Some(dir.0.join("missing.sock")));
    assert_eq!(run_doctor(&mut e).await, ExitCode::FAILURE);
    assert_eq!(
        lines(e).last().unwrap(),
        "fail tunnel: no tunnel; connect with `rowser ssh <host>` or set ROWSER_SOCKET"
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
                    let _ = conn.send(&Msg::Pong { version }).await;
                }
            });
        }
    });
}

#[tokio::test]
async fn minor_version_skew_warns_and_exits_0() {
    let dir = TmpDir::new("doc");
    let sock = dir.0.join("skew.sock");
    fake_daemon(sock.clone(), Version::new(1, 7));
    let mut e = env(&dir, Some(sock));
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    assert_eq!(
        lines(e).last().unwrap(),
        "warn version: version skew: daemon speaks 1.7, this rowser speaks 1.0; \
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
        out[out.len() - 2].starts_with(
            "fail version: protocol version mismatch: this rowser speaks 1.0, the peer speaks 99.0"
        ),
        "{out:?}"
    );
    assert!(
        out[out.len() - 1].starts_with("fail tunnel: no tunnel"),
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
    e.path = Some("/usr/bin".into());
    assert_eq!(run_doctor(&mut e).await, ExitCode::SUCCESS);
    let out = lines(e);
    assert!(out[0].starts_with("warn PATH: "), "{out:?}");
    assert_eq!(out[2], "warn BROWSER: BROWSER is not set");
}
