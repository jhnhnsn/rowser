//! The Wrapper (`rowser ssh`) against a real Daemon and a fake `ssh` script.
//!
//! Every test runs the fake ssh through `/bin/sh`, which all Unix targets
//! have. Tests that run the `rowser` binary find the runtime directory
//! through `XDG_RUNTIME_DIR`, so they are Linux-only.

#[path = "common/script.rs"]
mod script;

use script::write_script;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, NoticeHost, RELAY_SOCKET};
use rowser::protocol::{Conn, Hello, Msg};
use rowser::wrapper::{run_ssh, WrapperEnv};
use tokio::net::UnixStream;

/// A short temp dir under /tmp (Unix socket paths are limited to ~104 bytes).
struct TmpDir(PathBuf);

impl TmpDir {
    fn new() -> TmpDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = PathBuf::from(format!("/tmp/rww.{}.{n}", std::process::id()));
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

/// A fake ssh: `-G` prints a `host` line for `alias`; otherwise it records
/// its argv (one per line) in `argv`, waits for `done`, and exits `code`.
fn fake_ssh(dir: &Path, alias: Option<&str>, code: u8) -> PathBuf {
    fake_ssh_with(dir, alias, "", &format!("exit {code}"))
}

/// Like `fake_ssh`, with `pre` run before the argv is recorded and `tail`
/// run once `done` exists.
fn fake_ssh_with(dir: &Path, alias: Option<&str>, pre: &str, tail: &str) -> PathBuf {
    let g = match alias {
        Some(a) => format!("printf 'user me\\nhost {a}\\nport 22\\n'; exit 0"),
        None => "exit 255".into(),
    };
    let d = dir.display();
    let script = format!(
        "if [ \"$1\" = \"-G\" ]; then {g}; fi\n{pre}\n\
         printf '%s\\n' \"$@\" > {d}/argv.tmp\nmv {d}/argv.tmp {d}/argv\n\
         while [ ! -e {d}/done ]; do sleep 0.02; done\n{tail}"
    );
    let path = dir.join("ssh");
    write_script(&path, &script);
    path
}

fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(start.elapsed() < Duration::from_secs(10), "no {path:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn argv(dir: &Path) -> Vec<String> {
    wait_for(&dir.join("argv"));
    let text = std::fs::read_to_string(dir.join("argv")).unwrap();
    text.lines().map(String::from).collect()
}

struct Setup {
    dir: TmpDir,
    notifier: RecordingNotifier,
    env: WrapperEnv,
}

async fn setup(alias: Option<&str>, code: u8, daemon: bool) -> Setup {
    let dir = TmpDir::new();
    let run = dir.0.join("run");
    let notifier = RecordingNotifier::default();
    if daemon {
        let (direct, relay, control) = bind_sockets(&run).unwrap();
        let d = Daemon::new(
            DaemonConfig::default(),
            RecordingOpener::default(),
            notifier.clone(),
        );
        tokio::spawn(d.serve(direct, relay, control));
    } else {
        std::fs::create_dir(&run).unwrap();
    }
    let env = WrapperEnv {
        ssh: fake_ssh(&dir.0, alias, code),
        relay_socket: run.join(RELAY_SOCKET),
        session_dir: run,
    };
    Setup { dir, notifier, env }
}

fn spawn_wrapper(args: &[&str], env: WrapperEnv) -> std::thread::JoinHandle<ExitCode> {
    let args = args.iter().map(Into::into).collect();
    std::thread::spawn(move || run_ssh(args, env))
}

async fn open_via(socket: &str) -> Msg {
    let local = socket.split_once(':').unwrap().1;
    let stream = UnixStream::connect(local).await.unwrap();
    let mut conn = Conn::connect(stream, Hello::current()).await.unwrap();
    conn.send(&Msg::Open {
        url: "https://example.com/".into(),
        reported_host: "forged".into(),
        callback_ports: vec![],
    })
    .await
    .unwrap();
    conn.recv().await.unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn tunnel_is_stamped_with_the_ssh_g_alias() {
    let s = setup(Some("work-box"), 0, true).await;
    let wrapper = spawn_wrapper(
        &["-p", "2222", "alice@10.0.0.5", "tmux", "attach"],
        s.env.clone(),
    );
    let got = argv(&s.dir.0);
    assert_eq!(got[0], "-R");
    let (remote, local) = got[1].split_once(':').unwrap();
    assert!(
        remote.starts_with("/tmp/rowser-") && remote.ends_with(".sock"),
        "{remote}"
    );
    assert_eq!(Path::new(local).parent().unwrap(), s.env.session_dir);
    let mode = std::fs::metadata(local).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(got[2..], ["-p", "2222", "alice@10.0.0.5", "tmux", "attach"]);

    let reply = open_via(&got[1]).await;
    assert!(
        matches!(reply, Msg::OpenResult { error: None, .. }),
        "{reply:?}"
    );
    let hosts: Vec<_> = s.notifier.notices().into_iter().map(|n| n.host).collect();
    assert_eq!(hosts, [NoticeHost::Alias("work-box".into())]);

    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::SUCCESS);
    assert!(!Path::new(local).exists(), "per-session socket left behind");
}

#[tokio::test(flavor = "multi_thread")]
async fn alias_falls_back_to_the_argv_destination() {
    let s = setup(None, 0, true).await;
    let wrapper = spawn_wrapper(&["-i", "/k", "bob@myhost", "ls"], s.env.clone());
    let got = argv(&s.dir.0);
    open_via(&got[1]).await;
    let hosts: Vec<_> = s.notifier.notices().into_iter().map(|n| n.host).collect();
    assert_eq!(hosts, [NoticeHost::Alias("myhost".into())]);
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::SUCCESS);
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_exit_code_is_passed_through() {
    let s = setup(Some("h"), 42, true).await;
    let wrapper = spawn_wrapper(&["h"], s.env.clone());
    argv(&s.dir.0);
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::from(42));
}

