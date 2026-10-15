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
#   * BOUNDED RESOLVE WAIT: a real tool that is momentarily ABSENT (mid-
#     reinstall) is re-scanned for at most QONTINUI_SHIM_RESOLVE_WAIT_SECS
#     (default 10) before the fail-open fall-through; a resolved tool never
#     waits, and the only output added is on STDERR, on the miss path alone.
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

# ---------------------------------------------------------------------------
# Bounded wait on a MISS. A package manager reinstalling the tool (Claude Code's
# auto-updater runs `npm install --global` of the SAME version, and npm's reify
# RENAMES `bin/<tool>` and the package dir aside before re-linking) leaves a
# window of a second or two in which the tool is on no PATH entry at all. One
# scan inside that window used to fall straight through to `exec "$TOOL"`,
# which failed `not found` 73 ms after spawn and silently lost a runner-spawned
# gate continuation (2026-10-01, merytshost). So a miss re-scans with short
# sleeps for up to QONTINUI_SHIM_RESOLVE_WAIT_SECS (default 10; 0 disables)
# before giving up. A HIT costs nothing extra — the wait is only ever paid by
# an invocation that would otherwise have failed. One stderr line announces the
# wait so a genuinely absent tool is not a silent 10 s stall.
# ---------------------------------------------------------------------------
resolve_wait_secs() {
  local w="${QONTINUI_SHIM_RESOLVE_WAIT_SECS:-10}"
  case "$w" in ''|*[!0-9]*) w=10 ;; esac
  # Base 10 explicitly: `08` would otherwise be an invalid OCTAL literal in
  # the $(( )) below, and `010` would mean 8.
  printf '%s' "$((10#$w))"
}

resolve_real_waiting() {
  local found
  if found="$(resolve_real)"; then printf '%s' "$found"; return 0; fi
  local wait_secs; wait_secs="$(resolve_wait_secs)"
  [ "$wait_secs" -gt 0 ] || return 1
  printf 'qontinui shim: %s not found on PATH (excluding %s); waiting up to %ss for it to reappear (a package-manager reinstall briefly removes it)\n' \
    "$TOOL" "$SHIM_DIR" "$wait_secs" >&2
  local deadline=$((SECONDS + wait_secs))
  while [ "$SECONDS" -lt "$deadline" ]; do
    sleep 0.25 2>/dev/null || sleep 1
    if found="$(resolve_real)"; then printf '%s' "$found"; return 0; fi
  done
  return 1
}

# The last-resort fall-through is about to dispatch by NAME — say plainly what
# was searched, so a `not found` that follows is diagnosable from the pane.
report_unresolved() {
  printf 'qontinui shim: the real %s was not found on PATH after waiting %ss; falling back to PATH dispatch. PATH searched (excluding %s): %s\n' \
    "$TOOL" "$(resolve_wait_secs)" "$SHIM_DIR" "$PATH" >&2
}

REAL="$(resolve_real_waiting || true)"

# ---------------------------------------------------------------------------
# Bounded retry on a FAILED exec of the resolved tool. Claude Code's
# auto-updater rewrites `claude.exe` in place; an exec that lands while the
# updater still holds the file open for writing fails ETXTBSY ("Text file
# busy"), and a non-interactive bash exits on a failed exec -- so the terminal
# dropped to a bare prompt with the session lost (2026-10-05, merytshost,
# `/spawn-ai 2`). The file IS present, so resolve_real_waiting above never
# fires. `execfail` makes a failed exec return instead of exiting; we retry for
# at most QONTINUI_SHIM_RESOLVE_WAIT_SECS (default 10; 0 disables -> the old
# exit-on-failure behaviour), then exit with exec's own status. A SUCCESSFUL
# exec replaces this process, so a healthy launch pays nothing.
# ---------------------------------------------------------------------------
exec_with_retry() {
  shopt -s execfail 2>/dev/null || true
  local wait_secs deadline rc announced=0
  wait_secs="$(resolve_wait_secs)"
  deadline=$((SECONDS + wait_secs))
  while :; do
    "$@"
    rc=$?
    [ "$SECONDS" -lt "$deadline" ] || break
    if [ "$announced" -eq 0 ]; then
      printf 'qontinui shim: exec of %s failed (status %s; likely a reinstall holding it open); retrying for up to %ss\n' \
        "$REAL" "$rc" "$wait_secs" >&2
      announced=1
    fi
    sleep 0.5 2>/dev/null || sleep 1
  done
  exit "$rc"
}

