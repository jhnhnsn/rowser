//! Callback forwards: Daemon side with a fake keeper.

mod common;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use common::TmpDir;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET};
use rowser::detect::Procfs;
use rowser::protocol::{Conn, Hello, Msg};
use rowser::shim::{run_forward, run_open, ShimEnv};
use std::process::ExitCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use url::Url;

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

/// A port that is free on [::1] now.
async fn free_port() -> u16 {
    let l = TcpListener::bind(SocketAddr::new(V6, 0)).await.unwrap();
    l.local_addr().unwrap().port()
}

struct Harness {
    dir: TmpDir,
    opener: RecordingOpener,
    notifier: RecordingNotifier,
}

impl Harness {
    async fn start(mut config: DaemonConfig) -> Harness {
        // These tests use the direct socket; `[unverified]` asks by default.
        config.routes = rowser::routes::Routes::from_config(
            &rowser::config::parse("[unverified]\nopen = \"system\"\n").unwrap(),
        );
        let dir = TmpDir::new("f");
        let (direct, relay, control) = bind_sockets(&dir.0.join("run")).unwrap();
        let opener = RecordingOpener::default();
        let notifier = RecordingNotifier::default();
        let daemon = Daemon::new(config, opener.clone(), notifier.clone());
        tokio::spawn(daemon.serve(direct, relay, control));
        Harness {
            dir,
            opener,
            notifier,
        }
    }

    async fn connect(&self) -> Conn<UnixStream> {
        let s = UnixStream::connect(self.dir.0.join("run").join(DIRECT_SOCKET))
            .await
            .unwrap();
        Conn::connect(s, Hello::current()).await.unwrap()
    }

    /// Sends an Open request with `ports` and returns the reply's fields.
    async fn open(&self, ports: Vec<u16>) -> (Option<String>, Vec<String>, Option<String>) {
        let mut c = self.connect().await;
        c.send(&Msg::Open {
            url: "https://example.com/auth".into(),
            reported_host: "devbox".into(),
            callback_ports: ports,
        })
        .await
        .unwrap();
        match c.recv().await.unwrap() {
            Some(Msg::OpenResult {
                forward_id,
                warnings,
                error,
            }) => (forward_id, warnings, error),
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn v6_config() -> DaemonConfig {
    DaemonConfig {
        loopback: vec![V6],
        ..DaemonConfig::default()
    }
}

#[tokio::test]
async fn a_browser_connection_waits_for_the_keeper_and_is_spliced() {
    let h = Harness::start(v6_config()).await;
    let port = free_port().await;
    let (id, warnings, error) = h.open(vec![port]).await;
    assert_eq!((warnings, error), (vec![], None));
    let id = id.expect("a forward id");
    assert_eq!(h.opener.urls().len(), 1);

    // The browser connects before the keeper attaches: it is queued.
    let mut browser = TcpStream::connect(SocketAddr::new(V6, port)).await.unwrap();
    browser.write_all(b"GET /cb").await.unwrap();

    let mut control = h.connect().await;
    control
        .send(&Msg::AttachForward {
            forward_id: id.clone(),
        })
        .await
        .unwrap();
    let conn_id = expect_dial(&mut control, port).await;

    let mut data = h.connect().await;
    data.send(&Msg::DataConn {
        forward_id: id,
        conn_id,
    })
    .await
    .unwrap();
    let (mut data, extra) = data.into_raw();
    assert!(extra.is_empty());
    let mut buf = [0u8; 7];
    data.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"GET /cb");
    data.write_all(b"200 OK").await.unwrap();
    let mut buf = [0u8; 6];
    browser.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"200 OK");
}

async fn expect_dial(control: &mut Conn<UnixStream>, port: u16) -> u64 {
    match control.recv().await.unwrap() {
        Some(Msg::Dial { conn_id, port: p }) => {
            assert_eq!(p, port);
            conn_id
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Sends a `DataConn` and expects the Daemon to refuse it.
async fn data_conn_refused(h: &Harness, forward_id: &str, conn_id: u64) {
    let mut data = h.connect().await;
    data.send(&Msg::DataConn {
        forward_id: forward_id.into(),
        conn_id,
    })
    .await
    .unwrap();
    match data.recv().await.unwrap() {
        Some(Msg::OpenResult { error: Some(_), .. }) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn a_data_conn_of_another_forward_or_a_wrong_id_gets_no_connection() {
    let h = Harness::start(v6_config()).await;
    let port = free_port().await;
    let other_port = free_port().await;
    let id = h.open(vec![port]).await.0.unwrap();
    let other = h.open(vec![other_port]).await.0.unwrap();
    let mut control = h.connect().await;
    control
        .send(&Msg::AttachForward {
            forward_id: id.clone(),
        })
        .await
        .unwrap();
    let mut browser = TcpStream::connect(SocketAddr::new(V6, port)).await.unwrap();
    let conn_id = expect_dial(&mut control, port).await;
    let mut browser2 = TcpStream::connect(SocketAddr::new(V6, port)).await.unwrap();
    let conn_id2 = expect_dial(&mut control, port).await;
    assert_ne!(conn_id2, conn_id.wrapping_add(1), "ids are not sequential");

    data_conn_refused(&h, &id, conn_id ^ 1).await;
    data_conn_refused(&h, &other, conn_id).await;
    data_conn_refused(&h, "", conn_id).await;

    // The refusals did not take the browser connections.
    for (cid, b) in [(conn_id, &mut browser), (conn_id2, &mut browser2)] {
        let mut data = h.connect().await;
        data.send(&Msg::DataConn {
            forward_id: id.clone(),
            conn_id: cid,
        })
        .await
        .unwrap();
        let (mut data, _) = data.into_raw();
        b.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        data.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi");
    }
}

#[tokio::test]
async fn an_unknown_forward_id_is_rejected() {
    let h = Harness::start(v6_config()).await;
    let mut control = h.connect().await;
    control
        .send(&Msg::AttachForward {
            forward_id: "nope".into(),
        })
        .await
        .unwrap();
    match control.recv().await.unwrap() {
        Some(Msg::OpenResult { error: Some(_), .. }) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn a_busy_local_port_still_opens_the_url_with_a_warning() {
    let h = Harness::start(v6_config()).await;
    let busy = TcpListener::bind(SocketAddr::new(V6, 0)).await.unwrap();
    let port = busy.local_addr().unwrap().port();
    let (id, warnings, error) = h.open(vec![port]).await;
    assert_eq!(error, None);
    assert_eq!(id, None, "no forward when every port is busy");
    assert_eq!(
        warnings,
        [format!(
            "local port {port} is busy; the callback cannot be forwarded"
        )]
    );
    assert_eq!(h.opener.urls().len(), 1);
    let notices = h.notifier.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].warnings, warnings);
    assert!(notices[0].body().contains("busy"), "{}", notices[0].body());
}

#[tokio::test]
async fn a_port_below_1024_is_not_forwarded() {
    let h = Harness::start(v6_config()).await;
    let (id, warnings, error) = h.open(vec![80]).await;
    assert_eq!((id, error), (None, None));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

/// Waits until nothing accepts on [::1]:`port`.
async fn until_closed(port: u16) {
    for _ in 0..100 {
        if TcpStream::connect(SocketAddr::new(V6, port)).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("port {port} still accepts");
}

#[tokio::test]
async fn the_forward_closes_when_the_keeper_disconnects() {
    let h = Harness::start(v6_config()).await;
    let port = free_port().await;
    let id = h.open(vec![port]).await.0.unwrap();
    let mut control = h.connect().await;
    control
        .send(&Msg::AttachForward { forward_id: id })
        .await
        .unwrap();
    drop(control);
    until_closed(port).await;
}

// A real 200 ms timeout, not `start_paused`: the Daemon and this test talk
// over real sockets, and a paused clock jumps ahead while the runtime waits
// on socket I/O, so the timeout could fire before the keeper attaches.
#[tokio::test]
async fn the_forward_closes_after_the_timeout() {
    let h = Harness::start(DaemonConfig {
        forward_timeout: Duration::from_millis(200),
        ..v6_config()
    })
    .await;
    let port = free_port().await;
    let id = h.open(vec![port]).await.0.unwrap();
    let mut control = h.connect().await;
    control
        .send(&Msg::AttachForward { forward_id: id })
        .await
        .unwrap();
    until_closed(port).await;
    // The probes of `until_closed` were dialed; then the connection closes.
    while let Some(msg) = control.recv().await.unwrap() {
        assert!(matches!(msg, Msg::Dial { .. }), "{msg:?}");
    }
}

fn shim_env(h: &Harness) -> ShimEnv<Vec<u8>> {
    ShimEnv {
        tunnel_dir: PathBuf::from("/nonexistent"),
        socket_override: Some(h.dir.0.join("run").join(DIRECT_SOCKET)),
        reported_host: "devbox".into(),
        procfs: Procfs::new("/nonexistent"),
        caller_pid: 1,
        keeper_exe: Some(env!("CARGO_BIN_EXE_rowser").into()),
        graphical: false,
        search_path: None,
        self_exe: None,
        in_fallback: false,
        stderr: Vec::new(),
    }
}

/// A fake remote HTTP server on 127.0.0.1 (the Daemon binds only [::1] here,
/// so both fit on one machine). Answers each request with its path.
async fn fake_remote_http() -> (u16, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = l.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let line = String::from_utf8_lossy(&req);
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let body = format!("remote got {path}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(resp.as_bytes()).await.unwrap();
            });
        }
    });
    (port, task)
}

/// One HTTP request to [::1]:`port`, as a browser makes it; returns the body.
async fn browser_get(port: u16, path: &str) -> String {
    let mut s = TcpStream::connect(SocketAddr::new(V6, port)).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_string(&mut resp))
        .await
        .expect("a response in time")
        .unwrap();
    resp.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}

#[tokio::test]
async fn an_oauth_callback_reaches_the_remote_listener_through_the_keeper() {
    let h = Harness::start(v6_config()).await;
    let (port, _remote) = fake_remote_http().await;
    let url = Url::parse(&format!(
        "https://auth.example.com/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Fcb"
    ))
    .unwrap();
    let mut env = shim_env(&h);
    let started = std::time::Instant::now();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::SUCCESS);
    // The Shim returns before any callback happens.
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(String::from_utf8(env.stderr).unwrap(), "");
    assert_eq!(h.opener.urls(), [url]);

    assert_eq!(
        browser_get(port, "/cb?code=1").await,
        "remote got /cb?code=1"
    );
    assert_eq!(
        browser_get(port, "/cb?code=2").await,
        "remote got /cb?code=2"
    );
}

#[tokio::test]
async fn the_shim_prints_the_busy_port_warning_and_exits_0() {
    let h = Harness::start(v6_config()).await;
    let busy = TcpListener::bind(SocketAddr::new(V6, 0)).await.unwrap();
    let port = busy.local_addr().unwrap().port();
    let url = Url::parse(&format!("http://localhost:{port}/start")).unwrap();
    let mut env = shim_env(&h);
    assert_eq!(run_open(&url, &mut env).await, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        format!("rowser: warning: local port {port} is busy; the callback cannot be forwarded\n")
    );
    assert_eq!(h.opener.urls(), [url]);
}

#[tokio::test]
async fn rowser_forward_carries_connections_to_the_remote_port() {
    let h = Harness::start(v6_config()).await;
    let (port, _remote) = fake_remote_http().await;
    let mut env = shim_env(&h);
    assert_eq!(run_forward(port, &mut env).await, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        format!("rowser: forwarding local port {port} to port {port} on this host\n")
    );
    assert!(h.opener.urls().is_empty());
    assert_eq!(browser_get(port, "/x").await, "remote got /x");
}

#[tokio::test]
async fn rowser_forward_on_a_busy_port_exits_1() {
    let h = Harness::start(v6_config()).await;
    let busy = TcpListener::bind(SocketAddr::new(V6, 0)).await.unwrap();
    let port = busy.local_addr().unwrap().port();
    let mut env = shim_env(&h);
    assert_eq!(run_forward(port, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        format!("rowser: warning: local port {port} is busy; the callback cannot be forwarded\n")
    );
}

#[tokio::test]
async fn a_manual_forward_binds_the_port_without_a_url() {
    let h = Harness::start(v6_config()).await;
    let port = free_port().await;
    let mut c = h.connect().await;
    c.send(&Msg::Forward {
        reported_host: "devbox".into(),
        callback_ports: vec![port],
    })
    .await
    .unwrap();
    match c.recv().await.unwrap() {
        Some(Msg::OpenResult {
            forward_id: Some(_),
            error: None,
            ..
        }) => {}
        other => panic!("unexpected {other:?}"),
    }
    assert!(h.opener.urls().is_empty());
    TcpStream::connect(SocketAddr::new(V6, port)).await.unwrap();
}
