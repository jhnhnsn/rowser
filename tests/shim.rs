#[path = "common/script.rs"]
mod script;

use script::write_script;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
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

/// Held by tests that start child processes: a fork in another test thread
/// holds a copy of a closed listener until exec. A tokio Mutex, as some
/// holders keep it across an await.
static SPAWN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The no-Link message for `url`, both lines.
fn no_link(url: &str) -> String {
    format!(
        "rowser: no Link to your Local machine; open this URL manually: {url}\n\
         On your Local machine, run \"rowser host add <host>\", or check \"rowser host list\".\n"
    )
}

struct TestDaemon {
    dir: TmpDir,
    opener: RecordingOpener,
}

impl TestDaemon {
    /// The test Link socket of the Saved host `devbox`.
    fn link(&self) -> PathBuf {
        self.dir.path("run/test-link.sock")
    }
}

async fn start_daemon() -> TestDaemon {
    let dir = TmpDir::new();
    let run = dir.path("run");
    let control = bind_sockets(&run).unwrap();
    let link = UnixListener::bind(run.join("test-link.sock")).unwrap();
    let opener = RecordingOpener::default();
    let daemon = Daemon::new(
        DaemonConfig::default(),
        opener.clone(),
        RecordingNotifier::default(),
    );
    tokio::spawn(daemon.serve_with_test_links(control, vec![("devbox".into(), link)]));
    TestDaemon { dir, opener }
}

fn env(over: Option<PathBuf>, outpost: Option<PathBuf>) -> ShimEnv<Vec<u8>> {
    ShimEnv {
        socket_override: over,
        outpost_socket: outpost,
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
async fn no_link_in_a_graphical_session_runs_the_real_xdg_open() {
    let _spawn = SPAWN.lock().await;
    let t = TmpDir::new();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let mut env = env(None, Some(t.path("missing.sock")));
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
async fn no_link_without_a_graphical_session_does_not_run_xdg_open() {
    let _spawn = SPAWN.lock().await;
    let t = TmpDir::new();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let mut env = env(None, Some(t.path("missing.sock")));
    env.search_path = Some(bin.clone().into_os_string());
    env.self_exe = Some(env!("CARGO_BIN_EXE_rowser").into());
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert!(!bin.join("arg").exists());
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        no_link("https://example.com/")
    );
}

#[tokio::test]
async fn an_xdg_open_that_links_to_rowser_is_skipped() {
    let t = TmpDir::new();
    let bin = t.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_rowser"));
    std::os::unix::fs::symlink(&exe, bin.join("xdg-open")).unwrap();
    let mut env = env(Some(t.path("missing.sock")), None);
    env.graphical = true;
    env.search_path = Some(bin.into_os_string());
    env.self_exe = Some(exe);
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        no_link("https://example.com/")
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
    let mut env = env(Some(t.path("missing.sock")), None);
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
/// HOME and XDG_CONFIG_HOME are under `t`, and the Outpost socket folder is
/// an empty temp dir, so no Link answers. The child runs in its own process
/// group, killed after 20 s.
fn run_bin(t: &TmpDir, path: &Path, display: &str, extra: &[(&str, &str)]) -> (i32, String) {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let outpost = t.path("outpost");
    std::fs::create_dir_all(&outpost).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rowser"));
    cmd.args(["open", "https://example.com/a?b=c"])
        .env_clear()
        .env("PATH", path)
        .env("HOME", &t.0)
        .env("XDG_CONFIG_HOME", t.path("config"))
        .env("XDG_RUNTIME_DIR", t.path("xdg-run"))
        .env("DISPLAY", display)
        .env("ROWSER_TEST_OUTPOST_DIR", &outpost)
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
fn the_binary_with_no_link_and_a_display_hands_the_url_to_the_real_xdg_open() {
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
    assert_eq!(err, "");
}

#[test]
fn the_binary_with_no_link_and_no_display_prints_the_no_link_message() {
    let _spawn = SPAWN.blocking_lock();
    let t = TmpDir::new();
    let bin = t.path("bin");
    fake_xdg_open(&bin);
    let (code, err) = run_bin(&t, &bin, "", &[]);
    assert_eq!(code, 1);
    assert_eq!(err, no_link("https://example.com/a?b=c"));
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
    assert_eq!(err, no_link("https://example.com/a?b=c"));
}

#[tokio::test]
async fn open_through_the_rowser_socket_env_var_reaches_the_opener() {
    let d = start_daemon().await;
    let empty = TmpDir::new();
    let mut env = env(Some(d.link()), Some(empty.path("missing.sock")));
    let url = Url::parse("https://example.com/login?x=1").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::SUCCESS);
    let opened: Vec<String> = d.opener.urls().iter().map(|u| u.to_string()).collect();
    assert_eq!(opened, ["https://example.com/login?x=1"]);
    assert_eq!(String::from_utf8(env.stderr).unwrap(), "");
}

#[tokio::test]
async fn open_prints_a_daemon_error_and_exits_1() {
    let d = start_daemon().await;
    let mut env = env(Some(d.link()), None);
    let url = Url::parse("file:///etc/passwd").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert!(d.opener.urls().is_empty());
    let err = String::from_utf8(env.stderr).unwrap();
    assert!(err.starts_with("rowser: "), "{err}");
    assert!(!err.contains("no Link"), "{err}");
}

#[tokio::test]
async fn open_without_a_link_prints_the_url_and_the_hint_and_exits_1() {
    let empty = TmpDir::new();
    let mut env = env(None, Some(empty.path("missing.sock")));
    let url = Url::parse("https://example.com/a?b=c").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        no_link("https://example.com/a?b=c")
    );
}

#[tokio::test]
async fn open_reports_a_version_mismatch_then_no_link() {
    let dir = TmpDir::new();
    let sock = dir.path("old.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = Conn::connect(stream, Hello::with_version(Version::new(99, 0))).await;
        }
    });
    let mut env = env(Some(sock), None);
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        format!(
            "rowser: protocol version mismatch: this rowser speaks 2.0, the peer speaks 99.0; \
             update the remote with `rowser remote install <host>` or reinstall rowser locally\n{}",
            no_link("https://example.com/")
        )
    );
}

#[tokio::test]
async fn open_prints_daemon_warnings_and_exits_0() {
    let dir = TmpDir::new();
    let sock = dir.path("w.sock");
    let listener = UnixListener::bind(&sock).unwrap();
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
    let mut env = env(Some(sock), None);
    let url = Url::parse("https://example.com/").unwrap();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(env.stderr).unwrap(),
        "rowser: warning: x\n"
    );
}
