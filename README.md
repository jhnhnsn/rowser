# rowser

> [!WARNING]
> rowser is under active development. The latest release, v1.0.0-rc.2, is a release candidate.
> Expect rough edges and breaking changes until v1.0.0.
>
> rowser runs on Linux and macOS for now. Other platforms come later.

Open URLs from headless SSH hosts in your local browser — including OAuth logins that call back
to `localhost` on the remote host.

A program on the remote host runs `xdg-open <url>` (or uses `$BROWSER`). rowser sends the URL
over your SSH connection to a daemon on your local machine, which opens it in your browser and
shows a notification naming the host. When the URL starts an OAuth flow with a `localhost`
callback, rowser sets up a callback forward for that callback port to the remote host, so CLI logins
that use a `localhost` redirect (`gcloud auth login`, `az login`, …) just work.

| Role | Platforms |
| --- | --- |
| Local (daemon, `rowser ssh`) | Linux, macOS (aarch64 and x86_64) |
| Remote (shim) | Linux x86_64 and aarch64 (static musl) |

## Why rowser

Several tools already open a remote host's URLs in a local browser. Each one covers part of
the job, but none of them covers all of it. We wanted something that works over the SSH session
you already have, without a relay service or an open TCP port. It also had to finish OAuth logins
that call back to `localhost` on the remote host, work across many SSH sessions without root
changes to `sshd_config`, and give the local machine some control over what gets opened.

| Need | rowser | [opener] | [lemonade] | [rht] | [nssh] | [local-open] |
| --- | --- | --- | --- | --- | --- | --- |
| URL travels inside the SSH session (no extra open port, no third party) | ✅ Unix socket forward | ✅ Unix socket forward | ❌ TCP port, no auth or encryption | ⚠️ TCP port via `ssh -R` | ❌ ntfy relay | ❌ SSHes back into the local machine |
| OAuth `localhost` callbacks reach the remote host | ✅ | ❌ | ❌ rewrites the URL host only | ❌ | ✅ via `ssh -W` | ❌ rewrites the URL host only |
| Local machine: macOS and Linux | ✅ | ✅ | ⚠️ documented for Windows | ❓ not stated | ⚠️ macOS (Linux unofficial) | ✅ |
| Drop-in `xdg-open` / `$BROWSER` on the remote host | ✅ both, installed by `rowser remote install` | ✅ shell shims (need `nc`) | ✅ symlink | ⚠️ `$BROWSER` only | ✅ `nssh infect` | ⚠️ `$BROWSER` only |
| Many concurrent SSH sessions, no root `sshd_config` change | ✅ per-session sockets | ❌ needs `StreamLocalBindUnlink` in `sshd_config` | ⚠️ one fixed port | ⚠️ one fixed port | ✅ | ⚠️ one fixed reverse port |
| Local control: trusted host name, scheme allowlist, rate limit, per-host confirmation | ✅ | ❌ | ❌ | ⚠️ http/https only | ⚠️ http(s) only; topic secrecy is the only auth | ❌ |
| Pick the local browser and Profile per remote host or URL | ✅ Routes | ❓ | ❓ | ❓ | ❓ | ❓ |
| See and control requests from the local terminal (status, activity, pause, approve) | ✅ | ❓ | ❓ | ❓ | ❓ | ❓ |
| Last commit | active | Oct 2026 | Apr 2022 ("maintainers wanted") | Nov 2022 | Sep 2026 | Jun 2021 |

✅ yes · ⚠️ partly · ❌ no · ❓ not documented or not checked. From each project's README and
source, November 2026; the browser-and-Profile and terminal-control rows are not yet checked
against the other projects. Corrections are welcome.

[opener]: https://github.com/superbrothers/opener
[lemonade]: https://github.com/lemonade-command/lemonade
[rht]: https://github.com/misterjoshua/rht
[nssh]: https://github.com/abizer/nssh
[local-open]: https://github.com/suan/local-open

## Status

