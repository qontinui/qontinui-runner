#!/usr/bin/env bash
# coord-provision-nonce.sh - mint a coord-mcp proxy NONCE from the local runner,
# in-process, with NO WebView hop. The shell half of the shared credential door
# (the PowerShell half is scripts/lib/coord-credential.psm1).
#
# WHY THIS EXISTS
# ---------------
# Every shipped credential door in this repo used to mint through
# `POST /ui-bridge/control/page/evaluate`, which bounces the request through the
# runner's WebView. On a HEADLESS runner (`frontendState: "window_missing"`)
# there is no WebView, so that route answers HTTP 500 after a 10s timeout - and
# each door then reported "no credential" for a box whose device credential was
# live the whole time. Confidently wrong is worse than an outage.
#
# `POST <runner>/coord-mcp/provision-session` runs entirely INSIDE the runner
# process. It answers on a headless box, and what it returns is a NONCE, not a
# bearer: a local capability token, paired to this runner's own bound port,
# worthless off-box. The runner injects a freshly-read device JWT per forwarded
# request, so no coord credential and no TTL ever reaches a caller.
#
# Plan: 2026-08-24-headless-box-has-no-working-coord-credential-door (phase 2).
# One helper rather than N copies, because the six doors were byte-similar and
# all six broke identically - the lockstep-duplication risk the plan names.
#
# SAME-USER HANDSHAKE
# -------------------
# `127.0.0.1` is a TCP socket ANY local user can connect to, so "loopback" is
# not a trust boundary. The mint therefore requires a secret read from a 0600
# file the runner rewrites at every start: only the owning user can read it, so
# the handshake reproduces that file's boundary on the socket. The secret is
# staged into a private temp file and handed to curl with `-H @file` - it NEVER
# appears on argv, because process cmdlines are world-readable on this
# multi-session machine.
#
# WHICH file is PER RUNNER, and the runner says which. Every runner keys its
# secret on the port it actually bound (`~/.qontinui/runner-loopback-key-<port>`)
# and publishes that path as `loopback_key_path` in its breadcrumb record under
# `~/.qontinui/runner/api-port*.json` (`"schema": 2`). This script resolves the
# key PER ORIGIN from the record whose `.port` equals the origin's port - never
# by deriving a file name, and never by reading one box-wide file: a second
# runner on the box (a supervisor-spawned temp instance) used to overwrite the
# one shared file and close the primary's mint route for every file-reading
# caller until the operator next restarted it. A `"schema": 1` record (a runner
# built before the per-port key) still means the legacy bare file
# `~/.qontinui/runner-loopback-key`. Plan
# 2026-09-07-the-runner-loopback-handshake-key-is-box-global-and-a-second-runner-clobbers-it
# (Phase 0). The record is matched on its `.port` FIELD, not its file name:
# `api-port.json` is the name a runner uses when it considers itself primary,
# `api-port-<port>.json` otherwise, and which is which depends on the WRITER's
# environment.
#
# USAGE
#   coord-provision-nonce.sh [mint] [--cwd <dir>] [--tenant <uuid>] [--origin <url>]...
#   coord-provision-nonce.sh frontend-state [--origin <url>]...
#   coord-provision-nonce.sh --capabilities
#
# `--capabilities` prints one line, `capabilities=<comma-separated features>`,
# and exits 0 without touching the network. A caller detects an optional
# feature through it instead of reading this file's source: today `tenant`
# (the `--tenant` flag). A helper predating the line has no such mode and exits
# 4 on the unknown flag, which a caller reads as "no optional features".
#
# With no `--origin`, the candidates are $QONTINUI_RUNNER_URL, then
# $QONTINUI_RUNNER_PORT (else $QONTINUI_RUNNER_API_PORT) on the IPv4 loopback,
# then the default 127.0.0.1:9876. With `--origin`, the named origins come FIRST
# in the order given, then $QONTINUI_RUNNER_URL and $QONTINUI_RUNNER_PORT.
# Passing `--origin` SUPPRESSES that default: a caller that names its origins
# means those origins, and appending :9876 behind its back would let this helper
# talk to a runner it never asked for. Pass the default explicitly if you want
# it as a fallback.
#
# `mint` (the default) prints exactly two lines on stdout:
#     url=<the runner's own /coord-mcp url - use it VERBATIM>
#     nonce=<the proxy nonce>
# Use them as `-H "X-Coord-Mcp-Proxy-Key: <nonce>"` against `<url>` (MCP
# JSON-RPC) or `<url>/<route>` (the write forwarder). NEVER re-derive the port
# and never scan for one: the nonce is paired to the bound port, and a scanned
# port 401s.
#
# `--tenant <uuid>` sends `tenant_id` so the runner pins the minted nonce to that
# tenant (plan 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential
# P2). A runner that predates P2 IGNORES the field and mints for the machine's
# tenant, and nothing in its answer says so - a caller that passed a tenant must
# VERIFY the nonce's acting tenant before trusting it (coord-revive.sh does, via
# coord_query_identity). Omitted, the body is `{cwd}` exactly as before.
#
# `frontend-state` prints:
#     origin=<url>
#     frontendReady=<true|false|unknown>
#     frontendState=<string>
#     buildId=<id|unknown>
#     healthMs=<round-trip ms>
# This is what a caller keys its HEADLESS arm on. Key on `frontendReady`, NEVER
# on a timeout string: a desktop runner that is merely slow to boot its WebView
# produces the same timeout as a runner that has none, and only /health can tell
# them apart.
#
# `buildId=` and `healthMs=` come from the SAME /health body and the SAME
# request, so a caller that stamps a claim with them is bound to the read it
# actually made (plan 2026-09-03-capability-floor-claims-carry-their-probe,
# Phase 1; one home for the /health read, D1). `buildId=unknown` means the body
# carried no string `buildId` -- an older runner build -- never a default id.
# `healthMs` is curl's own `time_total`, in whole milliseconds. On the exit-4
# path (no origin answered) a single extra line may still print:
#     healthMs=timeout
# when at least one origin timed out (curl exit 28) rather than refusing: a
# /health that never answered inside the budget is a LOAD signal, and a caller
# reading it as "no runner" would miss the one fact this line carries.
#
# EXIT CODES (mint)
#   0    minted; url= and nonce= are on stdout
#   2    NO_HANDSHAKE_KEY  - no origin had a readable handshake secret: no
#                            runner has published a breadcrumb for that port
#                            (nothing bound it, or its build predates the
#                            record), the record names no key, or the file it
#                            names is unreadable. $QONTINUI_RUNNER_LOOPBACK_KEY
#                            overrides the resolution for every origin.
#   3    REFUSED           - the runner answered with a TYPED refusal; stderr
#                            names the code (..._NOT_OPTED_IN, ..._NO_HANDSHAKE,
#                            ..._HANDSHAKE_MISMATCH, ..._INVALID_BODY,
#                            ..._INVALID_CWD, ..._PORT_UNRESOLVABLE)
#   4    UNKNOWN           - no runner, timeout, or an unrecognised response.
#                            NOT a refusal and NOT an absent credential.
#   5    ROUTE_ABSENT      - 404: this runner's build predates the in-process
#                            mint. (Never restart a running runner over this -
#                            served policy production-and-cost runner-lifecycle.)
#   6    TENANT_REFUSED    - only with --tenant: the runner refused the tenant
#                            (COORD_MCP_PROVISION_INVALID_TENANT 400,
#                            ..._TENANT_NOT_PAIRED 422, ..._TENANT_UNKNOWN 503,
#                            ..._TENANT_REFUSED 422). Nothing was minted; stdout
#                            carries `refusal=<code>` so the caller can type it.
#   127  LOCAL fault       - curl missing, or neither jq nor python available.
#
# EXIT CODES (frontend-state)
#   0 the probe answered (read frontendReady); 4 it did not (UNKNOWN); 127 local.
#
# Diagnostics go to stderr, the answer to stdout. Nothing here ever echoes the
# handshake secret, and the nonce is printed ONLY on stdout for the caller to
# consume - never logged.

