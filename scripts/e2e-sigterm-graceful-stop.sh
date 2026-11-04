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
# other runner on the box. Every session it opens runs with cwd = that root, which
# is how the script finds (and, at the end, removes) the `claude` processes it
# started: a PTY-holder pane moves itself out of the unit's cgroup, so the cgroup
# is not a complete census.
#
# Usage:
#   scripts/e2e-sigterm-graceful-stop.sh <runner-binary> [--port N] [--display :D]
#                                        [--sessions N] [--keep]
#
# Defaults: --port 9893, --display from $DISPLAY (else :99), --sessions 2.
# Each session runs `claude` in a runner terminal; the first is sent `/effort max`.
# --keep leaves the temp instance root behind for inspection (the unit and every
# process the run started are always removed).
#
# Exit: 0 every check passed; 1 a check failed (each failure is printed as
# `FAIL: …`); 2 usage / environment; 3 the subject never became healthy.
#
# Checks (plan Phase 2, "Evidence owed"):
#   0. (Phase 0 probe) the live process does not catch SIGALRM.
#   A. `systemctl --user stop` completes in < 27 s and the journal records a
#      successful deactivation, not a failure.
#   B. the subject's log carries `signal SIGTERM` and `Shutdown drain:`.
#   C. last-shutdown.json reads clean:true.
#   F. every lifecycle row (snapshot + WAL replayed) is restorable:
#      `open`, or closed `pty-exit`.
#   D. on restart (log written AFTER the restart only): `previous shutdown was
#      clean`, never `NOT clean`, no new crash breadcrumb, /health not `errored`.
#   E. every session is back as one `claude --resume <id>` process.
#   G. two SIGTERMs 100 ms apart end the process within 1 s, by SIGTERM (read
#      from the journal: `code=killed, status=15/TERM`).

set -u

BIN="${1:-}"
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "usage: $0 <runner-binary> [--port N] [--display :D] [--sessions N] [--keep]" >&2; exit 2; }
BIN=$(readlink -f "$BIN")
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
for tool in systemd-run systemctl journalctl jq python3 curl; do
  command -v "$tool" >/dev/null || { echo "$tool not found (Linux + systemd user manager required)" >&2; exit 2; }
done
if curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/health"; then
  echo "port $PORT already answers — pick another with --port" >&2; exit 2
fi

UNIT=qontinui-sigterm-e2e-$PORT
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/qontinui-sigterm-e2e.XXXXXX")
BASE="http://127.0.0.1:$PORT"
FAILS=0
fail() { echo "FAIL: $*"; FAILS=$((FAILS + 1)); }
pass() { echo "PASS: $*"; }
now_ms() { date +%s%3N; }

