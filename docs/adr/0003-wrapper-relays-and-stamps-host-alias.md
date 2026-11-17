# The Wrapper relays the Tunnel and stamps the Host alias

The Tunnel ends at a per-session socket owned by the Wrapper, not at the Daemon. The Wrapper
relays each connection to the Daemon and stamps it with the Host alias the user typed. Anything
the Shim says about itself can be forged by a compromised Remote host; the Host alias cannot, so
notifications and `--confirm-hosts` rely on it. The cost is that the Wrapper must stay alive for
the whole SSH session, so it supervises `ssh` as a child rather than `exec`ing it.

## Consequences

- The stamp is trusted because of *which socket* it arrives on, not because of its content. The
  Daemon has a direct socket (for `ssh_config` forwards; stamps rejected) and a relay socket (for
  Wrappers; first frame must be a stamp). A Remote host cannot choose which local socket its Tunnel
  reaches, because that is set by the user's local `ssh` command or config.
