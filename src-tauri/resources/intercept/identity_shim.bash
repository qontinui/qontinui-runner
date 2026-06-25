#!/usr/bin/env bash
# qontinui session-restore IDENTITY shim (bash / Git-Bash / Unix).
#
# Always-on (NOT gated by QONTINUI_INSTALL_INTERCEPT_ENABLED — the out-of-box
# session-restore guarantee, plan §3b). Materialized per-terminal by the runner
# (`install_effects_producer::intercept::shim_materializer::materialize_identity`)
# from this TEMPLATE; the runner substitutes the `@@…@@` placeholders. Do NOT
# run it raw.
#
# Job (plan §3b "Determinism mechanism"):
#   The runner pre-generates a session UUID per terminal and injects it as
#   QONTINUI_PINNED_SESSION_ID. This shim wraps the provider CLI (claude/gemini)
#   and APPENDS `--session-id $QONTINUI_PINNED_SESSION_ID` to the real argv so a
#   HAND-STARTED `claude`/`gemini` is pinned to the runner-known id — the same
#   deterministic identity the "Launch AI Session" path already gets. The runner
#   already recorded the session authoritatively at spawn (zero round-trip); the
#   SessionStart hook POST is confirmation/liveness only.
#
# Hard invariants (mirrored from the install shim, plan §6):
#   * FAIL-OPEN: any failure -> exec the REAL provider unchanged. A user's
#     shell must NEVER be bricked by session-restore being unavailable.
#   * TRANSPARENT: stdio is inherited (exec), so the provider sees its TTY and
#     the user sees byte-identical output + exit code.
#   * RECURSION-GUARD: QONTINUI_INSTALL_INTERCEPT_GUARD=1 already set -> pure
#     passthrough (a nested invocation, e.g. a subagent, must not re-pin).
#   * DON'T DOUBLE-PIN: if the user already passed --session-id/--resume (or the
#     resume subcommand), do NOT append our id — their explicit choice wins.
#
# Placeholders:
#   @@TOOL@@      the wrapped provider program name (claude/gemini)
#   @@SHIM_DIR@@  absolute path of this shim's own bin dir (skipped in the
#                 real-tool PATH scan to avoid recursion)
set -u

TOOL="@@TOOL@@"
SHIM_DIR="@@SHIM_DIR@@"

# ---------------------------------------------------------------------------
# Resolve the REAL provider: first match on PATH that is NOT inside SHIM_DIR.
# ---------------------------------------------------------------------------
resolve_real() {
  local IFS=':'
  local dir cand
  for dir in $PATH; do
    [ -z "$dir" ] && continue
    case "${dir%/}" in
      "${SHIM_DIR%/}") continue ;;
    esac
    for cand in "$dir/$TOOL" "$dir/$TOOL.cmd" "$dir/$TOOL.exe"; do
      if [ -x "$cand" ] || [ -f "$cand" ]; then
        printf '%s' "$cand"
        return 0
      fi
    done
  done
  return 1
}

REAL="$(resolve_real || true)"

# Run the real provider, replacing this process (we don't need an exit code
# beyond what exec propagates). Strips our dir from PATH as a last resort when
# the real tool couldn't be resolved.
exec_real() {
  if [ -n "${REAL:-}" ]; then
    QONTINUI_INSTALL_INTERCEPT_GUARD=1 exec "$REAL" "$@"
  fi
  local newpath="" d
  local IFS=':'
  for d in $PATH; do
    case "${d%/}" in "${SHIM_DIR%/}") continue ;; esac
    if [ -z "$newpath" ]; then newpath="$d"; else newpath="$newpath:$d"; fi
  done
  QONTINUI_INSTALL_INTERCEPT_GUARD=1 PATH="$newpath" exec "$TOOL" "$@"
}

# Recursion guard: a nested invocation is a pure passthrough — never re-pin.
if [ "${QONTINUI_INSTALL_INTERCEPT_GUARD:-}" = "1" ]; then
  exec_real "$@"
fi

PINNED="${QONTINUI_PINNED_SESSION_ID:-}"

# Does the user's argv already choose a session? Then don't double-pin.
user_chose_session=0
for a in "$@"; do
  case "$a" in
    --session-id|--session-id=*|--resume|--resume=*|-r|resume|--continue|-c)
      user_chose_session=1; break ;;
  esac
done

# Best-effort confirmation/liveness POST to the runner loopback (the existing
# install-effects server on the seam-injected port). Identity is ALREADY pinned
# + recorded at spawn; this is the "hook fired" signal, never load-bearing.
notify_session_open() {
  local port="${QONTINUI_INSTALL_INTERCEPT_PORT:-}"
  local term="${QONTINUI_TERMINAL_ID:-}"
  local sid="$1" source="$2"
  [ -z "$port" ] && return 0
  [ -z "$sid" ] && return 0
  command -v curl >/dev/null 2>&1 || return 0
  local cwd_json
  cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
  local body
  body="{\"terminal_id\":\"$term\",\"session_id\":\"$sid\",\"source\":\"$source\",\"provider\":\"$TOOL\",\"cwd\":\"$cwd_json\"}"
  curl -fsS --connect-timeout 3 --max-time 10 \
      -X POST "http://127.0.0.1:$port/control/session-open" \
      -H 'Content-Type: application/json' \
      -d "$body" >/dev/null 2>&1 || true
}

if [ "$user_chose_session" -eq 1 ] || [ -z "$PINNED" ]; then
  # User chose their own session (or we have no pinned id) — passthrough.
  # Still send a best-effort confirmation when WE know the pinned id and the
  # user did not override it (so a bare `claude` with our pin still confirms).
  if [ "$user_chose_session" -eq 0 ] && [ -n "$PINNED" ]; then
    notify_session_open "$PINNED" "startup"
  fi
  exec_real "$@"
fi

# Append our pinned id and run the real provider. Confirmation POST first
# (fire-and-forget); then exec so stdio/exit are transparent.
notify_session_open "$PINNED" "startup"
exec_real "$@" --session-id "$PINNED"
