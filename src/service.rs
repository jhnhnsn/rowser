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
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let shown = format!("{program} {}", args.join(" "));
    match Command::new(program).args(args).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("`{shown}` failed ({s})")),
        Err(e) => Err(format!("cannot run `{shown}`: {e}")),
    }
}

/// Runs a program from `PATH` with its output discarded; ignores any failure.
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
    let path = launchd_plist_path()?;
    write_file(&path, &render_launchd_plist(exe, args))?;
    let domain = gui_domain();
    // Not loaded yet is fine: bootout only clears an earlier install.
    let _ = run(
        "launchctl",
        &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
    );
    // bootstrap fails with "5: Input/output error" while the old job stops.
    let plist = path.to_string_lossy().into_owned();
    let mut tries = 0;
    loop {
        match run("launchctl", &["bootstrap", &domain, &plist]) {
            Ok(()) => {
                return Ok(Installed {
                    path,
                    started: true,
                })
            }
            Err(_) if tries < 5 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(target_os = "macos")]
fn uninstall_inner() -> Result<Uninstalled, String> {
    let path = launchd_plist_path()?;
    let target = format!("{}/{LAUNCHD_LABEL}", gui_domain());
    let stop = ["bootout", target.as_str()];
    if !path.exists() {
        // The job can be loaded without its file: stop it, ignore errors.
        run_quiet("launchctl", &stop);
        return Ok(Uninstalled::NotInstalled(path));
    }
    if let Err(e) = run("launchctl", &stop) {
        warn(e);
    }
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    Ok(Uninstalled::Removed(path))
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
