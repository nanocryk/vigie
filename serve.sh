#!/usr/bin/env bash
# Builds and runs the vigie server, running it again when it asks for a restart.
# Arguments go to `vigie serve`. It runs a copy of the binary, so the build can be
# replaced while the server runs.
set -uo pipefail
cd "$(dirname "$0")"
cargo build --release --bin vigie || exit $?

# The server exits with this code to ask for a restart.
restart_code=75
exe=""
[[ $OSTYPE == msys* || $OSTYPE == cygwin* ]] && exe=.exe
built=target/release/vigie$exe
running=target/serve/vigie$exe
mkdir -p target/serve
staging=$running.tmp
while true; do
  # A partial copy never replaces a working one: copy aside, compare, then rename.
  stale=0
  if ! { cp "$built" "$staging" && cmp -s "$built" "$staging" && mv -f "$staging" "$running"; }; then
    rm -f "$staging"
    if [[ ! -f $running ]]; then
      echo "copying $built failed and no previous copy exists"
      exit 1
    fi
    echo "copying $built failed, running the previous copy"
    stale=1
  fi
  VIGIE_SUPERVISED=1 VIGIE_STALE_COPY=$stale "$running" serve "$@"
  code=$?
  if [[ $code -ne $restart_code ]]; then
    echo "vigie exited with code $code"
    exit "$code"
  fi
  echo "vigie restarting"
done
