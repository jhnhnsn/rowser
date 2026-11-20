# Routes match the Host alias only

A Route's `host` glob is matched against the Host alias (from the Wrapper's Host stamp), never
against a Reported host name. Requests that carry only a Reported host name (direct socket,
`ssh_config` setups) go to one `[unverified]` entry, which asks the user by default. A Route can
send URLs to a browser profile where the user is signed in to sensitive accounts; if a Shim could
choose its Route by claiming a name, a compromised Remote host could open pages in that profile
without asking.

## Consequences

- `ssh_config` users cannot route per host. They get one `[unverified]` Target for all such
  hosts, or they switch to `rowser ssh`.
- `--confirm-unverified` keeps its v0.1.0 meaning; `[unverified] open = "ask"` gives the same
  effect for every unverified host.
