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
# and the second probe fires ONLY on a retry-safe verdict (CREDENTIAL_REFRESHING or TIMEOUT); no loops.
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
# jwt-cascade-selection: every source is PROBED against coord and a rejection
# falls through to the next, so no local `exp` decode is needed and no static
# token can shadow the source behind it. Declared for
# scripts/lint-jwt-cascade-parity.py check E.
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
#   TIMEOUT                      — no answer reached this box inside the budget.
#                                  UNKNOWN, not dead; earns the second probe
#   TIMEOUT_UPSTREAM             — the proxy ANSWERED (408/504) that ITS upstream
#                                  hung. Shares the TIMEOUT* prefix, so it earns
#                                  the same retry, and is an HTTP-plane answer
#   PROXY_LIVE_UPSTREAM_DEAD     — HTTP 502 carrying one of the runner's typed
#                                  upstream codes: the proxy is LIVE and the
#                                  coord /mcp hop is not (the F2 class). Probed
#                                  twice; a second one is final
# and three verdicts that are NOT failures at all — the live-class set, see
# is_live_verdict(): LIVE (the end-to-end tools/call answered isError:false),
# LIVE_APP_ERROR (…answered isError:true — the TOOL complained, the transport is
# proven) and PROXY_LIVE_E2E_UNVERIFIED (tools/list answered, the end-to-end
# probe was not run). Two SKIPS that are statements of absence rather than
# verdicts about a door — SKIPPED_BUDGET_EXCEEDED and
# SKIPPED_SHARED_UPSTREAM_REFRESHING — and one verdict about a PORT rather than
# a door: RUNNER_WEDGED (below).
# plus HTTP_200_NOT_MCP (200 without a JSON-RPC result — treat as dead),
# UNREACHABLE / HTTP_<code>, typed L3 causes (NO_TOKEN / MINT_FAILED /
# HELPER_NOT_FOUND / HELPER_DEPS_MISSING) and typed L4 causes:
#   NO_RUNNER                    — nothing answered at that origin
#   RUNNER_TIMEOUT               — the port answered but never responded; often
#                                  SATURATION, not a dead runner
#   RUNNER_EVAL_FAILED           — it ANSWERED, but not with a well-formed
#                                  evaluate result (non-2xx / route absent /
#                                  success:false). NOT a sign-in problem
#   RUNNER_EVAL_CSP_BLOCKED      — the WebView CSP forbids evaluating a string
#                                  as JavaScript, so the eval mint cannot work
#                                  on this BUILD for any expression. A BROKEN
#                                  DOOR, not a credential state; never transient
#   RUNNER_EVAL_STATIC_GUARD     — the frontend blocklist rejected the
#                                  expression before evaluating it. About what
#                                  was SENT, not about the runner
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
# any config, and in particular it never writes a .mcp.json.
#
# MINTING, and the exact bound on it. This script used to promise it would NEVER
# call /coord-mcp/provision-session, because minting re-provisions the ONE-SLOT
# workdir key and would evict a live peer's — the exact failure class this
# script exists to recover from. That promise is now NARROWED rather than
# dropped, and the narrowing is what makes it safe:
#
#   * L4 source 3 mints, and it is reachable ONLY after L1 (this workdir's own
#     .mcp.json) and L2 (every sibling) have each been PROBED and found dead.
#   * It mints for $PWD — the very workdir whose key L1 just measured dead. A
#     peer sharing this workdir shares that dead key, so there is no live slot
#     here left to evict. Sibling workdirs are untouched: the slot is per-cwd.
#   * $COORD_REVIVE_NO_MINT=1 disables it outright, for an operator who wants
#     the old absolute guarantee back.
#
# Measured against the runner source while reviewing this change, and stronger
# than the bound argued above: an EPHEMERAL mint cannot evict ANY peer, in this
# workdir or another. `provision_session_proxy_config` ->
# `register_session_proxy_nonce` mints `NonceLifetime::Ephemeral`, and
# `coord_mcp.rs` states outright "do NOT re-add cwd-scoped eviction, which caused
# the sibling-DoS" — that eviction was deliberately REMOVED. Ephemeral bindings
# are also never persisted, so they never push against the 256-entry
# MAX_PERSISTED_DEVICE_NONCES cap either; each mint simply ADDS a binding that
# dies on its own TTL (12h). The operator's revoke is global and instant:
# deleting ~/.qontinui/allow-session-coord-identity, re-checked per request.
# Do not "restore" an eviction here on the strength of the first paragraph's
# wording — the hazard it names is historical, not live.
#
# It is narrowed because the alternative was worse. On a HEADLESS runner the
# UI-Bridge mint below CANNOT answer at all (no WebView), so a box with a live
# device credential and no sibling .mcp.json lying around had no door whatsoever
# and this script printed DEAD — confidently wrong. Plan
# 2026-08-24-headless-box-has-no-working-coord-credential-door.
#
# What the mint returns is a NONCE, never a bearer: a local capability token
# paired to this runner's own bound port, worthless off-box, with the runner
# injecting a freshly-read device JWT per forwarded request.
#
# L4's other outbound POSTs are the runner's UI-Bridge token GETTERS
# (`get_coord_device_token`, then `get_access_token_for_websocket`), which
# return a credential the runner already holds and mutate nothing; the second is
# the same call render-memory-cache.ps1 makes on every session boot.
#
# L4 source 4 mints its bearer through TWO runner doors, in order:
#   invoke  POST <origin>/ui-bridge/invoke/get_coord_device_token  {}, then
#           POST <origin>/ui-bridge/invoke/get_access_token_for_websocket  {}
#           an IN-PROCESS arm of the invoke proxy - no WebView hop, so it
#           answers on a headless runner. Two names for ONE credential slot;
#           the ungated one is tried first because the other calls
#           require_tier_2() and would turn a Tier-1 runner's live token into a
#           tier refusal. A build without the allowlist entry answers HTTP 400
#           "not in UI Bridge allowlist" (or 404 for the whole route); ONLY that
#           answer moves on to the next name, and only both of them failing that
#           way opens the eval fallback.
#   eval    POST <origin>/ui-bridge/control/page/evaluate - the WebView mint,
#           CSP-refused on the builds measured refusing (58414a05-1788118917383,
#           2026-09-02; an unrecorded build, 2026-08-31 - it ANSWERED on
#           546e9e024-1788209530736, 2026-09-01, so per-build, never every
#           build) and kept solely for a build that predates the invoke entry; never attempted when /health says the
#           runner is headless.
# Whichever answered is named on the door (`source=runner-invoke:<command>` /
# `source=runner-eval`). Both names read the SAME `access_token` slot -
# "one slot, two names, both shipped" (coord-gates-and-access.md) - so neither
# spelling upgrades the token's authority, and on the measured builds what comes
# back is the operator's own Cognito identity rather than a fleet service one
# (see PARTIAL_RUNNER_MINT below). The eval door additionally CANNOT ANSWER AT
# ALL on a CSP-enforcing build - see RUNNER_EVAL_CSP_BLOCKED - which is why the
# eval-free invoke door above is the one that has to carry this rung.
#
# TWO VERBS, no cascade: `coord-revive.sh call <tool> '<json-args>'` and
# `coord-revive.sh tools` EXECUTE over the door L1 would find - the caller's
# OWN .mcp.json nonce, walked up from $PWD - and print the JSON-RPC result.
# They never mint (see the block below the helper functions).
#
# WEDGE AND BUDGET, the two verdicts that are not about one door:
#   RUNNER_WEDGED    ONE endpoint answered on BOTH planes inside one sweep — a
#                    transport-level verdict interleaved with an HTTP-level one.
#                    No key or config fault can produce that set; the runner's
#                    HTTP surface is starved. The re-provision advice is
#                    SUPPRESSED on this path: re-provisioning during a wedge
#                    evicts a live peer's key to fix a key that was never broken.
#   BUDGET_EXCEEDED  the $PROBE_TOTAL_BUDGET wall clock ran out mid-sweep. It
#                    replaces DEAD rather than joining it, because DEAD asserts
#                    every door was probed and this run skipped some. The skipped
#                    doors are named in the same list.
#
# Output: probe log on stderr; the final "VERDICT: ..." on stdout.
# Exit: 0 = LIVE door found; 1 = cascade exhausted; 127 = missing curl, or
#       neither jq nor python available to read JSON (both LOCAL faults).
#       For the two verbs: 0 = the tool answered (result on stdout);
#       1 = the door did not carry the call (typed reason on stderr: no
#       provisioned nonce, 401, dead port, timeout); 3 = the tool answered
#       with a JSON-RPC error (its answer, printed on stderr); 4 = usage.

set -u

RPC_LIST='{"jsonrpc":"2.0","id":1,"method":"tools/list"}'

# THE END-TO-END PROBE, and why a `tools/list` green is not one.
#
# `tools/list` is answered by the PROXY: it proves the local hop, the nonce and
# the JSON-RPC framing, and it says nothing whatever about whether coord is
# behind it. A door can list 50-odd tools and then 502 on every call — the F2
# class this cascade kept reporting as a clean LIVE. So a `tools/list`-only
# green is downgraded to PROXY_LIVE_E2E_UNVERIFIED, which is still a LIVE-CLASS
# verdict (the transport IS proven, as far as it went) but never a bare LIVE.
#
# THE TOOL IS `coord_query_identity` AND THE CHOICE IS MEASURED, not obvious:
#   * zero required arguments, so `{}` is a VALID call rather than a validation
#     error. `coord_can` was the first candidate and is WRONG for exactly this
#     reason — with `{}` it answers isError:true, which would make every healthy
#     door report LIVE_APP_ERROR and teach readers to ignore that verdict.
#   * a READ, and a cheap one (~0.34s measured), so a diagnostic never mutates.
#   * present in the device tool set AND in the runner's proxy allowlist, so it
#     is callable over every rung this script probes, not just the loopback ones.
# Verified live 2026-09-05: HTTP 200, isError:false.
#
# $COORD_REVIVE_E2E=0 turns it off. That is a CHOICE and it is reported as one —
# the verdict stays PROXY_LIVE_E2E_UNVERIFIED, never an unearned LIVE.
RPC_E2E='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"coord_query_identity","arguments":{}}}'
PROBE_E2E="${COORD_REVIVE_E2E:-1}"
COORD_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
# FOUR budgets, not one (three per-call, one for the whole sweep — the fourth is
# defined below MINT_TIMEOUT). Until 2026-08-31 a single PROBE_TIMEOUT=15 was spent
# on two calls that are orders of magnitude apart: the cheap JSON-RPC
# `tools/list` probe (loopback, answers in milliseconds) and the WebView-driven
# device-JWT mint at L4 source 4 (`POST <origin>/ui-bridge/control/page/evaluate`,
# which bounces through a Tauri WebView to reach the Rust process holding the
# credential). 15s was the SHORTEST budget any door in the fleet gave that mint,
# and when it tripped this script printed VERDICT: DEAD - a false "no credential
# available" for a box whose credential was fine, which is exactly the
# confidently-wrong output this script exists to replace.
#
# Every value is env-overridable, following the $COORD_HTTP_URL idiom above.

# TCP connect ONLY. A loopback connect either happens at once or the port is
# dead; 5s is already generous, and its only real job is to stop a black-holed
# remote host from hanging the whole cascade before the transfer even starts.
PROBE_CONNECT_TIMEOUT="${COORD_REVIVE_CONNECT_TIMEOUT:-5}"

# The whole `tools/list` probe in probe_door(), start to finish. Cheap by
# construction on the L1/L2 loopback rungs; it also carries the L3/L4 bearer
# probes against ${COORD_URL}/mcp, which answer a tool catalog over ONE HTTPS
# POST with no session handshake. 15s leaves headroom for a cold TLS handshake
# on a loaded box while still failing fast enough that sweeping a dozen
# candidate .mcp.json files stays cheap - and a trip here is reported AS a
# timeout by classify(), never as a credential verdict.
PROBE_TIMEOUT="${COORD_REVIVE_PROBE_TIMEOUT:-15}"
DOOR_SCRIPT_NAME="coord-revive"

# The L4 source-4 BEARER mints - the in-process invoke door and the WebView
# eval fallback behind it - and nothing else. 60s, aligned with the most generous
# budget the fleet already gives this exact call (`/gate` residual (b) spends
# -TimeoutSec 60 on it) - deliberately aligned UPWARD rather than to the median
# of the fleet's 15/20/20/60 spread, because this is the door that actually has
# to succeed: below it there is nothing left but the honest-failure report. The
# measurement that forces the size is quoted at the mint itself - /health on
# this same runner has been sampled from 296ms to 10120ms on a loaded box, and
# the mint is a strictly longer call than /health (it adds the WebView hop).
MINT_TIMEOUT="${COORD_REVIVE_MINT_TIMEOUT:-60}"

# FOUR budgets now, and this one bounds the SWEEP rather than a call. The three
# above each bound ONE request, which bounds nothing in aggregate: the cascade
# probes as many doors as the filesystem hands it, so the total is a product
# nobody chose. 60s is the size a human waits for a diagnostic before assuming it
# has hung - and it is deliberately SMALLER than the arithmetic worst case,
# because reporting which doors went unprobed is strictly better than a complete
# answer nobody stayed to read. A trip is reported as BUDGET_EXCEEDED naming the
# doors probed and the doors skipped; it is never reported as DEAD, which would
# be a verdict about doors this run never touched.
#
# The measurement that forces a bound at all: every canonical door on this fleet
# now resolves to the SAME runner port, so a device-JWT refresh makes all ~12
# answer CREDENTIAL_REFRESHING together, each paying 15 + 3 + 15 = 33s ~= 400s
# for one correlated fact. The budget is the backstop; the SHARED-UPSTREAM
# short-circuit below is the actual fix.
PROBE_TOTAL_BUDGET="${COORD_REVIVE_TOTAL_BUDGET:-60}"
HERE="$(cd "$(dirname "$0")" && pwd)"

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
#
# THREE MORE PREFIXES ARE LOAD-BEARING, added by plan
# 2026-08-06-coord-mcp-post-remediation-plan Phase 1:
#   TIMEOUT*                     both carriers retry it once. `TIMEOUT_UPSTREAM` is
#                                DELIBERATELY spelled to share that prefix, so a
#                                proxy-reported upstream stall inherits the retry.
#                                It is separately classified as an HTTP-PLANE answer
#                                by coord-revive.sh's wedge detector, which reads
#                                TIMEOUT_UPSTREAM* BEFORE TIMEOUT*; swap that order
#                                and every 504 starts corroborating a runner wedge
#                                that is not happening.
#   LIVE_APP_ERROR*              TRANSPORT-LIVE. The door carried the call and the
#                                TOOL complained. Any caller comparing `verdict` to
#                                the literal "LIVE" drops it on the floor and reports
#                                a dead door over a demonstrably live one.
#   PROXY_LIVE_UPSTREAM_DEAD*    the proxy is LIVE and its coord hop is not. Both
#                                carriers re-probe it once; a second one is final.
#                                It is NOT a live-class verdict: no call can be
#                                carried over it.
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

# rpc_error_code — reads a JSON body on STDIN; prints the typed ERROR CODE the
# envelope carries (`.error.code`, else a top-level `.code`), or nothing.
#
# READ THE CODE, NEVER THE MESSAGE. The runner's proxy error text is due to grow
# a source chain, so a classifier keyed on message text would stop matching on
# the day it does — a live 502 silently re-reading as an unclassified `HTTP_502`,
# i.e. the undifferentiated mask this whole block exists to replace, restored by
# a wording change nobody would think to check. The code token is the contract;
# the message is prose.
#
# Anything that is not a STRING comes back empty, so a numeric JSON-RPC `code`
# (the -32601 family) cannot collide with the proxy's symbolic ones.
rpc_error_code() {
  if [ "$JSON_READER" = jq ]; then
    jq -r 'if type == "object" then (((.error? | objects | .code?) // .code?) // "") else "" end
           | if type == "string" then . else "" end' 2>/dev/null  # envelope-ok: reads ONLY the typed code token, never the message text, and a non-string is "" rather than a value
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(); sys.exit(0)
e=d.get("error") if isinstance(d,dict) else None
c=e.get("code") if isinstance(e,dict) else None
if not isinstance(c,str):
    c=d.get("code") if isinstance(d,dict) else None
print(c if isinstance(c,str) else "")' 2>/dev/null  # envelope-ok: same typed code token, python arm
  fi
}

# rpc_is_app_error — reads a JSON-RPC body on STDIN; 0 iff `.result` is a
# WELL-FORMED MCP TOOL ENVELOPE reporting a TOOL-level error: an object carrying
# `content` whose `isError` is true.
#
# READ THIS BEFORE TOUCHING IT — THE TRAP IS THE WHOLE POINT. `isError` is a
# property of the TOOL's answer, never of the transport: the door carried the
# call, coord ran the tool, and the tool complained. The obvious edit — folding
# an `isError` test into `rpc_has_result` above so a "failed" response stops
# counting as a result — turns that into a DEAD verdict over a door that just
# demonstrated it works end to end. That is the worst output this family of
# scripts can produce (coord-revive's SKILL.md sells its DEAD line as honest
# blocked-evidence), so the caller maps this to LIVE_APP_ERROR, which is a
# LIVE-CLASS verdict, and NEVER to a dead one.
#
# `content` is required alongside `isError` on purpose: without it, any tool
# whose own DATA happens to carry a field named `isError` would be read as an
# envelope. Requiring both keeps the predicate about the MCP envelope shape.
rpc_is_app_error() {
  if [ "$JSON_READER" = jq ]; then
    jq -e '((.result | objects | (has("content") and (.isError == true))) // false)' >/dev/null 2>&1  # envelope-ok: a two-key PRESENCE predicate over the tool envelope (exit status only, no value leaves it)
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
r=d.get("result") if isinstance(d,dict) else None  # envelope-ok: a PRESENCE test, not a read - the value never leaves this function, only the exit status does
sys.exit(0 if (isinstance(r,dict) and "content" in r and r.get("isError") is True) else 1)' >/dev/null 2>&1  # envelope-ok: same presence predicate, python arm
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
  # THE PROXY DID NOT ANSWER. Nothing came back inside the budget, so whether the
  # proxy ever answered is UNKNOWN — this box abandoned the request. Deliberately
  # NOT the same statement as TIMEOUT_UPSTREAM below, which is the proxy ANSWERING
  # that its own upstream hung. Collapsing the two loses the one bit that decides
  # what to do next: a door that never answered may still be alive and loaded,
  # while a door that answered 504 is provably alive and its coord hop is not.
  if [ "$ce" = "28" ]; then echo "TIMEOUT (no response reached this box within ${PROBE_TIMEOUT}s - the request was abandoned client-side, so whether the proxy answered at all is UNKNOWN; this is NOT the proxy reporting a stalled upstream, which is TIMEOUT_UPSTREAM). Often SATURATION rather than a dead door - do NOT restart the runner on this alone; re-run, or use another door"; return; fi
  # 26 = "couldn't open/read the local data file", i.e. the auth-header file.
  # This is a LOCAL fault and must never read as a coord verdict — the same
  # rule coord-acting-bearer.sh states for its mint ("a 'Failed to open' here
  # must not masquerade as 'coord down'").
  if [ "$ce" = "26" ]; then
    echo "AUTH_HEADER_UNREADABLE (curl could not open the staged header file - LOCAL fault, says nothing about coord)"; return
  fi
  # Also "the proxy did not answer", by a different route: the transfer failed
  # before any status line arrived (DNS, TLS, a reset). `000` is curl's own
  # no-status marker, so it is a statement about THIS BOX's reach, never about
  # the upstream behind the proxy.
  if [ "$ce" != "0" ] && [ "$code" = "000" ]; then echo "UNREACHABLE (curl exit $ce - the transfer never completed and NO HTTP status came back, so nothing here observed the proxy answering)"; return; fi
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
        # …and a result that IS a tool envelope reporting isError:true is still a
        # result: the transport is PROVEN end to end and the TOOL declined. See
        # rpc_is_app_error's header for why this must never become a dead verdict.
        if printf '%s' "$body" | rpc_is_app_error; then
          echo "LIVE_APP_ERROR (transport PROVEN end to end - the door carried the call, coord ran the tool, and the TOOL answered isError:true. That is the tool's verdict, not the door's: treat this as LIVE)"
        else
          echo "LIVE"
        fi
      else
        echo "HTTP_200_NOT_MCP (200 without a JSON-RPC result - treat as dead)"
      fi
      return ;;
    401) echo "$unauth"; return ;;
    408|504)
      # THE PROXY ANSWERED, and what it answered is that its own upstream did
      # not. Spelled to share the TIMEOUT* prefix so both carriers' retry picks
      # it up; classified on the HTTP plane, not the transport one, because a
      # status line came back.
      echo "TIMEOUT_UPSTREAM (HTTP $code - the proxy ANSWERED and reported that ITS upstream did not: a live local door in front of a hop that hung. Distinct from TIMEOUT, where nothing answered at all and this box abandoned the request. Retry-safe)"; return ;;
    502)
      # The F2 class: proxy LIVE, coord /mcp hop DEAD. Matched on the envelope's
      # typed `code`, deliberately NOT on its message — see rpc_error_code.
      # An unrecognised 502 deliberately FALLS THROUGH to the unclassified tail
      # below rather than borrowing this verdict: this script names causes it has
      # tested, and a 502 from something that is not the coord-mcp proxy has not
      # been.
      case "$(printf '%s' "$body" | rpc_error_code | tr -d '\r')" in
        COORD_MCP_PROXY_UPSTREAM_UNREACHABLE|COORD_MCP_PROXY_UPSTREAM_READ_FAILED|COORD_MCP_PROXY_UPSTREAM_NON_JSON_ERROR)
          echo "PROXY_LIVE_UPSTREAM_DEAD (proxy answered, the coord /mcp hop failed - the F2 class; re-probe once, then treat as retryable-unknown)"; return ;;
      esac ;;
    503) echo "CREDENTIAL_REFRESHING (retry-safe - HTTP 503)"; return ;;
  esac
  local snippet
  snippet=$(printf '%s' "$body" | tr -d '\n' | head -c 120)
  echo "HTTP_$code (unclassified: ${snippet:-<empty body>})"
}

