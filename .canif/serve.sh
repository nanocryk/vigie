#!/usr/bin/env bash
# ---
# description: Run the vigie server (opens a network port, prints the pair link); runs a copy of the binary, so it can be rebuilt meanwhile and restarted from the app
# risky: true
# args:
#   bind:
#     pattern: '[0-9a-fA-F.:\[\]]+'
#     optional: true
#     help: Address to bind, e.g. 0.0.0.0:7879 for LAN access (default 127.0.0.1:7879)
# ---
set -uo pipefail
exec "$CANIF_PROJECT/serve.sh" ${CANIF_BIND:+--bind "$CANIF_BIND"}
