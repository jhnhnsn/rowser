//! Two Local machines with a Link to one Remote host (#80): two Daemons,
//! each with its own runtime dir and fake `ssh`, whose Outposts share one
//! Outpost socket folder. The fake `ssh` never reaches a host: it runs
//! `rowser __outpost` here with HOME in the Daemon's temp dir.
//!
//! The forward test uses the `[::1]` / 127.0.0.1 split of tests/callback.rs.

mod common;
#[path = "common/script.rs"]
mod script;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::config::SavedHost;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
use rowser::detect::Procfs;
use rowser::protocol::{Conn, Hello};
use rowser::shim::{run_forward, run_open, ShimEnv};
use script::write_script;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use url::Url;

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

/// One Local machine: a Daemon in `dir` with the Saved host `g14`.
struct Local {
    dir: TmpDir,
    opener: RecordingOpener,
}

impl Local {
    /// Starts a Daemon whose fake `ssh` puts the Outpost socket in
    /// `outpost`. The `ssh` fails while the file `dir/down` exists.
    fn start(outpost: &Path) -> Local {
        let dir = TmpDir::new("2l");
        let ssh = dir.0.join("ssh");
        write_script(
            &ssh,
            &format!(
                "[ -e {down} ] && exit 1\n\
                 export HOME={home}\n\
                 export XDG_CONFIG_HOME={home}/cfg\n\
                 export ROWSER_TEST_OUTPOST_DIR={outpost}\n\
                 echo $$ > {pid}\n\
                 shift $(($# - 2))\n\
                 exec \"$@\"",
                down = dir.0.join("down").display(),
                home = dir.0.display(),
                outpost = outpost.display(),
                pid = dir.0.join("pid").display(),
            ),
        );
        let (direct, relay, control) = bind_sockets(&dir.0.join("run")).unwrap();
        let config = DaemonConfig {
            hosts: vec![SavedHost {
                name: "g14".into(),
                remote_path: env!("CARGO_BIN_EXE_rowser").into(),
                auto_update: true,
                default_browser: true,
            }],
            loopback: vec![V6],
            ssh,
            link_dir: dir.0.join("run"),
            ..DaemonConfig::default()
        };
        let opener = RecordingOpener::default();
        let daemon = Daemon::new(config, opener.clone(), RecordingNotifier::default());
        tokio::spawn(daemon.serve(direct, relay, control));
        Local { dir, opener }
    }

