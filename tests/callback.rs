//! Callback forwards through the Outpost, over a Link whose fake `ssh` runs
//! `rowser __outpost` on this machine (see tests/link.rs).
//!
//! Both sides of a forward share this machine, so they use different
//! loopback addresses: the Daemon binds the local side on `[::1]` only, and
//! the fake remote program listens on 127.0.0.1, which the Outpost dials
//! first. A browser connects to `[::1]`.

mod common;
#[path = "common/script.rs"]
mod script;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::config::SavedHost;
use rowser::daemon::testing::{Answer, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
use rowser::detect::Procfs;
use rowser::protocol::{Conn, Hello};
use rowser::shim::{run_forward, run_open, ShimEnv};
use script::write_script;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use url::Url;

const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

struct World {
    dir: TmpDir,
    opener: RecordingOpener,
    notifier: ScriptedNotifier,
}

/// Writes the fake `ssh`: it drops the ssh options and the alias and runs
/// the remote command here, with HOME and XDG_CONFIG_HOME in `dir` and the
/// Outpost socket folder at `dir/o/<alias>`. It never reaches a host.
fn fake_ssh(dir: &Path) -> PathBuf {
    let ssh = dir.join("ssh");
    write_script(
        &ssh,
        &format!(
            "eval \"alias=\\${{$(($# - 2))}}\"\n\
             export HOME={home}\n\
             export XDG_CONFIG_HOME={home}/cfg\n\
             mkdir -p {outpost}\n\
             export ROWSER_TEST_OUTPOST_DIR={outpost}/$alias\n\
             echo $$ > {pid}\n\
             shift $(($# - 2))\n\
             exec \"$@\"",
            home = dir.display(),
            outpost = dir.join("o").display(),
            pid = dir.join("pid").display(),
        ),
    );
    ssh
}

impl World {
    /// A Daemon with the Saved host `g14`, the local side of each forward
    /// on `[::1]` only, and `config` for the rest.
    async fn start(config: DaemonConfig, answer: Answer) -> World {
        let opener = RecordingOpener::default();
        World::start_with(config, answer, opener.clone(), opener).await
    }

    /// `start` with `daemon_opener` in the Daemon; `opener` is only kept.
    async fn start_with(
        config: DaemonConfig,
        answer: Answer,
        daemon_opener: impl rowser::daemon::Opener,
        opener: RecordingOpener,
    ) -> World {
        let dir = TmpDir::new("cb");
        let ssh = fake_ssh(&dir.0);
        let control = bind_sockets(&dir.0.join("run")).unwrap();
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
            ..config
        };
        let notifier = ScriptedNotifier::new(answer);
        let daemon = Daemon::new(config, daemon_opener, notifier.clone());
        tokio::spawn(daemon.serve(control));
        let w = World {
            dir,
            opener,
            notifier,
        };
        w.wait_connected().await;
        w
    }

    fn socket(&self) -> PathBuf {
        self.dir.0.join("o").join("g14").join("outpost.sock")
    }

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
        panic!("the Link did not connect");
    }

    /// A Shim that finds the Outpost socket, with no `/proc`.
    fn shim(&self) -> ShimEnv<Vec<u8>> {
        ShimEnv {
            socket_override: None,
            outpost_socket: Some(self.socket()),
            procfs: Procfs::new(self.dir.0.join("no-proc")),
            caller_pid: 1,
            graphical: false,
            search_path: None,
            self_exe: None,
            in_fallback: false,
            stderr: Vec::new(),
        }
    }

    /// Kills this test's Outpost (the fake `ssh` wrote its own pid).
    fn kill_outpost(&self) {
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

/// A port that is free on 127.0.0.1 and on `[::1]`.
fn free_port() -> u16 {
    loop {
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        if std::net::TcpListener::bind(SocketAddr::new(V6, port)).is_ok() {
            return port;
        }
    }
}

/// True while the Daemon holds the local side of `port` on `[::1]`.
fn local_held(port: u16) -> bool {
    std::net::TcpListener::bind(SocketAddr::new(V6, port)).is_err()
}

/// Waits until the forward on `port` ended (its local side is free), at most `secs`.
async fn ended_within(port: u16, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if !local_held(port) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    !local_held(port)
}

fn auth_url(port: u16) -> Url {
    Url::parse(&format!(
        "https://example.com/auth?redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Fcb"
    ))
    .unwrap()
}

/// The fake remote program: answers one connection on 127.0.0.1:`port`
/// with `reply` after it reads `want`, then closes the connection first.
async fn answer_once(listener: &TcpListener, want: &[u8], reply: &[u8]) {
    let (mut s, _) = listener.accept().await.unwrap();
    let mut buf = vec![0u8; want.len()];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, want);
    s.write_all(reply).await.unwrap();
    drop(s);
}

