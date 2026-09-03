#!/usr/bin/env bash
# pr-status — HTTP fallback for the coord_pr_status MCP read.
#
# The PRIMARY path is the native MCP tool `coord_pr_status` (call it directly
# from the session; it is in the read-only allow-set and needs no device JWT).
# This script is the fallback for when that tool reads as unknown/method-not-
# found in a session (per-agent allow-set masking) — it mirrors the `/gate`
# transport cascade: runner loopback proxy first, then a direct coord MCP call
# with an acting-bearer token.
#
# Usage:
#   pr-status.sh --mine
#   pr-status.sh --repo qontinui/qontinui-runner --number 676
#
# Emits the raw PrStatusCard JSON (one object, or an array for --mine) to
# stdout; the caller renders it. Never fabricate a status — if every transport
# fails, print the error and stop.
#
# Worst-case wall-clock: every door gets max 2 attempts (the second only on a
# retry-safe verdict — 503/CREDENTIAL_REFRESHING, or TIMEOUT since 2026-09-02 —
# after a 3s sleep). A stalling door NOW RETRIES, so the old "pure-stall bound
# stays 11 doors × -m 20 ≈ 4 minutes" no longer holds: a pure-stall sweep is the
# ceiling case, (10 loopback + 1 acting-bearer) × 2 × 20s + 11 × 3s ≈ 8 minutes.
# That is the same ceiling as before — stalls simply moved from the floor to it —
# so callers wrapping this script in their own timeout must budget the ceiling,
# not the floor.

set -euo pipefail

MINE="false"
REPO=""
NUMBER=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mine)   MINE="true"; shift ;;
    --repo)   REPO="${2:-}";   shift 2 ;;
    --number) NUMBER="${2:-}"; shift 2 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown arg: $1" >&2; exit 2 ;;
  esac
done

# ----- build the MCP tools/call arguments -------------------------------------
if [[ "$MINE" == "true" ]]; then
  ARGS='{"mine":true}'
elif [[ -n "$REPO" && -n "$NUMBER" ]]; then
  # json_string is deferred until after the shared block defines $JSON_READER,
  # so build ARGS below rather than here.
  ARGS=""
else
  echo "error: pass --mine, or both --repo and --number" >&2
  exit 2
fi

RPC="$(printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_pr_status","arguments":%s}}' "$ARGS")"

COORD_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
HERE="$(cd "$(dirname "$0")" && pwd)"

# Dependency floor + typed door classification. Until 2026-08-31 this script
# hard-required `jq curl python` — all three, no fallback — which exits 127 on
# the Windows operator box, where jq is ABSENT. The shared block below carries
# coord-revive.sh's reader selection instead (jq, else a smoke-tested python),
# so /pr-status now works wherever /coord-revive does.
# Caller contract for the block:
PROBE_TIMEOUT=20                 # matches this script's own `curl -m 20`
DOOR_SCRIPT_NAME="pr-status.sh"

# ============================ SHARED COORD-DOOR CLASSIFIER v1 ============================
# BYTE-IDENTICAL COPY. Lives in BOTH .claude/skills/coord-revive/coord-revive.sh and
# .claude/skills/pr-status/pr-status.sh, pinned equal by CI check #35
# (scripts/lint-shared-door-classifier.py). Edit ONE and CI reds until you edit BOTH.
#
# WHY A COPY AND NOT A SOURCED LIBRARY
# (plan 2026-07-31-extract-shared-coord-door-classify-helper):
#   qontinui-runner bundles fleet command files into its BINARY via `include_str!`
#   (src-tauri/src/fleet_commands.rs). `include_str!` embeds the text of ONE file at
#   compile time, so a bundled copy has no sibling files and no filesystem to resolve
#   them against — a `source` line in a bundled script is not fragile, it is
#   STRUCTURALLY IMPOSSIBLE. The bundle already carries gate.md and policy.md, i.e. it
#   is growing along exactly this coord-door axis. scripts/lib/ is good precedent for
#   code that always has a filesystem; these two scripts are about to stop having one.
#
# CALLER CONTRACT — set these BEFORE this block:
#   PROBE_TIMEOUT     seconds; interpolated into the TIMEOUT verdict string.
#   DOOR_SCRIPT_NAME  this script's name, for LOCAL-fault preflight errors.
#
# VERDICT STRINGS ARE AN INTERFACE. coord-revive.sh's probe_door() branches on
# `CREDENTIAL_REFRESHING*` as a PREFIX match to decide whether to retry (it appends a
# "[curl: …]" suffix to every non-LIVE verdict, which is why the match is a prefix).
# A one-character change silently removes that retry, with no error anywhere.
# ========================================================================================
: "${PROBE_TIMEOUT:?must be set before the shared coord-door classifier}"
: "${DOOR_SCRIPT_NAME:?must be set before the shared coord-door classifier}"