- **v0.1.0 (the core): built.** The daemon, shim, `rowser ssh` wrapper, callback forwards,
  `rowser daemon install` (systemd and launchd), `rowser remote install` (with checksum
  verification) and `rowser doctor` are built and tested, including an end-to-end test against a
  real `sshd`. Still open: the macOS notification fix
  ([#57](https://github.com/jhnhnsn/rowser/issues/57)) and the manual checks, such as
  `--confirm-hosts` ([#20](https://github.com/jhnhnsn/rowser/issues/20)).
- **v1.0 ("CLI-complete"): built, released as v1.0.0-rc.2.** A config file with Routes (open a
  host's URLs in a given browser and Profile), `rowser browsers`, a control socket with terminal
  commands (`status`, `activity`, `pause`/`resume`, `pending`/`approve`/`deny`, `reload`), and
  rowser as the default browser for the whole user session on a remote host. Before v1.0.0:
  manual checks on macOS and Linux desktops.
- **Release binaries:** v1.0.0-rc.2 is on
  [GitHub Releases](https://github.com/jhnhnsn/rowser/releases). `rowser remote install`
  downloads the release that matches the local `rowser` version; `--local-build` installs your
  own build instead.
- **After v1.0:** a menu bar / tray app that uses the control socket
  ([#35](https://github.com/jhnhnsn/rowser/issues/35)).

Progress is tracked in [#1](https://github.com/jhnhnsn/rowser/issues/1) (v0.1.0),
[#38](https://github.com/jhnhnsn/rowser/issues/38) (v1.0) and in [CHANGELOG.md](CHANGELOG.md).

## Install

Each Release after v1.0.0-rc.1 has `rowser-installer.sh` attached (v1.0.0-rc.1 shipped
before the installer; download its binaries from the Release page). Install the latest
release (Linux and macOS, x86_64 and arm64):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jhnhnsn/rowser/releases/latest/download/rowser-installer.sh | sh
```

A pre-release is not `latest` on GitHub. To install a pre-release or any given version, put
its version in the URL:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jhnhnsn/rowser/releases/download/v<version>/rowser-installer.sh | sh
```

Until v1.0.0, every release is a pre-release, so `latest` finds none. Install the current
release candidate with:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jhnhnsn/rowser/releases/download/v1.0.0-rc.2/rowser-installer.sh | sh
```

The script checks the download against the Release's `SHA256SUMS` and installs only the
`rowser` binary to `~/.local/bin` (set `ROWSER_INSTALL_DIR` to pick another folder). It uses
no `sudo` and writes nothing outside your home directory. Then run `rowser daemon install` on
the local machine, or `rowser remote install <host>` for a remote host.

Or download a binary from
[GitHub Releases](https://github.com/jhnhnsn/rowser/releases) (`x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`) and put `rowser`
on your `PATH`, or build it:

```sh
cargo install --git https://github.com/jhnhnsn/rowser
```

The Rust toolchain is pinned in `rust-toolchain.toml` (currently 1.100.0); rustup picks it up
in a checkout. To bump it, change `channel` there and the `dtolnay/rust-toolchain@<version>`
refs in `.github/workflows/ci.yml` (two steps) and `.github/workflows/release.yml`.

## Local setup

Install the daemon as a user service and start it:

```sh
rowser daemon install
```

On Linux this writes a systemd user unit (`~/.config/systemd/user/rowser.service`) bound to
`graphical-session.target`: the daemon runs while you are logged in to a desktop and stops with
it. If no graphical session is active (for example, you ran the install over SSH), the install
does not start it; it starts with your next graphical session. On macOS it writes a launchd
LaunchAgent.

Daemon flags given before `install` go into the service:

```sh
rowser daemon --confirm-hosts 'prod-*' --open-command 'firefox {url}' install
```

Remove the service with:

```sh
rowser daemon uninstall
```

You can also run the daemon in the foreground with `rowser daemon [flags]`.

### Daemon flags

- `--open-command "<cmd> {url}"` — the command that opens a URL. Default `xdg-open {url}` on
  Linux, `open {url}` on macOS. `{url}` is replaced with the URL as one argument. The command is
  split on spaces, with **no quoting or escaping**: `--open-command 'my browser {url}'` runs `my`
  with the arguments `browser` and the URL. Use a path without spaces, or a small wrapper script.
  This is what the `system` Target runs (see Routes).
- `--confirm-hosts <pattern>` — glob, may repeat. URLs from matching hosts wait until you click
  "Open" in the notification (or a dialog where notifications have no actions), or answer with
  `rowser approve` (see Control the daemon). Matches the host alias you typed to `rowser ssh`.
  Default: no confirmation. If you do not answer within 60 seconds, the confirmation ends and
  the URL does not open. It works on top of the Routes: a matching host asks first, then opens
  in its Route's Target.
- `--confirm-unverified` — also match `--confirm-hosts` against the host name the remote host
  reports about itself on the direct socket (the `ssh_config` setup below). That name is
  untrusted: a remote host can report any name. Without this flag, `--confirm-hosts` only
  matches the trusted names from `rowser ssh`.
- `--runtime-dir <dir>` — directory for the daemon's sockets (see Runtime directory).

### Runtime directory

The daemon's sockets live in `$XDG_RUNTIME_DIR/rowser/` on Linux and
`~/Library/Application Support/rowser/` on macOS. Set `ROWSER_RUNTIME_DIR` (an absolute path) to
use another directory; both `rowser daemon` and `rowser ssh` read it, so one variable sets both
sides. `rowser daemon --runtime-dir <dir>` wins over the variable. `rowser daemon install` writes
the directory from either one into the service.

## Remote setup

From your local machine, install rowser on a remote host:

```sh
rowser remote install myhost
```

This detects the remote architecture (`ssh myhost uname -sm`), downloads the matching release
binary, checks it against the release's `SHA256SUMS` (and refuses to install on a mismatch or
a missing entry), copies it to `~/.local/bin/rowser` on the host, and creates the `xdg-open`,
`sensible-browser` and `www-browser` symlinks next to it. Use `--local-build <path>` to copy a
binary you built instead of the download (no checksum check).

### Default browser on the remote host

The install makes rowser the default browser for your whole user session on the host, not only
for login shells. Nothing is installed system-wide.

It writes `~/.config/environment.d/rowser.conf` on the host, so systemd user services get
`BROWSER=rowser` and `~/.local/bin` first on `PATH`. This applies from the next login. The
file's first line marks it as rowser's: a later install overwrites only that file. If you
already have your own `rowser.conf` there, the install keeps it and prints a warning.

It also writes `~/.local/share/applications/rowser.desktop` and makes it the default handler
for `http` and `https` links, so desktop apps on the host open links through rowser. It uses
`xdg-mime` when the host has it; else it sets the two entries under `[Default Applications]` in
`~/.config/mimeapps.list` and keeps your other lines. If `mimeapps.list` is a symlink, the
install keeps it and prints a warning.

Then, on the remote host, add this line to your shell rc:

```sh
export BROWSER=rowser
```

`rowser <url>` acts as `rowser open <url>`, so programs that use `$BROWSER` work. Make sure
`~/.local/bin` comes before `/usr/bin` on `PATH`, so the `xdg-open` symlink wins.

Check the setup on the remote host (inside a `rowser ssh` session) and on the local machine.
`rowser doctor` runs the local checks when a config file or the runtime directory exists, and
the remote checks inside an SSH session or when neither side is found:

```sh
rowser doctor
```

It warns when the browser symlinks in `~/.local/bin` do not resolve to rowser, when
`~/.local/bin` is not before `/usr/bin` on `PATH` or `BROWSER` is not set or does not resolve to rowser, and
does a live round trip to the daemon. It also checks the default-browser setup: that
`rowser.conf` sets `BROWSER=rowser` and `PATH`, that `rowser.desktop` exists and is the default
for `http` and `https` (from `xdg-mime query default`, else `mimeapps.list`), and, with a
graphical session, that a real `xdg-open` exists for the fallback (see Usage). Each failed
check prints one warning with its fix.

On the local machine, `rowser doctor` checks that the config file parses (or prints the file and
line of the error), that the browser and Profile of each Route, of `[unverified]` and of
`[default]` exist (and where the request falls
back if not), and that the daemon answers, with its version.

## Usage

Connect with `rowser ssh` instead of `ssh`. It runs plain `ssh` with every argument unchanged,
plus a tunnel from a per-session socket to the daemon:

```sh
rowser ssh myhost
```

Most people alias it:

```sh
alias ssh='rowser ssh'
```

If the daemon is not running, `rowser ssh` prints a warning and runs `ssh` without a tunnel.

On the remote host:

```sh
xdg-open https://example.com      # or: rowser open https://example.com
rowser forward 8085               # forward a callback port by hand (1024–65535)
```

With no tunnel, the shim prints `rowser: no tunnel; open this URL manually: <url>` and exits 1.
On a remote host with a desktop (`WAYLAND_DISPLAY` or `DISPLAY` set), it instead hands the URL
to the first `xdg-open` on `PATH` that is not rowser itself, so local browsing on that host
still works.

### OAuth callbacks

rowser finds the callback port from `redirect_uri`, `redirect_url` or `callback` query
parameters pointing at `localhost`, `127.0.0.1` or `[::1]`, from the URL's own port when it points
at one of those, and (on Linux) from loopback ports the calling program is listening on. The
daemon binds that port on loopback on your local machine and relays each connection through the
Link to the Saved host, where `rowser __outpost` dials the remote port. `rowser open` exits as
soon as the daemon accepts the URL; no process stays behind on the remote host. The forward
closes when the remote port stops listening, when a dial to it fails, when the Link drops, or
after 10 minutes. Use `rowser forward <port>` when detection misses a port. Callback forwards
need a Saved host: a request through a `rowser ssh` session still opens the URL, but rowser
warns that the port is not forwarded.

### tmux

The shim uses the newest tunnel socket in `/tmp` that you own and that answers the handshake. A
tmux session reattached from a new SSH session therefore uses the new connection; nothing to
update.

### Environment variables

- `ROWSER_SOCKET` — a socket path the shim (and `rowser doctor`) tries before it looks in `/tmp`.
  Set it when the tunnel socket is somewhere else.
- `ROWSER_LOG` — the log level (for example `debug`). Default `warn`.
- `ROWSER_RUNTIME_DIR` — see Runtime directory.

### Jump hosts

`rowser ssh` passes `-J` / `ProxyJump` through unchanged. The tunnel ends at the final host, so
install rowser there, not on the jump host.

## Routes

A Route sends the URLs of one remote host (and, if you want, only some URLs) to one Target: a
browser on your local machine, optionally with a Profile. Without a config file, every URL opens
with `--open-command`, as before.

### Config file

Routes live in `$XDG_CONFIG_HOME/rowser/config.toml` (default `~/.config/rowser/config.toml`), on
macOS and Linux:

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

- `[[route]]` entries are checked in file order; the first match wins.
- `host` is a glob (`*`, `?`, ASCII case ignored, as for `--confirm-hosts`) matched against the
  host alias you typed to `rowser ssh`.
- `match` (optional) is a list of globs matched against the URL's host (`*.google.com`,
  `localhost`). Without `match`, every URL matches.
- `open` is the Target:
  - `system` — the `--open-command` (default `open {url}` / `xdg-open {url}`).
  - `ask` — wait for your answer, as `--confirm-hosts` does. "Open" uses the `[default]` Target
    (`system` when `[default]` is also `ask`); `rowser approve <id> --open <target>` can pick
    another.
  - `<browser>` or `<browser>:<profile>` — for example `safari`, `chrome:Work`, `firefox:Dev`.
    Browser names are the short names from `rowser browsers`. The Profile is a folder name
    (`Profile 2`) or a display name (`Work`); the folder name is tried first.
- `ask = true` (optional) uses the Route's Target only after you approve.
- `[default]` catches every request no Route matched. Without it, the default is `system`.
- If a Target's browser or Profile is missing, the URL opens in `[default]` and the notification
  and `rowser activity` say so.

The daemon reads the file at start, on `rowser reload`, and after each `rowser route` change. A
missing file means no Routes. An invalid file stops `rowser daemon` from starting, with the file,
line and reason. Flags win over the file.

### Unverified hosts

Routes match only the trusted host alias from `rowser ssh`. A request that comes in on the
direct socket (the `ssh_config` setup below) carries only the name the remote host reports about
itself, so it never matches a `[[route]]`: it goes to `[unverified]`, whose default is `ask`. A
remote host that reports a false name therefore cannot reach a named Route.

### `rowser host`

A Saved host is a `[[host]]` entry in the config file. The daemon keeps one Link (an `ssh`
connection) to each Saved host, so programs there open URLs without `rowser ssh`. The `host`
commands edit the config file in place, keep your comments, and work when the daemon is stopped.

```sh
rowser host add g14                       # finds rowser on g14 and saves its path
rowser host add desk --no-default-browser # a host with its own desktop
rowser host list
rowser host connect g14                   # log in once, for a key that needs a touch or passphrase
rowser host remove g14
```

```
g14  connected  since 2026-11-27 10:02:11  last connected 2026-11-27 10:02:11  outpost 1.0.0
desk  needs login  since 2026-11-27 10:02:12  last connected never  outpost -
```

- `host add` runs `command -v rowser` in your login shell on the host and saves the path as
  `remote_path`. If rowser is not there, or `rowser --version` there gives another version, it
  runs `rowser remote install <host>` first and saves `~/.local/bin/rowser`. Use
  `--no-default-browser` for a host with its own desktop: the install then copies only the
  binary (no browser symlinks, `environment.d` file or `rowser.desktop`) and leaves the marker
  `~/.local/share/rowser/no-default-browser` there. Adding a Saved host again updates its `remote_path` and
  `default_browser` (a plain `add` sets `default_browser` back to true); other keys and comments
  stay.
- `host list` shows each Saved host's Link state: `connected`, `connecting`, `needs login` or
  `version mismatch`, and `daemon not running` when the daemon is stopped. `outpost` is the
  rowser version on the host. With no Saved hosts it prints `no Saved hosts`.
- When the Outpost on a Saved host has another major version, the Link shows `version
  mismatch`. With `auto_update = true` (the default) the daemon runs the remote install for that
  host (with or without the default-browser part, as you added it), notifies you once
  ("updated rowser on <host> to <version>") and connects again. With `auto_update = false` in
  the `[[host]]` entry it notifies you once and waits for `rowser remote install <host>`.
- `host connect` exits 1 when the daemon has not loaded the host; run `rowser reload`, then
  connect again.
- A Link logs in with `BatchMode`. If that fails, the Link shows `needs login` and stops
  retrying. `host connect` runs `ssh` in your terminal, so it can ask for a passphrase or a key
  touch, and keeps a background master connection on rowser's own control path. The Link then
  connects again through it.
- `host remove` removes the entry and closes the Link.

### `rowser route`

Change and check the Routes from the terminal. The commands edit the config file in place and
keep your comments and layout. They work when the daemon is stopped; a running daemon reloads
the file after each change.

```sh
rowser route add g14 --match '*.google.com' --open chrome:Work
rowser route add g14 --open safari
rowser route add 'prod-*' --open firefox --ask
```

```
rowser: added route 1
rowser: config reloaded (1 route)
```

`route add` writes a Profile's display name as its folder name, with the display name as a
comment (`open = "chrome:Profile 2"  # Work`). An invalid Target is refused and the file does not
change. A browser or Profile that is not found gives a warning, and the Route falls back to
`[default]` until it is found.

```sh
rowser route list
```

```
1  g14  match *.google.com  chrome:Profile 2
2  g14  safari
3  prod-*  firefox  ask
[unverified]  ask
[default]  system
```

Use the numbers from `route list` to remove or move a Route. Each example below and in the
sections that follow starts from the `route list` above; they are not steps in order.

```sh
rowser route move 2 1     # rowser: moved route 2 to 1
rowser route remove 3     # rowser: removed route 3
```

`route test` prints the Target and the Route that a URL from a host takes, and opens nothing.
Add `--unverified` to test a reported host name from the direct socket:

```sh
rowser route test g14 https://accounts.google.com/o/oauth2/auth    # chrome:Profile 2  route 1
rowser route test --unverified g14 https://example.com             # ask  [unverified]
```

### `rowser browsers`

List the browsers found on the local machine, with their short names and Profiles
(`--json` for JSON):

```sh
rowser browsers
```

```
safari  Safari
chrome  Google Chrome  profiles: Personal (Default), Work (Profile 2)
firefox  Firefox  profiles: default-release (abcd1234.default-release)
```

On macOS the list follows the order below; on Linux, the order of the desktop files.

- macOS: Safari (`safari`), Google Chrome (`chrome`), Firefox (`firefox`), Microsoft Edge
  (`edge`), Brave (`brave`), Vivaldi (`vivaldi`), Chromium (`chromium`) and Arc (`arc`), from
  `/Applications` and `~/Applications`. A URL opens with `open -b <bundle id>`.
- Linux: the `.desktop` files in the XDG data directories that handle `x-scheme-handler/https`.
  A URL opens with the desktop file's `Exec` line, never through a shell.
- Profiles: Chromium-family browsers from their `Local State` file (opened with
  `--profile-directory=<folder>`); Firefox from `profiles.ini` (opened with `-P <name>`, which
  can start a second Firefox instance). Safari and Arc have no Profiles in rowser.

## Control the daemon

The daemon has a third socket, `rowserd-control.sock`, for rowser commands on the local machine.
No Tunnel reaches it from a remote host. Each command below prints
`rowser: the daemon is not running` and exits 1 when no daemon answers.

### `rowser status`

```sh
rowser status            # --json for one JSON object
```

```
daemon version:    0.1.0
protocol version:  1.1
runtime directory: /run/user/1000/rowser
config file:       /home/me/.config/rowser/config.toml
paused:            no
sessions:
  g14  12m 05s  safari
callback forwards:
  8085  g14  41s
```

A Session is one `rowser ssh` run. The list shows each, with how long each has run and the
Target of the host's first Route without `match` (`[default]`'s Target with no such Route). A
Session ends when `ssh` exits.

### `rowser activity`

The daemon's last 200 Open requests and `rowser forward` requests, newest last: time, host (the
reported host name is marked `(unverified)`), URL, the Route that matched, the Target and the
result (`opened`, `denied`, `timed out`, `paused`, `rate limited`, `scheme refused`,
`browser missing`). A Callback forward shows its ports and how long it stayed open.

```sh
rowser activity          # --follow to keep printing until Ctrl-C, --json for one object per line
```

```
2026-11-20 14:02:11  g14  https://accounts.google.com/o/oauth2/auth?…  route 1  chrome:Profile 2  opened  callback 8085 open 41s
2026-11-20 14:05:30  prod-1  https://example.com  route 3  firefox  opened  by terminal
```

The list is kept in memory only; a daemon restart clears it.

### `rowser pause` and `rowser resume`

```sh
rowser pause --for 1h    # rowser: paused until 2026-11-20 15:05:30
rowser resume            # rowser: resumed
```

While paused, the daemon refuses each Open request and `rowser forward`, and nothing is queued.
On the remote host, `xdg-open` prints `rowser: paused; open this URL manually: <url>` and exits 1,
and `rowser forward` prints `rowser: paused; port <p> was not forwarded`. Without `--for` the
pause lasts until `rowser resume`. A daemon restart ends a pause.

### `rowser pending`, `rowser approve`, `rowser deny`

Requests that wait for an answer (from `ask`, `ask = true` or `--confirm-hosts`) can be answered
from the terminal as well as from the notification or dialog. The first answer wins.

```sh
rowser pending
```

```
1  prod-1  https://example.com  firefox  8s
```

```sh
rowser approve 1                    # rowser: approved 1
rowser approve 1 --open chrome:Work # open in another Target
rowser deny 1                       # rowser: denied 1
```

An id that is unknown or already answered prints `rowser approve: no pending request <id>` (or
`rowser deny: …`) and exits 1. `--open ask`, an unknown browser, and `--open` on a
`rowser forward` request are refused the same way, and the request keeps waiting.

### `rowser reload`

```sh
rowser reload            # rowser: config reloaded (2 routes)
```

The daemon reads the config file again. An invalid file is refused with the file, line and
reason, the daemon keeps its old Routes, and the command exits 1.

## Alternative: `ssh_config` without the wrapper

If you cannot use `rowser ssh`, tunnel the daemon's direct socket in `~/.ssh/config`:

```
Host myhost
    RemoteForward /tmp/rowser-%C.sock /run/user/1000/rowser/rowserd.sock
```

Replace `/run/user/1000/rowser` with your runtime directory. On macOS the path has a space, so
quote it:

```
Host myhost
    RemoteForward /tmp/rowser-%C.sock "/Users/me/Library/Application Support/rowser/rowserd.sock"
```

The remote host creates the socket, so only the remote `sshd_config` setting
`StreamLocalBindUnlink yes` removes a stale socket from an earlier session; the same option in
your own `~/.ssh/config` has no effect on a `RemoteForward`. Without the remote setting, a stale
socket blocks the tunnel. In this setup the host name comes from the remote
host itself, so notifications mark it as unverified, and `--confirm-hosts` ignores it unless the
daemon runs with `--confirm-unverified`. Its URLs skip the `[[route]]` entries and go to
`[unverified]` (see Unverified hosts).

## Security model

- The daemon accepts only `http` and `https` URLs.
- It listens on three Unix sockets, mode 0600 in a 0700 directory:
  - `rowserd-relay.sock` — the relay socket, for `rowser ssh` only. The wrapper stamps each
    connection with the host alias you typed. A remote host cannot choose which local socket its
    tunnel reaches, so that name is trusted.
  - `rowserd.sock` — the direct socket, for `ssh_config` tunnels. The host name is what the
    remote host reports, and it is shown as untrusted. It never matches a named Route.
  - `rowserd-control.sock` — the control socket, for rowser commands on the local machine
    (`status`, `activity`, `pause`, `pending`, `reload`, …). No Tunnel reaches it.
- Rate limit: at most 5 URLs per 10 seconds per connection; more are rejected with an error.
- By default URLs open immediately with a notification. Use `--confirm-hosts` to require a click
  for hosts you trust less.
- Callback forwards bind on loopback only (`127.0.0.1` and `[::1]`), ports 1024–65535.
- rowser never changes files outside your home directory, apart from its own sockets in `/tmp` and in its runtime directory.

## Known limits

- Per-session sockets need `rowser ssh` (or an alias). Without root on the remote host, plain
  `ssh_config` cannot avoid stale sockets.
- The callback port is fixed by the remote program's `redirect_uri`. If that port is busy on
  your local machine, the URL still opens, but the callback cannot be forwarded; rowser shows a
  warning.
- `--open-command` splits on spaces and has no quoting.
- macOS as a remote host works only through `BROWSER=rowser`; rowser does not replace `open`.
  `rowser remote install` refuses a macOS host: copy the `aarch64-apple-darwin` or
  `x86_64-apple-darwin` binary to it by hand, then set `BROWSER=rowser`.

## Out of scope for v1.0

mosh, Windows (local), replacing `open` on macOS remotes, clipboard, file transfer, a menu bar or
tray app ([#35](https://github.com/jhnhnsn/rowser/issues/35)), a browser chooser window, Safari
and Arc Profiles, a system-wide install on the remote host.

## License

MIT.
