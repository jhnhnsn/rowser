# rowser spec

rowser lets programs on a headless Linux host open URLs in the browser on your local
machine (macOS or Linux), over SSH. When the URL starts an OAuth flow with a `localhost`
callback, rowser forwards that callback back to the remote host, so CLI logins work.

This spec is the outcome of design grillings: v0.1.0 (decisions Q1–Q32) and v1.0
(2026-11-20, decisions Q1–Q13, #38). Where a ticket disagrees with this spec, the spec wins
unless the user decides otherwise.

**Releases.** v0.1.0 (#1) is everything below except the sections marked *v1.0*. v1.0 (#38),
"CLI-complete", adds a config file, Routes (browser and profile selection), a control socket
with terminal commands, and rowser as the default browser on headless hosts. Everything stays
usable from the command line alone; a menu bar app (#35) comes after v1.0 and uses the control
socket.

## Platforms

| Role | Platforms |
| --- | --- |
| Local (daemon, wrapper) | Linux, macOS (aarch64 and x86_64) |
| Remote (shim) | Linux x86_64 and aarch64 (static musl) |

macOS as a remote host is supported only through `BROWSER=rowser`; rowser does not replace
`open` on macOS. Windows is out of scope.

## Components

One Rust binary, `rowser`. The mode comes from the subcommand, or from argv[0] when the binary
is invoked through a symlink named `xdg-open`, `sensible-browser` or `www-browser` (which act as
`rowser open`). `rowser <url>`, where the first argument is a `scheme://` URL, also acts as
`rowser open <url>`, so `BROWSER=rowser` works.

### Daemon (local) — `rowser daemon`

- Runs as a systemd user unit bound to `graphical-session.target` (Linux) or a launchd
  LaunchAgent (macOS). `rowser daemon install` writes and starts it; `rowser daemon uninstall`
  removes it. On Linux with no active graphical session, install does not start it: it starts
  with the next graphical session.
- Listens on three Unix sockets in its runtime directory (v0.1.0 has the first two):
  - Linux: `$XDG_RUNTIME_DIR/rowser/`; macOS: `~/Library/Application Support/rowser/`.
    Overridable with `ROWSER_RUNTIME_DIR` (read by `rowser daemon` and `rowser ssh`) and, for
    `rowser daemon`, with `--runtime-dir`, which wins over the variable. An empty
    `ROWSER_RUNTIME_DIR` is ignored. When it is read, a set value must be an absolute path; a
    relative value is an error, because each process would resolve it against its own current
    directory.
  - `rowserd.sock` — direct socket, for `ssh_config` forwards. Host stamps are rejected here;
    the host name is the shim's untrusted reported name.
  - `rowserd-relay.sock` — relay socket, for Wrappers only. The first frame after the
    handshake must be a host stamp. A remote host cannot choose which local socket its tunnel reaches, so the stamp is
    trusted.
  - `rowserd-control.sock` — control socket (*v1.0*), for rowser commands run by the same user
    on the Local machine (see Control socket). Never forwarded to a Remote host.
  - Sockets mode 0600; directory 0700.
- Accepts only `http` and `https` URLs. Every other scheme is rejected with an error to the shim.
- Opens URLs immediately and shows a desktop notification naming the host (see Wrapper for where
  the host name comes from). In v1.0 the URL opens in the Target of the first matching Route
  (see Routes), and the notification names that Target.
- `--confirm-hosts <pattern>`: URLs from matching hosts wait until the user clicks "Open" in the
  notification (or a dialog where notification actions are unavailable). Default: no
  confirmation. In v1.0 it keeps working, as "ask first" for matching Host aliases on top of
  the Routes.
- `--open-command "<cmd> {url}"`: the command that opens a URL. Default `xdg-open {url}` on
  Linux, `open {url}` on macOS. In v1.0 it is what the `system` Target runs.
- Daemon flags go before the action: `rowser daemon --confirm-hosts g14 install`.
- Rate limit: at most 5 URLs per 10 seconds per connection; excess requests are rejected with an
  error.

### Wrapper (local) — `rowser ssh <ssh args…>`

- Runs plain `ssh` with `-R /tmp/rowser-<random>.sock:<per-session local socket>`,
  passing every other argument through unchanged. Users may alias `ssh` to it.
- The wrapper listens on its own per-session local socket and relays each connection to the
  daemon's relay socket, stamping it with the host alias the user typed (resolved with
  `ssh -G`, falling back to parsing the ssh arguments). That stamp is the trustworthy host
  name used for notifications and `--confirm-hosts`.
- If the daemon is not running, it prints a warning and runs `ssh` without the forward. It never
  starts the daemon itself.
- Alternative without the wrapper: an `ssh_config` snippet with
  `RemoteForward /tmp/rowser-%C.sock <daemon direct socket>`. This only works reliably on hosts with
  `StreamLocalBindUnlink yes` in `sshd_config`. Host names then come from the shim (untrusted,
  shown as such).

### Shim (remote) — `rowser open <url>`, `rowser <url>` (and the symlinks)

- Socket discovery: tries `ROWSER_SOCKET` if set, then every `/tmp/rowser-*.sock` owned by the
  shim's own UID (others are ignored), newest first (by mtime). Uses the first one that answers the handshake. Removes sockets it owns that
  refuse connections (stale). Because discovery is by glob, a tmux session reattached from a new
  SSH session finds the new socket.
- No working socket: prints `rowser: no tunnel; open this URL manually: <url>` to stderr and
  exits 1. *v1.0*: when there is no working socket but the Shim runs in a graphical session
  (`WAYLAND_DISPLAY` or `DISPLAY` is set), it hands the URL to the real `xdg-open` instead: the
  first `xdg-open` on `PATH` that is not rowser itself. With no such program it prints the
  message above.
- Sends the URL plus the callback ports it detected (see Callback forwarding). When there are
  callback ports, it detects them first (the `/proc` walk needs the real parent), then re-executes
  itself as a detached keeper process (`rowser __keeper`, `setsid`, no fork inside tokio) that
  reconnects with the forward id and holds the callback forward. The daemon queues browser
  connections until the keeper attaches. The foreground shim
  exits 0 as soon as the daemon accepts the request, like a real `xdg-open`, so callers that
  wait for `xdg-open` to exit (for example Go's `cmd.Run`) are never blocked.

### Callback forwarding

- Callback ports are the union of:
  1. Ports from query parameters `redirect_uri`, `redirect_url`, `callback` that point at
     `localhost`, `127.0.0.1` or `[::1]`.
  2. The URL's own port when the URL itself points at one of those addresses.
  3. (Linux remote) Loopback TCP ports that the shim's parent process tree (parent and
     ancestors) is listening on, found through `/proc`. The walk stops at the first sshd
     process: it does not take the listeners of that process or of its ancestors.
- `rowser forward <port>` forwards a port manually.
- Only ports 1024–65535. The daemon binds the local side on loopback only (`127.0.0.1` and
  `[::1]`).
- For each connection to the local port, the daemon asks the shim to dial the remote loopback
  port; the shim opens a new connection through the same forwarded socket and relays bytes.
- A forward closes when the remote callback port stops listening (checked every 2 seconds), when
  a dial to it fails, or after 10 minutes.
- If the local port is busy, the daemon still opens the URL, shows a warning notification, and
  the shim prints the warning to stderr.

### Helpers

- `rowser remote install <host>` (run locally): detects the remote arch with `ssh host uname -sm`,
  downloads the matching release binary (or uses a local build), checks the download against
  the release's `SHA256SUMS` (refusing a missing `SHA256SUMS`, a missing entry or a mismatch;
  a local build skips the check), copies it to
  `~/.local/bin/rowser`, creates the symlinks, and prints the shell line for `BROWSER`.
  *v1.0*: it also makes rowser the default browser for the whole remote user session (see
  Default browser on a Remote host).
- `rowser doctor`: checks the local and remote setup.
  - On a Remote host: warns when `~/.local/bin` is not before `/usr/bin` on `PATH`, when
    `BROWSER` is not set, and does a live round trip through the tunnel (ping → daemon
    version). *v1.0*: it also checks each part of Default browser on a Remote host.
  - On the Local machine (*v1.0*): checks the config file, Routes whose Target names a missing
    browser or profile, and that the Daemon answers on the control socket.
- rowser never changes files outside the user's home directory (and its own sockets in `/tmp` and in its
  runtime directory, and its service file, which follows `XDG_CONFIG_HOME`).

