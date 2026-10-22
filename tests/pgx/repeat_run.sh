#!/bin/sh
# Repeated real-engine startup gate; each iteration keeps normal parallel tests
# and all authentication/readiness assertions. The first failure stops the gate.
set -eu
repetitions=${PGX_RUN_REPETITIONS:-20}
case "$repetitions" in
    ''|*[!0-9]*|0) echo "PGX_RUN_REPETITIONS must be a positive integer" >&2; exit 2 ;;
esac
iteration=1
while [ "$iteration" -le "$repetitions" ]; do
    echo "pgx run end-to-end repetition $iteration/$repetitions ($(uname -m))"
    cargo test -p pgx --release --test run -- --test-threads=8
    iteration=$((iteration + 1))
done
