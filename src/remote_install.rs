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

/// The release asset `<base>/rowser-<asset>.tar.gz`: `curl` piped into
/// `tar -xzO`, read into memory. No temporary files.
pub struct ReleaseDownload {
    pub base_url: String,
}

impl BinarySource for ReleaseDownload {
    fn binary(&self, asset: &str) -> Result<Vec<u8>, String> {
        let name = format!("rowser-{asset}.tar.gz");
        let url = format!("{}/{name}", self.base_url.trim_end_matches('/'));
        let mut curl = Command::new("curl")
            .args(["-fsSL", "--"])
            .arg(&url)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        let body = curl.stdout.take().expect("piped stdout");
        let tar = Command::new("tar")
            .args(["-xzO", "-f", "-", "rowser"])
            .stdin(body)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let tar = match tar {
            Ok(tar) => tar,
            Err(e) => {
                let _ = curl.kill();
                let _ = curl.wait();
                return Err(format!("cannot run tar: {e}"));
            }
        };
        let tar_out = tar
            .wait_with_output()
            .map_err(|e| format!("cannot run tar: {e}"))?;
        let curl_out = curl
            .wait_with_output()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        if !curl_out.status.success() {
            return Err(format!(
                "cannot download {url}: {}",
                String::from_utf8_lossy(&curl_out.stderr).trim()
            ));
        }
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
/// `$HOME/.local/bin/rowser` and links the browser names to it. It writes
/// only under `$HOME`. No single quotes, so it fits in `sh -c '…'`.
pub fn remote_script() -> String {
    let mut s = String::from(
        "set -e; [ -n \"$HOME\" ] || { echo \"HOME is not set\" >&2; exit 1; }; \
         d=\"$HOME/.local/bin\"; mkdir -p \"$d\"; rm -f \"$d/.rowser.tmp\"; cat >\"$d/.rowser.tmp\"; \
         chmod 755 \"$d/.rowser.tmp\"; mv -f \"$d/.rowser.tmp\" \"$d/rowser\";",
    );
    for name in BROWSER_NAMES {
        s.push_str(&format!(" ln -sfn rowser \"$d/{name}\";"));
    }
    s
}

/// Runs the install against `host` with the `ssh` on `PATH`.
pub fn install(host: &str, source: &dyn BinarySource) -> Result<(), String> {
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
        .spawn()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    let mut stdin = ssh.stdin.take().expect("piped stdin");
    let written = stdin.write_all(&bin);
    drop(stdin);
    let status = ssh.wait().map_err(|e| format!("cannot run ssh: {e}"))?;
    if !status.success() {
        return Err(format!("copy to {host} failed ({status})"));
    }
    written.map_err(|e| format!("copy to {host} failed: {e}"))?;
    Ok(())
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
        Ok(()) => {
            println!(
                "rowser installed on {host} in ~/.local/bin (with {}).\n\
                 Add this line to your shell rc on {host}:\n\n    export BROWSER=rowser",
                BROWSER_NAMES.join(", ")
            );
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
        assert!(s.contains("ln -sfn rowser \"$d/xdg-open\""), "{s}");
        assert!(s.contains("ln -sfn rowser \"$d/www-browser\""), "{s}");
    }
}
