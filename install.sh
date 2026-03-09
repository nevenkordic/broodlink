#!/bin/sh
# Broodlink installer — detects OS/arch, downloads the latest release, installs to /usr/local/bin.
# Usage: curl -fsSL https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.sh | sh
set -e

REPO="nevenkordic/broodlink"
INSTALL_DIR="/usr/local/bin"

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
echo "Fetching latest Broodlink release..."
TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
if [ -z "$TAG" ]; then
  echo "Failed to fetch latest release." >&2; exit 1
fi

ARCHIVE="broodlink-${TAG}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE}"
SHA_URL="${URL}.sha256"

echo "Installing Broodlink ${TAG} for ${TARGET}..."

# Download to temp directory
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" -o "${TMP}/${ARCHIVE}"
curl -fsSL "$SHA_URL" -o "${TMP}/${ARCHIVE}.sha256"

# Verify checksum
cd "$TMP"
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 -c "${ARCHIVE}.sha256"
elif command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "${ARCHIVE}.sha256"
else
  echo "Warning: no sha256 tool found, skipping checksum verification."
fi

# Extract
tar xzf "$ARCHIVE"
DIR="broodlink-${TAG}-${TARGET}"

# Install — use sudo if needed
if [ -w "$INSTALL_DIR" ]; then
  cp "$DIR"/broodlink "$DIR"/beads-bridge "$DIR"/coordinator "$DIR"/heartbeat \
     "$DIR"/embedding-worker "$DIR"/status-api "$DIR"/mcp-server "$DIR"/a2a-gateway \
     "$INSTALL_DIR/"
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo cp "$DIR"/broodlink "$DIR"/beads-bridge "$DIR"/coordinator "$DIR"/heartbeat \
       "$DIR"/embedding-worker "$DIR"/status-api "$DIR"/mcp-server "$DIR"/a2a-gateway \
       "$INSTALL_DIR/"
fi

echo ""
echo "Broodlink ${TAG} installed successfully!"
echo ""
echo "  Run 'broodlink' to launch the setup wizard and start all services."
echo ""
