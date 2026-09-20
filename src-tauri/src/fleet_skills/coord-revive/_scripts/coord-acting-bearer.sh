#!/bin/bash
# coord-acting-bearer.sh — mint an acting-user Service bearer for the coord
# operator-scoped HTTP routes (gate `register` + the work-unit writes) WITHOUT
# the operator's personal Cognito login.
#
# This is the agent-JWT → acting-user Service token flow shipped in plan
# `2026-06-08-coord-service-principal-for-operator-scoped-routes` (coord
# `POST /coord/auth/acting-user-service-token`). It is the RESIDUAL LAST
# RESORT of the `/gate` cascade — fire it only after the proxy-nonce
# transports (native MCP tool, live loopback proxy, the runner's
# `/coord-mcp/*` REST write forwarder) have all failed.
#
# How it works:
#   1. Read the caller's coord agent/device JWT from $COORD_AGENT_JWT — the
#      ONLY credential source. No `.mcp.json` is consulted as a credential
#      source: every `.mcp.json` the runner writes is proxy-shaped (loopback
#      `url` + a proxy NONCE — carried as `X-Coord-Mcp-Proxy-Key` on older
#      configs and as `Authorization: Bearer <nonce>` on ones written after the
#      Phase 2 header move; the proxy injects a fresh device JWT per request),
#      and legacy bearer shapes are rewritten to proxy shape on contact. The old
#      `mcpServers["coord-mcp"].headers.Authorization` sweep matched nothing
#      and was deleted (plan `2026-07-21-gate-cascade-step3-proxy-rebase`);
#      note that an `Authorization` header alone is NO LONGER evidence of a
#      bearer — a proxy nonce now travels there too, and the discriminator is
#      whether the token is JWT-shaped (see `is_proxy_shaped`).
#      (The no-JWT error path does a READ-ONLY shape census of on-disk
#      `.mcp.json` files purely for the diagnostic — it never reads a
#      credential from them.)
#   2. POST it to `$COORD_HTTP_URL/coord/auth/acting-user-service-token`.
#      Coord resolves the acting user SERVER-SIDE from `coord.devices.user_id`
#      for that device — there is no user argument, so a device can only ever
#      mint for its own bound user (the impersonation guard).
#   3. The minted token carries `operator_act_for_user` + `user_id` + `tenant_id`
#      and is accepted by coord's `resolve_operator_or_acting_service` layer on
#      the agent-writable operator-scoped routes. It deliberately grants NO
#      approve/reject/reopen/mute authority (those stay human-only via the
#      dashboard).
#
# Output: the acting-user Service token (JWT) on stdout. Use it as
#   `Authorization: Bearer <token>` for `POST /coord/gates/register` and the
#   work-unit write routes. Tenant derives server-side — never pass one.
#
# Note: when coord's MCP surface is reachable, `coord_register_gate` already
# works over MCP (tenant resolved from the injected device JWT's claims) —
# prefer that, or the proxy-nonce REST forwarder (`/gate` Step 3). This
# helper only helps a session that has explicitly exported $COORD_AGENT_JWT —
# the one credential source independent of the `.mcp.json` file family.
#
# Hard requirements (error loudly per `[[feedback_hook_failure_surface_before_bypass]]`):
#   - curl must be installed; JSON is parsed with jq, else a working python.
#   - $COORD_AGENT_JWT set to a live agent/device JWT.
#
# Exit codes: 0 ok; 2 no agent JWT; 3 coord mint failed; 127 missing curl or
#             no working JSON reader (jq / python).

set -u

COORD="${COORD_HTTP_URL:-https://coord.qontinui.io}"

if ! command -v curl >/dev/null 2>&1; then
  echo "coord-acting-bearer: ERROR: curl is required." >&2
  exit 127
fi

# jq is ABSENT on the Windows operator box. This helper is coord-revive's L3
# leg, so a 127 here makes the cascade print "VERDICT: DEAD" with its third door
# never attempted — a LOCAL fault wearing a coord verdict, which is exactly what
# coord-revive's header forbids. Pick jq, else a WORKING python.
#
# `command -v python` is not enough: Windows ships App Execution Alias stubs at
# %LOCALAPPDATA%/Microsoft/WindowsApps/python{,3}.exe that resolve, print
# nothing and exit non-zero. That yields empty output for every parse — the
# missing-binary-reads-as-empty-field bug this whole fix exists to kill. Smoke
# test each candidate before accepting it.
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
  echo "coord-acting-bearer: ERROR: neither jq nor a working python is available to parse JSON." >&2
  exit 127
fi