if ! command -v curl >/dev/null 2>&1; then
  echo "$DOOR_SCRIPT_NAME: ERROR: curl is required (LOCAL fault, not a coord verdict)." >&2
  exit 127
fi

# jq is NOT guaranteed to exist — it is ABSENT on the Windows operator box
# (verified 2026-08-06 by the /policy + /gate fix, and again 2026-08-07 when THIS
# script hard-exited 127 during a real dead-transport recovery). Requiring it
# outright is worse here than anywhere else in the fleet: this script is the
# documented recovery path for "Command failed with no output", so an agent hits
# it precisely when coord is already unreachable — and got a bare
# "jq is required" with no door probed and no typed verdict. The recovery tool
# must not be the thing that fails.
#
# Pick a reader up front and fail LOUD naming it a LOCAL fault, so a missing
# binary can never read as a coord verdict (same rule as AUTH_HEADER_* below).
# Inherited from the /policy and /gate fix rather than diverging from it.
# `command -v python` proves a NAME resolves, not that it WORKS. Windows ships
# App Execution Alias stubs at %LOCALAPPDATA%/Microsoft/WindowsApps/python{,3}.exe
# which resolve, print nothing and exit non-zero — and both names are the same
# stub, so a python3 fallback cannot rescue it. Accepting one would make every
# read_cfg return empty, every candidate be rejected as "no proxy-shaped
# coord-mcp entry" (a message that blames the CONFIG), and the cascade print
# VERDICT: DEAD over live doors. That is the missing-binary-reads-as-empty-field
# bug this fix exists to kill, reintroduced one layer up. Smoke-test instead.
JSON_READER=""
if command -v jq >/dev/null 2>&1; then
  JSON_READER=jq
else
  for c in python python3; do
    # Check the OUTPUT, not just the exit code. A python that prints at
    # interpreter start (sitecustomize/usercustomize, a printing .pth, a conda
    # or venv shim) exits 0 and would pass an exit-code-only probe -- then
    # shifts read_cfg's positional two-line pair by one, emptying every value
    # and printing DEAD over live doors. That is the same
    # missing-binary-reads-as-empty class this selection exists to close,
    # entering through stdout instead of the status. tr strips the CR a native
    # Windows python appends.
    if command -v "$c" >/dev/null 2>&1 \
       && [ "$("$c" -c 'import json;print(1)' </dev/null 2>/dev/null | tr -d '\r\n')" = "1" ]; then
      JSON_READER="$c"; break
    fi
  done
fi
if [ -z "$JSON_READER" ]; then
  echo "$DOOR_SCRIPT_NAME: ERROR: neither jq nor a working python can read JSON — cannot probe any door (LOCAL fault, not a coord verdict)." >&2
  exit 127
fi

# rpc_has_result — reads a JSON-RPC body on STDIN; 0 iff it carries a real
# `result` and no `error`. A JSON-RPC surface answers HTTP 200 even for in-band
# errors, so LIVE requires the result object itself, or the caller re-issues its
# lost write into a broken door.
rpc_has_result() {
  if [ "$JSON_READER" = jq ]; then
    jq -e '(.error == null) and (.result != null)' >/dev/null 2>&1  # envelope-ok: a two-key PRESENCE predicate (exit status only, no value leaves it); rpc_print_result is the typed reader
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
sys.exit(0 if (d.get("error") is None and d.get("result") is not None) else 1)' >/dev/null 2>&1  # envelope-ok: same presence predicate, python arm
  fi
}

