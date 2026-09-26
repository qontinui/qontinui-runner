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
# It also drops the parent terminal's coord-mcp key variables
# (QONTINUI_COORD_MCP_NONCE_<K> / QONTINUI_COORD_MCP_CREDENTIAL_<K>, plan
# 2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages): a
# nested `claude` is a different session and must fall to the in-cwd document's
# default (the workdir key), not present the parent's. Residual: a `claude`
# run by absolute path bypasses this shim and still inherits them.
if [ "${QONTINUI_INSTALL_INTERCEPT_GUARD:-}" = "1" ]; then
  for __qv in $(compgen -e); do
    case "$__qv" in
      QONTINUI_COORD_MCP_NONCE_*|QONTINUI_COORD_MCP_CREDENTIAL_*) unset "$__qv" ;;
    esac
  done
  unset __qv
  exec_real "$@"
fi

PINNED="${QONTINUI_PINNED_SESSION_ID:-}"

# ---------------------------------------------------------------------------
# Claude SessionStart hook delivery (plan §4, Phase 2).
# For TOOL=claude ONLY, append `--settings <runner-app-data hook file>` so the
# runner's SessionStart confirmation hook is delivered ADDITIVELY — Claude
# merges it on top of any ~/.claude config WITHOUT writing to it. The flag rides
# alongside `--session-id`. Other providers (gemini) have no `--settings` flag,
# so this is gated on the tool name (Phase 3 delivers gemini's hook differently).
# The runner injects QONTINUI_CLAUDE_HOOK_SETTINGS with the absolute settings
# path; empty/unset => append nothing (fail-open — identity still rides the pin).
SETTINGS_ARGS=()
if [ "$TOOL" = "claude" ] && [ -n "${QONTINUI_CLAUDE_HOOK_SETTINGS:-}" ] \
    && [ -f "${QONTINUI_CLAUDE_HOOK_SETTINGS}" ]; then
  SETTINGS_ARGS=(--settings "${QONTINUI_CLAUDE_HOOK_SETTINGS}")
fi

# ---------------------------------------------------------------------------
# Universal coord-mcp delivery (mcp-config-universal-provisioning plan).
# For TOOL=claude ONLY, append `--mcp-config <runner-app-data file>` so a
# hand-typed `claude` in ANY cwd (or a fresh install whose cwd has no
# `.mcp.json`) still gets coord-mcp out of the box. The runner injects
# QONTINUI_MCP_CONFIG with the absolute path (a DEVICE loopback-proxy config);
# empty/unset => append nothing (fail-open — the session simply has no
# coord-mcp, never a broken/FAILED server). NOTE: `--mcp-config` is VARIADIC
# (`<configs...>`), so it MUST be placed so a `--`-flag follows it in the exec
# lines below (before `--session-id`) — otherwise it would swallow a trailing
# positional. Same fail-open/recursion-guard/no-clobber contract as --settings.
MCP_CONFIG_ARGS=()
if [ "$TOOL" = "claude" ] && [ -n "${QONTINUI_MCP_CONFIG:-}" ] \
    && [ -f "${QONTINUI_MCP_CONFIG}" ]; then
  MCP_CONFIG_ARGS=(--mcp-config "${QONTINUI_MCP_CONFIG}")
fi

# Does the user's argv already choose a session? Then don't double-pin.
user_chose_session=0
for a in "$@"; do
  case "$a" in
    --session-id|--session-id=*|--resume|--resume=*|-r|resume|--continue|-c)
      user_chose_session=1; break ;;
  esac
done

