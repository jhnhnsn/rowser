//! A Link from the Daemon to a Saved host, through a fake `ssh` that runs
//! `rowser __outpost` on this machine. The fake `ssh` never reaches a host:
//! it drops the ssh options and the alias and runs the remote command here,
//! with HOME in a temp dir and the Outpost socket folder at `o/<alias>` in
//! that temp dir.

mod common;
#[path = "common/script.rs"]
mod script;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::config::SavedHost;
use rowser::control::run_reload;
use rowser::daemon::testing::{Answer, RecordingNotifier, RecordingOpener, ScriptedNotifier};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, LinkEvent, Notifier, Subject};
use rowser::detect::Procfs;
use rowser::protocol::{Conn, Error, Hello, Version};
use rowser::routes::Routes;
use rowser::shim::{run_open, ShimEnv};
use script::write_script;
use tokio::net::UnixStream;
use url::Url;

struct World {
    dir: TmpDir,
    opener: RecordingOpener,
    notifier: RecordingNotifier,
}

/// The Saved host `name`, whose `remote_path` is this rowser binary.
fn saved_host(name: &str) -> SavedHost {
    SavedHost {
        name: name.into(),
        remote_path: env!("CARGO_BIN_EXE_rowser").into(),
        auto_update: true,
        default_browser: true,
    }
}

/// Writes the fake `ssh` in `dir`. It runs `body` before the remote command;
/// `body` may exit to fail the run.
fn fake_ssh(dir: &Path, body: &str) -> PathBuf {
    let ssh = dir.join("ssh");
    write_script(
        &ssh,
        &format!(
            "RUNS={runs}\n\
             date +%s.%N >> \"$RUNS\"\n\
             printf '%s\\n' \"$*\" >> {args}\n\
             eval \"alias=\\${{$(($# - 2))}}\"\n\
             export HOME={home}\n\
             mkdir -p {outpost}\n\
             export ROWSER_TEST_OUTPOST_DIR={outpost}/$alias\n\
             {body}\n\
             echo $$ > {pid}-$alias\n\
             shift $(($# - 2))\n\
             exec \"$@\"",
            runs = dir.join("runs").display(),
            args = dir.join("args").display(),
            pid = dir.join("pid").display(),
            home = dir.display(),
            outpost = dir.join("o").display(),
        ),
    );
    ssh
}

/// Starts a Daemon in `dir` with `config`, the fake `ssh` and the runtime
/// directory `dir/run`.
fn start_daemon(
    dir: &Path,
    body: &str,
    config: DaemonConfig,
    opener: RecordingOpener,
    notifier: impl Notifier,
) {
    let ssh = fake_ssh(dir, body);
    let control = bind_sockets(&dir.join("run")).unwrap();
    let config = DaemonConfig {
        ssh,
        link_dir: dir.join("run"),
        ..config
    };
    let daemon = Daemon::new(config, opener, notifier);
    tokio::spawn(daemon.serve(control));
}

impl World {
    /// Starts a Daemon with one Saved host, `g14`, whose `ssh` runs `body`
    /// before the remote command. `body` may exit to fail the run.
    fn start(body: &str) -> World {
        World::start_with(
            body,
            DaemonConfig {
                hosts: vec![saved_host("g14")],
                ..DaemonConfig::default()
            },
        )
    }

    /// Starts a Daemon with `config`, the fake `ssh` and a recording
    /// Opener and Notifier.
    fn start_with(body: &str, config: DaemonConfig) -> World {
        let dir = TmpDir::new("l");
        let opener = RecordingOpener::default();
        let notifier = RecordingNotifier::default();
        start_daemon(&dir.0, body, config, opener.clone(), notifier.clone());
        World {
            dir,
            opener,
            notifier,
        }
    }

    fn socket(&self) -> PathBuf {
        socket_of(&self.dir.0, "g14")
    }

