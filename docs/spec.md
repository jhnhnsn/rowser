# rowser v1 spec

rowser lets programs on a headless Linux host open URLs in the browser on your local
machine (macOS or Linux), over SSH. When the URL starts an OAuth flow with a `localhost`
callback, rowser forwards that callback back to the remote host, so CLI logins work.

This spec is the outcome of a design grilling (decisions Q1–Q32). Where a ticket disagrees
with this spec, the spec wins unless the user decides otherwise.

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
`rowser open`).

### Daemon (local) — `rowser daemon`

- Runs as a systemd user unit (Linux) or a launchd LaunchAgent (macOS).
  `rowser daemon install` writes and starts it; `rowser daemon uninstall` removes it.
- Listens on two Unix sockets in its runtime directory:
  - Linux: `$XDG_RUNTIME_DIR/rowser/`; macOS: `~/Library/Application Support/rowser/`.
    Overridable with `ROWSER_RUNTIME_DIR` (read by `rowser daemon` and `rowser ssh`) and, for
    `rowser daemon`, with `--runtime-dir`, which wins over the variable.
  - `rowserd.sock` — direct socket, for `ssh_config` forwards. Host stamps are rejected here;
    the host name is the shim's untrusted reported name.
  - `rowserd-relay.sock` — relay socket, for Wrappers only. The first frame after the
    handshake must be a host stamp. A remote host cannot choose which local socket its tunnel reaches, so the stamp is
    trusted.
  - Sockets mode 0600; directory 0700.
- Accepts only `http` and `https` URLs. Every other scheme is rejected with an error to the shim.
- Opens URLs immediately and shows a desktop notification naming the host (see Wrapper for where
  the host name comes from).
- `--confirm-hosts <pattern>`: URLs from matching hosts wait until the user clicks "Open" in the
  notification (or a dialog where notification actions are unavailable). Default: no
  confirmation.
- `--open-command "<cmd> {url}"`: the command that opens a URL. Default `xdg-open {url}` on
  Linux, `open {url}` on macOS.
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

### Shim (remote) — `rowser open <url>` (and the symlinks)

- Socket discovery: tries `ROWSER_SOCKET` if set, then every `/tmp/rowser-*.sock` owned by the
  shim's own UID (others are ignored), newest first (by mtime). Uses the first one that answers the handshake. Removes sockets it owns that
  refuse connections (stale). Because discovery is by glob, a tmux session reattached from a new
  SSH session finds the new socket.
- No working socket: prints `rowser: no tunnel; open this URL manually: <url>` to stderr and
  exits 1.
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
     ancestors) is listening on, found through `/proc`.
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
  downloads the matching release binary (or uses a local build), copies it to
  `~/.local/bin/rowser`, creates the symlinks, and prints the shell line for `BROWSER`.
- `rowser doctor` (remote): warns when `~/.local/bin` is not before `/usr/bin` on `PATH`, when
  `BROWSER` is not set, and does a live round trip through the tunnel (ping → daemon version).
- rowser never changes files outside the user's home directory (and its own sockets in `/tmp`).

## Protocol

Newline-delimited JSON over the Unix socket. The first message on every connection is a
handshake carrying a protocol version `major.minor`. On a major mismatch both sides fail with a
clear error naming both versions and the update command.

## Configuration

No config file in v1. Flags and environment variables only (`ROWSER_SOCKET`, `ROWSER_LOG`, `ROWSER_RUNTIME_DIR`, …).

## Stack

tokio, clap, serde / serde_json, url, notify-rust, tracing.

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
  and a Homebrew tap come after v1.

## Out of scope for v1

mosh, Windows (local), replacing `open` on macOS remotes, clipboard, file transfer, a config
file.

## Known limits

- Per-session sockets need `rowser ssh` (or an alias). Without root on the remote host, plain
  `ssh_config` cannot avoid stale sockets.
- The callback port is fixed by the remote program's `redirect_uri`, so a busy local port cannot
  be worked around.
