//! `rowser daemon install|uninstall`: a systemd user unit (Linux) or a launchd
//! LaunchAgent (macOS) that runs `rowser daemon`.
//!
//! Shallow on purpose: rendering is pure and unit-tested; `systemctl` and
//! `launchctl` are run directly from `PATH`, with no port around them.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The systemd unit name.
pub const UNIT_NAME: &str = "rowser.service";
/// The launchd job label.
pub const LAUNCHD_LABEL: &str = "io.github.jhnhnsn.rowser";

/// Renders the systemd user unit that runs `<exe> daemon <args…>`; `args` are
/// the daemon flags the user gave to `rowser daemon … install`.
pub fn render_systemd_unit(exe: &Path, args: &[String]) -> String {
    let mut exec = format!("{} daemon", systemd_quote(&exe.to_string_lossy()));
    for arg in args {
        exec.push(' ');
        exec.push_str(&systemd_quote(arg));
    }
    format!(
        "[Unit]\n\
         Description=rowser daemon: open URLs from SSH sessions in the local browser\n\
         PartOf=graphical-session.target\n\
         After=graphical-session.target\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n"
    )
}

/// Renders the LaunchAgent plist that runs `<exe> daemon <args…>`.
pub fn render_launchd_plist(exe: &Path, args: &[String]) -> String {
    let mut program = String::new();
    for word in [exe.to_string_lossy().into_owned(), "daemon".into()]
        .iter()
        .chain(args)
    {
        program.push_str(&format!("\t\t<string>{}</string>\n", xml_escape(word)));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCHD_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{program}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#
    )
}

/// Quotes one systemd `ExecStart` word: double quotes, `\` and `"` escaped,
/// newline as `\n`, `%` doubled (specifier escape), `$` doubled (variable escape).
fn systemd_quote(word: &str) -> String {
    let mut out = String::from("\"");
    for c in word.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The unit path: `$XDG_CONFIG_HOME/systemd/user/rowser.service`
/// (default `~/.config`).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn systemd_unit_path() -> Result<PathBuf, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(config.join("systemd/user").join(UNIT_NAME))
}

/// The plist path: `~/Library/LaunchAgents/<label>.plist`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_plist_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

/// Runs a program from `PATH`; Err names the command and its failure.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let shown = format!("{program} {}", args.join(" "));
    match Command::new(program).args(args).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("`{shown}` failed ({s})")),
        Err(e) => Err(format!("cannot run `{shown}`: {e}")),
    }
}