# ========================== END SHARED COORD-DOOR CLASSIFIER v1 ==========================

# scripts/lib/envelope.sh: the typed envelope reader (an absent key is exit 3
# with an `UNKNOWN:` line on stderr and NOTHING on stdout, never ""). It
# inherits the reader chosen above rather than choosing its own. Resolved the
# way guard-decision-log.sh is at the bottom of this file, plus the PHYSICAL
# path: `<workspace-root>/.claude` is a symlink into the config repo, so the
# logical `$HERE/../../..` lands beside the workspace root, not in the repo.
# The three original rungs all assume $HERE sits THREE levels below a directory
# that also contains scripts/lib/ -- i.e. that this .claude/ is the config repo's
# own, or a symlink into it. That holds for <workspace-root>/.claude and for the
# config repo itself, and it is FALSE for every other checkout, because each one
# carries its OWN REAL .claude/ copy of the skills bundle. `pwd -P` then has no
# symlink to resolve and rung 2 lands on rung 1's non-existent path, leaving only
# $QONTINUI_ROOT -- which is routinely unset.
#
# Measured 2026-09-06 on the operator box (finding 21b7611b): 31 of the 32
# checkouts carrying this skill could not resolve the library, so the script
# exited 127 WITHOUT PROBING ANYTHING and emitted no VERDICT: line at all. The
# population that hits it is exactly this script's audience -- a session whose
# coord transport just died, working inside a repo checkout -- and in the session
# that measured it the coord doors were LIVE throughout.
#
# So walk UP instead of assuming a depth: from both the logical and the physical
# $HERE, test each ancestor for scripts/lib/ (the config repo, at whatever depth)
# and for qontinui-claude-config/scripts/lib/ (the workspace root, reached from a
# sibling checkout). The original three rungs are kept FIRST so the common cases
# still resolve on the first test and nothing about their behaviour changes.
ENVELOPE_READER="$JSON_READER"
ENVELOPE_LIB=""
__env_candidates() {
  printf '%s
' "$HERE/../../../scripts/lib/envelope.sh"
  printf '%s
' "$(cd "$HERE" 2>/dev/null && pwd -P)/../../../scripts/lib/envelope.sh"
  printf '%s
' "${QONTINUI_ROOT:-}/qontinui-claude-config/scripts/lib/envelope.sh"
  for __base in "$HERE" "$(cd "$HERE" 2>/dev/null && pwd -P)"; do
    [ -n "$__base" ] || continue
    __d="$(cd "$__base" 2>/dev/null && pwd)" || continue
    while [ -n "$__d" ]; do
      printf '%s
' "$__d/scripts/lib/envelope.sh"
      printf '%s
' "$__d/qontinui-claude-config/scripts/lib/envelope.sh"
      __parent="$(dirname "$__d")"
      [ "$__parent" = "$__d" ] && break
      __d="$__parent"
    done
  done
  # Last resort: the workspace root derived the way /policy Step 2 derives it --
  # `--git-common-dir`, never `--show-toplevel`, which inside a LINKED WORKTREE
  # returns the worktree path and would send this walk into the worktree
  # container instead of the workspace root.
  __gc="$(git rev-parse --git-common-dir 2>/dev/null)"
  if [ -n "$__gc" ]; then
    __gc="$(cd "$__gc" 2>/dev/null && pwd)"
    [ -n "$__gc" ] && printf '%s
' "$(dirname "$(dirname "$__gc")")/qontinui-claude-config/scripts/lib/envelope.sh"
  fi
}
while IFS= read -r __env_lib; do
  # An unset $QONTINUI_ROOT degenerates rung 3 to an absolute path off the root;
  # skip it rather than stat it, so the search reports honestly what it tried.
  case "$__env_lib" in ""|"/scripts/lib/envelope.sh"|"/qontinui-claude-config/scripts/lib/envelope.sh") continue ;; esac
  if [ -r "$__env_lib" ]; then ENVELOPE_LIB="$__env_lib"; break; fi
done <<__ENV_CANDIDATES__
$(__env_candidates)
__ENV_CANDIDATES__
if [ -z "$ENVELOPE_LIB" ]; then
  echo "$DOOR_SCRIPT_NAME: ERROR: scripts/lib/envelope.sh not found from HERE=$HERE — searched the three fixed rungs, every ancestor of HERE (logical and physical) for scripts/lib/ and qontinui-claude-config/scripts/lib/, and the --git-common-dir workspace root. Cannot read any door's answer (LOCAL fault, NOT a coord verdict: this says NOTHING about whether coord is reachable — probe a door directly before concluding anything about it)." >&2
  exit 127
fi
# shellcheck source=../../../scripts/lib/envelope.sh
. "$ENVELOPE_LIB"
unset __env_lib

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

# The verdict probe_door actually won with. `live_exit` prints it, so a
# qualified live verdict cannot reach the reader spelled as a bare LIVE.
LIVE_VERDICT="LIVE"

# is_live_verdict <verdict> — 0 iff this verdict means "this door works".
#
# THIS PREDICATE IS THE POINT OF THE WHOLE PHASE. There are now THREE live-class
# verdicts, and every one of them used to be a string that is not the literal
# "LIVE". A `[ "$verdict" = "LIVE" ]` anywhere in the caller silently drops two
# of the three on the floor and walks on to report DEAD over a door that just
# answered — the exact false-DEAD class this script exists to prevent, produced
# by the change that was meant to sharpen it. Every comparison in probe_door goes
# through here; none of them spells the literal any more.
#
#   LIVE                        the end-to-end tools/call answered isError:false
#   LIVE_APP_ERROR              …answered isError:true. The TOOL complained; the
#                               transport is proven. Still live.
#   PROXY_LIVE_E2E_UNVERIFIED   tools/list answered and the e2e probe did not run
#                               (disabled). Live as far as it was measured, and
#                               it says so instead of overstating.
#
# PROXY_LIVE_UPSTREAM_DEAD is deliberately NOT here despite the name: the proxy
# is live and no call can be carried over it, so it is a failure with an honest
# cause, not a door to hand the caller.
is_live_verdict() {
  case "$1" in
    LIVE|LIVE_APP_ERROR*|PROXY_LIVE_E2E_UNVERIFIED*) return 0 ;;
  esac
  return 1
}

# endpoint_hostport <url> — the host:port the URL resolves to, for the two
# per-ENDPOINT joins below. Deliberately coarser than seen_endpoint()'s (url,
# auth) signature: both properties below are about the PROCESS listening on that
# port, which every credential and every path under it shares.
endpoint_hostport() {
  local u="${1#*://}"
  printf '%s' "${u%%/*}"
}

# ----- the correlated-refresh short-circuit -----------------------------------
# Every canonical door on this fleet now points at the SAME runner port, so a
# device-JWT refresh makes ALL of them answer CREDENTIAL_REFRESHING together —
# each paying two probes and a 3s sleep for a fact the first one already
# established. Once one door has settled on CREDENTIAL_REFRESHING (settled = it
# still said so AFTER its own bounded retry), sibling doors on the same
# host:port share that upstream and are reported SKIPPED_SHARED_UPSTREAM_REFRESHING
# rather than re-probed.
#
# WHY "SETTLED" AND NOT "ONCE": a door that says CREDENTIAL_REFRESHING and then
# something else on the retry is not a withholding proxy, it is a WEDGE (below),
# and a wedge must not be collapsed into one skipped verdict — its evidence IS
# the mixed set. Only a door that answered 503 twice marks its upstream.
#
# It is a SKIP, not a verdict about the sibling, and it is named as one: nothing
# here observed that door, and the DEAD block prints it as unprobed.
REFRESHING_UPSTREAMS=""
NO_UPSTREAM_SKIP="${COORD_REVIVE_NO_UPSTREAM_SKIP:-}"
mark_upstream_refreshing() {
  local hp; hp="$(endpoint_hostport "$1")"
  [ -n "$hp" ] || return 0
  case "$REFRESHING_UPSTREAMS" in *"|$hp|"*) return 0 ;; esac
  REFRESHING_UPSTREAMS="${REFRESHING_UPSTREAMS}|$hp|"
}
upstream_is_refreshing() {
  [ -z "$NO_UPSTREAM_SKIP" ] || return 1
  local hp; hp="$(endpoint_hostport "$1")"
  [ -n "$hp" ] || return 1
  case "$REFRESHING_UPSTREAMS" in *"|$hp|"*) return 0 ;; esac
  return 1
}

# ----- the wedge detector -----------------------------------------------------
# SKILL.md has documented RUNNER_WEDGED since PR #259 and this script emitted the
# string NOWHERE — a table row promising a verdict the tool cannot produce, which
# is worse than no row: it sends a reader looking for output that will never
# arrive. This is that verdict, computed from probes the sweep already pays for.
#
# THE SHAPE: ONE endpoint answering on BOTH planes inside ONE sweep.
#   transport plane  TIMEOUT / CONNECT_REFUSED / UNREACHABLE — no HTTP status
#                    came back at all.
#   HTTP plane       a status line DID come back: 401-class, LIVE-class, 503,
#                    HTTP_200_NOT_MCP, TIMEOUT_UPSTREAM, any HTTP_<code>.
# No key fault and no config fault can produce both: a stale key 401s every time,
# a dead port refuses every time. Mixing them means the process is INTERMITTENTLY
# ACCEPTING — the runner is up and its HTTP surface is starved.
#
# Why it matters more than a nicer message: the default reading of the 401 in
# that set is "stale proxy key", whose remedy is a re-provision, which EVICTS a
# live peer's key. So on a wedge the ordinary advice actively worsens the
# incident. On RUNNER_WEDGED the re-provision advice is suppressed.
#
# TIMEOUT_UPSTREAM is read BEFORE TIMEOUT so it lands on the HTTP plane where it
# belongs: it is the proxy ANSWERING 504, not the proxy failing to answer. Get
# that order wrong and every stalled coord hop starts corroborating a wedge that
# is not happening.
WEDGE_TRANSPORT=""
WEDGE_HTTP=""
WEDGED_ENDPOINTS=""
verdict_plane() {
  case "$1" in
    TIMEOUT_UPSTREAM*)                                  echo http ;;
    TIMEOUT*|CONNECT_REFUSED*|UNREACHABLE*)             echo transport ;;
    LIVE|LIVE_APP_ERROR*|PROXY_LIVE_*|CREDENTIAL_REFRESHING*|HTTP_*) echo http ;;
    *UNAUTHORIZED*)                                     echo http ;;
    *)                                                  echo none ;;
  esac
}
# record_plane <url> <verdict> — accumulate one probe's plane against its
# endpoint and note the endpoint the moment it has answered on both.
# is_loopback_endpoint <host:port> — 0 for an endpoint served by a process on
# THIS box. The wedge verdict is a statement about THE RUNNER, which serves the
# coord-mcp proxy on its own loopback port; L3/L4 probe the PUBLIC coord host
# under several different credentials, and a 401 from one bearer beside a
# timeout from another there is ordinary internet, not a starved runner. Naming
# that RUNNER_WEDGED would be a confidently wrong verdict about a machine
# nobody measured — the one thing this script exists not to do.
#
# STATED LIMIT: a runner reached over a non-loopback address is therefore never
# reported wedged. That is the safe direction (a missed wedge costs a diagnosis;
# a fabricated one costs a live peer's key), and on this fleet the proxy is
# always loopback.
is_loopback_endpoint() {
  case "${1%%:*}" in
    127.*|localhost|"[::1]"|::1) return 0 ;;
  esac
  return 1
}
record_plane() {
  local hp plane
  hp="$(endpoint_hostport "$1")"
  [ -n "$hp" ] || return 0
  is_loopback_endpoint "$hp" || return 0
  plane="$(verdict_plane "$2")"
  case "$plane" in
    transport) case "$WEDGE_TRANSPORT" in *"|$hp|"*) ;; *) WEDGE_TRANSPORT="${WEDGE_TRANSPORT}|$hp|" ;; esac ;;
    http)      case "$WEDGE_HTTP" in      *"|$hp|"*) ;; *) WEDGE_HTTP="${WEDGE_HTTP}|$hp|" ;; esac ;;
    *)         return 0 ;;
  esac
  case "$WEDGE_TRANSPORT" in *"|$hp|"*) ;; *) return 0 ;; esac
  case "$WEDGE_HTTP" in      *"|$hp|"*) ;; *) return 0 ;; esac
  case " $WEDGED_ENDPOINTS " in *" $hp "*) return 0 ;; esac
  WEDGED_ENDPOINTS="$WEDGED_ENDPOINTS $hp"
}

# ----- the sweep's wall-clock bound -------------------------------------------
# $SECONDS is bash's own monotonic counter, so the bound costs no subprocess and
# cannot be skewed by a clock change. It is sampled BETWEEN doors, never inside a
# request: killing a probe mid-flight would leave a door with no verdict at all,
# which is the undifferentiated silence this script replaces.
PROBE_BUDGET_T0=$SECONDS
BUDGET_TRIPPED=""
DOORS_SKIPPED=0
budget_left() {
  [ $((SECONDS - PROBE_BUDGET_T0)) -lt "$PROBE_TOTAL_BUDGET" ]
}
# budget_skip <label> <name> — 0 (and records the skip) when the budget is gone.
budget_skip() {
  budget_left && return 1
  BUDGET_TRIPPED=1
  DOORS_SKIPPED=$((DOORS_SKIPPED + 1))
  local v="SKIPPED_BUDGET_EXCEEDED (the ${PROBE_TOTAL_BUDGET}s sweep budget was spent before this door was reached, so it was NOT probed. UNKNOWN, never dead - raise \$COORD_REVIVE_TOTAL_BUDGET to reach it)"
  echo "$1: $2 -> $v" >&2
  FAILS+=("$1 $2: $v")
  return 0
}

# seen_door <path> — 0 if this door was already probed (canonical-path dedup,
# so a symlinked/re-spelled path can't burn extra probes); records it otherwise.
seen_door() {
  local rp
  rp="$(realpath "$1" 2>/dev/null || printf '%s' "$1")"
  case "$SEEN" in *"|$rp|"*) return 0 ;; esac
  SEEN="${SEEN}|$rp|"
  return 1
}

