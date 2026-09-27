#!/usr/bin/env bash
# ---
# description: Check Rust formatting and run clippy with warnings denied (--fix formats first)
# args:
#   fix:
#     type: flag
#     help: Run cargo fmt before the checks
# output:
#   tail: 60
# ---
set -euo pipefail
cd "$CANIF_PROJECT"
if [[ -n "${CANIF_FIX:-}" ]]; then
  cargo fmt
fi
cargo fmt --check
cargo clippy --all-targets -- -D warnings
