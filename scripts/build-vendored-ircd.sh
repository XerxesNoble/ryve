#!/usr/bin/env bash
# Build the vendored ngIRCd binary from source.
#
# Usage:
#   ./scripts/build-vendored-ircd.sh [--prefix <install-dir>]
#
# By default the binary is placed at vendor/ircd/bin/ngircd.
# Pass --prefix to override (e.g. for CI artifact staging).
#
# Prerequisites (install via your system package manager):
#   macOS:  xcode-select --install    # provides cc + make
#   Linux:  apt install build-essential
#
# Pinned version is read from vendor/ircd/VERSION.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VERSION_FILE="$REPO_ROOT/vendor/ircd/VERSION"
VERSION="$(tr -d '[:space:]' < "$VERSION_FILE")"

PREFIX="$REPO_ROOT/vendor/ircd"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    *) echo "Unknown arg: $1" >&2; exit 1 ;;
  esac
done

BIN_DIR="$PREFIX/bin"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

echo "==> Building ngIRCd $VERSION"
echo "    source:  https://ngircd.barton.de/pub/ngircd/ngircd-$VERSION.tar.gz"
echo "    output:  $BIN_DIR/ngircd"

# Download
TARBALL="$BUILD_DIR/ngircd-$VERSION.tar.gz"
curl -fsSL "https://ngircd.barton.de/pub/ngircd/ngircd-$VERSION.tar.gz" \
  -o "$TARBALL"

# Extract
tar xzf "$TARBALL" -C "$BUILD_DIR"

# Build
cd "$BUILD_DIR/ngircd-$VERSION"

# Keep the dependency surface minimal so a fresh clone builds without extra
# system packages. ngIRCd's optional features (SSL, PAM, ident, tcp-wrappers,
# iconv, zlib) are all opt-in at configure time; we turn them off so the only
# hard requirement is a working cc + make. Ryve runs the daemon on localhost
# for workshop-scoped agent traffic, so plaintext-only + no PAM is acceptable.
CONFIGURE_ARGS=(
  --prefix="$BUILD_DIR/install"
  --without-iconv
  --without-ident
  --without-tcp-wrappers
  --without-pam
  --disable-ipv6
)

# Strip the `-g` debug flag from the ngIRCd autoconf default CFLAGS. On
# macOS, every `conftest` compile with `-g` triggers `dsymutil`, and
# `dsymutil` walks $TMPDIR via CoreFoundation's CFBundle scanner — an
# O(n) readdir over every sibling temp directory. On a busy developer
# machine with many ryve test runs, that turns each autoconf check into
# a minutes-long hang. We don't need debug symbols in the shipped daemon
# (we don't attach a debugger to it), so build with -O2 only.
export CFLAGS="-O2"

./configure "${CONFIGURE_ARGS[@]}" 2>&1 | tail -5
make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)" 2>&1 | tail -5
make install 2>&1 | tail -5

# Install the binary only. ngIRCd's `make install` drops the daemon at
# $prefix/sbin/ngircd; copy it into the vendor layout.
mkdir -p "$BIN_DIR"
cp "$BUILD_DIR/install/sbin/ngircd" "$BIN_DIR/ngircd"
chmod +x "$BIN_DIR/ngircd"

# Stamp the install with the version we just built so build.rs can detect
# a VERSION bump and re-run the script even when a binary is already on
# disk. Keep in sync with build_vendored_tmux_support::stamp_path().
printf '%s\n' "$VERSION" > "$BIN_DIR/.version"

echo "==> Installed ngircd $VERSION at $BIN_DIR/ngircd"
"$BIN_DIR/ngircd" --version
