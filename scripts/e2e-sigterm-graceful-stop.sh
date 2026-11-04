#!/usr/bin/env bash
# e2e-sigterm-graceful-stop.sh — a service stop (SIGTERM) must be a CLEAN stop.
#
# Plan: qontinui-dev-notes/plans/2026-10-26-a-sigtermd-runner-exits-without-a-clean-shutdown-so-a-service-stop-reads-as-a-crash.md
# (Phase 2). Linux only: it drives a transient `systemd-run --user` unit that
# mirrors the primary's (`KillMode=control-group`, `TimeoutStopSec=30`), stops it
# the way `systemctl --user stop` does, and reads — never asserts — the evidence.
#
# It never touches the primary runner. The subject is a runner binary YOU built,
# launched as an isolated SUBJECT runner (`QONTINUI_INSTANCE_ROOT` under a fresh
# temp dir, its own port), so it shares no config, store, marker or log with any
# other runner on the box.
#
# Usage:
#   scripts/e2e-sigterm-graceful-stop.sh <runner-binary> [--port N] [--display :D]
#                                        [--sessions N] [--keep]
#
# Defaults: --port 9893, --display from $DISPLAY (else :99), --sessions 2.
# Each session runs `claude` in a runner terminal; the first is sent `/effort max`.
# --keep leaves the temp instance root behind for inspection (the unit is always
# removed).
#
# Exit: 0 every check passed; 1 a check failed (each failure is printed as
# `FAIL: …`); 2 usage / environment; 3 the subject never became healthy.
#
# Checks (plan Phase 2, "Evidence owed"):
#   A. `systemctl --user stop` completes in < 27 s and the unit reads inactive,
#      not failed.
#   B. the subject's log carries `signal SIGTERM` and `Shutdown drain:`.
#   C. last-shutdown.json reads clean:true.
#   D. on restart: `previous shutdown was clean`, no new crash breadcrumb, and
#      /health is not `errored`.
#   E. every session is back as one `claude --resume <id>` process.
#   F. every lifecycle row is restorable (`pty-exit` or `open`).
#   G. two SIGTERMs 100 ms apart end the process within 1 s, by SIGTERM.

set -u

BIN="${1:-}"
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "usage: $0 <runner-binary> [--port N] [--display :D] [--sessions N] [--keep]" >&2; exit 2; }
shift
PORT=9893
DISP="${DISPLAY:-:99}"
NSESS=2
KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --display) DISP="$2"; shift 2 ;;
    --sessions) NSESS="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
