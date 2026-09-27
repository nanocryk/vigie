#!/usr/bin/env bash
# ---
# description: Build the web app into web/dist
# output:
#   tail: 40
# ---
set -euo pipefail
cd "$CANIF_PROJECT/web"
npx vite build
