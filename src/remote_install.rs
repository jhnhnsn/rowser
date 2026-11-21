//! `rowser remote install <host>` (run locally).
//!
//! Shallow on purpose: `ssh`, `curl` and `tar` run directly from `PATH`. The
//! one seam is [`BinarySource`]: a release download or a local build.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use crate::platform::BROWSER_NAMES;

/// The release download base for this version.
pub fn default_release_url() -> String {
    format!(
        "https://github.com/jhnhnsn/rowser/releases/download/v{}",
        env!("CARGO_PKG_VERSION")
    )
}

/// The release asset (target triple) for a remote `uname -sm` output.
pub fn asset_for(uname: &str) -> Result<&'static str, String> {
    let uname = uname.trim();
    let mut words = uname.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("Linux"), Some("x86_64"), None) => Ok("x86_64-unknown-linux-musl"),
        (Some("Linux"), Some("aarch64" | "arm64"), None) => Ok("aarch64-unknown-linux-musl"),
        _ => Err(format!(
            "unsupported system on the remote host `{uname}`: rowser runs on Linux x86_64 \
             and aarch64 remote hosts"
        )),
    }
}

/// Gives the rowser binary for one release asset.
pub trait BinarySource {
    /// Returns the bytes of the binary for `asset`. Writes no files.
    fn binary(&self, asset: &str) -> Result<Vec<u8>, String>;
}

/// A binary the user built (`--local-build <path>`).
pub struct LocalBuild(pub PathBuf);

impl BinarySource for LocalBuild {
    fn binary(&self, _asset: &str) -> Result<Vec<u8>, String> {
        if !self.0.is_file() {
            return Err(format!("--local-build {} is not a file", self.0.display()));
        }
        std::fs::read(&self.0).map_err(|e| format!("cannot read {}: {e}", self.0.display()))
    }
}

/// The release asset `<base>/rowser-<asset>.tar.gz`, checked against
/// `<base>/SHA256SUMS`, then unpacked with `tar -xzO`. All in memory; no
/// temporary files.
pub struct ReleaseDownload {
    pub base_url: String,
}

/// `curl` one URL into memory.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--"])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cannot download {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Refuses `body` unless `sums` (`sha256sum` output) has a matching entry for `name`.
fn check_sha256(sums: &str, name: &str, body: &[u8]) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let want = sums
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .map(|(hash, file)| (hash, file.trim_start()))
        .find(|(_, file)| file.strip_prefix('*').unwrap_or(file) == name)
        .map(|(hash, _)| hash.to_ascii_lowercase())
        .ok_or_else(|| format!("SHA256SUMS has no entry for {name}"))?;
    let got: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != want {
        return Err(format!(
            "{name} does not match SHA256SUMS (expected {want}, got {got})"
        ));
    }
    Ok(())
}