set -u

MODE="mint"
CWD_ARG=""
TENANT_ARG=""
EXTRA_ORIGINS=""
HAVE_EXPLICIT_ORIGINS=0

case "${1:-}" in
  --capabilities) echo "capabilities=tenant"; exit 0 ;;
  mint|frontend-state) MODE="$1"; shift ;;
  --*|"") ;;
  *) echo "coord-provision-nonce: unknown mode '$1' (expected mint|frontend-state)" >&2; exit 4 ;;
esac

while [ $# -gt 0 ]; do
  case "$1" in
    --cwd)    CWD_ARG="${2:-}"; shift 2 ;;
    --tenant) TENANT_ARG="${2:-}"; shift 2 ;;
    --origin) EXTRA_ORIGINS="$EXTRA_ORIGINS ${2:-}"; HAVE_EXPLICIT_ORIGINS=1; shift 2 ;;
    *) echo "coord-provision-nonce: unknown argument '$1'" >&2; exit 4 ;;
  esac
done

command -v curl >/dev/null 2>&1 || {
  echo "coord-provision-nonce: ERROR: curl is required (LOCAL fault, not a runner verdict)." >&2
  exit 127
}

CONNECT_TIMEOUT=5
# Sized against the TAIL, not the median. Five /health samples on a box running
# 31 live Claude sessions measured 296ms .. 10120ms; a budget between the two is
# the worst place to sit, because it works until the machine is busy and then
# fails in the way the caller is least able to interpret.
REQ_TIMEOUT=20

TMPD="$(mktemp -d)" || {
  echo "coord-provision-nonce: ERROR: mktemp -d failed - cannot stage the handshake header off argv." >&2
  exit 127
}
trap 'rm -rf "$TMPD"' EXIT