    /// The start time of each `ssh` run.
    fn runs(&self) -> Vec<f64> {
        std::fs::read_to_string(self.dir.0.join("runs"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.parse().unwrap())
            .collect()
    }

    /// Waits until the Outpost socket answers a handshake.
    async fn wait_connected(&self) {
        wait_connected(&self.socket()).await;
    }

    fn shim(&self) -> ShimEnv<Vec<u8>> {
        shim(&self.dir.0, Some(self.socket()))
    }
}

/// The Outpost socket of the Saved host `alias` in `dir`.
fn socket_of(dir: &Path, alias: &str) -> PathBuf {
    dir.join("o").join(alias).join("outpost.sock")
}

/// The pid that the fake `ssh` (then the Outpost) of `alias` wrote.
fn pid_of(dir: &Path, alias: &str) -> i32 {
    let pid: i32 = std::fs::read_to_string(dir.join(format!("pid-{alias}")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Never signal init or a process group.
    assert!(pid > 1, "{pid}");
    pid
}

/// Waits until the Outpost socket at `socket` answers a handshake.
async fn wait_connected(socket: &Path) {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if let Ok(s) = UnixStream::connect(socket).await {
            if Conn::connect(s, Hello::current()).await.is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the Link did not connect");
}

/// A Shim environment with no `/proc`.
fn shim(dir: &Path, outpost_socket: Option<PathBuf>) -> ShimEnv<Vec<u8>> {
    ShimEnv {
        socket_override: None,
        outpost_socket,
        procfs: Procfs::new(dir.join("no-proc")),
        caller_pid: 1,
        graphical: false,
        search_path: None,
        self_exe: None,
        in_fallback: false,
        stderr: Vec::new(),
    }
}

/// Waits until `f` is true, at most 20 s.
async fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f()
}

/// True while the process `pid` exists.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_url_through_the_outpost_opens_with_the_host_alias() {
    let w = World::start("");
    w.wait_connected().await;
    let url = Url::parse("https://example.com/a").unwrap();
    let mut env = w.shim();
    let code = run_open(&url, &mut env).await;
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&env.stderr)
    );
    assert_eq!(w.opener.urls(), [url]);
    let hosts: Vec<_> = w.notifier.notices().into_iter().map(|n| n.host).collect();
    assert!(hosts.iter().all(|h| h == "g14"));
    let args = std::fs::read_to_string(w.dir.0.join("args")).unwrap();
    assert!(
        args.starts_with("-T -o BatchMode=yes -o ControlMaster=no"),
        "{args}"
    );
    assert!(args.contains(" g14 "), "{args}");
    assert!(args.trim_end().ends_with("__outpost"), "{args}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_url_with_callback_ports_through_the_outpost_still_opens() {
    let w = World::start("");
    w.wait_connected().await;
    // A free port: nothing listens on it once the probe closes.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = Url::parse(&format!(
        "https://example.com/auth?redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2Fcb"
    ))
    .unwrap();
    let mut env = w.shim();
    let code = run_open(&url, &mut env).await;
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&env.stderr)
    );
    assert_eq!(w.opener.urls(), [url]);
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_on_the_link_use_the_host_alias() {
    let dir = TmpDir::new("lr");
    let file = dir.0.join("config.toml");
    std::fs::write(&file, "[[route]]\nhost = \"g14\"\nopen = \"safari\"\n").unwrap();
    let w = World::start_with(
        "",
        DaemonConfig {
            hosts: vec![saved_host("g14")],
            routes: Routes::from_config(&rowser::config::load(&file).unwrap()),
            ..DaemonConfig::default()
        },
    );
    w.wait_connected().await;
    let url = Url::parse("https://example.com/r").unwrap();
    let mut env = w.shim();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::SUCCESS);
    // With no browsers, the `safari` Target falls back with a warning that
    // names it: so the `g14` Route applied, not the Shim's own host name.
    // The notice is sent from a blocking task after the reply.
    assert!(wait_until(|| !w.notifier.notices().is_empty()).await);
    let warnings = w.notifier.notices()[0].warnings.clone();
    assert!(
        warnings.len() == 1 && warnings[0].contains("safari"),
        "{warnings:?}"
    );
}