# seen_endpoint <url> <header-name> <header-value> — 0 if this DOOR was already
# probed, 1 (and counted) if it is new.
#
# seen_door() above dedups by canonical PATH. That is the right guard for the
# SAME file reached twice through the glob, and it is the wrong guard for N
# DISTINCT files that all name ONE door: every session's workdir gets its own
# .mcp.json, so a workspace with 22 checkouts can hand this sweep 22 readable
# files carrying the SAME url+nonce. Path-dedup passes all 22, we pay 22 probe
# budgets against one endpoint, and the DEAD line then reports "22 probeable
# doors" — which reads as 22 independent pieces of evidence when it is one door
# probed 22 times. That inflated count is what makes a single flaky probe look
# corroborated.
#
# The signature is (url, header-name, header-value) reduced through cksum, so
# the accumulator never carries key material even though the inputs do. cksum
# is POSIX and present wherever this script already assumes sh + curl. A hash
# COLLISION would merge two doors (under-probe by one) and is vastly less likely
# than the miscount it prevents; an UNUSABLE signature is handled separately
# below and must never skip — see the fail-open guard.
DISTINCT_DOORS=0
SEEN_ENDPOINTS=""
seen_endpoint() {
  local sig
  sig="$(printf '%s\n%s\n%s' "$1" "$2" "$3" | cksum 2>/dev/null | awk '{print $1}')"
  # FAIL OPEN, never closed. If cksum is missing, errors, or (on a padding
  # implementation) yields a non-numeric field, $sig is empty — and an empty
  # signature makes the accumulator "||", after which the pattern *"||"* matches
  # for EVERY later call and every genuinely distinct door is skipped. The script
  # runs under `set -u` only, so nothing else would catch it, and the failure
  # prints "same door as an earlier candidate" for doors that are not the same
  # one and then VERDICT: DEAD — the false-DEAD this file exists to prevent.
  # Under-probing is NOT the safe direction here (that reasoning holds only for a
  # hash COLLISION, ~5e-8 at this N); an unusable signature must probe, not skip.
  case "$sig" in ''|*[!0-9]*) DISTINCT_DOORS=$((DISTINCT_DOORS + 1)); return 1 ;; esac
  case "$SEEN_ENDPOINTS" in *"|$sig|"*) return 0 ;; esac
  SEEN_ENDPOINTS="${SEEN_ENDPOINTS}|$sig|"
  DISTINCT_DOORS=$((DISTINCT_DOORS + 1))
  return 1
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
# NOTE for future callers: the retry case below prefix-matches CREDENTIAL_REFRESHING*
# and TIMEOUT*, and [unauth-wording] is CALLER-SUPPLIED and reaches classify() as a
# verdict. A caller whose wording begins with either prefix would silently acquire a
# retry. Keep unauth wordings starting with their own distinct token.
#
# THAT HAZARD IS NOW WORSE, so the rule is stricter: is_live_verdict() matches
# LIVE (exact), LIVE_APP_ERROR* and PROXY_LIVE_E2E_UNVERIFIED*. An unauth wording
# beginning with any of those would make a 401 read as a WORKING DOOR — a false
# LIVE, which is the one failure worse than the false DEADs the rest of this file
# guards against. Every existing wording starts with its own *_UNAUTHORIZED
# token; keep it that way.
# Max 2 attempts; attempt 2 ONLY on the retry-safe verdicts CREDENTIAL_REFRESHING
# and TIMEOUT. Returns 0 on any LIVE-CLASS verdict (is_live_verdict), NOT on the
# literal string "LIVE" — there are three of them now and comparing to the
# literal drops two on the floor.
#
# TWO STAGES since plan 2026-08-06-coord-mcp-post-remediation-plan Phase 1:
#   1. tools/list  proves the PROXY (local hop, nonce, JSON-RPC framing).
#   2. tools/call coord_query_identity {}  proves COORD is behind it. Runs only
#      when stage 1 said LIVE, self-bounded at 2 attempts (the second only on
#      PROXY_LIVE_UPSTREAM_DEAD / TIMEOUT / CREDENTIAL_REFRESHING), and it
#      SUPPRESSES the outer retry once it has run, so a door costs at most 3
#      requests: 2 tools/list, or 1 tools/list + 2 tools/call.
# Skipping stage 2 ($COORD_REVIVE_E2E=0) yields PROXY_LIVE_E2E_UNVERIFIED, never
# a bare LIVE.
#
# WHY TIMEOUT RETRIES (2026-09-02). A curl exit 28 is the one probe outcome that
# says nothing about the door: it says this box did not get an answer inside
# $PROBE_TIMEOUT. The fleet already diagnosed exactly this for the L4 mint and
# fixed it THERE ONLY — MINT_TIMEOUT went 15s -> 60s on 2026-08-31 because
# /health on this same runner samples 296ms..10120ms and a 15s budget was "enough
# to turn a slow-but-healthy runner into VERDICT: DEAD". The probe budget that
# gates the whole L1/L2 sweep was left at 15s with NO retry — the shorter budget
# on the more frequent call. Measured consequence (finding 4e8bcd86): a session
# swept its candidates, hit a loaded box, concluded "NO LIVE LOOPBACK DOOR",
# published that in a merged PR body, and the door answered HTTP 200 on the very
# next attempt.
#
# WORST-CASE WALL-CLOCK, stated as the 2026-07-27 retry plan requires. Per
# TIMING-OUT door: 2 * $PROBE_TIMEOUT + 3s sleep = 33s at the 15s default (was
# 15s). Only a door that times out TWICE pays it; every other verdict still
# costs one attempt. The sweep's TOTAL budget nonetheless falls, because the
# (url, auth) dedup in seen_endpoint() collapses N files naming one door to ONE
# probe — the case that produced the 22-candidate sweeps this bound is measured
# against. Bounded exactly as CREDENTIAL_REFRESHING is: one extra attempt, one
# sleep 3, no loops.
# The auth header travels via a private tmp file (curl -H @file), never argv —
# keys must not be visible in the machine-wide process list.
probe_door() {
  local label="$1" name="$2" url="$3" hname="$4" hvalue="$5" unauth="${6:-}"
  local attempt code ce verdict curlerr hdrpath bodypath e2e_ran e2e_attempt skipv
  local hdrfile="$TMPD/hdr" bodyfile="$TMPD/body" errfile="$TMPD/err"
  # TWO between-door checks, both BEFORE any request. Each is a SKIP with a named
  # reason, never a verdict about the door: nothing here observed it.
  if budget_skip "$label" "$name"; then return 1; fi
  if upstream_is_refreshing "$url"; then
    DOORS_SKIPPED=$((DOORS_SKIPPED + 1))
    skipv="SKIPPED_SHARED_UPSTREAM_REFRESHING (a sibling door on $(endpoint_hostport "$url") already settled on CREDENTIAL_REFRESHING after its own retry, and this door shares that upstream process - re-probing it would spend another $((2 * PROBE_TIMEOUT + 3))s to learn the same fact. NOT a verdict about this door: it was not probed. Set \$COORD_REVIVE_NO_UPSTREAM_SKIP=1 to probe every sibling anyway)"
    echo "$label: $name -> $skipv" >&2
    FAILS+=("$label $name: $skipv")
    return 1
  fi
  # Register the endpoint for the DISTINCT count regardless of rung. L1/L2 have
  # already called this (so it returns 0 and does not double count); L3/L4 have
  # not, and without it $DISTINCT_DOORS would count only the loopback rungs while
  # $DOORS_PROBED counted every rung — two different populations printed side by
  # side in the DEAD line, which produced "1 probe(s) across 0 distinct door(s)".
  seen_endpoint "$url" "$hname" "$hvalue" || :
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
  e2e_ran=""
  for attempt in 1 2; do
    DOORS_PROBED=$((DOORS_PROBED + 1))   # ATTEMPTS, not doors — the DEAD line says "probe(s)"
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
    record_plane "$url" "$verdict"

    # ----- STAGE 2: the END-TO-END probe ---------------------------------------
    # A bare LIVE here means only that the PROXY framed a tools/list. Spend one
    # more cheap read to find out whether coord is behind it, and downgrade the
    # verdict to PROXY_LIVE_E2E_UNVERIFIED when that read is not run — an
    # unqualified LIVE this script has not earned is the same overstatement the
    # PARTIAL machinery exists to prevent, one layer down.
    if [ "$verdict" = "LIVE" ]; then
      verdict="PROXY_LIVE_E2E_UNVERIFIED (the proxy framed a tools/list over this door, and the end-to-end tools/call was NOT run (\$COORD_REVIVE_E2E=0). The local hop, the nonce and the framing are proven; whether coord answers behind them is UNMEASURED)"
      if [ "$PROBE_E2E" != "0" ]; then
        e2e_ran=1
        # Self-bounded exactly as the outer loop is: 2 attempts, one 3s sleep, no
        # loops. Because it bounds itself, the OUTER retry is suppressed once this
        # stage has run — otherwise a stalled coord hop would buy a second full
        # tools/list that answers a question already answered.
        for e2e_attempt in 1 2; do
          DOORS_PROBED=$((DOORS_PROBED + 1))
          : > "$bodyfile"
          : > "$errfile"
          code=$(curl -sS -o "$bodypath" -w '%{http_code}' \
            --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$PROBE_TIMEOUT" \
            -X POST "$url" -H "Content-Type: application/json" \
            -H "@$hdrpath" -d "$RPC_E2E" 2>"$errfile")
          ce=$?
          verdict="$(classify "$ce" "$code" "$(cat "$bodyfile" 2>/dev/null)" ${unauth:+"$unauth"})"
          record_plane "$url" "$verdict"
          if [ "$e2e_attempt" = "1" ]; then
            case "$verdict" in
              PROXY_LIVE_UPSTREAM_DEAD*|TIMEOUT*|CREDENTIAL_REFRESHING*)
                echo "$label: $name -> $verdict [e2e attempt 1/2, re-probing once]" >&2
                sleep 3; continue ;;
            esac
          fi
          break
        done
      fi
    fi

    # UNREACHABLE and the TLS/DNS exits are exactly where a bare exit number is
    # not yet a cause, and the invariant above says every path must name one.
    if ! is_live_verdict "$verdict"; then
      curlerr=$(tr -d '\r' < "$errfile" 2>/dev/null | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g; s/^ //; s/ $//')
      [ -n "$curlerr" ] && verdict="$verdict [curl: $curlerr]"
    fi
    # EVERY live-class verdict wins the door. A literal `= "LIVE"` here would
    # drop LIVE_APP_ERROR and PROXY_LIVE_E2E_UNVERIFIED and walk on to report
    # DEAD over a door that answered — read is_live_verdict's header.
    if is_live_verdict "$verdict"; then
      rm -f "$hdrfile"
      echo "$label: $name -> $verdict ($url)" >&2
      LIVE_FILE="$name"
      LIVE_URL="$url"
      LIVE_VERDICT="$verdict"
      return 0
    fi
    echo "$label: $name -> $verdict [probe $attempt/2]" >&2
    if [ -z "$e2e_ran" ]; then
      case "$verdict" in
        CREDENTIAL_REFRESHING*|TIMEOUT*) if [ "$attempt" = "1" ]; then sleep 3; continue; fi ;;
      esac
    fi
    break
  done
  rm -f "$hdrfile"
  # SETTLED, i.e. after this door's own retry — see mark_upstream_refreshing.
  case "$verdict" in
    CREDENTIAL_REFRESHING*) mark_upstream_refreshing "$url" ;;
  esac
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
  local transport="$1" partial="${2:-}" v="${LIVE_VERDICT:-LIVE}"
  # The verdict NAME leads, because there are three live-class ones and only one
  # of them is a bare LIVE. Printing "LIVE" over a PROXY_LIVE_E2E_UNVERIFIED
  # would be the same overstatement PARTIAL exists to prevent, one layer down.
  # $v carries classify()'s parenthetical, so the line stays self-explaining.
  if [ -n "$partial" ]; then
    echo "VERDICT: $v door=$LIVE_FILE url=$LIVE_URL transport=$transport PARTIAL"
    echo "$partial"
  else
    echo "VERDICT: $v door=$LIVE_FILE url=$LIVE_URL transport=$transport"
  fi
  case "$v" in
    LIVE_APP_ERROR*)
      echo "NOTE: the end-to-end probe reached coord and the TOOL answered isError:true. The DOOR is proven - re-issue over it. The tool's own complaint is about the CALL (arguments, authority, or that tool's state), not about the transport, and it is not evidence your lost write failed." ;;
    PROXY_LIVE_E2E_UNVERIFIED*)
      echo "NOTE: only the PROXY was measured (tools/list). Whether coord answers behind it was NOT tested on this run, so re-issue your call and read the ANSWER rather than treating this verdict as end-to-end proof. Unset \$COORD_REVIVE_E2E to run the end-to-end probe." ;;
  esac
  echo "Re-issue the lost call over this door, then VERIFY BY READ (a \"no output\" write is presumed LOST - findings 2026-07-26 section 3)."
  # The approval half rides ON the LIVE verdict, not only on DEAD. A door
  # answering says nothing about whether the session's native coord_* tools were
  # ever allowed to load, and LIVE is exactly where that distinction gets lost:
  # an agent reads "LIVE", is told to re-issue over this door, and still has no
  # coord tools. PR #370 named this shape as the reason it left the gap open.
  # stdout, deliberately -- a pasted LIVE verdict must carry it, the same reason
  # the DEAD block repeats the breadcrumb.
  wedge_block
  approval_verdict_block
  exit 0
}

# wedge_block -- print VERDICT: RUNNER_WEDGED when one endpoint answered on both
# planes inside this sweep. Emitted on the LIVE path too: another door carrying
# the call does not un-wedge the wedged one, and an agent about to "fix" that
# port's key needs to see this before it re-provisions and evicts a live peer.
#
# It never changes the primary verdict and never suppresses a door -- it is a
# SECOND verdict about a PORT, printed beside the first, which is about a DOOR.
wedge_block() {
  [ -n "$WEDGED_ENDPOINTS" ] || return 0
  local hp
  for hp in $WEDGED_ENDPOINTS; do
    echo "VERDICT: RUNNER_WEDGED endpoint=$hp"
  done
  echo "  One endpoint answered on BOTH planes inside this single sweep - a transport-level verdict (TIMEOUT / CONNECT_REFUSED / UNREACHABLE, i.e. no HTTP status came back at all) interleaved with an HTTP-level one (a 401-class answer, a LIVE-class answer, 503, TIMEOUT_UPSTREAM, or any HTTP_<code>). NO key fault and NO config fault can produce that set: a stale key 401s every time, a dead port refuses every time. The process is INTERMITTENTLY ACCEPTING - the runner is up and its HTTP surface is starved."
  echo "  DO NOT RE-PROVISION OR ROTATE THE PROXY KEY. The key is fine, and a re-provision EVICTS a live peer's binding - during a wedge the ordinary 401 advice makes the incident worse. The re-provision advice is deliberately suppressed on this run."
  echo "  DO NOT restart or kill the runner (served policy production-and-cost runner-lifecycle); a restart destroys in-flight sessions and this evidence with them. Probe /livez on that port to tell one stuck handler from a starved runtime (a 404 there means the build predates the endpoint - inconclusive, not healthy). Treat every in-flight coord write as LOST and verify by read: the coord-mcp proxy is served BY the runner, so a wedge takes it down for every session on the box at once. Observed wedges have resolved on their own - waiting is a legitimate move."
}

# approval_verdict_block -- re-emit the approval summary on STDOUT beside a
# verdict. Defined before live_exit's first caller and guarded on the summary
# actually having been computed, because live_exit is reachable from a probe
# that runs before the approval block on any future re-ordering; printing
# nothing is the honest behaviour there, and a bare "APPROVAL:" prefix with an
# empty body would be a named-but-empty cause.
approval_verdict_block() {
  [ -n "${APPROVAL_VERDICT:-}" ] || return 0
  echo "APPROVAL: $APPROVAL_VERDICT"
  echo "  (the approval half is INDEPENDENT of every door above: .mcp.json declares the server, a settings key approves it, and Claude Code will not load a project-scoped server it has not approved. Full per-layer readings are on stderr above. This never changed the verdict.)"
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
PARTIAL_RUNNER_MINT="PARTIAL: on the builds measured, this door authenticates as the OPERATOR's own Cognito user/tenant, NOT as a fleet service identity. get_access_token_for_websocket and get_coord_device_token read the SAME access_token slot under two names, so preferring the ungated spelling removes a tier refusal and does NOT upgrade the token's authority - probe the door, never infer from the name.
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

# L5's PARTIAL block. Different subject from the two above: theirs is about
# REACH (what the door can read), this one is about PROVENANCE and BLAST RADIUS.
# The bootstrap token is an AGENT principal minted against a device UUID, not a
# device principal, and this script asserts nothing beyond what its control read
# actually measured.
PARTIAL_BOOTSTRAP="PARTIAL: the url= above is the MINT, not a door to re-issue a write over. This rung yields a BEARER; spend it on the device-authed hand-written \${COORD_HTTP_URL}/coord/... REST routes, which /gate's write-forwarder REST rung spells out. It is NOT carried onto \${COORD_HTTP_URL}/mcp - that door's device-JWT-only constraint is unchanged.
PARTIAL: this bearer is sub_type=agent with a DEVICE subject (sub=device:<uuid>) and NO agent_id claim - measured 2026-09-04 - so it is scoped by whatever coord grants such a principal in this tenant, which is not the same set the L1/L2 proxy or an L4 device JWT carries. coord's own agent-refresh helper will not refresh it for that reason (agent-only route); re-mint instead.
PARTIAL: VERIFIED here: the control read GET \${COORD_HTTP_URL}/coord/agent-findings?limit=1 answered 200. Measured 2026-09-04 the same bearer also read \${COORD_HTTP_URL}/coord/agent-prompt-documents and one policy document at 200, so tenant resolution DID work on those routes - but that is those routes' evidence, not a general guarantee: a 403 cannot-resolve-tenant elsewhere is THAT route's verdict, not a refutation of the credential.
PARTIAL: it is SHORT-LIVED - ~4h (14400s, measured). It is NOT over-broad: measured 2026-09-04 every scope in the minted token was empty or false (git_push [], merge_propose false, build_submit false, strategy_admin false, introspect false, no NATS subjects), which is NARROWER than the sibling allocate route's token (that one carries git_push scoped to the reserved branch plus agent NATS subjects; neither mints merge_propose). Use it for the read or write you came for and DISCARD it: never persist it, never print it, never put it on any process's argv.
PARTIAL: the anonymity of the SIBLING /agents/allocate route is an OPEN operator ruling - surfaced and deliberately left open by plan 2026-08-31-coord-mcp-credential-selection-by-binding-provenance Phase 8, and escalated as coord gate ece99898-30c6-4f8c-be8e-1de5f09abebc. Nothing here licenses that route, and this credential is never preferred anywhere a device JWT resolves."

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
# The INVOKE door answers the runner's ApiResponse envelope with `data` as the
# command's bare return value - {"success":true,"data":"<jwt>"} - so a string
# `data` is read too. One function, three envelopes, ONE reader:
# envelope_first_present with the tuple live-first, boxed second, bare-string
# last, and the non-empty-string predicate that stops the always-present `data`
# OBJECT from winning. An unreadable body is exit 3 with an `UNKNOWN:` line on
# stderr and NOTHING on stdout -- the caller's `[ -z "$MJWT" ]` then goes to
# read_eval_error for the cause, exactly as before, but the reader itself can
# no longer print "" as if it were a value. Same tuple as install-claude-settings.sh
# `cmd_mintedjwt`; lint-jwt-cascade-parity.py D1/D3 pin both.
read_minted_jwt() {
  envelope_first_present "runner-mint" "data.value,data.result.value,data" -
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
           | map(select(type == "string" and . != "")) | (.[0] // "")' 2>/dev/null  # envelope-ok: the ERROR-path reader; "" here means "no error string", never a value, and the token paths are read_minted_jwt's
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

# ----- The execute door: `call <tool> '<json-args>'` and `tools` ------------
# Diagnosis exists; execution did not - in bash. This script reported which
# door is LIVE and stopped there, so a session that had diagnosed correctly
# still had no way to ACT without hand-rolling a JSON-RPC client (which the
# 2026-09-02 session did). These two verbs are that client, once, as a port of
# lib/coord-credential.psm1's Invoke-CoordProxyTool: one JSON-RPC shape in each
# language, no third. Plan
# 2026-09-02-steering-layers-unreadable-without-a-credential, Phase 1c.
#
# THE DOOR IS THE CALLER'S OWN NONCE, and the verbs NEVER MINT. They read the
# nearest proxy-shaped .mcp.json walking UP from $PWD (a linked worktree's
# config sits one or two levels above it), both header shapes accepted, and
# replay it verbatim. They never call /coord-mcp/provision-session: minting
# there re-provisions the ONE-SLOT workdir key and EVICTS the live peer's
# binding - the exact failure class Phase 1a of that plan exists to end. The
# cascade's own mint (L4 source 3) is bounded to run only after L1 and L2 have
# PROBED this workdir's key and found it dead; a verb that runs on every call
# has no such bound, so it has no mint at all. A 401 here is therefore final
# for THIS verb and names its recovery instead.
#
# The nonce never touches argv or stdout: it is staged into a private header
# file (curl -H @file), the same discipline as probe_door. The RESULT is
# printed on stdout for the caller to pipe; everything else goes to stderr.
CALL_TIMEOUT="${COORD_REVIVE_CALL_TIMEOUT:-60}"

verb_usage() {
  cat >&2 <<'EOF'
usage: coord-revive.sh                          run the transport cascade (default)
       coord-revive.sh tools                    list the tools this session's own nonce may call
       coord-revive.sh call <tool> ['<json-object>']
                                                EXECUTE one coord MCP tool over that nonce - whatever
                                                it is allowed, reads AND writes; verify a write by read
Both verbs read the nearest .mcp.json above $PWD and never mint a nonce.
EOF
}

# find_own_cfg -> sets CFG_URL / CFG_KEY / CFG_KEY_HEADER / OWN_CFG_PATH from the
# first proxy-shaped .mcp.json on the walk up from $PWD; 1 (with the shapes of
# every rejected candidate in $CFG_TRIED) when none.
find_own_cfg() {
  local d="$PWD" f p
  OWN_CFG_PATH=""; CFG_TRIED=""
  while [ -n "$d" ]; do
    f="$d/.mcp.json"
    if [ -r "$f" ]; then
      if read_cfg "$f"; then OWN_CFG_PATH="$f"; return 0; fi
      CFG_TRIED="$CFG_TRIED
  $f: $(cfg_shape "$f")"
    fi
    p="$(dirname "$d")"
    [ "$p" = "$d" ] && break
    d="$p"
  done
  return 1
}

# build_rpc <method> <tool> <args-json> -> the JSON-RPC payload on stdout, or
# exit 1 with the reason on stderr. The args are parsed by the JSON reader, so
# a malformed object is refused HERE with its parse error, and an array or a
# scalar is refused as not-an-object - the forwarder would otherwise answer a
# JSON-RPC error that reads like coord's verdict on a local typo. The reader
# does the escaping; nothing here string-splices the caller's JSON.
build_rpc() {
  if [ "$JSON_READER" = jq ]; then
    if [ "$1" = "tools/list" ]; then
      jq -cn '{jsonrpc:"2.0",id:1,method:"tools/list",params:{}}'
    else
      jq -cn --arg n "$2" --argjson a "$3"         'if ($a|type) != "object" then error("arguments must be a JSON object ({...}), not \($a|type)")
         else {jsonrpc:"2.0",id:1,method:"tools/call",params:{name:$n,arguments:$a}} end'
    fi
  else
    RPC_METHOD="$1" RPC_TOOL="$2" RPC_ARGS="$3" "$JSON_READER" -c 'import json,os,sys
m=os.environ["RPC_METHOD"]
if m=="tools/list":
    print(json.dumps({"jsonrpc":"2.0","id":1,"method":m,"params":{}})); sys.exit(0)
try: a=json.loads(os.environ.get("RPC_ARGS") or "{}")
except Exception as e:
    sys.stderr.write("arguments are not valid JSON: %s\n" % e); sys.exit(1)
if not isinstance(a,dict):
    sys.stderr.write("arguments must be a JSON object ({...}), not %s\n" % type(a).__name__); sys.exit(1)
print(json.dumps({"jsonrpc":"2.0","id":1,"method":m,"params":{"name":os.environ["RPC_TOOL"],"arguments":a}}))' | tr -d '\r'
  fi
}

# rpc_print_result: stdin = the JSON-RPC body. Prints `.result` on stdout and
# returns 0; prints `.error` on stderr and returns 3; returns 1 for neither.
# Both keys are read through envelope_require: `error` first (its presence IS
# the answer), then `result`, whose absence the helper reports as a typed
# UNKNOWN on stderr -- mapped to 1 here so the caller's HTTP_200_NOT_MCP arm
# still fires. A `result` holding null is PRESENT and printed as `null`.
rpc_print_result() {
  local body err rc
  body="$(cat)"
  if err="$(printf '%s' "$body" | envelope_require "coord-mcp tools/call" error - 2>/dev/null)" \
     && [ -n "$err" ] && [ "$err" != "null" ]; then
    printf '%s\n' "$err" >&2
    return 3
  fi
  printf '%s' "$body" | envelope_require "coord-mcp tools/call" result -
  rc=$?
  case "$rc" in
    0) return 0 ;;
    *) return 1 ;;
  esac
}

if [ $# -gt 0 ]; then
  case "$1" in
    call|tools) ;;
    -h|--help|help) verb_usage; exit 0 ;;
    *) echo "coord-revive: unknown argument '$1'" >&2; verb_usage; exit 4 ;;
  esac
  VERB="$1"; shift
  V_TOOL=""; V_ARGS="{}"
  if [ "$VERB" = "call" ]; then
    V_TOOL="${1:-}"
    [ -n "$V_TOOL" ] || { echo "coord-revive: call needs a tool name" >&2; verb_usage; exit 4; }
    [ $# -le 2 ] || { echo "coord-revive: call takes ONE JSON argument - quote the object as a single argument" >&2; verb_usage; exit 4; }
    V_ARGS="${2:-}"
    [ -n "$V_ARGS" ] || V_ARGS='{}'
    V_METHOD="tools/call"
  else
    [ $# -eq 0 ] || { echo "coord-revive: tools takes no arguments" >&2; verb_usage; exit 4; }
    V_METHOD="tools/list"
  fi

  if ! build_rpc "$V_METHOD" "$V_TOOL" "$V_ARGS" > "$TMPD/rpc" 2>"$TMPD/rpcerr"; then
    echo "coord-revive: $VERB -> BAD_ARGUMENTS ($(one_line 300 < "$TMPD/rpcerr")). Local, before any request was sent." >&2
    exit 4
  fi

  if ! find_own_cfg; then
    echo "coord-revive: $VERB -> NO_PROXY_CONFIG (no proxy-shaped coord-mcp .mcp.json with a key on the walk up from $PWD).${CFG_TRIED}" >&2
    echo "  This session has no provisioned nonce to ride - a LOCAL fact, not a coord verdict. This verb does not mint one (that would evict a live peer's workdir slot): use a session the runner provisioned, /gate for a gate write, or the PowerShell coord-read.ps1 read verbs." >&2
    exit 1
  fi

  { printf '%s: %s\n' "$CFG_KEY_HEADER" "$CFG_KEY" > "$TMPD/vhdr"; } 2>/dev/null
  if [ ! -s "$TMPD/vhdr" ]; then
    echo "coord-revive: $VERB -> AUTH_HEADER_STAGING_FAILED (could not write the header file under $TMPD - LOCAL fault, says nothing about coord)" >&2
    exit 1
  fi
  : > "$TMPD/vbody"; : > "$TMPD/verr"
  VCODE=$(curl -sS -o "$(curl_path "$TMPD/vbody")" -w '%{http_code}' \
    --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$CALL_TIMEOUT" \
    -X POST "$CFG_URL" -H "Content-Type: application/json" \
    -H "@$(curl_path "$TMPD/vhdr")" --data-binary "@$(curl_path "$TMPD/rpc")" 2>"$TMPD/verr")
  VCE=$?
  rm -f "$TMPD/vhdr"
  VBODY="$(cat "$TMPD/vbody" 2>/dev/null)"
  VCURLERR="$(one_line 200 < "$TMPD/verr")"
  [ -n "$VCODE" ] || VCODE="000"
  case "$VCE:$VCODE" in
    0:200)
      # A JSON-RPC surface answers 200 for in-band errors too; the reader
      # decides which it was and prints to the matching stream.
      printf '%s' "$VBODY" | rpc_print_result
      VRC=$?
      case "$VRC" in
        0) echo "coord-revive: $VERB -> OK over $CFG_URL (nonce from $OWN_CFG_PATH, header $CFG_KEY_HEADER; source=own-mcp-json, nothing minted)" >&2; exit 0 ;;
        3) echo "coord-revive: $VERB -> RPC_ERROR (the tool answered with a JSON-RPC error - printed above; that is ITS answer, the door carried the call)" >&2; exit 3 ;;
        *) echo "coord-revive: $VERB -> HTTP_200_NOT_MCP (200 without a JSON-RPC result or error - treat the door as dead: $(printf '%s' "$VBODY" | one_line 200))" >&2; exit 1 ;;
      esac ;;
    *:401)
      echo "coord-revive: $VERB -> COORD_MCP_PROXY_UNAUTHORIZED (the forwarder at $CFG_URL rejected the nonce in $OWN_CFG_PATH; HTTP 401). The BINDING was superseded or never registered - the TRANSPORT is healthy." >&2
      echo "  Recovery for THIS caller: the file was re-read just now, so the key on disk is itself stale - run 'bash coord-revive.sh' (no verb) for the full cascade, which finds a sibling key or a bearer. Recovery for the NATIVE MCP client: it cannot re-read .mcp.json, so start a NEW SESSION. NEVER restart the runner over this, and this verb never mints (a /coord-mcp/provision-session mint evicts the live peer holding this workdir's slot)." >&2
      exit 1 ;;
    *)
      VV="$(classify "$VCE" "$VCODE" "$VBODY")"
      [ -n "$VCURLERR" ] && VV="$VV [curl: $VCURLERR]"
      echo "coord-revive: $VERB -> $VV (door $CFG_URL from $OWN_CFG_PATH; the call was NOT carried - a write here is presumed LOST, re-issue and verify by read)" >&2
      exit 1 ;;
  esac
