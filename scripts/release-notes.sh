#!/usr/bin/env bash
# Prints the CHANGELOG.md section for a version (e.g. 0.1.0), for release notes.
set -euo pipefail
awk -v v="$1" '
  /^## \[/ { if (on) exit; on = index($0, "[" v "]") > 0; next }
  on { print }
' "$(dirname "$0")/../CHANGELOG.md" | sed -e '/./,$!d'
