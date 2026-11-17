# SSH-forwarded Unix socket is the only transport

The Shim reaches the Daemon only through an SSH remote forward of a Unix socket. We rejected TCP
ports (any user on the Remote host can connect to a loopback port, so we would need our own auth)
and pub/sub relays such as ntfy (nssh's approach, which supports mosh but sends every URL through
a third party). SSH already authenticates and encrypts, and a 0600 socket limits access to the
user and root.

## Consequences

- mosh is not supported: it has no SSH channel to carry the Tunnel. Do not "fix" this by adding a
  relay transport without revisiting this ADR.
- rowser has no crypto or credentials of its own.