# A POSIX temp path handed to a NATIVE curl.exe under an inherited
# MSYS_NO_PATHCONV=1 is passed through unconverted and resolved against the
# drive root: curl then reads an empty/absent header file and sends the request
# with NO credential, which the runner answers 403 and this script would report
# as a refusal. Spell it the way the native binary expects.
curl_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi
}

# JSON reader: jq if present, else python. Both read from STDIN (or take the
# FILE PATH, which is not key material) so no credential ever crosses to argv.
if command -v jq >/dev/null 2>&1; then
  # `// empty` would be WRONG here and the bug would be invisible: jq's `//`
  # treats `false` as absent, so `.frontendReady // empty` returns nothing for
  # a HEADLESS runner - the one answer this whole helper exists to surface, read
  # as "the field is missing". `select(. != null)` keeps `false` and drops only
  # a genuinely absent key.
  json_get() { jq -r "try ($1) catch empty | select(. != null)" 2>/dev/null; }
elif command -v python3 >/dev/null 2>&1 || command -v python >/dev/null 2>&1; then
  PY="python3"; command -v python3 >/dev/null 2>&1 || PY="python"
  # $1 is a dotted path like `mcpServers.coord-mcp.url`; walk it defensively so
  # a shape change becomes an empty answer the caller NAMES, not a stack trace.
  json_get() {
    "$PY" -c '
import json,sys
path=sys.argv[1].lstrip(".").split(".")
try:
    node=json.load(sys.stdin)
except Exception:
    sys.exit(0)
for key in path:
    if isinstance(node,dict) and key in node:
        node=node[key]
    else:
        sys.exit(0)
if node is None:
    sys.exit(0)
print(node if isinstance(node,str) else json.dumps(node))
' "$(printf '%s' "$1" | tr -d '"[]' )" 2>/dev/null
  }
else
  echo "coord-provision-nonce: ERROR: neither jq nor python can read JSON (LOCAL fault, not a runner verdict)." >&2
  exit 127
fi

# Origin candidates, in preference order. With --origin: the origins the caller
# named (the spawning runner first, then a sibling .mcp.json's origin, say), in
# the caller's order, then $QONTINUI_RUNNER_URL, then $QONTINUI_RUNNER_PORT - no
# $QONTINUI_RUNNER_API_PORT and no default. Without --origin:
# $QONTINUI_RUNNER_URL, then $QONTINUI_RUNNER_PORT (else
# $QONTINUI_RUNNER_API_PORT), then the documented default. ORIGIN_ORDER below is
# the implementation.
#
# 127.0.0.1, never the name: the runner binds the IPv4 loopback ONLY while
# Windows resolves the name to ::1 FIRST, so a call by name pays a doomed IPv6
# connect before the socket that answers (measured 2026-08-03: 127.0.0.1
# 2133ms, [::1] 2057ms fail, the name 4047ms = the sum). Lint check #14.
PORT_ORIGIN=""
# $QONTINUI_RUNNER_PORT; else - ONLY when the caller named no --origin -
# $QONTINUI_RUNNER_API_PORT, the port the SPAWNING runner exports into its
# sessions, so a bare call mints from the session's own runner. A caller that
# names origins (coord-revive.sh passes the spawning runner's origin FIRST)
# means those origins, and an environment variable must not add a runner it
# never asked for.
RUNNER_PORT_VALUE="${QONTINUI_RUNNER_PORT:-}"
if [ -z "$RUNNER_PORT_VALUE" ] && [ "$HAVE_EXPLICIT_ORIGINS" = "0" ]; then
  RUNNER_PORT_VALUE="${QONTINUI_RUNNER_API_PORT:-}"
fi
if [ -n "$RUNNER_PORT_VALUE" ]; then
  case "$RUNNER_PORT_VALUE" in
    ''|*[!0-9]*)
      echo "coord-provision-nonce: ignoring malformed QONTINUI_RUNNER_PORT / QONTINUI_RUNNER_API_PORT (digits only)" >&2 ;;
    *)
      PORT_ORIGIN="http://127.0.0.1:$RUNNER_PORT_VALUE" ;;
  esac
fi
#
# `--origin` SUPPRESSES the default. A caller that names its origins means those
# origins: silently appending :9876 would let this helper talk to a runner the
# caller never asked for - which is a wrong-tenant hazard on a box with several
# runners, and it makes the helper untestable, since every negative case would
# be answered by whatever is really listening. A caller that wants the default
# as a fallback passes it explicitly (coord-revive.sh does).
DEFAULT_ORIGIN="http://127.0.0.1:9876"
[ "$HAVE_EXPLICIT_ORIGINS" = "1" ] && DEFAULT_ORIGIN=""
ORIGINS=""
SEEN=""
# ORDER. With no --origin: $QONTINUI_RUNNER_URL, the port variable, the default.
# With --origin: the caller's origins FIRST, in the caller's order, and only then
# the environment's. coord-revive.sh passes the session's SPAWNING runner first,
# and an ambient $QONTINUI_RUNNER_PORT (or $QONTINUI_RUNNER_URL) naming some other
# runner must not be tried ahead of it - that runner would mint for its own
# tenant (plan 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential).
if [ "$HAVE_EXPLICIT_ORIGINS" = "1" ]; then
  ORIGIN_ORDER="$EXTRA_ORIGINS ${QONTINUI_RUNNER_URL:-} $PORT_ORIGIN"
