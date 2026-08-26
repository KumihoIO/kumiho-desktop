# Kumiho Desktop 0.4.4

## Safer Community Edition setup on macOS and existing databases

- Desktop now finds Docker Desktop from its standard macOS application path
  even when a GUI launch does not inherit the terminal's `PATH`. Intel and
  Apple silicon Homebrew command locations are also supported, and a stale CLI
  no longer hides a working fallback. Mutating Docker commands still run once.
- Neo4j passwords are validated before setup begins. New containers require at
  least eight characters, matching Neo4j's own minimum, and passwords containing
  quotes or backslashes are preserved correctly in `server.toml`.
- An existing Neo4j container is never silently removed or recreated. When its
  original password differs from the one entered in Desktop, setup explains the
  mismatch and keeps the database intact.
- A candidate CE configuration is committed only after both the server and Neo4j
  report healthy. Failed starts stop the candidate server and restore the prior
  configuration, so retrying cannot overwrite a known-working password. Desktop
  records the exact startup PID and process-start identity across app restarts;
  cleanup targets only that process and never every `kumiho_server` on the machine.
- Password-bearing candidate, backup, and final configuration files remain
  owner-readable only (`0600`) on macOS and Linux.
- Start, stop, restart, update, and automatic startup share one action lock.
  Runtime controls now stay disabled while an action is running, and Start is
  disabled whenever Community Edition is already serving.
- The release workflow runs the new CE setup regression checks on Windows,
  Linux, Apple silicon macOS, and Intel macOS builds.