/// Runs a program from `PATH` with its output discarded; ignores any failure.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn run_quiet(program: &str, args: &[&str]) {
    let _ = Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn fail(action: &str, msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("rowser daemon {action}: {msg}");
    ExitCode::from(1)
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// `rowser daemon install`: writes the service for the current binary and
/// (re)starts it. Running it again rewrites the file and restarts the service.
/// `args` are the daemon flags to carry into the service (see
/// `cli::install_args`).
pub fn install(args: &[String]) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail("install", format!("cannot find the rowser binary: {e}")),
    };
    match install_with(&exe, args) {
        Ok(Installed { path, started }) => {
            println!("rowser daemon installed: {}", path.display());
            if !started {
                println!(
                    "rowser daemon is not started: no graphical session is active; \
                     it starts with the next graphical session"
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail("install", e),
    }
}

struct Installed {
    path: PathBuf,
    /// False when the service waits for the next graphical session.
    started: bool,
}

/// `rowser daemon uninstall`: stops and removes the service. Not installed
/// is a no-op with a message.
pub fn uninstall() -> ExitCode {
    match uninstall_inner() {
        Ok(Uninstalled::Removed(path)) => {
            println!("rowser daemon uninstalled: removed {}", path.display());
            ExitCode::SUCCESS
        }
        Ok(Uninstalled::NotInstalled(path)) => {
            println!("rowser daemon is not installed (no {})", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => fail("uninstall", e),
    }
}

enum Uninstalled {
    Removed(PathBuf),
    NotInstalled(PathBuf),
}

fn warn(e: impl std::fmt::Display) {
    eprintln!("rowser daemon uninstall: warning: {e}");
}

#[cfg(target_os = "macos")]
fn gui_domain() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn install_with(exe: &Path, args: &[String]) -> Result<Installed, String> {
    launchd_install(
        "launchctl",
        &launchd_plist_path()?,
        &gui_domain(),
        exe,
        args,
        std::time::Duration::from_millis(300),
    )
}

#[cfg(target_os = "macos")]
fn uninstall_inner() -> Result<Uninstalled, String> {
    let target = format!("{}/{LAUNCHD_LABEL}", gui_domain());
    let (result, warnings) = launchd_uninstall("launchctl", &launchd_plist_path()?, &target);
    warnings.into_iter().for_each(warn);
    result
}

/// Runs `launchctl` with its output captured, so launchctl prints nothing
/// itself; Err carries the exit code and launchctl's stderr.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn run_launchctl(launchctl: &str, args: &[&str]) -> Result<(), (Option<i32>, String)> {
    let shown = format!("launchctl {}", args.join(" "));
    match Command::new(launchctl).args(args).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stderr).trim().to_owned();
            if text.is_empty() {
                text = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            }
            let msg = if text.is_empty() {
                format!("`{shown}` failed ({})", o.status)
            } else {
                format!("`{shown}` failed ({}): {text}", o.status)
            };
            Err((o.status.code(), msg))
        }
        Err(e) => Err((None, format!("cannot run `{shown}`: {e}"))),
    }
}

/// The launchd install: `launchctl` is the program to run (a path under test).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_install(
    launchctl: &str,
    path: &Path,
    domain: &str,
    exe: &Path,
    args: &[String],
    retry_delay: std::time::Duration,
) -> Result<Installed, String> {
    write_file(path, &render_launchd_plist(exe, args))?;
    // bootout only clears an earlier install. Not loaded is fine; another
    // error is shown only if bootstrap then fails, as a likely cause.
    let bootout_err = match run_launchctl(
        launchctl,
        &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
    ) {
        Err((code, e)) if !is_not_loaded(code) => Some(e),
        _ => None,
    };
    // bootstrap fails with "5: Input/output error" while the old job stops.
    let plist = path.to_string_lossy().into_owned();
    let mut tries = 0;
    loop {
        match run_launchctl(launchctl, &["bootstrap", domain, &plist]) {
            Ok(()) => {
                return Ok(Installed {
                    path: path.to_path_buf(),
                    started: true,
                })
            }
            Err(_) if tries < 5 => {
                tries += 1;
                std::thread::sleep(retry_delay);
            }
            Err((_, e)) => {
                return Err(match bootout_err {
                    Some(b) => format!("{e} (after an earlier error: {b})"),
                    None => e,
                })
            }
        }
    }
}

/// launchctl bootout exit codes for a job that is not loaded: 3 is ESRCH
/// ("No such process"); 113 is launchd's "Could not find specified service".
const NOT_LOADED: [i32; 2] = [3, 113];

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_not_loaded(code: Option<i32>) -> bool {
    code.is_some_and(|c| NOT_LOADED.contains(&c))
}

/// The launchd uninstall; returns the result and the warnings to show.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_uninstall(
    launchctl: &str,
    path: &Path,
    target: &str,
) -> (Result<Uninstalled, String>, Vec<String>) {
    let stopped = run_launchctl(launchctl, &["bootout", target]);
    if !path.exists() {
        // The job can be loaded without its file: stop it, ignore errors.
        return (
            Ok(Uninstalled::NotInstalled(path.to_path_buf())),
            Vec::new(),
        );
    }
    let warnings = match stopped {
        Err((code, e)) if !is_not_loaded(code) => vec![e],
        _ => Vec::new(),
    };
    let removed = std::fs::remove_file(path)
        .map(|()| Uninstalled::Removed(path.to_path_buf()))
        .map_err(|e| format!("cannot remove {}: {e}", path.display()));
    (removed, warnings)
}

