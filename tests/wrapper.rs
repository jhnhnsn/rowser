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
    let g = match alias {
        Some(a) => format!("printf 'user me\\nhost {a}\\nport 22\\n'; exit 0"),
        None => "exit 255".into(),
    };
    let d = dir.display();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"-G\" ]; then {g}; fi\n\
         printf '%s\\n' \"$@\" > {d}/argv.tmp\nmv {d}/argv.tmp {d}/argv\n\
         while [ ! -e {d}/done ]; do sleep 0.02; done\nexit {code}\n"
    );
    let path = dir.join("ssh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let (direct, relay) = bind_sockets(&run).unwrap();
        let d = Daemon::new(
            DaemonConfig::default(),
            RecordingOpener::default(),
            notifier.clone(),
        );
        tokio::spawn(d.serve(direct, relay));
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
    wrapper.join().unwrap();
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
async fn without_a_daemon_ssh_runs_without_the_forward() {
    let s = setup(Some("h"), 3, false).await;
    let wrapper = spawn_wrapper(&["-v", "h"], s.env.clone());
    assert_eq!(argv(&s.dir.0), ["-v", "h"]);
    std::fs::write(s.dir.0.join("done"), "").unwrap();
    assert_eq!(wrapper.join().unwrap(), ExitCode::from(3));
}
