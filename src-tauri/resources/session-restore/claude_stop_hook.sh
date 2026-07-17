#!/usr/bin/env bash
# qontinui session-autonomy — Claude `Stop` HOOK (continuation verdict).
# Plan `2026-07-17-session-autonomy-fabric.md`, Phase 1.
#
# Delivered to Claude Code ADDITIVELY via the SAME `--settings
# <claude_hook_settings.json>` carrier as the SessionStart confirmation hook
# (the identity shim appends that flag; nothing is ever written to the user's
# `~/.claude/settings.json`).
#
# Claude invokes this when the agent is about to END A TURN, piping a JSON
# payload on stdin:
#   { "session_id": "<id>", "stop_hook_active": true|false, ... }
#
# The script is a DUMB CURL by design (plan D4): all policy lives in the
# runner's loopback verdict endpoint. It POSTs the stdin payload to
#   POST http://127.0.0.1:{port}/sessions/{id}/continuation-verdict
# and maps the response:
#   {"decision":"block","prompt":"<text>"} -> stdout {"decision":"block","reason":"<text>"}
#   anything else                          -> no output (allow the stop)
#
# FAIL-OPEN INVARIANTS (a broken runner/coord must NEVER trap a session):
#   - missing port / terminal id / curl / python  -> exit 0, no output
#   - curl error / non-2xx / undecodable response -> exit 0, no output
#   - `stop_hook_active:true` in the payload      -> exit 0 BEFORE the POST
#     (never re-block a continuation the hook chain already forced; the
#     endpoint honors the same guard as defense in depth)
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
command -v python >/dev/null 2>&1 || exit 0

# Loop guard, local leg: a Stop fired while a hook-forced continuation is
# already active must never be re-blocked. Cheap textual check (the endpoint
# re-checks with a real JSON parse).
case "$payload" in
  *'"stop_hook_active":true'* | *'"stop_hook_active": true'*) exit 0 ;;
esac

# Session key: prefer the runner terminal id (the endpoint resolves it against
# local state), else the Claude session id from the hook payload.
sid="$term"
if [ -z "$sid" ]; then
  sid="$(printf '%s' "$payload" | python -c "import sys,json
try:
  d=json.load(sys.stdin); print(d.get('session_id',''))
except Exception:
  print('')
" 2>/dev/null)"
fi
[ -z "$sid" ] && exit 0

resp="$(printf '%s' "$payload" | curl -fsS --connect-timeout 2 --max-time 10 \
  -X POST "http://127.0.0.1:${port}/sessions/${sid}/continuation-verdict" \
  -H 'Content-Type: application/json' \
  --data-binary @- 2>/dev/null || true)"
[ -z "$resp" ] && exit 0

# Map a block verdict to the Claude Stop-hook JSON contract; anything else
# (allow / garbage) produces NO output, which lets the stop proceed.
printf '%s' "$resp" | python -c "import sys,json
try:
  d=json.load(sys.stdin)
except Exception:
  sys.exit(0)
if d.get('decision') == 'block':
  reason = d.get('prompt') or 'Are there follow-ups? If so work on them; if nothing is left, clean up worktrees/branches and mark the session finished.'
  print(json.dumps({'decision': 'block', 'reason': reason}))
" 2>/dev/null || true
exit 0
