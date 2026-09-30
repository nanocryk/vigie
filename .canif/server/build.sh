#!/usr/bin/env bash
# ---
# description: Build the release server binary, one line per diagnostic; a running server keeps its copy until restarted
# ---
set -uo pipefail
source "$HOME/.canif/_lib/cargo.sh"
cd "$CANIF_PROJECT"

cargo_capture build --release --bin vigie2 --message-format short
summarize_diagnostics
exit "$cargo_code"
