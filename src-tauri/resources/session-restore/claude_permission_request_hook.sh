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
# The script is a DUMB CURL: every decision lives in the runner. curl streams
# the payload from stdin to
#   POST http://127.0.0.1:{port}/sessions/{terminal id}/permission-request
# which records the prompt, reads the tenant's `permission_auto_accept` dial,
# and answers `{"decision": "allow" | "none", ...}`. This script then prints
#   {"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}
# ONLY when the answer says `"decision": "allow"` AND echoes this terminal's
# nonce exactly — and NOTHING otherwise. Printing nothing is Claude Code's "no
# decision": the dialog shows as it always has.
#
# Two per-spawn secrets the runner put in this session's env prove the answer
# came from the runner that spawned the session:
#   - the REQUEST KEY (QONTINUI_PERMREQ_KEY) is SENT, in X-Qontinui-Permreq-Key;
#     the pad answers allow only for the terminal's own key;
#   - the NONCE (QONTINUI_PERMREQ_NONCE) is NEVER sent; the pad echoes it only
#     on an allow for the right key, and this script requires the exact echo.
# The key is derived one-way from the nonce, so a process squatting the
# loopback port while the runner is down learns only the key and cannot forge
# the echo; and a caller that does not hold the key cannot harvest a nonce
# from the running pad.
#
# FAIL-CLOSED-TO-THE-DIALOG INVARIANTS (a broken runner must never answer a
# prompt, and must never break a session):
#   - missing port / terminal id / key / nonce / curl   -> exit 0, no output
#   - curl error / timeout / non-2xx / any other body   -> exit 0, no output
#   - an allow without the exact nonce echo             -> exit 0, no output
#   - ONE bound for the whole exchange: curl reads the payload from stdin
#     itself, inside its own `--max-time 1`, so reading a large payload (a
#     `Write` carrying file content) and the round trip share the ~1 s budget
#     (`--connect-timeout 1 --max-time 1`; process start-up comes on top). The
#     dialog renders while this runs, and an allow inside that budget
#     dismisses it with no keystroke (Phase 0, T11).
#   - no JSON parser is required: the answer is matched with bash regexes
#
# Env (injected by the runner at PTY spawn — the identity seam):
#   QONTINUI_RUNNER_API_PORT         the runner's loopback API port
#   QONTINUI_INSTALL_INTERCEPT_PORT  fallback port (same server by default)
#   QONTINUI_TERMINAL_ID             the per-PTY terminal id — the pad's key
#   QONTINUI_PERMREQ_KEY             the per-spawn request key this script sends
#   QONTINUI_PERMREQ_NONCE           the per-spawn nonce the allow must echo
#
# A `claude` started by hand outside the runner has no QONTINUI_TERMINAL_ID,
# so this exits silently there (D5).
set -u

port="${QONTINUI_RUNNER_API_PORT:-${QONTINUI_INSTALL_INTERCEPT_PORT:-}}"
term="${QONTINUI_TERMINAL_ID:-}"
nonce="${QONTINUI_PERMREQ_NONCE:-}"
key="${QONTINUI_PERMREQ_KEY:-}"
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
# The nonce is lowercase hex (it is matched inside a regex below) and long
# enough not to be guessed.
case "$nonce" in
  "" | *[!0-9a-f]*) exit 0 ;;
esac
[ "${#nonce}" -ge 16 ] || exit 0
# The key is lowercase hex too (it is placed in a header line).
case "$key" in
  "" | *[!0-9a-f]*) exit 0 ;;
esac
[ "${#key}" -ge 16 ] || exit 0
command -v curl >/dev/null 2>&1 || exit 0

# `-T -` streams stdin as the request body DURING the transfer, so the read is
# inside `--max-time`; `--data-binary @-` would slurp all of stdin before the
# clock starts. `-X POST` overrides `-T`'s PUT; `Expect:` suppresses the
# 100-continue round trip; `--noproxy '*'` keeps a proxy env var from routing
# a loopback call elsewhere. stdin is the hook's own, inherited by curl.
resp="$(curl -fsS --noproxy '*' --connect-timeout 1 --max-time 1 \
  -X POST -H 'Content-Type: application/json' -H 'Expect:' \
  -H "X-Qontinui-Permreq-Key: ${key}" -T - \
  "http://127.0.0.1:${port}/sessions/${term}/permission-request" 2>/dev/null)" || exit 0

# Match the KEY and the VALUE together, so a body that merely contains the
# word "allow" somewhere else can never answer a prompt — and require the
# exact nonce echo beside it.
if [[ $resp =~ \"decision\"[[:space:]]*:[[:space:]]*\"allow\" ]] &&
  [[ $resp =~ \"nonce\"[[:space:]]*:[[:space:]]*\"${nonce}\" ]]; then
  printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'
fi
exit 0
