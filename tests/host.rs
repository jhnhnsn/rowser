//! `rowser host add|remove|list|connect`, run as the real binary with a
//! fake `ssh` first on `PATH`. The fake `ssh` never reaches a host: it logs
//! its arguments, answers `command -v rowser` from `FAKE_FOUND`, returns at
//! once for the master connection (`-N -f`), and runs a Link's remote
//! command (`__outpost`) on this machine with HOME in the temp dir.
//! HOME, XDG_CONFIG_HOME and ROWSER_RUNTIME_DIR all point into the temp dir.

mod common;
#[path = "common/script.rs"]
mod script;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
use rowser::protocol::{Conn, Hello};
use script::write_script;
use tokio::net::UnixStream;

const ROWSER: &str = env!("CARGO_BIN_EXE_rowser");

struct World {
    dir: TmpDir,
}

impl World {
    fn new(tag: &str) -> World {
        let dir = TmpDir::new(tag);
        let d = &dir.0;
        std::fs::create_dir_all(d.join("bin")).unwrap();
        write_script(
            &d.join("bin").join("ssh"),
            &format!(
                "printf '%s\\n' \"$*\" >> {args}\n\
                 case \"$*\" in\n\
                 *'-N -f'*) touch {master}; exit 0;;\n\
                 *'command -v rowser'*) [ -n \"$FAKE_FOUND\" ] || exit 1; echo \"$FAKE_FOUND\"; exit 0;;\n\
                 *__outpost)\n\
                 eval \"alias=\\${{$(($# - 2))}}\"\n\
                 if [ -f {deny} ] && [ ! -f {master} ]; then echo \"$alias: Permission denied (publickey).\" >&2; exit 255; fi\n\
                 export HOME={home}\n\
                 mkdir -p {outpost}\n\
                 export ROWSER_TEST_OUTPOST_DIR={outpost}/$alias\n\
                 echo $$ > {pid}-$alias\n\
                 shift $(($# - 2))\n\
                 exec \"$@\";;\n\
                 esac\n\
                 exit 1",
                args = d.join("args").display(),
                master = d.join("master").display(),
                deny = d.join("deny").display(),
                home = d.display(),
                outpost = d.join("o").display(),
                pid = d.join("pid").display(),
            ),
        );
        World { dir }
    }