# Run the real provider, replacing this process (we don't need an exit code
# beyond what exec propagates). Strips our dir from PATH as a last resort when
# the real tool couldn't be resolved.
exec_real() {
  if [ -n "${REAL:-}" ]; then
    QONTINUI_INSTALL_INTERCEPT_GUARD=1 exec_with_retry exec "$REAL" "$@"
  fi
  report_unresolved
  local newpath="" d
  local IFS=':'
  for d in $PATH; do
    case "${d%/}" in "${SHIM_DIR%/}") continue ;; esac
    if [ -z "$newpath" ]; then newpath="$d"; else newpath="$newpath:$d"; fi
  done
  # Whatever answers the name-dispatch below must not wait again: if it is
  # another shim of ours, the wait was already paid here.
  export QONTINUI_SHIM_RESOLVE_WAIT_SECS=0
  QONTINUI_INSTALL_INTERCEPT_GUARD=1 PATH="$newpath" exec "$TOOL" "$@"
}

# Recursion guard: a nested invocation is a pure passthrough — never re-pin.
# It also must not inherit its PARENT's launch argv (below): its own
# SessionStart hook would otherwise report the parent's flags under the nested
# session's id.
if [ "${QONTINUI_INSTALL_INTERCEPT_GUARD:-}" = "1" ]; then
  unset QONTINUI_LAUNCH_ARGV
  exec_real "$@"
fi

PINNED="${QONTINUI_PINNED_SESSION_ID:-}"

# ---------------------------------------------------------------------------
# Launch flags for a tier-2 relaunch (plan
# 2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions, Phase 6 /
# D11). For TOOL=claude, export in QONTINUI_LAUNCH_ARGV — as a JSON array —
# ONLY the replay-allowlisted flags this launch received, with their values:
# never the initial prompt, never any other flag, never another flag's value
# (review r2, N1). Every child of claude inherits this variable (its Bash-tool
# shells, hooks, MCP servers), so an `env` in a tool would print it; an inline
# `--settings` / `--mcp-config` JSON with a credential must never be in it.
# The runner-bundled SessionStart hook sends it in the body of its LOOPBACK
# POST /control/session-open (never over the PTY), and the runner re-derives
# and persists only the relaunch flags from it — it stays the authority
# (`session::resume_argv`), and refuses what it must.
#
# The allowlist below is a COPY of the runner's (`resume_argv::
# REPLAY_ALLOWLIST_*`), pinned equal to it by the runner's test
# `the_shim_allowlists_match_the_runners`. Two markers carry what the runner
# must refuse without carrying the token itself: print mode exports
# `--print`, and a combined short flag (`-pc`, `-rID`) exports `-??`.
#
# Every value is JSON-escaped into printable ASCII: control characters AND
# every non-ASCII character become `\uXXXX` (UTF-16 surrogate pairs above
# U+FFFF; a byte that is not valid UTF-8 becomes U+FFFD), so C1 controls such
# as U+009C cannot survive. Fail-open: past 4 KiB the encoding stops at once
# and nothing is exported; a relaunch then uses the default resume command.
QS_REPLAY_ONE=" --model --fallback-model --permission-mode "
QS_REPLAY_MANY=" --add-dir "
QS_REPLAY_OPTIONAL=" --debug "
QS_REPLAY_BOOL=" --verbose --ide --chrome --no-chrome "
QS_ARGV_MAX=4096

