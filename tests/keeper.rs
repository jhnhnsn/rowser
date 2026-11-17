//! The keeper process, driven in-process against a fake Tunnel.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rowser::protocol::{Conn, Msg};
use rowser::shim::{run_keeper, KeeperArgs};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, UnixListener, UnixStream};
use tokio::time::Instant;

struct TmpDir(PathBuf);

impl TmpDir {
    fn new() -> TmpDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = PathBuf::from(format!("/tmp/rw.k{}.{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    let dir = TmpDir::new();
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
        assert_eq!(data.recv().await.unwrap(), Some(Msg::DataConn { conn_id }));
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
    let dir = TmpDir::new();
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

#[tokio::test(start_paused = true)]
async fn the_keeper_ends_after_the_timeout() {
    let dir = TmpDir::new();
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
    let dir = TmpDir::new();
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
