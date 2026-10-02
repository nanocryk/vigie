#!/usr/bin/env bash
# ---
# description: Format the web app with prettier, or list unformatted files with --check; the changed or unformatted files, then a count
# args:
#   check:
#     type: flag
#     help: List files needing formatting without writing
# ---
set -uo pipefail
source "$CANIF_DIR/_lib/web.sh"

if [[ -n "${CANIF_CHECK:-}" ]]; then
  web_capture npx prettier --check --log-level warn .
  files=$(grep -E '^\[warn\] [^ ]+$' "$web_log" | sed -E 's/^\[warn\] //')
  if [[ $web_code -eq 0 ]]; then
    echo "all formatted"
  elif [[ -z "$files" ]]; then
    tail -20 "$web_log"
  else
    echo "$files"
    echo "$(echo "$files" | wc -l) unformatted files"
  fi
else
  web_capture npx prettier --write --list-different .
  if [[ $web_code -eq 0 ]]; then
    cat "$web_log"
    echo "$(grep -c . "$web_log") files changed"
  else
    tail -20 "$web_log"
  fi
fi
exit "$web_code"