# Append `$1`, JSON-escaped, to QS_OUT (with a comma when it is not the first
# element). Returns 1 — leaving QS_OUT unusable — as soon as QS_OUT passes
# QS_ARGV_MAX bytes, so a huge value is never escaped in full.
qs_json_push() {
  # C locale: every comparison below is on BYTES (`'$c` yields a byte value,
  # `${#s}` a byte count), whatever the operator's locale.
  local LC_ALL=C
  local s="$1" i=0 n b c cp need k u
  [ "$QS_OUT" = "[" ] || QS_OUT+=","
  QS_OUT+='"'
  # Fast path: printable ASCII with no quote or backslash needs no escaping.
  if [[ $s != *[^\ -~]* && $s != *[\"\\]* ]]; then
    QS_OUT+="$s\""
    [ "${#QS_OUT}" -le "$QS_ARGV_MAX" ]
    return
  fi
  n=${#s}
  while [ "$i" -lt "$n" ]; do
    [ "${#QS_OUT}" -le "$QS_ARGV_MAX" ] || return 1
    c="${s:i:1}"
    printf -v b '%d' "'$c"
    (( b < 0 )) && b=$((b + 256))
    i=$((i + 1))
    if (( b == 34 )); then
      QS_OUT+='\"'
    elif (( b == 92 )); then
      QS_OUT+='\\'
    elif (( b >= 32 && b < 127 )); then
      QS_OUT+="$c"
    elif (( b < 128 )); then
      printf -v u '\\u%04x' "$b"
      QS_OUT+="$u"
    else
      if (( b >= 0xC2 && b <= 0xDF )); then
        need=1; cp=$((b & 0x1F))
      elif (( b >= 0xE0 && b <= 0xEF )); then
        need=2; cp=$((b & 0x0F))
      elif (( b >= 0xF0 && b <= 0xF4 )); then
        need=3; cp=$((b & 0x07))
      else
        need=0; cp=$((0xFFFD))
      fi
      k=0
      while (( k < need )); do
        if (( i >= n )); then cp=$((0xFFFD)); break; fi
        c="${s:i:1}"
        printf -v b '%d' "'$c"
        (( b < 0 )) && b=$((b + 256))
        if (( b < 0x80 || b > 0xBF )); then cp=$((0xFFFD)); break; fi
        cp=$(( (cp << 6) | (b & 0x3F) ))
        i=$((i + 1))
        k=$((k + 1))
      done
      if (( cp >= 0x10000 )); then
        cp=$((cp - 0x10000))
        printf -v u '\\u%04x\\u%04x' $((0xD800 + (cp >> 10))) $((0xDC00 + (cp & 0x3FF)))
      else
        printf -v u '\\u%04x' "$cp"
      fi
      QS_OUT+="$u"
    fi
  done
  QS_OUT+='"'
  [ "${#QS_OUT}" -le "$QS_ARGV_MAX" ]
}

# Build QS_OUT: the allowlisted flags of "$@" with their values, as a JSON
# array. Returns 1 when it would pass QS_ARGV_MAX bytes.
qs_launch_flags_json() {
  local args=("$@") n=$# i=0 a name next
  QS_OUT="["
  while (( i < n )); do
    a="${args[i]}"
    i=$((i + 1))
    [ "$a" = "--" ] && break
    # A positional — the initial prompt, or a dropped flag's value: never.
    [[ $a == -?* ]] || continue
    if [[ $a != --* && ${#a} -gt 2 ]]; then
      qs_json_push "-??" || return 1
      continue
    fi
    case "$a" in
      -p | --print) qs_json_push "--print" || return 1; continue ;;
    esac
    name="${a%%=*}"
    if [[ $QS_REPLAY_BOOL == *" $name "* ]]; then
      qs_json_push "$a" || return 1
      continue
    fi
    if [[ $QS_REPLAY_ONE != *" $name "* && $QS_REPLAY_MANY != *" $name "* \
        && $QS_REPLAY_OPTIONAL != *" $name "* ]]; then
      continue # not on the allowlist: dropped, and its value with it
    fi
    qs_json_push "$a" || return 1
    [[ $a == --*=* ]] && continue
    if [[ $QS_REPLAY_ONE == *" $name "* ]]; then
      # A required value is taken greedily, as claude takes it.
      if (( i < n )); then
        qs_json_push "${args[i]}" || return 1
        i=$((i + 1))
      fi
    elif [[ $QS_REPLAY_OPTIONAL == *" $name "* ]]; then
      if (( i < n )) && [[ ${args[i]} != -?* ]]; then
        qs_json_push "${args[i]}" || return 1
        i=$((i + 1))
      fi
    else
      while (( i < n )); do
        next="${args[i]}"
        [[ $next == -?* ]] && break
        qs_json_push "$next" || return 1
        i=$((i + 1))
      done
    fi
  done
  QS_OUT+="]"
  [ "${#QS_OUT}" -le "$QS_ARGV_MAX" ]
}

unset QONTINUI_LAUNCH_ARGV
if [ "$TOOL" = "claude" ] && qs_launch_flags_json "$@"; then
  export QONTINUI_LAUNCH_ARGV="$QS_OUT"
fi
unset QS_OUT

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