else
  ORIGIN_ORDER="${QONTINUI_RUNNER_URL:-} $PORT_ORIGIN $DEFAULT_ORIGIN"
fi
for o in $ORIGIN_ORDER; do
  o="${o%/}"
  case "$o" in http://*|https://*) ;; *) continue ;; esac
  case " $SEEN " in *" $o "*) continue ;; esac
  SEEN="$SEEN $o"
  ORIGINS="$ORIGINS $o"
done

one_line() { tr -d '\r' | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g; s/^ //; s/ $//' | cut -c1-300; }

# ---------------------------------------------------------------------------
# frontend-state: the ONE cheap question a door should ask before it spends ten
# seconds on a mint that cannot answer.
# ---------------------------------------------------------------------------
if [ "$MODE" = "frontend-state" ]; then
  HEALTH_TIMED_OUT=""
  for origin in $ORIGINS; do
    # `-w` carries the status AND curl's own round-trip time on one trailing
    # line, so the timing is the request's rather than a shell-side stopwatch
    # around it (which would also count the fork). `time_total` is seconds with
    # a fractional part; it is rendered in whole milliseconds below.
    RAW="$(curl -sS -w '\n%{http_code} %{time_total} %{time_connect}' --connect-timeout "$CONNECT_TIMEOUT" -m "$REQ_TIMEOUT" \
      "$origin/health" 2>"$TMPD/err")"
    CE=$?
    TRAILER="$(printf '%s' "$RAW" | tail -n 1 | tr -d '\r')"
    # Exit 28 also covers a CONNECT that never completed (a filtered port). A
    # load signal is a runner that ACCEPTED and then did not answer, so the
    # timeout counts only when the connect itself completed (time_connect > 0).
    # `NR==1 ... END` so an EMPTY trailer (no `-w` output at all) reads as NOT
    # connected: a main-rule-only awk exits 0 on empty input, which would set
    # the flag in exactly the case it must not.
    if [ "$CE" = "28" ] && printf '%s' "$TRAILER" | awk 'NR == 1 { ok = ($3 ~ /^[0-9.]+$/ && ($3 + 0) > 0) } END { exit !ok }'; then
      HEALTH_TIMED_OUT=1
    fi
    CODE="$(printf '%s' "$TRAILER" | awk '{print $1}')"
    HEALTH_MS="$(printf '%s' "$TRAILER" | awk '{ if ($2 ~ /^[0-9.]+$/) printf "%d", ($2 * 1000) + 0.5; else print "unknown" }')"
    [ -n "$HEALTH_MS" ] || HEALTH_MS="unknown"
    BODY="$(printf '%s\n' "$RAW" | sed '$d')"
    case "$CODE" in
      2??) ;;
      *)
        echo "frontend-state: $origin -> HTTP ${CODE:-000} [$(printf '%s' "$(cat "$TMPD/err" 2>/dev/null)" | one_line)]" >&2
        continue ;;
    esac
    READY="$(printf '%s' "$BODY" | json_get '.frontendReady')"
    STATE="$(printf '%s' "$BODY" | json_get '.frontendState')"
    [ -n "$STATE" ] || STATE="unknown"
    # The runner's buildId, from the SAME body. A missing or non-string field
    # is `unknown` -- a caller stamping a claim with it renders the honest
    # UNKNOWN arm rather than a build it never read.
    BUILD_ID="$(printf '%s' "$BODY" | json_get '.buildId' | tr -d '\r\n' | one_line)"
    case "$BUILD_ID" in
      ""|*[!A-Za-z0-9._-]*) BUILD_ID="unknown" ;;
    esac
    case "$READY" in
      true|false) ;;
      *)
        # It ANSWERED but carried no boolean. UNKNOWN, and it says so rather
        # than defaulting to either verdict.
        READY="unknown"
        echo "frontend-state: $origin answered without a boolean frontendReady - UNKNOWN, not headless" >&2 ;;
    esac
    echo "origin=$origin"
    echo "frontendReady=$READY"
    echo "frontendState=$STATE"
    echo "buildId=$BUILD_ID"
    echo "healthMs=$HEALTH_MS"
    exit 0
  done
  echo "frontend-state: no runner answered /health on any candidate origin ($ORIGINS) - UNKNOWN, not headless" >&2
  # A timeout is not silence: it is the one shape of "no answer" that is
  # evidence of LOAD rather than of absence, and the caller's floor claim
  # renders it as such (`health_ms=timeout`). Printed only when measured.
  [ -n "$HEALTH_TIMED_OUT" ] && echo "healthMs=timeout"
  exit 4
