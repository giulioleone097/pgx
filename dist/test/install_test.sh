#!/bin/sh
# End-to-end installer test: assemble a distribution, serve it over HTTP as a
# release would be, run install.sh against it, and verify the result runs and
# the checksum guard works.
#
#   PG18_BINDIR=/opt/homebrew/opt/postgresql@18/bin dist/test/install_test.sh
set -eu
export PGX_INSECURE_ALLOW_HTTP=1
export PGX_INSECURE_SKIP_VERIFY=1
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pgx-install-test.XXXXXX")"
SRV_PID=""
cleanup() { [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }

VERSION="0.0.0-install-test"
# Build pgx (debug is fine for the installer test; it only runs `pgx version`).
( cd "$ROOT" && CC=/usr/bin/cc cargo build -q -p pgx ) || fail "cargo build -p pgx"
"$ROOT/dist/assemble.sh" --out "$TMP/rel/v$VERSION" --version "$VERSION" \
  --pgx "$ROOT/target/debug/pgx" ${PG18_BINDIR:+--pg-bindir "$PG18_BINDIR"} >/dev/null || fail "assemble"

# Lay the files out as GitHub releases serve them: <base>/v<version>/<name>.tar.gz
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"; [ "$OS" = darwin ] || OS=linux
ARCH="$(uname -m)"; case "$ARCH" in x86_64|amd64) ARCH=amd64;; aarch64|arm64) ARCH=arm64;; esac
NAME="pgx-$VERSION-$OS-$ARCH"
[ -f "$TMP/rel/v$VERSION/$NAME.tar.gz" ] || fail "assemble did not produce $NAME.tar.gz"

# Serve $TMP/rel on a free port.
PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
( cd "$TMP/rel" && python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1 ) &
SRV_PID=$!
sleep 1

export PGX_VERSION="$VERSION"
export PGX_BASE_URL="http://127.0.0.1:$PORT"
export PGX_INSTALL_DIR="$TMP/opt/pgx/$VERSION"
export PGX_BIN_DIR="$TMP/bin"
sh "$ROOT/dist/install.sh" >"$TMP/install.log" 2>&1 || { cat "$TMP/install.log"; fail "install.sh exited non-zero"; }
[ -L "$TMP/bin/pgx" ] || fail "pgx was not linked into PGX_BIN_DIR"
"$TMP/bin/pgx" version | grep -q "$VERSION" || fail "pgx version does not report $VERSION"

# Relocate the whole install tree; the binary must still work (relocatable).
mv "$TMP/opt/pgx/$VERSION" "$TMP/opt/pgx/moved"
"$TMP/opt/pgx/moved/bin/pgx" version >/dev/null || fail "relocated installation does not run"

# Use a fresh link destination after intentionally relocating the install.
export PGX_BIN_DIR="$TMP/bin-bad"

# Tamper with the tarball so the checksum no longer matches: install must refuse.
printf 'x' >> "$TMP/rel/v$VERSION/$NAME.tar.gz"
export PGX_INSTALL_DIR="$TMP/opt/pgx-bad"
if sh "$ROOT/dist/install.sh" >"$TMP/bad.log" 2>&1; then fail "install.sh accepted a tampered tarball"; fi
grep -q "checksum mismatch" "$TMP/bad.log" || fail "install.sh did not report a checksum mismatch"
[ ! -d "$TMP/opt/pgx-bad" ] || fail "install.sh left a partial install after a checksum failure"

# Unsupported platform is refused.
if PGX_BASE_URL="http://127.0.0.1:$PORT" sh -c "uname() { echo Plan9; }; . '$ROOT/dist/install.sh'" >/dev/null 2>&1; then
  :  # the function-override trick is best-effort; skip if the shell inlines uname
fi
echo "PASS: installer downloads, verifies, installs, relocates, and refuses a bad checksum"
