# Changelog

All notable changes to rowser are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rowser uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Saved hosts and Links: each `[[host]]` in the config file gets one Link, an `ssh` connection
  that the Daemon keeps open and runs `rowser __outpost` on the host. Programs on that host
  open URLs through the Outpost socket `/tmp/rowser-<uid>/outpost.sock`, with no `rowser ssh`
  session. A dropped Link reconnects after 1 s, then up to 60 s. A failed login shows one
  notification and stops the retries. `rowser reload` adds or removes Links.
  ([#77](https://github.com/jhnhnsn/rowser/issues/77))

### Changed

- Protocol 2.0. A rowser 1.x on the other side gets the version mismatch error; update both
  sides. ([#77](https://github.com/jhnhnsn/rowser/issues/77))

## [1.0.0-rc.2] - 2026-11-26

The second release candidate for v1.0.0, and the first Release with `rowser-installer.sh`.
Before v1.0.0: the manual checks of the macOS
notification fix ([#57](https://github.com/jhnhnsn/rowser/issues/57)) and of `--confirm-hosts`
on Linux ([#20](https://github.com/jhnhnsn/rowser/issues/20)).

### Added

- Install rowser on a Local machine or a Remote host with one line:
  `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jhnhnsn/rowser/releases/latest/download/rowser-installer.sh | sh`.
  Each Release after v1.0.0-rc.1 has `rowser-installer.sh` attached; put `download/v<version>`
  in the URL for a pre-release. The script picks the binary for your system, checks it against
  the Release's `SHA256SUMS`, and installs only `rowser` to `~/.local/bin`
  (`ROWSER_INSTALL_DIR` changes the folder). It needs no `sudo`, and prints the next steps.
  ([#75](https://github.com/jhnhnsn/rowser/issues/75))

## [1.0.0-rc.1] - 2026-11-25

The first release, a release candidate for v1.0.0. It has all of v0.1.0 (the daemon, the shim,
`rowser ssh`, OAuth callback forwards, `rowser daemon install`, `rowser remote install` and
`rowser doctor`) and all of v1.0 (a config file with Routes to a browser and Profile,
`rowser browsers`, the control commands `status`, `activity`, `pause`/`resume`,
`pending`/`approve`/`deny` and `reload`, and rowser as the default browser on a remote host).
Release binaries are on the GitHub Release. Before v1.0.0: the manual checks of the macOS
notification fix ([#57](https://github.com/jhnhnsn/rowser/issues/57)) and of `--confirm-hosts`
on Linux ([#20](https://github.com/jhnhnsn/rowser/issues/20)).

### Added

- The README documents v1.0: Routes (`rowser route add|list|test|move|remove`), `rowser
  browsers`, the commands that control the daemon (`status`, `activity`, `pause`/`resume`,
  `pending`/`approve`/`deny`, `reload`), and how to make rowser the default browser on a Remote
  host. `docs/design.md` gives the v1.0 module layout.
  ([#54](https://github.com/jhnhnsn/rowser/issues/54))
- `rowser pending` lists the requests that wait for a confirmation (id, host, URL, Target and
  age). `rowser approve <id> [--open <target>]` and `rowser deny <id>` answer one from the
  terminal, and the notification or dialog closes. The first answer wins: a later answer from
  the desktop or the terminal is refused. `rowser activity` shows who answered (`by terminal`
  or `by desktop`). ([#50](https://github.com/jhnhnsn/rowser/issues/50))
- `rowser doctor` now checks the Local machine too: that the config file parses (or the file
  and line of the error), that each Route's browser and Profile exist (and where the request
  falls back if not), and whether the Daemon answers, with its version. Doctor runs the Local
  checks where a config file or Daemon runtime directory exists, and the Remote host checks
  over SSH. ([#53](https://github.com/jhnhnsn/rowser/issues/53))
- Routes can open a URL in a given browser and Profile: `open = "chrome:Work"`,
  `"firefox:Dev"` or `"safari"`. A Profile is looked up by its folder name first, then by its
  display name. On macOS rowser runs `open -b <bundle id>` (Chromium browsers get their
  Profile with `--profile-directory`, Firefox with `-P`); on Linux it runs the browser's
  desktop file `Exec` line, never through a shell. If the browser or Profile is missing, the
  URL opens in `[default]` and the notification says so. `rowser route add --open chrome:Work`
  writes the folder name (`chrome:Profile 2  # Work`), and `rowser route test` shows the
  resolved browser and Profile, or the fallback.
  ([#47](https://github.com/jhnhnsn/rowser/issues/47))
- `rowser pause [--for <duration>]` (such as `30m`, `1h`, `2h30m`) stops the Daemon from
  opening URLs and starting `rowser forward`; `rowser resume` ends the pause. While paused,
  `xdg-open` on the remote host prints `rowser: paused; open this URL manually: <url>` and
  exits 1, and `rowser forward` prints `rowser: paused; port <p> was not forwarded`; nothing
  is queued. `rowser status` shows the pause and, with `--for`, until when;
  `rowser activity` shows `paused`. A Daemon restart ends a pause.
  ([#49](https://github.com/jhnhnsn/rowser/issues/49))
- `rowser doctor` on a Remote host also checks the default-browser setup:
  `~/.config/environment.d/rowser.conf` sets `BROWSER=rowser` and puts `~/.local/bin` first on
  `PATH`, `~/.local/share/applications/rowser.desktop` exists and is the default handler for
  `http` and `https` links (from `xdg-mime query default`, else `~/.config/mimeapps.list`), and,
  with a graphical session, a real `xdg-open` exists for the fallback. Each failed check prints
  one warning line with its fix: `rowser remote install <host>` on the Local machine, or the lines
  to add when the file is one that install keeps (your own `environment.d` file, a symlinked
  `mimeapps.list`).
  ([#52](https://github.com/jhnhnsn/rowser/issues/52))
- `rowser route list|add|remove|move|test` manages the Routes in the config file.
  `route add <host> [--match <glob>]… --open <target> [--ask]` appends a Route, `remove <n>` and
  `move <n> <to>` use the numbers from `route list`, and your comments and layout stay. An
  invalid Target is refused and the file is not changed. `route test <host> <url>
  [--unverified]` prints the Target and the Route that matched, and opens nothing. A running
  Daemon reloads the file after each change; the commands also work when it is stopped.
  ([#46](https://github.com/jhnhnsn/rowser/issues/46))
- `rowser activity [--follow] [--json]` shows the Daemon's last 200 Open requests and
  `rowser forward` requests, newest last: time, host (Host alias, or the Reported host name
  marked unverified), URL, the Route that matched, the Target, and the result (opened, denied,
  timed out, rate limited, scheme refused, browser missing). A Callback forward shows its ports
  and, once it closes, how long it stayed open. `--follow` keeps printing new requests until
  Ctrl-C; `--json` prints one JSON object per line. A Daemon restart clears the list.
  ([#48](https://github.com/jhnhnsn/rowser/issues/48))
- `rowser reload` makes the running Daemon read the config file again. With a valid file, the
  new Routes apply to the next Open request and it prints
  `rowser: config reloaded (<n> routes)`. An invalid file is refused with the file, line and
  reason, the Daemon keeps its old Routes, and the command exits 1. ([#45](https://github.com/jhnhnsn/rowser/issues/45))
- `rowser status` lists Sessions: each `rowser ssh` that is connected to the Daemon, with its
  Host alias, how long it has run, and the Target its first Route without `match` gives
  (`system` with no Route). A Session ends when `ssh` exits. A newer `rowser ssh` still works
  with an older (protocol 1.0) Daemon; it then does not show in `rowser status`.
  ([#44](https://github.com/jhnhnsn/rowser/issues/44))
- On a Remote host with a desktop, `xdg-open` linked to rowser no longer breaks local browsing:
  when no Tunnel answers and `WAYLAND_DISPLAY` or `DISPLAY` is set, the Shim hands the URL to
  the first `xdg-open` on `PATH` that is not rowser itself and exits with its exit code. With no
  such program, or with no graphical session, it prints the no-tunnel message as before.
  ([#43](https://github.com/jhnhnsn/rowser/issues/43))
- `rowser remote install <host>` also writes `~/.config/environment.d/rowser.conf`, so systemd
  user services on the remote host get `BROWSER=rowser` and `~/.local/bin` first on `PATH`
  (from the next login). Your own `rowser.conf` there is kept, with a warning.
  ([#42](https://github.com/jhnhnsn/rowser/issues/42))
- `rowser remote install <host>` also writes `~/.local/share/applications/rowser.desktop` and
  makes it the default for `http` and `https` links on the remote host: with `xdg-mime` when it
  is installed, else in `~/.config/mimeapps.list` (other lines kept; a symlink there is kept,
  with a warning). ([#51](https://github.com/jhnhnsn/rowser/issues/51))
- `rowser status` shows the running Daemon: its version and protocol version, the runtime
  directory, the config file, whether it is paused, and the open Callback forwards (local port,
  host, age). `--json` prints the same as one JSON object. With no Daemon it prints
  `rowser: the daemon is not running` and exits 1. The Daemon now also listens on
  `rowserd-control.sock` (mode 0600) for local rowser commands; the protocol is now 1.1, and a
  1.0 Shim or Wrapper still opens URLs and forwards callbacks.
  ([#40](https://github.com/jhnhnsn/rowser/issues/40))
- A config file, `$XDG_CONFIG_HOME/rowser/config.toml` (default `~/.config/rowser/config.toml`),
  with Routes: `[[route]]` entries send the URLs of one Host alias (optionally only URLs whose
  host matches `match` globs) to a Target, `system` or `ask`, and `ask = true` waits for the
  user first. `[unverified]` (default `ask`) takes requests that carry only a Reported host
  name, and `[default]` (default `system`) takes the rest. The notification names the Target
  when it is not `system`. Browser Targets such as `chrome:Work` are accepted but open with
  `[default]` for now, with a warning. An invalid file stops `rowser daemon` with the file,
  line and reason. ([#39](https://github.com/jhnhnsn/rowser/issues/39))
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

### Changed

- For v0.1.0 users: a URL that comes through the direct socket (no Host alias, only a Reported
  host name) now waits for the user to approve it, also when there is no config file, because
  `[unverified]` defaults to `ask`. To open these URLs at once as before, put
  `[unverified]` with `open = "system"` in the config file.
  ([#39](https://github.com/jhnhnsn/rowser/issues/39))

### Fixed

- The control tests that retry on a busy port now stop at the rate limit (5 tries), so a run of
  busy ports gives a clear "no free port" error, not an unexpected rate-limit reply.
  ([#74](https://github.com/jhnhnsn/rowser/issues/74))
- The daemon tests that retry on a busy port now stop at the rate limit (5 tries), so a run of
  busy ports gives a clear "no free port" error, not an unexpected rate-limit reply.
  ([#73](https://github.com/jhnhnsn/rowser/issues/73))
- The activity tests no longer fail at random when another test or process takes a free port
  before the Daemon binds it. ([#72](https://github.com/jhnhnsn/rowser/issues/72))
- Two more activity tests (an open request's Callback forward duration, and `--follow` when a
  Callback forward closes) no longer fail at random on a slow machine.
  ([#69](https://github.com/jhnhnsn/rowser/issues/69))
- The control and Daemon tests no longer fail at random when another test or process takes a
  free port before the Daemon binds it. ([#71](https://github.com/jhnhnsn/rowser/issues/71))
- The Callback forward tests no longer fail at random when another test or process takes a
  free port before the Daemon binds it. ([#68](https://github.com/jhnhnsn/rowser/issues/68))
- macOS builds no longer compile notify-rust, which only Linux uses (macOS shows notifications
  with `osascript`), so the macOS build has no `unused dependency` warning.
  ([#70](https://github.com/jhnhnsn/rowser/issues/70))
- The activity test for how long a Callback forward stayed open no longer fails at random on a
  slow machine. ([#65](https://github.com/jhnhnsn/rowser/issues/65))
- The spec and README now describe all of `rowser doctor`'s checks, on the Remote host and on
  the Local machine, and say when each side runs. ([#67](https://github.com/jhnhnsn/rowser/issues/67),
  [#63](https://github.com/jhnhnsn/rowser/issues/63))
- The doctor tests no longer share a home folder on macOS, where the file system ignores letter
  case, so they pass there again. ([#64](https://github.com/jhnhnsn/rowser/issues/64))
- Builds from source and CI now use the same pinned Rust toolchain (1.100.0, from
  `rust-toolchain.toml`), so a new stable clippy no longer breaks CI for code that passes
  locally. The README says how to bump it. ([#66](https://github.com/jhnhnsn/rowser/issues/66))
- The Stale socket tests (daemon bind, Shim discovery and `rowser status`) no longer fail at
  random when another test starts a process at the same time.
  ([#58](https://github.com/jhnhnsn/rowser/issues/58))
- Tests that run a fake script no longer fail at random with "Text file busy" (ETXTBSY) when
  other tests start processes at the same time. ([#55](https://github.com/jhnhnsn/rowser/issues/55))
- The control-socket test for a Forward that leaves `rowser status` no longer fails at random on
  a slow machine. ([#56](https://github.com/jhnhnsn/rowser/issues/56))
- The tests build without warnings on Rust 1.100, so `cargo clippy --all-targets -- -D warnings`
  passes again for contributors and in CI. ([#61](https://github.com/jhnhnsn/rowser/issues/61))
- `rowser doctor --help` now says "Check the local and remote setup", because doctor checks
  the Local machine too. ([#60](https://github.com/jhnhnsn/rowser/issues/60))
- On macOS, the notification for an opened URL now shows. It did not show on current macOS. The
  text now also shows as it is, so a URL with `&` no longer shows `&amp;`.
  ([#57](https://github.com/jhnhnsn/rowser/issues/57),
  [#18](https://github.com/jhnhnsn/rowser/issues/18))
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