/// A browser connection to the local side: sends `request`, returns all it gets.
async fn browse(port: u16, request: &[u8]) -> Vec<u8> {
    let mut b = TcpStream::connect(SocketAddr::new(V6, port)).await.unwrap();
    b.write_all(request).await.unwrap();
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), b.read_to_end(&mut got))
        .await
        .expect("the relay did not close")
        .unwrap();
    got
}

fn text(env: &ShimEnv<Vec<u8>>) -> String {
    String::from_utf8_lossy(&env.stderr).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oauth_callback_goes_through_the_outpost_both_ways() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let url = auth_url(port);
    let mut env = w.shim();
    assert_eq!(
        run_open(&url, &mut env).await,
        ExitCode::SUCCESS,
        "{}",
        text(&env)
    );
    assert_eq!(w.opener.urls(), [url]);
    assert!(local_held(port));
    let (got, ()) = tokio::join!(
        browse(port, b"GET /cb?code=42"),
        answer_once(&remote, b"GET /cb?code=42", b"HTTP/1.1 200 OK")
    );
    assert_eq!(got, b"HTTP/1.1 200 OK");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forward_ends_when_the_remote_listener_closes_after_it_closed_a_connection() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let mut env = w.shim();
    assert_eq!(run_open(&auth_url(port), &mut env).await, ExitCode::SUCCESS);
    // The program closes the connection first: a TIME-WAIT entry stays on
    // 127.0.0.1:port, which a bind probe would see as "in use" (ADR 0005).
    let (got, ()) = tokio::join!(browse(port, b"cb"), answer_once(&remote, b"cb", b"ok"));
    assert_eq!(got, b"ok");
    // Two liveness checks: the port listened at least once.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        local_held(port),
        "the forward ended while the port listened"
    );
    drop(remote);
    assert!(ended_within(port, 10).await, "the forward did not end");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forward_ends_when_the_link_drops() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let _remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    assert_eq!(
        run_open(&auth_url(port), &mut w.shim()).await,
        ExitCode::SUCCESS
    );
    assert!(local_held(port));
    w.kill_outpost();
    assert!(ended_within(port, 10).await, "the forward did not end");
}

/// The `open_secs` of each Activity entry, from `rowser activity --json`.
async fn open_secs(w: &World) -> Vec<Option<u64>> {
    activity(w)
        .await
        .iter()
        .map(|e| e["open_secs"].as_u64())
        .collect()
}

/// Each entry of `rowser activity --json`.
async fn activity(w: &World) -> Vec<serde_json::Value> {
    let run = w.dir.0.join("run");
    let out = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let _ = rowser::control::run_activity(
            rowser::control::ActivityArgs {
                runtime_dir: Ok(run),
                json: true,
                follow: false,
            },
            &mut out,
            &mut std::io::sink(),
        );
        String::from_utf8(out).unwrap()
    })
    .await
    .unwrap();
    out.lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_shows_how_long_the_forward_stayed_open_after_it_ends() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let _remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    assert_eq!(
        run_open(&auth_url(port), &mut w.shim()).await,
        ExitCode::SUCCESS
    );
    assert_eq!(open_secs(&w).await, [None]);
    w.kill_outpost();
    assert!(ended_within(port, 10).await, "the forward did not end");
    for _ in 0..50 {
        // Any whole number of seconds: the test does not depend on time.
        if open_secs(&w).await[0].is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the callback forward has no duration");
}

