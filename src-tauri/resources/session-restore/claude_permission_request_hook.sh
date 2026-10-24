#!/usr/bin/env bash
# qontinui permission auto-accept — Claude `PermissionRequest` HOOK.
# Plan `2026-10-23-runner-sessions-stop-at-permission-dialogs-so-accept-them-by-default-with-a-tenant-switch`,
# Phase 2 (D2).
#
# Delivered to Claude Code ADDITIVELY via the runner-owned `--settings` carrier
# (nothing is ever written to the user's `~/.claude/settings.json`). Registered
# UNCONDITIONALLY — no dial gates the registration (D6) — so it rides whichever
# carrier variant the session gets. See `session::claude_hook`.
#
# Claude invokes this BEFORE it shows a permission dialog, piping the
# `PermissionRequest` payload on stdin:
#   { "hook_event_name": "PermissionRequest", "tool_name": "...",
#     "tool_input": {...}, "permission_mode": "...", ... }
#
# The script is a DUMB CURL: every decision lives in the runner. It POSTs the
# payload to
#   POST http://127.0.0.1:{port}/sessions/{terminal id}/permission-request
# which records the prompt, reads the tenant's `permission_auto_accept` dial,
# and answers `{"decision": "allow" | "none", ...}`. This script then prints
#   {"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}
# ONLY when the answer says `"decision": "allow"`, and NOTHING otherwise —
# printing nothing is Claude Code's "no decision": the dialog shows as it
# always has.
#
# FAIL-CLOSED-TO-THE-DIALOG INVARIANTS (a broken runner must never answer a
# prompt, and must never break a session):
#   - missing port / terminal id / curl                -> exit 0, no output
#   - curl error / timeout / non-2xx / any other body  -> exit 0, no output
#   - the whole round trip is bounded at ~1 s (`--connect-timeout 1
#     --max-time 1`): the dialog renders while this runs, and an allow inside
#     that budget dismisses it with no keystroke (Phase 0, T11)
#   - no JSON parser is required: the answer is matched with a bash regex
#
# Env (injected by the runner at PTY spawn — the identity seam):
#   QONTINUI_RUNNER_API_PORT         the runner's loopback API port
#   QONTINUI_INSTALL_INTERCEPT_PORT  fallback port (same server by default)
#   QONTINUI_TERMINAL_ID             the per-PTY terminal id — the pad's key
#
# A `claude` started by hand outside the runner has no QONTINUI_TERMINAL_ID,
# so this exits silently there (D5).
set -u

# Drain stdin with a BUILTIN (no `cat`: process creation is slow on the
# fleet's Windows/MSYS boxes). `-t 1` bounds an event that attaches no stdin.
payload=""
IFS= read -r -t 1 -d '' payload || true

port="${QONTINUI_RUNNER_API_PORT:-${QONTINUI_INSTALL_INTERCEPT_PORT:-}}"
term="${QONTINUI_TERMINAL_ID:-}"
[ -z "$port" ] && exit 0
# A port is digits; anything else would re-point the request.
case "$port" in
  *[!0-9]*) exit 0 ;;
esac
# The terminal id becomes a URL PATH SEGMENT, so it is confined to the id
# alphabet and dot segments are refused (curl collapses `.` / `..` before
# sending, which would move the POST onto a different route).
case "$term" in
  "" | "." | ".." | *..* | */.* | *[!A-Za-z0-9._-]*) exit 0 ;;
esac
command -v curl >/dev/null 2>&1 || exit 0

resp="$(printf '%s' "$payload" | curl -fsS --connect-timeout 1 --max-time 1 \
  -X POST "http://127.0.0.1:${port}/sessions/${term}/permission-request" \
  -H 'Content-Type: application/json' \
  --data-binary @- 2>/dev/null)" || exit 0

# Match the KEY and the VALUE together, so a body that merely contains the
# word "allow" somewhere else can never answer a prompt.
if [[ $resp =~ \"decision\"[[:space:]]*:[[:space:]]*\"allow\" ]]; then
  printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'
fi
exit 0
