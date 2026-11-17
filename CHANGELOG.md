# Changelog

All notable changes to rowser are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rowser uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

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
