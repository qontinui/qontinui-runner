#!/usr/bin/env bash
# qontinui install-interception shim (bash / Git-Bash / Unix).
#
# This file is a TEMPLATE materialized per-terminal by the runner
# (`install_effects_producer::intercept::shim_materializer`). The runner
# substitutes the `@@…@@` placeholders at write time; do NOT run it raw.
#
# Hard invariants (plan §6):
#   * FAIL-OPEN: any runner-contact failure (no port env, connect refused,
#     timeout, non-2xx) -> exec the REAL tool unchanged. The agent's shell
#     must NEVER be bricked by interception being unavailable.
#   * TRANSPARENT: stdio is inherited (exec), so the real tool sees its TTY
#     and the agent sees byte-identical output + exit code.
#   * ZERO-OVERHEAD non-install: non-install verbs exec the real tool
#     immediately with no runner round-trip.
#   * RECURSION-GUARD: if QONTINUI_INSTALL_INTERCEPT_GUARD=1 is already set,
#     this is a nested invocation -> pure passthrough.
#   * BOUNDED RESOLVE WAIT: a real tool that is momentarily ABSENT (mid-
#     reinstall) is re-scanned for at most QONTINUI_SHIM_RESOLVE_WAIT_SECS
#     (default 3) before the fail-open fall-through; a resolved tool never
#     waits, and the only output added is on STDERR, on the miss path alone.
#
# Gate mode (Phase 3, plan §3 A3/A4/A6 + §4 Phase 3):
#   When QONTINUI_INSTALL_INTERCEPT_MODE=gate AND the pre-call returns
#   "gate":"escalate" AND QONTINUI_INSTALL_OVERRIDE != 1 AND this is NOT a
#   lockfile-sync (A6) AND this tool is NOT never-gate (npx) -> print the A4 UX
#   to STDERR and `exit 1` WITHOUT running the real tool and WITHOUT a post-call
#   (no install happened; the producer PreContext TTLs out — the designed
#   abandonment path). The block predicate is the shell transliteration of
#   `install_effects_producer::intercept::gate::should_block` (the canonical
#   truth table). Observe mode NEVER blocks; a garbled MODE fails open to
#   observe (anything != "gate" is observe).
#   Override: QONTINUI_INSTALL_OVERRIDE=1 -> the shim sets
#   "override_escalation":true on the pre-call JSON and runs the real tool +
#   post-call normally (the producer records +overridden provenance).
#
# Placeholders:
#   @@TOOL@@           the shadowed program name (npm/npx/pnpm/yarn/cargo/pip/pip3)
#   @@PM_WIRE_NAME@@   the wire package_manager the pre-call sends (npx->npm,
#                      pip3->pip) since the producer only accepts the coord enum
#   @@SHIM_DIR@@       absolute path of this shim's own bin dir (skipped in
#                      the real-tool PATH scan to avoid recursion)
#   @@INSTALL_VERBS@@  space-separated install-shaped verbs (verb-table PMs)
#   @@LOCKSYNC_VERBS@@ verbs that are lockfile-sync BY NAME (npm ci, cargo
#                      update) — never gated even with args (A6)
#   @@NEVER_GATE@@     "1" if this tool is never gate-blocked (npx) else "0" —
#                      a LOCAL tool property the gate branch checks (§3 step 6)
set -u

TOOL="@@TOOL@@"
PM_WIRE_NAME="@@PM_WIRE_NAME@@"
SHIM_DIR="@@SHIM_DIR@@"
INSTALL_VERBS="@@INSTALL_VERBS@@"
LOCKSYNC_VERBS="@@LOCKSYNC_VERBS@@"
NEVER_GATE="@@NEVER_GATE@@"

