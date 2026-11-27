//! `rowser host add|remove|list|connect`, run as the real binary with a
//! fake `ssh` first on `PATH`. The fake `ssh` never reaches a host: it logs
//! its arguments, answers `command -v rowser` from `FAKE_FOUND`, returns at
//! once for the master connection (`-N -f`), and runs a Link's remote
//! command (`__outpost`) on this machine with HOME in the temp dir.
//! HOME, XDG_CONFIG_HOME and ROWSER_RUNTIME_DIR all point into the temp dir.

mod common;
#[path = "common/script.rs"]
mod script;
#[path = "common/toolbox.rs"]
mod toolbox;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use common::TmpDir;
use rowser::daemon::testing::{RecordingNotifier, RecordingOpener};
use rowser::daemon::{bind_sockets, Daemon, DaemonConfig};
use rowser::protocol::{Conn, Hello};
use script::write_script;
use tokio::net::UnixStream;
use toolbox::remote_toolbox;

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
                 *'-- g14 uname -sm'|*'-- pi uname -sm') echo 'Linux x86_64'; exit 0;;\n\
                 *\"-- g14 sh -c '\"*|*\"-- pi sh -c '\"*)\n\
                 eval \"cmd=\\${{$#}}\"\n\
                 mkdir -p {remote}\n\
                 HOME={remote} PATH={toolbox} exec /bin/sh -c \"$cmd\";;\n\
                 *'command -v rowser'*) [ -n \"$FAKE_FOUND\" ] || exit 1; echo \"$FAKE_FOUND\";\n\
                 [ -n \"$FAKE_VERSION\" ] && echo \"rowser $FAKE_VERSION\"; exit 0;;\n\
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
                remote = d.join("remote").display(),
                toolbox = remote_toolbox(d).display(),
            ),
        );
        std::fs::write(d.join("rowser-built"), "LOCAL").unwrap();
        World { dir }
    }

    /// The remote HOME that installs write into.
    fn remote_home(&self) -> PathBuf {
        self.dir.0.join("remote")
    }

    /// Whether an install ran (`uname -sm` is its first step).
    fn installed(&self) -> bool {
        self.ssh_args().iter().any(|a| a.ends_with("uname -sm"))
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
        self.host_in(args, found, &self.run_dir())
    }

    /// `host`, with `ROWSER_RUNTIME_DIR` set to `run_dir`.
    fn host_in(&self, args: &[&str], found: Option<&str>, run_dir: &Path) -> Output {
        self.host_with(args, found, Some(env!("CARGO_PKG_VERSION")), run_dir)
    }

    /// `host_in`; `version` is what the remote `rowser --version` gives.
    fn host_with(
        &self,
        args: &[&str],
        found: Option<&str>,
        version: Option<&str>,
        run_dir: &Path,
    ) -> Output {
        let d = &self.dir.0;
        let mut c = std::process::Command::new(ROWSER);
        c.arg("host")
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", d.join("bin").display()))
            .env("HOME", d)
            .env("XDG_CONFIG_HOME", d.join("cfg"))
            .env("ROWSER_RUNTIME_DIR", run_dir)
            .env("ROWSER_TEST_LOCAL_BUILD", d.join("rowser-built"))
            .env_remove("FAKE_FOUND")
            .env_remove("FAKE_VERSION")
            .stdin(std::process::Stdio::null());
        if let Some(v) = version {
            c.env("FAKE_VERSION", v);
        }
        if let Some(f) = found {
            c.env("FAKE_FOUND", f);
        }
        c.output().unwrap()
    }

    /// Starts a Daemon on the config file, with the fake `ssh`.
    fn start_daemon(&self) {
        let control = bind_sockets(&self.run_dir()).unwrap();
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
        tokio::spawn(daemon.serve(control));
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
fn a_second_add_updates_the_path_and_default_browser() {
    let w = World::new("h2");
    assert!(w
        .host(&["add", "g14"], Some("/opt/rowser"))
        .status
        .success());
    let out = w.host(&["add", "g14", "--no-default-browser"], Some("/new/rowser"));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let c = rowser::config::parse(&w.config()).unwrap();
    assert_eq!(c.hosts.len(), 1);
    assert_eq!(c.hosts[0].remote_path, "/new/rowser");
    assert!(!c.hosts[0].default_browser);

    // A plain add means default_browser true (spec).
    assert!(w
        .host(&["add", "g14"], Some("/new/rowser"))
        .status
        .success());
    let c = rowser::config::parse(&w.config()).unwrap();
    assert_eq!(c.hosts.len(), 1);
    assert!(c.hosts[0].default_browser);
}

#[test]
fn add_reads_the_last_line_of_a_chatty_login_shell() {
    let w = World::new("hl");
    let out = w.host(&["add", "g14"], Some("Welcome to g14\n\n/opt/rowser\n"));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let c = rowser::config::parse(&w.config()).unwrap();
    assert_eq!(c.hosts[0].remote_path, "/opt/rowser");
}

#[test]
fn list_with_no_saved_hosts_says_so() {
    let w = World::new("he");
    let out = w.host(&["list"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "no Saved hosts\n");
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
fn add_without_rowser_on_the_host_installs_it_first() {
    let w = World::new("hm");
    let out = w.host(&["add", "g14"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(w.installed());
    let home = w.remote_home();
    assert_eq!(
        std::fs::read_to_string(home.join(".local/bin/rowser")).unwrap(),
        "LOCAL"
    );
    assert!(std::fs::symlink_metadata(home.join(".local/bin/xdg-open")).is_ok());
    let c = rowser::config::parse(&w.config()).unwrap();
    assert_eq!(c.hosts[0].remote_path, "~/.local/bin/rowser");
    assert!(c.hosts[0].default_browser);
    let stdout = text(&out.stdout);
    assert!(stdout.contains("rowser installed on g14"), "{stdout}");
}

#[test]
fn add_with_another_version_on_the_host_installs_this_one() {
    let w = World::new("hv");
    let out = w.host_with(
        &["add", "g14"],
        Some("/opt/rowser"),
        Some("0.9.0"),
        &w.run_dir(),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(w.installed());
    let c = rowser::config::parse(&w.config()).unwrap();
    assert_eq!(c.hosts[0].remote_path, "~/.local/bin/rowser");

    // No version answer (an old rowser) also installs.
    let w = World::new("hw");
    let out = w.host_with(&["add", "g14"], Some("/opt/rowser"), None, &w.run_dir());
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(w.installed());
}

#[test]
fn add_with_no_default_browser_installs_only_the_binary() {
    let w = World::new("hb");
    let out = w.host(&["add", "pi", "--no-default-browser"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let home = w.remote_home();
    assert!(home.join(".local/bin/rowser").is_file());
    assert!(std::fs::symlink_metadata(home.join(".local/bin/xdg-open")).is_err());
    assert!(!home.join(".config/environment.d/rowser.conf").exists());
    assert!(home
        .join(rowser::remote_install::NO_DEFAULT_BROWSER_MARKER)
        .is_file());
    let c = rowser::config::parse(&w.config()).unwrap();
    assert!(!c.hosts[0].default_browser);
}

#[test]
fn a_failed_install_saves_nothing() {
    let w = World::new("hf");
    std::fs::remove_file(w.dir.0.join("rowser-built")).unwrap();
    let out = w.host(&["add", "g14"], None);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("rowser host add: "), "{err}");
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
    let outpost = format!("outpost {}", env!("CARGO_PKG_VERSION"));
    assert!(list.trim_end().ends_with(&outpost), "{list}");

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

#[tokio::test(flavor = "multi_thread")]
async fn connect_to_a_host_the_daemon_has_not_loaded_asks_for_reload() {
    let w = World::new("hk");
    w.start_daemon();
    w.write_config("[[host]]\nname = \"g14\"\nremote_path = \"/a\"\n");
    let out = host_async(&w, &["connect", "g14"], None).await;
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    let err = text(&out.stderr);
    assert!(err.contains("rowser reload"), "{err}");
}

#[test]
fn connect_quotes_a_runtime_dir_with_a_space() {
    let w = World::new("hs");
    w.write_config("[[host]]\nname = \"g14\"\nremote_path = \"/a\"\n");
    let run = w.dir.0.join("Application Support");
    let out = w.host_in(&["connect", "g14"], None, &run);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let master = w
        .ssh_args()
        .into_iter()
        .find(|a| a.contains("-N -f"))
        .expect("no master ssh");
    assert_eq!(
        master,
        format!(
            "-o ControlMaster=yes -o ControlPersist=yes -o ControlPath=\"{}/Application Support/cm-%C\" -N -f g14",
            w.dir.0.display()
        )
    );
}

#[test]
fn connect_of_an_unknown_host_is_refused() {
    let w = World::new("hu");
    let out = w.host(&["connect", "nope"], None);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("nope"));
    assert!(w.ssh_args().is_empty());
}
