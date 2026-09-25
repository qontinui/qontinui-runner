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
# Delivered to Claude Code ADDITIVELY via the SAME runner-owned `--settings`
# carrier as the SessionStart confirmation hook (the identity shim appends that
# flag). This hook is registered UNCONDITIONALLY, so it rides whichever of the
# two carrier files the continuation flag selects; the `Stop` continuation hook
# is the one exception — it only ever ships in the armed one.
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
#   - no JSON parser on the box                -> parse in bash itself; the
#                                                 cascade's last rung spawns
#                                                 nothing, so this is no longer
#                                                 a reachable failure
#   - payload present but a field unreadable   -> DIAGNOSE on stderr, then
#                                                 degrade; never abort
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
#   QONTINUI_POLICY_DELIVERED_SHA    sha256 of the policy body this `claude`
#                                    received in its SYSTEM PROMPT at spawn
#                                    (set only when it did — see below)
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

# ── JSON field extraction ────────────────────────────────────────────────────
#
# Resolve a parser ONCE, here, rather than re-probing at each call site.
#
# THE DEFECT THIS REPLACES (plan
# `2026-09-21-policy-body-still-crosses-the-sessionstart-boundary`, Phase 1):
# this gate used to read `command -v python >/dev/null || { printf ''; return 0; }`
# — `python` and nothing else. On every box where `python` is absent but
# `python3` is present (i.e. every modern Linux) BOTH extractions below
# returned the empty string SILENTLY, so the hook built a URL with no
# `source=` and no `claude_session_id=` while still sending a valid
# `X-Qontinui-Policy-Delivered-Sha` header. The route then read
# `raw_source = None`, `source_permits_confirmation(None)` was false, and it
# served the FULL ~11 KB body — the exact outcome the marker exists to avoid —
# while logging the read as `source="startup"`, making a MISSING source
# indistinguishable from a real one. 98 injections on 2026-09-20 alone.
#
# The cascade's last rung parses in bash itself and spawns no process, so
# "no interpreter" is not a reachable failure any more. That deletes the class
# rather than widening it by one interpreter name.
qh_parser="shell"
for qh_cand in jq python3 python; do
  if command -v "$qh_cand" >/dev/null 2>&1; then
    qh_parser="$qh_cand"
    break
  fi
done