fi

# ---------------------------------------------------------------------------
# mint
# ---------------------------------------------------------------------------
HOME_DIR="${HOME:-${USERPROFILE:-}}"
if [ -z "$HOME_DIR" ]; then
  echo "coord-provision-nonce: NO_HANDSHAKE_KEY (neither \$HOME nor \$USERPROFILE is set, so the 0600 handshake file has no path - a LOCAL environment fault; it says nothing about whether a credential exists)" >&2
  exit 2
fi
BREADCRUMB_DIR="$HOME_DIR/.qontinui/runner"
# The legacy bare file. Read ONLY for a `"schema": 1` breadcrumb - a runner
# built before the per-port key - and deletable, together with the schema-1 arm
# in resolve_loopback_key_file, once no pre-fix runner remains on the fleet.
# Plan 2026-09-07-the-runner-loopback-handshake-key-is-box-global-and-a-second-runner-clobbers-it.
LEGACY_KEY_FILE="$HOME_DIR/.qontinui/runner-loopback-key"

# The port an origin addresses. `http://127.0.0.1:9876` -> 9876; a scheme
# default when none is spelled. Empty (rc 1) for anything that is not digits,
# so a malformed origin reads as "no port" rather than as port 0.
origin_port() {
  local rest="$1" scheme="http" p
  case "$1" in
    https://*) rest="${1#https://}"; scheme="https" ;;
    http://*)  rest="${1#http://}" ;;
  esac
  rest="${rest%%/*}"
  case "$rest" in
    *:*) p="${rest##*:}" ;;
    *)   p=80; [ "$scheme" = "https" ] && p=443 ;;
  esac
  case "$p" in ''|*[!0-9]*) return 1 ;; esac
  printf '%s' "$p"
}

# A path the runner wrote into its record is spelled for the runner's OS. Under
# an MSYS bash that is a `C:\...` Windows path; cygpath makes it one this shell
# can test and read. The mirror image of curl_path above.
shell_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -u "$1" 2>/dev/null || printf '%s' "$1"; else printf '%s' "$1"; fi
}

