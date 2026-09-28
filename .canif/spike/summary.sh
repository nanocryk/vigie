#!/usr/bin/env bash
# ---
# description: Summarize recorded spike runs of claude -p (reads spike/runs only)
# args:
#   scenario:
#     choices: [basic, permissions, plan, interrupt, background, cd, context, grants, agents]
#     many: true
# ---
set -euo pipefail
python "$CANIF_PROJECT/spike/summarize.py" "$@"