### Routes (*v1.0*)

A **Route** sends the Open requests of one Remote host (and optionally only some URLs) to one
**Target**: a browser, optionally with a **Profile**.

- Routes live in the config file. `[[route]]` entries are checked in file order; the first match
  wins. A `[default]` entry catches every request no Route matched; with no `[default]`, the
  default is `open = "system"`.
- `host`: a glob (`*`, `?`, ASCII case ignored, the same rules as `--confirm-hosts`) matched
  against the **Host alias only**. A request that carries only a Reported host name (direct
  socket) never matches a `[[route]]`: it goes to the `[unverified]` entry, whose default is
  `open = "ask"`. So a Shim that forges a host name cannot reach a named Route.
- `match` (optional): a list of globs matched against the URL's host (`*.google.com`,
  `localhost`). Without `match`, every URL matches.
- `open`: the Target, as a string:
  - `system` — the `--open-command` (default `open {url}` / `xdg-open {url}`).
  - `ask` — wait for the user's answer, as `--confirm-hosts` does; "Open" uses the `[default]`
    Target (`system` when `[default]` is also `ask`), and `rowser approve <id> --open <target>` can pick another.
  - `<browser>` or `<browser>:<profile>` — for example `safari`, `chrome:Work`,
    `firefox:Dev`. Browser names are short names from `rowser browsers`. The profile is a
    folder name (`Profile 2`) or a display name (`Work`); the folder name is tried first.
