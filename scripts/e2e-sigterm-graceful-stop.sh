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
#                                        [--sessions N] [--account A] [--keep]
#
# Defaults: --port 9893, --display from $DISPLAY (else :99), --sessions 2.
# Each session runs `claude` in a runner terminal; the first is sent `/effort max`.
# --account <config dir> runs every session's `claude` against that Claude
# config dir (a logged-in account on this box), set as CLAUDE_CONFIG_DIR on the
# subject itself: an isolated subject has no account roster to name one from.
# A session is only resumable once it has a conversation on disk, which an
# unauthenticated `claude` never writes.
# --keep leaves the temp instance root behind for inspection (the unit and every
# process the run started are always removed).
#
# Exit: 0 every check passed; 1 a check failed (each failure is printed as
# `FAIL: …`); 2 usage / environment; 3 the subject never became healthy.
#
# Checks (plan Phase 2, "Evidence owed"):
#   0. (Phase 0 probe) the live process catches SIGTERM (the handler is in)
#      and neither catches nor ignores SIGALRM (the backstop works).
#   A. `systemctl --user stop` completes in < 27 s and the journal records a
#      successful deactivation, not a failure.
#   B. the subject's log carries `signal SIGTERM` and `Shutdown drain:`.
#   C. last-shutdown.json reads clean:true.
#   F. every lifecycle row of the sessions this run opened (snapshot + WAL
#      replayed) is restorable: `open`, or closed with a restorable reason
#      (`pty-exit`, `poll-dead` — RESTORABLE_CLOSE_REASONS).
#   D. on restart (log written AFTER the restart only): `previous shutdown was
#      clean`, never `NOT clean`, no new crash breadcrumb, /health not `errored`.
#   E. every session this run opened is back as a `claude --resume <its id>`.
#   G. the first SIGTERM does NOT end the process at once (it runs the graceful
#      path), and a second one 100 ms later ends it within 1 s, by SIGTERM (the
#      wait status, observed directly on a launch outside systemd).
#
# State it leaves: the unit, every process it started and the instance root are
# removed, and so are the sessions' transcripts under the account's
# `projects/<encoded root>/`. One thing is NOT removed: accepting claude's
# folder-trust dialog records the (unique, now deleted) temp path as trusted in
# the account's `.claude.json`. That file is shared with every live session on
# the account, so the script does not rewrite it; the entry names a path that
# no longer exists.

set -u

BIN="${1:-}"
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "usage: $0 <runner-binary> [--port N] [--display :D] [--sessions N] [--account DIR] [--keep]" >&2; exit 2; }
BIN=$(readlink -f "$BIN")
shift
PORT=9893
DISP="${DISPLAY:-:99}"
NSESS=2
KEEP=0
ACCOUNT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --display) DISP="$2"; shift 2 ;;
    --sessions) NSESS="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --account) ACCOUNT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
for tool in systemd-run systemctl journalctl jq python3 curl; do
  command -v "$tool" >/dev/null || { echo "$tool not found (Linux + systemd user manager required)" >&2; exit 2; }
done
if curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/health"; then
  echo "port $PORT already answers — pick another with --port" >&2; exit 2
fi
if [ -n "$(systemctl --user list-units --all --no-legend "qontinui-sigterm-e2e-$PORT.service" 2>/dev/null)" ]; then
  echo "unit qontinui-sigterm-e2e-$PORT already exists (another run?) — pick another --port" >&2; exit 2
fi

UNIT=qontinui-sigterm-e2e-$PORT
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/qontinui-sigterm-e2e.XXXXXX") && ROOT=$(readlink -f "$ROOT")
# An empty ROOT would make own_pids match EVERY process (cwd under "/"), and
# cleanup would signal them all — never continue without a real root.
if [ -z "$ROOT" ] || [ "$ROOT" = / ] || [ ! -d "$ROOT" ]; then
  echo "could not create the instance root" >&2; exit 2
