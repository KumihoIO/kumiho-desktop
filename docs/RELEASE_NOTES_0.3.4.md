# Kumiho Desktop 0.3.4

This patch release restores the 9miho update path and bundles the signed 9miho 0.3.0 runtime.

## Changes

- Show **Update** instead of **Reinstall** when the bundled 9miho runtime is newer than the installed version.
- Update 9miho without replacing projects, assets, data, or logs, then restart the runtime.
- Re-read the installed manifest after restart so Apps shows the newly installed version immediately.
- Avoid treating an older bundled runtime as an update when a newer 9miho version is already installed.
- Bundle the signed, platform-specific 9miho 0.3.0 sidecar on Windows, Linux, macOS Apple Silicon, and macOS Intel.
