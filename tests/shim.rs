use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET};
use rowser::discovery::find_tunnel;
use rowser::protocol::{Conn, Msg};
use rowser::shim::{run_open, ShimEnv};
use tokio::net::UnixListener;
use url::Url;

/// A short temp dir under /tmp (Unix socket paths are limited to ~104 bytes).
struct TmpDir(PathBuf);

impl TmpDir {
    fn new() -> TmpDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = PathBuf::from(format!("/tmp/rw.s{}.{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fake Tunnel end: completes the handshake, then sends a HostStamp naming it.
fn serve_named(path: &Path, name: &str) {
    let listener = UnixListener::bind(path).unwrap();
    let name = name.to_string();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut conn, _) = Conn::accept(stream).await.unwrap();
            conn.send(&Msg::HostStamp {
                alias: name.clone(),
            })
            .await
            .unwrap();
        }
    });
}

/// Sets a file's mtime to `secs` since the epoch, without following symlinks.
fn set_mtime(path: &Path, secs: i64) {
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    let t = libc::timespec {
        tv_sec: secs,
        tv_nsec: 0,
    };
    let times = [t, t];
    // SAFETY: c is a valid NUL-terminated path and times has two entries.
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(rc, 0);
}

async fn found_name(dir: &Path, over: Option<PathBuf>) -> String {
    let mut conn = find_tunnel(dir, over).await.expect("a tunnel");
    match conn.recv().await.unwrap() {
        Some(Msg::HostStamp { alias }) => alias,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn discovery_picks_the_newest_socket() {
    let dir = TmpDir::new();
    for (name, t) in [("old", 1_000), ("new", 3_000), ("mid", 2_000)] {
        let p = dir.path(&format!("rowser-{name}.sock"));
        serve_named(&p, name);
        set_mtime(&p, t);
    }
    assert_eq!(found_name(&dir.0, None).await, "new");
}

#[tokio::test]
async fn discovery_ignores_files_that_do_not_match_the_pattern() {
    let dir = TmpDir::new();
    serve_named(&dir.path("other-x.sock"), "other");
    serve_named(&dir.path("rowser-x.socket"), "suffix");
    std::fs::write(dir.path("rowser-file.sock"), "").unwrap();
    assert!(find_tunnel(&dir.0, None).await.is_err());
}

#[tokio::test]
async fn discovery_removes_a_stale_socket_and_uses_the_next() {
    let dir = TmpDir::new();
    let stale = dir.path("rowser-stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    set_mtime(&stale, 3_000);
    let live = dir.path("rowser-live.sock");
    serve_named(&live, "live");
    set_mtime(&live, 1_000);

    assert_eq!(found_name(&dir.0, None).await, "live");
    assert!(!stale.exists(), "the Stale socket is removed");
    assert!(live.exists());
}

#[tokio::test]
async fn discovery_keeps_a_socket_that_fails_the_handshake() {
    let dir = TmpDir::new();
    let mute = dir.path("rowser-mute.sock");
    let listener = UnixListener::bind(&mute).unwrap();
    tokio::spawn(async move {
        // Accept and close at once: connected, but no handshake.
        loop {
            let _ = listener.accept().await;
        }
    });
    assert!(find_tunnel(&dir.0, None).await.is_err());
    assert!(mute.exists());
}

#[tokio::test]
async fn discovery_tries_the_override_first() {
    let dir = TmpDir::new();
    let glob = dir.path("rowser-glob.sock");
    serve_named(&glob, "glob");
    let over = dir.path("custom.sock");
    serve_named(&over, "override");
    set_mtime(&over, 1_000);
    assert_eq!(found_name(&dir.0, Some(over)).await, "override");
}

#[tokio::test]
async fn discovery_falls_back_from_a_dead_override() {
    let dir = TmpDir::new();
    serve_named(&dir.path("rowser-glob.sock"), "glob");
    let missing = dir.path("missing.sock");
    assert_eq!(found_name(&dir.0, Some(missing)).await, "glob");
}

struct TestDaemon {
    dir: TmpDir,
    opener: RecordingOpener,
}

async fn start_daemon() -> TestDaemon {
    let dir = TmpDir::new();
    let (direct, relay) = bind_sockets(&dir.path("run")).unwrap();
    let opener = RecordingOpener::default();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        opener.clone(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(direct, relay));
    TestDaemon { dir, opener }
}

fn env(tunnel_dir: &Path, over: Option<PathBuf>) -> ShimEnv<Vec<u8>> {
    ShimEnv {
        tunnel_dir: tunnel_dir.to_path_buf(),
        socket_override: over,
        reported_host: "devbox".into(),
        stderr: Vec::new(),
    }
}

#[tokio::test]
async fn open_through_the_direct_socket_reaches_the_opener() {
    let d = start_daemon().await;
    let empty = TmpDir::new();
    let mut env = env(&empty.0, Some(d.dir.path("run").join(DIRECT_SOCKET)));
    let url = Url::parse("https://example.com/login?x=1").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::SUCCESS);
    let opened: Vec<String> = d.opener.urls().iter().map(|u| u.to_string()).collect();
    assert_eq!(opened, ["https://example.com/login?x=1"]);
    assert_eq!(String::from_utf8(env.stderr).unwrap(), "");
}

#[tokio::test]
async fn open_prints_a_daemon_error_and_exits_1() {
    let d = start_daemon().await;
    let empty = TmpDir::new();
    let mut env = env(&empty.0, Some(d.dir.path("run").join(DIRECT_SOCKET)));
    let url = Url::parse("file:///etc/passwd").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert!(d.opener.urls().is_empty());
    let err = String::from_utf8(env.stderr).unwrap();
    assert!(err.starts_with("rowser: "), "{err}");
    assert!(!err.contains("no tunnel"), "{err}");
}

#[tokio::test]
async fn open_without_a_tunnel_prints_the_url_and_exits_1() {
    let empty = TmpDir::new();
    let mut env = env(&empty.0, Some(empty.path("missing.sock")));
    let url = Url::parse("https://example.com/a?b=c").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        "rowser: no tunnel; open this URL manually: https://example.com/a?b=c\n"
    );
}
