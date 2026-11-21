# rowser

> [!WARNING]
> rowser is under active development and has no release yet. Expect rough edges and breaking
> changes until `v0.1.0`.
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
| Last commit | active | Oct 2026 | Apr 2022 ("maintainers wanted") | Nov 2022 | Sep 2026 | Jun 2021 |

✅ yes · ⚠️ partly · ❌ no · ❓ not documented. From each project's README and source, November
2026. Corrections are welcome.

[opener]: https://github.com/superbrothers/opener
[lemonade]: https://github.com/lemonade-command/lemonade
[rht]: https://github.com/misterjoshua/rht
[nssh]: https://github.com/abizer/nssh
[local-open]: https://github.com/suan/local-open

## Status

- **Proof of concept: done.** From a Mac as the local machine to a Linux remote host over
  `rowser ssh`, a URL opened on the remote host opens in the Mac's browser, and a `localhost`
  OAuth callback on the remote host is reached from the Mac's browser through the tunnel.
- **v0.1.0: nearly feature-complete.** The daemon, shim, `rowser ssh` wrapper, callback forwards,
  `rowser daemon install` (systemd and launchd), `rowser remote install` (with checksum
  verification) and `rowser doctor` are built and tested, including an end-to-end test against a
  real `sshd`. Before the release: a few small fixes and manual checks of macOS notifications and
  confirmation dialogs.
- **No release binaries yet.** Until `v0.1.0` is tagged, install with `cargo install --git` on both
  machines; `rowser remote install` needs a release to download from.
- **Planned, not started:** a menu bar / tray admin UI (macOS first, Windows later) and browser
  selection ([#35](https://github.com/jhnhnsn/rowser/issues/35)).

Progress is tracked in [#1](https://github.com/jhnhnsn/rowser/issues/1) and in
[CHANGELOG.md](CHANGELOG.md).

## Install

On your local machine, either download a binary from
[GitHub Releases](https://github.com/jhnhnsn/rowser/releases) (`x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`) and put `rowser`
on your `PATH`, or build it:

```sh
cargo install --git https://github.com/jhnhnsn/rowser
```

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
- `--confirm-hosts <pattern>` — glob, may repeat. URLs from matching hosts wait until you click
  "Open" in the notification (or a dialog where notifications have no actions). Matches the host
  alias you typed to `rowser ssh`. Default: no confirmation. If you do not click within 60
  seconds, the confirmation ends and the URL does not open.
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

Then, on the remote host, add this line to your shell rc:

```sh
export BROWSER=rowser
```

`rowser <url>` acts as `rowser open <url>`, so programs that use `$BROWSER` work. Make sure
`~/.local/bin` comes before `/usr/bin` on `PATH`, so the `xdg-open` symlink wins.

Check the setup from the remote host (inside a `rowser ssh` session):

```sh
rowser doctor
```

It warns when `~/.local/bin` is not before `/usr/bin` on `PATH` or `BROWSER` is not rowser, and
does a live round trip to the daemon.

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

### OAuth callbacks

rowser finds the callback port from `redirect_uri`, `redirect_url` or `callback` query
parameters pointing at `localhost`, `127.0.0.1` or `[::1]`, from the URL's own port when it points
at one of those, and (on Linux) from loopback ports the calling program is listening on. The
daemon binds that port on loopback on your local machine and relays connections back to the
remote port. The forward closes when the remote port stops listening, when a dial to it fails,
or after 10 minutes. Use `rowser forward <port>` when detection misses a port.

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
daemon runs with `--confirm-unverified`.

## Security model

- The daemon accepts only `http` and `https` URLs.
- It listens on two Unix sockets, mode 0600 in a 0700 directory:
  - `rowserd-relay.sock` — the relay socket, for `rowser ssh` only. The wrapper stamps each
    connection with the host alias you typed. A remote host cannot choose which local socket its
    tunnel reaches, so that name is trusted.
  - `rowserd.sock` — the direct socket, for `ssh_config` tunnels. The host name is what the
    remote host reports, and it is shown as untrusted.
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

## Out of scope for v1

mosh, Windows, replacing `open` on macOS remotes, clipboard, file transfer, a config file.

## License

MIT.