    /// Drops this Link: the `ssh` fails from now on, and the Outpost is killed.
    fn drop_link(&self) {
        std::fs::write(self.dir.0.join("down"), "").unwrap();
        let pid: i32 = std::fs::read_to_string(self.dir.0.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Never signal init or a process group.
        assert!(pid > 1, "{pid}");
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// The shared Outpost socket folder, in a short temp dir.
struct Remote {
    dir: TmpDir,
}

impl Remote {
    fn new() -> Remote {
        let dir = TmpDir::new("2r");
        Remote { dir }
    }

    fn outpost(&self) -> PathBuf {
        self.dir.0.join("o")
    }

    fn socket(&self) -> PathBuf {
        self.outpost().join("outpost.sock")
    }

    /// The inode of the Outpost socket, if it exists.
    fn ino(&self) -> Option<u64> {
        std::fs::symlink_metadata(self.socket())
            .ok()
            .map(|m| m.ino())
    }

    /// Waits until the Outpost socket answers a handshake.
    async fn wait_connected(&self) {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if let Ok(s) = UnixStream::connect(self.socket()).await {
                if Conn::connect(s, Hello::current()).await.is_ok() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no Link answers on the Outpost socket");
    }

    /// Waits until `f` is true for the socket's inode, at most 20 s.
    async fn wait_ino(&self, f: impl Fn(Option<u64>) -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if f(self.ino()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the Outpost socket did not change: {:?}", self.ino());
    }

    /// A Shim on this Remote host that finds the Outpost socket.
    fn shim(&self) -> ShimEnv<Vec<u8>> {
        ShimEnv {
            tunnel_dir: self.dir.0.join("no-tunnels"),
            socket_override: None,
            outpost_socket: Some(self.socket()),
            reported_host: "g14-box".into(),
            procfs: Procfs::new(self.dir.0.join("no-proc")),
            caller_pid: 1,
            graphical: false,
            search_path: None,
            self_exe: None,
            in_fallback: false,
            stderr: Vec::new(),
        }
    }

    async fn open(&self, url: &Url) {
        let mut env = self.shim();
        let code = run_open(url, &mut env).await;
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "{}",
            String::from_utf8_lossy(&env.stderr)
        );
    }
}

/// Starts the older Link, then the newer one, and waits until the newer
/// one holds the socket. Returns the inode of the older one's socket.
async fn two_links(remote: &Remote) -> (Local, Local, u64) {
    let older = Local::start(&remote.outpost());
    remote.wait_connected().await;
    let first = remote.ino().unwrap();
    let newer = Local::start(&remote.outpost());
    remote.wait_ino(|i| i.is_some() && i != Some(first)).await;
    remote.wait_connected().await;
    (older, newer, first)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_link_that_connected_last_gets_the_url_and_the_other_after_it_drops() {
    let remote = Remote::new();
    let (older, newer, first) = two_links(&remote).await;

    let a = Url::parse("https://example.com/a").unwrap();
    remote.open(&a).await;
    assert_eq!(newer.opener.urls(), std::slice::from_ref(&a));
    assert!(older.opener.urls().is_empty());

    newer.drop_link();
    remote.wait_ino(|i| i == Some(first)).await;
    remote.wait_connected().await;
    let b = Url::parse("https://example.com/b").unwrap();
    remote.open(&b).await;
    assert_eq!(older.opener.urls(), [b]);
    assert_eq!(newer.opener.urls(), [a]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_newer_link_keeps_the_socket_when_the_older_one_drops() {
    let remote = Remote::new();
    let (older, newer, _) = two_links(&remote).await;
    let socket = remote.ino();
    older.drop_link();
    // Three repair checks of the newer Outpost.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(remote.ino(), socket);
    let c = Url::parse("https://example.com/c").unwrap();
    remote.open(&c).await;
    assert_eq!(newer.opener.urls(), [c]);
    assert!(older.opener.urls().is_empty());
}

/// The fake remote program's listener on 127.0.0.1, on a port that is also
/// free on `[::1]`.
fn remote_listener() -> (u16, TcpListener) {
    loop {
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        if std::net::TcpListener::bind(SocketAddr::new(V6, port)).is_ok() {
            v4.set_nonblocking(true).unwrap();
            return (port, TcpListener::from_std(v4).unwrap());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_callback_forward_of_the_older_link_works_while_the_newer_one_is_up() {
    let remote = Remote::new();
    let older = Local::start(&remote.outpost());
    remote.wait_connected().await;
    let first = remote.ino().unwrap();
    let (port, listener) = remote_listener();
    let mut env = remote.shim();
    assert_eq!(
        run_forward(port, &mut env).await,
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&env.stderr)
    );

    let _newer = Local::start(&remote.outpost());
    remote.wait_ino(|i| i.is_some() && i != Some(first)).await;
    remote.wait_connected().await;

    let browser = async {
        let mut b = TcpStream::connect(SocketAddr::new(V6, port)).await.unwrap();
        b.write_all(b"GET /cb").await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), b.read_to_end(&mut got))
            .await
            .expect("the relay did not close")
            .unwrap();
        got
    };
    let program = async {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 7];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"GET /cb");
        s.write_all(b"200 OK").await.unwrap();
    };
    let (got, ()) = tokio::join!(browser, program);
    assert_eq!(got, b"200 OK");
    drop(older);
}
