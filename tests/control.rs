//! The control socket and `rowser status`.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use common::TmpDir;
use rowser::control::{run_status, StatusArgs};
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET, RELAY_SOCKET};
use rowser::protocol::{Conn, Hello, Msg, Version, CONTROL_SOCKET};
use rowser::routes::Routes;
use tokio::net::{TcpListener, UnixStream};

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);
const EXE: &str = env!("CARGO_BIN_EXE_rowser");

async fn start(dir: &TmpDir) -> PathBuf {
    start_with(dir, DaemonConfig::default()).await
}

async fn start_with(dir: &TmpDir, config: DaemonConfig) -> PathBuf {
    let run = dir.0.join("run");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let config = DaemonConfig {
        loopback: vec![V6],
        ..config
    };
    let daemon = Daemon::new(
        config,
        RecordingOpener::default(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(direct, relay, control));
    run
}

async fn connect(path: PathBuf) -> Conn<UnixStream> {
    let s = UnixStream::connect(path).await.unwrap();
    Conn::connect(s, Hello::current()).await.unwrap()
}

async fn free_port() -> u16 {
    TcpListener::bind(SocketAddr::new(V6, 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Runs `rowser status` on a blocking thread; gives (exit, stdout, stderr).
async fn status(run: PathBuf, json: bool) -> (ExitCode, String, String) {
    tokio::task::spawn_blocking(move || status_blocking(run, json))
        .await
        .unwrap()
}

fn status_blocking(run: PathBuf, json: bool) -> (ExitCode, String, String) {
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
}

async fn json_status(run: PathBuf) -> serde_json::Value {
    let (code, out, err) = status(run, true).await;
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    serde_json::from_str(out.trim()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_shows_the_daemon_version_and_an_open_callback_forward() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;
    let port = free_port().await;
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
    assert_eq!(lines[5], "sessions:          none");
    assert_eq!(lines[6], "callback forwards:");
    assert!(
        lines[7].starts_with(&format!("  {port}  g14  ")),
        "{}",
        lines[7]
    );

    let v = json_status(run.clone()).await;
    assert_eq!(v["daemon_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["protocol_version"], "1.1");
    assert_eq!(v["runtime_dir"], run.display().to_string());
    assert_eq!(v["config_file"], serde_json::Value::Null);
    assert_eq!(v["config_file_exists"], false);
    assert_eq!(v["paused"], false);
    assert_eq!(v["forwards"][0]["port"], port);
    assert_eq!(v["forwards"][0]["host"], "g14");
    assert_eq!(v["forwards"][0]["unverified"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forward_from_the_direct_socket_shows_its_reported_host_as_unverified() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;
    let port = free_port().await;
    let mut direct = connect(run.join(DIRECT_SOCKET)).await;
    direct
        .send(&Msg::Forward {
            reported_host: "devbox".into(),
            callback_ports: vec![port],
        })
        .await
        .unwrap();
    assert!(matches!(
        direct.recv().await.unwrap(),
        Some(Msg::OpenResult { error: None, .. })
    ));

    let (_, out, _) = status(run.clone(), false).await;
    let line = out.lines().nth(7).unwrap_or_default().to_string();
    assert!(
        line.starts_with(&format!("  {port}  devbox (unverified)  ")),
        "{out}"
    );

    let v = json_status(run).await;
    assert_eq!(v["forwards"][0]["host"], "devbox");
    assert_eq!(v["forwards"][0]["unverified"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forward_leaves_status_when_it_ends() {
    let dir = TmpDir::new("ctl");
    let config = DaemonConfig {
        forward_timeout: Duration::from_millis(300),
        ..DaemonConfig::default()
    };
    let run = start_with(&dir, config).await;
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
            callback_ports: vec![free_port().await],
        })
        .await
        .unwrap();
    relay.recv().await.unwrap();
    assert_eq!(
        json_status(run.clone()).await["forwards"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // No keeper attaches, so the forward ends at its timeout.
    for _ in 0..50 {
        if json_status(run.clone()).await["forwards"] == serde_json::json!([]) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the forward is still in status");
}

#[tokio::test(flavor = "multi_thread")]
async fn status_shows_the_config_file_path_and_whether_it_exists() {
    let dir = TmpDir::new("ctl");
    let path = dir.0.join("conf").join("rowser").join("config.toml");
    let config = DaemonConfig {
        config_file: Some(path.clone()),
        ..DaemonConfig::default()
    };
    let run = start_with(&dir, config).await;
    let (_, out, _) = status(run.clone(), false).await;
    assert!(
        out.contains(&format!(
            "config file:       {} (no file)\n",
            path.display()
        )),
        "{out}"
    );
    let v = json_status(run).await;
    assert_eq!(v["config_file"], path.display().to_string());
    assert_eq!(v["config_file_exists"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_protocol_1_0_shim_still_opens_on_the_1_1_daemon() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;
    let s = UnixStream::connect(run.join(DIRECT_SOCKET)).await.unwrap();
    let mut conn = Conn::connect(s, Hello::with_version(Version::new(1, 0)))
        .await
        .unwrap();
    conn.send(&Msg::Open {
        url: "https://example.com/".into(),
        reported_host: "devbox".into(),
        callback_ports: vec![],
    })
    .await
    .unwrap();
    assert!(matches!(
        conn.recv().await.unwrap(),
        Some(Msg::OpenResult { error: None, .. })
    ));
}

/// A relay connection that registered a Session for `alias`.
async fn session(run: &Path, alias: &str) -> Conn<UnixStream> {
    let mut relay = connect(run.join(RELAY_SOCKET)).await;
    relay
        .send(&Msg::HostStamp {
            alias: alias.into(),
        })
        .await
        .unwrap();
    relay.send(&Msg::Session).await.unwrap();
    assert!(matches!(
        relay.recv().await.unwrap(),
        Some(Msg::OpenResult { error: None, .. })
    ));
    relay
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_shows_the_target_of_its_first_route_without_match() {
    let text = "[[route]]\nhost = \"g14\"\nmatch = [\"*.google.com\"]\nopen = \"chrome:Work\"\n\
                [[route]]\nhost = \"g1?\"\nopen = \"safari\"\n";
    let config = DaemonConfig {
        routes: Routes::from_config(&rowser::config::parse(text).unwrap()),
        ..DaemonConfig::default()
    };
    let dir = TmpDir::new("ctl");
    let run = start_with(&dir, config).await;
    let _a = session(&run, "g14").await;
    let _b = session(&run, "db1").await;

    let (_, out, _) = status(run.clone(), false).await;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[5], "sessions:");
    assert!(
        lines[6].starts_with("  g14  ") && lines[6].ends_with("  safari"),
        "{out}"
    );
    assert!(
        lines[7].starts_with("  db1  ") && lines[7].ends_with("  system"),
        "{out}"
    );

    let v = json_status(run).await;
    assert_eq!(v["sessions"][0]["host"], "g14");
    assert_eq!(v["sessions"][0]["target"], "safari");
    assert_eq!(v["sessions"][1]["target"], "system");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_ends_when_its_connection_closes() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;
    let conn = session(&run, "g14").await;
    assert_eq!(json_status(run.clone()).await["sessions"][0]["host"], "g14");
    drop(conn);
    for _ in 0..50 {
        if json_status(run.clone()).await["sessions"] == serde_json::json!([]) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the session is still in status");
}

#[tokio::test]
async fn a_session_is_refused_on_the_direct_socket() {
    let dir = TmpDir::new("ctl");
    let run = start(&dir).await;
    let mut direct = connect(run.join(DIRECT_SOCKET)).await;
    direct.send(&Msg::Session).await.unwrap();
    match direct.recv().await.unwrap() {
        Some(Msg::OpenResult { error: Some(e), .. }) => assert!(e.contains("relay socket"), "{e}"),
        other => panic!("unexpected {other:?}"),
    }
    let v = json_status(run).await;
    assert_eq!(v["sessions"], serde_json::json!([]));
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

#[test]
fn status_with_a_stale_control_socket_says_not_running() {
    let dir = TmpDir::new("ctl");
    let run = dir.0.join("run");
    std::fs::create_dir(&run).unwrap();
    // A socket file with no listener behind it.
    drop(std::os::unix::net::UnixListener::bind(run.join(CONTROL_SOCKET)).unwrap());
    let (code, out, err) = status_blocking(run, false);
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert_eq!(err, "rowser: the daemon is not running\n");
}

#[test]
fn status_reports_a_protocol_major_mismatch() {
    let dir = TmpDir::new("ctl");
    let run = dir.0.join("run");
    std::fs::create_dir(&run).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(run.join(CONTROL_SOCKET)).unwrap();
    let peer = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        (&stream)
            .write_all(b"{\"version\":{\"major\":2,\"minor\":0}}\n")
            .unwrap();
    });
    let (code, out, err) = status_blocking(run, false);
    peer.join().unwrap();
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert!(err.contains("protocol version mismatch"), "{err}");
    assert!(err.contains("2.0"), "{err}");
}

#[tokio::test]
async fn the_control_socket_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TmpDir::new("ctl");
    let run = dir.0.join("run");
    let _l = bind_sockets(&run).unwrap();
    let mode = std::fs::metadata(run.join(CONTROL_SOCKET))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// Starts `rowser daemon` with `XDG_CONFIG_HOME=conf` in `run`, runs
/// `rowser status --json` against it, and stops it.
fn binary_status(run: &Path, conf: &Path) -> serde_json::Value {
    let mut daemon = Command::new(EXE)
        .args(["daemon", "--open-command", "true {url}", "--runtime-dir"])
        .arg(run)
        .env("XDG_CONFIG_HOME", conf)
        .env_remove("ROWSER_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(daemon.stderr.take().unwrap()).lines();
    let line = lines.next().unwrap().unwrap();
    assert!(line.starts_with("rowser daemon: listening in "), "{line}");
    let out = Command::new(EXE)
        .args(["status", "--json"])
        .env("ROWSER_RUNTIME_DIR", run)
        .env("XDG_CONFIG_HOME", conf)
        .output()
        .unwrap();
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn rowser_status_names_the_config_file_the_daemon_read() {
    let dir = TmpDir::new("ctb");
    let conf = dir.0.join("conf");
    std::fs::create_dir_all(conf.join("rowser")).unwrap();
    let file = conf.join("rowser").join("config.toml");
    std::fs::write(&file, "[default]\nopen = \"system\"\n").unwrap();
    let v = binary_status(&dir.0.join("run"), &conf);
    assert_eq!(v["config_file"], file.display().to_string());
    assert_eq!(v["config_file_exists"], true);
}

#[test]
fn rowser_status_names_a_missing_config_file() {
    let dir = TmpDir::new("ctb");
    let conf = dir.0.join("conf");
    let v = binary_status(&dir.0.join("run"), &conf);
    assert_eq!(
        v["config_file"],
        conf.join("rowser")
            .join("config.toml")
            .display()
            .to_string()
    );
    assert_eq!(v["config_file_exists"], false);
}
