#!/usr/bin/env bash
#
# test-wrappers-multi-runner.sh
#
# Phase 5.1 integration test for the wrapper primary-only ownership migration.
# Verifies that secondary (non-primary) runners correctly proxy /wrappers/*
# routes through to the primary runner via wrappers/primary_proxy.rs.
#
# Topology:
#   - Primary runner   : http://localhost:9876   (must be already running)
#   - Supervisor       : http://localhost:9875
#   - Temp runner      : spawned via supervisor on a port in [9877, 9899]
#
# What this script tests:
#   1. Read parity: GET /wrappers on the temp runner returns the same payload
#      as the primary's GET /wrappers (because the temp proxies upstream).
#   2. Failure-mode pass-through: GET /wrappers/<nonexistent> on the temp
#      runner returns 404 (proxied from primary), not 503.
#   3. (Optional, RUN_INSTALL_TEST=1) Install via the temp runner, confirm
#      the wrapper appears in BOTH primary's and temp's /wrappers.
#
# Cleanup: stops the temp runner via POST /runners/{id}/stop on the supervisor.
#
# This script is a smoke test — run it after a successful runner build.
# Do not run it while the upstream build is broken; the temp runner cannot
# spawn without a passing build.

set -euo pipefail

# --------------------------------------------------------------------------
# Constants
# --------------------------------------------------------------------------
PRIMARY_PORT="${PRIMARY_PORT:-9876}"
SUPERVISOR_PORT="${SUPERVISOR_PORT:-9875}"
PRIMARY_URL="http://localhost:${PRIMARY_PORT}"
SUPERVISOR_URL="http://localhost:${SUPERVISOR_PORT}"
HEALTH_TIMEOUT_SECS="${HEALTH_TIMEOUT_SECS:-60}"
RUN_INSTALL_TEST="${RUN_INSTALL_TEST:-0}"
INSTALL_PACKAGE="${INSTALL_PACKAGE:-wrapper-v0}"

# --------------------------------------------------------------------------
# Result tracking
# --------------------------------------------------------------------------
PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
TEMP_RUNNER_ID=""

pass() { echo "  PASS: $1"; PASS_COUNT=$((PASS_COUNT + 1)); }
fail() { echo "  FAIL: $1"; FAIL_COUNT=$((FAIL_COUNT + 1)); }
skip() { echo "  SKIP: $1"; SKIP_COUNT=$((SKIP_COUNT + 1)); }
info() { echo "  ... $1"; }

# --------------------------------------------------------------------------
# Cleanup (always runs, even on failure)
# --------------------------------------------------------------------------
cleanup() {
    local exit_code=$?
    if [ -n "${TEMP_RUNNER_ID}" ]; then
        echo ""
        echo "Cleanup: stopping temp runner '${TEMP_RUNNER_ID}'..."
        curl --silent --show-error --max-time 10 \
            -X POST "${SUPERVISOR_URL}/runners/${TEMP_RUNNER_ID}/stop" \
            -H 'Content-Type: application/json' \
            -d '{}' \
            >/dev/null 2>&1 \
            && echo "  stopped" \
            || echo "  WARN: stop request failed (runner may already be gone)"
    fi
    exit $exit_code
}
trap cleanup EXIT

# --------------------------------------------------------------------------
# Step 1: probe primary
# --------------------------------------------------------------------------
echo "=== Step 1: probe primary runner at ${PRIMARY_URL} ==="
if ! curl --silent --fail --show-error --max-time 5 \
        "${PRIMARY_URL}/health" >/dev/null; then
    echo "FATAL: primary runner not reachable at ${PRIMARY_URL}/health."
    echo "       Start it manually before running this script."
    exit 2
fi
info "primary /health OK"

# --------------------------------------------------------------------------
# Step 2: spawn temp runner via supervisor
# --------------------------------------------------------------------------
echo ""
echo "=== Step 2: spawn temp runner via supervisor at ${SUPERVISOR_URL} ==="
SPAWN_BODY='{"rebuild": false, "wait": true, "wait_timeout_secs": '"${HEALTH_TIMEOUT_SECS}"'}'
SPAWN_RESPONSE=$(curl --silent --show-error --max-time $((HEALTH_TIMEOUT_SECS + 30)) \
    -X POST "${SUPERVISOR_URL}/runners/spawn-test" \
    -H 'Content-Type: application/json' \
    -d "${SPAWN_BODY}")

if [ -z "${SPAWN_RESPONSE}" ]; then
    echo "FATAL: empty response from /runners/spawn-test"
    exit 3
fi

TEMP_RUNNER_ID=$(python3 -c "import sys,json;print(json.loads(sys.stdin.read()).get('id',''))" <<< "${SPAWN_RESPONSE}")
TEMP_RUNNER_PORT=$(python3 -c "import sys,json;print(json.loads(sys.stdin.read()).get('port',''))" <<< "${SPAWN_RESPONSE}")

if [ -z "${TEMP_RUNNER_ID}" ] || [ -z "${TEMP_RUNNER_PORT}" ]; then
    echo "FATAL: could not parse id/port from spawn response: ${SPAWN_RESPONSE}"
    exit 3
fi
TEMP_URL="http://localhost:${TEMP_RUNNER_PORT}"
info "spawned ${TEMP_RUNNER_ID} on port ${TEMP_RUNNER_PORT}"

