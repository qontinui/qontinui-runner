#!/usr/bin/env bash
# qontinui session-autonomy — Claude `Stop` HOOK (continuation verdict).
# Plan `2026-07-17-session-autonomy-fabric.md`, Phase 1.
#
# Delivered to Claude Code ADDITIVELY via the SAME `--settings` carrier as the
# SessionStart confirmation hook (the identity shim appends that flag; nothing
# is ever written to the user's `~/.claude/settings.json`).
#
# THIS hook is the one whose REGISTRATION is gated, so — alone among the four
# bundled scripts — its carrier is fixed: it is only ever registered in
# `claude_hook_settings.json`, the ARMED variant. The dark variant
# (`claude_hook_settings-nostop.json`) carries no `Stop` key at all, which is
# what stops Claude spawning a `bash` for this script once per assistant turn.
# The short-circuit below therefore no longer covers runner-spawned sessions;
# it still covers a HAND-STARTED `claude` that picks up an armed carrier left on
# disk by a previously-armed runner. See `session::claude_hook`.
#
# Claude invokes this when the agent is about to END A TURN, piping a JSON
# payload on stdin:
#   { "session_id": "<id>", "stop_hook_active": true|false, ... }
#
# The script is a DUMB CURL by design (plan D4): all policy lives in the
# runner's loopback verdict endpoint. It POSTs the stdin payload to
#   POST http://127.0.0.1:{port}/sessions/{id}/continuation-verdict
# and maps the response:
#   {"decision":"block","prompt":"<text>"} -> stdout {"decision":"block","reason":"<text>"}
#   anything else                          -> no output (allow the stop)
#
# FAIL-OPEN INVARIANTS (a broken runner/coord must NEVER trap a session):
#   - missing port / terminal id / curl          -> exit 0, no output
#   - no JSON parser on the box                  -> parse in bash itself
#   - curl error / non-2xx / undecodable response -> exit 0, no output
#   - `stop_hook_active:true` in the payload      -> exit 0 BEFORE the POST
#     (never re-block a continuation the hook chain already forced; the
#     endpoint honors the same guard as defense in depth)
#
# Env (injected by the runner at PTY spawn — the identity seam):
#   QONTINUI_RUNNER_API_PORT         the runner's :9876 loopback API port
#   QONTINUI_INSTALL_INTERCEPT_PORT  fallback port (same server by default)
#   QONTINUI_TERMINAL_ID             the per-PTY terminal id
#   QONTINUI_STOP_HOOK_CONTINUATION  the runner's PARSED continuation mode
#                                    (`off`|`observe`|`on`) — see the dark-mode
#                                    short-circuit below
set -u

# Drain stdin with a BUILTIN, not `cat`. This fires on every assistant turn of
# every session, and process creation on this fleet's Windows/MSYS boxes is
# 0.5-2.3s per spawn — so `cat` alone cost more than the work it fed. `read -d ''`
# slurps to EOF; `-t 1` bounds a hook event that attaches no stdin.
# Plan: 2026-08-06-stop-hook-per-turn-latency (P3).
payload=""
IFS= read -r -t 1 -d '' payload || true

# Loop guard, local leg: a Stop fired while a hook-forced continuation is
# already active must never be re-blocked. Cheap textual check (the endpoint
# re-checks with a real JSON parse). Stays FIRST among the exits — it is the
# loop guard and must fire regardless of mode.
case "$payload" in
  *'"stop_hook_active":true'* | *'"stop_hook_active": true'*) exit 0 ;;
esac

# Dark-mode short-circuit. `QONTINUI_STOP_HOOK_CONTINUATION` defaults to `off`
# (mcp/continuation_verdict.rs), and in `Mode::Off` the verdict endpoint answers
# `allow` with zero coord traffic — so every turn was paying a loopback HTTP
# round trip plus a Python interpreter start to be told nothing happened.
#
# Fail SAFE in the same direction the runner does: an unset or unrecognised
# value reads as `off`, exactly like `Mode::from_flag`. The runner forwards its
# already-parsed mode, so an unrecognised value here means an OLD runner that
# does not inject the variable at all — in which case skipping is still correct,
# because such a runner is equally likely to be running the feature dark.
#
# Draining stdin BEFORE this exit is deliberate: returning without consuming the
# payload risks EPIPE on the writer, and the builtin drain above is free.
case "${QONTINUI_STOP_HOOK_CONTINUATION:-off}" in
  observe|on) ;;             # feature armed — fall through and ask the runner
  *)          exit 0 ;;      # off / unset / junk — nothing to ask
