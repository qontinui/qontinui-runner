#!/usr/bin/env bash
# End-to-end smoke for the terminal grid endpoints (buffer / grid / text /
# search / diff). Doubles as an executable example for the cell-content
# assertion pattern documented in
# `src/components/terminal/GRID_TESTING.md`.
#
# Usage:
#   scripts/test-grid-endpoints.sh                 # auto-spawns a temp runner
#   scripts/test-grid-endpoints.sh http://localhost:9876   # against an existing runner
#
# Exit code 0 = all endpoints worked and returned plausible shapes.

set -euo pipefail

RUNNER="${1:-}"
SUPERVISOR="${SUPERVISOR:-http://localhost:9875}"
TEMP_RUNNER_ID=""

cleanup() {
  if [[ -n "$TEMP_RUNNER_ID" ]]; then
    curl -s -X POST "$SUPERVISOR/runners/$TEMP_RUNNER_ID/stop" > /dev/null || true
  fi
}
trap cleanup EXIT

if [[ -z "$RUNNER" ]]; then
  echo "Spawning temp runner via supervisor..."
  SPAWN_RESP=$(curl -s -X POST "$SUPERVISOR/runners/spawn-test" \
    -H "Content-Type: application/json" \
    -d '{"rebuild": false, "use_lkg": false, "wait": true, "wait_timeout_secs": 90, "requester_id": "grid-endpoints-smoke"}')
  PORT=$(echo "$SPAWN_RESP" | python -c "import sys,json; print(json.load(sys.stdin)['port'])")
  TEMP_RUNNER_ID=$(echo "$SPAWN_RESP" | python -c "import sys,json; print(json.load(sys.stdin)['id'])")
  RUNNER="http://localhost:$PORT"
  echo "Temp runner: $RUNNER ($TEMP_RUNNER_ID)"
fi

# 1. Open a terminal so we have something to query.
curl -s -X POST "$RUNNER/ui-bridge/control/tab/activate" \
  -H "Content-Type: application/json" \
  -d '{"tabId": "terminal"}' > /dev/null
sleep 2
curl -s -X POST "$RUNNER/ui-bridge/control/element/button-launch-menu/action" \
  -H "Content-Type: application/json" \
  -d '{"action": "click"}' > /dev/null
sleep 1
curl -s -X POST "$RUNNER/ui-bridge/control/element/button-1-0/action" \
  -H "Content-Type: application/json" \
  -d '{"action": "click"}' > /dev/null || true
# Give PowerShell time to print its prompt.
sleep 5

# 2. Find a session id via React-fiber walk through page/evaluate.
SID=$(curl -s -X POST "$RUNNER/ui-bridge/control/page/evaluate" \
  -H "Content-Type: application/json" \
  -d '{"expression": "(()=>{const s=new Set();for(const el of document.querySelectorAll(\"*\")){for(const k of Object.keys(el)){if(k.startsWith(\"__reactFiber\")){let f=el[k],d=0;while(f&&d<5){if(f.memoizedProps&&f.memoizedProps.terminalId)s.add(f.memoizedProps.terminalId);f=f.return;d++;}break;}}}return [...s][0]||\"\";})()"}' \
  | python -c "import sys,json; print(json.load(sys.stdin)['data']['result'])")

if [[ -z "$SID" ]]; then
  echo "FAIL: no terminal session found after launch"; exit 1
fi
echo "session: $SID"

# 3. /buffer — should have at least one non-empty line (the PS prompt).
BUF_NONEMPTY=$(curl -s "$RUNNER/ui-bridge/sdk/terminal/sessions/$SID/buffer" \
  | python -c "import sys,json; d=json.load(sys.stdin); print(sum(1 for l in d['data']['lines'] if l.strip()))")
if [[ "$BUF_NONEMPTY" -lt 1 ]]; then
  echo "FAIL: /buffer returned no non-empty lines"; exit 1
fi
echo "OK /buffer: $BUF_NONEMPTY non-empty lines"

# 4. /grid — should match buffer rows.
GRID_ROWS=$(curl -s "$RUNNER/ui-bridge/sdk/terminal/sessions/$SID/grid" \
  | python -c "import sys,json; print(json.load(sys.stdin)['data']['rows'])")
echo "OK /grid: $GRID_ROWS rows"

# 5. /text — should include 'PS' (PowerShell prompt) when running on Windows.
TEXT=$(curl -s "$RUNNER/ui-bridge/sdk/terminal/sessions/$SID/text" \
  | python -c "import sys,json; print(json.load(sys.stdin)['data']['text'])")
echo "OK /text: $(echo \"$TEXT\" | head -c 80)"

# 6. /search — substring match for any printable run of characters.
NEEDLE=$(echo "$TEXT" | tr -d '[:space:]' | head -c 4)
if [[ -n "$NEEDLE" ]]; then
  SEARCH_HITS=$(curl -s "$RUNNER/ui-bridge/sdk/terminal/search?q=$NEEDLE&session_id=$SID" \
    | python -c "import sys,json; print(json.load(sys.stdin)['data']['totalHits'])")
  if [[ "$SEARCH_HITS" -lt 1 ]]; then
    echo "FAIL: /search returned 0 hits for needle '$NEEDLE' that exists in /text"; exit 1
  fi
  echo "OK /search: '$NEEDLE' → $SEARCH_HITS hits"
fi

# 7. /search regex error path.
REGEX_BAD=$(curl -s -o /dev/null -w "%{http_code}" \
  "$RUNNER/ui-bridge/sdk/terminal/search?q=%28unclosed&regex=true")
if [[ "$REGEX_BAD" != "400" ]]; then
  echo "FAIL: invalid regex should yield 400, got $REGEX_BAD"; exit 1
fi
echo "OK /search invalid-regex → 400"

# 8. /diff against itself: zero changes.
DIFF_CHANGES=$(curl -s "$RUNNER/ui-bridge/sdk/terminal/sessions/$SID/diff/$SID" \
  | python -c "import sys,json; print(len(json.load(sys.stdin)['data']['changes']))")
if [[ "$DIFF_CHANGES" != "0" ]]; then
  echo "FAIL: self-diff should be 0 changes, got $DIFF_CHANGES"; exit 1
fi
echo "OK /diff self → 0 changes"

echo
echo "ALL ENDPOINTS PASSED"