# ---------------------------------------------------------------------------
# Resolve the REAL tool: first match on PATH that is NOT inside SHIM_DIR.
# Mirrors pm_detect::pm_command intent (try <tool> then <tool>.cmd/.exe).
# ---------------------------------------------------------------------------
resolve_real() {
  local IFS=':'
  local dir cand
  for dir in $PATH; do
    [ -z "$dir" ] && continue
    # Skip our own shim dir (compare resolved-ish: trailing-slash tolerant).
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
# sleeps for up to QONTINUI_SHIM_RESOLVE_WAIT_SECS (default in
# resolve_wait_secs; 0 disables) before giving up. A HIT costs nothing extra —
# the wait is only ever paid by an invocation that would otherwise have failed.
# One stderr line announces the wait so a genuinely absent tool is not a silent
# stall. Same mechanism as identity_shim.bash.
# ---------------------------------------------------------------------------
resolve_wait_secs() {
  # 3 s here, not the identity shim's 10: an install-family tool that is simply
  # not installed (`pip3` on a box with only `pip`) is a common PROBE, and every
  # probe of an absent tool pays the whole wait. npm's reify window is ~2 s.
  local w="${QONTINUI_SHIM_RESOLVE_WAIT_SECS:-3}"
  case "$w" in ''|*[!0-9]*) w=3 ;; esac
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

# Fail-open: if we cannot even find the real tool, hand control back to the
# shell's own PATH resolution (without our dir) as a last resort.
passthrough() {
  if [ -n "${REAL:-}" ]; then
    exec "$REAL" "$@"
  fi
  # No resolved real tool: strip our dir from PATH and re-dispatch by name so
  # the shell finds whatever it would have without us.
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
  PATH="$newpath" exec "$TOOL" "$@"
}

# Recursion guard: a nested shim invocation is a pure passthrough.
if [ "${QONTINUI_INSTALL_INTERCEPT_GUARD:-}" = "1" ]; then
  passthrough "$@"
fi

# ---------------------------------------------------------------------------
# Classify argv. The parse differs by tool family (mirrors classify_tool in
# install_effects_producer::intercept::classify):
#   * npm / pnpm / yarn / cargo  -> verb-table parse (INSTALL_VERBS).
#   * npx                        -> execution-shaped (first non-flag token is
#                                   the package; -p/--package adds; -y ignored).
#   * pip / pip3                 -> `install` verb; requirement specifiers split
#                                   on the comparator; -r => lockfile-sync;
#                                   -e <path> editable-local => passthrough.
# Outputs: is_install (0/1), dev (true/false), lockfile_only (true/false), and
# the pkgs[] array.
# ---------------------------------------------------------------------------
is_install=0
dev=false
lockfile_only=false
declare -a pkgs=()

# Split a single token NAME[@VER] (npm/cargo/npx) -> name + vreq globals.
split_at_spec() {
  local p="$1"; name="$p"; vreq=""
  local base="${p#@}"
  if [ "$base" != "$p" ]; then
    # scoped: @scope/name[@ver]
    local rest="${base#*/}" scope="@${base%%/*}"
    case "$rest" in
      *@*) name="$scope/${rest%@*}"; vreq="${rest##*@}" ;;
      *)   name="$scope/$rest" ;;
    esac
  else
    case "$p" in
      *@*) name="${p%@*}"; vreq="${p##*@}" ;;
    esac
  fi
}

# Split a pip requirement NAME<op>VER -> name + vreq globals (op kept in vreq).
split_pip_spec() {
  local p="$1"; name="$p"; vreq=""
  # Find the earliest PEP-508 comparator. Order matters (=== before ==).
  local op rest
  for op in '===' '==' '>=' '<=' '~=' '!=' '>' '<'; do
    case "$p" in
      *"$op"*)
        rest="${p%%"$op"*}"
        if [ -n "$rest" ] && [ "$rest" != "$p" ]; then
          # Only accept if this op gives the SHORTEST name prefix so far.
          if [ -z "$vreq" ] || [ "${#rest}" -lt "${#name}" ]; then
            name="$rest"; vreq="${p#"$rest"}"
          fi
        fi
        ;;
    esac
  done
}

classify_verb_table() {
  # First non-flag token is the verb.
  local verb="" a
  for a in "$@"; do
    case "$a" in -*) continue ;; *) verb="$a"; break ;; esac
  done
  local v
  for v in $INSTALL_VERBS; do
    if [ "$verb" = "$v" ]; then is_install=1; break; fi
  done
  [ "$is_install" -ne 1 ] && return 0

  # Verb is lockfile-sync by name?
  for v in $LOCKSYNC_VERBS; do
    if [ "$verb" = "$v" ]; then lockfile_only=true; fi
  done

  local seen_verb=0
  for a in "$@"; do
    if [ "$seen_verb" -eq 0 ]; then
      if [ "$a" = "$verb" ]; then seen_verb=1; fi
      continue
    fi
    case "$a" in
      --save-dev|-D|--dev|--save-development|--build) dev=true ;;
      -*) : ;;  # ignore any other flag (and we do not consume its value)
      *) split_at_spec "$a"; pkgs+=("${name}${vreq:+@@VSEP@@$vreq}") ;;
    esac
  done
  [ "${#pkgs[@]}" -eq 0 ] && lockfile_only=true
}

