#!/usr/bin/env sh
# Kumiho Brain installer.
#
# One-liner:
#   curl -fsSL https://raw.githubusercontent.com/KumihoIO/kumiho-SDKs/main/dashboard/scripts/install.sh | sh
#
# Downloads the prebuilt kumiho-brain binary for this platform from the
# newest brain-v* GitHub release, verifies it against the release checksums
# (fail-closed), and installs it. No Rust toolchain required.
#
#   VERSION=v0.1.0       pin a release (accepts v0.1.0 or brain-v0.1.0)
#   INSTALL_DIR=~/bin    override the install destination (default ~/.kumiho/bin)
set -eu

REPO="KumihoIO/kumiho-SDKs"
VERSION="${VERSION:-latest}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.kumiho/bin}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os:$arch" in
    Linux:x86_64) asset_platform="linux-x86_64" ;;
    Linux:aarch64 | Linux:arm64) asset_platform="linux-aarch64" ;;
    Darwin:arm64) asset_platform="macos-aarch64" ;;
    Darwin:x86_64) echo "Intel Macs are not supported. Kumiho Brain ships Apple Silicon (arm64) macOS only." >&2; exit 1 ;;
    *) echo "Unsupported platform: $os $arch" >&2; exit 1 ;;
esac

# This repo hosts several release families (sdk-v*, memory-v*, go/v*), so the
# releases/latest endpoint can't be used — resolve the newest brain-v* tag
# from the release list (newest first) instead.
api_base="https://api.github.com/repos/$REPO/releases"
if [ "$VERSION" = "latest" ]; then
    tag="$(curl -fsSL -H "User-Agent: kumiho-brain-installer" "$api_base?per_page=100" \
        | grep -o '"tag_name": *"brain-v[^"]*"' | head -n 1 | cut -d'"' -f4)"
    if [ -z "$tag" ]; then
        echo "No brain-v* release found in $REPO" >&2
        exit 1
    fi
else
    case "$VERSION" in
        brain-v*) tag="$VERSION" ;;
        v*) tag="brain-$VERSION" ;;
        *) tag="brain-v$VERSION" ;;
    esac
fi

# Asset names are deterministic: kumiho-brain-<platform>-vX.Y.Z.tar.gz
short_version="${tag#brain-}"
asset="kumiho-brain-$asset_platform-$short_version.tar.gz"
download_base="https://github.com/$REPO/releases/download/$tag"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
archive="$tmp/$asset"
curl -fsSL -o "$archive" "$download_base/$asset"

# Verify the download against the release checksums (fail-closed).
curl -fsSL -o "$tmp/checksums.txt" "$download_base/checksums.txt" || {
    echo "checksums.txt not found in release $tag; refusing to install unverified binary" >&2
    exit 1
}
expected="$(grep " $asset\$" "$tmp/checksums.txt" | awk '{print $1}' || true)"
if [ -z "$expected" ]; then
    echo "No checksum entry for $asset in checksums.txt; refusing to install unverified binary" >&2
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$archive" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$archive" | awk '{print $1}')"
else
    echo "Neither sha256sum nor shasum is available; cannot verify download" >&2
    exit 1
fi
if [ "$expected" != "$actual" ]; then
    echo "SHA256 mismatch for $asset" >&2
    exit 1
fi

mkdir -p "$INSTALL_DIR"
tar -xzf "$archive" -C "$tmp"
binary="$(find "$tmp" -type f -name kumiho-brain | head -n 1)"
if [ -z "$binary" ]; then
    echo "kumiho-brain not found in release archive" >&2
    exit 1
fi
cp "$binary" "$INSTALL_DIR/kumiho-brain"
chmod +x "$INSTALL_DIR/kumiho-brain"

echo "Installed kumiho-brain $short_version to $INSTALL_DIR"
echo ""
echo "Run it (serves on 127.0.0.1 and opens your browser):"
echo "  $INSTALL_DIR/kumiho-brain --open"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo ""
        echo "Optionally add it to your PATH:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac
