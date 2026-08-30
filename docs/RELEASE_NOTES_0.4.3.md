# Kumiho Desktop 0.4.3

## CE setup failures finally say why

A tester's Community Edition setup died at step 4 ("Start the server") with
nothing but a red circle. The server had exited because the Neo4j password in
`~/.kumiho/server.toml` didn't match the one its database container was created
with — but Desktop discarded the server's output, and the wizard's own log
panel turned out to be wired to an element id that didn't exist, so every
diagnostic it built was dropped before reaching the screen. Diagnosing that
machine took a day and a second AI agent.

- The CE server's output now lands in `~/.kumiho/logs/kumiho_server.log`
  (fresh per start), and starting watches the process for up to ten seconds:
  a server that dies on a bad config or wrong password reports its actual
  error — with the log tail — instead of a generic 40-second timeout.
- The wizard's timeout message now includes that log tail, and when the log
  shows a Neo4j authentication failure it says so directly: the container
  keeps the password it was first created with, so setup must be re-run with
  that original password.
- The wizard's log panel (and the cloud panel's) actually renders now — the
  `setLog` helper only tried prefixed element ids and silently dropped every
  message aimed at `ce-log`/`cloud-log`.
- Setup always rewrites `server.toml`, so the password you type is the
  password the server uses. Previously a re-run kept the old file, which is
  how the config and the container drifted apart in the first place.
- Starting CE while it is already serving no longer spawns a doomed duplicate
  (which used to truncate the running server's log and report a bind failure
  for a healthy system).