fi
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
  case "$ROOT" in ""|/) return 0 ;; esac
  for d in /proc/[0-9]*; do
    pid=${d#/proc/}
    [ "$pid" = "$$" ] && continue
    case "$(readlink "$d/cwd" 2>/dev/null)" in
      "$ROOT"|"$ROOT"/*) echo "$pid"; continue ;;
    esac
    { tr '\0' '\n' < "$d/environ"; } 2>/dev/null | grep -qxF "QONTINUI_INSTANCE_ROOT=$ROOT" && echo "$pid"
  done
}

UNIT_STARTED=0
cleanup() {
  if [ "$UNIT_STARTED" = 1 ]; then
    systemctl --user stop "$UNIT" >/dev/null 2>&1
    systemctl --user reset-failed "$UNIT" >/dev/null 2>&1
  fi
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
  [ "$UNIT_STARTED" = 1 ] && systemctl --user is-active --quiet "$UNIT" && echo "WARN: unit $UNIT still active after cleanup"
  # The sessions' transcripts, which claude writes under the account dir.
  local cfg enc
  cfg="${ACCOUNT:-$HOME/.claude}"
  enc=$(printf '%s' "$ROOT" | sed 's/[^A-Za-z0-9]/-/g')
  [ "$KEEP" != 1 ] && [ -n "$enc" ] && [ -d "$cfg/projects/$enc" ] && rm -rf "${cfg:?}/projects/$enc"
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
    ${ACCOUNT:+--setenv=CLAUDE_CONFIG_DIR="$ACCOUNT"} \
    "$BIN" >/dev/null || { echo "systemd-run failed" >&2; exit 2; }
  UNIT_STARTED=1
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
lifecycle_rows() {   # optional: terminal ids to keep (space-separated)
  python3 - "$ROOT" "${1:-}" <<'PY'
import json, os, sys
root = sys.argv[1]
# The store is a snapshot plus a WAL; before the first compaction only the WAL
# may exist, so either file locates it.
found = [os.path.join(d, "terminal-sessions") for d, _, fs in os.walk(root)
         if "terminal-sessions.json" in fs or "terminal-sessions.wal.jsonl" in fs]
if not found:
    print("NO_STORE"); sys.exit(0)
base = found[0]
snap = base + ".json"
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
wal = base + ".wal.jsonl"
if os.path.exists(wal):
    for line in open(wal):
        try: d = json.loads(line)
        except Exception: continue
        if d.get("op") == "upsert" and isinstance(d.get("rec"), dict):
            r = d["rec"]; rows[r.get("claudeSessionId") or r.get("terminalId")] = r
        elif d.get("op") == "remove":
            rows.pop(d.get("id"), None)
keep = set(sys.argv[2].split()) if len(sys.argv) > 2 and sys.argv[2] else None
for k, r in rows.items():
    if keep is not None and r.get("terminalId") not in keep:
        continue
    print(f"{k} {r.get('state')} {r.get('closeReason') or '-'} {r.get('terminalId')}")
PY
}

# The claudeSessionIds of this run's sessions, from the lifecycle rows.
own_session_ids() { lifecycle_rows "${TIDS[*]:-}" | awk '{print $1}' | sort -u; }

resume_ids() {
  local pid
  for pid in $(own_pids); do
    { tr '\0' ' ' < "/proc/$pid/cmdline"; } 2>/dev/null | grep -o -- '--resume [0-9a-f-]\{36\}' || true
  done | sort -u
}
own_claude_cmdlines() {
  local pid
  for pid in $(own_pids); do
    { tr '\0' ' ' < "/proc/$pid/cmdline"; } 2>/dev/null | grep -E '(^|/)claude( |$)' | sed "s/^/  [$pid] /" || true
  done
}

echo "== subject: $BIN on :$PORT (display $DISP), root $ROOT, unit $UNIT"
start_unit
wait_healthy || { echo "subject never answered /health"; logs_since '' | tail -40; exit 3; }
echo "subject healthy, pid $(main_pid), $(curl -s "$BASE/health" | jq -c '{gitSha: (.gitSha // .data.gitSha), buildId: (.buildId // .data.buildId)}')"

# ── 0. Phase 0 probe: nothing in the live process catches SIGALRM ────────────
SIGCGT=$(awk '/^SigCgt:/{print $2}' "/proc/$(main_pid)/status" 2>/dev/null)
SIGIGN=$(awk '/^SigIgn:/{print $2}' "/proc/$(main_pid)/status" 2>/dev/null)
if [ -z "$SIGCGT" ] || [ -z "$SIGIGN" ]; then
  fail "could not read the subject's signal masks"
else
  # bit (signum - 1): SIGTERM 15 -> 14, SIGALRM 14 -> 13
  (( (16#$SIGCGT >> 14) & 1 )) && pass "SIGTERM is caught (the handler is installed)" || fail "SIGTERM is not caught (SigCgt=$SIGCGT)"
  if (( (16#$SIGCGT >> 13) & 1 )) || (( (16#$SIGIGN >> 13) & 1 )); then
    fail "SIGALRM is caught or ignored (SigCgt=$SIGCGT SigIgn=$SIGIGN) — the alarm backstop is unsafe"
  else
    pass "SIGALRM neither caught nor ignored (SigCgt=$SIGCGT SigIgn=$SIGIGN)"
  fi
fi

# Open the sessions.
TIDS=()
for n in $(seq 1 "$NSESS"); do
  body=$(jq -nc --arg t "sigterm-e2e-$n" --arg w "$ROOT" \
    '{title: $t, initial_command: "claude", working_dir: $w}')
  resp=$(curl -s -X POST "$BASE/terminals" -H 'Content-Type: application/json' -d "$body")
  tid=$(echo "$resp" | jq -r '.data.id // empty')
  [ -n "$tid" ] || { fail "could not open session $n: $(echo "$resp" | head -c 300)"; continue; }
  TIDS+=("$tid")
done
sleep 20
# A fresh working dir puts `claude` on its folder-trust dialog first, whose
# DEFAULT is "No, exit": move to "Yes, I trust this folder" (Down), then Enter.
# `/terminals/{id}/write` takes base64.
DOWN_B64=$(printf '\033[B' | base64); CR_B64=$(printf '\r' | base64)
screen() { curl -s "$BASE/terminals/$1/buffer" | jq -r '.data.data // empty' 2>/dev/null | base64 -d 2>/dev/null | sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g'; }
for tid in "${TIDS[@]}"; do
  SCR=$(screen "$tid" | tr -d ' ')
  if ! echo "$SCR" | grep -aq 'Itrustthisfolder' || ! echo "$SCR" | grep -aqE '❯[0-9.]*No,exit'; then
    fail "session $tid: the folder-trust dialog is not showing with 'No, exit' selected — not sending keys blind"
    continue
  fi
  curl -s -o /dev/null -X POST "$BASE/terminals/$tid/write" -H 'Content-Type: application/json' -d "{\"data\":\"$DOWN_B64\"}"
  sleep 1
  curl -s -o /dev/null -X POST "$BASE/terminals/$tid/write" -H 'Content-Type: application/json' -d "{\"data\":\"$CR_B64\"}"
done
sleep 12
# A session must have a conversation on disk to be resumable, so each gets one
# short prompt; the first is also put on `/effort max` (the sibling finding).
[ "${#TIDS[@]}" -gt 0 ] && curl -s -o /dev/null -X POST "$BASE/terminals/${TIDS[0]}/submit-prompt" \
  -H 'Content-Type: application/json' -d '{"message":"/effort max"}'
sleep 3
for tid in "${TIDS[@]}"; do
  curl -s -o /dev/null -X POST "$BASE/terminals/$tid/submit-prompt" \
    -H 'Content-Type: application/json' -d '{"message":"Reply with the single word: ok"}'
done
sleep 45
echo "claude processes before the stop:"; own_claude_cmdlines
for tid in "${TIDS[@]}"; do
  echo "screen of $tid (last lines):"
  screen "$tid" | tr -s '\r' '\n' | grep -av '^\s*$' | tail -6 | sed 's/^/    /'
done
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
echo "lifecycle rows after the stop (every row):"; lifecycle_rows | sed 's/^/  /'
# Judged: the rows of the sessions THIS run opened. The subject may open panes
# of its own, whose bookkeeping (e.g. superseded-terminal-reuse) predates the stop.
ROWS=$(lifecycle_rows "${TIDS[*]:-}")
echo "lifecycle rows of this run's sessions:"; echo "$ROWS" | sed 's/^/  /'
if [ "$ROWS" = NO_STORE ] || [ -z "$ROWS" ]; then
  fail "no lifecycle rows to read"
else
  BAD=$(echo "$ROWS" | awk '!($2=="open" || ($2=="closed" && ($3=="pty-exit" || $3=="poll-dead")))')
  [ -z "$BAD" ] && pass "every lifecycle row is open or closed restorable" || fail "non-restorable rows: $(echo "$BAD" | tr '\n' ';')"
fi

# ── D/E. the restart ──────────────────────────────────────────────────────────
OWN_SIDS=$(own_session_ids | tr '\n' ' ')
CRASH_BEFORE=$(find "$ROOT" -name 'crash_*.txt' | wc -l)
MANIFEST="$ROOT/.e2e-log-manifest"; log_manifest > "$MANIFEST"
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
echo "claude processes after the restart:"; own_claude_cmdlines
echo "resumed: $(resume_ids | tr '\n' ' ')"
MISSING=""
NOWN=$(echo $OWN_SIDS | wc -w)
[ "$NOWN" = "${#TIDS[@]}" ] || MISSING="$MISSING (only $NOWN of ${#TIDS[@]} opened sessions have a lifecycle row)"
for sid in $OWN_SIDS; do
  resume_ids | grep -q -- "--resume $sid" || MISSING="$MISSING $sid"
done
if [ "${#TIDS[@]}" -gt 0 ] && [ -z "$MISSING" ]; then
  pass "every session this run opened is back as claude --resume <its id> ($NOWN of ${#TIDS[@]})"
else
  fail "not resumed:${MISSING:- (no session ids recorded)} ($RESUMED --resume process(es) seen)"
fi

# ── G. the second signal ──────────────────────────────────────────────────────
# Run outside systemd so the wait status is observed directly: systemd counts a
# death by SIGTERM as a clean stop and logs no exit cause for it.
systemctl --user stop "$UNIT" >/dev/null 2>&1
env ${ACCOUNT:+CLAUDE_CONFIG_DIR="$ACCOUNT"} DISPLAY="$DISP" QONTINUI_INSTANCE_ROOT="$ROOT" QONTINUI_INSTANCE_NAME="sigterm-e2e-$PORT" \
  QONTINUI_PORT="$PORT" "$BIN" >/dev/null 2>&1 &
PID=$!
wait_healthy || fail "subject did not come back for the second-signal check"
GMAN="$ROOT/.e2e-log-manifest-g"; log_manifest > "$GMAN"
kill -TERM "$PID"
# Up to 2 s for the watcher's line to reach the file log; the graceful path
# itself takes several seconds, so the process is still alive meanwhile.
T1=$(now_ms); GLOG=""
while [ $(( $(now_ms) - T1 )) -lt 2000 ]; do
  GLOG=$(logs_since "$GMAN")
  echo "$GLOG" | grep -aq 'SIGTERM received' && break
  sleep 0.1
done
PSTATE=$(awk '{print $3}' "/proc/$PID/stat" 2>/dev/null)
# The graceful path's own line can only come from a process that ran it; a
# pid that merely still exists (dying, or an unreaped zombie) proves nothing.
if [ -n "$PSTATE" ] && [ "$PSTATE" != Z ] && [ "$PSTATE" != X ] \
   && echo "$GLOG" | grep -aq 'SIGTERM received'; then
  pass "the first SIGTERM started the graceful path (process alive, state $PSTATE, watcher logged it)"
else
  fail "the first SIGTERM did not start the graceful path (state '${PSTATE:-gone}', watcher line $(echo "$GLOG" | grep -ac 'SIGTERM received'))"
fi
T0=$(now_ms)
kill -TERM "$PID" 2>/dev/null
wait "$PID"; RC=$?
GONE_MS=$(( $(now_ms) - T0 ))
echo "second-signal: gone after ${GONE_MS} ms, wait status $RC (143 = SIGTERM)"
if [ "$GONE_MS" -lt 1000 ] && [ "$RC" = 143 ]; then
  pass "a second SIGTERM ends the process at once, by SIGTERM"
else
  fail "second SIGTERM: ${GONE_MS} ms, wait status $RC"
fi

echo "== $FAILS failure(s)"
[ "$FAILS" = 0 ]