classify_npx() {
  # Help/version probe with no package -> passthrough.
  local a has_probe=0 first_pkg="" want_value=0
  for a in "$@"; do
    case "$a" in
      --help|-h|--version|-v|--no-install) has_probe=1 ;;
    esac
  done
  # Collect -p/--package values, and the first bare positional.
  local i=0
  set -- "$@"
  for a in "$@"; do
    if [ "$want_value" -eq 1 ]; then
      want_value=0
      split_at_spec "$a"; pkgs+=("${name}${vreq:+@@VSEP@@$vreq}")
      continue
    fi
    case "$a" in
      -p|--package) want_value=1 ;;
      -c|--call) want_value=1 ;;  # value is a command, skip it
      -*) : ;;
      *) [ -z "$first_pkg" ] && first_pkg="$a" ;;
    esac
  done
  if [ "${#pkgs[@]}" -eq 0 ] && [ -n "$first_pkg" ]; then
    split_at_spec "$first_pkg"; pkgs+=("${name}${vreq:+@@VSEP@@$vreq}")
  fi
  if [ "${#pkgs[@]}" -eq 0 ]; then
    is_install=0; return 0
  fi
  if [ "$has_probe" -eq 1 ] && [ "${#pkgs[@]}" -eq 0 ]; then
    is_install=0; return 0
  fi
  is_install=1
  # npx is observe-only / never-gated: flag as lockfile-only so the UX never
  # blocks (the producer gate is bypassed for npx server-side too).
  lockfile_only=true
}

classify_pip() {
  local verb="" a
  for a in "$@"; do
    case "$a" in -*) continue ;; *) verb="$a"; break ;; esac
  done
  [ "$verb" != "install" ] && return 0

  local seen_verb=0 skip_next=0 req_file=0
  for a in "$@"; do
    if [ "$skip_next" -eq 1 ]; then skip_next=0; continue; fi
    if [ "$seen_verb" -eq 0 ]; then
      if [ "$a" = "$verb" ]; then seen_verb=1; fi
      continue
    fi
    case "$a" in
      -r|--requirement) req_file=1; skip_next=1 ;;
      -e|--editable) skip_next=1 ;;  # editable local target -> drop, no declare
      -c|--constraint|-i|--index-url|--extra-index-url|--find-links|-f|--target|-t|--prefix|--root|--python-version|--platform|--abi|--implementation)
        skip_next=1 ;;
      -*) : ;;  # bare flag (-U/--upgrade/--user/--no-deps/...) ignored
      *) split_pip_spec "$a"; pkgs+=("${name}${vreq:+@@VSEP@@$vreq}") ;;
    esac
  done

  if [ "$req_file" -eq 1 ] && [ "${#pkgs[@]}" -eq 0 ]; then
    is_install=1; lockfile_only=true; return 0
  fi
  if [ "${#pkgs[@]}" -eq 0 ] && [ "$req_file" -eq 0 ]; then
    # editable-only or nothing -> NOT a registry install -> passthrough.
    is_install=0; return 0
  fi
  is_install=1
  [ "${#pkgs[@]}" -eq 0 ] && lockfile_only=true
}

case "$TOOL" in
  npx)        classify_npx "$@" ;;
  pip|pip3)   classify_pip "$@" ;;
  *)          classify_verb_table "$@" ;;
esac

# Non-install verb (run/build/test/ls/…) or no verb -> zero-overhead passthrough.
if [ "$is_install" -ne 1 ]; then
  QONTINUI_INSTALL_INTERCEPT_GUARD=1 passthrough "$@"
fi

# ---------------------------------------------------------------------------
# Build the packages JSON array from pkgs[] (each entry is name or
# name@@VSEP@@vreq — the @@VSEP@@ sentinel avoids clobbering a literal '@').
# ---------------------------------------------------------------------------
pkgs_json="["
first=1
for entry in "${pkgs[@]:-}"; do
  [ -z "$entry" ] && continue
  case "$entry" in
    *@@VSEP@@*) pname="${entry%%@@VSEP@@*}"; pvreq="${entry#*@@VSEP@@}" ;;
    *)          pname="$entry"; pvreq="" ;;
  esac
  esc_name=$(printf '%s' "$pname" | sed 's/\\/\\\\/g; s/"/\\"/g')
  if [ "$first" -eq 1 ]; then first=0; else pkgs_json="$pkgs_json,"; fi
  if [ -n "$pvreq" ]; then
    esc_vreq=$(printf '%s' "$pvreq" | sed 's/\\/\\\\/g; s/"/\\"/g')
    pkgs_json="$pkgs_json{\"name\":\"$esc_name\",\"version_req\":\"$esc_vreq\"}"
  else
    pkgs_json="$pkgs_json{\"name\":\"$esc_name\"}"
  fi