fi

# ----- Spawn-time breadcrumb: the runner's OWN reason, when it left one --------
# `.coord-mcp-status` is a breadcrumb the RUNNER's coord-mcp provisioning
# drops into the workdir it is provisioning (for an agent session,
# that agent's PRIMARY worktree) when that provisioning went DEGRADED —
# `qontinui-runner/src-tauri/src/coord_mcp.rs`, `write_degraded_breadcrumb`.
# This script only READS it; /gate and /policy instruct an agent to read it, and
# the runner's own `qontinui-pr` CLI POINTS at it in its no-credential error
# without opening it; none of them writes it.
#
# It is read here because six of its seven reasons say THAT provisioning pass
# wrote no `.mcp.json` (no device JWT in the runner's access_token slot; a
# bearer whose `sub_type` is neither device nor agent; a workdir the
# non-clobber guard refused (a foreign `.mcp.json`, an unparseable one, or no
# file at all at a secondary runner's umbrella root); an unresolvable bound API port,
# device or agent path; an agent JWT with no `sub`). In those cases L1 finding
# nothing in your own cwd is not a second mystery — it is the documented
# consequence of a fault the runner already diagnosed. Without this line the
# cascade reports the SYMPTOM while the CAUSE sits one directory read away.
#
# The seventh is the PROBE's, and it means the opposite: a `.mcp.json` WAS
# written and did not answer at spawn. There is exactly ONE string for it —
# `port :N probe failed (dead port | 401 stale nonce | coord down)` — because
# the runner reduces every transport outcome to a single boolean on a 3-second
# budget, so it establishes none of the three causes it lists and absorbs a
# fourth it never names (a merely SATURATED runner). The typed per-door verdicts
# this script prints — TIMEOUT, CONNECT_REFUSED, UNAUTHORIZED (401),
# CREDENTIAL_REFRESHING (503), other HTTP statuses, HTTP_200_NOT_MCP, TRANSPORT
# — are THIS SCRIPT's vocabulary, not the breadcrumb's: typing the runner's own
# probe is Phase 1 of
# 2026-08-31-coord-mcp-status-is-a-stale-snapshot-with-an-untyped-cause and has
# not landed, so a breadcrumb never carries one of those words.
#
# The reason set is the runner's, not this script's, and it MOVES IN BOTH
# DIRECTIONS: the two middle reasons above landed in runner 38c337ba5
# (2026-08-19) and were missing from every document in this repo — this comment
# included — until 2026-08-28; then from 2026-08-31 to 2026-09-06 this comment
# claimed thirteen, seven of them verdicts the runner has never written.
# Re-derive it with scripts/breadcrumb-reason-drift.py rather than trusting any
# prose count, here or in the knowledge base — since 2026-09-06 that script
# runs in CI (.github/workflows/doc-transcription-parity.yml) against runner
# main, so a stale count here reddens a PR.
#
# NOT "the session never had a .mcp.json": `coord_mcp_safe_to_write` passes a
# workdir whose file is absent OR holds solely our own coord-mcp config, and the
# refusal arms return either side of that check (three of the eight never reach
# it) and none of them DELETES — so a RE-provision of a workdir that
# already carried one leaves the stale file in place. L1 probing a stale port or
# an evicted nonce is fully consistent with a "NOT written" breadcrumb.
#
# It NEVER changes the verdict. Three limits, all stated in the output rather
# than left to the reader:
#   - PRESENCE is spawn-time, and this reader SAYS HOW OLD. The runner clears it
#     (`clear_degraded_breadcrumb`) on a successful probe, but whether anything
#     re-evaluates it BETWEEN provisioning passes is a property of the runner
#     build, so a session that recovered on its own can keep a stale file. A
#     stamped breadcrumb carries `written_at` on line 2; past the TTL below it
#     is reported STALE — not as a fault, not as health, and never as a reason
#     to skip a probe. An UNSTAMPED (legacy) one has UNKNOWN age and gets the
#     same treatment, which is the common case while older builds are still on
#     the fleet. (A re-provision of the same workdir — a second terminal, a
#     looping agent — can clear or rewrite it on any build.)
#   - ABSENCE is UNKNOWN, never health. A healthy provision writes nothing, and
#     so does a hand-typed session, a workdir the runner never provisioned, and
#     a build predating the breadcrumb. One further silence is DELIBERATE and is
#     an absence over a LIVE door: when the non-clobber guard refuses a workdir
#     that already DECLARES a coord-mcp (an agent-JWT config held by the
#     no-downgrade guard, or a secondary runner declining the primary's
#     shared-root config) the runner writes nothing, because a permanent
#     never-cleared UNREACHABLE line in a session that works is worse than none.
#     Declared is only a `/mcpServers/coord-mcp` key test, never proof the door
#     answers, so a dead entry buys the same silence.
#     A non-device/agent `sub_type` is NOT in this list — that arm breadcrumbs
#     unconditionally; this comment claimed otherwise until 2026-08-28.
#     Reporting an absent breadcrumb as "coord-mcp was fine at spawn" would be
#     exactly the silent-empty-is-unknown error this script exists to stop
#     making.
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

# ----- Freshness: the file is a SNAPSHOT, so this reader must say WHEN --------
# A stamping runner appends a SECOND line — one JSON object carrying
# `written_at`, `workdir`, `port`, `verdict`, `build_id` and `schema`. Reading it
# is what lets a three-second-old verdict be told from a three-week-old one.
#
# THE SPLIT MUST HAPPEN BEFORE `one_line`, NOT AFTER. `one_line` maps every
# newline to a space, so piping the whole file through it CONCATENATES line 2
# onto line 1 — raw JSON glued to the end of the `BREADCRUMB:` line, and again
# inside the DEAD verdict block that is written to be pasted as evidence. The
# 4KB read cap and the 400-char flatten of line 1 are unchanged; only the order
# is. (This is why this script had to change BEFORE the stamping runner reached
# a box, not after.)
CRUMB_TTL_SECS=1800   # 30 min. Past it the breadcrumb EXPLAINS; it never CONCLUDES.

