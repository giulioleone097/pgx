#!/usr/bin/env bash
# Select an artifact version without interpolating workflow inputs into code.
set -euo pipefail
if [ "$GITHUB_EVENT_NAME" = workflow_dispatch ]; then
  version="${INPUT_VERSION:-}"
else
  version="${GITHUB_REF_NAME#v}"
fi
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || { echo "::error::invalid release version" >&2; exit 1; }
printf 'version=%s\n' "$version"
