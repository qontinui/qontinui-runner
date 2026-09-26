#!/usr/bin/env bash
# qontinui session-restore — Claude SessionStart HOOK (confirmation/liveness).
#
# Delivered to Claude Code ADDITIVELY via the runner-owned `--settings` carrier
# (the identity shim appends that flag), and NEVER written into the user's
# `~/.claude/settings.json` — the delivery is a separate runner-owned file, so
# this is zero-touch for every user out of the box (plan §2 Principle 2, §4
# `capture_hook_delivery`; proven by the Phase 0 probe).
#
# This hook is registered UNCONDITIONALLY, so it rides WHICHEVER carrier the
# continuation flag selects. The two carriers have different filenames and only
# one of them exists on a given box, so naming one here would name a file that
# is absent in the default posture — the wrong answer to "why did my
# SessionStart hook not run?". See `session::claude_hook`.
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

# Drain stdin with a BUILTIN, not `cat` — the same drain the policy and stop
# hooks use. Two reasons, both measured: `cat` blocks FOREVER on a stdin that
# never reaches EOF, which wedges session start rather than degrading it; and
# `cat` missing from PATH produced no request AND no diagnostic, because the
# "payload present?" guard below cannot tell a failed drain from no stdin.
# `read -d ''` slurps to EOF; `-t 1` bounds a hook event that attaches none.
payload=""
IFS= read -r -t 1 -d '' payload || true
port="${QONTINUI_INSTALL_INTERCEPT_PORT:-}"
term="${QONTINUI_TERMINAL_ID:-}"
[ -z "$port" ] && exit 0
command -v curl >/dev/null 2>&1 || exit 0

# Extract session_id + source from the stdin JSON. The field names are stable
# across Claude versions (session_id, source).
#
# This used to pipe straight into a bare `python`. On a box where `python` is
# absent but `python3` is present that pipe wrote nothing, `sid` came out
# empty, and the script exited 0 — so this hook was entirely dead and said
# nothing about it. See the header of `claude_policy_hook.sh` for the full
# diagnosis; the cascade below ends in a rung that spawns no process at all,
# so "no interpreter" is no longer reachable.
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

# JSON-escape a value for the hand-built body below: backslash FIRST, then
# quote. Pure bash, so `sed` is no longer a thing that can be missing from PATH
# and silently change what this hook sends.
#
# Note which rung is dangerous here, because it inverts the usual reading: the
# interpreter-free rung CANNOT produce a value needing this (its capture class
# excludes `"` and `\`), while the `jq` and `python3` rungs can. So the escape
# exists for the rungs that parse correctly.
qh_json_escape() {
  # LC_ALL=C IS LOAD-BEARING, not tidiness. The C0 sweep below is an ERE
  # (`[[ =~ ]]`), and `shopt globasciiranges` — which keeps a GLOB range
  # ASCII-ordered in every locale — does NOT apply to `=~`: bash hands the
  # range to `regcomp`, which honours LC_COLLATE. Under `en_US.UTF-8` the
  # range `[\x01-\x1f]` excludes 09 0a 0b 0c 0d, and since tab, LF and CR
  # have their own short escapes above, the leak is exactly VT (0x0B) and
  # FF (0x0C) — emitted RAW, so the route rejects the body as malformed and
  # `|| true` swallows the rejection. The predecessor of this loop was a
  # GLOB, so it had no such hole; converting it to an ERE opened one.
  local LC_ALL=C
  local qh_s="$1" qh_c qh_u
  qh_s="${qh_s//\\/\\\\}"
  qh_s="${qh_s//\"/\\\"}"
  # Control characters cannot appear RAW inside a JSON string, so a newline in
  # `$PWD` or a payload id used to emit a body the route rejects as malformed
  # — and `|| true` on the curl swallowed the rejection, so the record simply
  # never appeared. The three with short escapes get them; every other C0 byte
  # gets its `\u00XX` form.
  #
  # ESCAPED, NOT DROPPED, because dropping COLLIDES: `cle<0x01>ar` became
  # `clear` and `star<0x02>tup` became `startup` — an unparseable label
  # silently turning into the honesty-sensitive one. Escaping is lossless and
  # no two inputs can meet; it yields valid JSON for every C0 byte ONLY under
  # the C locale pinned above.
  qh_s="${qh_s//$'\n'/\\n}"
  qh_s="${qh_s//$'\r'/\\r}"
  qh_s="${qh_s//$'\t'/\\t}"
  while [[ $qh_s =~ [$'\x01'-$'\x1f'] ]]; do
    qh_c="${BASH_REMATCH[0]}"
    printf -v qh_u '\\u%04x' "'$qh_c"
    qh_s="${qh_s//"$qh_c"/$qh_u}"
  done
  printf '%s' "$qh_s"
}

