# Changelog

All notable changes to rowser are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rowser uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `rowser doctor` on the remote host checks that `~/.local/bin` comes before `/usr/bin` on
  `PATH`, that the browser symlinks and `BROWSER` resolve to rowser, and that a tunnel answers
  a ping; it shows the daemon's protocol version and any version skew, and exits 1 when a
  check fails. ([#11](https://github.com/jhnhnsn/rowser/issues/11))
- `rowser daemon install` runs the daemon at login: it writes a systemd user unit
  (`~/.config/systemd/user/rowser.service`) on Linux or a LaunchAgent
  (`~/Library/LaunchAgents/io.github.jhnhnsn.rowser.plist`) on macOS for the current `rowser`
  binary, and starts it. Daemon flags given before `install` (`--runtime-dir`,
  `--open-command`, `--confirm-hosts`, `--confirm-unverified`) go into the service, so
  `rowser daemon --confirm-hosts '*.prod' install` installs a daemon that confirms. Running it again rewrites the file and restarts the daemon.
  `rowser daemon uninstall` stops and removes it, and only prints a message when nothing is
  installed. ([#10](https://github.com/jhnhnsn/rowser/issues/10))
- OAuth `localhost` callbacks work: when `rowser open` finds callback ports, the daemon listens
  on the same ports on local loopback and a detached keeper carries each browser connection
  back to the remote port. `rowser open` still exits at once. The forward ends when the remote
  port stops listening, a dial fails, or after 10 minutes. A busy local port still opens the URL,
  with a warning in the notification and on stderr. With `--confirm-hosts`, the ports bind only
  after you click "Open". `rowser forward <port>` forwards a port by hand; from a host that
  matches `--confirm-hosts` it also waits for "Open" in a notification that names the host and
  the port, and other hosts show a notice with the port.
  ([#8](https://github.com/jhnhnsn/rowser/issues/8))
- `rowser daemon --confirm-hosts <pattern>` (glob, may repeat) makes URLs from matching hosts
  wait until you click "Open" in the notification (a dialog on macOS). Deny, or no answer within
  60 seconds, refuses the URL and `rowser open` exits 1 with the reason. Patterns match the Host
  alias from `rowser ssh`; add `--confirm-unverified` to also match the untrusted host name on
  the direct socket (`ssh_config` setups).
  ([#9](https://github.com/jhnhnsn/rowser/issues/9))
- `rowser ssh <ssh args…>` runs `ssh` with a per-session Tunnel to the daemon, passing every
  argument through unchanged, and stamps each URL with the Host alias you typed (from `ssh -G`,
  else the ssh arguments). It passes ssh's exit code through, removes its socket on exit, and
  warns and runs plain `ssh` when the daemon is not running or the Host alias cannot be found.
  ([#7](https://github.com/jhnhnsn/rowser/issues/7))
- `rowser open <url>` (and `xdg-open`, `sensible-browser`, `www-browser`) sends the URL to the
  daemon through a tunnel. It tries `ROWSER_SOCKET`, then your own `/tmp/rowser-*.sock` sockets,
  newest first, and removes stale ones. Without a working tunnel it prints the URL to open by
  hand and exits 1. ([#6](https://github.com/jhnhnsn/rowser/issues/6))
- `rowser daemon` runs the daemon in the foreground. It opens `http` and `https` URLs sent over
  its direct and relay sockets, shows a desktop notification naming the host, and limits each
  connection to 5 URLs per 10 seconds. `--runtime-dir` and `--open-command "<cmd> {url}"` set
  where the sockets live and how URLs open. ([#4](https://github.com/jhnhnsn/rowser/issues/4))
- Release binaries: each `v*` tag publishes `rowser-<target>.tar.gz` for
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin` and
  `x86_64-apple-darwin` on the GitHub Release, with a `SHA256SUMS` file. The Linux binaries are
  static. ([#12](https://github.com/jhnhnsn/rowser/issues/12))
- Callback port detection: rowser finds the loopback callback ports of a URL from its
  `redirect_uri`, `redirect_url` and `callback` parameters and its own loopback port, and on
  Linux from the loopback ports that the calling process tree listens on.
  ([#5](https://github.com/jhnhnsn/rowser/issues/5))
- The rowser wire protocol: newline-delimited JSON with a `major.minor` version handshake.
  Mismatched major versions fail with an error that names both versions and how to update.
  ([#3](https://github.com/jhnhnsn/rowser/issues/3))
- The `rowser` command with its subcommands (`daemon`, `ssh`, `open`, `forward`, `doctor`,
  `remote install`) as stubs. Invoked as `xdg-open`, `sensible-browser` or `www-browser`, it
  acts as `rowser open`. ([#2](https://github.com/jhnhnsn/rowser/issues/2))
