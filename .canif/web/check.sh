#!/usr/bin/env bash
# ---
# description: Type-check the web app, then run its vitest suite; type errors or test failures, then totals
# args:
#   filter:
#     pattern: '[A-Za-z0-9_./-]+'
#     optional: true
#     help: Only test files whose path contains this, such as transcript
# ---
set -uo pipefail
source "$CANIF_DIR/_lib/web.sh"

typecheck
[[ $web_code -eq 0 ]] || exit "$web_code"
run_tests ${CANIF_FILTER:+"$CANIF_FILTER"}
exit "$web_code"
