#!/bin/sh
# Runs each application fixture against the assembled PGX distribution: create a
# database with pgx, hand the app its DATABASE_URL, run it, drop the database.
#
#   PGX_DIST=<prefix> tests/apps/run_all.sh [app ...]   (default: all)
#
# Prints one line per app: PASS, FAIL or COVERAGE GAP. Exit 0 only if no app FAILED
# (coverage gaps fail when CI=true or CI=1).
set -eu
[ -n "${PGX_DIST:-}" ] || { echo "PGX_DIST is not set (dist/assemble.sh --out ...)" >&2; exit 2; }
HERE="$(cd "$(dirname "$0")" && pwd)"
PGX="$PGX_DIST/bin/pgx"
APPS="${*:-psql node python go rails}"
SCRATCH="${PGX_TEST_SCRATCH:-/tmp/pgxt}"; mkdir -p "$SCRATCH"
export PGX_HOME="$SCRATCH/apps-$$"
export PGX_RUN_DIR="/tmp/pgxa$$"
# shellcheck disable=SC2329 # Called by the EXIT/INT/TERM trap below.
cleanup() { "$PGX" stop --force >/dev/null 2>&1 || true; [ -n "${PGX_TEST_KEEP:-}" ] || rm -rf "$PGX_HOME" "$PGX_RUN_DIR"; }
trap 'cleanup' EXIT INT TERM
"$PGX" init >/dev/null
"$PGX" start >/dev/null
echo "runtime: $("$PGX" version | head -1)"
fail=0
for app in $APPS; do
  [ -x "$HERE/$app/run.sh" ] || { echo "APP $app: COVERAGE GAP (no fixture)"; case "${CI:-}" in 1|true) fail=1;; esac; continue; }
  "$PGX" drop "${app}_fx" --force >/dev/null 2>&1 || true
  "$PGX" create "${app}_fx" >/dev/null
  URL="$("$PGX" url "${app}_fx")"
  if DATABASE_URL="$URL" PGX_TEST_PSQL="${PGX_TEST_PSQL:-}" sh "$HERE/$app/run.sh"; then rc=0; else rc=$?; fi
  case "$rc" in
    0) ;;
    2) case "${CI:-}" in 1|true) fail=1;; esac ;;
    *) fail=1 ;;
  esac
  "$PGX" drop "${app}_fx" --force >/dev/null 2>&1 || true
done
exit $fail
