# On Linux the keeper reads /proc to see that a Callback port stops listening

ADR 0004 says the keeper ends a Callback forward when the remote Callback port stops listening
(checked every 2 seconds). The keeper first checked this with a bind probe: a bind of the port
refused with "address in use" meant that a program still held it. That is wrong after the program
closes a callback connection first, which is the usual end of an OAuth callback (`Connection:
close`). The program's side of that connection stays in TIME-WAIT on the Callback port for about
60 seconds, the bind probe sees "address in use", and the keeper and the `ssh` session stay alive
for that time. SO_REUSEADDR on the probe does not cure it: a TIME-WAIT entry left by a listener
without SO_REUSEADDR (Python's default) still blocks the bind on Linux.

So on Linux the keeper reads the LISTEN (`0A`) entries in `/proc/net/tcp` and `/proc/net/tcp6`
and counts the port as listening only when one of them is on the loopback address it dials, or on
the unspecified address (a `[::]` listener counts for both families, as `/proc` does not show
IPv6-only). Other states, TIME-WAIT among them, do not count. On macOS, or when `/proc` is not
readable, it keeps the bind probe. Neither way connects to the program.

This keeps ADR 0004; it changes only how "stops listening" is detected.

## Considered options

- A connect probe: simple and portable, but it connects to the program, and a program that
  accepts one connection (many OAuth helpers do) would take the probe as its callback.
- Keep the bind probe and give the e2e test a longer deadline: this hides the 60-second delay that
  users see after each OAuth login.

## Consequences

- On macOS remotes the TIME-WAIT delay can still happen; the forward then ends up to about 60
  seconds late, which is harmless but keeps the `ssh` session open that long.
