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
# Worst-case wall-clock: every door gets max 2 attempts (the second ONLY on the
# retry-safe 503/CREDENTIAL_REFRESHING verdict, after a 3s sleep). Doors that
# merely stall never retry, so the pure-stall bound stays 11 doors × -m 20
# ≈ 4 minutes; the ceiling is doors that BOTH take ~20s AND answer 503/CR —
# (10 loopback + 1 acting-bearer) × 2 × 20s + 11 × 3s ≈ 8 minutes. Callers
# wrapping this script in their own timeout must budget for that ceiling.

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
  ARGS="$(printf '{"repo":%s,"number":%s}' "$(printf '%s' "$REPO" | python -c 'import json,sys;print(json.dumps(sys.stdin.read()))')" "$NUMBER")"
else
  echo "error: pass --mine, or both --repo and --number" >&2
  exit 2
fi

RPC="$(printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_pr_status","arguments":%s}}' "$ARGS")"

COORD_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
HERE="$(cd "$(dirname "$0")" && pwd)"

# Fail loudly on a missing dependency. Under `set -e` an absent `jq` would
# otherwise abort the script at its first use with no message at all — a silent
# exit 127 that reads as "coord unreachable". `python` is worse than silent: it
# is used inside a command substitution whose OUTER printf still exits 0, so
# `set -e` never fires and the script sends malformed JSON to coord and reports
# the resulting parse error as a transport failure.
for bin in jq curl python; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: $bin is required by pr-status.sh" >&2
    exit 127
  fi
done

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
  python -c 'import json,sys
try:
    d=json.load(sys.stdin)
except Exception as e:
    sys.exit(3)
r=d.get("result") or {}
# MCP content is a list of {type,text}; the tool returns JSON text
for c in (r.get("content") or []):
    if c.get("type")=="text":
        print(c["text"]); sys.exit(0)
# some mounts return the value directly
if r: print(json.dumps(r)); sys.exit(0)
sys.exit(3)'
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
  local url="$1" attempt ce code resp errmsg
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
    # Non-2xx guard on the marker match: a healthy 200 whose PrStatusCard
    # payload merely CONTAINS the substring (a PR discussing this verdict)
    # must not trigger a spurious retry.
    if [[ "$attempt" -eq 1 && "$code" != 2* ]] && { [[ "$code" == "503" ]] || [[ "$resp" == *CREDENTIAL_REFRESHING* ]]; }; then
      sleep 3
      continue
    fi
    if [[ "$ce" -eq 0 && "$code" == 2* ]]; then
      if OUT="$(printf '%s' "$resp" | emit)"; then printf '%s\n' "$OUT"; exit 0; fi
    fi
    # 503/CREDENTIAL_REFRESHING persisted — classify BEFORE the refusal parse:
    # a withholding proxy is a transient, never a coord refusal, whatever
    # shape its body takes.
    if [[ "$code" != 2* ]] && { [[ "$code" == "503" ]] || [[ "$resp" == *CREDENTIAL_REFRESHING* ]]; }; then
      note_fail "HTTP 503/CREDENTIAL_REFRESHING verdict persisted after one 3s retry (HTTP $code)"
      return 1
    fi
    # A parseable JSON-RPC *error* from a door that answered means the
    # transport is HEALTHY and coord declined the call — report that instead
    # of walking the remaining doors and blaming the transport. Reachable for
    # refusals on ANY status code now that the body survives a non-2xx.
    if errmsg="$(printf '%s' "$resp" | python -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
e=d.get("error") or {}
m=e.get("message")
if not m: sys.exit(1)
print(m)' 2>/dev/null)"; then
      echo "error: coord_pr_status refused the call (transport OK via $url, HTTP $code): $errmsg" >&2
      exit 1
    fi
    if [[ "$ce" -ne 0 ]]; then
      note_fail "$(last_curl_error)"
    elif [[ "$code" == 2* ]]; then
      # 2xx, but neither a usable result nor a JSON-RPC error — a broken door.
      # Record it: falling through silently is how the final line ends up a
      # bare "unreachable" with nothing in the brackets, i.e. the mask again.
      # (coord-revive.sh names this same case HTTP_200_NOT_MCP.)
      note_fail "a door answered $code without a JSON-RPC result (broken door, not a transport failure)"
    else
      note_fail "HTTP $code without a JSON-RPC error body"
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
  URL="$(jq -r '.mcpServers["coord-mcp"].url // ""' < "$CFG" 2>/dev/null || true)"
  # BOTH header shapes. Plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning
  # Phase 2 moves the proxy nonce from the custom `X-Coord-Mcp-Proxy-Key` header
  # into `Authorization: Bearer <nonce>` (a custom header makes the MCP client
  # attach an OAuth provider, so a stale-key 401 escalates into discovery + DCR,
  # which the runner 404s). The server keeps accepting the legacy header, so both
  # shapes coexist on disk indefinitely — configs are rewritten only on session
  # spawn. Reading only the legacy name would empty $KEY on exactly the configs
  # the fix produces and this sweep would silently probe ZERO doors.
  # `Authorization` wins when both are present, mirroring the runner's own
  # precedence; the value is kept VERBATIM (`Bearer ` prefix included) so the
  # replay is byte-identical to what the client sends.
  KEY="$(jq -r '(.mcpServers["coord-mcp"].headers // {}) as $h
    | if (($h.Authorization // "") | tostring) != "" then $h.Authorization
      else ($h["X-Coord-Mcp-Proxy-Key"] // "") end' < "$CFG" 2>/dev/null || true)"
  KEYHDR="X-Coord-Mcp-Proxy-Key"
  if jq -e '((.mcpServers["coord-mcp"].headers.Authorization // "") | tostring) != ""' < "$CFG" >/dev/null 2>&1; then
    KEYHDR="Authorization"
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
    call_door "${COORD_URL}/mcp" || true
  else
    note_fail "could not stage the bearer header under ${TMPDIR:-/tmp} (LOCAL fault)"
  fi
fi

# Name WHY, never just "unreachable": the collected notes distinguish a local
# fault of this script's own plumbing from coord actually being unreachable.
echo "error: coord_pr_status unreachable via loopback proxy or acting-bearer — call the native MCP tool from the session instead${FAILNOTE:+ [$FAILNOTE]}" >&2
exit 1
