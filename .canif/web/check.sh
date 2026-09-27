#!/usr/bin/env bash
# ---
# description: Type-check the web app and run its vitest suite
# output:
#   tail: 60
# ---
set -euo pipefail
cd "$CANIF_PROJECT/web"
npx tsc --noEmit
npx vitest run