# is_proxy_shaped <file> — 0 iff the config's coord-mcp entry carries a loopback
# PROXY NONCE rather than a bearer JWT. Config on STDIN under both arms
# (MSYS_NO_PATHCONV).
#
# The old test was `has("X-Coord-Mcp-Proxy-Key") and not has("Authorization")`,
# and plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning Phase 2
# makes that FALSE for every newly written config: the nonce moves INTO
# `Authorization: Bearer <nonce>` (a custom header makes the MCP client attach an
# OAuth provider, so a stale-key 401 escalates into discovery and then DCR, which
# the runner 404s). Left alone, this census would count 0 on a workspace full of
# proxy configs and the no-JWT error would take the shape-blind "none is
# loopback-PROXY shape either (is the runner provisioning this workspace?)" arm —
# telling the operator the runner is not provisioning, at the exact moment it is.
#
# So the discriminator is no longer the header NAME but what the header CARRIES:
# a raw proxy nonce is not JWT-shaped, while a real bearer is. That is the same
# distinction the runner itself makes in `coord_mcp_safe_to_write` (it decodes
# the token and only treats it as an agent bearer if the JWT parse succeeds).
# The legacy header still counts, unconditionally — both shapes live on disk
# indefinitely, because configs are rewritten only on session spawn.
is_proxy_shaped() {
  if [ "$JSON_READER" = jq ]; then
    jq -e '(.mcpServers["coord-mcp"].headers // {}) as $h
      | (($h["X-Coord-Mcp-Proxy-Key"] // "") | tostring) as $legacy
      | ((($h.Authorization // "") | tostring) | sub("^[Bb][Ee][Aa][Rr][Ee][Rr] +"; "")) as $tok
      | ($legacy != "")
        or (($tok != "")
            and (($tok | test("^[A-Za-z0-9._-]+$")
                  and ((($tok | split(".")) | length) == 3)) | not))' < "$1" >/dev/null 2>&1
  else
    "$JSON_READER" -c 'import json,sys,re
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
h=((d.get("mcpServers",{}) or {}).get("coord-mcp",{}) or {}).get("headers",{}) or {}
legacy=h.get("X-Coord-Mcp-Proxy-Key") or ""
authz=(h.get("Authorization") or "").strip()
tok=re.sub(r"^[Bb][Ee][Aa][Rr][Ee][Rr] +","",authz)
jwtish=bool(tok) and tok.count(".")==2 and re.match(r"^[A-Za-z0-9._-]+$",tok) is not None
sys.exit(0 if (legacy or (tok and not jwtish)) else 1)' < "$1" >/dev/null 2>&1
  fi
}

# read_token — reads a mint response on STDIN, prints .token (empty if absent).
# The TOKEN leaves on stdout into a shell variable, never via argv.
read_token() {
  if [ "$JSON_READER" = jq ]; then
    jq -r '.token // ""' 2>/dev/null
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(""); sys.exit(0)
print(d.get("token","") or "")' 2>/dev/null
  fi
}

# ── Resolve the agent/device JWT — $COORD_AGENT_JWT is the ONLY source ───────
# Strip whitespace so an all-whitespace export reads as unset instead of
# minting with a garbage bearer.
jwt=$(printf '%s' "${COORD_AGENT_JWT:-}" | tr -d '[:space:]')
jwt_source="\$COORD_AGENT_JWT"
if [ -z "$jwt" ]; then
  # Read-only shape census for the diagnostic: count the DISTINCT on-disk
  # .mcp.json configs whose coord-mcp entry is loopback-PROXY shape (an
  # X-Coord-Mcp-Proxy-Key header and NO Authorization bearer). Those configs
  # can never feed this helper — they are the proxy-key door (/gate step 2) —
  # so the error must say that instead of a shape-blind "none carried
  # Authorization" (plan 2026-07-27-coord-mcp-flake-remediation, Phase 2).
  # Workspace root = the directory containing the repo checkouts. $QONTINUI_ROOT
  # overrides; otherwise the parent of the MAIN checkout via `--git-common-dir`,
  # anchored on THIS SCRIPT's directory (the script always lives in the repo).
  # The old `$(dirname "$0")/../..` broke for a worktree copy of the repo: from
  # `<wt>/scripts` it resolved to the worktree CONTAINER, which holds no repo
  # configs, so the census counted 0 and the error took the shape-blind "none is
  # loopback-PROXY shape either (is the runner provisioning this workspace?)"
  # branch — the exact opposite of the proxy-shape diagnostic this census was
  # added to deliver. Verified live 2026-07-27: worktree run said 0, the same run
  # with $QONTINUI_ROOT set said 11.
  # `--path-format=absolute` (git >= 2.31) instead of absolutising a relative
  # `.git` via `cd "$gc" && pwd`: a failed cd there yields an EMPTY substitution,
  # and dirname-twice then collapses to `.` — a wrong root that reads like a real
  # one, which for this census means counting 0 configs again.
  root="${QONTINUI_ROOT:-}"
  if [ -z "$root" ]; then
    gc="$(cd "$(dirname "$0")" 2>/dev/null && git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
    case "$gc" in
      ""|"."|"..") root="$(cd "$(dirname "$0")/../.." && pwd)" ;;
      *) root="$(dirname "$(dirname "$gc")")" ;;
    esac
  fi
  proxy_count=0
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    # jq via STDIN, never a path ARGUMENT: under an inherited MSYS_NO_PATHCONV=1
    # the POSIX spelling reaches the NATIVE jq.exe unconverted, it exits 2
    # "Could not open file", and every config silently fails the test. The
    # census then counts 0 and this helper prints the WRONG diagnostic — "none
    # is loopback-PROXY shape either (is the runner provisioning this
    # workspace?)" — when proxy-shaped configs do exist, sending the reader
    # after a non-problem. Same inversion the --show-toplevel bug caused here
    # (PR #161); same idiom PR #171 fixed in pr-status.sh.
    if is_proxy_shaped "$f"; then
      proxy_count=$((proxy_count + 1))
    fi
  done < <(for c in "$PWD/.mcp.json" "$root/.mcp.json" "$root"/*/.mcp.json; do
             # realpath canonicalizes for the dedup; if it's absent, fall back
             # to the raw path rather than silently dropping the candidate (a
             # probe that cannot run must never answer "none found").
             [ -r "$c" ] && { realpath "$c" 2>/dev/null || printf '%s\n' "$c"; }
           done | sort -u)
  if [ "$proxy_count" -gt 0 ]; then
    echo "coord-acting-bearer: ERROR: no agent JWT — \$COORD_AGENT_JWT is unset/empty, and it is the ONLY source. ${proxy_count} config(s) are loopback-PROXY shape (a proxy nonce, under X-Coord-Mcp-Proxy-Key or Authorization, not a bearer JWT); this helper needs a bearer — use the proxy-key door (/gate step 2) or set \$COORD_AGENT_JWT." >&2
  else
    echo "coord-acting-bearer: ERROR: no agent JWT — \$COORD_AGENT_JWT is unset/empty, and it is the ONLY source: no on-disk .mcp.json carries a bearer, and none is loopback-PROXY shape either (is the runner provisioning this workspace?) — use the proxy-key door (/gate step 2) once one exists, or set \$COORD_AGENT_JWT." >&2
  fi
  echo "coord-acting-bearer: use the proxy-nonce transports instead (/gate Steps 1-3: native coord_register_gate, a live loopback proxy, or the runner's /coord-mcp/* write forwarder), or export \$COORD_AGENT_JWT to use this last-resort mint." >&2
  exit 2
fi
# Always name the winning source. $COORD_AGENT_JWT is the only one today, but
# the unconditional line is a contract: with several accounts and
# session-scoped tenancy on one machine, the credential used determines which
# user/tenant the call is attributed to. It names the source, never the token.
echo "coord-acting-bearer: credential from $jwt_source" >&2

# ── Mint the acting-user Service token ────────────────────────────────────────
# Auth header via a private tempfile (curl -H @file), never argv: process
# cmdlines are world-readable on this multi-session machine, so a bearer on
# argv leaks to every peer session (same hazard class coord-revive.sh closed;
# flake-remediation Phase 2 review residual, fixed 2026-07-27).
auth_hdr=$(mktemp) || { echo "coord-acting-bearer: ERROR: mktemp failed." >&2; exit 3; }
trap 'rm -f "$auth_hdr"' EXIT
printf 'Authorization: Bearer %s\n' "$jwt" > "$auth_hdr"
# Native curl.exe cannot open mktemp's POSIX path when MSYS pathconv is off
# (MSYS_NO_PATHCONV=1 sessions): hand it the Windows path explicitly. curl
# stderr stays VISIBLE on this last-resort path — a "Failed to open" here
# must not masquerade as "coord down".
hdr_path=$auth_hdr
command -v cygpath >/dev/null 2>&1 && hdr_path=$(cygpath -w "$auth_hdr")
resp=$(curl -fsS -X POST "$COORD/coord/auth/acting-user-service-token" \
  -H @"$hdr_path" \
  -H "Content-Type: application/json") || {
  echo "coord-acting-bearer: ERROR: mint request to $COORD failed (device unknown / no bound user / coord down?)." >&2
  exit 3
}

token=$(printf '%s' "$resp" | read_token)
if [ -z "$token" ] || [ "$token" = "null" ]; then
  echo "coord-acting-bearer: ERROR: coord response carried no token: $resp" >&2
  exit 3
fi

printf '%s\n' "$token"
