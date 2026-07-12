#!/usr/bin/env bash
# qontinui session-restore — Claude SessionStart HOOK (confirmation/liveness).
#
# Delivered to Claude Code ADDITIVELY via `--settings <claude_hook_settings.json>`
# (the identity shim appends that flag). NEVER written into the user's
# `~/.claude/settings.json` — the delivery is a separate runner-owned file, so
# this is zero-touch for every user out of the box (plan §2 Principle 2, §4
# `capture_hook_delivery`; proven by the Phase 0 probe).
#
# Claude invokes this on a `SessionStart` event, piping a JSON payload on stdin:
#   { "session_id": "<id>", "source": "startup" | "resume", ... }
# It fires on BOTH a fresh `--session-id` startup (source:"startup", id = the
# runner-pinned uuid) AND a `--resume <id>` (source:"resume", same id) — even
# when Claude is "Not logged in" (identity capture precedes auth). We forward
# {session_id, source, terminal_id, provider, cwd} to the runner's loopback
# control server so it CONFIRMS the (provisional, spawn-time) record.
#
# Identity is ALREADY deterministic from the spawn-time `--session-id` pin +
# synchronous record; this POST is confirmation only and is fully fail-open —
# a missing curl/python/port/id is a silent no-op (exit 0) and NEVER blocks or
# breaks Claude's startup.
#
# Env (injected by the runner at PTY spawn — the identity seam):
#   QONTINUI_TERMINAL_ID            the per-PTY terminal id to correlate on
#   QONTINUI_INSTALL_INTERCEPT_PORT the bound runner loopback API port
set -u

payload="$(cat 2>/dev/null || true)"
port="${QONTINUI_INSTALL_INTERCEPT_PORT:-}"
term="${QONTINUI_TERMINAL_ID:-}"
[ -z "$port" ] && exit 0
command -v curl >/dev/null 2>&1 || exit 0

# Extract session_id + source from the stdin JSON. Prefer python (robust JSON);
# the field names are stable across Claude versions (session_id, source).
extract() {
  printf '%s' "$payload" | python -c "import sys,json
try:
  d=json.load(sys.stdin); print(d.get('$1',''))
except Exception:
  print('')
" 2>/dev/null
}
sid="$(extract session_id)"
src="$(extract source)"
[ -z "$sid" ] && exit 0
[ -z "$src" ] && src="startup"

# JSON-escape the cwd (backslashes + quotes) so a Windows path is valid JSON.
cwd_json=$(printf '%s' "${PWD:-}" | sed 's/\\/\\\\/g; s/"/\\"/g')

# Include the effective account config dir the runner set on this PTY child
# (session.rs sets CLAUDE_CONFIG_DIR) so the record binds the CORRECT Claude
# account for an autonomous boot-resume. An EMPTY/unset value means the default
# account — send NO config_dir field then (never a bogus "", which would resume
# under CLAUDE_CONFIG_DIR="").
cfg_field=""
if [ -n "${CLAUDE_CONFIG_DIR:-}" ]; then
  cfg_json=$(printf '%s' "$CLAUDE_CONFIG_DIR" | sed 's/\\/\\\\/g; s/"/\\"/g')
  cfg_field=",\"config_dir\":\"$cfg_json\""
fi

body="{\"terminal_id\":\"$term\",\"session_id\":\"$sid\",\"source\":\"$src\",\"provider\":\"claude\",\"cwd\":\"$cwd_json\"$cfg_field}"
curl -fsS --connect-timeout 3 --max-time 10 \
  -X POST "http://127.0.0.1:${port}/control/session-open" \
  -H 'Content-Type: application/json' \
  -d "$body" >/dev/null 2>&1 || true
exit 0
