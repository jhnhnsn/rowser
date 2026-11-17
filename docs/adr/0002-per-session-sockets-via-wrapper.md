# Per-session sockets through the Wrapper, not a fixed ssh_config path

opener-style tools use one fixed `RemoteForward` path. Without `StreamLocalBindUnlink yes` in the
Remote host's `sshd_config` (which needs root), the socket file outlives a dropped session and
every later session fails to bind it, and a second concurrent session cannot bind it at all.
`ssh_config` has no per-session token to make the path unique. So the Wrapper (`rowser ssh`)
chooses a random Session socket path for each session, and the Shim discovers sockets by glob
(`/tmp/rowser-*.sock`, only those owned by its own UID), newest first, removing Stale sockets it
owns. The name carries no user name because the Wrapper cannot know the remote user reliably.

## Considered options

- Fixed path plus `StreamLocalBindUnlink`: kept as a documented alternative, but needs root.
- `ssh_config` with `${ENV}` expansion: needs a shell function to set the variable per call, which
  is a wrapper anyway.

## Consequences

- Glob discovery is what makes tmux reattach work: a reattached shell finds the new session's
  socket without any environment change.
- Newest-first ordering decides which Local machine receives the URL when two are connected.
