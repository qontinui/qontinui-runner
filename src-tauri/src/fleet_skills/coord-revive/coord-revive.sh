#!/usr/bin/env bash
# coord-revive.sh — typed transport triage + recovery for a dead coord-mcp door.
#
# WHEN TO RUN: any coord MCP tool call returned "Command failed with no output"
# (the client-side mask for a dead cached transport), or the coord tools read as
# unknown/masked. Findings 2026-07-26 §3: 8 of 8 prod-adjudicable "no output"
# writes were silently LOST — treat the write as LOST, run this script, re-issue
# the call over the door it reports, then VERIFY BY READ.
#
# Cascade (stop at the first LIVE door; self-bound — max 2 probes per door,
# and the second probe fires ONLY on the one retry-safe verdict; no loops.
# coord has no auth-path rate limiting, so the bounding lives here):
#   L1  own cwd's .mcp.json    — re-read the key: the file may hold a NEWER key
#                                than the session's startup snapshot (the client
#                                reads .mcp.json once at startup, never again)
#   L2  sibling-key sweep      — <workspace-root>/.mcp.json plus every
#                                <workspace-root>/*/.mcp.json (a sibling repo's
#                                config often holds the live key/port)
#   L3  acting-bearer fallback — coord-acting-bearer.sh ($COORD_AGENT_JWT) ->
#                                direct coord MCP over HTTPS
#   L4  device-JWT bearer      — the fleet's documented THREE-source device-JWT
#                                cascade, in order: $COORD_DEVICE_JWT, then
#                                ~/.qontinui/coord-device-jwt, then a mint from
#                                a live runner's UI Bridge — used as a bearer
#                                against the SAME public coord MCP door as L3.
#                                Independent of every proxy key AND of
#                                $COORD_AGENT_JWT, which is unset on this
#                                fleet's sessions.
#
# The two STATIC sources are tried before the mint because they are the fleet's
# canonical order (CLAUDE.md; scripts/render-memory-cache.ps1 implements the
# same three). L4 previously implemented only the third: on a box with no runner
# but a valid $COORD_DEVICE_JWT the cascade printed VERDICT: DEAD over a
# credential sitting in the environment — the same false-DEAD class L4 was
# created to close, reached by a different route. Static tokens are static while
# device JWTs live ~4h, so a 401 from one is EXPECTED and must NOT end the
# cascade: it falls through to the runner mint. Only the mint's 401 is terminal.
#
# Why L4 exists (2026-08-08): every one of the 14 probeable doors answered 401
# (the one-slot workdir key had rotated under all of them) and L3 answered
# NO_TOKEN, so the cascade printed VERDICT: DEAD — while coord was reachable the
# whole time. A device JWT minted from the runner's UI Bridge drove
# withdraw_gate, register_gate and add_citation to completion moments later.
# L3's helper treats $COORD_AGENT_JWT as "the ONLY source", which is true of
# THAT helper and false of the session: the runner holds a device JWT and hands
# it over on request. A false DEAD is the worst output this script can produce
# (SKILL.md sells the DEAD line as honest blocked-evidence), so the rung that
# was actually load-bearing belongs in the cascade.
#
# Every failed probe gets a TYPED verdict instead of the client's mask:
#   COORD_MCP_PROXY_UNAUTHORIZED — stale/evicted proxy key (HTTP 401): the
#                                  one-slot workdir key rotated under you
#   CREDENTIAL_REFRESHING        — proxy up, deliberately withholding while its
#                                  device JWT refreshes (HTTP 503; the ONLY
#                                  retry-safe verdict — earns the second probe)
#   CONNECT_REFUSED              — dead port, no listener (runner gone or moved)
#   TIMEOUT                      — port answers but the request never returns
# plus HTTP_200_NOT_MCP (200 without a JSON-RPC result — treat as dead),
# UNREACHABLE / HTTP_<code>, typed L3 causes (NO_TOKEN / MINT_FAILED /
# HELPER_NOT_FOUND / HELPER_DEPS_MISSING) and typed L4 causes:
#   NO_RUNNER                    — nothing answered at that origin
#   RUNNER_TIMEOUT               — the port answered but never responded; often
#                                  SATURATION, not a dead runner
#   RUNNER_EVAL_FAILED           — it ANSWERED, but not with a well-formed
#                                  evaluate result (non-2xx / route absent /
#                                  success:false). NOT a sign-in problem
#   RUNNER_TIER_TOO_LOW          — the runner is Tier 0/1, where the Qontinui
#                                  account commands do not exist at all
#   RUNNER_TIER_UNKNOWN          — the runner could not resolve its own tier
#                                  (corrupt/unreadable settings.json)
#   RUNNER_SIGNED_OUT            — it answered, and holds no JWT-shaped token
#   ENV_UNSET / FILE_ABSENT      — that static credential source is simply not
#                                  present (a statement of ABSENCE, not a fault)
#   HOME_UNRESOLVED              — neither $HOME nor $USERPROFILE is set, so
#                                  source 2 has no path to read (LOCAL fault)
#   DEVICE_JWT_ENV_MALFORMED     — $COORD_DEVICE_JWT set but not JWT-shaped
#   DEVICE_JWT_FILE_MALFORMED    — ~/.qontinui/coord-device-jwt likewise
#   DEVICE_JWT_UNAUTHORIZED      — coord rejected the device JWT
#
# Why RUNNER_EVAL_FAILED and the two tier verdicts exist (2026-08-13): the
# response reader returns the EMPTY STRING for every body it cannot parse, so an
# evaluate fault ({"success":false,"error":"JS evaluation error: …"}), a
# route-absent 404 (whose body is NON-empty, so the NO_RUNNER fast-path does not
# fire either) and a genuinely signed-out runner all landed on
# RUNNER_SIGNED_OUT — i.e. "sign the runner in", which is useless advice for two
# of the three and actively misleading for the route-absent one. Worse,
# get_access_token_for_websocket_impl calls require_tier_2() as its FIRST
# statement, BEFORE any keychain read (qontinui-runner
# src-tauri/src/commands/auth.rs), so a perfectly healthy Tier-1 runner was told
# to sign in. The runner's own NO-DOWNGRADE (C4) comment documents exactly that
# wrong-CTA mistake internally; this script was reproducing it one layer out.
#
# PARTIAL: a LIVE verdict can be QUALIFIED. `live_exit` takes an optional second
# argument naming a limit on what the winning door can actually DO, and appends
# a ` PARTIAL` marker plus PARTIAL: lines. Only the device-JWT doors use it —
# L1/L2/L3 pass no second argument and print an unqualified LIVE, exactly as
# before. A bare LIVE that overstates its own reach is the same defect the DEAD
# verdicts here are written to avoid, one level down.
#
# LOCAL faults are named as such and never dressed up as a coord verdict —
# AUTH_HEADER_STAGING_FAILED (the header file could not be written) and
# AUTH_HEADER_UNREADABLE (curl could not open it). A diagnostic tool that
# blames the remote for its own broken plumbing is worse than no tool.
#
# READ-ONLY GUARANTEE: this script only READS .mcp.json files — it never writes
# any config. L4's one outbound POST is the runner's UI-Bridge token GETTER
# (`get_access_token_for_websocket`), which returns a credential the runner
# already holds and mutates nothing; it is the same call render-memory-cache.ps1
# makes on every session boot. This script still NEVER calls
# /coord-mcp/provision-session — minting THERE re-provisions the one-slot
# workdir key and evicts the live peer's, which is the exact failure class this
# script exists to recover from. Minting a device JWT is not that: it issues a
# session-independent token and evicts nothing.
#
# Output: probe log on stderr; the final "VERDICT: ..." on stdout.
# Exit: 0 = LIVE door found; 1 = cascade exhausted; 127 = missing curl, or
#       neither jq nor python available to read JSON (both LOCAL faults).

