#!/usr/bin/env bash
# qontinui session-autonomy — Claude `PreCompact` HOOK (context-exhaustion
# signal). Plan `2026-07-17-session-autonomy-fabric.md`, Phase 7.
#
# Delivered to Claude Code ADDITIVELY via the SAME runner-owned `--settings`
# carrier as the SessionStart hooks (the identity shim appends that flag;
# nothing is ever written to the user's `~/.claude/settings.json`). Like them
# this hook is registered UNCONDITIONALLY, so it rides whichever of the two
# carrier files the continuation flag selects — unlike the `Stop` hook, which
# only ever ships in the armed one. See `session::claude_hook`.
#
# Claude invokes this when a context compaction is IMMINENT, piping a JSON
# payload on stdin:
#   { "session_id": "<id>", "trigger": "auto"|"manual", ... }
#
# The script is a DUMB CURL by design (mirrors the Stop hook): all policy —
# the QONTINUI_CONTEXT_HANDOFF flag gate, the auto-vs-manual trigger filter,
# the once-per-session debounce shared with the grid-scan watcher — lives in
# the runner's loopback endpoint:
#   POST http://127.0.0.1:{port}/sessions/{id}/context-low
# The response is ignored; this hook NEVER emits output and NEVER blocks the
# compaction (PreCompact is a signal, not a gate).
#
# FAIL-OPEN INVARIANTS (a broken runner must NEVER break a session):
#   - missing port / session key / curl        -> exit 0, no output
#   - curl error / non-2xx / timeout           -> exit 0, no output
#
# Env (injected by the runner at PTY spawn — the identity seam):
#   QONTINUI_RUNNER_API_PORT         the runner's :9876 loopback API port
#   QONTINUI_INSTALL_INTERCEPT_PORT  fallback port (same server by default)
#   QONTINUI_TERMINAL_ID             the per-PTY terminal id
set -u

payload="$(cat 2>/dev/null || true)"
port="${QONTINUI_RUNNER_API_PORT:-${QONTINUI_INSTALL_INTERCEPT_PORT:-}}"
term="${QONTINUI_TERMINAL_ID:-}"
[ -z "$port" ] && exit 0
command -v curl >/dev/null 2>&1 || exit 0

# Session key: prefer the runner terminal id (the grid-scan watcher's key
# space, so both signals share one debounce), else the Claude session id
# from the hook payload.
sid="$term"
if [ -z "$sid" ]; then
  command -v python >/dev/null 2>&1 || exit 0
  sid="$(printf '%s' "$payload" | python -c "import sys,json
try:
  d=json.load(sys.stdin); print(d.get('session_id',''))
except Exception:
  print('')
" 2>/dev/null)"
fi
[ -z "$sid" ] && exit 0

printf '%s' "$payload" | curl -fsS --connect-timeout 2 --max-time 10 \
  -X POST "http://127.0.0.1:${port}/sessions/${sid}/context-low" \
  -H 'Content-Type: application/json' \
  --data-binary @- >/dev/null 2>&1 || true
exit 0
