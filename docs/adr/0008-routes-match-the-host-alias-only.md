# Routes match the Host alias only

A Route's `host` glob is matched against the Host alias of the Saved host whose Link carried the
request, never against a name the Shim gives. A Route can send URLs to a browser profile where
the user is signed in to sensitive accounts; if a Shim could choose its Route by claiming a name,
a compromised Remote host could open pages in that profile without asking.

## Note (2026-11-27)

Before ADR 0009, requests on the direct socket (`ssh_config` setups) carried only a Reported
host name and went to one `[unverified]` entry that asked by default. ADR 0009 removes the direct
socket, so every request has a Host alias, and `[unverified]` and `--confirm-unverified` are
removed with it.