# The interpreter-free rung: print `$1`'s string value out of the JSON document
# `$2`, or print NOTHING. It never guesses.
#
# THREE declines are what keep this rung from ever disagreeing with the
# structured ones ABOUT A VALID JSON DOCUMENT. Only the first is about
# escaping; the other two are about STRUCTURE, which a regex cannot see at all
# — it matches a flat byte stream, with no idea how deep the match sits or
# whether a later one supersedes it.
#
# The "valid" qualifier is load-bearing and the claim is false without it: on
# brace-balanced INVALID json — `{"source":"clear",}`, `{a:1,"source":"clear"}`
# — this rung reads what the bytes say while `jq` and `python3` refuse the
# document and return empty. That is a disagreement, but not a dangerous one:
# the rung reports the value actually present, and the structured rungs are
# declining the document rather than contradicting it.
#
#   1. The capture class excludes `"` and `\`, so a value carrying a JSON
#      escape is declined rather than mangled.
#
#   2. A document carrying more than ONE `{` is declined. This is the one that
#      matters, and an earlier cut of this fix got it wrong by counting
#      OCCURRENCES instead: a key living ONLY below the top level occurs
#      exactly once, so it passed the count and was returned, while
#      `json.load` and `jq` both return empty for it. On
#      `{"session_id":"…","meta":{"source":"startup"}}` that rung answered
#      `startup` — a value JSON says is not there. `startup` then passes the
#      four-literal gate, the route reaches the SHA comparison and serves the
#      SHORT confirmation to a session whose payload carried no top-level
#      `source` at all, SILENTLY, because the rung believed it had read
#      something and the "missing source" diagnostic never fired. That is
#      worse than the defect this plan started from, which at least produced
#      the full body. Nesting is also the ordinary way a payload schema grows,
#      unlike a duplicate key, which machine-generated JSON essentially never
#      emits.
#
#      BRACKETS ARE DELIBERATELY NOT CHECKED, but only because DECLINE 1b
#      above covers the case they were standing in for. An earlier version of
#      this note claimed "a key nested inside an array costs an extra `{` too,
#      so the brace test already covers every nesting shape". THAT IS FALSE,
#      and `[{"source":"clear"}]` refutes it: one `{`, one `}`, and the key is
#      nested. The bracket clause had been covering a non-object ROOT by
#      accident, so removing it without the root check reintroduced forgery.
#
#      Declining on a bracket is nonetheless not free, which is why the clause
#      is gone rather than restored: a `[` anywhere in the document, most
#      realistically in a `cwd` such as `/home/u/proj[old]`, returned a
#      no-interpreter box to the ORIGINAL defect (full body AND a
#      NULL-session, unattributable read), and degraded the Stop hook to its
#      constant reason whenever the prompt contained a markdown link.
#
#      Measured over 1588 randomised valid OBJECT-ROOTED documents plus an
#      adversarial corpus: dropping the clause holds disagreements at ZERO
#      while answerable cases rise 29 -> 76. The object-rooted qualifier is
#      load-bearing — that corpus contained NO non-object root, which is
#      exactly why the hole above stayed invisible. A later 16000-case
#      differential with non-object roots included reports 536 answered and
#      ZERO disagreements with DECLINE 1b in place.
#
#   3. A key occurring more than once is declined, because a regex takes the
#      LEFTMOST match while JSON takes the LAST duplicate.
#
# The remaining declines are deliberately conservative — a brace inside a
# string value, or a `"key"` appearing as a VALUE, trips them too.
# Over-declining loses a value; under-declining FORGES one, and only one of
# those is safe. The practical cost is near zero: in valid JSON a string value
# cannot contain the unescaped bytes `"key"` (escaping inserts backslashes,
# which break the match), and today's payloads are flat.
#
# COST, measured rather than asserted: this function is QUADRATIC, not linear —
# the duplicate-key loop re-scans a shrinking tail, so 4KB takes ~18ms and
# 64KB ~3.5s. Hook payloads are sub-KB, so there is no live impact, but do not
# reach for this on a large document.
qh_shell_field() {
  # Byte semantics: every pattern here is ASCII structure, and the C locale
  # makes bash's matching several times cheaper at no behavioural cost. It is
  # `local`, so it is restored on return.
  local LC_ALL=C
  local qh_key="$1" qh_rest="$2" qh_seen=0 qh_braces
  # DECLINE 0 — an absurdly large document. This function is QUADRATIC (see
  # the note above), and nothing downstream bounds it: `read -t 1` bounds the
  # stdin DRAIN, not the parse that follows, so a big payload on a
  # no-interpreter box would block SessionStart for minutes — user-visible,
  # and the same wedge class as the `cat` drain this change already fixed. A
  # ceiling 30x above any real payload and far below the pathological range
  # turns that hang into a loud decline in the already-safe direction: an
  # absent field is a case every caller already handles.
  #
  # It bounds the RESPONSE too, not only stdin: the Stop hook parses the
  # runner's verdict body through this same function, where an oversized
  # document is likelier than in a hook payload. That path degrades to the
  # constant reason, which is still a valid envelope.
  if [ "${#2}" -gt 32768 ]; then
    printf 'qh_shell_field: declining a %s-byte document for key %s — this interpreter-free rung is quadratic and a document this size would stall the hook. Degrading; this field is treated as absent.\n' "${#2}" "$1" >&2
    return 0
  fi
  # DECLINE 1b — the ROOT must be an OBJECT. A key nested inside a TOP-LEVEL
  # ARRAY is nested at NO extra `{`, so the brace test alone cannot see it:
  # `[{"source":"clear"}]` has exactly one `{` and one `}` and would be read
  # as a top-level `clear` that JSON says is not there. This is the hole the
  # bracket clause used to cover by accident, and removing that clause without
  # this line reintroduced forgery — 406 disagreements over 16000 differential
  # cases, every one a non-object root.
  case "${2#"${2%%[![:space:]]*}"}" in
    "{"*) ;;
    *) return 0 ;;
  esac
  # DECLINE 2 — exactly one `{` and one `}`, i.e. no nesting. Brackets are
  # deliberately not tested; see the note above.
  qh_braces="${2//[!{]/}"
  [ "${#qh_braces}" -eq 1 ] || return 0
  qh_braces="${2//[!\}]/}"
  [ "${#qh_braces}" -eq 1 ] || return 0
  # DECLINE 3 — the key occurs more than once.
  while [[ $qh_rest =~ \"$qh_key\"[[:space:]]*: ]]; do
    qh_seen=$((qh_seen + 1))
    [ "$qh_seen" -gt 1 ] && return 0
    qh_rest="${qh_rest#*\"$qh_key\"}"
  done
  [ "$qh_seen" -eq 1 ] || return 0
  if [[ $2 =~ \"$qh_key\"[[:space:]]*:[[:space:]]*\"([^\"\\]*)\" ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  fi
  return 0
}

# Constrain a value that becomes a URL PATH SEGMENT. A path segment is not a
# query parameter: a `/`, `?`, `#` or `..` inside it re-points the request at a
# DIFFERENT loopback route — and for the hooks that `POST --data-binary @-`,
# redirects the payload there with it. `csid` below was already shape-checked
# because it is interpolated into a query string; this is the same discipline
# for the segment, which had none. Anything outside the id alphabet is dropped
# rather than sent, so the hook degrades instead of addressing a wrong route.
qh_safe_id() {
  case "$1" in
    # Dot segments are spelled out because the class below PERMITS `.`, so
    # they passed the guard the comment claimed stopped them. curl collapses
    # `.` and `..` alike BEFORE sending, which takes the request out of
    # `/sessions/<id>/` entirely and onto the bare route — the opposite of
    # degrading. Verified against a real listener: `/sessions/./policy-context`
    # arrives as `GET /sessions/policy-context`, and `/sessions/../…` as
    # `GET /policy-context`. Both are 404 today, so this closes a class rather
    # than a live mis-route.
    "" | "." | ".." | *..* | */.* | *[!A-Za-z0-9._-]*) ;;
    *) printf '%s' "$1" ;;
  esac
}

# One diagnostic channel. stderr is what the runner captures into its log, so
# a degrade SAYS SO instead of looking like a legitimate absence — this script
# never had a way to report "I could not read the payload", which is why the
# regression above stayed invisible for five days.
qh_warn() {
  printf 'claude_policy_hook: %s (parser=%s payload_bytes=%s). Degrading, session not blocked.\n' \
    "$1" "$qh_parser" "${#payload}" >&2
}

# Extract one top-level string field from $payload.
#
# NEVER FABRICATES. An absent key, an unparseable payload, or a value a rung
# cannot represent all yield "" — and both callers below shape-validate what
# comes back (a canonical uuid; one of four literals), so a sloppy read can
# only LOSE a value, never forge one. That is what makes the bash rung safe.
extract() {
  case "$qh_parser" in
    jq)
      printf '%s' "$payload" | jq -r --arg k "$1" '.[$k] // empty' 2>/dev/null || true
      ;;
    python3 | python)
      printf '%s' "$payload" | "$qh_parser" -c "import sys,json
