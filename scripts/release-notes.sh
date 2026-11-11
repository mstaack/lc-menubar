#!/usr/bin/env bash
# Prints the CHANGELOG.md section for a version (e.g. 0.1.0), for release notes.
# Plain style ("Added:" labels, "•" bullets, hard line breaks): it reads well on
# GitHub and as plain text in the update dialog of older app versions.
set -euo pipefail
awk -v v="$1" '
  /^## \[/ { if (on) exit; on = index($0, "[" v "]") > 0; next }
  on { print }
' "$(dirname "$0")/../CHANGELOG.md" | sed -e '/./,$!d' \
  | sed -E -e 's/^### (.*)$/\1:/' -e 's/^- (.*)$/• \1  /'