# Every process this run started that outlives the unit: anything whose cwd is
# under the instance root (the sessions' working dir), or whose environment
# names it (the runner's own descendants).
own_pids() {
  local d pid
  for d in /proc/[0-9]*; do
    pid=${d#/proc/}
    [ "$pid" = "$$" ] && continue
    case "$(readlink "$d/cwd" 2>/dev/null)" in
      "$ROOT"|"$ROOT"/*) echo "$pid"; continue ;;
    esac
    tr '\0' '\n' < "$d/environ" 2>/dev/null | grep -qxF "QONTINUI_INSTANCE_ROOT=$ROOT" && echo "$pid"
  done
}

cleanup() {
  systemctl --user stop "$UNIT" >/dev/null 2>&1
  systemctl --user reset-failed "$UNIT" >/dev/null 2>&1
  local left
  left=$(own_pids | tr '\n' ' ')
  if [ -n "$left" ]; then
    kill -TERM $left 2>/dev/null; sleep 2
    left=$(own_pids | tr '\n' ' ')
    [ -n "$left" ] && kill -KILL $left 2>/dev/null
    sleep 1
  fi
  left=$(own_pids | tr '\n' ' ')
  [ -n "$left" ] && echo "WARN: processes still alive after cleanup: $left"
  systemctl --user is-active --quiet "$UNIT" && echo "WARN: unit $UNIT still active after cleanup"
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
  for i in $(seq 1 180); do
    curl -s -m 3 -o /dev/null "$BASE/health" && return 0
    sleep 1
  done
  return 1
}

main_pid() { systemctl --user show -p MainPID --value "$UNIT"; }
journal_since() { journalctl --user -u "$UNIT" --since "@$1" --no-pager -o cat 2>/dev/null; }

# Log text, per file, from a recorded byte offset (a manifest of `size path`
# lines), so a check can read only what was written after a point in time.
log_files() { find "$ROOT" -type f \( -name '*.log' -o -name '*.log.*' \) 2>/dev/null; }
log_manifest() { log_files | while read -r f; do echo "$(stat -c %s "$f") $f"; done; }
logs_since() {   # $1 = manifest file ('' = from the start)
  local f off
  log_files | while read -r f; do
    off=0
    [ -n "$1" ] && off=$(awk -v p="$f" '$2==p{print $1}' "$1")
    tail -c +"$(( ${off:-0} + 1 ))" "$f" 2>/dev/null
  done
}

# Lifecycle rows as the store would load them: snapshot, then the WAL replayed.
lifecycle_rows() {
  python3 - "$ROOT" <<'PY'
import json, os, sys
root = sys.argv[1]
snaps = [os.path.join(d, f) for d, _, fs in os.walk(root) for f in fs if f == "terminal-sessions.json"]
if not snaps:
    print("NO_STORE"); sys.exit(0)
snap = snaps[0]
rows = {}
try:
    data = json.load(open(snap))
except Exception:
    data = {}
def collect(o):
    if isinstance(o, dict):
        if "state" in o and ("claudeSessionId" in o or "terminalId" in o):
            rows[o.get("claudeSessionId") or o.get("terminalId")] = o
        else:
            for v in o.values(): collect(v)
    elif isinstance(o, list):
        for v in o: collect(v)
collect(data)
wal = snap[: -len(".json")] + ".wal.jsonl"
if os.path.exists(wal):
    for line in open(wal):
        try: d = json.loads(line)
        except Exception: continue
        if d.get("op") == "upsert" and isinstance(d.get("rec"), dict):
            r = d["rec"]; rows[r.get("claudeSessionId") or r.get("terminalId")] = r
        elif d.get("op") == "remove":
            rows.pop(d.get("id"), None)
for k, r in rows.items():
    print(f"{k} {r.get('state')} {r.get('closeReason') or '-'}")
PY
}

resume_ids() {
  local pid
  for pid in $(own_pids); do
    tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -o -- '--resume [0-9a-f-]\{36\}' || true
  done | sort -u
}

echo "== subject: $BIN on :$PORT (display $DISP), root $ROOT, unit $UNIT"
start_unit
wait_healthy || { echo "subject never answered /health"; logs_since '' | tail -40; exit 3; }
echo "subject healthy, pid $(main_pid), $(curl -s "$BASE/health" | jq -c '{gitSha: (.gitSha // .data.gitSha), buildId: (.buildId // .data.buildId)}')"

# ── 0. Phase 0 probe: nothing in the live process catches SIGALRM ────────────
SIGCGT=$(awk '/^SigCgt:/{print $2}' "/proc/$(main_pid)/status")
if (( (16#$SIGCGT >> 13) & 1 )); then fail "SIGALRM (14) is caught (SigCgt=$SIGCGT) — the alarm backstop is unsafe"; else pass "SIGALRM not caught (SigCgt=$SIGCGT)"; fi

# Open the sessions.
TIDS=()
for n in $(seq 1 "$NSESS"); do
  tid=$(curl -s -X POST "$BASE/terminals" -H 'Content-Type: application/json' \
    -d "{\"title\":\"sigterm-e2e-$n\",\"initial_command\":\"claude\",\"working_dir\":\"$ROOT\"}" | jq -r '.data.id // empty')
  [ -n "$tid" ] || { fail "could not open session $n"; continue; }
  TIDS+=("$tid")
done
sleep 20
[ "${#TIDS[@]}" -gt 0 ] && curl -s -o /dev/null -X POST "$BASE/terminals/${TIDS[0]}/submit-prompt" \
  -H 'Content-Type: application/json' -d '{"message":"/effort max"}'
sleep 5
echo "opened ${#TIDS[@]} session(s): ${TIDS[*]:-}"
echo "lifecycle rows before the stop:"; lifecycle_rows | sed 's/^/  /'

# ── A. the stop, timed ────────────────────────────────────────────────────────
SINCE=$(date +%s)
T0=$(now_ms)
systemctl --user stop "$UNIT"
TOOK_MS=$(( $(now_ms) - T0 ))
sleep 1
J=$(journal_since "$SINCE")
echo "stop took ${TOOK_MS} ms; journal: $(echo "$J" | grep -E 'Deactivated|Failed|Main process exited|Stopped' | tr '\n' '|')"
[ "$TOOK_MS" -lt 27000 ] && pass "stop under 27 s (${TOOK_MS} ms)" || fail "stop took ${TOOK_MS} ms"
if echo "$J" | grep -q 'Failed with result'; then fail "unit failed on stop"
elif echo "$J" | grep -qE 'Deactivated successfully|Stopped '; then pass "unit stopped cleanly (not failed)"
else fail "no deactivation line in the journal"; fi

# ── B. the log ────────────────────────────────────────────────────────────────
LOG1=$(logs_since '')
echo "$LOG1" | grep -q 'signal SIGTERM' && pass "log carries 'signal SIGTERM'" || fail "no 'signal SIGTERM' in the log"
echo "$LOG1" | grep -q 'Shutdown drain:' && pass "log carries 'Shutdown drain:'" || fail "no 'Shutdown drain:' in the log"

# ── C. the marker ─────────────────────────────────────────────────────────────
MARKER=$(find "$ROOT" -name last-shutdown.json | head -1)
if [ -n "$MARKER" ] && jq -e '.clean == true' "$MARKER" >/dev/null 2>&1; then
  pass "last-shutdown.json clean:true"
else
  fail "last-shutdown.json not clean:true: ${MARKER:-absent} $(cat "$MARKER" 2>/dev/null)"
fi

# ── F. lifecycle rows after the stop ──────────────────────────────────────────
ROWS=$(lifecycle_rows)
echo "lifecycle rows after the stop:"; echo "$ROWS" | sed 's/^/  /'
if [ "$ROWS" = NO_STORE ] || [ -z "$ROWS" ]; then
  fail "no lifecycle rows to read"
else
  BAD=$(echo "$ROWS" | awk '!($2=="open" || ($2=="closed" && $3=="pty-exit"))')
  [ -z "$BAD" ] && pass "every lifecycle row is open or pty-exit" || fail "non-restorable rows: $(echo "$BAD" | tr '\n' ';')"
fi

# ── D/E. the restart ──────────────────────────────────────────────────────────
CRASH_BEFORE=$(find "$ROOT" -name 'crash_*.txt' | wc -l)
MANIFEST=$(mktemp); log_manifest > "$MANIFEST"
systemctl --user reset-failed "$UNIT" >/dev/null 2>&1
start_unit
wait_healthy || { fail "subject did not come back"; exit 1; }
sleep 30
LOG2=$(logs_since "$MANIFEST"); rm -f "$MANIFEST"
echo "$LOG2" | grep -q 'previous shutdown was clean' && pass "boot classified the stop as clean" || fail "boot did not log 'previous shutdown was clean'"
echo "$LOG2" | grep -q 'previous shutdown was NOT clean' && fail "the restart logged 'previous shutdown was NOT clean'"
CRASH_AFTER=$(find "$ROOT" -name 'crash_*.txt' | wc -l)
[ "$CRASH_AFTER" = "$CRASH_BEFORE" ] && pass "no new crash breadcrumb" || fail "crash breadcrumbs $CRASH_BEFORE -> $CRASH_AFTER"
curl -s "$BASE/health" | grep -q '"errored"' && fail "/health reads errored" || pass "/health not errored"
RESUMED=$(resume_ids | grep -c . || true)
echo "resumed: $(resume_ids | tr '\n' ' ')"
if [ "${#TIDS[@]}" -gt 0 ] && [ "$RESUMED" -ge "${#TIDS[@]}" ]; then
  pass "$RESUMED claude --resume process(es) for ${#TIDS[@]} session(s)"
else
  fail "$RESUMED claude --resume process(es) for ${#TIDS[@]} session(s)"
fi

# ── G. the second signal ──────────────────────────────────────────────────────
PID=$(main_pid)
SINCE=$(date +%s)
T0=$(now_ms)
kill -TERM "$PID"; sleep 0.1; kill -TERM "$PID" 2>/dev/null
while kill -0 "$PID" 2>/dev/null && [ $(( $(now_ms) - T0 )) -lt 5000 ]; do sleep 0.02; done
GONE_MS=$(( $(now_ms) - T0 ))
sleep 2
JG=$(journal_since "$SINCE" | grep -E 'Main process exited|Deactivated|Failed' | tr '\n' '|')
echo "second-signal: gone after ${GONE_MS} ms; journal: $JG"
if [ "$GONE_MS" -lt 1000 ] && echo "$JG" | grep -q 'code=killed, status=15/TERM'; then
  pass "a second SIGTERM ends the process at once, by SIGTERM"
else
  fail "second SIGTERM: ${GONE_MS} ms, journal '$JG'"
fi

echo "== $FAILS failure(s)"
[ "$FAILS" = 0 ]