done
pkgs_json="$pkgs_json]"

# ---------------------------------------------------------------------------
# Pre-call: POST /install-effects/run {mode:"intercept"}. Best-effort.
# A failure here is NON-FATAL -> we still run the real tool (fail-open).
# OBSERVE mode: we always proceed regardless of gate.
# GATE mode (Phase 3): honor "gate":"escalate" -> block (unless overridden /
# lockfile-sync / never-gate). The wire package_manager is PM_WIRE_NAME.
# ---------------------------------------------------------------------------
correlation_id=""
gate=""
risk_factors=""
resp_eff_mode=""
PORT="${QONTINUI_INSTALL_INTERCEPT_PORT:-}"
MODE="${QONTINUI_INSTALL_INTERCEPT_MODE:-observe}"
OVERRIDE="${QONTINUI_INSTALL_OVERRIDE:-}"
pwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')

# Override path (A4): QONTINUI_INSTALL_OVERRIDE=1 sets override_escalation:true
# on the pre-call so the producer's shipped override path records +overridden
# provenance; the shim still runs the real tool + post-call normally.
override_json=false
if [ "$OVERRIDE" = "1" ]; then override_json=true; fi

# FAIL-OPEN one-time notice (plan §4 Phase 4): emitted at most once, ONLY on the
# pre-call failure path (absent PORT / connect-refused / non-2xx / empty body) —
# never on the post-call. A single shim process makes <=1 pre-call, so "at most
# once" is satisfied by construction.
intercept_unavailable() {
  printf '%s\n' "qontinui: install interception unavailable — running normally" >&2
}

if [ -z "$PORT" ]; then
  # No port injected -> interception is unavailable. Notice once, then run real.
  intercept_unavailable
elif command -v curl >/dev/null 2>&1; then
  pre_body="{\"mode\":\"intercept\",\"repo_path\":\"$pwd_json\",\"package_manager\":\"$PM_WIRE_NAME\",\"packages\":$pkgs_json,\"dev\":$dev,\"override_escalation\":$override_json}"
  # Pre-call: SHORT connect timeout (3s) so a down/slow runner never stalls the
  # shell; bounded total (20s). Failure (-f => non-2xx is an error too) yields an
  # empty body -> the one-time fail-open notice below.
  pre_resp="$(curl -fsS --connect-timeout 3 --max-time 20 \
      -X POST "http://127.0.0.1:$PORT/install-effects/run" \
      -H 'Content-Type: application/json' \
      -d "$pre_body" 2>/dev/null || true)"
  if [ -z "$pre_resp" ]; then
    # connect-refused / timeout / non-2xx / malformed -> fail open (notice once).
    intercept_unavailable
  fi
  if [ -n "$pre_resp" ]; then
    # Extract correlation_id without requiring jq (grep the uuid field).
    correlation_id="$(printf '%s' "$pre_resp" | grep -o '"correlation_id"[[:space:]]*:[[:space:]]*"[^"]*"' | head -n1 | sed 's/.*"\([^"]*\)"$/\1/')"
    # Gate verdict — the producer controls the response shape, so a substring
    # check for the escalate value is ROBUST (the gate FIELD extraction must be
    # robust; this is). The producer serializes GateOutcome snake_case, so the
    # body carries "gate":"escalate" or "gate":"proceed".
    if printf '%s' "$pre_resp" | grep -q '"gate"[[:space:]]*:[[:space:]]*"escalate"'; then
      gate="escalate"
    fi
    # DYNAMIC interception mode (P4): the producer returns the device's EFFECTIVE
    # interception level resolved from the fleet policy at pre-call time
    # ("off" | "observe" | "gate"). When present, it OVERRIDES this shim's
    # spawn-time MODE env so an operator flipping the policy takes effect on
    # already-injected terminals. Parsed with the SAME grep style as gate above.
    # ABSENT (old runner/coord) -> empty -> the eff_mode fallback below keeps the
    # env MODE (full back-compat). An "off" effective mode short-circuits the
    # producer (no correlation_id), so the post-call is already skipped by the
    # existing [ -n "$correlation_id" ] guard.
    resp_eff_mode="$(printf '%s' "$pre_resp" | grep -o '"effective_mode"[[:space:]]*:[[:space:]]*"[^"]*"' | head -n1 | sed 's/.*"\([^"]*\)"$/\1/')"
    # risk_factors is a JSON string array; the extraction is DISPLAY-ONLY
    # best-effort (not load-bearing — only the gate field is). Pull the bracketed
    # array, strip quotes/brackets, join elements with "; ".
    risk_factors="$(printf '%s' "$pre_resp" \
      | grep -o '"risk_factors"[[:space:]]*:[[:space:]]*\[[^]]*\]' \
      | head -n1 \
      | sed 's/.*\[//; s/\].*//; s/"//g; s/,[[:space:]]*/; /g')"
  fi
