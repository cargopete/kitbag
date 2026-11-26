#!/bin/sh
# Install kitbag from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/cargopete/kitbag/main/install.sh | sh
#
# Environment:
#   KITBAG_VERSION      release tag to install (default: latest), e.g. v0.1.3
#   KITBAG_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
set -eu

REPO="cargopete/kitbag"
BIN="kitbag"
INSTALL_DIR="${KITBAG_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${KITBAG_VERSION:-latest}"

say() { printf 'kitbag-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required"; }

need uname
need tar
need mkdir
if command -v curl >/dev/null 2>&1; then
  fetch() { curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q --https-only "$1" -O "$2"; }
else
  die "curl or wget is required"
fi

os="$(uname -s)"
arch="$(uname -m)"
case "$arch" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) die "unsupported CPU architecture: $arch (build from source: cargo install --git https://github.com/$REPO)" ;;
esac
case "$os" in
  Linux) target="$arch-unknown-linux-musl" ;;
  Darwin) target="$arch-apple-darwin" ;;
  MINGW* | MSYS* | CYGWIN*) die "on Windows, use install.ps1 or download the .zip from https://github.com/$REPO/releases" ;;
  *) die "unsupported OS: $os" ;;
esac

if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi
archive="$BIN-$target.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $archive ($VERSION)"
fetch "$base/$archive" "$tmp/$archive" || die "download failed: $base/$archive"
checksum="$BIN-$target.sha256"
fetch "$base/$checksum" "$tmp/$checksum" || die "checksum download failed: $base/$checksum"

expected="$(cut -d ' ' -f 1 < "$tmp/$checksum")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$archive" | cut -d ' ' -f 1)"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$archive" | cut -d ' ' -f 1)"
else
  die "sha256sum or shasum is required to verify the download"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual)"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$INSTALL_DIR"
src="$tmp/$BIN"
[ -f "$src" ] || src="$(find "$tmp" -type f -name "$BIN" | head -n 1)"
[ -n "$src" ] && [ -f "$src" ] || die "archive did not contain $BIN"
cp "$src" "$INSTALL_DIR/$BIN.tmp"
chmod 755 "$INSTALL_DIR/$BIN.tmp"
mv "$INSTALL_DIR/$BIN.tmp" "$INSTALL_DIR/$BIN"

say "installed $("$INSTALL_DIR/$BIN" --version) to $INSTALL_DIR/$BIN"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on your PATH; add it, e.g. export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
