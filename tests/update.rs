//! The automatic update: a Link to an Outpost of another major version.
//! The fake `ssh` never reaches a host: it answers `uname -sm`, runs the
//! install script and the Link's remote command on this machine, with HOME
//! at `<tmp>/remote` and the Outpost socket folder in the temp dir. The
//! "old" rowser on that HOME is a script that runs this rowser with the
//! hidden `ROWSER_TEST_OUTPOST_VERSION=0.9.0`; the install replaces it.

mod common;
#[path = "common/script.rs"]
mod script;
#[path = "common/toolbox.rs"]
mod toolbox;

use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::config::SavedHost;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig, LinkEvent, Subject};
use rowser::protocol::Msg;
use script::write_script;
use toolbox::remote_toolbox;

const ROWSER: &str = env!("CARGO_BIN_EXE_rowser");

/// How the test World is set up.
struct Setup {
    auto_update: bool,
    /// The old rowser's path under the remote HOME.
    old_path: &'static str,
    /// The update installs this build; false: a missing file, so it fails.
    build_ok: bool,
}

const DEFAULT: Setup = Setup {
    auto_update: true,
    old_path: ".local/bin/rowser",
    build_ok: true,
};

struct World {
    dir: TmpDir,
    notifier: RecordingNotifier,
}

impl World {
    fn start(setup: Setup) -> World {
        let dir = TmpDir::new("u");
        let d = &dir.0;
        let remote = d.join("remote");
        let old = remote.join(setup.old_path);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        write_script(
            &old,
            &format!("ROWSER_TEST_OUTPOST_VERSION=0.9.0 exec {ROWSER} \"$@\""),
        );
        std::fs::create_dir_all(d.join("bin")).unwrap();
        let ssh = d.join("bin").join("ssh");
        write_script(
            &ssh,
            &format!(
                "printf '%s\\n' \"$*\" >> {args}\n\
                 eval \"last=\\${{$#}}\"\n\
                 export HOME={remote}\n\
                 case \"$*\" in\n\
                 *'-- g14 uname -sm') echo 'Linux x86_64'; exit 0;;\n\
                 *\"-- g14 sh -c '\"*) PATH={toolbox} exec /bin/sh -c \"$last\";;\n\
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
        let build = if setup.build_ok {
            ROWSER.into()
        } else {
            d.join("missing-build")
        };
        let config = DaemonConfig {
            hosts: vec![SavedHost {
                name: "g14".into(),
                remote_path: format!("~/{}", setup.old_path),
                auto_update: setup.auto_update,
                default_browser: false,
            }],
            ssh,
            link_dir: run,
            update_local_build: Some(build),
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

    fn attempts(&self) -> usize {
        self.args()
            .lines()
            .filter(|l| l.ends_with("__outpost"))
            .count()
    }

    fn request(&self, msg: &Msg) -> String {
        let run = self.dir.0.join("run");
        let reply = tokio::task::block_in_place(|| rowser::control::request(&run, msg));
        format!("{reply:?}")
    }

    fn status(&self) -> String {
        self.request(&Msg::Status)
    }

    /// Waits a time in which a retrying Link would try again (1 s, then 2 s).
    async fn wait_for_retries(&self) {
        tokio::time::sleep(Duration::from_millis(3500)).await;
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

fn mismatch(after_update: bool) -> Subject {
    Subject::Link(LinkEvent::VersionMismatch {
        outpost: Some("0.9.0".into()),
        after_update,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mismatch_with_auto_update_installs_notifies_once_and_reconnects() {
    let w = World::start(DEFAULT);
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
        Subject::Link(LinkEvent::Updated {
            version: version.into()
        })
    );
    assert_eq!(
        notices[0].body(),
        format!("updated rowser on g14 to {version}")
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
async fn a_mismatch_without_auto_update_notifies_once_and_stops_retries() {
    let w = World::start(Setup {
        auto_update: false,
        ..DEFAULT
    });
    assert!(wait_until(|| !w.notifier.notices().is_empty()).await);
    w.wait_for_retries().await;
    // No more Link attempts: each one would start an Outpost on the host.
    assert_eq!(w.attempts(), 1, "{}", w.args());
    let notices = w.notifier.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].subject, mismatch(false));
    assert!(
        notices[0].body().contains("rowser remote install g14"),
        "{}",
        notices[0].body()
    );
    assert_eq!(w.installs(), 0);
    assert!(w.status().contains("VersionMismatch"), "{}", w.status());
}

#[tokio::test(flavor = "multi_thread")]
async fn rowser_remote_install_ends_the_wait() {
    let w = World::start(Setup {
        auto_update: false,
        ..DEFAULT
    });
    assert!(wait_until(|| !w.notifier.notices().is_empty()).await);
    let d = &w.dir.0;
    let out = tokio::task::block_in_place(|| {
        std::process::Command::new(ROWSER)
            .args([
                "remote",
                "install",
                "g14",
                "--no-default-browser",
                "--local-build",
            ])
            .arg(ROWSER)
            .env("PATH", format!("{}:/usr/bin:/bin", d.join("bin").display()))
            .env("HOME", d)
            .env("XDG_CONFIG_HOME", d.join("cfg"))
            .env("ROWSER_RUNTIME_DIR", d.join("run"))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    });
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("the Link to g14 connects again"),
        "{out:?}"
    );
    let version = env!("CARGO_PKG_VERSION");
    let want = format!("outpost_rowser_version: Some(\"{version}\")");
    let connected = wait_until(|| {
        let s = w.status();
        s.contains("Connected") && s.contains(&want)
    })
    .await;
    assert!(connected, "{}", w.status());
    assert_eq!(w.notifier.notices().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_that_still_mismatches_says_to_add_the_host_again() {
    // rowser is not at ~/.local/bin/rowser, where the install puts it.
    let w = World::start(Setup {
        old_path: "opt/rowser",
        ..DEFAULT
    });
    assert!(wait_until(|| !w.notifier.notices().is_empty()).await);
    w.wait_for_retries().await;
    let notices = w.notifier.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].subject, mismatch(true));
    assert!(
        notices[0].body().contains("rowser host add g14"),
        "{}",
        notices[0].body()
    );
    assert_eq!(w.installs(), 1, "{}", w.args());
    assert_eq!(w.attempts(), 2, "{}", w.args());
    assert!(w.status().contains("VersionMismatch"), "{}", w.status());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_update_waits_and_a_reconnect_tries_once_more() {
    let w = World::start(Setup {
        build_ok: false,
        ..DEFAULT
    });
    assert!(wait_until(|| !w.notifier.notices().is_empty()).await);
    w.wait_for_retries().await;
    let notices = w.notifier.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].subject, mismatch(false));
    assert_eq!(w.attempts(), 1, "{}", w.args());
    // The update tries once: `uname -sm` runs, then the missing build fails.
    assert_eq!(w.installs(), 1, "{}", w.args());

    // `rowser host connect` sends `Reconnect`: one more Link attempt and
    // one more update try, with one more notification.
    let reply = w.request(&Msg::Reconnect { host: "g14".into() });
    assert!(reply.contains("Reconnected"), "{reply}");
    assert!(wait_until(|| w.notifier.notices().len() == 2).await);
    w.wait_for_retries().await;
    assert_eq!(w.notifier.notices().len(), 2);
    assert_eq!(w.installs(), 2, "{}", w.args());
    assert_eq!(w.attempts(), 2, "{}", w.args());
}