#[cfg(not(target_os = "macos"))]
fn install_with(exe: &Path, args: &[String]) -> Result<Installed, String> {
    let path = systemd_unit_path()?;
    write_file(&path, &render_systemd_unit(exe, args))?;
    run("systemctl", &["--user", "daemon-reload"])?;
    // reenable, not enable: drops a default.target.wants link left by an
    // install from before the unit moved to graphical-session.target.
    run("systemctl", &["--user", "reenable", UNIT_NAME])?;
    // Outside a graphical session (an install over SSH) the user manager
    // has no DISPLAY: leave the start to the next graphical session.
    if !graphical_session_active() {
        return Ok(Installed {
            path,
            started: false,
        });
    }
    // restart, not start: a second install picks up a new binary path.
    run("systemctl", &["--user", "restart", UNIT_NAME])?;
    Ok(Installed {
        path,
        started: true,
    })
}

#[cfg(not(target_os = "macos"))]
fn graphical_session_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "graphical-session.target"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(not(target_os = "macos"))]
fn uninstall_inner() -> Result<Uninstalled, String> {
    let path = systemd_unit_path()?;
    let stop = ["--user", "disable", "--now", UNIT_NAME];
    if !path.exists() {
        // The unit can be loaded without its file: stop it, ignore errors.
        run_quiet("systemctl", &stop);
        return Ok(Uninstalled::NotInstalled(path));
    }
    if let Err(e) = run("systemctl", &stop) {
        warn(e);
    }
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    // The file is gone, so the uninstall is done even if the reload fails.
    if let Err(e) = run("systemctl", &["--user", "daemon-reload"]) {
        warn(e);
    }
    Ok(Uninstalled::Removed(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir that is removed on drop, so a failed test cleans up too.
    struct TmpDir(PathBuf);

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fake `launchctl` in a fresh temp dir. It logs each call to `log`,
    /// and for each `(subcommand, text, code)` in `fails` it prints `text`
    /// (to stdout when it starts with `stdout:`, else to stderr) and exits
    /// with `code`.
    fn fake_launchctl(tag: &str, fails: &[(&str, &str, i32)]) -> (PathBuf, TmpDir) {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("rowser-launchctl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let guard = TmpDir(dir.clone());
        let log = dir.join("log");
        let exe = dir.join("launchctl");
        let mut script = format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display());
        for (cmd, text, code) in fails {
            let (text, redirect) = match text.strip_prefix("stdout:") {
                Some(t) => (t, ""),
                None => (*text, " >&2"),
            };
            script.push_str(&format!(
                "if [ \"$1\" = '{cmd}' ]; then echo '{text}'{redirect}; exit {code}; fi\n"
            ));
        }
        script.push_str("exit 0\n");
        // fs::write closes its handle before it returns, so the script is
        // not busy when it runs.
        std::fs::write(&exe, script).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        (exe, guard)
    }

    fn install(exe: &Path, plist: &Path) -> Result<Installed, String> {
        launchd_install(
            exe.to_str().unwrap(),
            plist,
            "gui/501",
            Path::new("/bin/rowser"),
            &[],
            std::time::Duration::ZERO,
        )
    }

    #[test]
    fn launchd_install_ignores_a_bootout_of_a_job_that_is_not_loaded() {
        let (exe, tmp) = fake_launchctl(
            "first",
            &[("bootout", "Boot-out failed: 3: No such process", 3)],
        );
        let dir = &tmp.0;
        let plist = dir.join("agents/rowser.plist");
        let got = install(&exe, &plist);
        assert!(got.is_ok(), "{:?}", got.err());
        assert!(plist.exists());
        let calls = std::fs::read_to_string(dir.join("log")).unwrap();
        let want = format!(
            "bootout gui/501/{LAUNCHD_LABEL}\nbootstrap gui/501 {}\n",
            plist.display()
        );
        assert_eq!(calls, want);
    }

    #[test]
    fn launchd_install_reports_the_bootstrap_error_text() {
        let (exe, tmp) = fake_launchctl(
            "boot",
            &[("bootstrap", "Bootstrap failed: 5: Input/output error", 5)],
        );
        let plist = tmp.0.join("rowser.plist");
        let err = install(&exe, &plist)
            .err()
            .expect("bootstrap failure is an error");
        assert!(
            err.contains("Bootstrap failed: 5: Input/output error"),
            "{err}"
        );
    }

    #[test]
    fn launchd_install_error_uses_stdout_when_stderr_is_empty() {
        let (exe, tmp) = fake_launchctl("out", &[("bootstrap", "stdout:Load failed: 5", 5)]);
        let plist = tmp.0.join("rowser.plist");
        let err = install(&exe, &plist).err().expect("an error");
        assert!(err.contains("Load failed: 5"), "{err}");
    }

    #[test]
    fn launchd_install_failure_carries_an_earlier_bootout_error() {
        let (exe, tmp) = fake_launchctl(
            "both",
            &[
                ("bootout", "Boot-out failed: 1: Operation not permitted", 1),
                ("bootstrap", "Bootstrap failed: 5: Input/output error", 5),
            ],
        );
        let plist = tmp.0.join("rowser.plist");
        let err = install(&exe, &plist).err().expect("an error");
        assert!(err.contains("Input/output error"), "{err}");
        assert!(err.contains("Operation not permitted"), "{err}");
    }

    #[test]
    fn launchd_install_failure_omits_a_not_loaded_bootout() {
        let (exe, tmp) = fake_launchctl(
            "nl",
            &[
                ("bootout", "Boot-out failed: 3: No such process", 3),
                ("bootstrap", "Bootstrap failed: 5: Input/output error", 5),
            ],
        );
        let plist = tmp.0.join("rowser.plist");
        let err = install(&exe, &plist).err().expect("an error");
        assert!(!err.contains("No such process"), "{err}");
    }

    #[test]
    fn launchd_uninstall_of_a_job_that_is_not_loaded_removes_the_file_quietly() {
        for (tag, text, code) in [
            ("unl3", "Boot-out failed: 3: No such process", 3),
            (
                "unl113",
                "Boot-out failed: 113: Could not find specified service",
                113,
            ),
        ] {
            let (exe, tmp) = fake_launchctl(tag, &[("bootout", text, code)]);
            let plist = tmp.0.join("rowser.plist");
            std::fs::write(&plist, "x").unwrap();
            let (got, warnings) = launchd_uninstall(exe.to_str().unwrap(), &plist, "gui/501/x");
            assert!(matches!(got, Ok(Uninstalled::Removed(_))), "{tag}");
            assert!(warnings.is_empty(), "{tag}: {warnings:?}");
            assert!(!plist.exists(), "{tag}");
        }
    }

    #[test]
    fn launchd_uninstall_without_the_file_stops_the_job_and_reports_not_installed() {
        let (exe, tmp) = fake_launchctl(
            "none",
            &[("bootout", "Boot-out failed: 1: Operation not permitted", 1)],
        );
        let plist = tmp.0.join("rowser.plist");
        let (got, warnings) = launchd_uninstall(exe.to_str().unwrap(), &plist, "gui/501/x");
        assert!(matches!(got, Ok(Uninstalled::NotInstalled(_))));
        assert!(warnings.is_empty(), "{warnings:?}");
        let calls = std::fs::read_to_string(tmp.0.join("log")).unwrap();
        assert_eq!(calls, "bootout gui/501/x\n");
    }

    #[test]
    fn launchd_uninstall_warns_on_another_bootout_error() {
        let (exe, tmp) = fake_launchctl(
            "unw",
            &[("bootout", "Boot-out failed: 1: Operation not permitted", 1)],
        );
        let plist = tmp.0.join("rowser.plist");
        std::fs::write(&plist, "x").unwrap();
        let (got, warnings) = launchd_uninstall(exe.to_str().unwrap(), &plist, "gui/501/x");
        assert!(matches!(got, Ok(Uninstalled::Removed(_))));
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("Operation not permitted"),
            "{warnings:?}"
        );
    }

    #[test]
    fn systemd_unit_runs_the_binary_as_daemon() {
        let unit = render_systemd_unit(Path::new("/home/u/.cargo/bin/rowser"), &[]);
        assert_eq!(
            unit,
            "[Unit]
Description=rowser daemon: open URLs from SSH sessions in the local browser
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=\"/home/u/.cargo/bin/rowser\" daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
"
        );
    }

    #[test]
    fn systemd_unit_escapes_spaces_quotes_percent_and_dollar() {
        let unit = render_systemd_unit(Path::new("/opt/my dir/50%\"$x\\/rowser"), &[]);
        assert!(
            unit.contains("ExecStart=\"/opt/my dir/50%%\\\"$$x\\\\/rowser\" daemon\n"),
            "{unit}"
        );
    }

    #[test]
    fn launchd_plist_runs_the_binary_as_daemon() {
        let plist = render_launchd_plist(Path::new("/usr/local/bin/rowser"), &[]);
        assert_eq!(
            plist,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
\t<key>Label</key>
\t<string>io.github.jhnhnsn.rowser</string>
\t<key>ProgramArguments</key>
\t<array>
\t\t<string>/usr/local/bin/rowser</string>
\t\t<string>daemon</string>
\t</array>
\t<key>RunAtLoad</key>
\t<true/>
\t<key>KeepAlive</key>
\t<dict>
\t\t<key>SuccessfulExit</key>
\t\t<false/>
\t</dict>
\t<key>ProcessType</key>
\t<string>Interactive</string>
</dict>
</plist>
"
        );
    }

    #[test]
    fn systemd_unit_carries_the_daemon_flags() {
        let args: Vec<String> = [
            "--open-command",
            "firefox --new-tab {url}",
            "--confirm-hosts",
            "*.prod",
            "--confirm-unverified",
        ]
        .map(String::from)
        .into();
        let unit = render_systemd_unit(Path::new("/usr/bin/rowser"), &args);
        assert!(
            unit.contains(
                "ExecStart=\"/usr/bin/rowser\" daemon \"--open-command\" \
                 \"firefox --new-tab {url}\" \"--confirm-hosts\" \"*.prod\" \
                 \"--confirm-unverified\"\n"
            ),
            "{unit}"
        );
    }

    #[test]
    fn systemd_unit_escapes_a_newline_in_a_flag() {
        let unit = render_systemd_unit(Path::new("/r"), &["a\nb".to_string()]);
        assert!(
            unit.contains("ExecStart=\"/r\" daemon \"a\\nb\"\n"),
            "{unit}"
        );
    }

    #[test]
    fn launchd_plist_carries_the_daemon_flags() {
        let args: Vec<String> = ["--open-command", "open -a \"Safari\" {url} && x"]
            .map(String::from)
            .into();
        let plist = render_launchd_plist(Path::new("/usr/local/bin/rowser"), &args);
        assert!(
            plist.contains(
                "\t\t<string>/usr/local/bin/rowser</string>\n\
                 \t\t<string>daemon</string>\n\
                 \t\t<string>--open-command</string>\n\
                 \t\t<string>open -a &quot;Safari&quot; {url} &amp;&amp; x</string>\n\
                 \t</array>\n"
            ),
            "{plist}"
        );
    }

    #[test]
    fn launchd_plist_escapes_xml() {
        let plist = render_launchd_plist(Path::new("/Users/a&b/<x>/rowser"), &[]);
        assert!(
            plist.contains("<string>/Users/a&amp;b/&lt;x&gt;/rowser</string>"),
            "{plist}"
        );
    }
}
