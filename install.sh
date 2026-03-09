#!/bin/sh
# Broodlink installer — detects OS/arch, downloads the latest release, installs to /usr/local/bin.
# Usage:
#   Install:   curl -fsSL https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.sh | sh
#   Uninstall: curl -fsSL https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.sh | sh -s -- --uninstall
set -e

REPO="nevenkordic/broodlink"
INSTALL_DIR="/usr/local/bin"
BINS="broodlink beads-bridge coordinator heartbeat embedding-worker status-api mcp-server a2a-gateway"

# ─── Uninstall ───────────────────────────────────────────────────────────────
if [ "${1:-}" = "--uninstall" ]; then
  echo "Uninstalling Broodlink..."
  REMOVED=0
  for bin in $BINS; do
    if [ -f "$INSTALL_DIR/$bin" ]; then
      if [ -w "$INSTALL_DIR/$bin" ]; then
        rm "$INSTALL_DIR/$bin"
      else
        sudo rm "$INSTALL_DIR/$bin"
      fi
      REMOVED=$((REMOVED + 1))
    fi
  done
  if [ "$REMOVED" -gt 0 ]; then
    echo "Removed $REMOVED binaries from $INSTALL_DIR."
  else
    echo "No Broodlink binaries found in $INSTALL_DIR."
  fi
  echo "Broodlink uninstalled."
  exit 0
fi

# ─── Install ─────────────────────────────────────────────────────────────────

# Detect OS
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin) OS_TAG="apple-darwin" ;;
  Linux)  OS_TAG="unknown-linux-gnu" ;;
  *)      echo "Unsupported OS: $OS. Use install.ps1 for Windows." >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH_TAG="x86_64" ;;
  arm64|aarch64) ARCH_TAG="aarch64" ;;
  *)             echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET="${ARCH_TAG}-${OS_TAG}"

# Get latest release tag
printf "  Fetching latest release...  "
TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
if [ -z "$TAG" ]; then
  echo "FAILED" >&2; exit 1
fi
echo "${TAG}"

echo ""
echo "  Broodlink ${TAG}"
echo "  Platform: ${TARGET}"
echo ""

# Download to temp directory
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

ARCHIVE="broodlink-${TAG}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE}"
SHA_URL="${URL}.sha256"

echo "  [1/4] Downloading..."
curl -fSL --progress-bar "$URL" -o "${TMP}/${ARCHIVE}"
echo "        done"

printf "  [2/4] Verifying checksum...  "
curl -fsSL "$SHA_URL" -o "${TMP}/${ARCHIVE}.sha256"
cd "$TMP"
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 -c "${ARCHIVE}.sha256" >/dev/null 2>&1 && echo "OK" || { echo "FAILED" >&2; exit 1; }
elif command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "${ARCHIVE}.sha256" >/dev/null 2>&1 && echo "OK" || { echo "FAILED" >&2; exit 1; }
else
  echo "skipped (no sha256 tool)"
fi

printf "  [3/4] Extracting...          "
tar xzf "$ARCHIVE"
echo "done"

printf "  [4/4] Installing binaries... "
DIR="broodlink-${TAG}-${TARGET}"
if [ -w "$INSTALL_DIR" ]; then
  for bin in $BINS; do cp "$DIR/$bin" "$INSTALL_DIR/"; done
else
  echo ""
  echo "        (requires sudo for $INSTALL_DIR)"
  for bin in $BINS; do sudo cp "$DIR/$bin" "$INSTALL_DIR/"; done
fi
echo "done"

echo ""
echo "  ✓ Broodlink ${TAG} installed to ${INSTALL_DIR}"
echo ""
echo "  Get started:"
echo "    broodlink          Launch setup wizard + all services"
echo "    broodlink --help   Show all commands"
echo ""
echo "  To uninstall:"
echo "    curl -fsSL https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.sh | sh -s -- --uninstall"
echo ""