try:
  d=json.load(sys.stdin); print(d.get('$1',''))
except Exception:
  print('')
" 2>/dev/null || true
      ;;
    *)
      qh_shell_field "$1" "$payload"
      ;;
  esac
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
if [ -n "$payload" ] && [ -z "$raw_sid" ]; then
  qh_warn "no 'session_id' in the SessionStart payload — coord will record this policy read with a NULL session, so it is unattributable"
fi
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
sid="$(qh_safe_id "$term")"
if [ -z "$sid" ]; then
  sid="$(qh_safe_id "$raw_sid")"
fi
[ -z "$sid" ] && exit 0

# `source` tells the route WHY the session is starting. All three values
# inject: a resumed session carries its old context but not the policies as
# they now stand, and a compacted one has just had them evicted — both are
# exactly the cases Step 0 exists for. Constrained to the known set so the
# value is safe to interpolate into a query string without encoding; anything
# else is dropped and the route applies its own default.
#
# A MISSING `source` is the defect signature, so it gets its own diagnostic
# naming the consequence: with no `source=` param the route cannot reach the
# SHA comparison at all and serves the full body regardless of the marker.
# Distinguish it from an EMPTY payload (a hook invoked with no stdin), which
# is an honest UNKNOWN rather than a parse failure — only the latter warns.
src="$(extract source)"
if [ -n "$payload" ] && [ -z "$src" ]; then
  qh_warn "no 'source' in the SessionStart payload — the route cannot confirm spawn delivery and will serve the FULL policy body even though a delivered-sha marker is present"