impl BinarySource for ReleaseDownload {
    fn binary(&self, asset: &str) -> Result<Vec<u8>, String> {
        let base = self.base_url.trim_end_matches('/');
        let name = format!("rowser-{asset}.tar.gz");
        let body = download(&format!("{base}/{name}"))?;
        let sums = download(&format!("{base}/SHA256SUMS"))?;
        check_sha256(&String::from_utf8_lossy(&sums), &name, &body)?;
        let mut tar = Command::new("tar")
            .args(["-xzO", "-f", "-", "rowser"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run tar: {e}"))?;
        let mut stdin = tar.stdin.take().expect("piped stdin");
        let writer = std::thread::spawn(move || stdin.write_all(&body));
        let tar_out = tar
            .wait_with_output()
            .map_err(|e| format!("cannot run tar: {e}"))?;
        // A failed write (e.g. tar exits early) is safe to ignore: tar then
        // fails or gives no output, and the check below refuses that.
        let _ = writer.join();
        if !tar_out.status.success() || tar_out.stdout.is_empty() {
            return Err(format!(
                "{name} does not hold a rowser binary: {}",
                String::from_utf8_lossy(&tar_out.stderr).trim()
            ));
        }
        Ok(tar_out.stdout)
    }
}

/// The remote `sh` script: reads the binary on stdin into
/// `$HOME/.local/bin/rowser`, links the browser names to it, and writes
/// `~/.config/environment.d/rowser.conf` (see [`environment_d_script`]). It
/// writes only under `$HOME`. No single quotes, so it fits in `sh -c '…'`.
pub fn remote_script() -> String {
    let mut s = String::from(
        "set -e; [ -n \"$HOME\" ] || { echo \"HOME is not set\" >&2; exit 1; }; \
         d=\"$HOME/.local/bin\"; mkdir -p \"$d\"; rm -f \"$d/.rowser.tmp\"; cat >\"$d/.rowser.tmp\"; \
         chmod 755 \"$d/.rowser.tmp\"; mv -f \"$d/.rowser.tmp\" \"$d/rowser\";",
    );
    for name in BROWSER_NAMES {
        s.push_str(&format!(" ln -sfn rowser \"$d/{name}\";"));
    }
    s.push_str(&environment_d_script());
    s.push_str(&desktop_script());
    s
}

/// What the remote script prints on stdout when it wrote `rowser.desktop`.
const DESKTOP_WRITTEN: &str = "rowser-desktop: written";
/// What the remote script prints on stdout when `xdg-mime` made rowser the default.
const DEFAULT_BY_XDG_MIME: &str = "rowser-default: xdg-mime";
/// What the remote script prints on stdout when it wrote the default to `mimeapps.list`.
const DEFAULT_BY_MIMEAPPS: &str = "rowser-default: mimeapps.list";

/// The `awk` program that sets `$h1` and `$h2` under `$sec` (`[Default
/// Applications]`) in a `mimeapps.list`: it drops the old `http` and `https`
/// entries of that section, adds rowser's at the section end (or adds the
/// section at the end of the file), and keeps every other line. No quotes and
/// `$` escaped for `sh` double quotes.
const MIMEAPPS_AWK: &str = "/^\\[/ {if(s&&!d){print h1;print h2;d=1} s=(\\$0==sec); print; next} \
     s&&/^x-scheme-handler\\/https?[ \\t]*=/ {next} {print} \
     END{if(!d){if(!s){if(NR>0)print nl; print sec} print h1; print h2}}";

/// The `sh` part that turns the path in `$x` into a quoted `Exec` argument, as
/// the Desktop Entry spec says: `"`, `` ` ``, `$` and `\` get a backslash, `%`
/// becomes `%%`, and then each backslash is doubled (the string escape rule
/// applies before the quote rule). No single quotes and no `\\`: a fish login
/// shell on the Remote host folds `\\` to `\` inside `'…'`, so `$b` holds the
/// backslash.
const EXEC_QUOTE: &str = r#"b=$(printf "\134"); r="$x"; x=; while [ -n "$r" ]; do c="${r%"${r#?}"}"; r="${r#?}"; case "$c" in "$b") x="$x$b$b$b$b";; \"|\`|\$) x="$x$b$b$c";; %) x="$x%%";; *) x="$x$c";; esac; done; x="\"$x\"";"#;

/// The `sh` part that writes `~/.local/share/applications/rowser.desktop` and
/// makes it the default for `http` and `https`: with `xdg-mime` when the
/// Remote host has it, else in `~/.config/mimeapps.list` (a symlink there is
/// kept, with a warning). Each failure is a warning, as in [`environment_d_script`].
fn desktop_script() -> String {
    format!(
        " a=\"$HOME/.local/share/applications\"; t=\"$a/rowser.desktop\"; x=\"$HOME/.local/bin/rowser\"; \
         {EXEC_QUOTE} \
         if mkdir -p \"$a\" && rm -f \"$t.tmp\" && printf \"%s\\n\" \"[Desktop Entry]\" \"Type=Application\" \
         \"Name=rowser\" \"Comment=Open URLs in the browser on your local machine\" \"Exec=$x open %u\" \
         \"NoDisplay=true\" \"MimeType=x-scheme-handler/http;x-scheme-handler/https;\" >\"$t.tmp\" \
         && mv -f \"$t.tmp\" \"$t\"; then echo \"{DESKTOP_WRITTEN}\"; \
         if command -v xdg-mime >/dev/null 2>&1; then \
         if xdg-mime default rowser.desktop x-scheme-handler/http x-scheme-handler/https; then \
         echo \"{DEFAULT_BY_XDG_MIME}\"; \
         else echo \"rowser: warning: xdg-mime failed: rowser is not the default browser.\" >&2; fi; \
         else m=\"$HOME/.config/mimeapps.list\"; \
         if [ -L \"$m\" ]; then echo \"rowser: warning: kept ~/.config/mimeapps.list: it is a symlink. \
         Add x-scheme-handler/http=rowser.desktop and x-scheme-handler/https=rowser.desktop \
         under [Default Applications] in it.\" >&2; \
         else src=\"$m\"; [ -e \"$m\" ] || src=/dev/null; \
         if mkdir -p \"$HOME/.config\" && rm -f \"$m.tmp\" && awk -v sec=\"[Default Applications]\" -v nl= \
         -v h1=x-scheme-handler/http=rowser.desktop -v h2=x-scheme-handler/https=rowser.desktop \
         \"{MIMEAPPS_AWK}\" \"$src\" >\"$m.tmp\" && mv -f \"$m.tmp\" \"$m\"; then echo \"{DEFAULT_BY_MIMEAPPS}\"; \
         else rm -f \"$m.tmp\" 2>/dev/null || true; \
         echo \"rowser: warning: cannot write ~/.config/mimeapps.list: rowser is not the default browser.\" >&2; fi; fi; fi; \
         else rm -f \"$t.tmp\" 2>/dev/null || true; \
         echo \"rowser: warning: cannot write ~/.local/share/applications/rowser.desktop: rowser is not the default browser.\" >&2; fi;"
    )
}

/// How rowser became the default for `http` and `https` links.
#[derive(Debug, PartialEq, Eq)]
pub enum DefaultBy {
    XdgMime,
    MimeappsList,
}

use crate::platform::ENVIRONMENT_D_MARK;

/// The whole `~/.config/environment.d/rowser.conf` rowser writes: the mark,
/// then `BROWSER` and `PATH`. `$HOME` and `$PATH` are literal, for
/// environment.d to expand.
pub const ENVIRONMENT_D_CONTENT: &str =
    "# Written by rowser remote install; rowser overwrites this file.\n\
     BROWSER=rowser\n\
     PATH=$HOME/.local/bin:$PATH\n";

/// What the remote script prints on stdout when it wrote the `environment.d` file.
const ENVIRONMENT_D_WRITTEN: &str = "rowser-environment.d: written";

/// The `sh` part that writes `~/.config/environment.d/rowser.conf` (for
/// systemd user services), unless a file without [`ENVIRONMENT_D_MARK`] is
/// there: then it keeps it and warns. A failed write is a warning too (the
/// binary is in place by then), so it runs as one `&&` chain that `set -e`
/// does not abort.
fn environment_d_script() -> String {
    let lines: Vec<String> = ENVIRONMENT_D_CONTENT
        .lines()
        .map(|l| format!("\"{}\"", l.replace('$', "\\$")))
        .collect();
    let lines = lines.join(" ");
    // On success the script echoes ENVIRONMENT_D_WRITTEN on stdout; `install`
    // reads it into `Installed::environment_d`.
    format!(
        " e=\"$HOME/.config/environment.d\"; f=\"$e/rowser.conf\"; \
         if {{ [ ! -e \"$f\" ] && [ ! -L \"$f\" ]; }} || [ \"$(head -n 1 \"$f\" 2>/dev/null)\" = \"{ENVIRONMENT_D_MARK}\" ]; then \
         if mkdir -p \"$e\" && rm -f \"$f.tmp\" && printf \"%s\\n\" {lines} >\"$f.tmp\" && mv -f \"$f.tmp\" \"$f\"; then \
         echo \"{ENVIRONMENT_D_WRITTEN}\"; \
         else rm -f \"$f.tmp\" 2>/dev/null || true; \
         echo \"rowser: warning: cannot write ~/.config/environment.d/rowser.conf: systemd user services do not get BROWSER=rowser.\" >&2; fi; \
         else echo \"rowser: warning: kept ~/.config/environment.d/rowser.conf: not written by rowser. \
         Add BROWSER=rowser and PATH=\\$HOME/.local/bin:\\$PATH to it for systemd user services.\" >&2; fi;"
    )
}

/// What an install changed on the Remote host.
#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    /// rowser wrote `~/.config/environment.d/rowser.conf` (false: the user's own file was kept).
    pub environment_d: bool,
    /// rowser wrote `~/.local/share/applications/rowser.desktop`.
    pub desktop: bool,
    /// How rowser became the default for `http` and `https` (none: it did not).
    pub default_by: Option<DefaultBy>,
}

/// Runs the install against `host` with the `ssh` on `PATH`.
pub fn install(host: &str, source: &dyn BinarySource) -> Result<Installed, String> {
    let out = Command::new("ssh")
        .args(["--", host, "uname -sm"])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    if !out.status.success() {
        return Err(format!("`ssh {host} uname -sm` failed ({})", out.status));
    }
    let asset = asset_for(&String::from_utf8_lossy(&out.stdout))?;

    let bin = source.binary(asset)?;
    let mut ssh = Command::new("ssh")
        .args(["--", host, &format!("sh -c '{}'", remote_script())])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    let mut stdin = ssh.stdin.take().expect("piped stdin");
    let written = stdin.write_all(&bin);
    drop(stdin);
    let out = ssh
        .wait_with_output()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    if !out.status.success() {
        return Err(format!("copy to {host} failed ({})", out.status));
    }
    written.map_err(|e| format!("copy to {host} failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let has = |mark: &str| stdout.lines().any(|l| l.trim() == mark);
    let default_by = if has(DEFAULT_BY_XDG_MIME) {
        Some(DefaultBy::XdgMime)
    } else if has(DEFAULT_BY_MIMEAPPS) {
        Some(DefaultBy::MimeappsList)
    } else {
        None
    };
    Ok(Installed {
        environment_d: has(ENVIRONMENT_D_WRITTEN),
        desktop: has(DESKTOP_WRITTEN),
        default_by,
    })
}

/// `rowser remote install <host>`.
pub fn install_main(
    host: &str,
    local_build: Option<PathBuf>,
    release_url: Option<String>,
) -> ExitCode {
    let source: Box<dyn BinarySource> = match local_build {
        Some(path) => Box::new(LocalBuild(path)),
        None => Box::new(ReleaseDownload {
            base_url: release_url.unwrap_or_else(default_release_url),
        }),
    };
    match install(host, source.as_ref()) {
        Ok(installed) => {
            println!(
                "rowser installed on {host} in ~/.local/bin (with {}).",
                BROWSER_NAMES.join(", ")
            );
            if installed.environment_d {
                println!(
                    "Wrote ~/.config/environment.d/rowser.conf on {host} (BROWSER and PATH for \
                     systemd user services); it applies from the next login."
                );
            }
            if installed.desktop {
                println!("Wrote ~/.local/share/applications/rowser.desktop on {host}.");
            }
            match installed.default_by {
                Some(DefaultBy::XdgMime) => println!(
                    "Made rowser.desktop the default for http and https links on {host} (with xdg-mime)."
                ),
                Some(DefaultBy::MimeappsList) => println!(
                    "Made rowser.desktop the default for http and https links on {host} \
                     (in ~/.config/mimeapps.list)."
                ),
                None => {}
            }
            println!("Add this line to your shell rc on {host}:\n\n    export BROWSER=rowser");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rowser remote install: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_x86_64_maps_to_the_musl_asset() {
        assert_eq!(asset_for("Linux x86_64\n"), Ok("x86_64-unknown-linux-musl"));
    }

    #[test]
    fn linux_aarch64_and_arm64_map_to_the_aarch64_asset() {
        assert_eq!(asset_for("Linux aarch64"), Ok("aarch64-unknown-linux-musl"));
        assert_eq!(asset_for("Linux arm64"), Ok("aarch64-unknown-linux-musl"));
    }

    #[test]
    fn other_systems_get_a_clear_error() {
        let err = asset_for("Darwin arm64").unwrap_err();
        assert!(
            err.contains("Darwin arm64") && err.contains("Linux"),
            "{err}"
        );
        let err = asset_for("Linux armv7l").unwrap_err();
        assert!(err.contains("armv7l"), "{err}");
    }

    #[test]
    fn remote_script_fits_in_single_quotes_and_stays_in_home() {
        let s = remote_script();
        assert!(!s.contains('\''), "{s}");
        // A fish login shell folds `\\` to `\` inside single quotes.
        assert!(!s.contains("\\\\"), "{s}");
        assert!(s.contains("ln -sfn rowser \"$d/xdg-open\""), "{s}");
        assert!(s.contains("ln -sfn rowser \"$d/www-browser\""), "{s}");
    }

    #[test]
    fn environment_d_script_writes_the_mark_browser_and_literal_home_and_path() {
        let s = environment_d_script();
        assert!(!s.contains('\''), "{s}");
        assert!(
            s.contains("\"# Written by rowser remote install; rowser overwrites this file.\""),
            "{s}"
        );
        assert!(s.contains("\"BROWSER=rowser\""), "{s}");
        assert!(s.contains("\"PATH=\\$HOME/.local/bin:\\$PATH\""), "{s}");
        assert!(ENVIRONMENT_D_CONTENT.starts_with(&format!("{ENVIRONMENT_D_MARK}\n")));
    }

    // SHA-256 of "abc" (FIPS 180-2 example).
    const ABC: &str = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";

    #[test]
    fn check_sha256_takes_the_binary_mode_star_and_an_uppercase_hash() {
        assert_eq!(
            check_sha256(&format!("{ABC} *a.tgz\n"), "a.tgz", b"abc"),
            Ok(())
        );
    }

    #[test]
    fn check_sha256_takes_only_one_star() {
        let err = check_sha256(&format!("{ABC} **a.tgz\n"), "a.tgz", b"abc").unwrap_err();
        assert!(err.contains("no entry for a.tgz"), "{err}");
    }
}
