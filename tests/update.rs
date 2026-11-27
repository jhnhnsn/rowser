//! The automatic update: a Link to an Outpost of another major version.
//! The fake `ssh` never reaches a host: it answers `uname -sm`, runs the
//! install script and the Link's remote command on this machine, with HOME
//! at `<tmp>/remote` and the Outpost socket folder in the temp dir. The
//! "old" rowser on that HOME is a script that runs this rowser with the
//! hidden `ROWSER_TEST_OUTPOST_VERSION=0.9.0`; the install replaces it.

mod common;
#[path = "common/script.rs"]
mod script;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::config::SavedHost;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, Subject};
use script::write_script;

const ROWSER: &str = env!("CARGO_BIN_EXE_rowser");

/// The remote `PATH` of the install: only the tools its script needs.
fn remote_toolbox(dir: &Path) -> PathBuf {
    let tools = dir.join("toolbox");
    std::fs::create_dir_all(&tools).unwrap();
    let path = std::env::var("PATH").unwrap();
    for tool in [
        "sh", "mkdir", "rm", "cat", "chmod", "mv", "ln", "head", "awk",
    ] {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on PATH"));
        std::os::unix::fs::symlink(found, tools.join(tool)).unwrap();
    }
    tools
}

struct World {
    dir: TmpDir,
    notifier: RecordingNotifier,
}

impl World {
    fn start(auto_update: bool) -> World {
        let dir = TmpDir::new("u");
        let d = &dir.0;
        let remote = d.join("remote");
        let bin = remote.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_script(
            &bin.join("rowser"),
            &format!("ROWSER_TEST_OUTPOST_VERSION=0.9.0 exec {ROWSER} \"$@\""),
        );
        let ssh = d.join("ssh");
        write_script(
            &ssh,
            &format!(
                "printf '%s\\n' \"$*\" >> {args}\n\
                 eval \"last=\\${{$#}}\"\n\
                 export HOME={remote}\n\
                 case \"$*\" in\n\
                 *' -- g14 uname -sm') echo 'Linux x86_64'; exit 0;;\n\
                 *\" -- g14 sh -c '\"*) PATH={toolbox} exec /bin/sh -c \"$last\";;\n\
                 *' g14 '*' __outpost')\n\
                 mkdir -p {o}\n\
                 export ROWSER_TEST_OUTPOST_DIR={o}/g14\n\
                 eval \"path=\\${{$(($# - 1))}}\"\n\
                 exec /bin/sh -c \"$path __outpost\";;\n\
                 esac\n\
                 exit 1",
                args = d.join("args").display(),
                remote = remote.display(),
                toolbox = remote_toolbox(d).display(),
                o = d.join("o").display(),
            ),
        );
        let run = d.join("run");
        let (direct, relay, control) = bind_sockets(&run).unwrap();
        let config = DaemonConfig {
            hosts: vec![SavedHost {
                name: "g14".into(),
                remote_path: "~/.local/bin/rowser".into(),
                auto_update,
                default_browser: false,
            }],
            ssh,
            link_dir: run,
            update_local_build: Some(ROWSER.into()),
            ..DaemonConfig::default()
        };
        let notifier = RecordingNotifier::default();
        let daemon = Daemon::new(config, RecordingOpener::default(), notifier.clone());
        tokio::spawn(daemon.serve(direct, relay, control));
        World { dir, notifier }
    }

    fn args(&self) -> String {
        std::fs::read_to_string(self.dir.0.join("args")).unwrap_or_default()
    }

    fn installs(&self) -> usize {
        self.args()
            .lines()
            .filter(|l| l.ends_with("uname -sm"))
            .count()
    }

    fn status(&self) -> String {
        let run = self.dir.0.join("run");
        let reply = tokio::task::block_in_place(|| {
            rowser::control::request(&run, &rowser::protocol::Msg::Status)
        });
        format!("{reply:?}")
    }
}

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

#[tokio::test(flavor = "multi_thread")]
async fn a_mismatch_with_auto_update_installs_notifies_once_and_reconnects() {
    let w = World::start(true);
    let version = env!("CARGO_PKG_VERSION");
    let want = format!("outpost_rowser_version: Some(\"{version}\")");
    let connected = wait_until(|| {
        let s = w.status();
        s.contains("Connected") && s.contains(&want)
    })
    .await;
    assert!(connected, "{}", w.status());
    let notices = w.notifier.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0].subject,
        Subject::Updated {
            version: version.into()
        }
    );
    assert_eq!(
        notices[0].body(),
        format!("From g14: updated rowser on g14 to {version}")
    );
    assert_eq!(w.installs(), 1, "{}", w.args());
    // The install keeps how the host was added: no default-browser part.
    let remote = w.dir.0.join("remote");
    assert!(std::fs::symlink_metadata(remote.join(".local/bin/xdg-open")).is_err());
    assert!(remote
        .join(rowser::remote_install::NO_DEFAULT_BROWSER_MARKER)
        .is_file());
    // The install runs without prompts, on rowser's own control path.
    let install = w
        .args()
        .lines()
        .find(|l| l.ends_with("uname -sm"))
        .unwrap()
        .to_string();
    assert!(install.starts_with("-o BatchMode=yes"), "{install}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mismatch_without_auto_update_notifies_once_and_does_not_install() {
    let w = World::start(false);
    assert!(wait_until(|| !w.notifier.notices().is_empty()).await);
    // Wait for at least two more Link attempts (1 s, then 2 s).
    let attempts = || {
        w.args()
            .lines()
            .filter(|l| l.ends_with("__outpost"))
            .count()
    };
    assert!(wait_until(|| attempts() >= 3).await, "{}", w.args());
    let notices = w.notifier.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0].subject,
        Subject::VersionMismatch {
            outpost: Some("0.9.0".into())
        }
    );
    assert!(
        notices[0].body().contains("rowser remote install g14"),
        "{}",
        notices[0].body()
    );
    assert_eq!(w.installs(), 0);
    assert!(w.status().contains("VersionMismatch"), "{}", w.status());
}