# classify <curl_exit> <http_code> <body> [unauth-wording] -> typed verdict.
# 401 wording is parameterized because on a loopback proxy it means "stale
# proxy key" while on the direct bearer door it means "rejected bearer".
#
# INVARIANT: every path returns a NAMED cause. A verdict that names nothing is
classify() {
  local ce="$1" code="$2" body="$3" unauth="${4:-COORD_MCP_PROXY_UNAUTHORIZED (stale/evicted proxy key)}"
  # curl can fail BEFORE the transfer starts (unopenable `--header @file`, bad
  # URL). It then never writes its `-w` output, so $code arrives EMPTY — not
  # "000", which is what curl reports for a failure during the transfer. An
  # empty code fell through every branch below into the "HTTP_$code
  # (unclassified: …)" tail and printed a bare `HTTP_ (unclassified: )`:
  # exactly the undifferentiated mask this script exists to replace. Normalize
  # first so the pre-transfer failures reach the same named causes.
  [ -n "$code" ] || code="000"
  if [ "$ce" = "7" ]; then echo "CONNECT_REFUSED (dead port - no listener)"; return; fi
  if [ "$ce" = "28" ]; then echo "TIMEOUT (no response within ${PROBE_TIMEOUT}s). Often SATURATION rather than a dead door - do NOT restart the runner on this alone; re-run, or use another door"; return; fi
  # 26 = "couldn't open/read the local data file", i.e. the auth-header file.
  # This is a LOCAL fault and must never read as a coord verdict — the same
  # rule coord-acting-bearer.sh states for its mint ("a 'Failed to open' here
  # must not masquerade as 'coord down'").
  if [ "$ce" = "26" ]; then
    echo "AUTH_HEADER_UNREADABLE (curl could not open the staged header file - LOCAL fault, says nothing about coord)"; return
  fi
  if [ "$ce" != "0" ] && [ "$code" = "000" ]; then echo "UNREACHABLE (curl exit $ce)"; return; fi
  # Non-2xx guard on the body-marker match (asymmetry 2). Without it a healthy
  # 200 whose payload merely CONTAINS the marker — a tool description or a PR
  # body discussing this very verdict — classifies as CREDENTIAL_REFRESHING
  # instead of LIVE: a confidently wrong DEAD verdict about a live door.
  # Adopted from pr-status.sh's `[[ "$code" != 2* ]]`, which guarded it first.
  # Latent, not live, when written: a real tools/list against the live loopback
  # door returned 200 / 101499 bytes with 0 occurrences of either marker
  # (measured 2026-07-31). The guard closes it before it becomes live.
  case "$code" in
    2*) ;;
    *)
      case "$body" in
        *COORD_MCP_PROXY_CREDENTIAL_REFRESHING*|*CREDENTIAL_REFRESHING*)
          echo "CREDENTIAL_REFRESHING (retry-safe - proxy up, withholding while its device JWT refreshes)"; return ;;
        *COORD_MCP_PROXY_UNAUTHORIZED*)
          echo "$unauth"; return ;;
      esac ;;
  esac
  case "$code" in
    200)
      # A JSON-RPC surface answers 200 even for in-band errors — LIVE requires
      # an actual result object, or the caller re-issues into a broken door.
      if printf '%s' "$body" | rpc_has_result; then
        echo "LIVE"
      else
        echo "HTTP_200_NOT_MCP (200 without a JSON-RPC result - treat as dead)"
      fi
      return ;;
    401) echo "$unauth"; return ;;
    503) echo "CREDENTIAL_REFRESHING (retry-safe - HTTP 503)"; return ;;
  esac
  local snippet
  snippet=$(printf '%s' "$body" | tr -d '\n' | head -c 120)
  echo "HTTP_$code (unclassified: ${snippet:-<empty body>})"
}

# ========================== END SHARED COORD-DOOR CLASSIFIER v1 ==========================

# json_string — read STDIN, print it as ONE JSON string literal (quotes included).
# Reader-agnostic on purpose. This used to be a bare `python -c json.dumps`, and
# it sat INSIDE a command substitution whose OUTER printf still exits 0 — so on a
# python-less box `set -e` never fired, ARGS became `{"repo":,"number":1}`, and
# coord's parse error got reported as a TRANSPORT failure. The old `for bin in jq
# curl python` preflight was the only thing catching that, and the shared block
# above replaced it; without this function the removal would be a regression.
json_string() {
  if [ "$JSON_READER" = jq ]; then
    jq -Rs .
  else
    "$JSON_READER" -c 'import json,sys;print(json.dumps(sys.stdin.read()))'
  fi
}

# Deferred from the argument parse above: needs $JSON_READER.
if [[ "$MINE" != "true" ]]; then
  REPO_JSON="$(printf '%s' "$REPO" | json_string | tr -d '\r')"
  # A reader that produced nothing would yield `{"repo":,...}` — malformed JSON
  # that coord answers with a parse error this script would blame on the
  # transport. Fail LOUD and locally instead.
  [[ -n "$REPO_JSON" ]] || {
    echo "$DOOR_SCRIPT_NAME: ERROR: could not JSON-encode --repo with $JSON_READER (LOCAL fault, not a coord verdict)." >&2
    exit 127
  }
  ARGS="$(printf '{"repo":%s,"number":%s}' "$REPO_JSON" "$NUMBER")"