/// Runs `rowser status` against the Daemon in `dir`; gives (exit, stdout).
async fn status(dir: &Path, json: bool) -> (ExitCode, String) {
    let run = dir.join("run");
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = rowser::control::StatusArgs {
            runtime_dir: Ok(run),
            json,
        };
        let code = rowser::control::run_status(args, &mut out, &mut err);
        (code, String::from_utf8(out).unwrap())
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_shows_each_link_with_its_state_outpost_and_target() {
    let dir = TmpDir::new("ls");
    let file = dir.0.join("config.toml");
    std::fs::write(&file, "[[route]]\nhost = \"g14\"\nopen = \"safari\"\n").unwrap();
    let w = World::start_with(
        "",
        DaemonConfig {
            hosts: vec![saved_host("g14")],
            routes: Routes::from_config(&rowser::config::load(&file).unwrap()),
            ..DaemonConfig::default()
        },
    );
    w.wait_connected().await;
    // The Daemon marks the Link connected after the Outpost's Pong.
    let out = status_until(&w.dir.0, |o| o.contains("  g14  connected  since ")).await;
    let lines: Vec<&str> = out.lines().collect();
    let at = lines.iter().position(|l| *l == "links:").expect(&out);
    let g14 = lines[at + 1];
    assert!(g14.starts_with("  g14  connected  since "), "{out}");
    assert!(
        g14.ends_with(&format!(
            "  outpost {}  target safari",
            env!("CARGO_PKG_VERSION")
        )),
        "{out}"
    );
    assert!(lines[at + 2].starts_with("callback forwards"), "{out}");

    let (code, json) = status(&w.dir.0, true).await;
    assert_eq!(code, ExitCode::SUCCESS);
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let link = &v["links"][0];
    assert_eq!(link["host"], "g14", "{v}");
    assert_eq!(link["state"], "connected", "{v}");
    assert!(link["since"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(link["outpost_rowser_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(link["outpost_version"], "2.0", "{v}");
    assert_eq!(link["target"], "safari", "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn status_without_saved_hosts_says_no_links() {
    let w = World::start_with("", DaemonConfig::default());
    let out = status_until(&w.dir.0, |o| !o.is_empty()).await;
    assert!(out.contains("\nlinks:             none\n"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&status(&w.dir.0, true).await.1).unwrap();
    assert_eq!(v["links"], serde_json::json!([]), "{v}");
}

/// The `rowser status` text once `done` is true for it, at most 20 s.
async fn status_until(dir: &Path, done: impl Fn(&str) -> bool) -> String {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let out = status(dir, false).await.1;
        if done(&out) || Instant::now() >= end {
            assert!(done(&out), "{out}");
            return out;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn confirm_hosts_on_the_link_match_the_host_alias() {
    let dir = TmpDir::new("lc");
    let notifier = ScriptedNotifier::new(Answer::Deny);
    start_daemon(
        &dir.0,
        "",
        DaemonConfig {
            hosts: vec![saved_host("g14")],
            confirm_hosts: vec!["g1?".into()],
            ..DaemonConfig::default()
        },
        RecordingOpener::default(),
        notifier.clone(),
    );
    let socket = socket_of(&dir.0, "g14");
    wait_connected(&socket).await;
    let url = Url::parse("https://example.com/c").unwrap();
    let mut env = shim(&dir.0, Some(socket));
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    let confirms = notifier.confirms();
    assert_eq!(confirms.len(), 1, "{confirms:?}");
    assert_eq!(confirms[0].host, "g14");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reload_adds_and_removes_links() {
    let dir = TmpDir::new("ll");
    let file = dir.0.join("config.toml");
    let host = |name: &str| {
        format!(
            "[[host]]\nname = \"{name}\"\nremote_path = \"{}\"\n",
            env!("CARGO_BIN_EXE_rowser")
        )
    };
    std::fs::write(&file, host("g14")).unwrap();
    start_daemon(
        &dir.0,
        "",
        DaemonConfig {
            hosts: rowser::config::load(&file).unwrap().hosts,
            config_file: Some(file.clone()),
            ..DaemonConfig::default()
        },
        RecordingOpener::default(),
        RecordingNotifier::default(),
    );
    wait_connected(&socket_of(&dir.0, "g14")).await;
    let g14 = pid_of(&dir.0, "g14");

    std::fs::write(&file, host("pi")).unwrap();
    let run = dir.0.join("run");
    let (code, err) = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_reload(Ok(run), &mut out, &mut err);
        (code, String::from_utf8(err).unwrap())
    })
    .await
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "{err}");

    // The new Saved host gets a Link; the removed one loses its Link.
    wait_connected(&socket_of(&dir.0, "pi")).await;
    assert!(
        wait_until(|| !alive(g14)).await,
        "the g14 Link is still open"
    );
    let args = std::fs::read_to_string(dir.0.join("args")).unwrap();
    assert_eq!(args.lines().filter(|l| l.contains(" g14 ")).count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_1_x_hello_through_the_outpost_gets_the_mismatch_error() {
    let w = World::start("");
    w.wait_connected().await;
    let s = UnixStream::connect(w.socket()).await.unwrap();
    let r = Conn::connect(s, Hello::with_version(Version::new(1, 1))).await;
    match r {
        Err(e @ Error::VersionMismatch { .. }) => {
            let text = e.to_string();
            assert!(text.contains("2.0") && text.contains("1.1"), "{text}");
        }
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rate_limit_holds_across_shim_connections() {
    let w = World::start("");
    w.wait_connected().await;
    let mut codes = Vec::new();
    for i in 0..6 {
        let url = Url::parse(&format!("https://example.com/{i}")).unwrap();
        codes.push(run_open(&url, &mut w.shim()).await);
    }
    assert_eq!(&codes[..5], [ExitCode::SUCCESS; 5]);
    assert_eq!(codes[5], ExitCode::FAILURE);
    assert_eq!(w.opener.urls().len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_link_reconnects_with_backoff() {
    // The first two runs fail; the third connects.
    let w = World::start("[ \"$(wc -l < \"$RUNS\")\" -le 2 ] && exit 1");
    w.wait_connected().await;
    let runs = w.runs();
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert!(runs[1] - runs[0] >= 0.9, "{runs:?}");
    assert!(runs[2] - runs[1] >= 1.9, "{runs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_link_reconnects_after_the_backoff() {
    let w = World::start("");
    w.wait_connected().await;
    // The Outpost of this test only: the fake ssh wrote its own pid.
    let pid = pid_of(&w.dir.0, "g14");
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let end = Instant::now() + Duration::from_secs(20);
    while w.runs().len() < 2 && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let runs = w.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    w.wait_connected().await;
    let url = Url::parse("https://example.com/again").unwrap();
    assert_eq!(run_open(&url, &mut w.shim()).await, ExitCode::SUCCESS);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_failure_needs_login_and_stops_retries() {
    let w = World::start("echo 'g14: Permission denied (publickey).' >&2\nexit 255");
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(w.runs().len(), 1);
    let notices = w.notifier.notices();
    assert_eq!(notices.len(), 1);
    assert!(matches!(
        notices[0].subject,
        Subject::Link(LinkEvent::NeedsLogin)
    ));
    assert_eq!(notices[0].host, "g14");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_permission_error_of_the_remote_command_retries() {
    // The remote shell cannot run `remote_path`: not a login failure.
    let w = World::start("echo 'sh: 1: /x/rowser: Permission denied' >&2\nexit 126");
    let more = wait_until(|| w.runs().len() >= 2).await;
    assert!(more, "{:?}", w.runs());
    assert!(w
        .notifier
        .notices()
        .iter()
        .all(|n| !matches!(n.subject, Subject::Link(LinkEvent::NeedsLogin))));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shim_skips_an_outpost_socket_in_a_folder_that_others_can_read() {
    let w = World::start("");
    w.wait_connected().await;
    let folder = w.socket().parent().unwrap().to_path_buf();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let url = Url::parse("https://example.com/skip").unwrap();
    let mut env = w.shim();
    assert_eq!(run_open(&url, &mut env).await, ExitCode::FAILURE);
    let stderr = String::from_utf8_lossy(&env.stderr);
    assert!(
        stderr.contains("warning: not using the Outpost socket") && stderr.contains("0700"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rowser: no Link to your Local machine; open this URL manually: https://example.com/skip"),
        "{stderr}"
    );
    assert!(w.opener.urls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn rowser_socket_skips_the_outpost_owner_check() {
    let w = World::start("");
    w.wait_connected().await;
    let folder = w.socket().parent().unwrap().to_path_buf();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let url = Url::parse("https://example.com/override").unwrap();
    let mut env = shim(&w.dir.0, None);
    env.socket_override = Some(w.socket());
    let code = run_open(&url, &mut env).await;
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&env.stderr)
    );
    assert_eq!(w.opener.urls(), [url]);
}

#[test]
fn the_outpost_refuses_a_folder_that_others_can_read() {
    let dir = TmpDir::new("lo");
    let o = dir.0.join("o");
    std::fs::create_dir(&o).unwrap();
    std::fs::set_permissions(&o, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .arg("__outpost")
        .env("HOME", &dir.0)
        .env("XDG_CONFIG_HOME", dir.0.join("cfg"))
        .env("ROWSER_TEST_OUTPOST_DIR", &o)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("rowser __outpost: "), "{err}");
    assert!(!Path::new(&o).join("outpost.sock").exists());
}

#[test]
fn the_outpost_removes_its_socket_when_its_stdio_closes() {
    let dir = TmpDir::new("ls");
    let o = dir.0.join("o");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .arg("__outpost")
        .env("HOME", &dir.0)
        .env("XDG_CONFIG_HOME", dir.0.join("cfg"))
        .env("ROWSER_TEST_OUTPOST_DIR", &o)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let socket = o.join("outpost.sock");
    let end = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "the Outpost made no socket");
    drop(child.stdin.take());
    drop(child.stdout.take());
    let end = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > end {
            let _ = child.kill();
            panic!("the Outpost did not exit when its stdio closed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status:?}");
    assert!(!socket.exists());
}
