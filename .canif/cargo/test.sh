#!/usr/bin/env bash
# ---
# description: Run the vigie2 Rust tests
# args:
#   filter:
#     pattern: '[A-Za-z0-9_:]+'
#     optional: true
#     help: Only tests whose name contains this
# output:
#   tail: 60
# ---
set -euo pipefail
cd "$CANIF_PROJECT"
cargo test ${CANIF_FILTER:+"$CANIF_FILTER"}
