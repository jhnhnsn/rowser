# rowser spec

rowser lets programs on a headless Linux host open URLs in the browser on your local
machine (macOS or Linux), over SSH. When the URL starts an OAuth flow with a `localhost`
callback, rowser forwards that callback back to the remote host, so CLI logins work.

This spec is the outcome of design grillings: v0.1.0 (decisions Q1–Q32), v1.0
(2026-11-20, decisions Q1–Q13, #38) and v1.0 Links (2026-11-27, decisions Q1–Q26). Where a
ticket disagrees with this spec, the spec wins unless the user decides otherwise.

**Goal.** After the install and one `rowser host add <host>`, every program on a Saved host
can open URLs in the browser on the Local machine, also when no interactive SSH session is
open (detached herdr agents, tmux jobs, cron).

**Releases.** v0.1.0 (#1) was built with a per-session Wrapper (`rowser ssh`); it was never
released. v1.0 (#38), "CLI-complete", adds a config file, Routes (browser and profile
selection), a control socket with terminal commands, rowser as the default browser on headless
hosts, and (*v1.0 Links*) replaces the Wrapper with Links that the Daemon keeps to Saved hosts.
v1.0.0 is held until the Links work ships (v1.0.0-rc.1 and rc.2 still have the Wrapper).
Everything stays usable from the command line alone; a menu bar app (#35) comes after v1.0 and
uses the control socket.

## Platforms

| Role | Platforms |
| --- | --- |
| Local (daemon) | Linux, macOS (aarch64 and x86_64) |
| Remote (shim, outpost) | Linux x86_64 and aarch64 (static musl) |

A Saved host must be a Linux Remote host: the Outpost runs only there. Windows is out of scope.

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
- Keeps one Link to each Saved host (see Links and Saved hosts). Every Open request arrives on a
  Link, so the Daemon always knows the Host alias it came from.
- Listens on one Unix socket in its runtime directory:
  - Linux: `$XDG_RUNTIME_DIR/rowser/`; macOS: `~/Library/Application Support/rowser/`.
    Overridable with `ROWSER_RUNTIME_DIR` (read by `rowser daemon` and the local commands) and,
    for `rowser daemon`, with `--runtime-dir`, which wins over the variable. An empty
    `ROWSER_RUNTIME_DIR` is ignored. When it is read, a set value must be an absolute path; a
    relative value is an error, because each process would resolve it against its own current
    directory.
  - `rowserd-control.sock` — control socket, for rowser commands run by the same user on the
    Local machine (see Control socket). Never reachable from a Remote host.
  - Socket mode 0600; directory 0700.
  - *v1.0 Links*: the direct socket (`rowserd.sock`) and the relay socket (`rowserd-relay.sock`)
    are removed.
- Accepts only `http` and `https` URLs. Every other scheme is rejected with an error to the shim.
- Opens URLs immediately and shows a desktop notification naming the Host alias. The URL opens
  in the Target of the first matching Route (see Routes), and the notification names that
  Target.
- `--confirm-hosts <pattern>`: URLs from matching hosts wait until the user clicks "Open" in the
  notification (or a dialog where notification actions are unavailable). Default: no
  confirmation. In v1.0 it keeps working, as "ask first" for matching Host aliases on top of
  the Routes.
- `--open-command "<cmd> {url}"`: the command that opens a URL. Default `xdg-open {url}` on
  Linux, `open {url}` on macOS. In v1.0 it is what the `system` Target runs.
- Daemon flags go before the action: `rowser daemon --confirm-hosts g14 install`.
- Rate limit: at most 5 URLs per 10 seconds per Saved host (all streams of its Link together);
  excess requests are rejected with an error.
- `--confirm-unverified` is removed (*v1.0 Links*): `rowser daemon --confirm-unverified` fails
  with an error that says so, and `rowser doctor` tells the user to run `rowser daemon install`
  again when the installed service still has it.

### Links and Saved hosts (local) — `rowser host …` (*v1.0 Links*)

- A **Saved host** is a `[[host]]` entry in the config file. `name` is the Host alias: a name
  from the user's `ssh_config` or a `user@host`, given to `ssh` unchanged, so `User`, `Port`,
  keys and `ProxyJump` from `ssh_config` apply. `remote_path` is where rowser is on that host
  (default `~/.local/bin/rowser`). `auto_update` (default `true`) lets the Daemon update rowser
  on that host. `default_browser` (default `true`; `false` after `--no-default-browser`) says
  whether installs on that host include the default-browser part. The install also leaves a
  marker on the Remote host when it skips that part, so `rowser doctor` there skips its checks.
- `rowser host add <host> [--no-default-browser]`: finds rowser on the host once
  (`command -v rowser` in the user's login shell there) and saves its path as `remote_path`. If
  rowser is missing or its version differs from the local one, it runs the remote install first
  (see Helpers), with the default-browser part unless `--no-default-browser` is given (for a
  host with its own desktop). It then writes the `[[host]]` entry and asks a running Daemon to
  read the config file again.
- `rowser host remove <host>`: removes the entry and closes its Link.
- `rowser host list`: each Saved host, its Link state (connected, connecting, needs login,
  version mismatch), the time of its last connection and its Outpost version.
- `rowser host connect <host>`: logs in to the host in the user's terminal, where `ssh` may ask
  for a passphrase or a hardware key touch. It starts a background master connection
  (`-o ControlMaster=yes -o ControlPersist=yes -N -f`) on rowser's own control path (below), then
  tells the Daemon to connect the Link again, which then needs no login while that master lives.
- The `host` commands write the config file themselves (ADR 0007) and work when the Daemon is
  not running.
- A **Link** is one `ssh -T -o BatchMode=yes -o ControlMaster=no
  -o ControlPath=<runtime dir>/cm-%C -o ServerAliveInterval=15 -o ServerAliveCountMax=3 <host>
  <remote_path> __outpost` process that the Daemon starts and supervises. The protocol runs over
  its stdin and stdout (see Protocol). The control path is rowser's own, never the user's, so a
  Link never shares an SSH connection with the user's own sessions; it uses a master there only
  when `rowser host connect` started one.
- A Link is always open while the Daemon runs: it connects when the Daemon starts and when a
  host is added. When it drops, the Daemon connects again with backoff (1 s, doubling, at most
  60 s), and at once after the Local machine wakes or its network changes.
- A failed login (`BatchMode` cannot authenticate) puts the Link in the **needs login** state:
  the Daemon stops retrying that host and shows one notification. `rowser host connect <host>`
  ends it.
- A major version mismatch with the Outpost puts the Link in the **version mismatch** state. With
  `auto_update = true`, the Daemon runs the remote install for that host (keeping how it was
  added, with or without the default-browser part), shows one notification "updated rowser on
  <host> to <version>", and connects again. With `auto_update = false` it shows one
  notification and waits for `rowser remote install <host>`.
- Notifications about Links: only needs login, version mismatch and updated. A drop or a
  reconnect shows none.
- *v1.0 Links*: the Wrapper is removed. For one release `rowser ssh …` prints
  `rowser ssh is removed: run "rowser host add <host>" once, then use plain ssh` and exits 2;
  the release after removes it. The `ssh_config` `RemoteForward` setup is no longer supported.

### Outpost (remote) — `rowser __outpost`

- Started only by a Link, on its far end. It speaks the protocol with the Daemon over its stdin
  and stdout, and exits when they close.
- Listens on the Outpost socket `/tmp/rowser-<uid>/outpost.sock`. The path comes from the UID
  alone, never from environment variables, so cron jobs, systemd services, tmux and herdr
  sessions all find it. The folder must belong to the user and have mode 0700, else the Outpost
  refuses it and the Link reports the error. The socket has mode 0600.
- Two Local machines with a Link to one Remote host share one Outpost socket for that user: the
  Link that connected last takes the Open requests. When it drops, requests go to the other Link
  again.
- Relays each Shim connection to the Daemon as a new stream on the Link, and carries Callback
  forwards (see Callback forwarding).
- When its Link drops it closes its streams, ends its Callback forwards and exits; its socket
  goes away with it.

### Shim (remote) — `rowser open <url>`, `rowser <url>` (and the symlinks)

- Connects to `ROWSER_SOCKET` if set, else to the Outpost socket `/tmp/rowser-<uid>/outpost.sock`.
- No working socket: prints `rowser: no Link to your Local machine; open this URL manually: <url>`
  and, on a second line, `On your Local machine, run "rowser host add <host>", or check "rowser host list".`
  to stderr, and exits 1. When there is no working socket but the Shim runs in a graphical
  session (`WAYLAND_DISPLAY` or `DISPLAY` is set), it hands the URL to the real `xdg-open`
  instead: the first `xdg-open` on `PATH` that is not rowser itself. With no such program it
  prints the message above.
- Sends the URL plus the callback ports it detected (see Callback forwarding). It detects them
  before it sends (the `/proc` walk needs the real parent). The Outpost holds the Callback
  forward, so the Shim exits 0 as soon as the Daemon accepts the request, like a real
  `xdg-open`, and callers that wait for `xdg-open` to exit (for example Go's `cmd.Run`) are
  never blocked. *v1.0 Links*: the detached keeper (`rowser __keeper`) is removed.

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
- For each connection to the local port, the Daemon opens a new stream on the Link; the Outpost
  dials the remote loopback port and relays bytes.
- A forward closes when the remote callback port stops listening (checked every 2 seconds, with
  `/proc` as ADR 0005 says), when a dial to it fails, when its Link drops, or after 10 minutes.
- If the local port is busy, the daemon still opens the URL, shows a warning notification, and
  the shim prints the warning to stderr.

### Helpers

- `rowser remote install <host>` (run locally): detects the remote arch with `ssh host uname -sm`,
  downloads the matching release binary (or uses a local build), checks the download against
  the release's `SHA256SUMS` (refusing a missing `SHA256SUMS`, a missing entry or a mismatch;
  a local build skips the check), copies it to
  `~/.local/bin/rowser`, creates the symlinks, and prints the shell line for `BROWSER`.
  *v1.0*: it also makes rowser the default browser for the whole remote user session (see
  Default browser on a Remote host). *v1.0 Links*: `--no-default-browser` installs only the
  binary (no symlinks, `environment.d` or `rowser.desktop`), for a host with its own desktop;
  `rowser host add` and the Daemon's automatic update call the same install.
- `rowser doctor`: checks the local and remote setup.
  - On a Remote host: checks the Outpost socket (owner, mode, and a live round trip through the
    Link: ping → Daemon version). Unless the host was added with `--no-default-browser`, it also
    warns when the browser symlinks in `~/.local/bin` do not resolve to rowser, when
    `~/.local/bin` is not before `/usr/bin` on `PATH`, when `BROWSER` is not set or does not
    resolve to rowser, and checks each part of Default browser on a Remote host.
  - On the Local machine: checks the config file, Targets (of Routes and `[default]`) that name
    a missing browser or profile, that the Daemon answers on the control socket, each Saved
    host's Link state, and that `ssh -o BatchMode=yes <host> true` succeeds for each Saved host.
- rowser never changes files outside the user's home directory (and its own sockets in
  `/tmp/rowser-<uid>/` and in its runtime directory, and its service file, which follows
  `XDG_CONFIG_HOME`).

### Routes (*v1.0*)

A **Route** sends the Open requests of one Remote host (and optionally only some URLs) to one
**Target**: a browser, optionally with a **Profile**.

- Routes live in the config file. `[[route]]` entries are checked in file order; the first match
  wins. A `[default]` entry catches every request no Route matched; with no `[default]`, the
  default is `open = "system"`.
- `host`: a glob (`*`, `?`, ASCII case ignored, the same rules as `--confirm-hosts`) matched
  against the **Host alias** of the Saved host whose Link carried the request. A Shim cannot
  name its own host, so it cannot reach another host's Route. *v1.0 Links*: the `[unverified]`
  entry is removed.
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
  protocol and handshake as the Link.
- `rowser status`: Daemon version and protocol version, config file path, paused or not,
  Links (each Saved host's Host alias, Link state, since when, Outpost version, and its Route's
  Target), and open Callback forwards (port, Host alias, age).
- `rowser activity [--follow] [--json]`: the last 200 Open requests and `rowser forward`
  requests, newest last: time, Host alias, URL,
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
- *v1.0 Links*: `--no-default-browser` (on `rowser host add` and `rowser remote install`) skips
  this part and the `~/.local/bin` symlinks. Use it on a host with its own desktop, where the
  desktop's apps keep the host's own browser and the user sets `BROWSER=rowser` only where they
  want it (for example in SSH sessions). Without it, an always-open Link would send the host's
  desktop URLs to the Local machine, and the real `xdg-open` fallback could reach
  `rowser.desktop` again.

## Protocol

Newline-delimited JSON. The first message on every connection or stream is a handshake carrying
a protocol version `major.minor`. On a major mismatch both sides fail with a clear error naming
both versions and the update command.

- v1.0 is protocol 1.1 on the control socket and between Shim and Wrapper/Daemon.
- *v1.0 Links* is protocol **2.0**. A Link's stdin and stdout carry `yamux` streams; each Shim
  connection, each Callback forward connection and each Daemon-to-Outpost control exchange is
  one stream, with NDJSON inside. The Shim speaks NDJSON to the Outpost socket. A 1.x Shim or
  Outpost gets the major mismatch error.

## Configuration

v0.1.0 has no config file: flags and environment variables only (`ROWSER_SOCKET`, `ROWSER_LOG`,
`ROWSER_RUNTIME_DIR`, …).

*v1.0*: one TOML file, `$XDG_CONFIG_HOME/rowser/config.toml` (default
`~/.config/rowser/config.toml`) on macOS and Linux. It holds the Saved hosts and the Routes.
The Daemon only reads it: at start, on `rowser reload`, and after each `rowser route` or
`rowser host` change. Those commands change it in place and keep the user's comments and
layout. A missing file means no Saved hosts and no Routes. An invalid file stops
`rowser daemon` from starting, with the file, line and reason. Flags win over the file.

*v1.0 Links*: an `[unverified]` entry is ignored for one release, with one warning in the log and
in `rowser doctor`; the release after makes it an error.

```toml
[[host]]
name = "g14"
remote_path = "~/.local/bin/rowser"
auto_update = true
default_browser = false

[[route]]
host = "g14"
match = ["*.google.com"]
open = "chrome:Profile 2"

[[route]]
host = "g14"
open = "safari"

[default]
open = "system"
```

## Stack

tokio, clap, serde / serde_json, url, notify-rust (not on macOS, which uses osascript), tracing,
tracing-subscriber (features `env-filter`, `fmt`, `ansi`, `std`), sha2. *v1.0*: toml, toml_edit.
*v1.0 Links*: yamux.

## Tests

1. Unit tests: URL parsing and callback detection, rate limiting, Link state and backoff.
2. Integration tests: Shim, Outpost and Daemon over real Unix sockets and a Link whose `ssh` is
   a fake that runs `rowser __outpost` locally, with a fake opener and a fake remote HTTP
   listener. No SSH.
3. One end-to-end test on Linux CI: throwaway `sshd` on localhost, a Saved host, a detached
   Shim call with no interactive session, and a full OAuth-style callback.

macOS CI runs levels 1 and 2.

## Distribution

- GitHub Release binaries: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
  `aarch64-apple-darwin`, `x86_64-apple-darwin`, built in GitHub Actions.
- An installer script, `rowser-installer.sh`, attached to each GitHub Release:
  `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jhnhnsn/rowser/releases/latest/download/rowser-installer.sh | sh`.
  It checks the tarball against `SHA256SUMS` as `rowser remote install` does, and installs only
  the `rowser` binary to `~/.local/bin` (`ROWSER_INSTALL_DIR` overrides it).
- `cargo install --git https://github.com/jhnhnsn/rowser`.
- Reserve the `rowser` crate name on crates.io with the first release. Full crates.io publishing
  and a Homebrew tap come after v1.0.

## Out of scope for v1.0

mosh, Windows (local), macOS as a Remote host, clipboard, file transfer, a menu bar or tray app
(#35), a browser chooser window, Safari and Arc profiles, a system-wide install on the Remote
host, transports other than SSH (for example a direct LAN or Tailscale connection; it would need
its own ADR).

## Known limits

- A Link needs a login with no prompt (ssh-agent, the macOS Keychain, or an unencrypted key).
  A key that needs a touch or a passphrase each time works only through `rowser host connect`.
- URLs open only while the Local machine is awake and can reach the Saved host. On a Linux Local
  machine, the Daemon (and so every Link) runs only during a graphical session.
- The callback port is fixed by the remote program's `redirect_uri`, so a busy local port cannot
  be worked around.
