#!/usr/bin/env bash
# qontinui session-restore — Claude `SessionEnd` HOOK (deliberate-end report).
#
# Delivered to Claude Code ADDITIVELY via the SAME runner-owned `--settings`
# carrier as the SessionStart / PreCompact hooks (nothing is ever written to
# the user's `~/.claude/settings.json`), and registered UNCONDITIONALLY, so it
# rides whichever of the two carrier files the continuation flag selects. See
# `session::claude_hook`.
#
# Claude invokes this when a session ENDS, piping a JSON payload on stdin:
#   { "session_id": "<id>", "reason": "prompt_input_exit"|"clear"|"resume"|
#     "logout"|"other", ... }
# `/exit` is `prompt_input_exit`; a SIGKILL / OOM kill / crash fires nothing.
# That difference is the whole point: it is the runner's only POSITIVE
# evidence that the user ended a session on purpose, so such a session is not
# brought back at the next boot while a killed one still is.
#
# A DUMB CURL by design: the payload is forwarded verbatim to
#   POST http://127.0.0.1:{port}/sessions/{terminal}/session-end
# and every decision (which reasons count) lives in the runner
# (`session::claude_ended_watch`). The response is ignored and nothing is
# printed.
#
# FAIL-OPEN INVARIANTS (a broken or absent runner must NEVER hold up an exit):
#   - missing port / curl                      -> exit 0, no output
#   - curl error / non-2xx / unreachable       -> exit 0, no output
#   - bounded: 1 s to connect, 2 s in total
#
# Env (injected by the runner at PTY spawn — the identity seam):
#   QONTINUI_RUNNER_API_PORT         the runner's :9876 loopback API port
#   QONTINUI_INSTALL_INTERCEPT_PORT  fallback port (same server by default)
#   QONTINUI_TERMINAL_ID             the per-PTY terminal id: the runner
#                                    records the report only when the payload's
#                                    `session_id` is OPEN on this terminal
set -u

# Drain stdin with a BUILTIN, not `cat` (see `claude_session_hook.sh`).
payload=""
IFS= read -r -t 1 -d '' payload || true
port="${QONTINUI_RUNNER_API_PORT:-${QONTINUI_INSTALL_INTERCEPT_PORT:-}}"
# The port goes into the URL: digits only, else nothing is sent.
case "$port" in
  "" | *[!0-9]*) exit 0 ;;
esac
[ -z "$payload" ] && exit 0
command -v curl >/dev/null 2>&1 || exit 0

# The terminal id is REQUIRED: the runner records the report only for the
# session OPEN on that terminal. It becomes a URL PATH SEGMENT, so it is kept
# to the id alphabet (no `/`, `?`, `#` or dot segments); anything else, or no
# id at all, sends nothing.
term="${QONTINUI_TERMINAL_ID:-}"
case "$term" in
  "" | "." | ".." | *..* | */.* | *[!A-Za-z0-9._-]*) exit 0 ;;
esac

printf '%s' "$payload" | curl -fsS --connect-timeout 1 --max-time 2 \
  -X POST "http://127.0.0.1:${port}/sessions/${term}/session-end" \
  -H 'Content-Type: application/json' \
  --data-binary @- >/dev/null 2>&1 || true
exit 0