fi
case "$src" in
  startup|resume|compact|clear) ;;
  *)
    if [ -n "$src" ]; then
      qh_warn "unrecognised source '$src' — dropped; the route will apply its own default and serve the full body"
    fi
    src=""
    ;;
esac

# The spawn-time delivery marker (plan
# `2026-09-15-runner-policy-injection-off-sessionstart-hook-channel`). The
# policy body now normally reaches the session through its system prompt, and
# the runner exports the sha256 of the exact body it composed on the `claude`
# process that got it. Forwarded so the route can send a short confirmation
# INSTEAD of the ~11 KB body — but only on a startup/compact whose sha
# equals the hash of the body it has just fetched; a resume (which re-sends the
# system prompt recorded when its conversation began), a clear (whose snapshot
# reset is unverified), any other value, or none gets the full body. The
# script decides nothing: it forwards, the route judges. Constrained to 64 hex
# characters (a sha256), so a garbage value is simply not sent. It rides a
# request HEADER, not the query string: the runner's HTTP trace log records
# request URIs.
dsha="${QONTINUI_POLICY_DELIVERED_SHA:-}"
case "$dsha" in
  *[!0-9a-fA-F]*) dsha="" ;;
esac
[ "${#dsha}" -eq 64 ] || dsha=""

# Every param is shape-constrained above, so none needs encoding: `src` is one
# of four literals, `csid` is hex-and-hyphens.
url="http://127.0.0.1:${port}/sessions/${sid}/policy-context"
sep="?"
if [ -n "$src" ]; then
  url="${url}${sep}source=${src}"
  sep="&"
fi
if [ -n "$csid" ]; then
  url="${url}${sep}claude_session_id=${csid}"
fi
dsha_header=()
if [ -n "$dsha" ]; then
  dsha_header=(-H "X-Qontinui-Policy-Delivered-Sha: ${dsha}")
fi

# The route fetches from coord, so allow more headroom than the loopback trip
# itself needs — but stay bounded: a hung coord must not stall a session
# start. The route's own coord client times out well inside this budget.
resp="$(curl -fsS --connect-timeout 2 --max-time 15 ${dsha_header[@]+"${dsha_header[@]}"} "$url" 2>/dev/null || true)"
[ -z "$resp" ] && exit 0

# Verbatim. The route already rendered the complete hook envelope; adding
# anything here (a trailing newline is fine, JSON is not) would corrupt it.
printf '%s' "$resp"
exit 0
