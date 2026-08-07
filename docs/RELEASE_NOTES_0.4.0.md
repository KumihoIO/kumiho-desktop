# Kumiho Desktop 0.4.0

## Component updates

- 9miho now checks the public binary-only `KumihoIO/9miho-release` feed,
  downloads the latest platform archive, verifies its SHA-256 and component
  Minisign signature, validates the embedded manifest, and replaces the local
  runtime while preserving project data.
- The bundled 9miho runtime is updated to 0.4.0 and remains available as an
  offline fallback.
- Kumiho Memory now reports the actual `kumiho-memory` engine version installed
  in each host-owned Python environment, checks the canonical PyPI release, and
  updates the affected environments without modifying a global Python install.
- Claude Code, ChatGPT/Codex, and OpenClaw adapter versions remain visible and
  independently installable/updateable under Settings > Agents.
- Memory View (Kumiho Brain) remains bundled and version-locked with Desktop.

After a Kumiho Memory engine or adapter update, restart the affected agent host.
After a 9miho update, Desktop restarts the runtime and confirms the installed
version from its local manifest.
