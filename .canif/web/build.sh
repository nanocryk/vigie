#!/usr/bin/env bash
# ---
# description: Build the web app into web/dist; the build time, or the error output
# ---
set -uo pipefail
source "$CANIF_DIR/_lib/web.sh"

web_capture npx vite build
if [[ $web_code -eq 0 ]]; then
  grep -E 'built in' "$web_log"
else
  tail -30 "$web_log"
fi
exit "$web_code"
