#[path = "common/script.rs"]
mod script;

use script::write_script;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, DIRECT_SOCKET};
use rowser::discovery::find_tunnel;
use rowser::protocol::{Conn, Hello, Msg, Version};
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

/// Held by tests that start child processes, and by the Stale socket test: a
/// fork in another test thread holds a copy of a closed listener until exec,
/// so the Stale socket can look live for a moment. A tokio Mutex, as some
/// holders keep it across an await.
static SPAWN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _spawn = SPAWN.lock().await;
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
    let (direct, relay, control) = bind_sockets(&dir.path("run")).unwrap();
    let opener = RecordingOpener::default();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        opener.clone(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve(direct, relay, control));
    TestDaemon { dir, opener }
}

fn env(tunnel_dir: &Path, over: Option<PathBuf>) -> ShimEnv<Vec<u8>> {
    ShimEnv {
        tunnel_dir: tunnel_dir.to_path_buf(),
        socket_override: over,
        outpost_socket: None,
        reported_host: "devbox".into(),
        procfs: rowser::detect::Procfs::new("/nonexistent"),
        caller_pid: 1,
        graphical: false,
        search_path: None,
        self_exe: None,
        in_fallback: false,
        stderr: Vec::new(),
    }
}

/// Writes an executable fake `xdg-open` into `dir` that records its argument
/// in `dir/arg` and exits 3.
fn fake_xdg_open(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let script = dir.join("xdg-open");
    let rec = dir.join("arg");
    write_script(
        &script,
        &format!("printf '%s' \"$1\" > '{}'\nexit 3", rec.display()),
    );
    script
}

#[tokio::test]
async fn no_tunnel_in_a_graphical_session_runs_the_real_xdg_open() {
    let _spawn = SPAWN.lock().await;
    let t = TmpDir::new();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let mut env = env(&t.0, Some(t.path("missing.sock")));
    env.graphical = true;
    env.search_path = Some(std::env::join_paths([t.path("empty"), bin.clone()]).unwrap());
    env.self_exe = Some(env!("CARGO_BIN_EXE_rowser").into());
    let url = Url::parse("https://example.com/a?b=c").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::from(3));
    assert_eq!(
        std::fs::read_to_string(bin.join("arg")).unwrap(),
        "https://example.com/a?b=c"
    );
    assert_eq!(String::from_utf8(env.stderr).unwrap(), "");
}

#[tokio::test]
async fn no_tunnel_without_a_graphical_session_does_not_run_xdg_open() {
    let _spawn = SPAWN.lock().await;
    let t = TmpDir::new();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let mut env = env(&t.0, Some(t.path("missing.sock")));
    env.search_path = Some(bin.clone().into_os_string());
    env.self_exe = Some(env!("CARGO_BIN_EXE_rowser").into());
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert!(!bin.join("arg").exists());
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        "rowser: no tunnel; open this URL manually: https://example.com/\n"
    );
}

#[tokio::test]
async fn an_xdg_open_that_links_to_rowser_is_skipped() {
    let t = TmpDir::new();
    let bin = t.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_rowser"));
    std::os::unix::fs::symlink(&exe, bin.join("xdg-open")).unwrap();
    let mut env = env(&t.0, Some(t.path("missing.sock")));
    env.graphical = true;
    env.search_path = Some(bin.into_os_string());
    env.self_exe = Some(exe);
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        "rowser: no tunnel; open this URL manually: https://example.com/\n"
    );
}

#[tokio::test]
async fn the_first_xdg_open_that_is_not_rowser_runs() {
    let _spawn = SPAWN.lock().await;
    let t = TmpDir::new();
    let link = t.path("link");
    std::fs::create_dir_all(&link).unwrap();
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_rowser"));
    std::os::unix::fs::symlink(&exe, link.join("xdg-open")).unwrap();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let mut env = env(&t.0, Some(t.path("missing.sock")));
    env.graphical = true;
    env.search_path = Some(std::env::join_paths([link, bin.clone()]).unwrap());
    env.self_exe = Some(exe);
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::from(3));
    assert_eq!(
        std::fs::read_to_string(bin.join("arg")).unwrap(),
        "https://example.com/"
    );
}