set -u

RPC_LIST='{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
COORD_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
PROBE_CONNECT_TIMEOUT=5
PROBE_TIMEOUT=15
HERE="$(cd "$(dirname "$0")" && pwd)"

if ! command -v curl >/dev/null 2>&1; then
  echo "coord-revive: ERROR: curl is required (LOCAL fault, not a coord verdict)." >&2
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
  echo "coord-revive: ERROR: neither jq nor a working python can read JSON — cannot probe any door (LOCAL fault, not a coord verdict)." >&2
  exit 127
fi

# Named loudly: every probe stages its auth header here, so a silent failure
# would surface later as AUTH_HEADER_STAGING_FAILED with an empty path — the
# symptom without the cause.
TMPD="$(mktemp -d)" || { echo "coord-revive: ERROR: mktemp -d failed — cannot stage probe headers." >&2; exit 127; }
trap 'rm -rf "$TMPD"' EXIT

FAILS=()
DOORS_PROBED=0
LIVE_FILE=""
LIVE_URL=""
SEEN=""

# seen_door <path> — 0 if this door was already probed (canonical-path dedup,
# so a symlinked/re-spelled path can't burn extra probes); records it otherwise.
seen_door() {
  local rp
  rp="$(realpath "$1" 2>/dev/null || printf '%s' "$1")"
  case "$SEEN" in *"|$rp|"*) return 0 ;; esac
  SEEN="${SEEN}|$rp|"
  return 1
}

# classify <curl_exit> <http_code> <body> [unauth-wording] -> typed verdict.
# 401 wording is parameterized because on a loopback proxy it means "stale
# proxy key" while on the direct bearer door it means "rejected bearer".
#
# INVARIANT: every path returns a NAMED cause. A verdict that names nothing is
# this script reproducing the client mask it exists to replace.
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
  if [ "$ce" = "28" ]; then echo "TIMEOUT (no response within ${PROBE_TIMEOUT}s)"; return; fi
  # 26 = "couldn't open/read the local data file", i.e. the auth-header file.
  # This is a LOCAL fault and must never read as a coord verdict — the same
  # rule coord-acting-bearer.sh states for its mint ("a 'Failed to open' here
  # must not masquerade as 'coord down'").
  if [ "$ce" = "26" ]; then
    echo "AUTH_HEADER_UNREADABLE (curl could not open the staged header file - LOCAL fault, says nothing about coord)"; return
  fi
  if [ "$ce" != "0" ] && [ "$code" = "000" ]; then echo "UNREACHABLE (curl exit $ce)"; return; fi
  case "$body" in
    *COORD_MCP_PROXY_CREDENTIAL_REFRESHING*|*CREDENTIAL_REFRESHING*)
      echo "CREDENTIAL_REFRESHING (retry-safe - proxy up, withholding while its device JWT refreshes)"; return ;;
    *COORD_MCP_PROXY_UNAUTHORIZED*)
      echo "$unauth"; return ;;
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

# curl_path <file> — the spelling of $file that CURL can open. Git Bash's
# mktemp hands out a POSIX path (/tmp/…) that a native curl.exe cannot open
# when MSYS pathconv is off (MSYS_NO_PATHCONV=1 sessions); hand it the Windows
# spelling instead. Same treatment, same reason, as the auth header in
# scripts/coord-acting-bearer.sh.
curl_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi
}