# read_crumb_meta: stdin = line 2. stdout = EXACTLY four lines — written_at,
# verdict, workdir, schema — or NOTHING when the line is not a JSON object.
# Emitting nothing rather than four empties is what lets the caller tell
# "unstamped or unparseable" from "stamped with a field missing"; an empty
# string that reads as a VALUE is the failure class read_eval_error exists for.
read_crumb_meta() {
  if [ "$JSON_READER" = jq ]; then
    jq -r 'if type == "object"
           then [(.written_at // ""), (.verdict // ""), (.workdir // ""), (.schema // "")]
                | map(if type == "string" then . else tostring end) | .[]
           else empty end' 2>/dev/null
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
if not isinstance(d, dict): sys.exit(0)
for k in ("written_at","verdict","workdir","schema"):
    v=d.get(k)
    if v is None: v=""
    if not isinstance(v,str): v=json.dumps(v)
    print(v.replace("\r"," ").replace("\n"," "))' 2>/dev/null
  fi
}

# crumb_age_secs <iso8601-utc> — whole seconds since that stamp, empty if it
# cannot be read. Deliberately NOT `date -d`: a BSD/macOS date rejects `-d` and
# would silently print nothing, which would read as "no stamp" over a perfectly
# good one. The reader this script already proved working does the arithmetic.
crumb_age_secs() {
  [ -n "${1:-}" ] || return 0
  if [ "$JSON_READER" = jq ]; then
    jq -rn --arg t "$1" 'try ((now | floor) - ($t | fromdateiso8601)) catch empty' 2>/dev/null
  else
    "$JSON_READER" -c 'import sys,time,calendar
s=sys.argv[1].strip()
for f in ("%Y-%m-%dT%H:%M:%SZ","%Y-%m-%dT%H:%M:%S"):
    try:
        print(int(time.time()) - calendar.timegm(time.strptime(s,f))); break
    except ValueError:
        continue' "$1" 2>/dev/null
  fi
}

# fmt_age <seconds> -> 45s / 4m / 2h13m / 3d4h. Coarse on purpose: the decision
# is fresh-vs-stale, and a precise age would invite reading it as precision
# about coord, which this file never had.
fmt_age() {
  local s=$1
  if   [ "$s" -lt 60 ];    then printf '%ds' "$s"
  elif [ "$s" -lt 3600 ];  then printf '%dm' "$((s / 60))"
  elif [ "$s" -lt 86400 ]; then printf '%dh%dm' "$((s / 3600))" "$(((s % 3600) / 60))"
  else printf '%dd%dh' "$((s / 86400))" "$(((s % 86400) / 3600))"
  fi
}

CRUMB_FILE="$PWD/.coord-mcp-status"
CRUMB=""            # line 1 — the statement every reader quotes
CRUMB_META=""       # line 2 — the JSON stamp, when the writing build made one
CRUMB_QUAL="age UNKNOWN"
CRUMB_NOTE=""
CRUMB_WRITTEN_AT=""
CRUMB_VERDICT=""
CRUMB_WORKDIR=""
CRUMB_SCHEMA=""
# Capped at READ time: the breadcrumb is two short lines, so a stray large file
# here costs a 4KB read rather than a full slurp into a shell variable.
if [ -r "$CRUMB_FILE" ]; then
  CRUMB_RAW="$(head -c 4096 < "$CRUMB_FILE" 2>/dev/null)"
  CRUMB="$(printf '%s\n' "$CRUMB_RAW" | sed -n '1p' | one_line 400)"
  CRUMB_META="$(printf '%s\n' "$CRUMB_RAW" | sed -n '2p' | one_line 2048)"
fi
if [ -n "$CRUMB" ]; then
  CRUMB_AGE=""
  if [ -n "$CRUMB_META" ]; then
    CRUMB_FIELDS="$(printf '%s' "$CRUMB_META" | read_crumb_meta)"
    if [ -n "$CRUMB_FIELDS" ]; then
      CRUMB_WRITTEN_AT="$(printf '%s\n' "$CRUMB_FIELDS" | sed -n '1p')"
      CRUMB_VERDICT="$(printf '%s\n' "$CRUMB_FIELDS" | sed -n '2p')"
      CRUMB_WORKDIR="$(printf '%s\n' "$CRUMB_FIELDS" | sed -n '3p')"
      CRUMB_SCHEMA="$(printf '%s\n' "$CRUMB_FIELDS" | sed -n '4p')"
      CRUMB_AGE="$(crumb_age_secs "$CRUMB_WRITTEN_AT")"
      # Non-numeric OR NEGATIVE is UNKNOWN, not zero: a stamp in the future is a
      # clock disagreement, and "0s old" would be the freshest possible reading
      # of the least trustworthy possible file.
      case "$CRUMB_AGE" in ''|*[!0-9]*) CRUMB_AGE="" ;; esac
    fi
  fi
  if [ -z "$CRUMB_AGE" ]; then
    if [ -z "$CRUMB_META" ]; then
      CRUMB_NOTE=" [LEGACY: line 1 only, no stamp - written by a runner build predating the stamped breadcrumb, so its age is UNKNOWN. Treated exactly like an expired one: STALE, NOT evidence of the current state. The probes below decide.]"
    else
      CRUMB_NOTE=" [UNSTAMPED AGE: line 2 is present but carries no readable written_at (unparseable, or a clock ahead of this one), so its age is UNKNOWN. STALE - NOT evidence of the current state. The probes below decide.]"
    fi
  elif [ "$CRUMB_AGE" -gt "$CRUMB_TTL_SECS" ]; then
    CRUMB_QUAL="age $(fmt_age "$CRUMB_AGE")"
    CRUMB_NOTE=" [STALE: older than the ${CRUMB_TTL_SECS}s TTL, so it is NOT evidence of the current state - it explains, it does not conclude. The probes below decide.]"
  else
    CRUMB_QUAL="age $(fmt_age "$CRUMB_AGE")"
    CRUMB_NOTE=" [within the ${CRUMB_TTL_SECS}s TTL - still SPAWN-TIME evidence about ONE provisioning pass, never a probe of coord now.]"
  fi
  [ -n "$CRUMB_VERDICT" ] && CRUMB_QUAL="$CRUMB_QUAL, verdict $CRUMB_VERDICT"
  [ -n "$CRUMB_WORKDIR" ] && CRUMB_QUAL="$CRUMB_QUAL, workdir $CRUMB_WORKDIR"
  if [ -n "$CRUMB_WORKDIR" ] && [ "$CRUMB_WORKDIR" != "$PWD" ]; then
    CRUMB_NOTE="$CRUMB_NOTE [WORKDIR MISMATCH: it describes $CRUMB_WORKDIR, not this cwd ($PWD). The runner writes into the workdir IT provisioned, which from a linked worktree is often the primary checkout - so this is another directory's evidence.]"
  fi
  if [ -n "$CRUMB_SCHEMA" ] && [ "$CRUMB_SCHEMA" != "1" ]; then
    CRUMB_NOTE="$CRUMB_NOTE [SCHEMA $CRUMB_SCHEMA: this reader knows schema 1, so anything beyond written_at/verdict/workdir may be misread - open the file yourself.]"
  fi
  echo "BREADCRUMB ($CRUMB_QUAL): $CRUMB_FILE (runner, spawn-time): $CRUMB$CRUMB_NOTE" >&2
else
  echo "BREADCRUMB: none in this cwd ($CRUMB_FILE: absent, unreadable or empty) - UNKNOWN, not health. A healthy provision writes nothing, and so does a workdir the runner never provisioned. The runner writes into the workdir IT provisioned, which on a linked worktree may be the primary checkout rather than here." >&2
fi

# ----- The APPROVAL half: DECLARED is not the same as ALLOWED TO LOAD ----------
# The whole cascade below probes ONE half of the wiring. `.mcp.json` DECLARES the
# coord-mcp server; a settings key APPROVES it, and Claude Code will not load a
# project-scoped server it has not approved. The two halves fail INDEPENDENTLY
# and only the declaration half leaves a `.coord-mcp-status` breadcrumb, so an
# absent `coord_*` tool has at least two causes and everything above this block
# can distinguish exactly one of them.
#
# That is not a hypothetical: PR #370 restored the approval key after PR #256
# deleted it, and closed with the gap this block fills -- "/coord-revive cannot
# name this failure mode. Its L1 door re-reads .mcp.json, which comes back
# healthy in exactly this case, so the cascade would report a LIVE transport
# while tools stay masked." A recovery tool that reports LIVE at the exact moment
# an agent has no coord tools is the mask this script exists to replace, wearing
# a success label -- the same defect class as the false DEADs the cascade already
# guards against, with the sign flipped.
#
# WHAT THIS BLOCK IS NOT. It reports what it READ; it does not reproduce Claude
# Code's resolution and never claims to. Three documented facts make a
# file-reading reporter unable to decide approval on its own, and all three are
# printed rather than silently assumed:
#
#   1. A repository's OWN approvals are gated on WORKSPACE TRUST. An approval in
#      the project's `.claude/settings.json` does not count in a folder you have
#      not trusted: interactively "Claude Code asks you before connecting them.
#      The repository's own approvals don't count". Approvals from `~/.claude.json`
#      and from managed settings still apply there.
#   2. Trust is keyed on the GIT REPOSITORY ROOT, and in a linked worktree on the
#      MAIN CHECKOUT's root -- which is why the key below is resolved from
#      `--git-common-dir` rather than from $PWD. It is stored as
#      `projects["<root>"].hasTrustDialogAccepted` in `~/.claude.json`, the key
#      the docs name for trusting a folder by hand.
#   3. In a `claude -p` run or an SDK session the trust dialog never appears and
#      project servers are "connected without asking, approved or not". So a
#      HELD_UNTRUSTED reading is NOT a prediction that your tools are masked --
#      it is a statement about one input, in a session type this script cannot
#      observe from inside a shell.
#
# The one asymmetry worth acting on: `disabledMcpjsonServers` "still rejects the
# server, even in an untrusted folder", in any settings file and every permission
# mode. It is the only reading here that is decisive on its own, which is why it
# outranks every other token in the summary.
#
# IT NEVER CHANGES THE VERDICT, for the same reason the breadcrumb does not: the
# doors below are a DIFFERENT transport from the session's native coord_* tools,
# and the approval half governs only the latter. A LIVE door plus a withheld
# approval is a perfectly coherent pair -- and it is precisely the pair no line
# in this script could previously print.
#
# $HOME_DIR is hoisted here from L4 (source 2 re-uses it); $USERPROFILE is the
# documented fallback rather than an improvisation -- see the note there.
# $OWN is hoisted for the same reason: this block and L1 read the same file, and
# a second variable naming one path is how two readers of "the config" drift.
HOME_DIR="${HOME:-${USERPROFILE:-}}"
OWN="$PWD/.mcp.json"

# WHERE CLAUDE CODE ACTUALLY KEEPS THESE TWO FILES.
#
# $CLAUDE_CONFIG_DIR relocates BOTH of them, and reading the home-derived paths
# on a machine that sets it is not a near miss -- it is a different file with a
# different answer. Measured on the operator box 2026-08-30, where
# CLAUDE_CONFIG_DIR=C:\claude\.claude-tiohorst:
#
#   ~/.claude.json                    mtime 2026-07-17,  7 projects, 1 trusted,
#                                     NO entry for the config repo
#   $CLAUDE_CONFIG_DIR/.claude.json   mtime 2026-08-30, 12 projects, 8 trusted,
#                                     config repo hasTrustDialogAccepted: true
#
# Reading the first reports `noentry` -> APPROVAL_TRUST_UNKNOWN for a folder
# whose trust IS recorded -- the silent always-UNKNOWN this file calls the worst
# shape a diagnostic can take, on the very machine the block was written for.
# This class has burned this fleet before and is recorded in
# `.claude/commands/cleanup-steward.md`: a flag written to ~/.claude/settings.json
# while $CLAUDE_CONFIG_DIR pointed elsewhere, so the file Claude Code loads never
# carried it.
#
# THE TWO SHAPES DIFFER, so this is a per-file resolution and not one prefix
# swap. Unset: settings at `~/.claude/settings.json`, store at `~/.claude.json`
# -- a directory apart. Set: BOTH sit directly in $CLAUDE_CONFIG_DIR (verified
# on the operator box, both files present).
#
# Backslashes normalised -- the variable arrives as `C:\claude\...` on Windows
# while every other path here is forward-slash. Same handling as
# `scripts/lib/claude-registry-name.sh`.
#
# Empty means UNRESOLVED, not "use the default": with neither $CLAUDE_CONFIG_DIR
# nor a home directory there is no path to read, and the layer says so rather
# than reporting an absence it never looked for.
if [ -n "${CLAUDE_CONFIG_DIR:-}" ]; then
  CC_DIR="${CLAUDE_CONFIG_DIR//\\//}"
  CC_SETTINGS="$CC_DIR/settings.json"
  CC_STORE="$CC_DIR/.claude.json"
elif [ -n "$HOME_DIR" ]; then
  CC_SETTINGS="$HOME_DIR/.claude/settings.json"
  CC_STORE="$HOME_DIR/.claude.json"
else
  CC_SETTINGS=""
  CC_STORE=""
fi

# approval_keys -- reads ONE settings-shaped JSON doc on STDIN and prints three
# space-separated tokens: <enabled> <all> <disabled>.
#
#   enabled/disabled : named | unnamed | absent | badtype
#   all              : true | false | absent | badtype
#   whole line       : badjson, when the document does not parse
#
# STDIN, not a path argument, for the reason spelled out on read_cfg: under an
# inherited MSYS_NO_PATHCONV a POSIX path reaches a NATIVE jq.exe/python.exe
# unconverted, the open fails, and every field comes back EMPTY -- which here
# would print "no approval found anywhere" for a machine that is correctly
# approved. Same wording-in-one-place discipline as cfg_shape_token: both readers
# emit TOKENS and the prose lives below, so the two arms cannot drift.
#
# The `type` guards are load-bearing, not defensive noise: `enabledMcpjsonServers`
# set to a string rather than an array is a real hand-edit mistake, and reporting
# it as `absent` would name a cause that is WRONG rather than merely missing.
approval_keys() {
  if [ "$JSON_READER" = jq ]; then
    jq -r '
      def arr(k): (.[k]) as $v
        | if $v == null then "absent"
          elif ($v | type) != "array" then "badtype"
          elif ($v | index("coord-mcp")) != null then "named"
          else "unnamed" end;
      def flag: (.enableAllProjectMcpServers) as $v
        | if $v == null then "absent"
          elif ($v | type) != "boolean" then "badtype"
          elif $v then "true" else "false" end;
      if type != "object" then "badjson"
      else arr("enabledMcpjsonServers") + " " + flag + " " + arr("disabledMcpjsonServers") end
      ' 2>/dev/null || echo badjson
  else
    "$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print("badjson"); sys.exit(0)
if not isinstance(d,dict): print("badjson"); sys.exit(0)
def arr(k):
    v=d.get(k)
    if v is None: return "absent"
    if not isinstance(v,list): return "badtype"
    return "named" if "coord-mcp" in v else "unnamed"
f=d.get("enableAllProjectMcpServers")
flag="absent" if f is None else ("badtype" if not isinstance(f,bool) else ("true" if f else "false"))
print(arr("enabledMcpjsonServers"),flag,arr("disabledMcpjsonServers"))' 2>/dev/null || echo badjson
  fi
}

# claude_json_project -- the same three tokens plus a trust token, read out of
# the `projects` map of `~/.claude.json` for ONE key.
#
# The key travels in the ENVIRONMENT, never on argv -- which keeps a filesystem
# layout off the process list, the habit `.claude/**` is linted for.
#
# That is NOT because the environment is a safe channel for a path. This comment
# claimed until 2026-08-30 that "an env value is not path-converted", and the
# claim is false: MSYS rewrites a POSIX-LOOKING value on its way into a native
# child through EITHER channel. Measured under Git Bash with jq 1.8.2, and again
# with the python fallback, which saw the identical rewrite. The caller is what
# makes this channel safe -- it names the variable in `MSYS2_ENV_CONV_EXCL`
# before spawning the reader. Read that block before changing how the key
# arrives; dropping the exclusion reintroduces a permanent, silent `noentry`.
#
#   trust : accepted | declined | noentry | nomap
#
# `noentry` and `declined` are kept APART on purpose. "No entry at all" is a
# folder Claude Code has never been started in; "false" is one it has, and where
# trust was not granted. Collapsing them would report a never-visited path as a
# refusal.
claude_json_project() {
  if [ "$JSON_READER" = jq ]; then
    jq -r '
      def arr($o; k): ($o[k]) as $v
        | if $v == null then "absent"
          elif ($v | type) != "array" then "badtype"
          elif ($v | index("coord-mcp")) != null then "named"
          else "unnamed" end;
      (env.COORD_REVIVE_TRUST_KEY // "") as $k
      | if type != "object" or ((.projects // null) | type) != "object" then "nomap absent absent absent"
        elif (.projects | has($k)) | not then "noentry absent absent absent"
        else (.projects[$k]) as $p
          | (if ($p | type) != "object" then "nomap"
             elif ($p.hasTrustDialogAccepted // false) == true then "accepted"
             else "declined" end)
            + " " + arr($p; "enabledMcpjsonServers")
            + " " + (($p.enableAllProjectMcpServers) as $f
                     | if $f == null then "absent"
                       elif ($f | type) != "boolean" then "badtype"
                       elif $f then "true" else "false" end)
            + " " + arr($p; "disabledMcpjsonServers")
        end' 2>/dev/null || echo "badjson absent absent absent"
  else
    "$JSON_READER" -c 'import json,os,sys
try: d=json.load(sys.stdin)
except Exception: print("badjson absent absent absent"); sys.exit(0)
k=os.environ.get("COORD_REVIVE_TRUST_KEY","")
pr=d.get("projects") if isinstance(d,dict) else None
if not isinstance(pr,dict): print("nomap absent absent absent"); sys.exit(0)
if k not in pr: print("noentry absent absent absent"); sys.exit(0)
p=pr[k]
if not isinstance(p,dict): print("nomap absent absent absent"); sys.exit(0)
def arr(key):
    v=p.get(key)
    if v is None: return "absent"
    if not isinstance(v,list): return "badtype"
    return "named" if "coord-mcp" in v else "unnamed"
f=p.get("enableAllProjectMcpServers")
flag="absent" if f is None else ("badtype" if not isinstance(f,bool) else ("true" if f else "false"))
print(("accepted" if p.get("hasTrustDialogAccepted") is True else "declined"),arr("enabledMcpjsonServers"),flag,arr("disabledMcpjsonServers"))' 2>/dev/null || echo "badjson absent absent absent"
  fi
}

# resolve_main_checkout -- the folder workspace trust is KEYED on.
#
# NOT $PWD and NOT --show-toplevel. The documented rule is the git repository
# root, and "in a worktree, it uses the MAIN checkout's root". `--git-common-dir`
# points at the main repo's .git from a linked worktree and from the canonical
# checkout alike, so ONE dirname yields the main checkout in both -- the same
# anchor resolve_root() uses, stopped one level earlier. Reading the worktree's
# own path instead would look up a `projects` key that has never existed and
# report `noentry` for a repository whose trust was granted years ago.
#
# $PWD ONLY -- and this is a DELIBERATE divergence from resolve_root(), which
# falls back to $HERE. The two are answering different questions. resolve_root
# hunts for the fleet's workspace root, and any anchor that finds it is as good
# as another. Trust is a property of the folder the SESSION is in: outside a
# repository the documented key is "the directory you started from". $HERE is
# where this skill file happens to live -- always inside qontinui-claude-config
# -- so accepting it as a fallback would report THAT repository's trust for a
# session running somewhere else entirely, which is a named-but-WRONG cause and
# strictly worse than the honest $PWD the caller falls back to.
# It is right for a normal checkout (`<repo>/.git` -> `<repo>`), from a
# subdirectory of one, and for a linked worktree (the common dir is the MAIN
# repo's `.git` wherever you stand). It is NOT right under `--separate-git-dir`
# or in a bare repo, where the git dir is not `<checkout>/.git` and the dirname
# lands on an unrelated parent. Neither shape exists on this fleet; a wrong key
# there reports `noentry`, which the output already states as UNKNOWN rather
# than as a refusal, so the failure mode is a shrug and not a false verdict.
resolve_main_checkout() {
  local gc out
  gc="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || { printf ''; return; }
  case "$gc" in ""|"."|"..") printf ''; return ;; esac
  out="$(dirname "$gc")"
  # Guarded AFTER the dirname as well as before it. `--path-format=absolute` is
  # honoured by every git this fleet runs, but a git that accepted the flag and
  # returned a bare `.git` would leave `dirname` = `.`, which is NON-EMPTY -- so
  # the caller's `-n` test passes, TRUST_KEY becomes "." and every lookup
  # silently reports `noentry` for a machine whose trust is recorded. The
  # pre-dirname guard was inherited from resolve_root() and does not cover this.
  case "$out" in ""|"."|"..") printf ''; return ;; esac
  printf '%s' "$out"
}

# read_one <file> — bash opens it, so no path crosses to a native reader.
#
# A leading UTF-8 BOM is stripped, because the two readers DISAGREE about one.
# Measured 2026-08-30: `/c/Users/<windows-user>/.claude/settings.json` starts EF BB BF
# (PowerShell writes it that way); jq 1.8.2 parses it fine, `python -c
# json.load` fails with "Expecting value: line 1 column 1". Without this the
# same file reads as real settings on a jq box and as `does not parse as JSON`
# on a python one -- a platform-dependent answer from a tool whose whole point
# is a typed, reproducible cause.
#
# The BOM bytes are built with printf rather than written literally, so this
# file stays pure ASCII for the lint that checks exactly that.
approval_read() {
  [ -r "$1" ] || return 1
  local content bom
  content="$(cat < "$1" 2>/dev/null)" || return 1
  bom="$(printf '\357\273\277')"
  printf '%s' "${content#"$bom"}"
}

APPROVAL_LINES=""
approval_note() { APPROVAL_LINES="${APPROVAL_LINES}APPROVAL: $1
"; }

# The declaration half, restated here in the approval block's own terms. This is
# only a `/mcpServers/coord-mcp` KEY test -- the same test the runner's
# non-clobber guard makes, and never proof the door answers. L1 above is what
# says whether it answers; the point here is that with NO declaration there is
# nothing for a settings key to approve, so a missing approval is not the story.
APPROVAL_DECLARED="unreadable"
if [ -r "$OWN" ]; then
  if [ "$JSON_READER" = jq ]; then
    APPROVAL_DECLARED="$(jq -r 'if (type == "object") and (((.mcpServers // null) | type) == "object") and (.mcpServers | has("coord-mcp")) then "yes" else "no" end' < "$OWN" 2>/dev/null || echo badjson)"
  else
    APPROVAL_DECLARED="$("$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print("badjson"); sys.exit(0)
s=d.get("mcpServers") if isinstance(d,dict) else None
print("yes" if isinstance(s,dict) and "coord-mcp" in s else "no")' < "$OWN" 2>/dev/null || echo badjson)"
  fi
  [ -n "$APPROVAL_DECLARED" ] || APPROVAL_DECLARED="badjson"
fi
case "$APPROVAL_DECLARED" in
  yes)        approval_note "declaration $OWN: coord-mcp DECLARED (a /mcpServers/coord-mcp key test only - L1 above is what says whether it ANSWERS)" ;;
  no)         approval_note "declaration $OWN: no coord-mcp entry - there is nothing here for a settings key to approve, so the missing half is the DECLARATION (see the breadcrumb above), not the approval" ;;
  badjson)    approval_note "declaration $OWN: does not parse as JSON - the declaration half is UNKNOWN from here" ;;
  *)          approval_note "declaration $OWN: absent or unreadable - UNKNOWN, and the same absence the breadcrumb above describes" ;;
esac

# The trust anchor, and the settings layers, in the documented precedence order:
# managed settings, then `--settings`, then project-local, then shared project,
# then user. Managed settings and `--settings` are deliberately NOT guessed at --
# their locations are platform- and invocation-specific, and inventing a path to
# report "absent" for would be a named-but-wrong cause.
TRUST_KEY="$(resolve_main_checkout)"
[ -n "$TRUST_KEY" ] || TRUST_KEY="$PWD"
APPROVAL_ENABLED_IN=""   # layers naming coord-mcp in an approving key
APPROVAL_UNGATED=""      # ...of those, the ones workspace trust does NOT gate
APPROVAL_DISABLED_IN=""  # layers naming it in disabledMcpjsonServers

approval_layer() {  # <label> <path> <trust-gated: yes|no>
  local label="$1" path="$2" gated="$3" toks e a dis
  if [ ! -r "$path" ]; then
    approval_note "$label $path: absent or unreadable - a statement of ABSENCE, not of a withheld approval"
    return
  fi
  toks="$(approval_read "$path" | approval_keys | one_line 200)"
  # `read`, not `set --`: this runs at script scope via its caller and clobbering
  # the positional parameters is a side effect a diagnostic must not have.
  read -r e a dis <<<"$toks"
  [ -n "$e" ] || e="badjson"; [ -n "$a" ] || a="absent"; [ -n "$dis" ] || dis="absent"
  if [ "$e" = "badjson" ]; then
    # No claim about what Claude Code would make of the same file. This line used
    # to add "and Claude Code would not read an approval out of it either",
    # which nothing here tested -- and a BOM'd settings file, the most likely way
    # to reach this arm on Windows, is one Claude Code plainly does load. UNKNOWN
    # is the honest stopping point; asserting the consequence is a named-but-wrong
    # cause.
    approval_note "$label $path: does not parse as JSON for this reader - UNKNOWN"
    return
  fi
  approval_note "$label $path: enabledMcpjsonServers=$e enableAllProjectMcpServers=$a disabledMcpjsonServers=$dis"
  if [ "$e" = "named" ] || [ "$a" = "true" ]; then
    APPROVAL_ENABLED_IN="$APPROVAL_ENABLED_IN $label"
    [ "$gated" = "yes" ] || APPROVAL_UNGATED="$APPROVAL_UNGATED $label"
  fi
  [ "$dis" = "named" ] && APPROVAL_DISABLED_IN="$APPROVAL_DISABLED_IN $label"
  return 0
}