    fn config_file(&self) -> PathBuf {
        self.dir.0.join("cfg").join("rowser").join("config.toml")
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.config_file()).unwrap_or_default()
    }

    fn write_config(&self, text: &str) {
        std::fs::create_dir_all(self.config_file().parent().unwrap()).unwrap();
        std::fs::write(self.config_file(), text).unwrap();
    }

    fn ssh_args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.0.join("args"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn run_dir(&self) -> PathBuf {
        self.dir.0.join("run")
    }

    /// Runs `rowser host <args>`; `found` is what `command -v rowser` gives.
    fn host(&self, args: &[&str], found: Option<&str>) -> Output {
        let d = &self.dir.0;
        let mut c = std::process::Command::new(ROWSER);
        c.arg("host")
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", d.join("bin").display()))
            .env("HOME", d)
            .env("XDG_CONFIG_HOME", d.join("cfg"))
            .env("ROWSER_RUNTIME_DIR", self.run_dir())
            .env_remove("FAKE_FOUND")
            .stdin(std::process::Stdio::null());
        if let Some(f) = found {
            c.env("FAKE_FOUND", f);
        }
        c.output().unwrap()
    }

    /// Starts a Daemon on the config file, with the fake `ssh`.
    fn start_daemon(&self) {
        let (direct, relay, control) = bind_sockets(&self.run_dir()).unwrap();
        let file = self.config_file();
        let config = DaemonConfig {
            ssh: self.dir.0.join("bin").join("ssh"),
            link_dir: self.run_dir(),
            hosts: rowser::config::load(&file).unwrap().hosts,
            config_file: Some(file),
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(
            config,
            RecordingOpener::default(),
            RecordingNotifier::default(),
        );
        tokio::spawn(daemon.serve(direct, relay, control));
    }

    fn socket(&self, alias: &str) -> PathBuf {
        self.dir.0.join("o").join(alias).join("outpost.sock")
    }
}

/// Runs `w.host(…)` off the tokio threads, so the Daemon can answer.
async fn host_async(w: &World, args: &[&str], found: Option<&str>) -> Output {
    tokio::task::block_in_place(|| w.host(args, found))
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

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

/// Runs `rowser host list` until its output contains `want`, at most 20 s.
async fn wait_list(w: &World, want: &str) -> String {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let out = text(&host_async(w, &["list"], None).await.stdout);
        if out.contains(want) || Instant::now() > end {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[test]
fn add_finds_rowser_and_saves_its_path() {
    let w = World::new("ha");
    w.write_config("# my rowser config\n\n[default]\nopen = \"system\"\n");
    let out = w.host(&["add", "g14"], Some("/home/u/bin/rowser"));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let file = w.config();
    assert!(file.starts_with("# my rowser config\n"), "{file}");
    let c = rowser::config::parse(&file).unwrap();
    assert_eq!(c.hosts.len(), 1);
    assert_eq!(c.hosts[0].name, "g14");
    assert_eq!(c.hosts[0].remote_path, "/home/u/bin/rowser");
    assert!(c.hosts[0].default_browser);
    let args = w.ssh_args();
    assert_eq!(args.len(), 1, "{args:?}");
    assert!(args[0].contains("g14") && args[0].contains("command -v rowser"));
    assert!(
        text(&out.stdout).contains("the daemon is not running"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn add_of_a_saved_host_is_idempotent() {
    let w = World::new("hi");
    let found = Some("/opt/rowser");
    assert!(w.host(&["add", "g14"], found).status.success());
    let first = w.config();
    let out = w.host(&["add", "g14"], found);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(w.config(), first);
}

#[test]
fn add_with_no_default_browser_records_it() {
    let w = World::new("hn");
    let out = w.host(&["add", "pi", "--no-default-browser"], Some("/opt/rowser"));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let c = rowser::config::parse(&w.config()).unwrap();
    assert!(!c.hosts[0].default_browser);
    assert!(w.config().contains("default_browser = false"));
}

#[test]
fn add_without_rowser_on_the_host_names_the_install_command() {
    let w = World::new("hm");
    let out = w.host(&["add", "g14"], None);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("rowser remote install g14"), "{err}");
    assert!(!w.config_file().exists());
}

#[test]
fn remove_and_list_work_without_a_daemon() {
    let w = World::new("hr");
    w.write_config(
        "# hosts\n\n[[host]]\nname = \"g14\"\nremote_path = \"/a\"\n\n\
         [[host]]\nname = \"pi\" # the pi\nremote_path = \"/b\"\n",
    );
    let out = w.host(&["list"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let list = text(&out.stdout);
    assert!(list.contains("g14") && list.contains("pi"), "{list}");
    assert!(list.contains("daemon not running"), "{list}");

    let out = w.host(&["remove", "g14"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let file = w.config();
    assert!(file.starts_with("# hosts\n"), "{file}");
    assert!(file.contains("# the pi"), "{file}");
    let c = rowser::config::parse(&file).unwrap();
    assert_eq!(c.hosts.len(), 1);
    assert_eq!(c.hosts[0].name, "pi");

    let out = w.host(&["remove", "g14"], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("g14"));
}

#[tokio::test(flavor = "multi_thread")]
async fn add_list_and_remove_with_a_running_daemon() {
    let w = World::new("hd");
    w.start_daemon();
    let out = host_async(&w, &["add", "g14"], Some(ROWSER)).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("reloaded"),
        "{}",
        text(&out.stdout)
    );
    wait_connected(&w.socket("g14")).await;
    let list = wait_list(&w, "connected").await;
    assert!(list.contains("g14  connected"), "{list}");
    assert!(list.contains("outpost 2."), "{list}");

    // The pid of this test's own Outpost, which the fake ssh wrote.
    let pid: i32 = std::fs::read_to_string(w.dir.0.join("pid-g14"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(pid > 1, "{pid}");
    // SAFETY: signal 0 only checks that the process exists.
    let alive = || unsafe { libc::kill(pid, 0) == 0 };
    let out = host_async(&w, &["remove", "g14"], None).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let end = Instant::now() + Duration::from_secs(20);
    while alive() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(), "the Link is still open");
    let list = text(&host_async(&w, &["list"], None).await.stdout);
    assert!(!list.contains("g14"), "{list}");
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_starts_a_master_and_the_link_connects_again() {
    let w = World::new("hc");
    std::fs::write(w.dir.0.join("deny"), "").unwrap();
    w.write_config(&format!(
        "[[host]]\nname = \"g14\"\nremote_path = \"{ROWSER}\"\n"
    ));
    w.start_daemon();
    let list = wait_list(&w, "needs login").await;
    assert!(list.contains("g14  needs login"), "{list}");

    let out = host_async(&w, &["connect", "g14"], None).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let master = w
        .ssh_args()
        .into_iter()
        .find(|a| a.contains("-N -f"))
        .expect("no master ssh");
    let cm = w.run_dir().join("cm-%C");
    assert_eq!(
        master,
        format!(
            "-o ControlMaster=yes -o ControlPersist=yes -o ControlPath=\"{}\" -N -f g14",
            cm.display()
        )
    );
    wait_connected(&w.socket("g14")).await;
    let list = wait_list(&w, "g14  connected").await;
    assert!(list.contains("g14  connected"), "{list}");
}

#[test]
fn connect_of_an_unknown_host_is_refused() {
    let w = World::new("hu");
    let out = w.host(&["connect", "nope"], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("nope"));
    assert!(w.ssh_args().is_empty());
}
