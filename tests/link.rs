//! A Link from the Daemon to a Saved host, through a fake `ssh` that runs
//! `rowser __outpost` on this machine. The fake `ssh` never reaches a host:
//! it drops the ssh options and the alias and runs the remote command here,
//! with the Outpost socket folder and HOME in a temp dir.

mod common;
#[path = "common/script.rs"]
mod script;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::config::SavedHost;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, NoticeHost, Subject};
use rowser::detect::Procfs;
use rowser::protocol::{Conn, Error, Hello, Version};
use rowser::shim::{run_open, ShimEnv};
use script::write_script;
use tokio::net::UnixStream;
use url::Url;

struct World {
    dir: TmpDir,
    opener: RecordingOpener,
    notifier: RecordingNotifier,
}

impl World {
    /// Starts a Daemon with one Saved host, `g14`, whose `ssh` runs `body`
    /// before the remote command. `body` may exit to fail the run.
    fn start(body: &str) -> World {
        let dir = TmpDir::new("l");
        let ssh = dir.0.join("ssh");
        write_script(
            &ssh,
            &format!(
                "RUNS={runs}\n\
                 date +%s.%N >> \"$RUNS\"\n\
                 printf '%s\\n' \"$*\" >> {args}\n\
                 export HOME={home}\n\
                 export ROWSER_TEST_OUTPOST_DIR={outpost}\n\
                 {body}\n\
                 echo $$ > {pid}\n\
                 shift $(($# - 2))\n\
                 exec \"$@\"",
                runs = dir.0.join("runs").display(),
                args = dir.0.join("args").display(),
                pid = dir.0.join("pid").display(),
                home = dir.0.display(),
                outpost = dir.0.join("o").display(),
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
            ssh,
            link_dir: dir.0.join("run"),
            ..DaemonConfig::default()
        };
        let opener = RecordingOpener::default();
        let notifier = RecordingNotifier::default();
        let daemon = Daemon::new(config, opener.clone(), notifier.clone());
        tokio::spawn(daemon.serve(direct, relay, control));
        World {
            dir,
            opener,
            notifier,
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.0.join("o").join("outpost.sock")
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

    fn shim(&self) -> ShimEnv<Vec<u8>> {
        ShimEnv {
            tunnel_dir: self.dir.0.join("no-tunnels"),
            socket_override: None,
            outpost_socket: Some(self.socket()),
            reported_host: "g14-box".into(),
            procfs: Procfs::new(self.dir.0.join("no-proc")),
            caller_pid: 1,
            keeper_exe: None,
            graphical: false,
            search_path: None,
            self_exe: None,
            in_fallback: false,
            stderr: Vec::new(),
        }
    }
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
    assert!(hosts.iter().all(|h| *h == NoticeHost::Alias("g14".into())));
    let args = std::fs::read_to_string(w.dir.0.join("args")).unwrap();
    assert!(
        args.starts_with("-T -o BatchMode=yes -o ControlMaster=no"),
        "{args}"
    );
    assert!(args.contains(" g14 "), "{args}");
    assert!(args.trim_end().ends_with("__outpost"), "{args}");
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
    let pid: i32 = std::fs::read_to_string(w.dir.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The Outpost of this test only: the fake ssh wrote its own pid.
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
    assert!(matches!(notices[0].subject, Subject::NeedsLogin));
    assert_eq!(notices[0].host, NoticeHost::Alias("g14".into()));
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
