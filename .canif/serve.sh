#!/usr/bin/env bash
# ---
# description: Run the vigie2 server (opens a network port, prints the pair link); runs a copy of the binary, so it can be rebuilt meanwhile and restarted from the app
# risky: true
# args:
#   bind:
#     pattern: '[0-9a-fA-F.:\[\]]+'
#     optional: true
#     help: Address to bind, e.g. 0.0.0.0:7879 for LAN access (default 127.0.0.1:7879)
# ---
set -uo pipefail
cd "$CANIF_PROJECT"
cargo build --release --bin vigie2 || exit $?

# The server exits with this code to ask for a restart.
restart_code=75
exe=""
[[ $OSTYPE == msys* || $OSTYPE == cygwin* ]] && exe=.exe
built=target/release/vigie2$exe
running=target/serve/vigie2$exe
mkdir -p target/serve
while true; do
  cp "$built" "$running" || exit $?
  VIGIE2_SUPERVISED=1 "$running" serve ${CANIF_BIND:+--bind "$CANIF_BIND"}
  code=$?
  if [[ $code -ne $restart_code ]]; then
    echo "vigie2 exited with code $code"
    exit "$code"
  fi
  echo "vigie2 restarting"
done