/// Runs `rowser open <url>` with a clean environment: `PATH` is only `path`,
/// HOME is `t`, an empty Tunnel directory and a missing `ROWSER_SOCKET`.
/// The child runs in its own process group, killed after 20 s.
fn run_bin(t: &TmpDir, path: &Path, display: &str, extra: &[(&str, &str)]) -> (i32, String) {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let tunnels = t.path("tunnels");
    std::fs::create_dir_all(&tunnels).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rowser"));
    cmd.args(["open", "https://example.com/a?b=c"])
        .env_clear()
        .env("PATH", path)
        .env("HOME", &t.0)
        .env("XDG_CONFIG_HOME", t.path("config"))
        .env("DISPLAY", display)
        .env("ROWSER_TEST_TUNNEL_DIR", &tunnels)
        .env("ROWSER_SOCKET", t.path("missing.sock"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let pgid = child.id() as i32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() > deadline {
            // SAFETY: kill has no memory preconditions.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
            let _ = child.wait();
            panic!("rowser open did not end in 20 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    (status.code().unwrap_or(-1), err)
}

#[test]
fn the_binary_reads_display_and_runs_the_real_xdg_open() {
    let _spawn = SPAWN.blocking_lock();
    let t = TmpDir::new();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let (code, err) = run_bin(&t, &bin, ":99", &[]);
    assert_eq!(code, 3, "{err}");
    assert_eq!(
        std::fs::read_to_string(bin.join("arg")).unwrap(),
        "https://example.com/a?b=c"
    );
}

#[test]
fn the_binary_with_an_empty_display_prints_the_no_tunnel_message() {
    let _spawn = SPAWN.blocking_lock();
    let t = TmpDir::new();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let (code, err) = run_bin(&t, &bin, "", &[]);
    assert_eq!(code, 1);
    assert_eq!(
        err,
        "rowser: no tunnel; open this URL manually: https://example.com/a?b=c\n"
    );
    assert!(!bin.join("arg").exists());
}

#[test]
fn a_real_xdg_open_that_runs_browser_rowser_does_not_loop() {
    let _spawn = SPAWN.blocking_lock();
    let t = TmpDir::new();
    let bin = t.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("xdg-open");
    write_script(&script, "exec \"$BROWSER\" \"$1\"");
    let (code, err) = run_bin(
        &t,
        &bin,
        ":99",
        &[("BROWSER", env!("CARGO_BIN_EXE_rowser"))],
    );
    assert_eq!(code, 1, "{err}");
    assert_eq!(
        err,
        "rowser: no tunnel; open this URL manually: https://example.com/a?b=c\n"
    );
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

#[tokio::test]
async fn open_reports_a_version_mismatch_then_no_tunnel() {
    let dir = TmpDir::new();
    let listener = UnixListener::bind(dir.path("rowser-old.sock")).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = Conn::connect(stream, Hello::with_version(Version::new(99, 0))).await;
        }
    });
    let mut env = env(&dir.0, None);
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        "rowser: protocol version mismatch: this rowser speaks 2.0, the peer speaks 99.0; \
         update the remote with `rowser remote install <host>` or reinstall rowser locally\n\
         rowser: no tunnel; open this URL manually: https://example.com/\n"
    );
}

#[tokio::test]
async fn open_prints_daemon_warnings_and_exits_0() {
    let dir = TmpDir::new();
    let listener = UnixListener::bind(dir.path("rowser-w.sock")).unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (mut conn, _) = Conn::accept(stream).await.unwrap();
        let _ = conn.recv().await.unwrap();
        conn.send(&Msg::OpenResult {
            forward_id: None,
            warnings: vec!["x".into()],
            error: None,
        })
        .await
        .unwrap();
    });
    let mut env = env(&dir.0, None);
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        "rowser: warning: x\n"
    );
}

#[tokio::test]
async fn discovery_ignores_a_symlink_to_a_live_socket() {
    let dir = TmpDir::new();
    let live = dir.path("live.sock");
    serve_named(&live, "live");
    std::os::unix::fs::symlink(&live, dir.path("rowser-l.sock")).unwrap();
    assert!(find_tunnel(&dir.0, None).await.is_err());
}
