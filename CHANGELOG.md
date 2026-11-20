# Changelog

All notable changes to rowser are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rowser uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `rowser browsers` lists the browsers found on this machine, with their short names (such as
  `chrome` or `firefox`) and Profiles; `--json` prints the same list as JSON. On macOS it looks
  for the known browsers in `/Applications` and `~/Applications`; on Linux it reads the
  `.desktop` files that handle `https`. ([#41](https://github.com/jhnhnsn/rowser/issues/41))
- `rowser remote install` now checks the downloaded release asset against the release's
  `SHA256SUMS` before it installs anything. A mismatch, a missing entry or a missing
  `SHA256SUMS` stops the install with a clear error and leaves the Remote host unchanged.
  `--local-build` skips the check. ([#25](https://github.com/jhnhnsn/rowser/issues/25))
- A user README: install from release binaries or `cargo install --git`, local and Remote host
  setup, `alias ssh='rowser ssh'`, the `ssh_config` alternative, tmux and jump hosts, the
  environment variables, the security model, and the known limits.
  ([#15](https://github.com/jhnhnsn/rowser/issues/15))
- `rowser <url>` now acts as `rowser open <url>`, so `BROWSER=rowser` works on a Remote host as
  the install hint says. A first argument of the form `scheme://…` counts as a URL; the Daemon
  still opens only http and https. ([#21](https://github.com/jhnhnsn/rowser/issues/21))
- `ROWSER_RUNTIME_DIR` sets the Daemon's runtime directory for both `rowser daemon` and
  `rowser ssh`, so `rowser ssh` can find a Daemon that does not use the default directory. The
  `--runtime-dir` flag of `rowser daemon` wins over it. The value must be an absolute path.
  `rowser daemon install` writes it into the service as `--runtime-dir` when no flag is given.
  ([#19](https://github.com/jhnhnsn/rowser/issues/19))
- `ROWSER_LOG` sets the log level of every rowser process (default `warn`): for example,
  `ROWSER_LOG=debug rowser daemon` shows debug lines, and a failed desktop notification now shows
  a warning. Log lines go to stderr only. ([#17](https://github.com/jhnhnsn/rowser/issues/17))
- An OAuth callback forward on Linux now ends a few seconds after the remote program stops
  listening, also when the program closed the callback connection first: the keeper reads the
  listening ports in `/proc/net/tcp` and `/proc/net/tcp6` (macOS keeps the bind check). Before,
  such a forward could keep `rowser ssh` waiting for about 60 seconds. An end-to-end test with a
  throwaway `sshd` now checks `xdg-open`, an OAuth callback, a reattached session, `ProxyJump`
  and `rowser remote install` on every Linux CI run.
  ([#14](https://github.com/jhnhnsn/rowser/issues/14),
  [#27](https://github.com/jhnhnsn/rowser/issues/27))
- `rowser remote install <host>` installs rowser on a Linux x86_64 or aarch64 remote host: it
  copies the matching release binary (or `--local-build <path>`) to `~/.local/bin/rowser`,
  links `xdg-open`, `sensible-browser` and `www-browser` to it, and prints the
  `export BROWSER=rowser` line for your shell rc.
  ([#13](https://github.com/jhnhnsn/rowser/issues/13))
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

### Fixed

- On macOS, a first `rowser daemon install` no longer prints launchctl's
  `Boot-out failed: 3: No such process`. A real bootstrap failure still shows launchctl's message,
  and `rowser daemon uninstall` of an agent that is not loaded is quiet too.
  ([#34](https://github.com/jhnhnsn/rowser/issues/34))
- `docs/spec.md` now says that rowser also keeps its own sockets in its runtime directory (for
  example `$XDG_RUNTIME_DIR/rowser/`), outside the home directory, as the README does.
  ([#33](https://github.com/jhnhnsn/rowser/issues/33))
- `cargo test` no longer prints `/bin/sh: /tmp/rw.<id>/slow.sh: No such file or directory`: the
  test of a slow open command now keeps its temp directory until the command ends.
  ([#32](https://github.com/jhnhnsn/rowser/issues/32))
- On a Linux Remote host with IPv6 turned off, an OAuth callback forward now ends a few seconds
  after the remote program stops listening, as it does with IPv6 on. Before, the forward stayed
  open for about a minute more. ([#30](https://github.com/jhnhnsn/rowser/issues/30))
- `docs/spec.md` now says that an empty `ROWSER_RUNTIME_DIR` is ignored and that a set value
  must be an absolute path when rowser reads it, as the code does. Its Stack list now names
  `tracing-subscriber`. ([#31](https://github.com/jhnhnsn/rowser/issues/31))
- When sshd on the Remote host listens on a loopback port of 1024 or more (for example in a
  container), rowser no longer takes that port as a Callback port. Before, the forward and the
  ssh session stayed open until the 10-minute timeout. Callback port detection now stops at the
  first sshd process. ([#29](https://github.com/jhnhnsn/rowser/issues/29))
- On Linux, the daemon installed by `rowser daemon install` now starts with your graphical
  session and stops when it ends, so it always has `DISPLAY` or `WAYLAND_DISPLAY` to open the
  browser. The systemd user unit binds to `graphical-session.target`. Run outside a graphical
  session (for example over SSH), install does not start the daemon and says that it starts with
  the next graphical session. A desktop that never reaches `graphical-session.target` does not
  start the daemon. Run `rowser daemon install` again to move an older install to the new target.
  ([#22](https://github.com/jhnhnsn/rowser/issues/22))
- `cargo test` no longer fails at random in `a_failed_open_closes_the_forward`: the test now
  waits up to 5 seconds for a failed open's forward to free its local port.
  ([#23](https://github.com/jhnhnsn/rowser/issues/23))
- rowser builds on macOS with `cargo clippy -- -D warnings` again: an unused field in the
  callback port detection no longer stops the macOS build.
  ([#16](https://github.com/jhnhnsn/rowser/issues/16))
