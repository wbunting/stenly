#!/bin/bash
# macOS integration regression: no audio capture, private library, or app install.
# Usage: scripts/check-window-borrows.sh /absolute/path/to/stenly [cycles=25]
set -euo pipefail
bin=${1:?Pass a built Stenly binary}; cycles=${2:-25}
case "$cycles" in ''|*[!0-9]*) echo 'cycles must be a positive integer' >&2; exit 2;; esac
if (( cycles < 1 )); then exit 2; fi
work=$(mktemp -d "${TMPDIR:-/tmp}/stenly-window-check.XXXXXX")
trap 'rm -rf "$work"' EXIT
cat > "$work/config.toml" <<EOF
[app]
onboarded = true
[output]
audio_dir = '$work/recordings'
notes_dir = '$work/notes'
[calendar]
source = "ics"
[mcp]
http = false
[context]
enabled = false
EOF
export STENLY_CONFIG="$work/config.toml" STENLY_DATA_DIR="$work/data"
# Unset dev flags that could capture, process, or paste user data.
unset STENLY_TEST_RECORD STENLY_TEST_DICTATE STENLY_TEST_HOTKEY STENLY_TEST_PASTE STENLY_NOTE_READY STENLY_OPEN_MEETING STENLY_ONBOARDING
STENLY_STRESS_WINDOWS="$cycles" "$bin" > "$work/log" 2>&1
errors=$(grep -c 'RefCell already borrowed' "$work/log" || true)
completed=$(grep -c 'stress cycle' "$work/log" || true)
printf 'window cycles: %s/%s; RefCell borrow errors: %s\n' "$completed" "$cycles" "$errors"
# Do not print arbitrary app logs: only pass/fail counts from synthetic fixtures.
[[ "$errors" == 0 && "$completed" == "$cycles" ]] && grep -q 'stress done' "$work/log"