fi


# ----- credential staging -----------------------------------------------------
# Both doors below authenticate with key material (a proxy nonce, then a minted
# acting-user bearer). Neither may travel on curl's argv: process cmdlines are
# world-readable on this multi-session machine, so a credential on argv leaks to
# every peer session. Stage it in a private tempfile and pass `curl -H @file`
# (same rule and shape as scripts/coord-acting-bearer.sh).
HDRFILE="$(mktemp)" || { echo "error: mktemp failed — cannot stage a credential off argv" >&2; exit 3; }
CURLERR="$(mktemp)" || { echo "error: mktemp failed" >&2; exit 3; }
RESPFILE="$(mktemp)" || { echo "error: mktemp failed" >&2; exit 3; }
trap 'rm -f "$HDRFILE" "$CURLERR" "$RESPFILE"' EXIT
# curl's -o needs a spelling the NATIVE curl.exe can open. Under an inherited
# MSYS_NO_PATHCONV=1 (exported by the SSM runbooks this fleet follows) the
# POSIX /tmp path is NOT converted, Windows resolves it against the drive
# root, and the body lands in a different file than the one bash later reads
# — every 200 then reads as a broken door. Same rule as stage_header; the
# 2>"$CURLERR" redirections are opened by bash itself and never need this.
if command -v cygpath >/dev/null 2>&1; then
  RESPPATH="$(cygpath -w "$RESPFILE")" || { echo "error: cygpath failed for the response tempfile" >&2; exit 3; }
else
  RESPPATH="$RESPFILE"
fi

# stage_header <name> <value> -> echoes the path spelling curl can open.
# Git Bash's mktemp yields a POSIX path a native curl.exe cannot open when MSYS
# pathconv is off, so hand curl the Windows spelling when cygpath exists.
#
# Returns non-zero if the header could not be staged. That check is load-bearing
# twice over: `curl -H @<empty file>` does NOT error, it sends the request with
# NO credential — so a silent staging failure would produce an unauthenticated
# POST, a 401, and a "coord unreachable" verdict about a door that is fine. And
# because one file is reused across doors, a failed *open* would otherwise leave
# the PREVIOUS door's nonce in place and send it to the next URL. Truncate
# first, then verify.
# Braces so 2>/dev/null covers the redirection failure itself: redirections are
# applied left to right, so a bare `printf > file 2>/dev/null` still lets the
# shell's "cannot create" reach the console before the suppression takes effect.
stage_header() {
  { : > "$HDRFILE"; } 2>/dev/null || return 1
  { printf '%s: %s\n' "$1" "$2" > "$HDRFILE"; } 2>/dev/null || return 1
  [[ -s "$HDRFILE" ]] || return 1
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$HDRFILE"; else printf '%s' "$HDRFILE"; fi
}

# last_curl_error -> curl's own one-line explanation from the previous call.
# Kept OUT of the console for expected failures but folded into the final error,
# so a local fault (unopenable header file, TLS, DNS) never masquerades as
# "coord unreachable" — the rule coord-revive.sh enforces with typed verdicts.
last_curl_error() { tr -d '\r' < "$CURLERR" 2>/dev/null | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g; s/^ //; s/ $//'; }
FAILNOTE=""
# Dedup: ten stale doors produce ten copies of the same curl message, which
# buries the one distinct note that explains the failure.
note_fail() {
  [[ -n "$1" ]] || return 0
  case ";$FAILNOTE;" in *";$1;"*) return 0 ;; esac
  FAILNOTE="${FAILNOTE}${FAILNOTE:+; }$1"
  return 0
}