command -v systemd-run >/dev/null || { echo "systemd-run not found (Linux + systemd user manager required)" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq not found" >&2; exit 2; }
if curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/health"; then
  echo "port $PORT already answers — pick another with --port" >&2; exit 2
fi

UNIT=qontinui-sigterm-e2e-$PORT
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/qontinui-sigterm-e2e.XXXXXX")
BASE="http://127.0.0.1:$PORT"
FAILS=0
fail() { echo "FAIL: $*"; FAILS=$((FAILS + 1)); }
pass() { echo "PASS: $*"; }

cleanup() {
  systemctl --user stop "$UNIT" >/dev/null 2>&1
  systemctl --user reset-failed "$UNIT" >/dev/null 2>&1
  if systemctl --user is-active --quiet "$UNIT"; then
    echo "WARN: unit $UNIT still active after cleanup"
  fi
  if [ "$KEEP" = 1 ]; then echo "kept instance root: $ROOT"; else rm -rf "$ROOT"; fi
}
trap cleanup EXIT

start_unit() {
  systemd-run --user --unit="$UNIT" \
    -p KillMode=control-group -p TimeoutStopSec=30 \
    --working-directory="$ROOT" \
    --setenv=DISPLAY="$DISP" \
    --setenv=QONTINUI_INSTANCE_ROOT="$ROOT" \
    --setenv=QONTINUI_INSTANCE_NAME="sigterm-e2e-$PORT" \
    --setenv=QONTINUI_PORT="$PORT" \
    --setenv=PATH="$PATH" \
    "$BIN" >/dev/null || { echo "systemd-run failed" >&2; exit 2; }
}

wait_healthy() {
  local i
  for i in $(seq 1 120); do
    if curl -s -m 3 "$BASE/health" >/dev/null; then return 0; fi
    sleep 1
  done
  return 1
}

main_pid() { systemctl --user show -p MainPID --value "$UNIT"; }

# Every file under the instance root the subject logs into.
logs() { find "$ROOT" -type f \( -name '*.log' -o -name '*.log.*' \) -print0 2>/dev/null | xargs -0 cat 2>/dev/null; }

resume_procs() {
  # One line per live `claude --resume <id>` in the unit's cgroup.
  local pid
  for pid in $(systemctl --user show -p ControlGroup --value "$UNIT" | xargs -I{} cat "/sys/fs/cgroup{}/cgroup.procs" 2>/dev/null); do
    tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -o -- '--resume [0-9a-f-]\{36\}' || true
  done | sort -u
}

echo "== subject: $BIN on :$PORT (display $DISP), root $ROOT, unit $UNIT"
start_unit
wait_healthy || { echo "subject never answered /health"; logs | tail -40; exit 3; }
echo "subject healthy, pid $(main_pid), gitSha $(curl -s "$BASE/health" | jq -r '.gitSha // .data.gitSha // "?"')"

# Phase 0 probe, folded in: nothing in the live process catches SIGALRM.
SIGCGT=$(awk '/^SigCgt:/{print $2}' "/proc/$(main_pid)/status")
if (( (16#$SIGCGT >> 13) & 1 )); then fail "SIGALRM (14) is caught (SigCgt=$SIGCGT) — the alarm backstop is unsafe"; else pass "SIGALRM not caught (SigCgt=$SIGCGT)"; fi

# Open the sessions.
TIDS=()
for n in $(seq 1 "$NSESS"); do
  tid=$(curl -s -X POST "$BASE/terminals" -H 'Content-Type: application/json' \
    -d "{\"title\":\"sigterm-e2e-$n\",\"initial_command\":\"claude\",\"working_dir\":\"$ROOT\"}" | jq -r '.data.id // .id // .data.terminal_id // empty')
  [ -n "$tid" ] || { fail "could not open session $n"; continue; }
  TIDS+=("$tid")
done
sleep 20
if [ "${#TIDS[@]}" -gt 0 ]; then
  curl -s -X POST "$BASE/terminals/${TIDS[0]}/submit-prompt" -H 'Content-Type: application/json' -d '{"message":"/effort max"}' >/dev/null
fi
sleep 5
echo "opened ${#TIDS[@]} session(s): ${TIDS[*]:-}"

# ── A. the stop, timed ────────────────────────────────────────────────────────
T0=$(date +%s.%N)
systemctl --user stop "$UNIT"
T1=$(date +%s.%N)
TOOK=$(echo "$T1 - $T0" | bc)
RESULT=$(systemctl --user show -p Result --value "$UNIT")
STATE=$(systemctl --user show -p ActiveState --value "$UNIT")
echo "stop took ${TOOK}s; ActiveState=$STATE Result=$RESULT"
if (( $(echo "$TOOK < 27" | bc) )); then pass "stop under 27 s"; else fail "stop took ${TOOK}s"; fi
if [ "$STATE" = inactive ] && [ "$RESULT" = success ]; then pass "unit inactive, Result=success"; else fail "unit $STATE / $RESULT"; fi

# ── B. the log ────────────────────────────────────────────────────────────────
LOG1=$(logs)
echo "$LOG1" | grep -q 'signal SIGTERM' && pass "log carries 'signal SIGTERM'" || fail "no 'signal SIGTERM' in the log"
echo "$LOG1" | grep -q 'Shutdown drain:' && pass "log carries 'Shutdown drain:'" || fail "no 'Shutdown drain:' in the log"

# ── C. the marker ─────────────────────────────────────────────────────────────
MARKER=$(find "$ROOT" -name last-shutdown.json | head -1)
if [ -n "$MARKER" ] && jq -e '.clean == true' "$MARKER" >/dev/null 2>&1; then
  pass "last-shutdown.json clean:true ($MARKER)"
else
  fail "last-shutdown.json not clean:true: ${MARKER:-absent} $(cat "$MARKER" 2>/dev/null)"
fi

# ── F. lifecycle rows (read from the store the stop left behind) ─────────────
STORE=$(find "$ROOT" -name terminal-sessions.json | head -1)
if [ -n "$STORE" ]; then
  echo "lifecycle rows:"; jq -r '.. | objects | select(has("state")) | "  \(.id // .terminal_id // "?") state=\(.state) close_reason=\(.close_reason // "-")"' "$STORE" | sort -u
  BAD=$(jq -r '.. | objects | select(has("state")) | select(.state != "open" and (.close_reason // "") != "pty-exit") | "\(.state)/\(.close_reason // "-")"' "$STORE")
  [ -z "$BAD" ] && pass "every lifecycle row is open or pty-exit" || fail "non-restorable rows: $BAD"
else
  fail "no terminal-sessions.json under $ROOT"
fi

# ── D/E. the restart ──────────────────────────────────────────────────────────
CRASH_BEFORE=$(find "$ROOT" -name 'crash_*.txt' | wc -l)
systemctl --user reset-failed "$UNIT" >/dev/null 2>&1
start_unit
wait_healthy || { fail "subject did not come back"; exit 1; }
sleep 30
LOG2=$(logs)
echo "$LOG2" | grep -q 'previous shutdown was clean' && pass "boot classified the stop as clean" || fail "boot did not log 'previous shutdown was clean'"
echo "$LOG2" | grep -q 'previous shutdown was NOT clean' && fail "a boot logged 'previous shutdown was NOT clean'"
CRASH_AFTER=$(find "$ROOT" -name 'crash_*.txt' | wc -l)
[ "$CRASH_AFTER" = "$CRASH_BEFORE" ] && pass "no new crash breadcrumb" || fail "crash breadcrumbs $CRASH_BEFORE -> $CRASH_AFTER"
HEALTH=$(curl -s "$BASE/health")
echo "$HEALTH" | grep -q '"errored"' && fail "/health reads errored" || pass "/health not errored"
RESUMED=$(resume_procs | wc -l)
echo "resumed: $(resume_procs | tr '\n' ' ')"
[ "$RESUMED" -ge "${#TIDS[@]}" ] && [ "${#TIDS[@]}" -gt 0 ] && pass "$RESUMED claude --resume process(es) for ${#TIDS[@]} session(s)" || fail "$RESUMED claude --resume process(es) for ${#TIDS[@]} session(s)"

# ── G. the second signal ──────────────────────────────────────────────────────
PID=$(main_pid)
T0=$(date +%s.%N)
kill -TERM "$PID"; sleep 0.1; kill -TERM "$PID" 2>/dev/null
while kill -0 "$PID" 2>/dev/null; do
  (( $(echo "$(date +%s.%N) - $T0 > 5" | bc) )) && break
  sleep 0.02
done
GONE=$(echo "$(date +%s.%N) - $T0" | bc)
sleep 1
CODE=$(systemctl --user show -p ExecMainCode --value "$UNIT")   # 2 = killed
STATUS=$(systemctl --user show -p ExecMainStatus --value "$UNIT") # signal number
echo "second-signal: gone after ${GONE}s, ExecMainCode=$CODE ExecMainStatus=$STATUS"
if (( $(echo "$GONE < 1" | bc) )) && [ "$CODE" = 2 ] && [ "$STATUS" = 15 ]; then
  pass "a second SIGTERM ends the process at once, by SIGTERM"
else
  fail "second SIGTERM: ${GONE}s, code=$CODE status=$STATUS"
fi

echo "== $FAILS failure(s)"
[ "$FAILS" = 0 ]