# resolve_loopback_key_file <port> - find the handshake file for the runner on
# <port>, from that runner's OWN breadcrumb. Sets KEY_FILE (a READABLE,
# non-empty path) and KEY_SOURCE (`breadcrumb-schema2` | `legacy-schema1`) and
# returns 0; or sets KEY_DIAG to the reason and returns 1. Reads nothing but
# records and the candidate file's readability.
#
# The match is on the record's `.port` FIELD. The file NAME encodes which
# runner considered itself primary when it wrote, which depends on the
# writer's environment and says nothing a reader can use.
#
# MORE THAN ONE record can carry the same port by design - `api-port-<p>.json`
# and `api-port.json` are the same port written by runners with different
# QONTINUI_PRIMARY_PORT notions, and a crash leaves its record behind - and glob
# order puts `-` before `.`, so a stale record sorts FIRST. The scan therefore
# never stops on a record it cannot use (null key path, unknown schema, a
# non-absolute path, a file that is absent/empty/unreadable): it records why,
# continues, and returns the first record whose key file is actually readable.
# The recorded reason is reported only when no record was usable.
resolve_loopback_key_file() {
  local port="$1" f rec_port rec_schema rec_key cand src diag
  KEY_FILE=""; KEY_SOURCE=""; KEY_DIAG=""
  for f in "$BREADCRUMB_DIR"/api-port*.json; do
    [ -r "$f" ] || continue
    rec_port="$(json_get '.port' < "$f")"
    [ "$rec_port" = "$port" ] || continue
    rec_schema="$(json_get '.schema' < "$f")"
    cand=""; src=""; diag=""
    case "$rec_schema" in
      2)
        rec_key="$(json_get '.loopback_key_path' < "$f")"
        if [ -z "$rec_key" ]; then
          # The runner bound the port but could not write its key, and its
          # record says so (`loopback_key_path: null`). Its mint route is
          # closed; no file on this box will open it.
          diag="the breadcrumb for port $port ($f) names no handshake file (loopback_key_path is null): that runner could not write its key at start, so its mint route is closed until it next starts"
        else
          case "$rec_key" in
            /*|[A-Za-z]:[\\/]*) cand="$(shell_path "$rec_key")"; src="breadcrumb-schema2" ;;
            # The runner writes an ABSOLUTE path. A relative one would be
            # resolved against this script's cwd, i.e. against nothing the
            # writer meant; refuse it rather than read a wrong file.
            *) diag="the breadcrumb for port $port ($f) names a handshake file that is not an absolute path ('$rec_key'); refusing to resolve it against this shell's cwd" ;;
          esac
        fi ;;
      1)
        # LEGACY ARM - a runner built before the per-port key advertises its
        # secret at the bare box-global path. Deletable with LEGACY_KEY_FILE
        # once no such runner remains. Plan
        # 2026-09-07-the-runner-loopback-handshake-key-is-box-global-and-a-second-runner-clobbers-it.
        cand="$LEGACY_KEY_FILE"; src="legacy-schema1" ;;
      *)
        diag="the breadcrumb for port $port ($f) carries schema '${rec_schema:-<none>}', which this script does not know - it reads 1 (legacy bare file) and 2 (loopback_key_path); a newer runner needs a newer script" ;;
    esac
    if [ -n "$cand" ]; then
      if [ -r "$cand" ] && [ -s "$cand" ]; then
        KEY_FILE="$cand"; KEY_SOURCE="$src"
        return 0
      fi
      diag="the breadcrumb for port $port ($f) resolves the handshake file to $cand [$src], which is absent, empty or unreadable - the runner that wrote the record has exited, or it runs as a different user"
    fi
    # Unusable: remember the FIRST reason (the record glob order presents
    # first is the one a reader would have trusted) and keep scanning.
    [ -n "$KEY_DIAG" ] || KEY_DIAG="$diag"
  done
  [ -n "$KEY_DIAG" ] || KEY_DIAG="no runner has published a breadcrumb for port $port under $BREADCRUMB_DIR - nothing bound that port, or the runner that did was built before the breadcrumb record"
  return 1
}

[ -n "$CWD_ARG" ] || CWD_ARG="$PWD"
# The cwd binds the nonce to this session's workdir - it is how coord attributes
# the session, so it is not decoration. Emit it as JSON with the reader that is
# already present rather than hand-escaping.
# `tenant_id` rides only when --tenant was given, so a caller that names no
# tenant sends the byte-identical `{cwd}` body every runner build understands.
if command -v jq >/dev/null 2>&1; then
  if [ -n "$TENANT_ARG" ]; then
    BODY_JSON="$(jq -nc --arg cwd "$CWD_ARG" --arg t "$TENANT_ARG" '{cwd:$cwd, tenant_id:$t}')"
  else
    BODY_JSON="$(jq -nc --arg cwd "$CWD_ARG" '{cwd:$cwd}')"
  fi
else
  BODY_JSON="$("$PY" -c 'import json,sys
b={"cwd": sys.argv[1]}
if sys.argv[2]: b["tenant_id"]=sys.argv[2]
print(json.dumps(b))' "$CWD_ARG" "$TENANT_ARG")"
fi
if [ -z "$BODY_JSON" ]; then
  echo "coord-provision-nonce: BODY_ENCODE_FAILED (could not encode the cwd as JSON - LOCAL fault)" >&2
  exit 127
fi

# The exit code carried out of the loop. UNKNOWN (4) is the WEAKEST verdict
# here, so `unknown_rc` never overwrites a stronger one: a named cause from one
# origin must survive a later origin that merely failed to answer. Reporting
# UNKNOWN over a ROUTE_ABSENT would discard the only verdict that explains
# itself, which is the whole failure mode this helper exists to remove.
#
# The ranking is 5 > 2 > 4, in EITHER order of origins. ROUTE_ABSENT (5) came
# from a runner that ANSWERED, so it beats NO_HANDSHAKE_KEY (2), which only
# says this box holds no usable record for that port - and 2 is still a named
# local cause, so it beats UNKNOWN (4). Without the rank, a sibling .mcp.json
# naming an exited temp runner (no record -> 2) listed before the primary would
# mask the primary's ROUTE_ABSENT, the one verdict that explains itself. Every
# origin's own line is on stderr regardless of which code is carried out.
#
# The rank governs only the codes that CONTINUE the loop. A typed refusal - 3,
# or 6 for a tenant refusal - exits at once, so it carries out whatever an
# earlier origin had recorded: a ROUTE_ABSENT 5 seen first is reported on
# stderr and then superseded by the later refusal's exit code. That is
# deliberate - a runner that answered about THIS call's credential settles the
# question - but it means exit 6 does not imply the earlier origins were
# healthy.
LAST_RC=4
unknown_rc() { :; }
rc_rank() { case "$1" in 5) echo 3 ;; 2) echo 2 ;; *) echo 1 ;; esac; }
named_rc() { [ "$(rc_rank "$1")" -gt "$(rc_rank "$LAST_RC")" ] && LAST_RC="$1"; }
ORIGIN_N=0
for origin in $ORIGINS; do
  ORIGIN_N=$((ORIGIN_N + 1))

  # The handshake secret, resolved PER ORIGIN from the record of the runner on
  # that port. $QONTINUI_RUNNER_LOOPBACK_KEY is the client-side override of the
  # header VALUE (the runner never reads that variable), and it wins for every
  # origin - a caller that sets it has decided which key to post.
  LOOPBACK_KEY="${QONTINUI_RUNNER_LOOPBACK_KEY:-}"
  KEY_FILE=""; KEY_SOURCE=""; KEY_DIAG=""
  if [ -n "$LOOPBACK_KEY" ]; then
    KEY_FILE="\$QONTINUI_RUNNER_LOOPBACK_KEY"; KEY_SOURCE="override"
  else
    PORT="$(origin_port "$origin")" || PORT=""
    if [ -z "$PORT" ]; then
      echo "coord-provision-nonce: NO_HANDSHAKE_KEY for $origin (no port could be read from that origin, so no runner breadcrumb can be matched to it, and \$QONTINUI_RUNNER_LOOPBACK_KEY is unset). This is a LOCAL fault, NOT 'no credential'." >&2
      named_rc 2; continue
    fi
    if ! resolve_loopback_key_file "$PORT"; then
      echo "coord-provision-nonce: NO_HANDSHAKE_KEY for $origin ($KEY_DIAG; and \$QONTINUI_RUNNER_LOOPBACK_KEY is unset). The runner names its handshake file in that record, so with no usable record there is no key to read. This is a LOCAL fault, NOT 'no credential'." >&2
      named_rc 2; continue
    fi
    LOOPBACK_KEY="$(tr -d '[:space:]' < "$KEY_FILE" 2>/dev/null)"
    if [ -z "$LOOPBACK_KEY" ]; then
      # The resolver saw a readable, non-empty file; a read that still yields
      # nothing is whitespace-only content or a race with a rotation.
      echo "coord-provision-nonce: NO_HANDSHAKE_KEY for $origin (the breadcrumb for port $PORT resolves the handshake file to $KEY_FILE [$KEY_SOURCE], which read as empty; and \$QONTINUI_RUNNER_LOOPBACK_KEY is unset). This is a LOCAL fault, NOT 'no credential'." >&2
      named_rc 2; continue
    fi
  fi

  # Stage the secret off argv - a FRESH header file per origin, because the
  # secret is per origin now. An EMPTY header file is the dangerous failure:
  # curl does not error, it sends the request with NO credential, the runner
  # answers 403 ..._NO_HANDSHAKE, and this script would report a refusal for a
  # LOCAL staging fault. Catch it here instead.
  HDR="$TMPD/hdr-$ORIGIN_N"
  { printf 'X-Qontinui-Loopback-Key: %s\n' "$LOOPBACK_KEY" > "$HDR"; } 2>/dev/null
  if [ ! -s "$HDR" ]; then
    echo "coord-provision-nonce: HANDSHAKE_STAGING_FAILED (could not write the header file under $TMPD - LOCAL fault, not a runner verdict)" >&2
    exit 127
  fi
  HDRP="$(curl_path "$HDR")"

  : > "$TMPD/err"
  RAW="$(curl -sS -w '\n%{http_code}' --connect-timeout "$CONNECT_TIMEOUT" -m "$REQ_TIMEOUT" \
    -X POST "$origin/coord-mcp/provision-session" \
    -H "Content-Type: application/json" -H "@$HDRP" -d "$BODY_JSON" 2>"$TMPD/err")"
  CE=$?
  rm -f "$HDR"
  CURLERR="$(cat "$TMPD/err" 2>/dev/null | one_line)"
  CODE="$(printf '%s' "$RAW" | tail -n 1 | tr -d '[:space:]')"
  BODY="$(printf '%s\n' "$RAW" | sed '$d')"
  SUFFIX=""
  [ -n "$CURLERR" ] && SUFFIX=" [curl: $CURLERR]"

  # curl's own exit code first: a REFUSED connection and a HUNG one are
  # different faults, and on this fleet "restart the runner" is exactly the
  # wrong move for the second (served policy production-and-cost
  # runner-lifecycle).
  case "$CE" in
    7)
      echo "coord-provision-nonce: NO_RUNNER (nothing is listening at $origin)$SUFFIX" >&2
      unknown_rc; continue ;;
    28)
      echo "coord-provision-nonce: RUNNER_TIMEOUT ($origin answered nothing within ${REQ_TIMEOUT}s - often SATURATION, not a dead runner; re-run rather than restarting it)$SUFFIX" >&2
      unknown_rc; continue ;;
  esac
  if [ -z "$CODE" ] || [ "$CODE" = "000" ]; then
    echo "coord-provision-nonce: NO_RUNNER ($origin produced no HTTP status)$SUFFIX" >&2
    unknown_rc; continue
  fi

  ERRCODE="$(printf '%s' "$BODY" | json_get '.code')"
  ERRMSG="$(printf '%s' "$BODY" | json_get '.error')"

  case "$CODE" in
    2??)
      URL="$(printf '%s' "$BODY" | json_get '.mcpServers["coord-mcp"].url')"
      NONCE="$(printf '%s' "$BODY" | json_get '.mcpServers["coord-mcp"].headers["X-Coord-Mcp-Proxy-Key"]')"
      if [ -z "$NONCE" ]; then
        # Configs written after the header move carry the SAME nonce under
        # Authorization. It is a nonce either way - never a bearer.
        AUTHV="$(printf '%s' "$BODY" | json_get '.mcpServers["coord-mcp"].headers.Authorization')"
        NONCE="$(printf '%s' "$AUTHV" | sed -E 's/^[[:space:]]*[Bb]earer[[:space:]]+//')"
      fi
      if [ -n "$URL" ] && [ -n "$NONCE" ]; then
        echo "coord-provision-nonce: minted a proxy nonce from $origin (cwd=$CWD_ARG)" >&2
        echo "url=$URL"
        echo "nonce=$NONCE"
        exit 0
      fi
      echo "coord-provision-nonce: PROVISION_SHAPE_UNRECOGNISED ($origin answered $CODE but the config carried no mcpServers[\"coord-mcp\"].url + proxy nonce - UNKNOWN, not a refusal)" >&2
      unknown_rc; continue ;;
    404)
      echo "coord-provision-nonce: ROUTE_ABSENT ($origin answered 404 for /coord-mcp/provision-session - this runner's build predates the in-process mint. Do NOT restart a running runner over this; the next start picks it up.)" >&2
      # Named cause beats generic UNKNOWN: a later origin that merely fails to
      # answer must not overwrite the one verdict that actually explains itself.
      named_rc 5; continue ;;
    4??|5??)
      if [ -n "$ERRCODE" ]; then
        # A TYPED refusal settles the question for this box, and each code has a
        # different fix - which is exactly why the runner emits three of them
        # instead of one 403. Never collapse them.
        case "$ERRCODE" in
          *_INVALID_TENANT|*_TENANT_NOT_PAIRED|*_TENANT_UNKNOWN|*_TENANT_REFUSED)
            # P2's refusals of the TENANT, not of the caller: nothing was minted,
            # and the fix is about which tenant was named (or pairing), never the
            # opt-in marker or the handshake. Its own exit code so no caller can
            # read it as one of those - and ONLY for a caller that named a
            # tenant, which is the documented contract of exit 6 (header,
            # scripts/README.md). Without --tenant the code cannot be about a
            # tenant this call sent, so it stays an ordinary typed refusal (3)
            # rather than an exit the caller has no input to fix.
            if [ -n "$TENANT_ARG" ]; then
              echo "coord-provision-nonce: REFUSED $ERRCODE - the runner will not mint a session credential for tenant $TENANT_ARG: ${ERRMSG:-no message} (HTTP $CODE from $origin)" >&2
              echo "refusal=$ERRCODE"
              exit 6
            fi
            echo "coord-provision-nonce: REFUSED $ERRCODE from $origin, on a call that named NO tenant: ${ERRMSG:-no message} (HTTP $CODE). Reported as a typed refusal, not as a tenant refusal." >&2 ;;
          *NOT_OPTED_IN*)
            echo "coord-provision-nonce: REFUSED $ERRCODE - session-provisioned coord identity is not opted in on this machine (create ~/.qontinui/allow-session-coord-identity; it is the operator's live kill switch, re-read per request). Message: ${ERRMSG:-none}" >&2 ;;
          *NO_HANDSHAKE*)
            echo "coord-provision-nonce: REFUSED $ERRCODE - the runner saw no loopback handshake header. The secret was staged from $KEY_FILE [$KEY_SOURCE], so this is a LOCAL staging fault (empty file? path conversion?), not an opt-in problem. Message: ${ERRMSG:-none}" >&2 ;;
          *HANDSHAKE_MISMATCH*)
            # NOT "re-read the file": the file is usually fresh and belongs to
            # a DIFFERENT runner (a second instance on the box, or a runner
            # that restarted since $QONTINUI_RUNNER_LOOPBACK_KEY was set).
            # The runner on this origin names the file it wrote in its own
            # breadcrumb; compare against that, not against a cached value.
            echo "coord-provision-nonce: REFUSED $ERRCODE - the key you posted is not the key of the runner on $origin; its breadcrumb (under $BREADCRUMB_DIR, the record whose .port matches) names the file it wrote. This script posted the secret from $KEY_FILE [$KEY_SOURCE]: a \$QONTINUI_RUNNER_LOOPBACK_KEY set from another runner or before this one restarted is the usual cause when that is the override, and a legacy schema-1 record beside a rewritten bare file when it is not. Message: ${ERRMSG:-none}" >&2 ;;
          *)
            echo "coord-provision-nonce: REFUSED $ERRCODE - ${ERRMSG:-no message} (HTTP $CODE from $origin)" >&2 ;;
        esac
        exit 3
      fi
      echo "coord-provision-nonce: MINT_HTTP_${CODE} ($origin answered $CODE with no typed code in the body - UNKNOWN. It says nothing about whether this box has a credential.)$SUFFIX" >&2
      unknown_rc; continue ;;
    *)
      echo "coord-provision-nonce: MINT_HTTP_${CODE} (unhandled status from $origin - UNKNOWN)$SUFFIX" >&2
      unknown_rc; continue ;;
  esac
done

echo "coord-provision-nonce: no candidate origin yielded a nonce ($ORIGINS)" >&2
exit "$LAST_RC"