#[test]
#[cfg(target_os = "linux")]
fn rowser_ssh_warns_when_the_daemon_is_not_running() {
    let dir = TmpDir::new();
    let run = dir.0.join("rowser");
    std::fs::create_dir(&run).unwrap();
    fake_ssh(&dir.0, Some("h"), 5);
    std::fs::write(dir.0.join("done"), "").unwrap();
    let path = format!("{}:{}", dir.0.display(), std::env::var("PATH").unwrap());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["ssh", "h", "true"])
        .env("PATH", path)
        .env("XDG_RUNTIME_DIR", &dir.0)
        .env_remove("ROWSER_RUNTIME_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "rowser ssh: the daemon is not running (no answer on {}/rowserd-relay.sock); \
             running ssh without the URL forward\n",
            run.display()
        )
    );
    assert_eq!(argv(&dir.0), ["h", "true"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_stopped_by_a_signal_exits_128_plus_the_signal() {
    let s = setup(Some("h"), 0, true).await;
    let mut env = s.env.clone();
    env.ssh = fake_ssh_with(&s.dir.0, Some("h"), "", "kill -TERM $$");
    let wrapper = spawn_wrapper(&["h"], env);
    argv(&s.dir.0);
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::from(128 + 15));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_host_alias_ssh_runs_without_the_forward() {
    let s = setup(None, 0, true).await;
    let wrapper = spawn_wrapper(&["-V"], s.env.clone());
    assert_eq!(argv(&s.dir.0), ["-V"]);
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::SUCCESS);
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_that_cannot_start_exits_255() {
    let s = setup(Some("h"), 0, true).await;
    let mut env = s.env.clone();
    env.ssh = s.dir.0.join("no-such-ssh");
    assert_eq!(
        spawn_wrapper(&["h"], env).join().unwrap(),
        ExitCode::from(255)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bind_failure_runs_ssh_without_the_forward() {
    let s = setup(Some("h"), 4, true).await;
    let mut env = s.env.clone();
    env.session_dir = s.dir.0.join("missing");
    let wrapper = spawn_wrapper(&["-v", "h"], env);
    assert_eq!(argv(&s.dir.0), ["-v", "h"]);
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::from(4));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_stamp_forged_through_the_tunnel_is_refused() {
    let s = setup(Some("work-box"), 0, true).await;
    let wrapper = spawn_wrapper(&["h"], s.env.clone());
    let got = argv(&s.dir.0);
    let local = got[1].split_once(':').unwrap().1;
    let stream = UnixStream::connect(local).await.unwrap();
    let mut conn = Conn::connect(stream, Hello::current()).await.unwrap();
    conn.send(&Msg::HostStamp {
        alias: "bank".into(),
    })
    .await
    .unwrap();
    let reply = conn.recv().await.unwrap().unwrap();
    match reply {
        Msg::OpenResult { error: Some(e), .. } => {
            assert_eq!(e, "only one host stamp is allowed per connection")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(s.notifier.notices().is_empty());
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::SUCCESS);
}

/// Runs `rowser ssh h` with no Daemon, sends `sig` to the Wrapper, and
/// checks that ssh got it (the fake ssh traps it and exits 7).
#[cfg(target_os = "linux")]
fn signal_reaches_ssh(sig: libc::c_int, name: &str) {
    let dir = TmpDir::new();
    std::fs::create_dir(dir.0.join("rowser")).unwrap();
    let d = dir.0.display();
    let trap = format!("trap 'echo {name} > {d}/sig; exit 7' {name}");
    fake_ssh_with(&dir.0, Some("h"), &trap, "exit 0");
    let path = format!("{}:{}", dir.0.display(), std::env::var("PATH").unwrap());
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["ssh", "h"])
        .env("PATH", path)
        .env("XDG_RUNTIME_DIR", &dir.0)
        .env_remove("ROWSER_RUNTIME_DIR")
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    argv(&dir.0);
    // SAFETY: kill has no memory-safety preconditions.
    unsafe {
        libc::kill(child.id() as libc::pid_t, sig);
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(7), "{name}");
    let got = std::fs::read_to_string(dir.0.join("sig")).unwrap();
    assert_eq!(got.trim(), name);
}

#[test]
#[cfg(target_os = "linux")]
fn sigterm_is_sent_on_to_ssh() {
    signal_reaches_ssh(libc::SIGTERM, "TERM");
}

#[test]
#[cfg(target_os = "linux")]
fn sighup_is_sent_on_to_ssh() {
    signal_reaches_ssh(libc::SIGHUP, "HUP");
}

#[test]
#[cfg(target_os = "linux")]
fn rowser_ssh_keeps_a_double_dash_and_help_flags() {
    let dir = TmpDir::new();
    std::fs::create_dir(dir.0.join("rowser")).unwrap();
    fake_ssh(&dir.0, Some("h"), 0);
    std::fs::write(dir.0.join("done"), "").unwrap();
    let path = format!("{}:{}", dir.0.display(), std::env::var("PATH").unwrap());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["ssh", "-h", "--", "h"])
        .env("PATH", path)
        .env("XDG_RUNTIME_DIR", &dir.0)
        .env_remove("ROWSER_RUNTIME_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(argv(&dir.0), ["-h", "--", "h"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn rowser_ssh_finds_the_daemon_through_rowser_runtime_dir() {
    let dir = TmpDir::new();
    let run = dir.0.join("rt");
    let (direct, relay, control) = bind_sockets(&run).unwrap();
    let d = Daemon::new(
        DaemonConfig::default(),
        RecordingOpener::default(),
        RecordingNotifier::default(),
    );
    tokio::spawn(d.serve(direct, relay, control));
    fake_ssh(&dir.0, Some("h"), 0);
    let path = format!("{}:{}", dir.0.display(), std::env::var("PATH").unwrap());
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["ssh", "h"])
        .env("PATH", path)
        .env("XDG_RUNTIME_DIR", dir.0.join("elsewhere"))
        .env("ROWSER_RUNTIME_DIR", &run)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let got = argv(&dir.0);
    std::fs::write(dir.0.join("done"), "").unwrap();
    let out = tokio::task::spawn_blocking(|| child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(got[0], "-R", "{}", String::from_utf8_lossy(&out.stderr));
    let local = got[1].split_once(':').unwrap().1;
    assert_eq!(Path::new(local).parent().unwrap(), run);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
#[cfg(unix)]
fn rowser_ssh_refuses_a_relative_rowser_runtime_dir() {
    let dir = TmpDir::new();
    fake_ssh(&dir.0, Some("h"), 0);
    std::fs::write(dir.0.join("done"), "").unwrap();
    let path = format!("{}:{}", dir.0.display(), std::env::var("PATH").unwrap());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rowser"))
        .args(["ssh", "h"])
        .env("PATH", path)
        .env("ROWSER_RUNTIME_DIR", "rt")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "rowser ssh: ROWSER_RUNTIME_DIR must be an absolute path, not rt; \
         running ssh without the URL forward\n"
    );
    assert_eq!(argv(&dir.0), ["h"]);
}

/// `rowser status --json` against the runtime directory `run`.
async fn status_json(run: PathBuf) -> serde_json::Value {
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = rowser::control::run_status(
            rowser::control::StatusArgs {
                runtime_dir: Ok(run),
                json: true,
            },
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
        serde_json::from_slice(&out).unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_lists_the_session_while_ssh_runs_and_drops_it_after() {
    let s = setup(Some("g14"), 0, true).await;
    let wrapper = spawn_wrapper(&["g14"], s.env.clone());
    argv(&s.dir.0);
    let v = status_json(s.env.session_dir.clone()).await;
    let sessions = v["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{v}");
    assert_eq!(sessions[0]["host"], "g14");
    assert_eq!(sessions[0]["target"], "system");
    assert!(sessions[0]["age_secs"].as_u64().unwrap() < 60);

    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::SUCCESS);
    for _ in 0..50 {
        if status_json(s.env.session_dir.clone()).await["sessions"] == serde_json::json!([]) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the session is still in status");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_1_0_daemon_is_refused_and_ssh_runs_without_the_forward() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let dir = TmpDir::new();
    let run = dir.0.join("run");
    std::fs::create_dir(&run).unwrap();
    let listener = tokio::net::UnixListener::bind(run.join(RELAY_SOCKET)).unwrap();
    // A 1.0 Daemon: answers each hello with 1.0 and records every line.
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = lines.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut r = BufReader::new(r).lines();
                let _hello = r.next_line().await;
                w.write_all(b"{\"version\":{\"major\":1,\"minor\":0}}\n")
                    .await
                    .unwrap();
                while let Ok(Some(l)) = r.next_line().await {
                    seen.lock().unwrap().push(l);
                }
            });
        }
    });
    let env = WrapperEnv {
        ssh: fake_ssh(&dir.0, Some("g14"), 0),
        relay_socket: run.join(RELAY_SOCKET),
        session_dir: run,
    };
    let wrapper = spawn_wrapper(&["g14"], env);
    // Protocol 2.0 refuses a 1.x peer, so ssh runs with no `-R` forward.
    assert_eq!(argv(&dir.0), ["g14"]);
    std::fs::write(dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::SUCCESS);
    let seen = lines.lock().unwrap().clone();
    assert!(seen.iter().all(|l| !l.contains("Session")), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_daemon_ssh_runs_without_the_forward() {
    let s = setup(Some("h"), 3, false).await;
    let wrapper = spawn_wrapper(&["-v", "h"], s.env.clone());
    assert_eq!(argv(&s.dir.0), ["-v", "h"]);
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::from(3));
}