- `ask = true` (optional): the Route's Target is used only after the user approves, as with
  `--confirm-hosts`.
- A Target whose browser or profile is missing falls back to `[default]`, and the notification
  and `rowser activity` say so.
- `rowser route list`, `rowser route add <host> [--match <glob>]… --open <target> [--ask]`,
  `rowser route remove <n>`, `rowser route move <n> <to>`, `rowser route test <host> <url>`
  (prints the Target and the Route that matched, opens nothing). `route add` resolves a
  profile display name to its folder name when it writes the file. These commands work when
  the Daemon is not running.

### Browsers (*v1.0*)

- `rowser browsers` lists the browsers found on the Local machine, with their short names and
  Profiles.
- macOS: a known list, looked up in `/Applications` and `~/Applications`: Safari (`safari`),
  Google Chrome (`chrome`), Firefox (`firefox`), Microsoft Edge (`edge`), Brave (`brave`),
  Vivaldi (`vivaldi`), Chromium (`chromium`), Arc (`arc`). A URL opens with
  `open -b <bundle id>`, with the profile argument when there is one.
- Linux: the `.desktop` files in the XDG data directories that handle
  `x-scheme-handler/https`; the short name comes from the known list where the desktop id is
  known, else from the desktop id.
- Profiles: Chromium-family browsers from `profile.info_cache` in their `Local State` file
  (opened with `--profile-directory=<folder>`); Firefox from `profiles.ini` (opened with
  `-P <name>`, which can start a second Firefox instance). Safari and Arc have no Profiles in
  rowser.

### Control socket and terminal commands (*v1.0*)

- `rowserd-control.sock` answers only rowser commands on the Local machine. It uses the same
  protocol and handshake as the other sockets.
- `rowser status`: Daemon version and protocol version, config file path, paused or not,
  Sessions (each connected Wrapper's Host alias, since when, and its Route's Target), and open
  Callback forwards (port, Host alias, age).
- `rowser activity [--follow] [--json]`: the last 200 Open requests and `rowser forward`
  requests, newest last: time, host (Host alias, or Reported host name marked unverified), URL,
  the Route that matched, the Target, and the result (opened, denied, timed out, paused, rate
  limited, scheme refused, browser missing) and, for a Callback forward, how long it stayed
  open. Kept in memory only; a Daemon restart clears it.
