#!/usr/bin/env bash
# ---
# description: Run the vigie2 server (opens a network port, prints the pair link)
# confirm: true
# args:
#   bind:
#     pattern: '[0-9a-fA-F.:\[\]]+'
#     optional: true
#     help: Address to bind, e.g. 0.0.0.0:7879 for LAN access (default 127.0.0.1:7879)
# ---
set -euo pipefail
cd "$CANIF_PROJECT"
cargo run --release --bin vigie2 -- serve ${CANIF_BIND:+--bind "$CANIF_BIND"}
