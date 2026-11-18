//! The keeper process, driven in-process against a fake Tunnel.

mod common;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use rowser::protocol::{Conn, Msg};
use rowser::shim::{run_keeper, KeeperArgs};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream, UnixListener, UnixStream};
use tokio::time::Instant;

fn tmp() -> common::TmpDir {
    common::TmpDir::new("k")
}

fn args(socket: PathBuf, port: u16) -> KeeperArgs {
    KeeperArgs {
        socket,
        forward_id: "fid".into(),
        ports: vec![port],
        timeout: Duration::from_secs(600),
        liveness: Duration::from_secs(2),
    }
}

/// A fake Tunnel: accepts the keeper's control connection and checks the
/// attach. Returns the listener (for data connections) and the control.
async fn attached(listener: &UnixListener) -> Conn<UnixStream> {
    let (s, _) = listener.accept().await.unwrap();
    let (mut c, _) = Conn::accept(s).await.unwrap();
    assert_eq!(
        c.recv().await.unwrap(),
        Some(Msg::AttachForward {
            forward_id: "fid".into()
        })
    );
    c
}

async fn remote_listener() -> (TcpListener, u16) {
    let l = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

#[tokio::test]
async fn the_keeper_dials_the_remote_port_and_splices_bytes() {
    let dir = tmp();
    let sock = dir.0.join("t.sock");
    let tunnel = UnixListener::bind(&sock).unwrap();
    let (remote, port) = remote_listener().await;
    tokio::spawn(async move {
        loop {
            let (mut s, _) = remote.accept().await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping");
            s.write_all(b"pong").await.unwrap();
        }
    });
    let keeper = tokio::spawn(run_keeper(args(sock, port)));
    let mut control = attached(&tunnel).await;
    for conn_id in [7, 8] {
        control.send(&Msg::Dial { conn_id, port }).await.unwrap();
        let (s, _) = tunnel.accept().await.unwrap();
        let (mut data, _) = Conn::accept(s).await.unwrap();
        assert_eq!(
            data.recv().await.unwrap(),
            Some(Msg::DataConn {
                forward_id: "fid".into(),
                conn_id
            })
        );
        let (mut data, _) = data.into_raw();
        data.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        data.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }
    drop(control);
    assert_eq!(keeper.await.unwrap(), ExitCode::SUCCESS);
}

#[tokio::test(start_paused = true)]
async fn the_keeper_ends_when_the_remote_port_stops_listening() {
    let dir = tmp();
    let sock = dir.0.join("t.sock");
    let tunnel = UnixListener::bind(&sock).unwrap();
    let (remote, port) = remote_listener().await;
    let keeper = tokio::spawn(run_keeper(args(sock, port)));
    let _control = attached(&tunnel).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!keeper.is_finished(), "alive while the port listens");
    let start = Instant::now();
    drop(remote);
    keeper.await.unwrap();
    assert!(
        start.elapsed() <= Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

/// The remote program closes a callback connection first, so its side of
/// that connection stays in TIME-WAIT on the Callback port for about 60 s.
/// That is not a listener: the forward still ends when the port stops
/// listening.
#[cfg(target_os = "linux")]
#[tokio::test(start_paused = true)]
async fn the_keeper_ends_when_the_remote_port_stops_listening_after_it_closed_a_connection() {
    let dir = tmp();
    let sock = dir.0.join("t.sock");
    let tunnel = UnixListener::bind(&sock).unwrap();
    // Like Python's `socket.listen`: no SO_REUSEADDR, so the TIME-WAIT
    // entry blocks a plain bind of the port.
    let plain = TcpSocket::new_v4().unwrap();
    plain
        .bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .unwrap();
    let port = plain.local_addr().unwrap().port();
    let remote = plain.listen(1).unwrap();
    let keeper = tokio::spawn(run_keeper(args(sock, port)));
    let _control = attached(&tunnel).await;
    let mut browser = TcpStream::connect(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))
        .await
        .unwrap();
    let (served, _) = remote.accept().await.unwrap();
    drop(served);
    let mut rest = Vec::new();
    browser.read_to_end(&mut rest).await.unwrap();
    drop(browser);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!keeper.is_finished(), "alive while the port listens");
    let start = Instant::now();
    drop(remote);
    keeper.await.unwrap();
    assert!(
        start.elapsed() <= Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn the_keeper_ends_after_the_timeout() {
    let dir = tmp();
    let sock = dir.0.join("t.sock");
    let tunnel = UnixListener::bind(&sock).unwrap();
    let (_remote, port) = remote_listener().await;
    let start = Instant::now();
    let keeper = tokio::spawn(run_keeper(args(sock, port)));
    let _control = attached(&tunnel).await;
    keeper.await.unwrap();
    let t = start.elapsed();
    assert!(
        t >= Duration::from_secs(600) && t < Duration::from_secs(603),
        "{t:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_keeper_ends_when_a_dial_fails() {
    let dir = tmp();
    let sock = dir.0.join("t.sock");
    let tunnel = UnixListener::bind(&sock).unwrap();
    // Bound but not listening: the liveness check sees the port in use, but
    // a dial is refused.
    let bound = TcpSocket::new_v4().unwrap();
    bound
        .bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .unwrap();
    let port = bound.local_addr().unwrap().port();
    let keeper = tokio::spawn(run_keeper(args(sock, port)));
    let mut control = attached(&tunnel).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!keeper.is_finished(), "alive while the port is in use");
    control.send(&Msg::Dial { conn_id: 1, port }).await.unwrap();
    keeper.await.unwrap();
    assert_eq!(control.recv().await.unwrap(), None, "the control closes");
}

/// A port with a listener on 127.0.0.1 (the remote program) and another on
/// [::1] (a Daemon on the same host, as in the e2e setup).
async fn port_on_both() -> (TcpListener, TcpListener, u16) {
    loop {
        let (v4, port) = remote_listener().await;
        if let Ok(v6) = TcpListener::bind(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)).await {
            return (v4, v6, port);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_keeper_ends_when_its_listener_stops_although_the_other_loopback_address_is_held() {
    let dir = tmp();
    let sock = dir.0.join("t.sock");
    let tunnel = UnixListener::bind(&sock).unwrap();
    let (remote, _daemon_side, port) = port_on_both().await;
    let keeper = tokio::spawn(run_keeper(args(sock, port)));
    let _control = attached(&tunnel).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!keeper.is_finished(), "alive while the port listens");
    let start = Instant::now();
    drop(remote);
    keeper.await.unwrap();
    assert!(
        start.elapsed() <= Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn the_keeper_waits_for_a_port_that_does_not_listen_yet() {
    let dir = tmp();
    let sock = dir.0.join("t.sock");
    let tunnel = UnixListener::bind(&sock).unwrap();
    let (early, port) = remote_listener().await;
    drop(early);
    let keeper = tokio::spawn(run_keeper(args(sock, port)));
    let _control = attached(&tunnel).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!keeper.is_finished(), "alive before the port listened once");
    let remote = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!keeper.is_finished(), "alive while the port listens");
    let start = Instant::now();
    drop(remote);
    keeper.await.unwrap();
    assert!(
        start.elapsed() <= Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}
