# Kumiho Desktop 0.3.3

This patch release restores browser-compatible file drag and drop inside the embedded 9miho canvas and updates the bundled creative canvas to 9miho 0.1.3.

## Changes

- Let the embedded web canvas receive HTML5 file drag-and-drop events instead of having the Tauri window consume them.
- Add a release regression check for the required Tauri window configuration.
- Bundle the signed, platform-specific 9miho 0.1.3 sidecar on Windows, Linux, macOS Apple Silicon, and macOS Intel.
