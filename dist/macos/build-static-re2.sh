#!/bin/sh
# Build a STATIC re2 + abseil prefix for self-contained macOS release builds.
#
# Homebrew ships re2 with a static libre2.a but abseil as dylibs ONLY, so a
# release engine built against Homebrew links ~65 absl dylibs by absolute
# /opt/homebrew path and is not self-contained (issue #10). regexp_alt/build.rs
# already prefers a static re2 + abseil closure when pkg-config points at one;
# this script produces that closure. Point the build at it with:
#
#   PREFIX="$(dist/macos/build-static-re2.sh)"
#   export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:$PKG_CONFIG_PATH"
#   cargo build --release --locked --bin postgres --bin pgx
#
# Sources are pinned to reviewed commit IDs; override with
# ABSL_COMMIT / RE2_COMMIT (full 40-character IDs). The prefix dir is printed on stdout (all logs go to
# stderr) so it can be captured as above.
set -eu
PREFIX="${PGX_RE2_PREFIX:-${RUNNER_TEMP:-/tmp}/pgx-static-re2}"
WORK="${PREFIX}.build"
ABSL_COMMIT="${ABSL_COMMIT:-2065f4ded0558c6f89fee67c8e5228feb4eb960e}" # 20260817.0
RE2_COMMIT="${RE2_COMMIT:-927f5d53caf8111721e734cf24724686bb745f55}" # 2025-11-05
log() { echo "build-static-re2: $*" >&2; }
fetch_source() {
  repo="$1"; commit="$2"; dest="$3"
  case "$commit" in *[!0-9a-f]*|"") log "invalid source commit"; exit 1;; esac
  [ "${#commit}" -eq 40 ] || { log "source commit must be a full SHA"; exit 1; }
  git init -q "$dest"
  git -C "$dest" remote add origin "$repo"
  git -C "$dest" fetch --depth 1 origin "$commit" >&2
  git -C "$dest" checkout --detach "$commit" >&2
  [ "$(git -C "$dest" rev-parse HEAD)" = "$commit" ] || exit 1
}


if [ -f "$PREFIX/lib/libre2.a" ] && [ -f "$PREFIX/lib/pkgconfig/re2.pc" ] && ls "$PREFIX"/lib/libabsl_*.a >/dev/null 2>&1; then
  log "reusing existing static prefix $PREFIX"
  echo "$PREFIX"; exit 0
fi
command -v cmake >/dev/null 2>&1 || { log "cmake is required"; exit 1; }
command -v ninja >/dev/null 2>&1 && GEN="-G Ninja" || GEN=""
: "${CC:=/usr/bin/cc}"; : "${CXX:=/usr/bin/c++}"
export CC CXX
rm -rf "$PREFIX" "$WORK"; mkdir -p "$PREFIX" "$WORK"

log "cloning + building abseil $ABSL_COMMIT (static)"
fetch_source https://github.com/abseil/abseil-cpp "$ABSL_COMMIT" "$WORK/absl"
cmake -S "$WORK/absl" -B "$WORK/absl-b" $GEN \
  -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
  -DABSL_ENABLE_INSTALL=ON -DABSL_PROPAGATE_CXX_STD=ON \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_CXX_STANDARD=17 \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" >&2
cmake --build "$WORK/absl-b" --target install >&2 2>&1

log "cloning + building re2 $RE2_COMMIT (static, against the static abseil)"
fetch_source https://github.com/google/re2 "$RE2_COMMIT" "$WORK/re2"
cmake -S "$WORK/re2" -B "$WORK/re2-b" $GEN \
  -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DRE2_BUILD_TESTING=OFF \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_CXX_STANDARD=17 \
  -DCMAKE_PREFIX_PATH="$PREFIX" -DCMAKE_INSTALL_PREFIX="$PREFIX" >&2
cmake --build "$WORK/re2-b" --target install >&2 2>&1

[ -f "$PREFIX/lib/libre2.a" ] || { log "ERROR: libre2.a not produced"; exit 1; }
ls "$PREFIX"/lib/libabsl_*.a >/dev/null 2>&1 || { log "ERROR: no static abseil archives"; exit 1; }
log "ok: $(ls "$PREFIX"/lib/libabsl_*.a | wc -l | tr -d ' ') abseil archives + libre2.a in $PREFIX"
echo "$PREFIX"
