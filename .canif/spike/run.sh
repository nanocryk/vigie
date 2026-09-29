#!/usr/bin/env bash
# ---
# description: >
#   Run spike scenarios against the live claude CLI in a scratch folder. Spends
#   subscription usage and auto-approves the tool calls Claude makes.
# risky: true
# args:
#   scenario:
#     choices: [basic, permissions, plan, interrupt, background, cd, context, grants, agents, filerules]
#     many: true
# ---
set -euo pipefail
python "$CANIF_PROJECT/spike/drive.py" "$TEMP/vigie2-spike/cwd" "$@"
