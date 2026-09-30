#!/usr/bin/env bash
# ---
# description: Type-check the web app only, one line per type error then a count
# ---
set -uo pipefail
source "$CANIF_DIR/_lib/web.sh"

typecheck
exit "$web_code"
