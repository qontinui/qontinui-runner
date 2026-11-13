#!/usr/bin/env bash
# qontinui session-restore READ-BACK identity shim (bash / Git-Bash / Unix).
#
# Wraps a CLI whose profile reads its session id back (Codex): the CLI mints
# its own id, so there is nothing to pin and no flag to append. This wrapper
# never changes the user's argv. Its one job is to SIGNAL the runner that a
# session is starting in this terminal — POST /control/session-open with NO
# session_id — so the runner's read-back capture can find the session's
# rollout file (`session::provider_adapter::CodexAdapter` →
# `session::codex_capture`). Ported from qontinui-runner PR #651.
#
# The pinning counterpart is identity_shim.bash. The materializer picks the
# template from the tool's CLI profile (`shim_materializer::identity_bash_template`).
# Materialized from this TEMPLATE; the runner substitutes the `@@…@@`
# placeholders. Do NOT run it raw.
#
# Hard invariants (mirrored from identity_shim.bash):
#   * FAIL-OPEN: any failure -> exec the REAL CLI unchanged.
#   * TRANSPARENT: stdio is inherited (exec); ARGS PASS THROUGH UNCHANGED.
#   * RECURSION-GUARD: QONTINUI_INSTALL_INTERCEPT_GUARD=1 -> pure passthrough.
#   * NEVER set or relocate CODEX_HOME — the user's login lives there. Its
#     value, when the user set one, is only REPORTED (as config_dir) so the
#     runner scans the home this session actually writes to.
#
# Placeholders:
#   @@TOOL@@      the wrapped program name
#   @@SHIM_DIR@@  absolute path of this shim's own bin dir (skipped in the
#                 real-tool PATH scan to avoid recursion)
set -u

TOOL="@@TOOL@@"
SHIM_DIR="@@SHIM_DIR@@"

# ---------------------------------------------------------------------------
# is_qontinui_shim_dir <dir>: true for ANY qontinui shim dir, not only ours.
# A PATH inherited from another runner session (a supervisor started inside a
# session, then a temp runner spawned from it) carries THAT session's shim dir
# too; resolving its shim as the "real" tool made two shims exec each other for
# ever, keeping the pid (plan 2026-11-04-identity-shim-foreign-shim-ping-pong-
# and-secondary-runner-pty-socket-overflow, Phase 1). The same predicate as
# intercept_core::shim_dir::is_qontinui_shim_dir: the dir's final name starts
# with qontinui-identity- or qontinui-shim-, or it holds the identity-build
# marker. Trailing separators are ignored, and the name splits on `/` and `\`
# alike, so the MSYS and Windows spellings of one dir (our own included) read
# the same. The name match is ASCII-case-insensitive like the Rust and .cmd
# copies (a case-changed Windows `Path` entry under Git Bash), spelled as
# bracket classes because `${x,,}` needs bash 4 and macOS ships 3.2.
# Builtins only (no fork per PATH entry).
# ---------------------------------------------------------------------------
is_qontinui_shim_dir() {
  local d="$1" name
  while :; do
    case "$d" in
      */|*\\) d="${d%?}" ;;
      *) break ;;
    esac
  done
  [ -n "$d" ] || return 1
  name="${d##*/}"
  name="${name##*\\}"
  case "$name" in
    [qQ][oO][nN][tT][iI][nN][uU][iI]-[iI][dD][eE][nN][tT][iI][tT][yY]-*) return 0 ;;
    [qQ][oO][nN][tT][iI][nN][uU][iI]-[sS][hH][iI][mM]-*) return 0 ;;
  esac
  [ -e "$d/.qontinui-identity-build" ]
}

# Resolve the REAL CLI: first match on PATH that is NOT inside SHIM_DIR or any
# other qontinui shim dir.
resolve_real() {
  local IFS=':'
  local dir cand
  for dir in $PATH; do
    [ -z "$dir" ] && continue
    case "${dir%/}" in
      "${SHIM_DIR%/}") continue ;;
    esac
    is_qontinui_shim_dir "$dir" && continue
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