esac

port="${QONTINUI_RUNNER_API_PORT:-${QONTINUI_INSTALL_INTERCEPT_PORT:-}}"
term="${QONTINUI_TERMINAL_ID:-}"
[ -z "$port" ] && exit 0
command -v curl >/dev/null 2>&1 || exit 0

# ── JSON parser ──────────────────────────────────────────────────────────────
#
# This used to be `command -v python >/dev/null 2>&1 || exit 0` right here,
# which killed the ENTIRE hook — verdict request included — on any box where
# `python` is absent but `python3` is present. The cascade below ends in a
# rung that spawns no process, so no interpreter is required at all; see
# `claude_policy_hook.sh` for the full diagnosis (plan
# `2026-09-21-policy-body-still-crosses-the-sessionstart-boundary`).
#
# Resolved ONCE, and used for both parses below (the payload's session id, and
# the runner's verdict response).
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

qh_warn() {
  printf 'claude_stop_hook: %s (parser=%s). Degrading, stop not blocked.\n' "$1" "$qh_parser" >&2
}

# Session key: prefer the runner terminal id (the endpoint resolves it against
# local state), else the Claude session id from the hook payload.
sid="$(qh_safe_id "$term")"
if [ -z "$sid" ]; then
  case "$qh_parser" in
    jq)
      sid="$(printf '%s' "$payload" | jq -r '.session_id // empty' 2>/dev/null || true)"
      ;;
    python3 | python)
      sid="$(printf '%s' "$payload" | "$qh_parser" -c "import sys,json
try:
  d=json.load(sys.stdin); print(d.get('session_id',''))
except Exception:
  print('')
" 2>/dev/null || true)"
      ;;
    *)
      sid="$(qh_shell_field session_id "$payload")"
      ;;
  esac
  sid="$(qh_safe_id "$sid")"
  if [ -n "$payload" ] && [ -z "$sid" ]; then
    qh_warn "no session key — QONTINUI_TERMINAL_ID unset and no 'session_id' in the Stop payload; the continuation verdict is never requested"
  fi
fi
[ -z "$sid" ] && exit 0

resp="$(printf '%s' "$payload" | curl -fsS --connect-timeout 2 --max-time 10 \
  -X POST "http://127.0.0.1:${port}/sessions/${sid}/continuation-verdict" \
  -H 'Content-Type: application/json' \
  --data-binary @- 2>/dev/null || true)"
[ -z "$resp" ] && exit 0

# Map a block verdict to the Claude Stop-hook JSON contract; anything else
# (allow / garbage) produces NO output, which lets the stop proceed.
QH_DEFAULT_REASON='Are there follow-ups? If so work on them; if nothing is left, clean up worktrees/branches and mark the session finished.'
case "$qh_parser" in
  jq)
    printf '%s' "$resp" | jq -c --arg d "$QH_DEFAULT_REASON" \
      'if .decision == "block" then {decision:"block", reason:(if (.prompt // "") == "" then $d else .prompt end)} else empty end' \
      2>/dev/null || true
    ;;
  python3 | python)
    printf '%s' "$resp" | "$qh_parser" -c "import sys,json
try:
  d=json.load(sys.stdin)
except Exception:
  sys.exit(0)
if d.get('decision') == 'block':
  reason = d.get('prompt') or sys.argv[1]
  print(json.dumps({'decision': 'block', 'reason': reason}))
" "$QH_DEFAULT_REASON" 2>/dev/null || true
    ;;
  *)
    # No parser: decide on the verdict with a regex rather than a substring
    # test, so `"decision": "allow"` cannot be matched by the word "block"
    # appearing anywhere else in the body.
    if [[ $resp =~ \"decision\"[[:space:]]*:[[:space:]]*\"block\" ]]; then
      # Whatever this yields is SAFE TO EMBED VERBATIM in the JSON below: the
      # rung's capture class excludes `"` and `\`, and it declines an
      # ambiguous document outright. A prompt it cannot read falls back to the
      # constant reason — a degrade, never a corrupt envelope.
      qh_reason="$(qh_shell_field prompt "$resp")"
      [ -z "$qh_reason" ] && qh_reason="$QH_DEFAULT_REASON"
      printf '{"decision": "block", "reason": "%s"}\n' "$qh_reason"
    fi
    ;;
esac
exit 0