# Project-supplied layers are read at $PWD -- the SAME anchor as the `.mcp.json`
# above, and deliberately NOT the trust key.
#
# Those are two different questions and conflating them was this block's first
# bug (caught in pre-PR review). Project settings are read from the PROJECT
# directory; only TRUST is keyed on the repository root and, in a worktree, on
# the main checkout. Reading the approval at the main checkout while reading the
# declaration at $PWD meant a session in `agent-worktrees/<uuid>` whose own
# `.claude/settings.local.json` approves coord-mcp would be told
# NO_APPROVAL_FOUND -- a false negative on a correctly approved session, and on
# this fleet the DEFAULT path rather than an edge, since sessions run under
# QONTINUI_AGENT_WORKTREE_MODE=1. `settings.local.json` is per-machine and
# gitignored, so it is also the file most likely to differ between the two.
approval_layer "project-local" "$PWD/.claude/settings.local.json" yes
approval_layer "project-shared" "$PWD/.claude/settings.json" yes
if [ -n "$CC_SETTINGS" ]; then
  # UNGATED, and this one is quoted rather than reasoned to: the approvals that
  # "still apply in an untrusted folder" are `~/.claude/settings.json`, managed
  # settings, and settings passed with `--settings`. This is that first entry --
  # at whichever path $CLAUDE_CONFIG_DIR puts it.
  approval_layer "user" "$CC_SETTINGS" no
else
  approval_note "user settings: neither \$CLAUDE_CONFIG_DIR nor \$HOME/\$USERPROFILE is set, so this layer has no path to read - a LOCAL environment fault (HOME_UNRESOLVED), not an absent approval"
fi

# The user store (`.claude.json`) is where the INTERACTIVE approval actually
# lands, and it is one of the sources that keeps applying in an untrusted folder
# -- so it is read even though it is not a settings file. It also carries the
# trust bit itself. $CC_STORE, not a home-derived path: see the resolution block
# above for the measurement that made that distinction load-bearing.
APPROVAL_TRUST="unknown"
if [ -n "$CC_STORE" ] && [ -r "$CC_STORE" ]; then
  # EXPORTED, not an assignment-prefix. `VAR=x cmd1 | cmd2` sets VAR for cmd1
  # ONLY, and the reader is cmd2 -- the key would arrive empty, no `projects`
  # entry would ever match, and every machine would report `noentry` (UNKNOWN)
  # for a folder whose trust is recorded. A silent always-UNKNOWN is the worst
  # shape a diagnostic can take: it looks like an answer.
  #
  # ...and EXCLUDED from MSYS's env path conversion, which is the half this
  # comment used to get WRONG. `claude_json_project`'s header claimed an env
  # value "is not path-converted" and that argv was the only hazard. Measured on
  # the operator box 2026-08-30, jq 1.8.2 under Git Bash: with the shell holding
  # `/tmp/tmp.ABC/c5/proj`, native jq read `env.X` as
  # `C:/Users/<windows-user>/AppData/Local/Temp/tmp.ABC/c5/proj`. MSYS rewrites
  # POSIX-LOOKING VALUES on the way into a native child whether they ride on argv
  # or in the environment -- the python fallback reader saw the identical
  # rewrite, so this is the MSYS runtime, not a jq behaviour.
  #
  # The rewritten key then matches no `projects` entry and the block reports
  # `noentry` forever: exactly the silent always-UNKNOWN the paragraph above
  # calls the worst shape a diagnostic can take, arrived at by a different route.
  #
  # NOT live on this fleet, and the reason is worth stating so nobody "simplifies"
  # this away: TRUST_KEY normally comes from `--git-common-dir`, which already
  # yields a native `D:/...` spelling that MSYS passes through untouched
  # (measured in the same run). The bug bites where the key is POSIX-spelled --
  # a session started outside a repo under an MSYS mount, and the self-test's own
  # `mktemp -d` sandbox, which is what caught it.
  #
  # Scoped to a COMMAND SUBSTITUTION subshell so both variables die with it.
  # MSYS2_ENV_CONV_EXCL is read by the MSYS runtime for every native child, and
  # the L1-L4 doors below spawn native curl -- widening their environment as a
  # side effect of a settings read is not a trade this block gets to make.
  # Appended rather than assigned, so an outer value keeps its entries.
  CJ_TOKS="$(
    export COORD_REVIVE_TRUST_KEY="$TRUST_KEY"
    case ";${MSYS2_ENV_CONV_EXCL-};" in
      *";COORD_REVIVE_TRUST_KEY;"*) ;;
      *) export MSYS2_ENV_CONV_EXCL="${MSYS2_ENV_CONV_EXCL:+$MSYS2_ENV_CONV_EXCL;}COORD_REVIVE_TRUST_KEY" ;;
    esac
    approval_read "$CC_STORE" | claude_json_project | one_line 200
  )"
  # EMPTY means the reader produced nothing, and that is NOT the same as "not
  # read". jq exits 0 printing NOTHING on empty input, so its `|| echo badjson`
  # fallback cannot fire -- a zero-byte ~/.claude.json would report
  # "was not read" on CI (jq) and "badjson" on Windows (python), for the same
  # file. Normalising here keeps the two readers on one vocabulary; the guard
  # belongs at the call site because only the caller knows the file WAS opened.
  [ -n "$CJ_TOKS" ] || CJ_TOKS="badjson absent absent absent"
  read -r APPROVAL_TRUST CJ_E CJ_A CJ_D <<<"$CJ_TOKS"
  [ -n "$APPROVAL_TRUST" ] || APPROVAL_TRUST="badjson"
  [ -n "$CJ_E" ] || CJ_E="absent"; [ -n "$CJ_A" ] || CJ_A="absent"; [ -n "$CJ_D" ] || CJ_D="absent"
  # The RESOLVED path, not the literal `~/.claude.json` this line used to print.
  # Every other layer names the file it opened; this one did not, so the single
  # line that would let a reader notice the wrong store was read was the one line
  # that hid it -- which is how the $CLAUDE_CONFIG_DIR bug above survived being
  # looked at.
  approval_note "user-store $CC_STORE projects[$TRUST_KEY]: enabledMcpjsonServers=$CJ_E enableAllProjectMcpServers=$CJ_A disabledMcpjsonServers=$CJ_D"
  # Counted as an APPROVAL, but NOT as an ungated one. `~/.claude.json` is where
  # the interactive approval lands and it is plainly a user-level store, so it is
  # tempting to file it beside the user settings file above -- but the documented
  # list of what still applies in an untrusted folder names
  # `~/.claude/settings.json`, managed settings and `--settings`, and not this.
  # Asserting ungated here would be extending a citation rather than reading one,
  # which is the one thing the summary must not do. Left gated, an approval found
  # only here reports TRUST_UNKNOWN instead of a confident APPROVED_UNGATED --
  # the conservative direction, and it says which fact it is missing.
  if [ "$CJ_E" = "named" ] || [ "$CJ_A" = "true" ]; then
    APPROVAL_ENABLED_IN="$APPROVAL_ENABLED_IN user-store"
  fi
  [ "$CJ_D" = "named" ] && APPROVAL_DISABLED_IN="$APPROVAL_DISABLED_IN user-store"
else
  approval_note "user-store ${CC_STORE:-<unresolved: neither \$CLAUDE_CONFIG_DIR nor \$HOME/\$USERPROFILE is set>}: absent or unreadable - the trust bit and any stored approval are UNKNOWN from here"
fi
case "$APPROVAL_TRUST" in
  accepted) approval_note "trust $TRUST_KEY: ACCEPTED (hasTrustDialogAccepted true) - repository-supplied approvals count in this folder" ;;
  declined) approval_note "trust $TRUST_KEY: NOT accepted (hasTrustDialogAccepted is not true - either explicitly false, or absent from an entry that does exist; this reading does not separate those) - a repository's OWN approvals do not count here, though the user store and managed settings still do" ;;
  noentry)  approval_note "trust $TRUST_KEY: no projects entry at all - UNKNOWN, and NOT the same as a refusal: this is a folder Claude Code has no record of being started in. Trust is keyed on the git repository root, and on the MAIN checkout's root from a worktree, which is the key resolved here" ;;
  nomap|badjson) approval_note "trust $TRUST_KEY: the user store has no readable projects map - UNKNOWN" ;;
  *)        approval_note "trust $TRUST_KEY: UNKNOWN (~/.claude.json was not read)" ;;
esac

# Trim the leading space the accumulators carry, so a summary reads
# "(project-shared)" rather than "( project-shared)".
APPROVAL_ENABLED_IN="${APPROVAL_ENABLED_IN# }"
APPROVAL_UNGATED="${APPROVAL_UNGATED# }"
APPROVAL_DISABLED_IN="${APPROVAL_DISABLED_IN# }"

# The summary.
#
# THE DECLARATION IS TESTED FIRST, ahead of the rejection. Those are not
# competing claims about one question -- NOT_APPLICABLE is a statement about the
# declaration and REJECTED about the approval -- and putting the rejection first
# printed "Remove the entry; no door below can work around it" for a server that
# is not declared here at all, suppressing the line that points at the half
# actually missing. Within the approval question the rejection still outranks
# everything, because it is the one reading that is decisive on its own.
#
# `$APPROVAL_DECLARED` has FOUR values, not two. Testing it for exactly "no"
# let `badjson` and the `unreadable` initialiser fall through to the final else,
# whose text asserts "coord-mcp is declared" -- flatly contradicting the
# declaration line printed moments earlier on stderr, and reachable by simply
# running this from a directory with no `.mcp.json`, which is what the knowledge
# base tells people to do in the sibling repos. A missing declaration and an
# unreadable one are different answers and get different tokens.
if [ "$APPROVAL_DECLARED" = "no" ]; then
  APPROVAL_VERDICT="NOT_APPLICABLE - no coord-mcp is declared in this cwd, so there is no project-scoped server here to approve. The missing half is the DECLARATION; read the breadcrumb line above, not this block."
elif [ "$APPROVAL_DECLARED" != "yes" ]; then
  APPROVAL_VERDICT="DECLARATION_UNKNOWN - this cwd's .mcp.json was absent, unreadable or unparseable ($APPROVAL_DECLARED), so whether there is anything here to approve is UNKNOWN and the approval half cannot be summarised. That is a statement about THIS directory only; the layer readings above still stand on their own."
elif [ -n "$APPROVAL_DISABLED_IN" ]; then
  APPROVAL_VERDICT="REJECTED - coord-mcp is named in disabledMcpjsonServers ($APPROVAL_DISABLED_IN). That rejects the server in EVERY permission mode and in an untrusted folder too, so it outranks every approval above. Remove the entry; no door below can work around it."
elif [ -n "$APPROVAL_UNGATED" ]; then
  APPROVAL_VERDICT="APPROVED_UNGATED - an approval sits in a layer workspace trust does not gate ($APPROVAL_UNGATED), so it applies whether or not this folder is trusted."
elif [ -n "$APPROVAL_ENABLED_IN" ] && [ "$APPROVAL_TRUST" = accepted ]; then
  APPROVAL_VERDICT="APPROVED_TRUST_GATED - the only approval found is trust-gated ($APPROVAL_ENABLED_IN) and this folder IS trusted, so it counts. It would stop counting in a folder that is not."
elif [ -n "$APPROVAL_ENABLED_IN" ] && [ "$APPROVAL_TRUST" = declined ]; then
  # "has an entry whose bit is not true", NOT "the bit is present and not
  # accepted". The reader's `declined` arm is `hasTrustDialogAccepted is not
  # true`, which covers an explicit false AND a key absent from an entry that
  # exists; it never tested presence. Claiming presence here would assert a fact
  # no reading produced -- the same overstatement the noentry/declined split two
  # hundred lines up exists to avoid, one field further in.
  APPROVAL_VERDICT="APPROVAL_HELD_UNTRUSTED - the only approval found is trust-gated ($APPROVAL_ENABLED_IN) and this folder HAS a projects entry whose hasTrustDialogAccepted is not true (explicitly false, or absent from that entry - this does not separate them). Interactively a repository's own approvals do not count until the folder is trusted. This is NOT a prediction that your tools are masked: a claude -p or SDK session connects project servers without asking, approved or not."
elif [ -n "$APPROVAL_ENABLED_IN" ]; then
  # `noentry`, `nomap`, `badjson`, `unknown` -- every state that is not a
  # recorded refusal. This block keeps `noentry` and `declined` apart two
  # hundred lines up, on the grounds that a never-visited folder is not a
  # refusal; collapsing them back together HERE, in the one token an agent
  # greps for, would undo exactly that care.
  APPROVAL_VERDICT="APPROVAL_TRUST_UNKNOWN - the only approval found is trust-gated ($APPROVAL_ENABLED_IN) and this folder's trust could not be established (trust=$APPROVAL_TRUST). That is UNKNOWN, NOT a refusal: nothing here observed a withheld trust. Read the trust line above for which of absent-entry, absent-map and unparseable it was."
else
  APPROVAL_VERDICT="NO_APPROVAL_FOUND - coord-mcp is declared but no readable layer names it in enabledMcpjsonServers and none sets enableAllProjectMcpServers. UNKNOWN rather than proof: managed settings and a --settings file are real approval sources this script does not guess at a path for."
fi
approval_note "$APPROVAL_VERDICT"
approval_note "These are UNIONED readings across the layers, NOT a resolved effective value: an approval in ANY readable layer counts, so a higher-precedence enableAllProjectMcpServers=false is printed above and then not subtracted. Only disabledMcpjsonServers is treated as decisive, and only because it is documented as rejecting the server from any settings file. Where the layers disagree, read the per-layer lines rather than the summary token."
approval_note "This block READ files; it does not reproduce Claude Code's own resolution and never changes the verdict below. The doors below are a DIFFERENT transport from your native coord_* tools - a LIVE door beside a withheld approval is a coherent pair, and it is the pair no line here could print before."
printf '%s' "$APPROVAL_LINES" >&2

# ----- L1: re-read OWN cwd's .mcp.json ----------------------------------------
# THE SWEEP BUDGET STARTS HERE, not at process start. Everything above this line
# is local file reads (the approval layers, the breadcrumb) that no probe budget
# should be charged for: a slow filesystem must not spend the allowance that
# exists to bound NETWORK doors, or the run would report doors skipped for a
# reason that has nothing to do with them.
PROBE_BUDGET_T0=$SECONDS
# $OWN is set in the approval block above -- one variable, one path, two readers.
if [ -r "$OWN" ] && read_cfg "$OWN"; then
  seen_door "$OWN"
  seen_endpoint "$CFG_URL" "$CFG_KEY_HEADER" "$CFG_KEY" || :
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
  # (url, auth) dedup — a DIFFERENT file naming a door already probed is not a
  # second door. Must follow read_cfg (it sets CFG_*) and precede probe_door.
  if seen_endpoint "$CFG_URL" "$CFG_KEY_HEADER" "$CFG_KEY"; then
    echo "L2: $f -> same door as an earlier candidate ($CFG_URL) - not re-probed" >&2
    continue
  fi
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
# $HOME_DIR itself is resolved once, in the approval block above (this same
# idiom); source 2 only names the file under it.
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

# ----- L4 source 3: the runner's IN-PROCESS nonce mint --------------------------
# Preferred AHEAD of the UI-Bridge mint below, because it is the only one of the
# two that can answer on a headless runner: POST /coord-mcp/provision-session
# runs entirely inside the runner process, with no /ui-bridge/* hop. The whole
# /ui-bridge/* family is a FRONTEND PROXY — it bounces the request through the
# WebView to reach the Rust process that holds the credential — so with no
# WebView it cannot answer, and re-wording the mint as an `invoke` does not help
# (measured: 504 after a full 30.0s).
#
# The mint is bounded by the header's MINTING note: it is reachable only after
# L1 and L2 have each probed a door and found it dead, it mints for $PWD, and
# $COORD_REVIVE_NO_MINT=1 turns it off. The helper is shared with /gate rather
# than inlined here — the six credential doors were byte-similar and all six
# broke identically, which is exactly why this one lives in scripts/.
#
# Origins come from the configs already read (so a runner on a non-default port
# is found without configuration), passed through to the helper.
#
# The documented default (127.0.0.1:9876) is passed EXPLICITLY rather than left
# to the helper: `--origin` suppresses the helper's own default, so naming the
# discovered origins without it would silently drop the fallback that finds a
# runner no .mcp.json pointed at. $QONTINUI_RUNNER_URL still wins inside the
# helper - it reads it from the environment - so the precedence is unchanged
# from the UI-Bridge loop below.
MINT_ORIGIN_ARGS=""
for o in $RUNNER_ORIGINS http://127.0.0.1:9876; do
  case " $MINT_ORIGIN_ARGS " in *" $o "*) continue ;; esac
  MINT_ORIGIN_ARGS="$MINT_ORIGIN_ARGS --origin $o"
done

CPN=""
if [ -f "$HERE/../../../scripts/coord-provision-nonce.sh" ]; then
  CPN="$HERE/../../../scripts/coord-provision-nonce.sh"
elif [ -n "${QONTINUI_ROOT:-}" ] \
     && [ -f "${QONTINUI_ROOT}/qontinui-claude-config/scripts/coord-provision-nonce.sh" ]; then
  CPN="${QONTINUI_ROOT}/qontinui-claude-config/scripts/coord-provision-nonce.sh"
fi

if [ -n "${COORD_REVIVE_NO_MINT:-}" ]; then
  l4_fail "nonce-mint" "SKIPPED_BY_ENV (\$COORD_REVIVE_NO_MINT is set, so the in-process mint was not attempted. That is a CHOICE, not a fault, and it says nothing about whether the mint would have worked)"
elif [ -z "$CPN" ]; then
  l4_fail "nonce-mint" "HELPER_NOT_FOUND (coord-provision-nonce.sh not at the repo-relative path; set \$QONTINUI_ROOT for out-of-repo copies). LOCAL fault - it says nothing about the runner"
else
  # The helper prints `url=` / `nonce=` on STDOUT and its named diagnosis on
  # STDERR, which flows straight through to this script's probe log. The nonce
  # never reaches argv: it is read into a variable and handed to probe_door,
  # which stages it into a private header file.
  NOUT="$(bash "$CPN" mint --cwd "$PWD" $MINT_ORIGIN_ARGS)"
  NRC=$?
  NURL="$(printf '%s\n' "$NOUT" | sed -n 's/^url=//p' | head -n 1 | tr -d '\r')"
  NKEY="$(printf '%s\n' "$NOUT" | sed -n 's/^nonce=//p' | head -n 1 | tr -d '\r')"
  if [ "$NRC" = "0" ] && [ -n "$NURL" ] && [ -n "$NKEY" ]; then
    # $NURL VERBATIM: the nonce is paired to the runner's own bound port, and a
    # scanned or assumed port 401s.
    probe_door "L4" "nonce-mint@$NURL" "$NURL" "X-Coord-Mcp-Proxy-Key" "$NKEY" \
      "PROXY_UNAUTHORIZED (the runner minted this nonce and then refused it - the registry was rotated or the slot re-provisioned between the two calls; re-run)" \
      && live_exit "loopback-proxy-minted"
  else
    case "$NRC" in
      2)   NV="NO_HANDSHAKE_KEY (no readable ~/.qontinui/runner-loopback-key. The runner writes that 0600 file at startup, so an absent one means this runner's build predates the same-user handshake, or it runs as another user. LOCAL fault - NOT 'no credential')" ;;
      3)   NV="MINT_REFUSED (the runner answered with a TYPED refusal - see the coord-provision-nonce line above for which: the opt-in marker ~/.qontinui/allow-session-coord-identity is absent, or the handshake was missing/wrong. Each has a different fix, which is why they are three codes and not one)" ;;
      5)   NV="MINT_ROUTE_ABSENT (the runner answered 404 - this build predates the in-process mint. Do NOT restart a running runner over this: served policy production-and-cost runner-lifecycle. The next runner start picks it up)" ;;
      127) NV="HELPER_DEPS_MISSING (coord-provision-nonce.sh: curl missing, or no working JSON reader)" ;;
      *)   NV="MINT_UNKNOWN (no runner answered, or the answer was unrecognised - see the coord-provision-nonce line above. UNKNOWN, not a refusal and not an absent credential)" ;;
    esac
    l4_fail "nonce-mint" "$NV"
  fi