# probe_door <label> <name> <url> <header-name> <header-value> [unauth-wording]
# Max 2 attempts; attempt 2 ONLY on CREDENTIAL_REFRESHING. Returns 0 on LIVE.
# The auth header travels via a private tmp file (curl -H @file), never argv —
# keys must not be visible in the machine-wide process list.
probe_door() {
  local label="$1" name="$2" url="$3" hname="$4" hvalue="$5" unauth="${6:-}"
  local attempt code ce verdict curlerr hdrpath bodypath
  local hdrfile="$TMPD/hdr" bodyfile="$TMPD/body" errfile="$TMPD/err"
  DOORS_PROBED=$((DOORS_PROBED + 1))
  # Braces so 2>/dev/null is in effect for the redirection failure itself —
  # `printf > file 2>/dev/null` applies redirections left to right, so the
  # shell's "cannot create" reaches the console before the suppression does.
  { printf '%s: %s\n' "$hname" "$hvalue" > "$hdrfile"; } 2>/dev/null
  # An EMPTY header file is the dangerous failure: curl would send the request
  # with no credential, coord would answer 401, and classify() would call that
  # a stale/evicted proxy key — a confidently wrong verdict about a door that
  # is fine. Catch the local fault here instead, and never probe unauthenticated.
  if [ ! -s "$hdrfile" ]; then
    verdict="AUTH_HEADER_STAGING_FAILED (could not write the header file under $TMPD - LOCAL fault, says nothing about coord)"
    echo "$label: $name -> $verdict" >&2
    FAILS+=("$label $name: $verdict")
    return 1
  fi
  hdrpath="$(curl_path "$hdrfile")"
  # The BODY file needs the same spelling for the same reason. `-o` is opened by
  # the native curl.exe, not by bash, so under an inherited MSYS_NO_PATHCONV=1
  # the POSIX "$TMPD/body" is passed through unconverted and Windows resolves it
  # against the drive root. Two outcomes, both bad: if that directory does not
  # exist curl dies with exit 23 ("client returned ERROR on write") having
  # written nothing; if it DOES (mktemp -d under /tmp -> D:\tmp, which exists on
  # this fleet) curl exits 0, reports the right http_code, and writes the body to
  # a file bash never reads — silent. Either way the file bash reads is EMPTY, so
  # classify() sees no body for a real HTTP 200 and returns HTTP_200_NOT_MCP
  # ("treat as dead") — a confidently wrong DEAD verdict about a LIVE door,
  # which is the worst output this script can produce (SKILL.md sells the DEAD
  # line as honest blocked-evidence). curl_path existed here already and was
  # applied to the header only; PR #171 fixed the identical omission in
  # pr-status.sh after a live A/B surfaced it.
  bodypath="$(curl_path "$bodyfile")"
  for attempt in 1 2; do
    : > "$bodyfile"
    : > "$errfile"
    # -S keeps curl's own diagnosis (e.g. "Failed to open/read local data")
    # available instead of discarded: it lands in $errfile, off the console for
    # expected failures, and is folded into every non-LIVE verdict below.
    code=$(curl -sS -o "$bodypath" -w '%{http_code}' \
      --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$PROBE_TIMEOUT" \
      -X POST "$url" -H "Content-Type: application/json" \
      -H "@$hdrpath" -d "$RPC_LIST" 2>"$errfile")
    ce=$?
    verdict="$(classify "$ce" "$code" "$(cat "$bodyfile" 2>/dev/null)" ${unauth:+"$unauth"})"
    # UNREACHABLE and the TLS/DNS exits are exactly where a bare exit number is
    # not yet a cause, and the invariant above says every path must name one.
    if [ "$verdict" != "LIVE" ]; then
      curlerr=$(tr -d '\r' < "$errfile" 2>/dev/null | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g; s/^ //; s/ $//')
      [ -n "$curlerr" ] && verdict="$verdict [curl: $curlerr]"
    fi
    if [ "$verdict" = "LIVE" ]; then
      rm -f "$hdrfile"
      echo "$label: $name -> LIVE ($url)" >&2
      LIVE_FILE="$name"
      LIVE_URL="$url"
      return 0
    fi
    echo "$label: $name -> $verdict [probe $attempt/2]" >&2
    case "$verdict" in
      CREDENTIAL_REFRESHING*) if [ "$attempt" = "1" ]; then sleep 3; continue; fi ;;
    esac
    break
  done
  rm -f "$hdrfile"
  FAILS+=("$label $name: $verdict")
  return 1
}

# read_cfg <file> -> sets CFG_URL/CFG_KEY; 1 unless proxy-shaped coord-mcp entry
# Feed jq via STDIN — bash opens the file, so no path ever crosses to the NATIVE
# jq. Passing "$1" as an ARGUMENT fails under an inherited MSYS_NO_PATHCONV=1:
# the POSIX spelling reaches jq.exe unconverted, jq exits 2 "Could not open
# file", both vars come back EMPTY, and every candidate is then rejected as "no
# proxy-shaped coord-mcp entry". L1 and L2 probe ZERO doors and the cascade
# prints VERDICT: DEAD with a live door sitting there — the same false DEAD the
# --show-toplevel bug caused (check #6), reached by a different route. PR #171
# fixed this exact idiom in pr-status.sh's sweep.
#
# BOTH arms take the config on STDIN. The reason is the MSYS_NO_PATHCONV note
# above — a POSIX path passed as an ARGUMENT reaches a NATIVE jq.exe/python.exe
# unconverted and the open fails, emptying both vars for every candidate. It is
# NOT about keeping the key off argv: the key is this function's OUTPUT and
# leaves on stdout either way. Do not "simplify" back to a path argument.
#
# The python arm reads the file ONCE and prints url and key on two lines: this
# runs when the operator is already blocked, and L2 sweeps every sibling repo
# (~14 here), so one interpreter start per candidate instead of two halves the
# wait.
# BOTH header shapes are accepted, and the NAME the key was found under is
# returned in $CFG_KEY_HEADER so probe_door replays the config verbatim.
# Plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning Phase 2 moves the
# proxy nonce out of the custom `X-Coord-Mcp-Proxy-Key` header and into
# `Authorization: Bearer <nonce>` — because a custom header makes the MCP client
# attach an OAuth auth provider, so a stale-key 401 ESCALATES into discovery and
# then Dynamic Client Registration, which the runner 404s. The server keeps
# accepting the legacy header, so BOTH shapes exist on disk simultaneously and
# forever: configs are rewritten only on session spawn, never periodically.
# Reading only the legacy name would empty $CFG_KEY on exactly the configs the
# fix produces, and every candidate would then be rejected as "no proxy-shaped
# coord-mcp entry" — VERDICT: DEAD over a live door, the same false DEAD the
# MSYS_NO_PATHCONV bug caused, reached by a third route.
#
# `Authorization` WINS when both are present, mirroring the runner's own
# precedence in `coord_mcp_proxy_handler`, so the script probes with whatever the
# server would actually honour. The value is kept VERBATIM (the `Bearer ` prefix
# included) precisely so the replay is byte-identical to what the client sends.
read_cfg() {
  CFG_KEY_HEADER="X-Coord-Mcp-Proxy-Key"
  if [ "$JSON_READER" = jq ]; then
    CFG_URL=$(jq -r '.mcpServers["coord-mcp"].url // ""' < "$1" 2>/dev/null)
    CFG_KEY=$(jq -r '(.mcpServers["coord-mcp"].headers // {}) as $h
      | if (($h.Authorization // "") | tostring) != "" then $h.Authorization
        else ($h["X-Coord-Mcp-Proxy-Key"] // "") end' < "$1" 2>/dev/null)
    if [ -n "$CFG_KEY" ] && jq -e '((.mcpServers["coord-mcp"].headers.Authorization // "") | tostring) != ""' < "$1" >/dev/null 2>&1; then
      CFG_KEY_HEADER="Authorization"
    fi
  else
    local _pair
    _pair=$("$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(); print(); print(); sys.exit(0)
c=((d.get("mcpServers",{}) or {}).get("coord-mcp",{}) or {})
h=(c.get("headers",{}) or {})
authz=h.get("Authorization","") or ""
legacy=h.get("X-Coord-Mcp-Proxy-Key","") or ""
print(c.get("url","") or "")
print(authz or legacy)
print("Authorization" if authz else "X-Coord-Mcp-Proxy-Key")' < "$1" 2>/dev/null)
    # `tr -d '\r'` is NOT redundant. The reader here is a NATIVE Windows python
    # whose text-mode stdout emits CRLF, so $_pair is literally "URL\r\nKEY":
    # `$(...)` strips only the TRAILING CRLF, leaving a CR at the end of line 1.
    # MSYS GNU sed happens to strip it, so this reads fine today — but `head -1`,
    # `IFS= read -r`, `cut` and `mapfile` do NOT, and any of those is a natural
    # "simplification" of the two lines below. A CR surviving into CFG_URL makes
    # curl exit 3 with http_code 000, which classify reports as UNREACHABLE —
    # i.e. VERDICT: DEAD over a live door, the exact false-DEAD this fix exists
    # to kill, and invisible on Linux where python emits bare LF. Strip it here
    # rather than depending on which line-tool a future edit reaches for.
    CFG_URL=$(printf '%s\n' "$_pair" | sed -n '1p' | tr -d '\r')
    CFG_KEY=$(printf '%s\n' "$_pair" | sed -n '2p' | tr -d '\r')
    CFG_KEY_HEADER=$(printf '%s\n' "$_pair" | sed -n '3p' | tr -d '\r')
    [ -n "$CFG_KEY_HEADER" ] || CFG_KEY_HEADER="X-Coord-Mcp-Proxy-Key"
  fi
  case "$CFG_URL" in *"/coord-mcp"*) ;; *) return 1 ;; esac
  [ -n "$CFG_KEY" ] || return 1
}

# cfg_shape <file> -> a precise one-line reason this file is not a probeable door.
#
# "missing/unreadable/other shape" collapsed three very different states into one
# message, and the STDIO one is not a defect at all: a session configured as
# {"mcpServers":{"coord":{"command":...}}} reaches coord through a CHILD PROCESS,
# so it exposes no loopback URL for this cascade to probe and never will. Calling
# that "other shape" sends the reader hunting a config bug that does not exist —
# and it hides the thing that actually matters, that such a session's NATIVE
# coord_* tools may be perfectly healthy while every door here is dead (see the
# SCOPE lines on the DEAD verdict).
#
# It also could not tell "no coord-mcp entry" from "entry present, key empty" —
# and the second is the stale/evicted nonce, the single most common condition
# this whole tool exists to name.
#
# Both readers emit a TOKEN and the wording lives in ONE place below, so the jq
# and python paths cannot drift apart the way two copies of a message always
# eventually do. The `type == "object"` guards are load-bearing, not defensive
# noise: on a half-written .mcp.json a non-object server value makes jq's
# `select(.command)` throw a RUNTIME error, and `jq -e` surfaces that as just
# another non-zero exit — so without the guard one malformed entry poisons the
# whole comprehension and a genuinely STDIO-shaped entry sitting after it is
# never seen.
cfg_shape_token() {
  if [ "$JSON_READER" = jq ]; then
    jq -r '
      def srv: (.mcpServers // {});
      if (srv | type) != "object" then "none"
      elif ([srv[] | select(type == "object") | select(has("command"))] | length) > 0 then "stdio"
      else
        (srv["coord-mcp"] // {}) as $c
        | (if ($c | type) == "object" then ($c.url // "") else "" end) as $u
        | if (($u | tostring) | contains("/coord-mcp")) then
            (($c.headers // {}) as $h
             | if (($h | type) == "object")
                  and (((($h["X-Coord-Mcp-Proxy-Key"] // "") | tostring) != "")
                       or ((($h.Authorization // "") | tostring) != ""))
               then "complete" else "stalekey" end)
          else "none" end
      end' < "$1" 2>/dev/null || echo badjson
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print("badjson"); sys.exit(0)
srv=d.get("mcpServers")
if not isinstance(srv,dict): print("none"); sys.exit(0)
if any(isinstance(v,dict) and v.get("command") for v in srv.values()): print("stdio"); sys.exit(0)
c=srv.get("coord-mcp"); c=c if isinstance(c,dict) else {}
if "/coord-mcp" in (c.get("url") or ""):
    h=c.get("headers"); h=h if isinstance(h,dict) else {}
    print("complete" if ((h.get("X-Coord-Mcp-Proxy-Key") or "") or (h.get("Authorization") or "")) else "stalekey")
else:
    print("none")' < "$1" 2>/dev/null | tr -d '\r' || echo badjson
  fi
}

cfg_shape() {
  local tok
  [ -r "$1" ] || { echo "missing or unreadable"; return; }
  tok="$(cfg_shape_token "$1")"
  case "$tok" in
    stdio)
      echo "STDIO-shaped coord entry (command/args) - reaches coord via a child process, exposing no loopback URL this cascade can probe. NOT a defect, and says nothing about your native coord_* tools." ;;
    stalekey)
      echo "proxy-shaped coord-mcp entry IS present and its URL is fine, but it carries NEITHER an X-Coord-Mcp-Proxy-Key header NOR an Authorization header (stale/evicted key?)" ;;
    complete)
      # Unreachable from the cascade (cfg_shape only runs once read_cfg has
      # already rejected the file), but a diagnostic must never assert a cause it
      # has not tested — that is the habit this function exists to break.
      echo "proxy-shaped coord-mcp entry looks complete (URL + key both present) - if it was still rejected, read the per-door probe verdicts above for the real cause" ;;
    badjson)
      echo "not valid JSON" ;;
    *)
      echo "no proxy-shaped coord-mcp entry (needs .mcpServers[\"coord-mcp\"].url plus either an X-Coord-Mcp-Proxy-Key or an Authorization header)" ;;
  esac
}

# rpc_has_result — reads a JSON-RPC body on STDIN; 0 iff it carries a real
# `result` and no `error`. A JSON-RPC surface answers HTTP 200 even for in-band
# errors, so LIVE requires the result object itself, or the caller re-issues its
# lost write into a broken door.
rpc_has_result() {
  if [ "$JSON_READER" = jq ]; then
    jq -e '(.error == null) and (.result != null)' >/dev/null 2>&1
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
sys.exit(0 if (d.get("error") is None and d.get("result") is not None) else 1)' >/dev/null 2>&1
  fi
}

# live_exit <transport> [partial-block]
#
# The optional second argument is a pre-formatted block of `PARTIAL: ` lines
# naming what this door CANNOT do. When present the verdict line gains a
# trailing ` PARTIAL` token and the block is printed under it. L1/L2/L3 pass no
# second argument, so their output is byte-identical to before.
#
# INVARIANT (the reach half of the named-cause rule on classify()): a qualified
# LIVE must name its limit. `LIVE … PARTIAL` with no reason would be the mask
# this script exists to replace, wearing a success label.
live_exit() {
  local transport="$1" partial="${2:-}"
  if [ -n "$partial" ]; then
    echo "VERDICT: LIVE door=$LIVE_FILE url=$LIVE_URL transport=$transport PARTIAL"
    echo "$partial"
  else
    echo "VERDICT: LIVE door=$LIVE_FILE url=$LIVE_URL transport=$transport"
  fi
  echo "Re-issue the lost call over this door, then VERIFY BY READ (a \"no output\" write is presumed LOST - findings 2026-07-26 section 3)."
  exit 0
}

# The runner-mint door's PARTIAL block. Every clause here was MEASURED over a
# live L4 door on 2026-08-13; do not soften it into a hypothetical.
#
# Terminology matters and is the whole reason the caveat lands:
# get_access_token_for_websocket returns the runner's COGNITO ACCESS TOKEN
# (qontinui-runner src-tauri/src/commands/auth.rs -> AuthManager::get_access_token),
# NOT a coord-issued device JWT. coord accepts it as a bearer, but it
# authenticates as the OPERATOR's own Cognito user and tenant rather than as a
# fleet service identity — which is exactly why the fleet's canonical_repos
# authority rows are absent: they are not this tenant's rows. The door is real;
# the IDENTITY is different.
PARTIAL_RUNNER_MINT="PARTIAL: this door authenticates as the OPERATOR's own Cognito user/tenant, NOT as a fleet service identity. get_access_token_for_websocket hands back the runner's Cognito ACCESS TOKEN, not a coord-issued device JWT.
PARTIAL: so TENANT-SCOPED AUTHORITY reads come back VACUOUS over it. Measured 2026-08-13: coord_query_merge_economics answered \"qontinui-<repo> is not in your tenant's coord authority (canonical_repos tenant/global rows union tenant_repos) - no economics computed\" for ALL SIX fleet repos.
PARTIAL: PATH-KEYED reads work normally over the same door - coord_pr_status, and POST /pr-merge/prs/<owner>/<repo>/<n>/reevaluate returned refreshed_from_github: true.
PARTIAL: a vacuous or empty authority answer over THIS door is UNKNOWN, NEVER ZERO. An agent that reads \"no economics computed\" as \"no merge activity\" draws exactly the wrong conclusion (same rule as served policy verification-and-evidence silent-empty-is-unknown). Re-ask over a door with fleet authority, or say UNKNOWN."

# The STATIC-source PARTIAL block. Deliberately weaker than the one above and it
# SAYS SO: a token in $COORD_DEVICE_JWT or ~/.qontinui/coord-device-jwt may be a
# genuine coord-issued device JWT with fleet authority, or a copy of the same
# operator-tenant Cognito token — this script cannot tell from the bearer alone,
# and asserting either would be a cause it has not tested.
PARTIAL_STATIC_JWT="PARTIAL: this is a STATIC bearer of unverified identity - it may be a coord-issued device JWT with fleet authority, or an operator-tenant token like the runner mint's. This script cannot tell which from the bearer alone and does not guess.
PARTIAL: so before trusting any TENANT-SCOPED AUTHORITY read over it (coord_query_merge_economics and friends), check the answer itself. A \"not in your tenant's coord authority\" or empty reply is UNKNOWN, NEVER ZERO - never read it as \"no merge activity\".
PARTIAL: PATH-KEYED reads (coord_pr_status, /pr-merge/.../reevaluate) are unaffected."

# ----- runner-origin bookkeeping (feeds L4) -----------------------------------
# Every proxy-shaped .mcp.json names a RUNNER: the proxy is served BY the runner
# on its own HTTP API, so `http://127.0.0.1:9877/coord-mcp` tells us a runner
# answers at `http://127.0.0.1:9877`. Collecting origins here means L4 finds a
# runner that has moved off the default port without any configuration — the
# same reason L2 sweeps siblings rather than assuming one port.
RUNNER_ORIGINS=""
note_origin() {
  case "$1" in http://*|https://*) ;; *) return 0 ;; esac
  local o="${1%/coord-mcp*}"
  case " $RUNNER_ORIGINS " in *" $o "*) return 0 ;; esac
  RUNNER_ORIGINS="$RUNNER_ORIGINS $o"
}

# read_minted_jwt: stdin = the UI-Bridge evaluate response, stdout = the token.
# Same dual-reader discipline as read_cfg, and the same CR strip: a NATIVE
# Windows jq/python emits CRLF, and a CR surviving into an Authorization header
# makes curl exit 3 with http_code 000, which classify() reports as UNREACHABLE
# — a false DEAD over a live door, invisible on Linux.
# `.data.value` is the LIVE shape and is read FIRST; `.data.result.value` is kept
# only as a fallback. The runner unwraps the frontend's `result` envelope before
# it reaches HTTP (qontinui-runner `ui_bridge/page.rs` ->
# `Ok(resp.result.unwrap_or(...))`), so a healthy runner answers
# {"success":true,"data":{"value":"<jwt>","type":"scalar"}} with NO `result` key.
# Reading `result` alone returned "" from a runner that was holding a valid
# token, which classify() then reported as a DEAD door — the false-DEAD this
# function's own CR-strip comment above exists to prevent, reached by a
# different route.
read_minted_jwt() {
  if [ "$JSON_READER" = jq ]; then
    jq -r '[(.data? | objects | .value?), (.data? | objects | .result? | objects | .value?)]
           | map(select(type == "string" and . != "")) | (.[0] // "")' 2>/dev/null
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(); sys.exit(0)
data=d.get("data") or {}
if not isinstance(data,dict): data={}
res=data.get("result") or {}
if not isinstance(res,dict): res={}
for v in (data.get("value"), res.get("value")):
    if isinstance(v,str) and v: print(v); sys.exit(0)
print()' 2>/dev/null
  fi
}

# read_eval_error: stdin = the UI-Bridge evaluate response, stdout = the ERROR
# string it carried, or empty. This is the missing half of read_minted_jwt:
# that function returns "" for "no token", "no answer" and "wrong route" alike,
# so `[ -z "$MJWT" ]` at the call site cannot tell a signed-out runner from a
# broken one — an empty string reads as a VALUE. (Same class as
# reference_missing_schema_object_swallow_arm_hides_wrong_column_forever.)
#
# It reads ONLY error-carrying fields and NEVER either token path
# (`.data.value` or `.data.result.value`), so no token can escape through this
# path even if the runner ever put one in an error string.
read_eval_error() {
  if [ "$JSON_READER" = jq ]; then
    jq -r '[.error?, (.data? | objects | .error?), (.data? | objects | .result? | objects | .error?), .message?]
           | map(select(type == "string" and . != "")) | (.[0] // "")' 2>/dev/null
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(); sys.exit(0)
def g(o,*ks):
    for k in ks:
        if not isinstance(o,dict): return ""
        o=o.get(k)
    return o if isinstance(o,str) else ""
for v in (g(d,"error"), g(d,"data","error"), g(d,"data","result","error"), g(d,"message")):
    if v:
        print(v); sys.exit(0)
print()' 2>/dev/null
  fi
}

# one_line <max> — collapse a captured string to a single trimmed line, capped.
# Used on curl's stderr and on evaluate error strings so a multi-line payload
# cannot break the one-verdict-per-line contract the log format depends on.
#
# The trim must apply in BOTH modes. `read_eval_error` prints a bare newline
# when there is no error, and `tr '\n' ' '` turns that into a single SPACE that
# `$(...)` does NOT strip — so a caller that skips the trim sees a non-empty
# string, takes the "there WAS an error" branch, and prints an empty error to
# show. That regression was live for exactly one edit here; the emptiness test
# and the display string must come from the SAME trimmed value.
one_line() {
  local out
  out="$(tr -d '\r' | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g; s/^ //; s/ $//')"
  if [ -n "${1:-}" ]; then printf '%s' "$out" | head -c "$1"; else printf '%s' "$out"; fi
}

# jwt_shaped <token> — 0 iff the token is 3 dot-separated base64url parts.
# Factored out of L4's inline check so the static sources and the runner mint
# share ONE shape test; two copies of a credential check drift.
#
# The CHARACTER-SET half is not pedantry. A dot count alone accepts
# `{"token":"a.b.c"}` — a plausible thing to find in ~/.qontinui/coord-device-jwt
# if a mint script ever wrote the whole response — which would then be SENT, draw
# a 401, and be reported as DEVICE_JWT_UNAUTHORIZED: a coord verdict for a purely
# local malformation, the exact thing this check exists to prevent.
jwt_shaped() {
  case "$1" in
    "" | *[!A-Za-z0-9._-]* ) return 1 ;;
  esac
  [ "$(printf '%s' "$1" | tr -cd '.' | wc -c | tr -d '[:space:]')" = "2" ]
}

# ----- Spawn-time breadcrumb: the runner's OWN reason, when it left one --------
# `.coord-mcp-status` is a one-line breadcrumb the RUNNER's coord-mcp
# provisioning drops into the workdir it is provisioning (for an agent session,
# that agent's PRIMARY worktree) when that provisioning went DEGRADED —
# `qontinui-runner/src-tauri/src/coord_mcp.rs`, `write_degraded_breadcrumb`.
# This script only READS it; /gate, /policy and `qontinui-pr` read it too; none
# of them writes it.
#
# It is read here because four of its five reasons say THAT provisioning pass
# wrote no `.mcp.json` (no device JWT in the runner's access_token slot; an
# unresolvable bound API port, device or agent path; an agent JWT with no
# `sub`). In those cases L1 finding nothing in your own cwd is not a second
# mystery — it is the documented consequence of a fault the runner already
# diagnosed. Without this line the cascade reports the SYMPTOM while the CAUSE
# sits one directory read away.
#
# NOT "the session never had a .mcp.json": `coord_mcp_safe_to_write` passes a
# workdir whose file is absent OR holds solely our own coord-mcp config, and the
# refusal arms return AFTER that check — so a RE-provision of a workdir that
# already carried one leaves the stale file in place. L1 probing a stale port or
# an evicted nonce is fully consistent with a "NOT written" breadcrumb.
#
# It NEVER changes the verdict. Three limits, all stated in the output rather
# than left to the reader:
#   - PRESENCE is spawn-time. The runner clears it (`clear_degraded_breadcrumb`)
#     only on a successful probe DURING provisioning, so a session that
#     recovered on its own keeps a stale file. (A re-provision of the same
#     workdir — a second terminal, a looping agent — can clear or rewrite it.)
#   - ABSENCE is UNKNOWN, never health. A healthy provision writes nothing, and
#     so does a hand-typed session, a workdir the runner never provisioned, a
#     bearer whose `sub_type` is neither device nor agent, a secondary-instance
#     write refusal, and a build predating the breadcrumb. Reporting an absent
#     breadcrumb as "coord-mcp was fine at spawn" would be exactly the
#     silent-empty-is-unknown error this script exists to stop making.
#   - SCOPE is the cwd, deliberately. L2 sweeps siblings for `.mcp.json` because
#     ANY live door serves you; a breadcrumb is the opposite — it describes ONE
#     workdir's provisioning, so a sibling's copy is another session's evidence
#     and quoting it here would be the inference this script refuses to make.
#     But the runner writes into the workdir IT provisioned, which on a linked
#     worktree may be the primary checkout rather than your cwd (measured: the
#     worktree copy was gone 29h later while the checkout's survived — plan
#     2026-08-20-worktree-spawn-autonomy-and-trust-preconditions, finding 18),
#     so the absent case says where else to look instead of implying nothing
#     exists anywhere.
CRUMB_FILE="$PWD/.coord-mcp-status"
CRUMB=""
# Capped at READ time: the breadcrumb is one short line, so a stray large file
# here costs a 4KB read rather than a full slurp into a shell variable.
if [ -r "$CRUMB_FILE" ]; then
  CRUMB="$(head -c 4096 < "$CRUMB_FILE" 2>/dev/null | one_line 400)"
fi
if [ -n "$CRUMB" ]; then
  echo "BREADCRUMB: $CRUMB_FILE (runner, spawn-time): $CRUMB" >&2
else
  echo "BREADCRUMB: none in this cwd ($CRUMB_FILE: absent, unreadable or empty) - UNKNOWN, not health. A healthy provision writes nothing, and so does a workdir the runner never provisioned. The runner writes into the workdir IT provisioned, which on a linked worktree may be the primary checkout rather than here." >&2
fi

# ----- L1: re-read OWN cwd's .mcp.json ----------------------------------------
OWN="$PWD/.mcp.json"
if [ -r "$OWN" ] && read_cfg "$OWN"; then
  seen_door "$OWN"
  note_origin "$CFG_URL"
  probe_door "L1" "$OWN" "$CFG_URL" "$CFG_KEY_HEADER" "$CFG_KEY" && live_exit "loopback-proxy"
else
  L1WHY="$(cfg_shape "$OWN")"
  echo "L1: $OWN -> $L1WHY" >&2
  FAILS+=("L1 $OWN: $L1WHY")
fi

# ----- L2: sibling-key sweep (workspace root + every child repo) --------------
# Workspace root = the directory containing the repo checkouts. $QONTINUI_ROOT
# overrides; otherwise the parent of the MAIN checkout, derived from
# `--git-common-dir`. Same resolution as the /gate Step-2 sweep.
#
# NOT `--show-toplevel`: in a LINKED GIT WORKTREE that returns the worktree's
# own path, so its parent is the worktree container (`.claude/worktrees`,
# `agent-worktrees/<uuid>`, `qontinui-worktrees/<uuid>`) — a directory that
# holds no repo `.mcp.json` at all. The sweep then probes ZERO doors and the
# cascade reports a FALSE DEAD while a live door sits at the real workspace
# root. That is the worst failure this script can produce (SKILL.md sells the
# DEAD line as honest blocked-evidence), and it was the DEFAULT path on this
# fleet: sessions run under QONTINUI_AGENT_WORKTREE_MODE=1. `--git-common-dir`
# points at the MAIN repo's .git from a worktree and from the canonical
# checkout alike, so dirname-twice yields the workspace root in both.
resolve_root() {
  local gc anchor
  if [ -n "${QONTINUI_ROOT:-}" ]; then printf '%s' "$QONTINUI_ROOT"; return; fi
  # Anchor on $PWD first (the caller's real repo), then on this script's own
  # location, so a run from a non-repo cwd still finds the config repo's
  # workspace instead of silently sweeping an unrelated directory.
  # `--path-format=absolute` (git >= 2.31) rather than absolutising a relative
  # `.git` with `cd "$gc" && pwd`: when that cd fails the substitution is EMPTY
  # and dirname-twice collapses to `.`, i.e. a plausible-looking wrong root
  # instead of a detectable failure. One less way for this cascade to sweep the
  # wrong directory and print a false DEAD.
  for anchor in "$PWD" "$HERE"; do
    gc="$(cd "$anchor" 2>/dev/null && git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || continue
    case "$gc" in ""|"."|"..") continue ;; esac
    printf '%s' "$(dirname "$(dirname "$gc")")"
    return
  done
  printf ''
}
ROOT="$(resolve_root)"
if [ -z "$ROOT" ] || [ "$ROOT" = "." ]; then
  echo "L2: not inside a git checkout — assuming \$PWD is the workspace root (set \$QONTINUI_ROOT to override)" >&2
  ROOT="$PWD"
fi
for f in "$ROOT/.mcp.json" "$ROOT"/*/.mcp.json; do
  [ -r "$f" ] || continue
  seen_door "$f" && continue # canonical-path dedup: L1 (or an earlier glob hit) already probed it
  read_cfg "$f" || continue
  note_origin "$CFG_URL"
  probe_door "L2" "$f" "$CFG_URL" "$CFG_KEY_HEADER" "$CFG_KEY" && live_exit "loopback-proxy"
done

# ----- L3: acting-bearer fallback (direct coord MCP over HTTPS) ---------------
# $HERE = .claude/skills/coord-revive, so the repo's scripts/ dir is three
# levels up; $QONTINUI_ROOT covers copies installed outside the repo. The
# helper's stderr flows through (it names the credential source, never the
# token) and its exit code is mapped to a typed cause.
AB=""
if [ -f "$HERE/../../../scripts/coord-acting-bearer.sh" ]; then
  AB="$HERE/../../../scripts/coord-acting-bearer.sh"
elif [ -n "${QONTINUI_ROOT:-}" ] \
     && [ -f "${QONTINUI_ROOT}/qontinui-claude-config/scripts/coord-acting-bearer.sh" ]; then
  AB="${QONTINUI_ROOT}/qontinui-claude-config/scripts/coord-acting-bearer.sh"
fi
if [ -z "$AB" ]; then
  echo "L3: acting-bearer -> HELPER_NOT_FOUND (coord-acting-bearer.sh not at the repo-relative path; set \$QONTINUI_ROOT for out-of-repo copies)" >&2
  FAILS+=("L3 acting-bearer: HELPER_NOT_FOUND (coord-acting-bearer.sh not found)")
else
  OUT="$(bash "$AB")"
  RC=$?
  TOKEN="$(printf '%s' "$OUT" | tr -d '\r\n')"
  if [ "$RC" = "0" ] && [ -n "$TOKEN" ]; then
    probe_door "L3" "acting-bearer" "${COORD_URL}/mcp" "Authorization" "Bearer $TOKEN" \
      "BEARER_UNAUTHORIZED (acting-bearer rejected - mint again or check device binding)" \
      && live_exit "https-acting-bearer"
  else
    case "$RC" in
      2)   L3V="NO_TOKEN (\$COORD_AGENT_JWT unset/empty - it is the helper's only credential source)" ;;
      3)   L3V="MINT_FAILED (coord rejected or never answered the acting-user mint - device unknown / no bound user / coord down)" ;;
      127) L3V="HELPER_DEPS_MISSING (coord-acting-bearer.sh: curl missing, or no working JSON reader)" ;;
      *)   L3V="HELPER_FAILED (coord-acting-bearer.sh exit $RC)" ;;
    esac
    echo "L3: acting-bearer -> $L3V" >&2
    FAILS+=("L3 acting-bearer: $L3V")
  fi
fi

# ----- L4: device-JWT bearer, three sources in the documented order -----------
# Sources 1 and 2 are STATIC; source 3 mints from a live runner. Independent of
# BOTH failure modes above: none of them cares that every proxy key rotated
# (L1/L2), and none needs $COORD_AGENT_JWT (L3).
#
# l4_fail <name> <verdict> — one place that logs and records an L4 cause, so a
# new arm cannot forget half of it (the DEAD block prints FAILS; a verdict that
# only reached stderr is invisible there).
l4_fail() {
  echo "L4: $1 -> $2" >&2
  FAILS+=("L4 $1: $2")
}

# ----- L4 source 1: $COORD_DEVICE_JWT ------------------------------------------
# A stale STATIC token is EXPECTED (they live ~4h and nothing refreshes an env
# var), so its 401 is recorded and the cascade CONTINUES. Exiting here would
# turn a stale credential into a false DEAD.
if [ -n "${COORD_DEVICE_JWT:-}" ]; then
  ENVJWT="$(printf '%s' "$COORD_DEVICE_JWT" | tr -d '[:space:]')"
  if jwt_shaped "$ENVJWT"; then
    probe_door "L4" "device-jwt@\$COORD_DEVICE_JWT" "${COORD_URL}/mcp" "Authorization" "Bearer $ENVJWT" \
      "DEVICE_JWT_UNAUTHORIZED (coord rejected the STATIC \$COORD_DEVICE_JWT - device JWTs live ~4h so a stale env var is the normal cause; NOT terminal, falling through to the next source)" \
      && live_exit "https-device-jwt-env" "$PARTIAL_STATIC_JWT"
  else
    l4_fail "device-jwt@\$COORD_DEVICE_JWT" "DEVICE_JWT_ENV_MALFORMED (\$COORD_DEVICE_JWT is set but is not JWT-shaped - a JWT is 3 dot-separated base64url parts. NOT sent: an unshaped bearer would draw a 401 this script would then report against coord)"
  fi
else
  l4_fail "device-jwt@\$COORD_DEVICE_JWT" "ENV_UNSET (\$COORD_DEVICE_JWT is unset or empty - source 1 of the fleet's three-source device-JWT cascade)"
fi

# ----- L4 source 2: ~/.qontinui/coord-device-jwt -------------------------------
# bash opens the file (no path crosses to a native binary), so no MSYS spelling
# issue here. Whitespace is stripped whole: a JWT contains none, and a trailing
# CR from a Windows-written file becomes an Authorization header curl cannot
# send (exit 3 / http_code 000 -> UNREACHABLE, i.e. a false DEAD).
#
# $USERPROFILE is the documented fallback rather than an improvisation:
# scripts/render-memory-cache.ps1 resolves this same "source 2" from
# $env:USERPROFILE, and the two implementations of one documented cascade must
# not disagree about WHERE source 2 lives. They coincide under Git Bash; they
# need not under every shell. An unresolvable home is named as such rather than
# reported as a missing file — the guard knows the real cause, so discarding it
# would be a named-but-WRONG cause, which the named-cause invariant does not buy.
HOME_DIR="${HOME:-${USERPROFILE:-}}"
STATIC_JWT_FILE="$HOME_DIR/.qontinui/coord-device-jwt"
if [ -z "$HOME_DIR" ]; then
  l4_fail "device-jwt@~/.qontinui/coord-device-jwt" "HOME_UNRESOLVED (neither \$HOME nor \$USERPROFILE is set, so source 2 of the device-JWT cascade has no path to read - a LOCAL environment fault, and it says nothing about whether the credential exists)"
elif [ -r "$STATIC_JWT_FILE" ]; then
  FILEJWT="$(tr -d '[:space:]' < "$STATIC_JWT_FILE" 2>/dev/null)"
  if jwt_shaped "$FILEJWT"; then
    probe_door "L4" "device-jwt@$STATIC_JWT_FILE" "${COORD_URL}/mcp" "Authorization" "Bearer $FILEJWT" \
      "DEVICE_JWT_UNAUTHORIZED (coord rejected the STATIC file token - device JWTs live ~4h so a stale file is the normal cause; NOT terminal, falling through to the runner mint)" \
      && live_exit "https-device-jwt-file" "$PARTIAL_STATIC_JWT"
  else
    l4_fail "device-jwt@$STATIC_JWT_FILE" "DEVICE_JWT_FILE_MALFORMED (the file is readable but its contents are not JWT-shaped - a JWT is 3 dot-separated base64url parts (a whole JSON response left in the file fails here too, by design). NOT sent)"
  fi
else
  l4_fail "device-jwt@$STATIC_JWT_FILE" "FILE_ABSENT (no readable ~/.qontinui/coord-device-jwt - source 2 of the fleet's three-source device-JWT cascade)"
fi

# ----- L4 source 3: runner UI-Bridge mint --------------------------------------
# Origins come from the configs already read, so a runner on a non-default port
# is found without configuration; $QONTINUI_RUNNER_URL overrides, and 9876 is
# the documented default (spelled as the IPv4 loopback deliberately).
MINT_BODY='{"expression":"window.__TAURI__ ? window.__TAURI__.core.invoke(\"get_access_token_for_websocket\") : invoke(\"get_access_token_for_websocket\")","await_promise":true}'
L4_SEEN=""
for origin in ${QONTINUI_RUNNER_URL:-} $RUNNER_ORIGINS http://127.0.0.1:9876; do
  case " $L4_SEEN " in *" $origin "*) continue ;; esac
  L4_SEEN="$L4_SEEN $origin"
  MEVAL_URL="$origin/ui-bridge/control/page/evaluate"
  # `-w '\n%{http_code}'` appends the status to STDOUT rather than using `-o`/
  # `-D` with a temp path: a POSIX temp path handed to the native curl.exe is
  # the check-#9 MSYS trap, and the status is the only extra fact needed. curl's
  # own stderr is kept (not /dev/null'd as it used to be) so every L4 verdict
  # can carry its one-line explanation like every other verdict here.
  : > "$TMPD/merr"
  MRAW="$(curl -sS -w '\n%{http_code}' --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$PROBE_TIMEOUT" \
    -X POST "$MEVAL_URL" \
    -H "Content-Type: application/json" -d "$MINT_BODY" 2>"$TMPD/merr")"
  MCE=$?
  MCURLERR="$(one_line 200 < "$TMPD/merr")"
  MCODE="$(printf '%s' "$MRAW" | tail -n 1 | tr -d '[:space:]')"
  MRESP="$(printf '%s\n' "$MRAW" | sed '$d')"
  MSUFFIX=""
  [ -n "$MCURLERR" ] && MSUFFIX=" [curl: $MCURLERR]"

  # Every mint verdict goes through here so curl's own one-line explanation is
  # attached UNIFORMLY. Appending $MSUFFIX per-arm let an arm silently drop it,
  # and a partial transfer produces BOTH a parseable error string and curl
  # stderr — precisely the case where losing the stderr costs the most.
  mint_fail() { l4_fail "mint@$origin" "$1$MSUFFIX"; }

  # curl's exit code, before anything else: a REFUSED connection and a HUNG one
  # are different faults, and classify() already draws that line for L1/L2/L3.
  # Collapsing them here would tell a saturated runner's operator that it is
  # down — and on this fleet "restart it" is exactly the wrong move (served
  # policy production-and-cost runner-lifecycle; /health has been sampled from
  # 296ms to 10120ms on a loaded box, against this script's 15s bound).
  case "$MCE" in
    7)
      mint_fail "NO_RUNNER (nothing is listening at $origin - runner down, moved to another port, or never started)"
      continue ;;
    28)
      mint_fail "RUNNER_TIMEOUT (the port answered but produced no response within ${PROBE_TIMEOUT}s). Often SATURATION rather than a dead runner - do NOT restart it on this alone; re-run, or use another door"
      continue ;;
  esac

  # No answer at all (or a transfer that never produced a status) is the runner
  # not being there. Distinct from every "it answered" arm below.
  if [ -z "$MRAW" ] || [ -z "$MCODE" ] || [ "$MCODE" = "000" ]; then
    mint_fail "NO_RUNNER (UI Bridge did not answer - runner down, wedged, or on another port)"
    continue
  fi

  # It ANSWERED. Everything below distinguishes HOW it answered, because a
  # single "sign the runner in" for all of it is wrong advice in most of these
  # arms — see the header note on the three-way conflation.
  MJWT="$(printf '%s' "$MRESP" | read_minted_jwt | tr -d '\r\n')"
  # Shape-check BEFORE trusting it. A signed-out runner can answer with a
  # non-token value; sending that as a bearer draws a 401 that classify() would
  # report as coord rejecting a token — a confident verdict about the remote for
  # a fault that is entirely local. Same reason probe_door refuses to probe with
  # an empty header file. The token itself is NEVER echoed, here or anywhere.
  if [ -n "$MJWT" ] && jwt_shaped "$MJWT"; then
    probe_door "L4" "device-jwt@$origin" "${COORD_URL}/mcp" "Authorization" "Bearer $MJWT" \
      "DEVICE_JWT_UNAUTHORIZED (coord rejected the runner-minted token - expired, or bound to another tenant)" \
      && live_exit "https-device-jwt" "$PARTIAL_RUNNER_MINT"
    continue
  fi

  # No usable token. The response's own error string is the only thing that can
  # NAME the cause, so read it — and read it BEFORE looking at the status code.
  # Measured 2026-08-13: this runner returns HTTP 400 for a rejected invoke, so
  # the tier errors arrive on a NON-2xx and a status-first structure would route
  # every one of them into the generic arm and never reach these patterns.
  # Match on the FULL string and truncate only for DISPLAY. Truncating first
  # couples classification to a byte budget: a runner that ever prefixed the
  # tier message with a stack trace would push "Tier 0/1" past the cut and land
  # in the generic arm, silently. (The measured prefix is 21 chars today.)
  MERRFULL="$(printf '%s' "$MRESP" | read_eval_error | one_line)"
  MERR="$(printf '%s' "$MERRFULL" | one_line 200)"
  MHTTP=""
  case "$MCODE" in 2??) ;; *) MHTTP="HTTP $MCODE from $MEVAL_URL: " ;; esac
  case "$MERRFULL" in
    # Both tier strings come from qontinui-runner
    # src-tauri/src/commands/auth.rs (require_tier_2 / require_tier_2_for),
    # which get_access_token_for_websocket_impl calls as its FIRST statement,
    # BEFORE any keychain read — so a healthy Tier-1 runner reaches here without
    # its keychain ever being consulted. Matched on a stable ASCII substring
    # (the real messages contain an em dash and an arrow); a future runner
    # reword lands in the generic arm below, never back on RUNNER_SIGNED_OUT.
    *"Tier 0/1"*)
      mint_fail "RUNNER_TIER_TOO_LOW (${MHTTP}this runner is Tier 0/1 (Local / LocalProvider), where the Qontinui account commands do not exist at all. It is NOT signed out - signing in will not help; change the runner's tier in Settings, or use another door)" ;;
    *"Runner tier could not be determined"*)
      mint_fail "RUNNER_TIER_UNKNOWN (${MHTTP}the runner could not resolve its own tier - a corrupt or unreadable settings.json; its account state is unchanged. Repair settings.json. A sign-in CTA here is precisely the mistake the runner's own NO-DOWNGRADE (C4) comment records)" ;;
    *"Not authenticated"*)
      mint_fail "RUNNER_SIGNED_OUT (${MHTTP}the runner answered and says it holds no tokens: \"$MERR\"). Sign the runner in" ;;
    ?*)
      mint_fail "RUNNER_EVAL_FAILED (${MHTTP}the evaluate call returned no token, and said: \"$MERR\"). Read the quoted error - this is NOT necessarily a sign-in problem" ;;
    *)
      # No token AND no error string. Do not assert a cause not tested: a 4xx is
      # consistent with a route that moved, a 5xx is the route being PRESENT and
      # failing server-side, and sending a reader hunting a renamed route on a
      # 500 is the habit cfg_shape()'s comment says this script exists to break.
      if [ -n "$MJWT" ]; then
        mint_fail "RUNNER_SIGNED_OUT (the UI Bridge returned a value, but it is not a JWT-shaped token - a JWT is 3 dot-separated base64url parts. NOT sent). Sign the runner in"
      else
        case "$MCODE" in
          4??) mint_fail "RUNNER_EVAL_FAILED (HTTP $MCODE from $MEVAL_URL with no error string in the body - the UI-Bridge evaluate route is absent/renamed, or something else answers on this port. NOT a sign-in problem)" ;;
          5??) mint_fail "RUNNER_EVAL_FAILED (HTTP $MCODE from $MEVAL_URL with no error string in the body - the route is PRESENT and failed server-side. Says nothing about the route existing or about your sign-in state)" ;;
          *)   mint_fail "RUNNER_EVAL_FAILED (HTTP $MCODE but the body carried neither a .data.value / .data.result.value nor an error string - the UI-Bridge response shape has changed)" ;;
        esac
      fi ;;
  esac
done

# ----- Honest failure: name the exhausted cascade ------------------------------
echo "VERDICT: DEAD - no OUT-OF-BAND door, $DOORS_PROBED probeable door(s): L1 (own $OWN), L2 (sibling sweep under $ROOT), L3 (acting-bearer), L4 (device JWT: \$COORD_DEVICE_JWT, ~/.qontinui/coord-device-jwt, then the runner mint):"
for f in "${FAILS[@]}"; do
  echo "  - $f"
done
# The runner's own spawn-time reason belongs IN the verdict block, not only in
# the stderr log above it: an agent that pastes this DEAD verdict as its
# blocked-evidence would otherwise drop the one line that names WHY there was no
# door to find. Context only - it never changed the verdict, and it is a fact
# about spawn time, not about now (see the reader near L1).
if [ -n "$CRUMB" ]; then
  echo "BREADCRUMB: the runner recorded a DEGRADED coord-mcp provision for this workdir: $CRUMB"
  echo "  (spawn-time evidence from $CRUMB_FILE, cleared only by a successful probe DURING provisioning - so it can be stale, and it does not describe coord's state now. A 'NOT written' reason means THAT pass wrote no .mcp.json; a stale one from an earlier pass can still be sitting there.)"
else
  echo "BREADCRUMB: none in this cwd ($CRUMB_FILE: absent, unreadable or empty). That is UNKNOWN, not evidence of a healthy provision - the runner writes nothing on the healthy path AND nothing at all for a workdir it never provisioned. This reader looks only in the cwd; the runner writes into the workdir IT provisioned, which on a linked worktree may be the primary checkout."
fi
# SCOPE is not a footnote. L4 closed the missing-DOOR route to a false DEAD; this
# closes the remaining INFERENCE route to one. Every rung above probes a loopback
# proxy or an HTTPS bearer — none touches the session's own coord_* MCP tools,
# which are a different transport. On a stdio-configured session the entire
# .mcp.json proxy family can be dead while the native tools answer normally,
# because they never went through it. Observed 2026-08-08: VERDICT DEAD and a
# successful coord_gate_inspect in the same minute. Without this line the verdict
# reads as "coord is down", which is exactly what the lost-write doctrine keys
# off — so an agent would presume landed writes lost.
echo "SCOPE: this covers ONLY the out-of-band doors above. It does NOT probe your native coord_* MCP tools - a separate transport that can be fully LIVE while every door here is dead (observed 2026-08-08: DEAD alongside a successful coord_gate_inspect in the same minute)."
echo "So BEFORE applying the lost-write doctrine, issue one cheap native coord read (coord_gate_inspect on any known gate_id). If it answers, coord is REACHABLE: re-issue over the native tools and verify by read - do not presume the write lost on this verdict alone."
echo "Next: run 'coord doctor' (runner self-check) to name the failing credential-chain link. Do NOT call /coord-mcp/provision-session - minting evicts the live one-slot workdir key."
exit 1