/// The `forwards` of `rowser status --json`.
async fn status_forwards(w: &World) -> serde_json::Value {
    let run = w.dir.0.join("run");
    let out = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let _ = rowser::control::run_status(
            rowser::control::StatusArgs {
                runtime_dir: Ok(run),
                json: true,
            },
            &mut out,
            &mut std::io::sink(),
        );
        String::from_utf8(out).unwrap()
    })
    .await
    .unwrap();
    serde_json::from_str::<serde_json::Value>(out.trim()).unwrap()["forwards"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_shows_the_forward_with_its_saved_host_until_it_ends() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let _remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    assert_eq!(run_forward(port, &mut w.shim()).await, ExitCode::SUCCESS);
    let f = status_forwards(&w).await;
    assert_eq!(f[0]["port"], port);
    assert_eq!(f[0]["host"], "g14");
    assert!(f[0].get("unverified").is_none());
    w.kill_outpost();
    for _ in 0..100 {
        if status_forwards(&w).await == serde_json::json!([]) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the forward is still in status");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watch_stream_that_cannot_open_starts_no_forward_but_the_url_opens() {
    let config = DaemonConfig {
        confirm_hosts: vec!["g14".into()],
        ..DaemonConfig::default()
    };
    let w = World::start(config, Answer::Hang).await;
    let port = free_port();
    let _remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let url = auth_url(port);
    let mut env = w.shim();
    let shim = tokio::spawn({
        let url = url.clone();
        async move { run_open(&url, &mut env).await }
    });
    for _ in 0..100 {
        if !w.notifier.confirms().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(w.notifier.confirms().len(), 1);
    // The Link drops while the Notice waits: the watch stream cannot open.
    w.kill_outpost();
    tokio::time::sleep(Duration::from_millis(500)).await;
    w.notifier.release(true);
    let _ = shim.await;
    for _ in 0..50 {
        if w.opener.urls() == [url.clone()] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(w.opener.urls(), [url]);
    assert!(!local_held(port), "the local port is still held");
    assert_eq!(status_forwards(&w).await, serde_json::json!([]));
    let entries = activity(&w).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["open_secs"], 0);
    let warnings = entries[0]["warnings"].to_string();
    assert!(warnings.contains("cannot watch"), "{warnings}");
}

struct FailingOpener;

impl rowser::daemon::Opener for FailingOpener {
    fn open(&self, _: &Url, _: &rowser::config::Target) -> std::io::Result<()> {
        Err(std::io::Error::other("no browser"))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_open_ends_the_forward() {
    let w = World::start_with(
        DaemonConfig::default(),
        Answer::Approve,
        FailingOpener,
        RecordingOpener::default(),
    )
    .await;
    let port = free_port();
    let _remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let mut env = w.shim();
    assert_eq!(run_open(&auth_url(port), &mut env).await, ExitCode::FAILURE);
    assert!(text(&env).contains("no browser"), "{}", text(&env));
    for _ in 0..50 {
        if status_forwards(&w).await == serde_json::json!([]) {
            assert!(ended_within(port, 5).await);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the failed open did not end the forward");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forward_ends_after_the_timeout() {
    let config = DaemonConfig {
        forward_timeout: Duration::from_secs(1),
        ..DaemonConfig::default()
    };
    let w = World::start(config, Answer::Approve).await;
    let port = free_port();
    let _remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    assert_eq!(
        run_open(&auth_url(port), &mut w.shim()).await,
        ExitCode::SUCCESS
    );
    assert!(local_held(port));
    assert!(ended_within(port, 5).await, "the forward did not time out");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_dial_ends_the_forward() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    // Nothing listens on the remote port.
    let port = free_port();
    assert_eq!(
        run_open(&auth_url(port), &mut w.shim()).await,
        ExitCode::SUCCESS
    );
    assert!(local_held(port));
    assert_eq!(browse(port, b"cb").await, b"");
    assert!(ended_within(port, 5).await, "the forward did not end");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_local_port_still_opens_the_url_with_the_warning() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let _busy = std::net::TcpListener::bind(SocketAddr::new(V6, port)).unwrap();
    let url = auth_url(port);
    let mut env = w.shim();
    assert_eq!(
        run_open(&url, &mut env).await,
        ExitCode::SUCCESS,
        "{}",
        text(&env)
    );
    assert_eq!(w.opener.urls(), [url]);
    let err = text(&env);
    assert!(
        err.contains(&format!("warning: local port {port} is busy")),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rowser_forward_goes_through_the_outpost() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let remote = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let mut env = w.shim();
    assert_eq!(
        run_forward(port, &mut env).await,
        ExitCode::SUCCESS,
        "{}",
        text(&env)
    );
    assert!(text(&env).contains(&format!("forwarding local port {port}")));
    let (got, ()) = tokio::join!(
        browse(port, b"ping"),
        answer_once(&remote, b"ping", b"pong")
    );
    assert_eq!(got, b"pong");
    assert!(w.notifier.confirms().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn rowser_forward_on_a_busy_local_port_exits_1_with_the_warning() {
    let w = World::start(DaemonConfig::default(), Answer::Approve).await;
    let port = free_port();
    let _busy = std::net::TcpListener::bind(SocketAddr::new(V6, port)).unwrap();
    let mut env = w.shim();
    assert_eq!(run_forward(port, &mut env).await, ExitCode::FAILURE);
    assert!(text(&env).contains("is busy"), "{}", text(&env));
}

#[tokio::test(flavor = "multi_thread")]
async fn rowser_forward_waits_for_approval_on_a_confirm_host() {
    let config = || DaemonConfig {
        confirm_hosts: vec!["g14".into()],
        ..DaemonConfig::default()
    };
    let denied = World::start(config(), Answer::Deny).await;
    let port = free_port();
    let mut env = denied.shim();
    assert_eq!(run_forward(port, &mut env).await, ExitCode::FAILURE);
    assert!(text(&env).contains("not approved"), "{}", text(&env));
    assert_eq!(denied.notifier.confirms().len(), 1);
    assert!(!local_held(port));

    let approved = World::start(config(), Answer::Approve).await;
    let mut env = approved.shim();
    assert_eq!(
        run_forward(port, &mut env).await,
        ExitCode::SUCCESS,
        "{}",
        text(&env)
    );
    assert_eq!(approved.notifier.confirms().len(), 1);
    assert!(local_held(port));
}