# Run the real CLI, replacing this process. Strips our dir, and every other
# qontinui shim dir, from PATH as a last resort when the real tool couldn't be resolved.
exec_real() {
  if [ -n "${REAL:-}" ]; then
    QONTINUI_INSTALL_INTERCEPT_GUARD=1 exec "$REAL" "$@"
  fi
  local newpath="" d
  local IFS=':'
  for d in $PATH; do
    case "${d%/}" in "${SHIM_DIR%/}") continue ;; esac
    is_qontinui_shim_dir "$d" && continue
    if [ -z "$newpath" ]; then newpath="$d"; else newpath="$newpath:$d"; fi
  done
  QONTINUI_INSTALL_INTERCEPT_GUARD=1 PATH="$newpath" exec "$TOOL" "$@"
}

# Recursion guard: a nested invocation is a pure passthrough — never re-signal.
if [ "${QONTINUI_INSTALL_INTERCEPT_GUARD:-}" = "1" ]; then
  exec_real "$@"
fi

# A string as JSON string content: backslash and quote escaped, newline, CR
# and tab as their escapes, and every other control character dropped (none
# belongs in a path). Unescaped, a cwd holding one made the body invalid JSON
# and the signal was silently lost.
json_escape() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\r'/\\r}
  s=${s//$'\t'/\\t}
  printf '%s' "$s" | tr -d '\001-\010\013\014\016-\037\177'
}

# Best-effort start signal. Never load-bearing, and never in the way: it runs
# in the BACKGROUND with a 2 s ceiling, so the CLI starts at once whatever the
# runner's port does; every failure is ignored. The cwd is reported in the
# frame the CLI records it in: under Git-Bash `$PWD` is the mingw `/c/...`
# form while a Windows binary writes `C:\...`, so prefer `pwd -W` (MSYS
# Windows form) and fall back to `$PWD` on a real Unix shell. The runner also
# folds the mingw form (#651, `6b38190c6`).
notify_session_start() {
  local port="${QONTINUI_INSTALL_INTERCEPT_PORT:-}"
  [ -z "$port" ] && return 0
  command -v curl >/dev/null 2>&1 || return 0
  local cwd_raw
  cwd_raw="$(pwd -W 2>/dev/null || printf '%s' "$PWD")"
  local body
  # `codex resume [<id>]` continues an EXISTING rollout: say so, with the
  # argv id when one is given, so the runner does not wait for a new one.
  local source="startup" resume=""
  if [ "${1:-}" = "resume" ]; then
    source="resume"
    case "${2:-}" in
      ""|-*) ;;
      *) resume=",\"resume_id\":\"$(json_escape "$2")\"" ;;
    esac
  fi
  body="{\"terminal_id\":\"${QONTINUI_TERMINAL_ID:-}\",\"provider\":\"$TOOL\",\"source\":\"$source\",\"cwd\":\"$(json_escape "$cwd_raw")\"$resume"
  if [ -n "${CODEX_HOME:-}" ]; then
    body="$body,\"config_dir\":\"$(json_escape "$CODEX_HOME")\""
  fi
  body="$body}"
  curl -fsS --connect-timeout 1 --max-time 2 \
      -X POST "http://127.0.0.1:$port/control/session-open" \
      -H 'Content-Type: application/json' \
      -d "$body" >/dev/null 2>&1 &
}

# Only a launch that can start a session signals. A management subcommand
# (codex-cli 0.159.1 `--help`) writes no rollout, and signalling for it would
# leave a capture watching this cwd for a rollout some OTHER session might
# write. `exec`, `review`, `resume` and `fork` do run sessions, and signal.
case "${1:-}" in
  login|logout|mcp|plugin|app-server|remote-control|completion|update|doctor| \
  sandbox|debug|apply|a|queue|archive|delete|migrate-rollouts|unarchive| \
  cloud|exec-server|features|agents|help|-h|--help|-V|--version) ;;
  *) notify_session_start "$@" ;;
esac
exec_real "$@"
