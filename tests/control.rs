//! The control socket and `rowser status`.

mod common;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::process::ExitCode;

use common::TmpDir;
use rowser::control::{run_status, StatusArgs};
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET, RELAY_SOCKET};
use rowser::protocol::{Conn, Hello, Msg};
use tokio::net::{TcpListener, UnixStream};

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

async fn start(dir: &TmpDir) -> std::path::PathBuf {
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let config = DaemonConfig {
        loopback: vec![V6],
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(
        config,
        RecordingOpener::default(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(direct, relay, control));
    run
}

async fn connect(path: std::path::PathBuf) -> Conn<UnixStream> {
    let s = UnixStream::connect(path).await.unwrap();
    Conn::connect(s, Hello::current()).await.unwrap()
}

/// Runs `rowser status` on a blocking thread; gives (exit, stdout, stderr).
async fn status(run: std::path::PathBuf, json: bool) -> (ExitCode, String, String) {
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_status(
            StatusArgs {
                runtime_dir: Ok(run),
                json,
            },
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_shows_the_daemon_version_and_an_open_callback_forward() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;
    let port = TcpListener::bind(SocketAddr::new(V6, 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut relay = connect(run.join(RELAY_SOCKET)).await;
    relay
        .send(&Msg::HostStamp {
            alias: "g14".into(),
        })
        .await
        .unwrap();
    relay
        .send(&Msg::Forward {
            reported_host: "devbox".into(),
            callback_ports: vec![port],
        })
        .await
        .unwrap();
    assert!(matches!(
        relay.recv().await.unwrap(),
        Some(Msg::OpenResult { error: None, .. })
    ));

    let (code, out, err) = status(run.clone(), false).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[..5],
        [
            format!("daemon version:    {}", env!("CARGO_PKG_VERSION")),
            "protocol version:  1.1".to_string(),
            format!("runtime directory: {}", run.display()),
            "config file:       none".to_string(),
            "paused:            no".to_string(),
        ]
    );
    assert_eq!(lines[5], "callback forwards:");
    assert!(
        lines[6].starts_with(&format!("  {port}  g14  ")),
        "{}",
        lines[6]
    );

    let (code, out, _) = status(run, true).await;
    assert_eq!(code, ExitCode::SUCCESS);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["daemon_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["protocol_version"], "1.1");
    assert_eq!(v["config_file"], serde_json::Value::Null);
    assert_eq!(v["paused"], false);
    assert_eq!(v["forwards"][0]["port"], port);
    assert_eq!(v["forwards"][0]["host"], "g14");
    assert_eq!(v["forwards"][0]["unverified"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_with_no_forwards_says_none() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;
    let (code, out, _) = status(run, false).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.ends_with("callback forwards: none\n"), "{out}");
}

#[tokio::test]
async fn control_messages_are_refused_on_the_direct_and_relay_sockets() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;

    let mut direct = connect(run.join(DIRECT_SOCKET)).await;
    direct.send(&Msg::Status).await.unwrap();
    match direct.recv().await.unwrap() {
        Some(Msg::OpenResult { error: Some(e), .. }) => {
            assert!(e.contains("control socket"), "{e}")
        }
        other => panic!("unexpected {other:?}"),
    }

    let mut relay = connect(run.join(RELAY_SOCKET)).await;
    relay
        .send(&Msg::HostStamp {
            alias: "g14".into(),
        })
        .await
        .unwrap();
    relay.send(&Msg::Status).await.unwrap();
    match relay.recv().await.unwrap() {
        Some(Msg::OpenResult { error: Some(e), .. }) => {
            assert!(e.contains("control socket"), "{e}")
        }
        other => panic!("unexpected {other:?}"),
    }

    // The Daemon keeps serving.
    let mut again = connect(run.join(DIRECT_SOCKET)).await;
    again.send(&Msg::Ping).await.unwrap();
    assert!(matches!(
        again.recv().await.unwrap(),
        Some(Msg::Pong { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn status_with_no_daemon_says_not_running_and_exits_1() {
    let dir = TmpDir::new("ctl");
    let (code, out, err) = status(dir.0.join("run"), false).await;
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert_eq!(err, "rowser: the daemon is not running\n");
}

#[tokio::test]
async fn the_control_socket_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TmpDir::new("ctl");
    let run = dir.0.join("run");
    let _l = bind_sockets(&run).unwrap();
    let mode = std::fs::metadata(run.join(rowser::daemon::CONTROL_SOCKET))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}