emit() { # print the tool result payload from an MCP JSON-RPC response on stdin
  # Goes through $JSON_READER, not bare `python`. A python-only emit would have
  # silently re-imposed the very dependency the shared block above removes:
  # the floor would say "jq is enough" while this function still needed python.
  # MCP content is a list of {type,text}; some mounts return the value directly.
  if [ "$JSON_READER" = jq ]; then
    jq -er '.result as $r
            | if $r == null then empty
              else ((($r.content? // []) | map(select(.type == "text")) | .[0].text?) as $t
                    | if $t != null then $t
                      elif ($r | length) > 0 then ($r | tojson)
                      else empty end)
              end'
  else
    "$JSON_READER" -c 'import json,sys
try:
    d=json.load(sys.stdin)
except Exception:
    sys.exit(3)
r=d.get("result") or {}
for c in (r.get("content") or []):
    if c.get("type")=="text":
        print(c["text"]); sys.exit(0)
if r: print(json.dumps(r)); sys.exit(0)
sys.exit(3)'
  fi
}

# rpc_error_message — prints a JSON-RPC error.message from STDIN; non-zero if
# there is none. Reader-agnostic for the same reason as emit(). Not part of the
# shared block: coord-revive.sh folds this case into HTTP_200_NOT_MCP and has
# no caller for it.
rpc_error_message() {
  if [ "$JSON_READER" = jq ]; then
    jq -er '.error.message // empty'
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
e=d.get("error") or {}
m=e.get("message")
if not m: sys.exit(1)
print(m)'
  fi
}

# call_door <url> — POST $RPC to one door, credential header staged at $HDRPATH.
# Replaces the old `curl -fsS` shape on both doors: `-f` threw the body away on
# any non-2xx, which made the "transport OK, coord refused" branch unreachable
# for a JSON-RPC error carried on a 4xx/5xx, and left HTTP 503 /
# CREDENTIAL_REFRESHING — the ONE verdict coord-revive.sh classifies as
# retry-safe (the proxy is up, deliberately withholding while its device JWT
# refreshes) — indistinguishable from a dead door. Mirrors coord-revive.sh's
# probe: `-sS -o <body> -w '%{http_code}'`, max 2 attempts per door, attempt 2
# ONLY on that one verdict after `sleep 3`, no loops — coord has no auth-path
# rate limiting, so the bound lives here in the client.
# On a usable result: prints the payload and exits 0 (the script is done).
# On a JSON-RPC error from an answering door (ANY status): reports "coord
# refused, transport OK" and exits 1 — never blames the transport for it.
# Otherwise: records a note_fail and returns 1 (the caller walks on).
call_door() {
  local url="$1" unauth="${2:-}" attempt ce code resp errmsg verdict
  for attempt in 1 2; do
    : > "$CURLERR"
    : > "$RESPFILE"
    ce=0
    code="$(curl -sS --connect-timeout 5 -m 20 -X POST "$url" \
          -H "Content-Type: application/json" \
          -H @"$HDRPATH" \
          -o "$RESPPATH" -w '%{http_code}' \
          -d "$RPC" 2>"$CURLERR")" || ce=$?
    resp="$(cat "$RESPFILE" 2>/dev/null)"
    # One typed verdict from the SHARED classifier, replacing this function's
    # own partial copy. What /pr-status gains (asymmetry 1 of the plan): a
    # named 401 cause and named curl-exit causes (7 / 26 / 28), which the
    # inline version folded into an undifferentiated "HTTP $code without a
    # JSON-RPC error body" or a raw curl string — this script reproducing the
    # very client mask it exists to replace. The unauth wording is passed per
    # door, exactly as coord-revive.sh parameterises it.
    verdict="$(classify "$ce" "$code" "$resp" ${unauth:+"$unauth"})"

    # Retry ONLY a retry-safe verdict, and only once. The prefix match (not
    # equality) is required: coord-revive.sh appends a "[curl: …]" suffix to
    # non-LIVE verdicts, and both copies must branch the same way.
    #
    # TIMEOUT joined the set 2026-09-02, in lockstep with coord-revive.sh. It has
    # to: the classifier block above is pinned byte-identical across both carriers
    # by CI check #35, so this script now PRINTS "Often SATURATION rather than a
    # dead door … re-run, or use another door" on a curl exit 28. Leaving the
    # retry out here would have made that verdict a promise this carrier does not
    # keep, and /pr-status sweeps up to 11 doors on the same loaded boxes that
    # produced the false-DEAD in the first place.
    if [[ "$attempt" -eq 1 && ( "$verdict" == CREDENTIAL_REFRESHING* || "$verdict" == TIMEOUT* ) ]]; then
      sleep 3
      continue
    fi

    if [[ "$verdict" == "LIVE" ]]; then
      if OUT="$(printf '%s' "$resp" | emit)"; then printf '%s\n' "$OUT"; exit 0; fi
    fi
    # A withholding proxy is a transient, never a coord refusal — classify
    # BEFORE the refusal parse, whatever shape its body takes.
    if [[ "$verdict" == CREDENTIAL_REFRESHING* ]]; then
      note_fail "$verdict persisted after one 3s retry (HTTP $code)"
      return 1
    fi
    # A parseable JSON-RPC *error* from a door that answered means the
    # transport is HEALTHY and coord declined the call — report that instead
    # of walking the remaining doors and blaming the transport. Reachable for
    # refusals on ANY status code now that the body survives a non-2xx.
    if errmsg="$(printf '%s' "$resp" | rpc_error_message 2>/dev/null)"; then
      echo "error: coord_pr_status refused the call (transport OK via $url, HTTP $code): $errmsg" >&2
      exit 1
    fi
    # Every remaining path is already a NAMED cause from classify() — the
    # invariant the shared block states. Appending curl's own stderr mirrors
    # coord-revive.sh's probe_door(), which does the same for non-LIVE verdicts.
    if [[ "$ce" -ne 0 ]]; then
      local curlerr; curlerr="$(last_curl_error)"
      note_fail "${verdict}${curlerr:+ [curl: $curlerr]}"
    else
      note_fail "$verdict"
    fi
    return 1
  done
}

# ----- 1) runner loopback proxy (injects a live device JWT) -------------------
# Discover the proxy door the way the rest of the fleet does: sweep the
# runner-written `.mcp.json` candidates (own cwd → workspace root → sibling
# repos) and use the first whose coord-mcp entry is proxy-shaped and answers.
# This previously read `$HOME/.qontinui/coord-mcp-proxy-key`, a file NOTHING in
# any repo writes (grep-verified: this reader was its only mention fleet-wide),
# so door 1 was dead by construction and every /pr-status fallback fell through
# to the acting-bearer door — which needs $COORD_AGENT_JWT and is normally unset
# too. Same candidate ORDER as `/gate` Step 2 and coord-revive.sh; the dedup key
# differs deliberately — see the (url, key) note at the loop below.
# Workspace root via `--git-common-dir`, NOT `--show-toplevel`: inside a LINKED
# GIT WORKTREE the latter returns the worktree's own path, whose parent is the
# worktree container (`agent-worktrees/<uuid>`, `.claude/worktrees`) — a
# directory holding no repo `.mcp.json`, so the sweep would probe ZERO doors and
# report "unreachable" while a live door sits at the real root. Sessions run
# under QONTINUI_AGENT_WORKTREE_MODE=1, so that is the common path. Same
# resolution as `/gate` Step 2 and coord-revive.sh (fixed there in PR #161).
ROOT="${QONTINUI_ROOT:-}"
if [[ -z "$ROOT" ]]; then
  for anchor in "$PWD" "$HERE"; do
    GC="$(cd "$anchor" 2>/dev/null && git rev-parse --git-common-dir 2>/dev/null)" || continue
    [[ -n "$GC" ]] || continue
    GC="$(cd "$anchor" 2>/dev/null && cd "$GC" 2>/dev/null && pwd)" || continue
    [[ -n "$GC" ]] || continue
    ROOT="$(dirname "$(dirname "$GC")")"
    break
  done
fi
if [[ -z "$ROOT" || "$ROOT" == "." ]]; then ROOT="$PWD"; fi

MAX_DOORS=10
SEEN=""
DOORS=0
CANDIDATES=0
for CFG in "$PWD/.mcp.json" "$ROOT/.mcp.json" "$ROOT"/*/.mcp.json; do
  [[ -r "$CFG" ]] || continue
  # Feed jq via stdin — bash opens the file, so no path ever crosses to the
  # NATIVE jq. Passing "$CFG" as an argument dies under an inherited
  # MSYS_NO_PATHCONV=1 (POSIX-spelled path, unconverted, unopenable), and the
  # failed command substitution then kills the WHOLE script under `set -e`,
  # exit 2, before a single door is probed. `|| true` keeps a malformed
  # .mcp.json from doing the same — a bad candidate is skipped, not fatal.
  # Reader-agnostic, mirroring coord-revive.sh's read_cfg(). A bare `jq` here
  # was the same silent-empty bug one directory over: with jq absent, `|| true`
  # made URL and KEY empty, EVERY candidate was skipped as "no proxy-shaped
  # coord-mcp entry", and the script reported "probed 0 doors" — a confident
  # nothing-found over doors that were live. It reads as a CONFIG problem.
  #
  # BOTH header shapes. Plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning
  # Phase 2 moves the proxy nonce from the custom `X-Coord-Mcp-Proxy-Key` header
  # into `Authorization: Bearer <nonce>` (a custom header makes the MCP client
  # attach an OAuth provider, so a stale-key 401 escalates into discovery + DCR,
  # which the runner 404s). The server keeps accepting the legacy header, so both
  # shapes coexist on disk indefinitely — configs are rewritten only on session
  # spawn. `Authorization` wins when both are present, mirroring the runner's own
  # precedence; the value is kept VERBATIM (`Bearer ` prefix included) so the
  # replay is byte-identical to what the client sends.
  KEYHDR="X-Coord-Mcp-Proxy-Key"
  if [ "$JSON_READER" = jq ]; then
    URL="$(jq -r '.mcpServers["coord-mcp"].url // ""' < "$CFG" 2>/dev/null || true)"
    KEY="$(jq -r '(.mcpServers["coord-mcp"].headers // {}) as $h
      | if (($h.Authorization // "") | tostring) != "" then $h.Authorization
        else ($h["X-Coord-Mcp-Proxy-Key"] // "") end' < "$CFG" 2>/dev/null || true)"
    if jq -e '((.mcpServers["coord-mcp"].headers.Authorization // "") | tostring) != ""' < "$CFG" >/dev/null 2>&1; then
      KEYHDR="Authorization"
    fi
  else
    _triple="$("$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(); print(); print(); sys.exit(0)
c=((d.get("mcpServers",{}) or {}).get("coord-mcp",{}) or {})
h=(c.get("headers",{}) or {})
authz=h.get("Authorization","") or ""
legacy=h.get("X-Coord-Mcp-Proxy-Key","") or ""
print(c.get("url","") or "")
print(authz or legacy)
print("Authorization" if authz else "X-Coord-Mcp-Proxy-Key")' < "$CFG" 2>/dev/null || true)"
    # `tr -d '\r'` is NOT redundant: a NATIVE Windows python emits CRLF, so a CR
    # surviving into $URL makes curl exit 3 with http_code 000, which classify()
    # reports as UNREACHABLE — VERDICT DEAD over a live door.
    URL="$(printf '%s\n' "$_triple" | sed -n '1p' | tr -d '\r')"
    KEY="$(printf '%s\n' "$_triple" | sed -n '2p' | tr -d '\r')"
    KEYHDR="$(printf '%s\n' "$_triple" | sed -n '3p' | tr -d '\r')"
    [[ -n "$KEYHDR" ]] || KEYHDR="X-Coord-Mcp-Proxy-Key"
  fi
  case "$URL" in *"/coord-mcp"*) ;; *) continue ;; esac
  [[ -n "$KEY" ]] || continue
  # LOOPBACK ONLY. Before this sweep, door 1 was a hard-coded 127.0.0.1 URL;
  # honouring an arbitrary `url` from any .mcp.json under the workspace would
  # let a config that merely contains "/coord-mcp" answer with PR state — and
  # `--mine` resolves author identity server-side from whatever device JWT the
  # answering proxy injects, so a non-local door could render someone else's
  # PRs as yours. The skill's contract is "never claim a status you did not
  # read from the twin"; keep the door local, as it always was.
  # The brackets in the IPv6 literal MUST be escaped: unescaped, `[::1]` is a
  # case-pattern bracket EXPRESSION, which never matches the URL it is meant to
  # allow and does match nonsense like `http://1:9876`.
  case "$URL" in
    http://127.0.0.1:*|http://localhost:*|'http://[::1]:'*) ;;
    *) continue ;;
  esac
  # Dedup on (url, key), not on the config path: the fleet routinely has a dozen
  # configs naming the SAME loopback port with DIFFERENT nonces (one-slot-
  # per-workdir rotation), and exactly one of those keys is live — deduping by
  # URL alone would try one key and give up. Path dedup, by contrast, prevented
  # nothing: the duplicates are distinct files.
  DOORKEY="$URL|$KEY"
  case "$SEEN" in *"|$DOORKEY|"*) continue ;; esac
  SEEN="${SEEN}|$DOORKEY|"
  CANDIDATES=$((CANDIDATES + 1))
  if (( DOORS >= MAX_DOORS )); then
    note_fail "stopped after $MAX_DOORS doors (more candidates exist under $ROOT)"
    break
  fi
  DOORS=$((DOORS + 1))
  HDRPATH="$(stage_header "$KEYHDR" "$KEY")" || {
    note_fail "could not stage the proxy-key header under ${TMPDIR:-/tmp} (LOCAL fault)"
    break
  }
  # --connect-timeout (inside call_door) bounds a firewalled/hung port:
  # without it, ten doors at -m 20 each can outlast the caller's own timeout
  # and look like a hang.
  call_door "$URL" || true
done

# "Every door refused" and "there were no doors" are different failures with
# different fixes, and only the first is about coord. Without this note the
# final line carries the acting-bearer note alone and reads as though the
# loopback half of the cascade was tried and lost, when it never ran at all.
# coord-revive.sh reports the same fact as its DOORS_PROBED count.
if (( CANDIDATES == 0 )); then
  note_fail "no proxy-shaped coord-mcp door found under $ROOT (loopback sweep probed 0 doors — set \$QONTINUI_ROOT, or no runner has written an .mcp.json here)"
fi

# ----- 2) direct coord MCP with an acting-bearer token ------------------------
# $HERE = .claude/skills/pr-status, so the repo root (and its scripts/) is
# three levels up. Fallback: $QONTINUI_ROOT = the workspace dir containing the
# repo checkouts, for copies of this skill installed outside the repo.
#
# The helper's EXIT CODE is the diagnosis and must not be discarded: it is a
# typed contract (0 ok; 2 no agent JWT; 3 coord mint failed; 127 missing
# jq/curl), and coord-revive.sh — the sibling this script mirrors — maps all
# four to named causes. Collapsing them into one "$COORD_AGENT_JWT unset"
# message is the confidently-wrong-verdict failure mode the rest of this
# cascade was built to eliminate: a MISSING HELPER, a coord-side mint refusal
# and a missing `jq` each got reported as an unset env var, sending the reader
# to set a variable that was never the problem.
#
# `-f`, not `-x`: the helper is invoked as `bash <path>`, which needs the file
# readable, not executable. A checkout that dropped the exec bit (routine on
# Windows/MSYS) would otherwise skip an available fallback silently.
BEARER_SH=""
for CAND in "$HERE/../../../scripts/coord-acting-bearer.sh" \
            "${QONTINUI_ROOT:+${QONTINUI_ROOT}/qontinui-claude-config/scripts/coord-acting-bearer.sh}"; do
  [[ -n "$CAND" && -f "$CAND" ]] || continue
  BEARER_SH="$CAND"
  break
done

TOKEN=""
if [[ -z "$BEARER_SH" ]]; then
  note_fail "HELPER_NOT_FOUND (coord-acting-bearer.sh not at the repo-relative path; set \$QONTINUI_ROOT for out-of-repo copies)"
else
  # `|| RC=$?` keeps `set -e` from aborting on the helper's typed non-zero exits.
  RC=0
  TOKEN="$(bash "$BEARER_SH" 2>/dev/null | tr -d '\r\n')" || RC=$?
  if (( RC != 0 )) || [[ -z "$TOKEN" ]]; then
    TOKEN=""
    case "$RC" in
      0)   note_fail "MINT_EMPTY (coord-acting-bearer.sh exited 0 but printed no token)" ;;
      2)   note_fail "NO_TOKEN (\$COORD_AGENT_JWT unset/empty — it is the helper's only credential source)" ;;
      3)   note_fail "MINT_FAILED (coord rejected or never answered the acting-user mint — device unknown / no bound user / coord down)" ;;
      127) note_fail "HELPER_DEPS_MISSING (coord-acting-bearer.sh: curl missing, or no working JSON reader)" ;;
      *)   note_fail "HELPER_FAILED (coord-acting-bearer.sh exit $RC)" ;;
    esac
  fi
fi

if [[ -n "$TOKEN" ]]; then
  if HDRPATH="$(stage_header "Authorization" "Bearer ${TOKEN}")"; then
    # Bearer door: a 401 here means a REJECTED BEARER, not a stale proxy key.
    # Same parameterisation coord-revive.sh uses, so the two agree on cause.
    call_door "${COORD_URL}/mcp" "BEARER_UNAUTHORIZED (coord rejected the bearer)" || true
  else
    note_fail "could not stage the bearer header under ${TMPDIR:-/tmp} (LOCAL fault)"
  fi
fi

# Name WHY, never just "unreachable": the collected notes distinguish a local
# fault of this script's own plumbing from coord actually being unreachable.
echo "error: coord_pr_status unreachable via loopback proxy or acting-bearer — call the native MCP tool from the session instead${FAILNOTE:+ [$FAILNOTE]}" >&2
exit 1