else
  # PORT set but no curl on PATH -> cannot reach the runner -> fail open once.
  intercept_unavailable
fi

# ---------------------------------------------------------------------------
# GATE DECISION (plan §3 A4 — shell transliteration of gate::should_block).
# BLOCK iff: eff_mode==gate AND gate==escalate AND OVERRIDE!=1 AND NOT
# lockfile_only (A6) AND NOT never-gate tool (npx, NEVER_GATE==1). Every other
# case proceeds. A block prints the A4 UX to STDERR and exits 1 WITHOUT running
# the real tool and WITHOUT a post-call (no install happened — PreContext TTLs
# out).
#
# eff_mode (P4): the EFFECTIVE interception mode is the producer-returned
# effective_mode when present, else the spawn-time MODE env (back-compat with an
# old runner/coord that omits the field). This is what makes the per-install
# mode DYNAMIC — the fleet policy governs already-injected terminals, not just
# the env captured when the terminal was spawned.
# ---------------------------------------------------------------------------
eff_mode="${resp_eff_mode:-$MODE}"
if [ "$eff_mode" = "gate" ] \
   && [ "$gate" = "escalate" ] \
   && [ "$OVERRIDE" != "1" ] \
   && [ "$lockfile_only" != "true" ] \
   && [ "$NEVER_GATE" != "1" ]; then
  # Render the package list for the message: "name@req name …".
  pkgs_disp=""
  for entry in "${pkgs[@]:-}"; do
    [ -z "$entry" ] && continue
    case "$entry" in
      *@@VSEP@@*) dn="${entry%%@@VSEP@@*}"; dr="${entry#*@@VSEP@@}"; disp="${dn}@${dr}" ;;
      *)          disp="$entry" ;;
    esac
    if [ -z "$pkgs_disp" ]; then pkgs_disp="$disp"; else pkgs_disp="$pkgs_disp $disp"; fi
  done
  {
    printf '%s\n' "⚠ qontinui: this install is predicted RISKY and was blocked."
    printf '  package(s): %s\n' "$pkgs_disp"
    printf '  risks: %s\n' "$risk_factors"
    printf '%s\n' "  To override (record an audited +overridden install), re-run with:"
    printf '      QONTINUI_INSTALL_OVERRIDE=1 %s %s\n' "$TOOL" "$*"
  } >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Run the REAL tool transparently. We need its exit code for the post-call,
# so we do NOT exec here — we run it as a child, inheriting stdio.
# Set the recursion guard for the child.
# ---------------------------------------------------------------------------
# An unresolved REAL must not run as `"" "$@"` (a `command not found` for the
# EMPTY name): dispatch by name through passthrough's PATH-stripped fall-through
# in a subshell, so its exec replaces the subshell and we still get the code.
if [ -n "${REAL:-}" ]; then
  QONTINUI_INSTALL_INTERCEPT_GUARD=1 "$REAL" "$@"
else
  ( QONTINUI_INSTALL_INTERCEPT_GUARD=1; export QONTINUI_INSTALL_INTERCEPT_GUARD; passthrough "$@" )
fi
real_code=$?

# ---------------------------------------------------------------------------
# Post-call: POST /install-effects/observe-verify {correlation_id, exit}.
# Best-effort, never alters the agent-visible exit code.
# ---------------------------------------------------------------------------
if [ -n "$correlation_id" ] && [ -n "$PORT" ] && command -v curl >/dev/null 2>&1; then
  post_body="{\"correlation_id\":\"$correlation_id\",\"install_exit_code\":$real_code}"
  curl -fsS --max-time 30 --connect-timeout 3 \
      -X POST "http://127.0.0.1:$PORT/install-effects/observe-verify" \
      -H 'Content-Type: application/json' \
      -d "$post_body" >/dev/null 2>&1 || true
fi

exit $real_code