# --------------------------------------------------------------------------
# Step 3: poll temp /health (defensive — wait:true should already mean ready)
# --------------------------------------------------------------------------
echo ""
echo "=== Step 3: poll temp /health (up to ${HEALTH_TIMEOUT_SECS}s) ==="
DEADLINE=$(($(date +%s) + HEALTH_TIMEOUT_SECS))
while [ "$(date +%s)" -lt "${DEADLINE}" ]; do
    if curl --silent --fail --max-time 3 "${TEMP_URL}/health" >/dev/null 2>&1; then
        info "temp /health OK"
        break
    fi
    sleep 2
done
if ! curl --silent --fail --max-time 3 "${TEMP_URL}/health" >/dev/null 2>&1; then
    fail "temp runner never became healthy at ${TEMP_URL}/health"
    exit 4
fi

# --------------------------------------------------------------------------
# Step 4: read-side parity
# --------------------------------------------------------------------------
echo ""
echo "=== Step 4: read-side parity (GET /wrappers) ==="
PRIMARY_LIST=$(curl --silent --fail --show-error --max-time 10 "${PRIMARY_URL}/wrappers")
TEMP_LIST=$(curl --silent --fail --show-error --max-time 10 "${TEMP_URL}/wrappers")

if python3 - "${PRIMARY_LIST}" "${TEMP_LIST}" <<'PY'
import json, sys
a = json.loads(sys.argv[1])
b = json.loads(sys.argv[2])
sys.exit(0 if a == b else 1)
PY
then
    pass "GET /wrappers on temp == GET /wrappers on primary"
else
    fail "GET /wrappers diverges between temp and primary"
    echo "    primary: ${PRIMARY_LIST}"
    echo "    temp:    ${TEMP_LIST}"
fi

# --------------------------------------------------------------------------
# Step 5: failure-mode pass-through (404, not 503)
# --------------------------------------------------------------------------
echo ""
echo "=== Step 5: failure-mode pass-through (404 not 503) ==="
NONEXISTENT_STATUS=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --max-time 10 "${TEMP_URL}/wrappers/foo-nonexistent-xyz")
case "${NONEXISTENT_STATUS}" in
    404) pass "GET /wrappers/foo-nonexistent-xyz returned 404 (proxied)";;
    503) fail "got 503 — proxy returned ServiceUnavailable instead of forwarding 404";;
    *)   fail "unexpected status ${NONEXISTENT_STATUS} for nonexistent wrapper";;
esac

# --------------------------------------------------------------------------
# Step 6: optional install round-trip
# --------------------------------------------------------------------------
echo ""
echo "=== Step 6: install round-trip (RUN_INSTALL_TEST=${RUN_INSTALL_TEST}) ==="
if [ "${RUN_INSTALL_TEST}" != "1" ]; then
    skip "install test gated by RUN_INSTALL_TEST=1"
else
    INSTALL_BODY='{"package":"'"${INSTALL_PACKAGE}"'"}'
    INSTALL_RESPONSE=$(curl --silent --show-error --max-time 120 \
        -X POST "${TEMP_URL}/wrappers/install" \
        -H 'Content-Type: application/json' \
        -d "${INSTALL_BODY}")
    INSTALLED_ID=$(python3 -c "import sys,json;d=json.loads(sys.stdin.read());print((d.get('data') or d).get('id',''))" <<< "${INSTALL_RESPONSE}")
    if [ -z "${INSTALLED_ID}" ]; then
        fail "install via temp returned no id: ${INSTALL_RESPONSE}"
    else
        info "installed wrapper id='${INSTALLED_ID}' via temp"
        PRIMARY_LIST_AFTER=$(curl --silent --fail --max-time 10 "${PRIMARY_URL}/wrappers")
        TEMP_LIST_AFTER=$(curl --silent --fail --max-time 10 "${TEMP_URL}/wrappers")
        FOUND_PRIMARY=$(python3 -c "import sys,json;d=json.loads(sys.stdin.read());items=(d.get('data') or d) if isinstance(d,dict) else d;print('Y' if any((w.get('id')=='${INSTALLED_ID}') for w in (items if isinstance(items,list) else [])) else 'N')" <<< "${PRIMARY_LIST_AFTER}")
        FOUND_TEMP=$(python3 -c "import sys,json;d=json.loads(sys.stdin.read());items=(d.get('data') or d) if isinstance(d,dict) else d;print('Y' if any((w.get('id')=='${INSTALLED_ID}') for w in (items if isinstance(items,list) else [])) else 'N')" <<< "${TEMP_LIST_AFTER}")
        if [ "${FOUND_PRIMARY}" = "Y" ] && [ "${FOUND_TEMP}" = "Y" ]; then
            pass "installed wrapper visible from BOTH primary and temp"
        else
            fail "installed wrapper visibility — primary=${FOUND_PRIMARY} temp=${FOUND_TEMP}"
        fi
    fi
fi

# --------------------------------------------------------------------------
# Summary
# --------------------------------------------------------------------------
echo ""
echo "=========================================="
echo "  Wrapper multi-runner integration test"
echo "  PASS:  ${PASS_COUNT}"
echo "  FAIL:  ${FAIL_COUNT}"
echo "  SKIP:  ${SKIP_COUNT}"
echo "=========================================="

if [ "${FAIL_COUNT}" -gt 0 ]; then
    exit 1
fi
exit 0
