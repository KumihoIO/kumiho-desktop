# Kumiho Desktop 0.4.1

## 9miho runtime lifecycle

An update wrote the new 9miho to disk and then reported success while the
previous runtime kept serving on 9999, so the app went on showing the old
version until the process was killed by hand. On Windows the same root cause
failed the update outright, because the binary being replaced was still running.

- Desktop now records which build it actually spawned, separately from which
  build is installed on disk, and reports both. A runtime that does not match
  the installed build is marked stale and restarted rather than adopted.
- Stopping 9miho also retires runtimes left behind by an earlier Desktop
  session, and waits for port 9999 to close before returning. On macOS and
  Linux the runtime gets SIGTERM before SIGKILL, so it can remove its extracted
  temporary directory instead of leaking it.
- The whole runtime lifecycle — stop, binary swap, start — is serialized, so the
  three UI paths that can request a start no longer each spawn their own
  runtime, and a start can no longer land in the middle of an update.
- Install and update restart the runtime themselves. When only the restart
  fails — no Community Edition or Cloud mode chosen yet, or a missing cloud
  token — the update is still reported as installed with the restart failure
  named, instead of sending you back to re-download what already landed.
- The Apps view shows "restart required" when the installed and serving
  versions diverge.

The bundled offline 9miho fallback stays at 0.4.0; online updates continue to
come from the signed public runtime feed.