fi

# ----- L4 source 4: the runner's bearer mint - invoke first, eval fallback -----
# TWO doors per origin. The IN-PROCESS invoke door
# (POST /ui-bridge/invoke/get_access_token_for_websocket, body {}) answers on
# a headless runner because it never touches the WebView; it is tried FIRST,
# always. The WebView eval mint is KEPT rather than deleted - it is correct on
# a runner build that predates the invoke entry and has a WebView - but it is
# reached ONLY through that build's own answer: HTTP 400 "not in UI Bridge
# allowlist" (or 404 for the route). Every other invoke answer is a verdict in
# its own right (signed out, tier too low, refused) and the eval door, which
# fronts the SAME Rust fn through a WebView hop, adds nothing to it. Plan
# 2026-09-02-steering-layers-unreadable-without-a-credential, Phase 1f.
#
# Origins come from the configs already read, so a runner on a non-default port
# is found without configuration; $QONTINUI_RUNNER_URL overrides, and 9876 is
# the documented default (spelled as the IPv4 loopback deliberately).
#
# $QONTINUI_RUNNER_URL genuinely OVERRIDES the default, which is what SKILL.md
# has always claimed and what this line did NOT do until 2026-08-29: the literal
# `http://127.0.0.1:9876` sat outside the expansion, so setting the variable
# PREPENDED an origin and the default was still probed afterwards. Pointing this
# script at a dead port therefore did not stop it reaching the real runner --
# it minted a live Cognito access token there and sent it to production coord,
# on every run. Found by pre-PR review of the approval half, whose self-test
# relied on the documented override to stay hermetic and silently did not: 13
# real mints per suite run, invisible on a CI box with nothing on 9876. The
# default now applies only when the variable is unset, so an override is one.
MINT_BODY='{"expression":"window.__TAURI__ ? window.__TAURI__.core.invoke(\"get_access_token_for_websocket\") : invoke(\"get_access_token_for_websocket\")","await_promise":true}'

# THE HEADLESS ARM, and what it keys on.
#
# A WebView timeout used to land in the same bucket as "the runner is signed
# out", so a headless box was told to sign in a runner that was already holding
# a valid token. The fix keys on /health.frontendReady — a fact the runner
# STATES — and deliberately NOT on the timeout string: a desktop runner that is
# merely slow to boot its WebView produces the same timeout, and keying on the
# text would leave that ambiguity exactly where it was. A probe that FAILED
# leaves this `unknown`, and `unknown` never suppresses the mint.
FE_ORIGIN=""; FE_READY="unknown"; FE_STATE="unknown"
if [ -n "$CPN" ]; then
  FEOUT="$(bash "$CPN" frontend-state $MINT_ORIGIN_ARGS 2>/dev/null)"
  if [ $? = 0 ]; then
    FE_ORIGIN="$(printf '%s\n' "$FEOUT" | sed -n 's/^origin=//p' | head -n 1 | tr -d '\r')"
    FE_READY="$(printf '%s\n' "$FEOUT" | sed -n 's/^frontendReady=//p' | head -n 1 | tr -d '\r')"
    FE_STATE="$(printf '%s\n' "$FEOUT" | sed -n 's/^frontendState=//p' | head -n 1 | tr -d '\r')"
  fi
fi

L4_SEEN=""
for origin in ${QONTINUI_RUNNER_URL:-http://127.0.0.1:9876} $RUNNER_ORIGINS; do
  case " $L4_SEEN " in *" $origin "*) continue ;; esac
  L4_SEEN="$L4_SEEN $origin"
  # The mint is the single most expensive step in the cascade ($MINT_TIMEOUT is
  # 60s on its own), so the sweep budget is sampled before it as well as before
  # every probe. Skipped, never guessed at.
  if budget_skip "L4" "mint@$origin"; then continue; fi
  MEVAL_URL="$origin/ui-bridge/control/page/evaluate"
  MINT_SOURCE="runner-invoke"
  # TWO command names on the invoke door, tried in this order, because they are
  # two spellings of ONE credential slot with different gates in front of them
  # (`coord-gates-and-access.md`: "one slot, two names, both shipped"):
  #
  #   get_coord_device_token          - no require_tier_2(), unpaired is a plain
  #                                     Ok(None), and its allowlist entry is
  #                                     Dispatch::InProcess, so it answers on a
  #                                     HEADLESS runner as well as a windowed
  #                                     one. Added to UI_BRIDGE_COMMANDS by plan
  #                                     2026-08-30-every-runner-credential-door-\
  #                                     goes-through-one-csp-forbidden-eval.
  #   get_access_token_for_websocket  - the historical name. Calls
  #                                     require_tier_2() as its FIRST statement,
  #                                     so a healthy Tier-0/1 runner refuses it
  #                                     while holding a perfectly good token.
  #                                     Kept, and tried second, for a runner
  #                                     build that carries the older entry.
  #
  # Order matters for exactly one reason: the tier gate. Trying the gated name
  # first turns a Tier-1 runner's live credential into RUNNER_TIER_TOO_LOW.
  #
  # This is NOT a claim that either name yields fleet authority - they read the
  # same slot, so the swap removes a tier refusal and nothing more. The token is
  # probed against coord below before this rung is called LIVE, which is also
  # what covers `get_coord_device_token` not checking `exp`: an expired token
  # comes back DEVICE_JWT_UNAUTHORIZED rather than being handed on as a
  # credential. Probe the door; do not infer from the name.
  MINT_INVOKE_COMMANDS="get_coord_device_token get_access_token_for_websocket"
  # THE ONE ANSWER THAT OPENS THE FALLBACK: this build serves neither name.
  MFALLBACK=""
  for MCMD in $MINT_INVOKE_COMMANDS; do
    MINVOKE_URL="$origin/ui-bridge/invoke/$MCMD"
    MINT_URL="$MINVOKE_URL"
    # `-w '\n%{http_code}'` appends the status to STDOUT rather than using `-o`/
    # `-D` with a temp path: a POSIX temp path handed to the native curl.exe is
    # the check-#9 MSYS trap, and the status is the only extra fact needed. curl's
    # own stderr is kept (not /dev/null'd as it used to be) so every L4 verdict
    # can carry its one-line explanation like every other verdict here.
    : > "$TMPD/merr"
    # -m "$MINT_TIMEOUT", NOT "$PROBE_TIMEOUT": a mint, not a probe. The two
    # budgets were one number until 2026-08-31, and 15s on the eval call is what
    # produced a DEAD verdict over a healthy credential; the in-process door is
    # cheaper but shares the budget rather than inventing a fourth.
    MRAW="$(curl -sS -w '\n%{http_code}' --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$MINT_TIMEOUT" \
      -X POST "$MINVOKE_URL" \
      -H "Content-Type: application/json" -d '{}' 2>"$TMPD/merr")"
    MCE=$?
    MCURLERR="$(one_line 200 < "$TMPD/merr")"
    MCODE="$(printf '%s' "$MRAW" | tail -n 1 | tr -d '[:space:]')"
    MRESP="$(printf '%s\n' "$MRAW" | sed '$d')"

    MFALLBACK=""
    if [ "$MCE" = "0" ]; then
      case "$MCODE" in
        404) MFALLBACK="HTTP 404 - the invoke route is absent on this build" ;;
        400) case "$(printf '%s' "$MRESP" | read_eval_error)" in
               *"not in UI Bridge allowlist"*) MFALLBACK="HTTP 400 - $MCMD is not on this build's invoke allowlist" ;;
             esac ;;
      esac
    fi
    # Anything that is NOT "this build does not serve that name" is this
    # command's own answer - a token, a refusal, a transport fault - and the
    # arms below classify it. Stop here rather than asking the next name a
    # question this one already answered.
    [ -z "$MFALLBACK" ] && break
    echo "L4: mint@$origin source=runner-invoke cmd=$MCMD -> INVOKE_MINT_ROUTE_ABSENT ($MFALLBACK. A runner start does NOT pick up an allowlist entry its BINARY does not carry, so this is a build fact, not a configuration one - never restart a running runner over it)" >&2
  done
  MINT_SOURCE="runner-invoke:$MCMD"
  if [ -n "$MFALLBACK" ]; then
    echo "L4: mint@$origin source=runner-invoke -> INVOKE_MINT_ROUTE_ABSENT (this build serves NONE of: $MINT_INVOKE_COMMANDS. Falling back to the WebView eval mint)" >&2
    MINT_SOURCE="runner-eval"
    MINT_URL="$MEVAL_URL"
    # The distinct arm, scoped to the eval FALLBACK only - the invoke door
    # above was already tried on this headless box. Not attempted, because it
    # provably cannot answer — and named as a DEAD TRANSPORT so nobody reads it
    # as a missing credential. Only fires for the origin the /health probe
    # actually answered from, and only on an explicit `false`; `unknown` falls
    # through to the mint below.
    if [ -n "$FE_ORIGIN" ] && [ "$origin" = "$FE_ORIGIN" ] && [ "$FE_READY" = "false" ]; then
      l4_fail "mint@$origin source=runner-eval" "RUNNER_HEADLESS (this build has no in-process mint, and /health reports frontendReady=false, frontendState=$FE_STATE, so POST $MEVAL_URL cannot answer either: every /ui-bridge/control/* route is a FRONTEND PROXY that bounces the request through the WebView. This is a DEAD TRANSPORT on this build, NOT a signed-out runner - a live device credential can be sitting in the runner store the whole time. The in-process door for this box is L4 source 3 above; read its verdict, not this one)"
      continue
    fi
    : > "$TMPD/merr"
    MRAW="$(curl -sS -w '\n%{http_code}' --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$MINT_TIMEOUT" \
      -X POST "$MEVAL_URL" \
      -H "Content-Type: application/json" -d "$MINT_BODY" 2>"$TMPD/merr")"
    MCE=$?
    MCURLERR="$(one_line 200 < "$TMPD/merr")"
    MCODE="$(printf '%s' "$MRAW" | tail -n 1 | tr -d '[:space:]')"
    MRESP="$(printf '%s\n' "$MRAW" | sed '$d')"
  fi
  MSUFFIX=""
  [ -n "$MCURLERR" ] && MSUFFIX=" [curl: $MCURLERR]"

  # Every mint verdict goes through here so curl's own one-line explanation is
  # attached UNIFORMLY. Appending $MSUFFIX per-arm let an arm silently drop it,
  # and a partial transfer produces BOTH a parseable error string and curl
  # stderr — precisely the case where losing the stderr costs the most. The
  # door name carries which of the two mints answered.
  mint_fail() { l4_fail "mint@$origin source=$MINT_SOURCE" "$1$MSUFFIX"; }

  # curl's exit code, before anything else: a REFUSED connection and a HUNG one
  # are different faults, and classify() already draws that line for L1/L2/L3.
  # Collapsing them here would tell a saturated runner's operator that it is
  # down — and on this fleet "restart it" is exactly the wrong move (served
  # policy production-and-cost runner-lifecycle; /health has been sampled from
  # 296ms to 10120ms on a loaded box, against this mint's own $MINT_TIMEOUT (60s),
  # which was 15s until 2026-08-31 - the shortest in the fleet for this call,
  # and enough to turn a slow-but-healthy runner into VERDICT: DEAD).
  case "$MCE" in
    7)
      mint_fail "NO_RUNNER (nothing is listening at $origin - runner down, moved to another port, or never started)"
      continue ;;
    28)
      mint_fail "RUNNER_TIMEOUT (the port answered but produced no response within ${MINT_TIMEOUT}s). Often SATURATION rather than a dead runner - do NOT restart it on this alone; re-run, or use another door"
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
    probe_door "L4" "device-jwt@$origin source=$MINT_SOURCE" "${COORD_URL}/mcp" "Authorization" "Bearer $MJWT" \
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
  case "$MCODE" in 2??) ;; *) MHTTP="HTTP $MCODE from $MINT_URL: " ;; esac
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
    # The WebView eval door refused to evaluate ANY string. Matched on the CSP
    # error's stable substring rather than on the full sentence: the directive
    # text moves (it grew three sha256- hashes between 2026-08-30 and
    # 2026-09-01) and the "Refused to evaluate a string as JavaScript" /
    # "unsafe-eval" pair is what every engine emits.
    #
    # This is a BROKEN DOOR, not a missing credential and not a transient. The
    # bundled app ships `script-src 'self'` with no 'unsafe-eval'
    # (qontinui-runner src-tauri/tauri.conf.json), the frontend evaluator runs
    # the expression with `new Function`
    # (src/hooks/ui-bridge-events/utils.ts), and CSP forbids exactly that - so
    # the refusal is independent of the expression, of the command name, and of
    # sign-in state. Measured 2026-09-01 on build 58414a05: even `1+1` was
    # refused. Retrying, re-signing-in or changing the command cannot open it;
    # the Rust-side window.eval fallback in page.rs cannot be reached either,
    # because the refusal arrives over a HEALTHY IPC round-trip as
    # Ok({success:false}) and that fallback fires only on an IPC transport
    # error. Plan
    # 2026-08-30-every-runner-credential-door-goes-through-one-csp-forbidden-eval.
    *"unsafe-eval"*|*"Refused to evaluate a string as JavaScript"*)
      mint_fail "RUNNER_EVAL_CSP_BLOCKED (${MHTTP}the runner's WebView Content-Security-Policy forbids evaluating a string as JavaScript, so POST $MEVAL_URL can NEVER mint on this build - for ANY expression, windowed or headless, signed in or not. This is a BROKEN DOOR, not a missing credential and NOT transient: do not retry it, do not read it as a sign-in problem, and do NOT restart the runner over it (served policy production-and-cost runner-lifecycle). The eval-free replacement is the invoke door tried above (POST $origin/ui-bridge/invoke/get_coord_device_token); a build whose allowlist lacks it cannot serve this rung at all - use L4 source 3 (the in-process nonce mint), the static device-JWT sources, or L5. Door said: \"$MERR\"" ;;
    # A caller-side input problem with a documented remedy, NOT a door fault.
    # The frontend applies a static blocklist (PAGE_EVALUATE_STRUCTURAL_PATTERNS)
    # BEFORE evaluating, so `new Function(`, `eval(` and friends are rejected
    # ahead of CSP. The runner already returns a `hint` for it; surfacing that
    # here keeps it out of the RUNNER_EVAL_FAILED catch-all, which reads as a
    # broken runner. It also names the trap the migration has to avoid: an
    # expression that wraps its own eval to route around a CSP block is
    # rejected for an unrelated reason and reports the wrong cause.
    *"Expression rejected: contains prohibited pattern"*)
      mint_fail "RUNNER_EVAL_STATIC_GUARD (${MHTTP}the runner's frontend static blocklist rejected the expression BEFORE evaluating it - this is about what was SENT, not about the runner's health or sign-in state. Send a plain expression, never one that wraps its own eval()/new Function(). Door said: \"$MERR\"" ;;
    ?*)
      mint_fail "RUNNER_EVAL_FAILED (${MHTTP}the $MINT_SOURCE mint returned no token, and said: \"$MERR\"). Read the quoted error - this is NOT necessarily a sign-in problem" ;;
    *)
      # No token AND no error string. Do not assert a cause not tested: a 4xx is
      # consistent with a route that moved, a 5xx is the route being PRESENT and
      # failing server-side, and sending a reader hunting a renamed route on a
      # 500 is the habit cfg_shape()'s comment says this script exists to break.
      if [ -n "$MJWT" ]; then
        mint_fail "RUNNER_SIGNED_OUT (the UI Bridge returned a value, but it is not a JWT-shaped token - a JWT is 3 dot-separated base64url parts. NOT sent). Sign the runner in"
      else
        case "$MCODE" in
          # A 2xx with no token and no error is NOT a shape change when the
          # command that answered was get_coord_device_token: `Ok(None)` is its
          # documented, deliberate answer for "this runner is unpaired", chosen
          # over an error precisely because it is a credential PROBE. Reading it
          # as "the UI-Bridge response shape has changed" would send the reader
          # hunting a renamed route for a runner that answered correctly.
          2??) case "$MINT_SOURCE" in
                 *get_coord_device_token) mint_fail "RUNNER_SIGNED_OUT (${MHTTP}get_coord_device_token answered normally with null, which is its documented 'this device is unpaired' result - the runner is healthy, the credential slot is empty. Pair or sign this runner in; nothing here is broken)" ;;
                 *) mint_fail "RUNNER_EVAL_FAILED (HTTP $MCODE but the body carried neither a .data.value / .data.result.value nor an error string - the UI-Bridge response shape has changed)" ;;
               esac ;;
          4??) mint_fail "RUNNER_EVAL_FAILED (HTTP $MCODE from $MINT_URL with no error string in the body - the route is absent/renamed, or something else answers on this port. NOT a sign-in problem)" ;;
          5??) mint_fail "RUNNER_EVAL_FAILED (HTTP $MCODE from $MINT_URL with no error string in the body - the route is PRESENT and failed server-side. Says nothing about the route existing or about your sign-in state)" ;;
          *)   mint_fail "RUNNER_EVAL_FAILED (HTTP $MCODE but the body carried neither a .data.value / .data.result.value nor an error string - the UI-Bridge response shape has changed)" ;;
        esac
      fi ;;
  esac
done

# ----- L5: the BOOTSTRAP credential — the first rung that needs no runner ------
# L1/L2 ARE the runner (it serves the loopback proxy). L4 sources 3 and 4 are
# credentials the runner mints. L3 needs $COORD_AGENT_JWT, unset on this fleet.
# So one wedged runner takes every rung above at once, and the two static
# device-JWT sources that survive it hold a ~4h token — expired more often than
# not. Measured 2026-08-28: an /unattended closeout walked all of it (MCP tools
# absent, proxy HTTP 000 on 5 candidates, 401 on the device JWT, mint timed out
# at 60s/0 bytes) and had to report its own work DROPPED. Plan
# 2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline.
#
# L5's only input is a FILE ON DISK: a device_id, POSTed anonymously to coord's
# dedicated credential route. Anonymous is not a re-opened hole — it is the
# pair_via_browser carve-out shape sanctioned by the SHIPPED plan
# 2026-08-14-runner-unauthenticated-coord-writers: a credential-minting route is
# anonymous BECAUSE requiring a credential would be circular, and that plan's
# coord_auth_pin.rs guard objects to an unauthenticated WRITE, not to using an
# issued token. Everything after the mint here carries a bearer.
#
# It probes THAT ROUTE AND NOTHING ELSE - and that route IS DEPLOYED. Measured
# against production coord 2026-09-04 from merytshost: the anonymous POST
# answered HTTP 200 with {token, token_exp, token_jti}, the token an EdDSA JWT
# claiming iss=qontinui-coord, sub=device:<device_id>, sub_type=agent, a
# resolved tenant_id, NO agent_id claim, ALL scopes empty/false, ~4h TTL - and
# the control read below answered 200 over it (401 without a bearer). The whole
# cascade was run to exhaustion that day and L5 reported LIVE as the ONLY live
# rung: the static ~/.qontinui/coord-device-jwt was 401, the invoke mint 400
# (not on this build's allowlist) and the WebView eval mint 400 (CSP
# unsafe-eval). Until 2026-09-04 this comment and every doc around it said the
# route was NOT DEPLOYED and that L5 "always ends in BOOTSTRAP_ROUTE_ABSENT" -
# a rung documented as permanently shut is a rung nobody tries, which removed
# the one working door in exactly the state it exists for.
#
# The 404/405 arm below is KEPT: it is what another deployment, or a future
# rollback, would answer, and reading a router artefact as a device refusal is
# still the false verdict this script exists not to draw. It is no longer the
# expected outcome HERE.
#
# It does NOT substitute POST /agents/allocate. Allocate mints the same class of
# token today, and three shipped documents forbid carrying a coord rung on it
# (/gate and /policy both carry it on their generic remote MCP rung, and
# coord-gates-and-access.md restates it). Whether it may ever
# be used this way is an OPEN OPERATOR RULING, escalated as coord gate
# ece99898-30c6-4f8c-be8e-1de5f09abebc (operator_approval, gate_class
# security-surface); an agent does not pre-empt a ruling that has just been
# asked for. Until it lands, the honest outcome of an exhausted cascade is a
# DEAD verdict plus a DURABLY RECORDED BLOCKER - never a token from that door.
if budget_skip "L5" "bootstrap-credential"; then
  : # already recorded by budget_skip; L5 is not attempted on an exhausted budget
