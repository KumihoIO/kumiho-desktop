# Windows security notices — "Trojan", "unrecognized app", "unwanted app"

**Short version: this is a false positive, and the installer is safe.** Windows
Defender's machine-learning heuristic flags the Kumiho Desktop installer because
the Windows build is not code-signed *yet* — not because the file contains
malware. This page explains what you're seeing, how to confirm the download is
genuine, and how to let it through.

## What you might see

Downloading or running the Windows installer, Windows may show one or more of:

| Notice | Meaning |
|---|---|
| **"Windows protected your PC"** (blue SmartScreen box) | The app has no reputation with Microsoft yet. Not a virus warning. |
| **`Trojan:Win32/Wacatac.B!ml`** ("Threat blocked" / "Threat found") | A machine-learning *guess*, not a signature match — see below. |
| **"…block potentially unwanted apps…"** | A generic PUA heuristic, same root cause. |

All three come from the same underlying reason, explained next.

## Why this happens (and why it's a false positive)

- **`!ml` means "the model guessed."** The `!ml` suffix on
  `Trojan:Win32/Wacatac.B!ml` marks it as a **machine-learning heuristic**
  detection, not a match against a known-malware signature. `Wacatac.B!ml` is one
  of the most common generic labels Defender applies to *legitimate, unsigned*
  installers (Tauri, Electron, PyInstaller, indie tools, game mods, and so on).
- **The Windows build isn't code-signed yet.** Kumiho Desktop's macOS builds are
  Apple-signed and notarized, and every auto-update is Minisign-verified — but
  the Windows `.exe`/`.msi` do not yet carry an Authenticode signature. An
  unsigned binary that Microsoft has never seen before, freshly built with zero
  SmartScreen reputation, is exactly what this heuristic over-flags.
- **Brand-new file, zero reputation.** Each release is a new binary. Reputation
  builds up as more people download and run it, so early downloads of a new
  version are the most likely to be flagged.

A genuine trojan would be caught by a specific named signature across many
antivirus engines — not a single vendor's `!ml` guess.

## Confirm your download is genuine

You don't have to take our word for it. Verify the source and the file:

1. **Download only from the official Releases page:**
   <https://github.com/KumihoIO/kumiho-desktop/releases>
   The file must come from `github.com` or `release-assets.githubusercontent.com`.
   If Windows shows the source URL in the alert, confirm it points there.
2. **Scan it on [VirusTotal](https://www.virustotal.com/).** For this false
   positive you'll typically see only one or two ML-based engines flag it
   (`Wacatac`, `!ml`, or a generic "unwanted") while the rest report the file
   clean. A *real* infection looks the opposite: many engines agreeing on a
   specific malware name.

If instead you see broad agreement across engines on a specific threat name, stop
and [open an issue](https://github.com/KumihoIO/kumiho-desktop/issues) — that
would not be a normal false positive.

## Let the installer through

Since the file is safe, allow it:

- **At the SmartScreen box:** click **More info → Run anyway**.
- **If Defender already quarantined it:** open **Windows Security → Protection
  history**, select the Kumiho Desktop item, then **Actions → Restore** (or
  **Allow**). Then run the installer again.
- **If it was removed mid-download:** re-download from the Releases page and
  choose **Keep** if the browser warns.

After installation, Kumiho Desktop updates itself through the in-app updater,
whose packages are Minisign-verified — so you generally won't repeat this dance
on every release.

## Help clear the flag for everyone

Reporting the false positive to Microsoft gets it cleared for all users, usually
within a day or two:

- **Submit the installer to Microsoft:**
  <https://www.microsoft.com/en-us/wdsi/filesubmission> — choose "I'm a software
  developer" / "This is a false positive," and attach the flagged `.exe`.

The durable fix is Authenticode code signing for the Windows builds (an EV
certificate or Azure Trusted Signing), which removes both the SmartScreen prompt
and the ML false positive. Until that lands, the guidance above is the safe path.

## macOS and Linux

- **macOS** builds are Apple Developer ID **signed and notarized** — no manual
  override needed.
- **Linux** `.deb`/`.AppImage` are not affected by this Windows-specific
  heuristic.