# ---------------------------------------------------------------------------
# Spawn-time policy delivery marker (plan
# 2026-09-15-runner-policy-injection-off-sessionstart-hook-channel).
# QONTINUI_POLICY_DELIVERED_SHA tells the runner's policy hook that THIS
# `claude` received the policy body in its system prompt, so the hook may skip
# re-sending it; QONTINUI_POLICY_DELIVERED_FILE names the composed file that
# body rode in. Both are inherited by every descendant, but only a `claude`
# whose argv passes THAT file to --append-system-prompt-file (either spelling)
# actually received the body — a nested `claude --append-system-prompt-file
# ./eval.md` did not. This shim is what delivers the hook (--settings) to a
# nested `claude` typed inside a session, so it is where an inherited marker
# must be dropped — otherwise that nested session would be told it has a body
# it was never given. A REPLACEMENT prompt (--system-prompt[-file], ahead of
# `--`) drops it too: whether Claude Code still applies the append file beside
# one is not behaviourally verified, so the rule every path shares withholds it.
if [ "$TOOL" = "claude" ]; then
  keep_policy_sha=0
  replacement_prompt=0
  for a in "$@"; do
    case "$a" in
      --) break ;;
      --system-prompt|--system-prompt=*|--system-prompt-file|--system-prompt-file=*)
        replacement_prompt=1; break ;;
    esac
  done
  delivered_file="${QONTINUI_POLICY_DELIVERED_FILE:-}"
  if [ -n "$delivered_file" ] && [ "$replacement_prompt" = "0" ]; then
    prev_arg=""
    for a in "$@"; do
      if [ "$prev_arg" = "--append-system-prompt-file" ] && [ "$a" = "$delivered_file" ]; then
        keep_policy_sha=1; break
      fi
      case "$a" in
        --append-system-prompt-file=*)
          if [ "${a#--append-system-prompt-file=}" = "$delivered_file" ]; then
            keep_policy_sha=1; break
          fi ;;
      esac
      prev_arg="$a"
    done
  fi
  if [ "$keep_policy_sha" != "1" ]; then
    unset QONTINUI_POLICY_DELIVERED_SHA QONTINUI_POLICY_DELIVERED_FILE
  fi
fi

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

# Fire-and-forget DIAGNOSTIC beacon on EVERY real shim invocation (even the
# don't-double-pin passthrough that sends no session-open). Log-only on the
# runner side — makes "did the shim run for this terminal, and will it deliver
# --session-id/--settings?" observable in the logs. Never blocks the exec.
notify_shim_beacon() {
  local port="${QONTINUI_INSTALL_INTERCEPT_PORT:-}"
  [ -z "$port" ] && return 0
  command -v curl >/dev/null 2>&1 || return 0
  local settings="false"
  [ "${#SETTINGS_ARGS[@]}" -gt 0 ] && settings="true"
  local mcp_config="false"
  [ "${#MCP_CONFIG_ARGS[@]}" -gt 0 ] && mcp_config="true"
  local pinned="false"; [ -n "$PINNED" ] && pinned="true"
  local user_chose="false"; [ "$user_chose_session" -eq 1 ] && user_chose="true"
  local detail="user_session_id=$user_chose settings=$settings mcp_config=$mcp_config pinned=$pinned"
  local body
  body="{\"terminal_id\":\"${QONTINUI_TERMINAL_ID:-}\",\"tool\":\"$TOOL\",\"event\":\"invoked\",\"detail\":\"$detail\"}"
  curl -fsS --connect-timeout 3 --max-time 10 \
      -X POST "http://127.0.0.1:$port/control/shim-beacon" \
      -H 'Content-Type: application/json' \
      -d "$body" >/dev/null 2>&1 || true
}
notify_shim_beacon

if [ "$user_chose_session" -eq 1 ] || [ -z "$PINNED" ]; then
  # User chose their own session (or we have no pinned id) — don't append our
  # `--session-id`, but STILL deliver the `--settings` hook (claude) so a
  # `--resume`/`--continue` confirms via the SessionStart hook. Also send a
  # best-effort confirmation when WE know the pinned id and the user did not
  # override it (so a bare `claude` with our pin still confirms).
  if [ "$user_chose_session" -eq 0 ] && [ -n "$PINNED" ]; then
    notify_session_open "$PINNED" "startup"
  fi
  exec_real "$@" "${SETTINGS_ARGS[@]}" "${MCP_CONFIG_ARGS[@]}"
fi

# Append our pinned id (+ the claude --settings hook) and run the real provider.
# Confirmation POST first (fire-and-forget); then exec so stdio/exit are
# transparent.
notify_session_open "$PINNED" "startup"
exec_real "$@" "${SETTINGS_ARGS[@]}" "${MCP_CONFIG_ARGS[@]}" --session-id "$PINNED"
