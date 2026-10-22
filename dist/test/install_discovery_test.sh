#!/bin/sh
# Tests dist/install.sh release discovery and safety against a LOCAL fake of the
# GitHub API + release downloads (no network). Covers: beta/prerelease discovery,
# explicit PGX_VERSION override, missing releases, checksum mismatch, failed
# (404) download. Builds a debug pgx and assembles a real tarball to install.
#
#   PG18_BINDIR=/opt/homebrew/opt/postgresql@18/bin dist/test/install_discovery_test.sh
set -eu
export PGX_INSECURE_ALLOW_HTTP=1
export PGX_INSECURE_SKIP_VERIFY=1
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pgx-disc.XXXXXX")"
SRV_PID=""
cleanup() { [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

VER="0.1.0-beta.1"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"; [ "$OS" = darwin ] || OS=linux
ARCH="$(uname -m)"; case "$ARCH" in x86_64|amd64) ARCH=amd64;; aarch64|arm64) ARCH=arm64;; esac
NAME="pgx-$VER-$OS-$ARCH"

( cd "$ROOT" && CC=/usr/bin/cc cargo build -q -p pgx ) || fail "cargo build -p pgx"
"$ROOT/dist/assemble.sh" --out "$TMP/stage" --version "$VER" \
  --pgx "$ROOT/target/debug/pgx" ${PG18_BINDIR:+--pg-bindir "$PG18_BINDIR"} >/dev/null || fail "assemble"

# Release layout as GitHub serves downloads: <base>/v<version>/<name>.tar.gz
mkdir -p "$TMP/srv/download/v$VER"
cp "$TMP/stage/$NAME.tar.gz" "$TMP/stage/$NAME.tar.gz.sha256" "$TMP/srv/download/v$VER/"

# Fake GitHub API /releases responses (pretty JSON, newest first).
beta_json() { printf '[\n  {\n    "tag_name": "v%s",\n    "prerelease": true\n  },\n  {\n    "tag_name": "v0.0.9",\n    "prerelease": false\n  }\n]\n' "$VER"; }
other_json() { printf '[\n  {\n    "tag_name": "v9.9.9",\n    "prerelease": false\n  }\n]\n'; }
empty_json() { printf '[]\n'; }

PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
( cd "$TMP/srv" && python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1 ) &
SRV_PID=$!
sleep 1
SRV="http://127.0.0.1:$PORT"

run_install() { # extra env... ; sets global RC and captures LOG
  LOG="$TMP/log"; rm -rf "${TMP:?}/opt" "${TMP:?}/bin"
  # `if` so a nonzero install exit is captured, not aborted by set -e.
  if env PGX_API_URL="$SRV" PGX_BASE_URL="$SRV/download" \
      PGX_INSTALL_DIR="$TMP/opt/pgx" PGX_BIN_DIR="$TMP/bin" "$@" \
      sh "$ROOT/dist/install.sh" >"$LOG" 2>&1; then RC=0; else RC=$?; fi
}

echo "## 1. beta/prerelease discovery (no PGX_VERSION -> resolves $VER)"
beta_json > "$TMP/srv/releases"
run_install
[ "$RC" = 0 ] || { cat "$LOG"; fail "beta discovery exited $RC"; }
"$TMP/bin/pgx" version | grep -q "$VER" || { cat "$LOG"; fail "did not install $VER"; }
ok "resolved and installed $VER from the releases list"

echo "## 2. PGX_VERSION override wins over discovery"
other_json > "$TMP/srv/releases"   # discovery would say 9.9.9 (no such artifact)
run_install PGX_VERSION="$VER"
[ "$RC" = 0 ] || { cat "$LOG"; fail "override exited $RC"; }
"$TMP/bin/pgx" version | grep -q "$VER" || fail "override did not install $VER"
ok "explicit PGX_VERSION=$VER installed despite newest=9.9.9"

echo "## 3. missing releases (empty list, no PGX_VERSION -> loud failure)"
empty_json > "$TMP/srv/releases"
run_install
[ "$RC" != 0 ] || fail "empty releases should fail"
grep -q "could not find a published release" "$LOG" || { cat "$LOG"; fail "wrong error for empty releases"; }
[ ! -e "$TMP/bin/pgx" ] || fail "symlink created despite no release"
ok "empty releases -> exit $RC, clear error, no install"

echo "## 4. checksum mismatch (tampered tarball -> refuse)"
beta_json > "$TMP/srv/releases"
printf 'tampered' >> "$TMP/srv/download/v$VER/$NAME.tar.gz"
run_install
[ "$RC" != 0 ] || fail "tampered tarball should fail"
grep -q "checksum mismatch" "$LOG" || { cat "$LOG"; fail "no checksum-mismatch error"; }
[ ! -e "$TMP/opt/pgx" ] || fail "install left behind after checksum failure"
ok "tampered tarball -> exit $RC, refused, no install"
# restore good tarball
cp "$TMP/stage/$NAME.tar.gz" "$TMP/srv/download/v$VER/"

echo "## 5. failed download (artifact 404 -> loud failure, no partial install)"
beta_json > "$TMP/srv/releases"
rm -f "$TMP/srv/download/v$VER/$NAME.tar.gz"
run_install PGX_VERSION="$VER"
[ "$RC" != 0 ] || fail "missing artifact should fail"
grep -q "download failed" "$LOG" || { cat "$LOG"; fail "no download-failed error"; }
[ ! -e "$TMP/opt/pgx" ] || fail "partial install after failed download"
ok "artifact 404 -> exit $RC, download failed, no partial install"

echo "PASS: install.sh discovery, override, missing-release, checksum, and failed-download behavior"