qh_warn() {
  printf 'claude_session_hook: %s (parser=%s payload_bytes=%s). Degrading, session not blocked.\n' \
    "$1" "$qh_parser" "${#payload}" >&2
}

# Never fabricates: an unreadable payload yields "" and the caller decides.
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
sid="$(extract session_id)"
src="$(extract source)"
if [ -n "$payload" ] && [ -z "$sid" ]; then
  qh_warn "no 'session_id' in the SessionStart payload — this session will not be recorded as open, so an autonomous boot-resume cannot find it"
fi
[ -z "$sid" ] && exit 0

# An unreadable `source` is OMITTED, not defaulted. This line used to read
# `[ -z "$src" ] && src="startup"`, which recorded a `resume` whose source
# could not be parsed as a startup — "a missing source indistinguishable from
# a real one", precisely the failure this plan exists to remove, surviving one
# hook over. Warning about it was not enough: the persisted record still said
# `startup`, so every downstream consumer still saw a real startup.
#
# Omitting is safe and already handled: `SessionOpenRequest.source` is
# `#[serde(default)] Option<String>`, `session_open_rejection` validates only
# `terminal_id` and `session_id`, and the route already renders the absent
# case distinctly (`unwrap_or("?")`). So the record now says "unknown" where
# it is unknown, which is the whole point.
src_field=""
if [ -n "$src" ]; then
  src_field=",\"source\":\"$(qh_json_escape "$src")\""
elif [ -n "$payload" ]; then
  qh_warn "no readable 'source' in the SessionStart payload — OMITTING the field so the record says unknown rather than claiming a startup that may have been a resume or a clear"
fi

# JSON-escape the cwd (backslashes + quotes) so a Windows path is valid JSON.
cwd_json="$(qh_json_escape "${PWD:-}")"

# Include the effective account config dir the runner set on this PTY child
# (session.rs sets CLAUDE_CONFIG_DIR) so the record binds the CORRECT Claude
# account for an autonomous boot-resume. An EMPTY/unset value means the default
# account — send NO config_dir field then (never a bogus "", which would resume
# under CLAUDE_CONFIG_DIR="").
cfg_field=""
if [ -n "${CLAUDE_CONFIG_DIR:-}" ]; then
  cfg_json="$(qh_json_escape "$CLAUDE_CONFIG_DIR")"
  cfg_field=",\"config_dir\":\"$cfg_json\""
fi

# Every interpolated field is escaped, not just the two that obviously needed
# it. `sid` comes straight off the payload and `term` straight off the
# environment, so neither is ours to trust; an unescaped `"` in either used to
# produce a body the route rejects as malformed, silently.
term_json="$(qh_json_escape "$term")"
sid_json="$(qh_json_escape "$sid")"
body="{\"terminal_id\":\"$term_json\",\"session_id\":\"$sid_json\"$src_field,\"provider\":\"claude\",\"cwd\":\"$cwd_json\"$cfg_field}"
curl -fsS --connect-timeout 3 --max-time 10 \
  -X POST "http://127.0.0.1:${port}/control/session-open" \
  -H 'Content-Type: application/json' \
  -d "$body" >/dev/null 2>&1 || true
exit 0