elif [ -n "${COORD_REVIVE_NO_BOOTSTRAP:-}" ]; then
  echo "L5: bootstrap-credential -> SKIPPED_BY_ENV (\$COORD_REVIVE_NO_BOOTSTRAP is set, so the runner-independent credential was not attempted. That is a CHOICE, not a fault, and it says nothing about whether it would have worked)" >&2
  FAILS+=("L5 bootstrap-credential: SKIPPED_BY_ENV (\$COORD_REVIVE_NO_BOOTSTRAP is set)")
else
  # THE COUNTER (plan Phase 4). Reaching L5 means the ordinary doors already
  # failed, so a counter that had to reach coord would be missing in exactly the
  # outage it exists to measure. This is the LOCAL breadcrumb the guard
  # component already ships — scripts/lib/guard-decision-log.sh, whose `tag`
  # field its own header calls "the field you grep and count", rotated at
  # SessionStart by scripts/session-id-stamp.sh. Sourcing is best-effort: a
  # missing library must never cost a recovery its door.
  l5_count() { :; }
  for __gdl in "$HERE/../../../scripts/lib/guard-decision-log.sh" \
               "${QONTINUI_ROOT:-}/qontinui-claude-config/scripts/lib/guard-decision-log.sh"; do
    # shellcheck source=/dev/null
    if [ -r "$__gdl" ] && . "$__gdl" 2>/dev/null && command -v guard_decide >/dev/null 2>&1; then
      l5_count() { guard_decide "$DOOR_SCRIPT_NAME" "$1" "$2"; }
      break
    fi
  done
  l5_count warn l5-reached

  # l5_fail <verdict> — log + record, one place, same contract as l4_fail.
  l5_fail() {
    echo "L5: bootstrap-credential -> $1" >&2
    FAILS+=("L5 bootstrap-credential: $1")
  }

  # --- device_id: env first, then the static local file -----------------------
  # $QONTINUI_MACHINE_ID then ~/.qontinui/machine.json "device_id", falling back
  # to the legacy "machine_id" spelling. This is the fleet's established
  # resolution order (/vet-plan, /vet-imp, /preflight, /manual-test-coord all
  # spell it), reused rather than reinvented.
  DEV_ID="${QONTINUI_MACHINE_ID:-}"
  MACHINE_FILE="$HOME_DIR/.qontinui/machine.json"
  MACHINE_READABLE=""
  # The SOURCE NAME, never the value — a device UUID is not a secret but it is
  # the only bar on the mint route, so it stays out of every printed line the
  # same way a key does.
  DEV_ID_SRC="\$QONTINUI_MACHINE_ID"
  [ -n "$DEV_ID" ] || DEV_ID_SRC="$MACHINE_FILE"
  if [ -z "$DEV_ID" ] && [ -n "$HOME_DIR" ] && [ -r "$MACHINE_FILE" ]; then
    MACHINE_READABLE=1
    if [ "$JSON_READER" = jq ]; then
      DEV_ID="$(jq -r '((.device_id // .machine_id) // "") | tostring' < "$MACHINE_FILE" 2>/dev/null | tr -d '[:space:]')"
    else
      DEV_ID="$("$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(); sys.exit(0)
v=d.get("device_id") or d.get("machine_id") or ""
print(v if isinstance(v,str) else "")' < "$MACHINE_FILE" 2>/dev/null | tr -d '[:space:]')"
    fi
    [ "$DEV_ID" = "null" ] && DEV_ID=""
  fi

  if [ -z "$DEV_ID" ]; then
    # Two different LOCAL faults with two different fixes. Collapsing them would
    # send a reader to repair a file that is not there, or to pair a machine
    # whose file exists and merely says nothing — the named-cause invariant.
    if [ -n "$MACHINE_READABLE" ]; then
      l5_fail "BOOTSTRAP_MACHINE_FILE_MALFORMED ($MACHINE_FILE is readable but carries neither a \"device_id\" nor the legacy \"machine_id\" (or is not JSON). LOCAL fault - nothing was sent, and it says nothing about coord)"
      l5_count unknown l5-no-device-id
    else
      l5_fail "BOOTSTRAP_NO_DEVICE_ID (\$QONTINUI_MACHINE_ID is unset AND $MACHINE_FILE is absent or unreadable. LOCAL fault - a statement of ABSENCE, not a coord verdict. Nothing was sent: an empty device_id would draw a 4xx this script would then blame on coord)"
      l5_count unknown l5-no-device-id
    fi
  else
    # --- mint: anonymous POST, no Authorization header of any kind ------------
    # The dedicated credential-only route, and the ONLY route this rung probes.
    # NOT /agents/allocate - see the block
    # comment above and SKILL.md's L5 section for why that one is hand-run.
    BOOT_URL="${COORD_URL}/agents/credential"
    BOOT_BODY="$TMPD/bootbody"
    BOOT_ERR="$TMPD/booterr"
    : > "$BOOT_BODY"; : > "$BOOT_ERR"
    # device_id is a UUID from a local file, not key material, so a -d payload is
    # fine here; the TOKEN that comes back never goes near argv.
    BOOT_CODE=$(curl -sS -o "$(curl_path "$BOOT_BODY")" -w '%{http_code}' \
      --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$PROBE_TIMEOUT" \
      -X POST "$BOOT_URL" -H "Content-Type: application/json" \
      -d "{\"device_id\":\"$DEV_ID\"}" 2>"$BOOT_ERR")
    BOOT_CE=$?
    BOOT_CURLERR="$(one_line 200 < "$BOOT_ERR")"
    BOOT_RESP="$(cat "$BOOT_BODY" 2>/dev/null)"
    BOOT_MSG="$(printf '%s' "$BOOT_RESP" | read_eval_error | one_line 200)"

    if [ "$BOOT_CE" != "0" ] || [ -z "$BOOT_CODE" ] || [ "$BOOT_CODE" = "000" ]; then
      l5_fail "BOOTSTRAP_UNREACHABLE (the mint POST to $BOOT_URL never completed - connect refused, DNS, TLS or a timeout (${PROBE_CONNECT_TIMEOUT}s connect / ${PROBE_TIMEOUT}s total). This is the ONE L5 verdict that is about coord or this box's network)${BOOT_CURLERR:+ [curl: $BOOT_CURLERR]}"
      l5_count unknown l5-unreachable
    elif [ "$BOOT_CODE" = "404" ] || [ "$BOOT_CODE" = "405" ]; then
      # 405, not only 404. Measured against production coord 2026-09-02: an
      # unregistered POST anywhere under /agents/ answers 405 with an EMPTY body
      # - `POST /agents/definitely-not-a-route` returns the identical 405
      # (re-confirmed 2026-09-04) - so on this deployment "the route has not
      # landed" is spelled 405. Reading that as a refusal would be a confidently
      # wrong verdict about the DEVICE for a fact about the ROUTER, which is the
      # one thing this script exists not to do. This arm is NOT what
      # /agents/credential answers here any more - it answered 200 on
      # 2026-09-04 - so reaching it is a REGRESSION or a different deployment,
      # not the norm. (`GET /agents/credential` answered 403 tenant_not_resolved
      # on 2026-09-02 and 405 on 2026-09-04; either is a router artefact and not
      # a device verdict - but this rung only ever POSTs.)
      l5_fail "BOOTSTRAP_ROUTE_ABSENT (HTTP $BOOT_CODE from $BOOT_URL - the dedicated credential route is not answering on THIS coord; a 405 can mean exactly that here, since coord answers 405 for ANY unregistered POST under /agents/, verified 2026-09-02 and 2026-09-04). This is NOT the expected outcome: the same anonymous POST answered 200 with a device-subject agent JWT against production coord on 2026-09-04, so this arm now means a REGRESSION, a rollback, or a different deployment - report it as such rather than as a known-absent rung. It still does NOT license substituting POST /agents/allocate: three shipped documents forbid carrying a coord rung on that door, and whether it may ever be used this way is an OPEN OPERATOR RULING, coord gate ece99898-30c6-4f8c-be8e-1de5f09abebc (operator_approval, security-surface). With this arm hit and no other door, the honest outcome is a DEAD verdict PLUS a durably recorded blocker: write the gate or finding SPEC verbatim so a peer with a working transport can carry it"
      l5_count unknown l5-route-absent
    else
      case "$BOOT_CODE" in
        2??)
          # The token field. `token` is the spelling coord's sibling
          # AllocateResponse uses (agent_worktrees.rs), and MEASURED 2026-09-04
          # it is also what the dedicated route returns - alongside token_exp
          # and token_jti. The alternates stay as defensive reads rather than
          # asserted. Read on STDIN - no path crosses to a native binary.
          if [ "$JSON_READER" = jq ]; then
            BOOT_TOKEN="$(jq -r '((.token // .agent_jwt // .jwt // .access_token) // "") | tostring' < "$BOOT_BODY" 2>/dev/null | tr -d '[:space:]')"
          else
            BOOT_TOKEN="$("$JSON_READER" -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print(); sys.exit(0)
for k in ("token","agent_jwt","jwt","access_token"):
    v=d.get(k)
    if isinstance(v,str) and v:
        print(v); sys.exit(0)
print()' < "$BOOT_BODY" 2>/dev/null | tr -d '[:space:]')"
          fi
          [ "$BOOT_TOKEN" = "null" ] && BOOT_TOKEN=""
          if ! jwt_shaped "$BOOT_TOKEN"; then
            l5_fail "BOOTSTRAP_NO_TOKEN_IN_RESPONSE (HTTP $BOOT_CODE from $BOOT_URL but the body carried no JWT-shaped token - a JWT is 3 dot-separated base64url parts. The route's response shape changed, or something else answers on this host. NOT sent onward: an unshaped bearer would draw a 401 this script would then report against coord)"
            l5_count unknown l5-token-unverified
          else
            # --- VERIFY BEFORE DECLARING LIVE ---------------------------------
            # A minted token that does not authenticate is a FALSE GREEN, and a
            # false green is what cost the 2026-08-28 closeout its output. The
            # control read is cheap, read-only, and proves the exact property
            # this rung asserts: GET /coord/agent-findings answers 200 to a good
            # bearer and 401 to none (coord routes.rs, agent_findings_authed,
            # behind require_jwt). Header staged OFF argv, same rule as
            # probe_door - the process list is world-readable on this box.
            CTRL_HDR="$TMPD/boothdr"
            CTRL_BODY="$TMPD/ctrlbody"
            CTRL_ERR="$TMPD/ctrlerr"
            { printf 'Authorization: Bearer %s\n' "$BOOT_TOKEN" > "$CTRL_HDR"; } 2>/dev/null
            if [ ! -s "$CTRL_HDR" ]; then
              l5_fail "AUTH_HEADER_STAGING_FAILED (could not write the bearer header under $TMPD - LOCAL fault, says nothing about coord or about the token, which WAS minted). Re-run; if it repeats, check \$TMPDIR"
              l5_count unknown l5-token-unverified
            else
              : > "$CTRL_BODY"; : > "$CTRL_ERR"
              CTRL_URL="${COORD_URL}/coord/agent-findings?limit=1"
              CTRL_CODE=$(curl -sS -o "$(curl_path "$CTRL_BODY")" -w '%{http_code}' \
                --connect-timeout "$PROBE_CONNECT_TIMEOUT" -m "$PROBE_TIMEOUT" \
                -H "@$(curl_path "$CTRL_HDR")" "$CTRL_URL" 2>"$CTRL_ERR")
              CTRL_CE=$?
              CTRL_CURLERR="$(one_line 200 < "$CTRL_ERR")"
              rm -f "$CTRL_HDR"
              if [ "$CTRL_CE" = "0" ] && [ "$CTRL_CODE" = "200" ]; then
                echo "L5: bootstrap-credential -> LIVE (minted at $BOOT_URL, verified by $CTRL_URL -> 200)" >&2
                l5_count allow l5-live
                LIVE_FILE="bootstrap-credential (device_id from $DEV_ID_SRC)"
                LIVE_URL="$BOOT_URL"
                live_exit "https-bootstrap-agent-jwt" "$PARTIAL_BOOTSTRAP"
              else
                l5_fail "BOOTSTRAP_TOKEN_UNVERIFIED (the mint SUCCEEDED - HTTP $BOOT_CODE from $BOOT_URL returned a JWT-shaped token - and the control read $CTRL_URL answered HTTP ${CTRL_CODE:-<none>} instead of 200. A mint is not an authentication: report BOTH facts, and do NOT call this door LIVE)${CTRL_CURLERR:+ [curl: $CTRL_CURLERR]}"
                l5_count unknown l5-token-unverified
              fi
            fi
          fi ;;
        *)
          # Every other status is the route REFUSING THIS DEVICE - an unknown or
          # unregistered device_id, a malformed UUID, or (on the hand-run
          # allocate fallback) a 409 about a repo. A verdict about the device,
          # not about coord's health, which is why it is not UNREACHABLE.
          l5_fail "BOOTSTRAP_DEVICE_REJECTED (HTTP $BOOT_CODE from $BOOT_URL - the route answered and refused this device_id. Check that the id resolved from $DEV_ID_SRC is the one coord knows)${BOOT_MSG:+ coord said: \"$BOOT_MSG\"}${BOOT_CURLERR:+ [curl: $BOOT_CURLERR]}"
          l5_count unknown l5-device-rejected ;;
      esac
    fi
  fi
fi

# ----- Honest failure: name the exhausted cascade ------------------------------
# BUDGET_EXCEEDED IS NOT DEAD, and the distinction is the whole reason it exists.
# DEAD asserts that every door was probed and none answered; a run that ran out
# of wall clock probed SOME doors and skipped the rest, so calling it DEAD would
# be a confident verdict about doors this process never touched - the same
# false-DEAD class every other guard in this file is written against, reached by
# a stopwatch instead of a bug. The exhausted list still prints underneath either
# way: the skipped doors are IN it, named SKIPPED_*, so nothing is hidden.
if [ -n "$BUDGET_TRIPPED" ]; then
  echo "VERDICT: BUDGET_EXCEEDED - the ${PROBE_TOTAL_BUDGET}s sweep budget ran out after $DOORS_PROBED probe(s) across $DISTINCT_DOORS distinct door(s), with $DOORS_SKIPPED door(s) SKIPPED UNPROBED. This is UNKNOWN, NOT dead: no door answered among the ones reached, and the ones below marked SKIPPED_BUDGET_EXCEEDED were never asked. Raise \$COORD_REVIVE_TOTAL_BUDGET (default 60) to finish the sweep, or read the skipped list and probe one by hand:"
else
  echo "VERDICT: DEAD - no OUT-OF-BAND door, $DOORS_PROBED probe(s) across $DISTINCT_DOORS distinct door(s): L1 (own $OWN), L2 (sibling sweep under $ROOT), L3 (acting-bearer), L4 (\$COORD_DEVICE_JWT, ~/.qontinui/coord-device-jwt, the in-process nonce mint, then the UI-Bridge mint), L5 (the runner-independent bootstrap credential):"
fi
for f in "${FAILS[@]}"; do
  echo "  - $f"
done
# The runner's own spawn-time reason belongs IN the verdict block, not only in
# the stderr log above it: an agent that pastes this DEAD verdict as its
# blocked-evidence would otherwise drop the one line that names WHY there was no
# door to find. Context only - it never changed the verdict, and it is a fact
# about spawn time, not about now (see the reader near L1).
if [ -n "$CRUMB" ]; then
  echo "BREADCRUMB ($CRUMB_QUAL): the runner recorded a DEGRADED coord-mcp provision for this workdir: $CRUMB$CRUMB_NOTE"
  echo "  (spawn-time evidence from $CRUMB_FILE. The AGE above is part of the evidence - quote it whenever you paste this block, because an unaged breadcrumb travels furthest and reads as present tense. Whether anything re-evaluated it after provisioning is a property of the runner build, so it can be stale, and it never describes coord's state now. A 'NOT written' reason means THAT pass wrote no .mcp.json; a stale one from an earlier pass can still be sitting there.)"
else
  echo "BREADCRUMB: none in this cwd ($CRUMB_FILE: absent, unreadable or empty). That is UNKNOWN, not evidence of a healthy provision - the runner writes nothing on the healthy path AND nothing at all for a workdir it never provisioned. It also stays SILENT on purpose when it declines to overwrite a workdir that already DECLARES a coord-mcp (a foreign agent-JWT config, or a secondary runner leaving alone a primary shared-root config that declares one) - and declaring is only a /mcpServers/coord-mcp key test, never proof the door answers. So an absent breadcrumb beside an .mcp.json that L1 could not revive is a DIAGNOSED shape, not an unexplained one: a dead declared entry is precisely what buys that silence. This reader looks only in the cwd; the runner writes into the workdir IT provisioned, which on a linked worktree may be the primary checkout."
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
wedge_block
approval_verdict_block
echo "SCOPE: this covers ONLY the out-of-band doors above. It does NOT probe your native coord_* MCP tools - a separate transport that can be fully LIVE while every door here is dead (observed 2026-08-08: DEAD alongside a successful coord_gate_inspect in the same minute)."
echo "So BEFORE applying the lost-write doctrine, issue one cheap native coord read (coord_gate_inspect on any known gate_id). If it answers, coord is REACHABLE: re-issue over the native tools and verify by read - do not presume the write lost on this verdict alone."
# THE RE-PROVISION ADVICE IS WEDGE-GATED. The ordinary Next line points at the
# provisioning route to explain why re-running it by hand is pointless - but on a
# wedged port the reader's takeaway from a mixed set is "the key is stale, mint a
# new one", and naming the route at all is what they act on. During a wedge a
# re-provision evicts a live peer's binding to fix a key that was never broken,
# so the route is not mentioned on this path at all.
if [ -n "$WEDGED_ENDPOINTS" ]; then
  echo "Next: the RUNNER_WEDGED verdict above governs. Do NOT re-provision, rotate a key, or restart the runner; probe /livez on the wedged endpoint, treat in-flight coord writes as LOST and verify by read, and re-run this cascade in a few minutes - observed wedges have cleared on their own. ('coord doctor' is still safe to run: it is a read-only self-check.)"
else
  echo "Next: run 'coord doctor' (runner self-check) to name the failing credential-chain link. L4 source 3 ALREADY attempted /coord-mcp/provision-session for this cwd (bounded: only after L1+L2 proved this workdir's key dead, so there was no live slot here to evict) - re-running it by hand will not find a door this did not, and calling it for ANOTHER workdir would evict that workdir's live key."
fi
exit 1