- `rowser pause [--for <duration>]` and `rowser resume`. While paused, the Daemon refuses each
  Open request and `rowser forward`; the Shim prints
  `rowser: paused; open this URL manually: <url>` and exits 1. Nothing is queued.
- `rowser pending`, `rowser approve <id> [--open <target>]`, `rowser deny <id>`: act on
  requests that wait for an answer. The first answer wins, from the terminal or from the
  notification or dialog.
- `rowser reload`: the Daemon reads the config file again. An invalid file is refused with the
  error, and the Daemon keeps the Routes it had.
- Each command that needs the Daemon and finds no control socket prints
  `rowser: the daemon is not running` and exits 1.

### Default browser on a Remote host (*v1.0*)

`rowser remote install <host>` makes rowser the default browser for the remote user, not only
for login shells:

- `~/.config/environment.d/rowser.conf` sets `BROWSER=rowser` and puts `~/.local/bin` first on
  `PATH` for systemd user services.
- `~/.local/share/applications/rowser.desktop`, registered as the default handler for
  `x-scheme-handler/http` and `x-scheme-handler/https` (through `xdg-mime` when it is installed,
  else by writing `~/.config/mimeapps.list`).
- No system-wide install.

## Protocol

Newline-delimited JSON over the Unix socket. The first message on every connection is a
handshake carrying a protocol version `major.minor`. On a major mismatch both sides fail with a
clear error naming both versions and the update command. v1.0 is protocol 1.1: it adds the
control socket's messages and the Wrapper's Session registration; a 1.0 peer still works for
opening URLs and Callback forwards.

## Configuration

v0.1.0 has no config file: flags and environment variables only (`ROWSER_SOCKET`, `ROWSER_LOG`,
`ROWSER_RUNTIME_DIR`, …).

*v1.0*: one TOML file, `$XDG_CONFIG_HOME/rowser/config.toml` (default
`~/.config/rowser/config.toml`) on macOS and Linux. It holds the Routes. The Daemon only reads
it: at start, on `rowser reload`, and after each `rowser route` change. The `rowser route`
commands change it in place and keep the user's comments and layout. A missing file means no
Routes. An invalid file stops `rowser daemon` from starting, with the file, line and reason.
Flags win over the file.

```toml
[[route]]
host = "g14"
match = ["*.google.com"]
open = "chrome:Profile 2"

[[route]]
host = "g14"
open = "safari"

[unverified]
open = "ask"

[default]
open = "system"
```

## Stack

tokio, clap, serde / serde_json, url, notify-rust, tracing, tracing-subscriber (features
`env-filter`, `fmt`, `ansi`, `std`), sha2. *v1.0*: toml, toml_edit.

## Tests

1. Unit tests: URL parsing and callback detection, socket discovery ordering, rate limiting.
2. Integration tests: shim and daemon over a real Unix socket, with a fake opener and a fake
   remote HTTP listener. No SSH.
3. One end-to-end test on Linux CI: throwaway `sshd` on localhost, full OAuth-style callback.

macOS CI runs levels 1 and 2.

## Distribution

- GitHub Release binaries: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
  `aarch64-apple-darwin`, `x86_64-apple-darwin`, built in GitHub Actions.
- `cargo install --git https://github.com/jhnhnsn/rowser`.
- Reserve the `rowser` crate name on crates.io with the first release. Full crates.io publishing
  and a Homebrew tap come after v1.0.

## Out of scope for v1.0

mosh, Windows (local), replacing `open` on macOS remotes, clipboard, file transfer, a menu bar
or tray app (#35), a browser chooser window, Safari and Arc profiles, a system-wide install on
the Remote host.

## Known limits

- Per-session sockets need `rowser ssh` (or an alias). Without root on the remote host, plain
  `ssh_config` cannot avoid stale sockets.
- The callback port is fixed by the remote program's `redirect_uri`, so a busy local port cannot
  be worked around.
