# Changelog

All notable changes to rowser are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rowser uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

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
