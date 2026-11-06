#!/bin/sh
# Installs the sarvaex CLI: downloads the release for this machine, checks its checksum and
# puts `sarvaex` in ~/.local/bin (override with SARVAEX_INSTALL_DIR).
#   curl -fsSL https://raw.githubusercontent.com/Rustix69/SarvaexCLI/main/install.sh | sh
# Pin a version with SARVAEX_VERSION=0.1.0.
set -eu

REPO="Rustix69/SarvaexCLI"
BASE="${SARVAEX_DOWNLOAD_BASE:-https://github.com/$REPO/releases/download}"
DEST="${SARVAEX_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }
need curl
need tar
need uname

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  *) die "no pre-built download for $(uname -s) $(uname -m). Build from source: cargo install --git https://github.com/$REPO sarvaex-cli" ;;
esac

VERSION="${SARVAEX_VERSION:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$VERSION" ] || die "could not find a release. Build from source instead: cargo install --git https://github.com/$REPO sarvaex-cli"
fi

NAME="sarvaex-$VERSION-$TARGET"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "Downloading sarvaex $VERSION for $TARGET ..."
curl -fsSL "$BASE/v$VERSION/$NAME.tar.gz" -o "$TMP/$NAME.tar.gz" || die "download failed: $BASE/v$VERSION/$NAME.tar.gz"

if curl -fsSL "$BASE/v$VERSION/$NAME.tar.gz.sha256" -o "$TMP/$NAME.sha256" 2>/dev/null; then
  EXPECT="$(cut -d ' ' -f 1 "$TMP/$NAME.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then GOT="$(sha256sum "$TMP/$NAME.tar.gz" | cut -d ' ' -f 1)"
  elif command -v shasum >/dev/null 2>&1; then GOT="$(shasum -a 256 "$TMP/$NAME.tar.gz" | cut -d ' ' -f 1)"
  else GOT=""; fi
  if [ -n "$GOT" ]; then
    [ "$GOT" = "$EXPECT" ] || die "checksum mismatch, not installing"
    say "Checksum OK"
  fi
fi

tar xzf "$TMP/$NAME.tar.gz" -C "$TMP"
mkdir -p "$DEST"
install -m 0755 "$TMP/$NAME/sarvaex" "$DEST/sarvaex"
say "Installed $DEST/sarvaex"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) say "Add it to your PATH:  export PATH=\"$DEST:\$PATH\"" ;;
esac
say "Next:  sarvaex auth login"
