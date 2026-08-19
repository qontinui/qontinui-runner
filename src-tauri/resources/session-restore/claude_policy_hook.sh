#!/usr/bin/env bash
# qontinui policy delivery — Claude `SessionStart` HOOK (policy injection).
# Plan `2026-08-08-runner-enforced-policy-pull.md`, Phase 1.
#
# ## Why this exists
#
# `policy/session-protocol` Step 0 says: "Never work from memory of these
# documents; they version frequently." It depends entirely on a session
# VOLUNTARILY calling coord_list_prompt_documents + coord_get_prompt_document
# at turn one, and nothing checks that it did. The failure is silent and
# total: a session that skips Step 0 does not degrade, it simply operates with
# no policy at all while producing work that looks normal.
#
# This hook removes the failure class instead of detecting it. The runner
# fetches the policy documents from coord and hands them to the session as
# SessionStart context, so Step 0 is satisfied BY CONSTRUCTION.
#
# ## Delivery
#
# Delivered to Claude Code ADDITIVELY via the SAME `--settings
# <claude_hook_settings.json>` carrier as the SessionStart confirmation hook
# and the Stop continuation hook (the identity shim appends that flag).
# NOTHING is ever written to the user's `~/.claude/settings.json`. This script
# is registered as a SECOND command inside the EXISTING `SessionStart` block —
# it is a sibling of `claude_session_hook.sh`, never an edit to it: that
# script is the confirmation/liveness carrier and must keep its silent-stdout
# contract (it POSTs and discards its response).
#
# Claude invokes this on a `SessionStart` event, piping a JSON payload on
# stdin:
#   { "session_id": "<id>", "source": "startup" | "resume" | "compact", ... }
#
# ## The contract on stdout
#
# Claude reads a SessionStart hook's stdout as the JSON envelope
#   {"hookSpecificOutput":{"hookEventName":"SessionStart",
#                          "additionalContext":"<text>"}}
# and splices `additionalContext` into the session's context.
#
# THIS SCRIPT BUILDS NO JSON. The runner's route returns that complete
# envelope already rendered, and we print its body VERBATIM. Keeping the
# script dumb is the same design rule `claude_stop_hook.sh` follows (plan D4):
# all policy — what to fetch, what to render, the flag, the cache, the
# fail-open notice — lives in Rust, in
# `src/mcp/policy_context.rs`, where it is unit-testable and shippable without
# re-materializing a shell script.
#
# An EMPTY response body means "inject nothing" (the flag is `off` or
# `observe`), and printing nothing is exactly how a hook declines to inject.
# So the empty-body and the failure paths coincide, which is why every failure
# below is a bare `exit 0`.
#
# ## FAIL-OPEN INVARIANTS (a broken runner/coord must NEVER block a session)
#
#   - missing port / session key / curl        -> exit 0, no output
#   - curl error / non-2xx / empty body        -> exit 0, no output
#   - missing python (source/session_id parse) -> degrade, never abort
#
# The route itself never 5xxs and never refuses: when coord is unreachable it
# still answers 200 with an `additionalContext` telling the session the pull
# failed and it must fetch policy itself. So a silent no-op here is reserved
# for the cases where we cannot reach our OWN runner.
#
# Env (injected by the runner at PTY spawn — the identity seam):
#   QONTINUI_RUNNER_API_PORT         the runner's :9876 loopback API port
#   QONTINUI_INSTALL_INTERCEPT_PORT  fallback port (same server by default)
#   QONTINUI_TERMINAL_ID             the per-PTY terminal id
#
# NOTE: there is deliberately NO env kill-switch read here (unlike
# `claude_stop_hook.sh`'s `QONTINUI_STOP_HOOK_CONTINUATION` short-circuit).
# `QONTINUI_POLICY_INJECTION` is read RUNNER-side by the route, which answers
# an empty body when it is `off`/`observe`. SessionStart fires once per
# session, not once per turn, so the per-turn latency argument that justified
# the stop hook's dark-mode short-circuit does not apply — and a single
# source of truth for the flag cannot drift.
set -u

