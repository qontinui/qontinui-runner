#!/usr/bin/env bash
# qontinui session-restore CODEX IDENTITY shim (bash / Git-Bash / Unix).
#
# DISTINCT from identity_shim.bash. Codex has NO session-id flag — it GENERATES
# its own session id (a UUIDv7 it writes into
# `$CODEX_HOME/sessions/.../rollout-*.jsonl`'s line-1 `session_meta`). Appending
# any identity flag would BREAK the codex argv. So this wrapper NEVER mutates the
# user's argv and NEVER pins an id. Its only job is to SIGNAL
# the runner that a codex session is starting in THIS terminal, so the runner
# can begin its read-back capture (watch CODEX_HOME for the post-spawn rollout —
# `install_effects_producer::routes()` → `/control/session-open`, then
# `session::codex_capture::capture_and_record`).
#
# Always-on (NOT gated by QONTINUI_INSTALL_INTERCEPT_ENABLED — the out-of-box
# session-restore guarantee). Materialized per-terminal by the runner
# (`shim_materializer::materialize_identity`) from this TEMPLATE; the runner
# substitutes the `@@…@@` placeholders. Do NOT run it raw.
#
# Hard invariants (mirrored from identity_shim.bash):
#   * FAIL-OPEN: any failure -> exec the REAL codex unchanged. A user's shell
#     must NEVER be bricked by session-restore being unavailable.
#   * TRANSPARENT: stdio is inherited (exec), so codex sees its TTY and the user
#     sees byte-identical output + exit code. ARGS ARE PASSED THROUGH UNCHANGED.
#   * RECURSION-GUARD: QONTINUI_INSTALL_INTERCEPT_GUARD=1 already set -> pure
#     passthrough (a nested invocation must not re-signal).
#   * NEVER touch/relocate CODEX_HOME — auth lives there; relocating breaks the
#     user's `codex login`. The runner already isolates CODEX_HOME at spawn.
#
# Placeholders:
#   @@TOOL@@      the wrapped program name (always `codex` here)
#   @@SHIM_DIR@@  absolute path of this shim's own bin dir (skipped in the
#                 real-tool PATH scan to avoid recursion)
set -u

TOOL="@@TOOL@@"
SHIM_DIR="@@SHIM_DIR@@"

# ---------------------------------------------------------------------------
# Resolve the REAL codex: first match on PATH that is NOT inside SHIM_DIR.
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

# Run the real codex, replacing this process. Strips our dir from PATH as a last
# resort when the real tool couldn't be resolved. ARGS UNCHANGED — never inject.
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

# Recursion guard: a nested invocation is a pure passthrough — never re-signal.
if [ "${QONTINUI_INSTALL_INTERCEPT_GUARD:-}" = "1" ]; then
  exec_real "$@"
fi

# Best-effort signal to the runner loopback that a codex session is starting in
# this terminal. NO session_id field (codex generates its own; the runner reads
# it back). This is what triggers the runner-side read-back capture. Never
# load-bearing — short timeout, all failures ignored.
notify_session_open() {
  local port="${QONTINUI_INSTALL_INTERCEPT_PORT:-}"
  local term="${QONTINUI_TERMINAL_ID:-}"
  [ -z "$port" ] && return 0
  command -v curl >/dev/null 2>&1 || return 0
  # Report the cwd in the SAME frame codex records it (Windows form). In
  # Git-Bash `$PWD` is the mingw `/c/...` form but codex (a Windows binary)
  # writes `C:\...` into the rollout session_meta — so `pwd -W` (MSYS Windows
  # form `C:/...`) is what makes the runner's cwd-match succeed. Falls back to
  # `$PWD` on a shell without `pwd -W` (real Unix). The runner ALSO folds the
  # mingw form defensively, but emitting the Windows form here is the primary.
  local cwd_raw
  cwd_raw="$(pwd -W 2>/dev/null || printf '%s' "$PWD")"
  local cwd_json
  cwd_json=$(printf '%s' "$cwd_raw" | sed 's/\\/\\\\/g; s/"/\\"/g')
  local body
  body="{\"terminal_id\":\"$term\",\"provider\":\"$TOOL\",\"source\":\"startup\",\"cwd\":\"$cwd_json\"}"
  curl -fsS --connect-timeout 3 --max-time 10 \
      -X POST "http://127.0.0.1:$port/control/session-open" \
      -H 'Content-Type: application/json' \
      -d "$body" >/dev/null 2>&1 || true
}

notify_session_open
# Exec the REAL codex with the user's args UNCHANGED — no flag injection.
exec_real "$@"
