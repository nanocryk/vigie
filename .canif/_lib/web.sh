# Shared helpers for web scripts: run a tool in web/ with captured output, print compact summaries.

# Full output, for following a running command; kept when a step fails so it can be read back.
web_log=$(mktemp)
web_code=0
trap '[[ $web_code -eq 0 ]] && rm -f "$web_log"' EXIT
echo "log: $(cygpath -m "$web_log" 2>/dev/null || echo "$web_log")"
export NO_COLOR=1
cd "$CANIF_PROJECT/web"

# Runs the command, appending its output to the log; sets web_code.
web_capture() {
  "$@" >>"$web_log" 2>&1
  web_code=$?
}

# One line per type error, then a count.
typecheck() {
  web_capture npx tsc --noEmit --pretty false
  local errors
  errors=$(grep -cE '^[^ ].*\([0-9]+,[0-9]+\): error TS' "$web_log")
  grep -E '^[^ ].*\([0-9]+,[0-9]+\): error TS' "$web_log"
  if [[ $web_code -eq 0 ]]; then
    echo "types clean"
  elif [[ $errors -eq 0 ]]; then
    tail -20 "$web_log"
  else
    echo "$errors type errors"
  fi
}

# Failure details, then the file and test totals.
run_tests() {
  web_capture npx vitest run "$@"
  sed -n -E '/(Failed Tests|Failed Suites|Unhandled Error)/,/^ *Test Files /p' "$web_log" \
    | grep -vE '^ *Test Files ' | cat -s | head -150
  if grep -qE '^ *Test Files ' "$web_log"; then
    grep -E '^ *(Test Files|Tests) ' "$web_log" | sed -E 's/^ +//'
  else
    tail -20 "$web_log"
  fi
}
