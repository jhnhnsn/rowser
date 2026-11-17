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

/// Renders the systemd user unit that runs `<exe> daemon`.
pub fn render_systemd_unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=rowser daemon: open URLs from SSH sessions in the local browser\n\
         \n\
         [Service]\n\
         ExecStart={} daemon\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        systemd_quote(&exe.to_string_lossy())
    )
}

/// Renders the LaunchAgent plist that runs `<exe> daemon`.
pub fn render_launchd_plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCHD_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>daemon</string>
	</array>
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
"#,
        xml_escape(&exe.to_string_lossy())
    )
}

/// Quotes one systemd `ExecStart` word: double quotes, `\` and `"` escaped,
/// `%` doubled (specifier escape), `$` doubled (variable escape).
fn systemd_quote(word: &str) -> String {
    let mut out = String::from("\"");
    for c in word.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
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
pub fn install() -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail("install", format!("cannot find the rowser binary: {e}")),
    };
    match install_with(&exe) {
        Ok(path) => {
            println!("rowser daemon installed: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => fail("install", e),
    }
}

/// `rowser daemon uninstall`: stops and removes the service. Not installed
/// is a no-op with a message.
pub fn uninstall() -> ExitCode {
    match uninstall_inner() {
        Ok(Some(path)) => {
            println!("rowser daemon uninstalled: removed {}", path.display());
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => fail("uninstall", e),
    }
}

#[cfg(target_os = "macos")]
fn gui_domain() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn install_with(exe: &Path) -> Result<PathBuf, String> {
    let path = launchd_plist_path()?;
    write_file(&path, &render_launchd_plist(exe))?;
    let domain = gui_domain();
    // Not loaded yet is fine: bootout only clears an earlier install.
    let _ = run(
        "launchctl",
        &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
    );
    run(
        "launchctl",
        &["bootstrap", &domain, &path.to_string_lossy()],
    )?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn uninstall_inner() -> Result<Option<PathBuf>, String> {
    let path = launchd_plist_path()?;
    if !path.exists() {
        println!("rowser daemon is not installed (no {})", path.display());
        return Ok(None);
    }
    if let Err(e) = run(
        "launchctl",
        &["bootout", &format!("{}/{LAUNCHD_LABEL}", gui_domain())],
    ) {
        eprintln!("rowser daemon uninstall: warning: {e}");
    }
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    Ok(Some(path))
}

#[cfg(not(target_os = "macos"))]
fn install_with(exe: &Path) -> Result<PathBuf, String> {
    let path = systemd_unit_path()?;
    write_file(&path, &render_systemd_unit(exe))?;
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", UNIT_NAME])?;
    // restart, not start: a second install picks up a new binary path.
    run("systemctl", &["--user", "restart", UNIT_NAME])?;
    Ok(path)
}

#[cfg(not(target_os = "macos"))]
fn uninstall_inner() -> Result<Option<PathBuf>, String> {
    let path = systemd_unit_path()?;
    if !path.exists() {
        println!("rowser daemon is not installed (no {})", path.display());
        return Ok(None);
    }
    if let Err(e) = run("systemctl", &["--user", "disable", "--now", UNIT_NAME]) {
        eprintln!("rowser daemon uninstall: warning: {e}");
    }
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    run("systemctl", &["--user", "daemon-reload"])?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_runs_the_binary_as_daemon() {
        let unit = render_systemd_unit(Path::new("/home/u/.cargo/bin/rowser"));
        assert_eq!(
            unit,
            "[Unit]
Description=rowser daemon: open URLs from SSH sessions in the local browser

[Service]
ExecStart=\"/home/u/.cargo/bin/rowser\" daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
"
        );
    }

    #[test]
    fn systemd_unit_escapes_spaces_quotes_percent_and_dollar() {
        let unit = render_systemd_unit(Path::new("/opt/my dir/50%\"$x\\/rowser"));
        assert!(
            unit.contains("ExecStart=\"/opt/my dir/50%%\\\"$$x\\\\/rowser\" daemon\n"),
            "{unit}"
        );
    }

    #[test]
    fn launchd_plist_runs_the_binary_as_daemon() {
        let plist = render_launchd_plist(Path::new("/usr/local/bin/rowser"));
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
    fn launchd_plist_escapes_xml() {
        let plist = render_launchd_plist(Path::new("/Users/a&b/<x>/rowser"));
        assert!(
            plist.contains("<string>/Users/a&amp;b/&lt;x&gt;/rowser</string>"),
            "{plist}"
        );
    }
}