# Drain stdin with a builtin rather than `cat` (process creation on this
# fleet's Windows/MSYS boxes is 0.5-2.3s per spawn). `read -d ''` slurps to
# EOF; `-t 1` bounds a hook event that attaches no stdin.
payload=""
IFS= read -r -t 1 -d '' payload || true

port="${QONTINUI_RUNNER_API_PORT:-${QONTINUI_INSTALL_INTERCEPT_PORT:-}}"
term="${QONTINUI_TERMINAL_ID:-}"
[ -z "$port" ] && exit 0
command -v curl >/dev/null 2>&1 || exit 0

# Extract a field from the stdin JSON. Python is PREFERRED (robust JSON) but
# OPTIONAL here: unlike the Stop hook, this script can still do useful work
# without it whenever `QONTINUI_TERMINAL_ID` is set, so a missing interpreter
# degrades the `source` label and the read's ATTRIBUTION instead of killing the
# injection. The session still receives its policy; coord just records the read
# with a NULL session id, which reads downstream as `unavailable` — an admitted
# blind spot, never a non-compliance verdict.
extract() {
  command -v python >/dev/null 2>&1 || { printf ''; return 0; }
  printf '%s' "$payload" | python -c "import sys,json
try:
  d=json.load(sys.stdin); print(d.get('$1',''))
except Exception:
  print('')
" 2>/dev/null
}

# The CLAUDE session id, straight off the hook payload. This is the id coord
# attributes a policy read to (`X-Coord-Caller-Session`), and it is NOT the
# runner terminal id below — one runner terminal can host several Claude
# sessions, so the terminal id would attribute every one of them to the same
# session. Sent as its own query param precisely so the route never has to
# guess which of the two it is holding.
#
# Constrained to canonical UUID shape here as a cheap first filter (the route
# parses it strictly and drops anything that fails). Never fabricated: if the
# payload carries no session id, the param is simply omitted and coord records
# the read with a NULL session — an honest "unattributable", which downstream
# reads as `unavailable`, never as non-compliance.
raw_sid="$(extract session_id)"
csid="$raw_sid"
case "$csid" in
  [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]-[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]) ;;
  *) csid="" ;;
esac

# Session key for ADDRESSING the route: prefer the runner terminal id (the
# route resolves it against local state), else the Claude session id — the same
# precedence `claude_stop_hook.sh` uses, so both hooks key on one identity.
# Deliberately separate from `csid` above: this one names the route's path
# segment, that one names WHO the read is attributed to.
sid="$term"
if [ -z "$sid" ]; then
  sid="$raw_sid"
fi
[ -z "$sid" ] && exit 0

# `source` tells the route WHY the session is starting. All three values
# inject: a resumed session carries its old context but not the policies as
# they now stand, and a compacted one has just had them evicted — both are
# exactly the cases Step 0 exists for. Constrained to the known set so the
# value is safe to interpolate into a query string without encoding; anything
# else is dropped and the route applies its own default.
src="$(extract source)"
case "$src" in
  startup|resume|compact|clear) ;;
  *) src="" ;;
esac

# Both params are shape-constrained above, so neither needs encoding: `src` is
# one of four literals and `csid` is hex-and-hyphens.
url="http://127.0.0.1:${port}/sessions/${sid}/policy-context"
sep="?"
if [ -n "$src" ]; then
  url="${url}${sep}source=${src}"
  sep="&"
fi
if [ -n "$csid" ]; then
  url="${url}${sep}claude_session_id=${csid}"
fi

# The route fetches from coord, so allow more headroom than the loopback trip
# itself needs — but stay bounded: a hung coord must not stall a session
# start. The route's own coord client times out well inside this budget.
resp="$(curl -fsS --connect-timeout 2 --max-time 15 "$url" 2>/dev/null || true)"
[ -z "$resp" ] && exit 0

# Verbatim. The route already rendered the complete hook envelope; adding
# anything here (a trailing newline is fine, JSON is not) would corrupt it.
printf '%s' "$resp"
exit 0
