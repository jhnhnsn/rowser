# The CLI writes the config file; the Daemon only reads it

Routes live in `$XDG_CONFIG_HOME/rowser/config.toml`. The `rowser route` commands change that
file themselves, with `toml_edit` so the user's comments and layout stay, and then ask a running
Daemon to read it again over the control socket. The Daemon never writes the file. We chose
this over making the Daemon the only writer, because route commands then work when the Daemon is
stopped, hand edits and CLI edits are the same thing, and there is one copy of the truth on
disk.

## Consequences

- The Daemon reads the file at start, on `rowser reload`, and after each `rowser route` change.
  An invalid file at start stops the Daemon with the file, line and reason; an invalid file on
  reload is refused, and the Daemon keeps its old Routes.
- Two `rowser route` commands at the same moment can race. Each writes the whole file through a
  temporary file and a rename, so the file is never half written; the last one wins.
- A later UI changes Routes through the same code path as the CLI.
