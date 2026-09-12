---
description: One transport-agnostic read-only door to list or fetch coord prompt documents (the fleet policies) — runs the native MCP tool, an auto-discovered loopback proxy (JSON-RPC), the generic remote MCP door (POST /mcp, device JWT), or the device-authed HTTP agent routes, with the local steering cache as the last rung for the six intent kinds and the qontinui-dev-notes policy mirrors as the last rung for `policy` (both disclosed; `mirrors` reports the mirrors' drift) — so you never touch ports, nonces, or proxies. Use it whenever coord_list_prompt_documents is not a visible tool.
argument-hint: "list | get <kind> <name> | mirrors"
allowed-tools: Read, Bash, PowerShell, Glob, Grep, ToolSearch
---

# Policy — read the fleet policy documents over whatever transport works

`/policy` is the **single executable front door** for the coord prompt-document
**read** surface. Served policy (`session-protocol` Step 0 — read the policies
fresh; ask only what no clause answers) makes consulting tenant policies a
mandatory step, but names live in coord — and a session with no coord-mcp
configured has
no `coord_list_prompt_documents` tool and, without this door, dead-ends on the
operator CRUD routes. `/policy` figures out *how* to reach the documents —
native MCP tool, the loopback proxy (MCP JSON-RPC), coord's generic remote
MCP door (`POST $COORD_HTTP_URL/mcp`, device JWT), the device-authed HTTP
agent routes, or (disclosed, last resort) the file mirrors — and reports which
transport carried the read.

This is the same transport-cascade pattern `/gate` applies to gate
registration, applied to the prompt-document read surface. `_gate-registration`
and `/gate` own gate registration; do not merge them with this door. Server
side, the agent routes are coord `routes.rs`
`agent_prompt_documents_list_authed` / `agent_prompt_documents_one_authed` —
`require_jwt`-gated, with the tenant lifted from the verified JWT **inside the
handler, never from an argument**.

> **If `coord_list_prompt_documents` isn't a visible tool in this session, that
> is not a dead end — run `/policy`.** A masked or absent MCP tool is exactly
> what the cascade below is for.

## Arguments — `$ARGUMENTS`

- `list` — list every prompt document the caller's tenant can see (kinds
  include `policy`, `agent_playbook`, `continuation_rules`, `prompt_template`,
  `response_prompt`). **Default when no sub-verb is given.**
- `get <kind> <name>` — fetch one document's body, e.g.
  `/policy get policy escalation-bar`, or `/policy get domain_spec
  coord-merge-train` for one of the six **intent** kinds (`product_intent`,
  `initiative`, `success_metric`, `domain_spec`, `audience_profile`,
  `decision_record`).
- `mirrors` — **diagnostic only.** Compare every rung-5 mirror's version stamp
  against the served `current_version` and print the drift table (Step 5).
  Requires a reachable coord transport; answers "should these files be
  re-rendered?", never "what does the policy say?".

Every output MUST name the transport that carried the read (see "Honesty
rules" below).

## Non-goals (scope fence — do not widen)

- **Read-only.** No create, patch, or restore-default — writes stay on the
  operator CRUD routes (`/coord/prompt-documents`, operator Cognito context)
  and the web editor. This door never mutates anything.
- **No caching to disk.** Policies version frequently (`coordination` reached
  v9 within days); a cached copy drifts and reintroduces the stale-source
  problem the fleet already retired a SessionStart hook to avoid. Reading the
  LAST-RESORT mirrors (Step 5) is **not** caching — this skill writes nothing, and every
  mirror it serves carries its own version stamp plus an explicit statement
  that the stamp could not be verified. The same holds for the steering cache
  (Step 4c): a separate renderer writes it, this door only reads it, and every
  answer served from it quotes the cache's own Rendered stamp.
  > *Corrected 2026-08-06.* This bullet used to say the mirrors "are maintained
  > elsewhere." **There is no elsewhere** — nothing maintains them, and 6 of 14
  > were behind the served store six days after a full hand regeneration. The
  > disclosure, not a maintainer, is what makes rung 5 safe; that is why Step 5
  > withholds the body of any mirror that cannot state its own version — and why
  > it discloses drift-unknown *unconditionally* rather than above an age
  > threshold (Step 5 records the 2026-08-30 measurement behind that change).

---

## The transport cascade (try in order; stop at the first that works)

**Each step is validated by a cheap probe before you trust it.** Always report
which step carried the read.

### Step 1 — Native MCP tools (probe: tool present)

If `coord_list_prompt_documents` / `coord_get_prompt_document` are in this
session's tool set, call them directly (load via `ToolSearch` if they are
deferred tool names):

- `list` → `coord_list_prompt_documents` (no arguments).
- `get`  → `coord_get_prompt_document` with `{"kind":"<kind>","name":"<name>"}`.

Tenant derives server-side from the session's device identity.

- **Probe:** the tool exists / `tools/list` shows it. If the call returns
  **unknown / method-not-found**, the tool is masked → **do not stop**; fall to
  Step 2. A masked tool reading as "no such tool" is the trigger for the
  cascade, not a failure to report.
- If a coord MCP tool was VISIBLE and returned `"Command failed with no
  output"`, that is a dead cached transport, not a masked tool — run
  `/coord-revive` first, then re-issue over the door it names. (For a read
  this is cheap: just retry over the live door; there is no lost-write hazard.)

### Step 2 — Auto-discover a live loopback proxy (probe: `tools/list` → HTTP 200)

A runner-provisioned `.mcp.json` may point coord-mcp at a **loopback proxy**:

```json
{ "mcpServers": { "coord-mcp": {
    "type": "http",
    "url": "http://127.0.0.1:<port>/coord-mcp",
    "headers": { "X-Coord-Mcp-Proxy-Key": "<nonce>" } } } }
```

**Two header shapes, both live.** Phase 2 of plan
`2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning` moves the nonce into
`"headers": { "Authorization": "Bearer <nonce>" }` — a custom header makes the
MCP client attach an OAuth provider, so a stale-key 401 escalates into discovery
and then Dynamic Client Registration, which the runner 404s. The runner keeps
honouring the legacy header and configs are rewritten only on session spawn, so
**both shapes coexist on disk indefinitely — read either.**

A raw JSON-RPC POST to that `url` with that header authenticates as a **device
principal** and the proxy injects a fresh device JWT per request — no static
bearer, no TTL worry. **The catch:** the workspace-root `.mcp.json` is often
**stale/mis-ported** (dead port or evicted nonce → 401) while a **sibling
repo's** `.mcp.json` (e.g. `qontinui-coord/.mcp.json`) holds the **live**
key/port. So **probe every candidate and use the first whose `tools/list`
returns HTTP 200.** This discovery block is `/gate`'s Step-2 sweep — if
`/gate`'s cascade is fixed, inherit the fix here rather than diverging.

Candidate order (cwd → repo root → siblings):

```bash
COORD_RPC='{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
# Workspace root = the directory containing the repo checkouts. $QONTINUI_ROOT
# overrides; otherwise the parent of the MAIN checkout via `--git-common-dir`;
# from a non-git cwd (e.g. the workspace root itself) fall back to $PWD.
#
# NOT `--show-toplevel`: inside a LINKED GIT WORKTREE that returns the worktree
# path, whose parent is the worktree container (`agent-worktrees/<uuid>`,
# `.claude/worktrees`) — no repo `.mcp.json` lives there, so the sweep below
# probes nothing and Step 2 reports "no live proxy" while one is live at the
# real root. Sessions run under QONTINUI_AGENT_WORKTREE_MODE=1, so this is the
# common path, not an edge case. `--git-common-dir` resolves to the MAIN repo's
# .git from a worktree and the canonical checkout alike.
ROOT="${QONTINUI_ROOT:-}"
if [ -z "$ROOT" ]; then
  GC="$(git rev-parse --git-common-dir 2>/dev/null)"
  [ -n "$GC" ] && GC="$(cd "$GC" 2>/dev/null && pwd)"
  [ -n "$GC" ] && ROOT="$(dirname "$(dirname "$GC")")"
fi
if [ -z "$ROOT" ] || [ "$ROOT" = "." ]; then
  echo "warn: not inside a git checkout — assuming \$PWD is the workspace root (set QONTINUI_ROOT to override)" >&2
  ROOT="$PWD"
fi
CANDIDATES=(
  "$PWD/.mcp.json"
  "$ROOT/.mcp.json"
  "$ROOT/qontinui-coord/.mcp.json"
  "$ROOT/qontinui-runner/.mcp.json"
  "$ROOT/qontinui-web/.mcp.json"
)
# also sweep any sibling repo .mcp.json not listed above — dedupe so the three
# explicit repos above aren't probed a second time (a live curl each) when they
# reappear in the glob.
while IFS= read -r f; do
  for c in "${CANDIDATES[@]}"; do [ "$c" = "$f" ] && continue 2; done
  CANDIDATES+=("$f")
done < <(ls "$ROOT"/*/.mcp.json 2>/dev/null)

# The proxy nonce is key material and must NEVER travel on curl's argv: process
# cmdlines are world-readable on this multi-session machine, so a nonce on argv
# leaks to every peer session — and this loop would leak EVERY candidate's, not
# just the live one. Stage it in a private tempfile and pass `curl -H @file`.
# (`cygpath -w` because a native curl.exe cannot open mktemp's POSIX path when
# MSYS pathconv is off.) Same rule for the device-JWT header in Steps 3-4 below.
HDR=$(mktemp) || { echo "mktemp failed — cannot stage the nonce off argv" >&2; exit 1; }
AUTH=""   # Steps 3 and 4 stage the device-JWT header here; ONE trap must cover
          # both, or a later `trap … EXIT` silently replaces this one and leaves
          # a live nonce in $TMPDIR after exit.
trap 'rm -f "$HDR" "$AUTH"' EXIT
hdrp() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$HDR" || printf '%s' "$HDR"; }

# jq is NOT guaranteed to exist — it is ABSENT on the Windows operator box
# (verified 2026-08-06). With `jq ... 2>/dev/null` inline, a missing binary is
# indistinguishable from an empty field: url/key come back EMPTY for EVERY
# candidate, the `continue` below skips them all, and the sweep reports "no live
# proxy" while a door is live — the SAME false-negative class the MSYS_NO_PATHCONV
# note below describes, from a different cause. Pick a reader up front and fail
# LOUD if neither exists, so a missing tool can never read as a coord verdict.
# NEVER use a shell positional parameter — a `$` followed by a single digit —
# anywhere in these fences. In a slash-command markdown body those are HARNESS
# ARGUMENT PLACEHOLDERS, not shell positionals: Claude Code substitutes the
# invocation's argument words into the body BEFORE injecting it, indexed from
# ZERO (the zeroth placeholder is the FIRST word), and leaves unfilled positions
# LITERAL. So on `/policy get policy escalation-bar` the first-index placeholder
# these readers used became the word `policy`; both opened a file named `policy`
# that does not exist; url AND key came back EMPTY for every candidate; and the
# cascade reported an exhausted door over a LIVE one — a silent-empty failure.
# Read the named `$MCP_CFG` set by the sweep loop below instead, and never
# reintroduce a positional. (This comment spells no `$`-digit of its own on
# purpose: it would be substituted too, garbling the warning.)
if command -v jq >/dev/null 2>&1; then
  # STDIN, not an argument — see the MSYS_NO_PATHCONV note below.
  mcp_url() { jq -r '.mcpServers["coord-mcp"].url // ""' < "$MCP_CFG" 2>/dev/null; }
  # BOTH header shapes. Plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning
  # Phase 2 moves the proxy nonce out of the custom `X-Coord-Mcp-Proxy-Key`
  # header and into `Authorization: Bearer <nonce>` -- a custom header makes the
  # MCP client attach an OAuth provider, so a stale-key 401 escalates into
  # discovery and then Dynamic Client Registration, which the runner 404s. The
  # runner keeps accepting the legacy header, so BOTH shapes sit on disk
  # indefinitely (configs are rewritten only on session spawn). Reading only the
  # legacy name would empty `key` on exactly the configs the fix produces and
  # this sweep would report "no live proxy" over a workspace full of live doors.
  # `Authorization` wins when both are present, mirroring the runner's own
  # precedence; the value is kept VERBATIM (`Bearer ` prefix included), and
  # `mcp_keyhdr` reports which header name to stage it under.
  mcp_key() { jq -r '(.mcpServers["coord-mcp"].headers // {}) as $h | if (($h.Authorization // "") | tostring) != "" then $h.Authorization else ($h["X-Coord-Mcp-Proxy-Key"] // "") end' < "$MCP_CFG" 2>/dev/null; }
  mcp_keyhdr() { jq -r 'if (((.mcpServers["coord-mcp"].headers.Authorization // "") | tostring) != "") then "Authorization" else "X-Coord-Mcp-Proxy-Key" end' < "$MCP_CFG" 2>/dev/null; }
elif command -v python >/dev/null 2>&1; then
  # BOTH the path and the key stay off argv, and for TWO different reasons.
  # The KEY is credential material: it is returned on stdout into a shell
  # variable, exactly as the jq arm does. The PATH is fed on STDIN because a
  # NATIVE python.exe cannot open a POSIX path under an inherited
  # MSYS_NO_PATHCONV=1 / MSYS2_ARG_CONV_EXCL='*' — the identical hazard the jq
  # arm below the loop is hardened against, and the identical failure: every
  # candidate reads EMPTY, every one is skipped, and the sweep reports "no live
  # proxy" over a workspace full of live doors. Until 2026-09-06 this comment
  # said the path on argv "is fine — it is not key material", which answers the
  # security question and silently waves through the path-conversion one; on the
  # Windows operator box, where jq is ABSENT, this arm is the ONLY arm, so the
  # defect sat on the live path. Measured that day: the sweep reported no live
  # proxy while http://127.0.0.1:9876/coord-mcp answered tools/list 200.
  # coord-revive.sh — the canonical resolver — already feeds BOTH arms on stdin
  # for this reason; this arm is now consistent with it.
  # Same no-positionals rule as the jq arm: `$MCP_CFG`, never `$N`.
  mcp_url() { python -c "import json,sys;print(json.load(sys.stdin).get('mcpServers',{}).get('coord-mcp',{}).get('url',''))" < "$MCP_CFG" 2>/dev/null; }
  mcp_key() { python -c "import json,sys;h=json.load(sys.stdin).get('mcpServers',{}).get('coord-mcp',{}).get('headers',{});print(h.get('Authorization') or h.get('X-Coord-Mcp-Proxy-Key','') or '')" < "$MCP_CFG" 2>/dev/null; }
  mcp_keyhdr() { python -c "import json,sys;h=json.load(sys.stdin).get('mcpServers',{}).get('coord-mcp',{}).get('headers',{});print('Authorization' if h.get('Authorization') else 'X-Coord-Mcp-Proxy-Key')" < "$MCP_CFG" 2>/dev/null; }
else
  echo "neither jq nor python can read .mcp.json — cannot probe any proxy candidate (LOCAL fault, not a coord verdict)" >&2
  exit 1
fi

LIVE_URL=""; LIVE_KEY=""; LIVE_HDR="X-Coord-Mcp-Proxy-Key"
for f in "${CANDIDATES[@]}"; do
  [ -r "$f" ] || continue
  # jq reads via STDIN so bash opens the file and no path crosses to the NATIVE
  # jq.exe. As an ARGUMENT, "$f" reaches jq unconverted under an inherited
  # MSYS_NO_PATHCONV=1 — which persists in the shell once any SSM runbook fence
  # exports it (ui-bridge.md, ui-bridge-debug/SKILL.md both do) — jq exits 2
  # "Could not open file", url/key come back EMPTY, and EVERY candidate is
  # skipped. The sweep then reports no live proxy while every door is fine:
  # a false "no policy door" verdict on the policy-read cascade itself. Same
  # fix PR #171 made in pr-status.sh's sweep.
  # Hand the candidate to the readers through a NAMED variable, never as a
  # positional argument — a positional inside this fence is harness-substituted at
  # injection time (see the readers' definitions above).
  MCP_CFG="$f"
  url=$(mcp_url)
  key=$(mcp_key)
  case "$url" in *"/coord-mcp"*) ;; *) continue ;; esac
  [ -n "$key" ] || continue
  # Verify the staging: `curl -H @<empty file>` does NOT error, it sends the
  # probe with NO credential — every door then 401s and the sweep concludes
  # "no live proxy" while every door is fine.
  { printf '%s: %s\n' "$(mcp_keyhdr)" "$key" > "$HDR"; } 2>/dev/null
  [ -s "$HDR" ] || { echo "cannot stage the nonce header (LOCAL fault, not a coord verdict)" >&2; break; }
  code=$(curl -s --connect-timeout 5 -m 20 -o /dev/null -w '%{http_code}' -X POST "$url" \
    -H "Content-Type: application/json" \
    -H @"$(hdrp)" -d "$COORD_RPC")
  if [ "$code" = "200" ]; then LIVE_URL="$url"; LIVE_KEY="$key"; LIVE_HDR="$(mcp_keyhdr)"; echo "live proxy: $f ($url)"; break; fi
  echo "skip stale: $f -> HTTP $code"
done
```

**Run each of these blocks as ONE shell invocation.** They rely on shell state
(`$LIVE_KEY`, `$HDR`, the `EXIT` trap); the Bash tool does not persist state
between calls, so splitting them mid-block leaves an empty variable and a
tempfile the previous call's trap already deleted.

> **A uniform failure across every candidate is the expected shape of ONE flaky
> probe, not corroboration.** These candidates usually name the SAME door — one
> `.mcp.json` per session workdir, all pointing at the local runner — so "all N
> timed out" is frequently N attempts at a single endpoint on a loaded box, not N
> independent verdicts. A curl exit 28 says nothing about the door; it says this
> box got no answer inside the budget. Before reporting no live proxy: **re-run
> the sweep once** (`coord-revive.sh`'s `probe_door()` now retries a `TIMEOUT`
> exactly once after `sleep 3`, and dedups candidates to distinct `(url, auth)`
> pairs so the count it prints is doors, not files). A live door has been
> reported DEAD this way — finding `4e8bcd86`.

If a live proxy is found, read via raw JSON-RPC `tools/call` against it. The
proxy carries MCP JSON-RPC only, so use the **MCP tools** here:

```bash
# $HDR/hdrp() still hold the winning nonce from the sweep above (same shell). In
# a fresh shell, re-stage it — never inline it on argv:
#   HDR=$(mktemp); trap 'rm -f "$HDR"' EXIT
#   printf '%s: %s\n' "$LIVE_HDR" "$LIVE_KEY" > "$HDR"   # $LIVE_HDR = the header name the sweep found the nonce under
#   hdrp() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$HDR" || printf '%s' "$HDR"; }
# list:
curl -fsS -X POST "$LIVE_URL" -H "Content-Type: application/json" \
  -H @"$(hdrp)" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name":"coord_list_prompt_documents","arguments":{}}}'
# get one:
curl -fsS -X POST "$LIVE_URL" -H "Content-Type: application/json" \
  -H @"$(hdrp)" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"coord_get_prompt_document",
        "arguments":{"kind":"<kind>","name":"<name>"}}}'
```

Read the document(s) out of the JSON-RPC `result`.

> **Which file wins decides which tenant you read as.** With several accounts
> and session-scoped tenancy on one machine, a sibling repo's `.mcp.json` may
> hold a **different account's** proxy nonce (the injected device JWT — and so
> the tenant — follows the nonce). Sweep your own worktree / `$PWD` first when
> it matters, and check which candidate file won (the `live proxy: <file>`
> line) before trusting the attribution.

### Step 3 — Generic remote MCP: `POST $COORD_HTTP_URL/mcp` (probe: `tools/list` → HTTP 200 with a tool catalog)

Coord serves its **whole MCP tool surface** over one plain HTTPS POST at
`$COORD_HTTP_URL/mcp` — JSON-RPC in the body, **no session handshake, no
`Mcp-Session-Id` to carry**, guarded by `require_jwt` alone. Verified live
2026-08-31: with a device JWT it answers **200 and a 75-tool catalog**;
unauthenticated it answers **401**.

**Why this sits above Step 4 even though Step 4 also leaves the box.** Step 4 is
a pair of hand-written REST paths, so it reaches exactly the two routes someone
wrote out by hand. This rung addresses coord tools *by name*, so the same door
carries anything the tenant's catalog exposes — including the ~14 coord writes
that have no REST twin at all (`coord_report_status`, `coord_send_message`,
`coord_record_decision`, `coord_memory_record`, `coord_reserve_resource`,
`coord_request_handoff`, `coord_yield`, `coord_request_merge`, …). A session
whose local transport is dead otherwise keeps most of its *sight* and loses most
of its *voice*. For `/policy`'s own two reads either rung works; this one is
first so the door a reader copies out of this file is the general one.

> **The scope fence still holds — it is a property of `/policy`, not of the
> transport.** This rung *can* call any tool; `/policy` calls exactly
> `coord_list_prompt_documents` and `coord_get_prompt_document` from it. The
> Non-goals above are unchanged: no create, no patch, no restore-default.

**Credential — the DEVICE JWT, and only that.** Three sources, first hit wins:
`$COORD_DEVICE_JWT`; then `~/.qontinui/coord-device-jwt`; then a mint from the
local runner, which holds no secret at rest. That cascade is already implemented
and freshness-checked in `scripts/lib/coord-credential.psm1`
(`Get-CoordDoorTransport` → `Kind = 'bearer'`), which also reports a headless
runner as a **dead transport** rather than a missing credential — call it
rather than writing a fourth copy of the cascade here.

> ⚠️ **NEVER carry this rung on a JWT minted from `POST /agents/allocate`.**
> That route is genuinely unauthenticated and mints a 4-hour full-scope agent
> JWT to anyone who knows a registered device UUID. It is an open security
> question the plan
> `2026-08-31-coord-mcp-credential-selection-by-binding-provenance` surfaces and
> explicitly refuses to build on; adding a rung that depends on it would deepen
> exactly the exposure being questioned. Device JWT, or this rung does not run.

```bash
COORD_HTTP_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
# 1) Resolve the device JWT. $ROOT is the workspace root resolved exactly as the
#    Step-2 block does (`--git-common-dir`, NOT `--show-toplevel`).
DEVICE_JWT="${COORD_DEVICE_JWT:-}"
[ -n "$DEVICE_JWT" ] || DEVICE_JWT="$(tr -d '\r\n' < "$HOME/.qontinui/coord-device-jwt" 2>/dev/null)"
[ -n "$DEVICE_JWT" ] || DEVICE_JWT="$(powershell -NoProfile -Command "
  Import-Module '$ROOT/qontinui-claude-config/scripts/lib/coord-credential.psm1' -Force
  \$t = Get-CoordDoorTransport -Cwd (Get-Location).Path
  if (\$t.Kind -eq 'bearer') { \$t.Jwt } else { [Console]::Error.WriteLine(\$t.FailureReport) }" 2>/dev/null | tr -d '\r\n')"
# 2) Stage it OFF argv — same rule and same reason as the Step-2 nonce. $AUTH +
#    the EXIT trap come from the Step-2 block when you carried that shell
#    forward; in a fresh shell the guard below creates both, and the trap MUST
#    name $HDR too or it silently replaces Step 2's and leaks the nonce.
[ -n "$AUTH" ] || { AUTH=$(mktemp) || exit 1; trap 'rm -f "$HDR" "$AUTH"' EXIT; }
# An empty JWT would stage 'Authorization: Bearer ' and coord answers 401 —
# which reads as a coord verdict when the truth is a LOCAL fault.
[ -n "$DEVICE_JWT" ] || { echo "no device JWT resolvable (LOCAL fault, not a coord verdict) — see the FailureReport above" >&2; exit 1; }
printf 'Authorization: Bearer %s\n' "$DEVICE_JWT" > "$AUTH"
[ -s "$AUTH" ] || { echo "cannot stage the JWT header (LOCAL fault)" >&2; exit 1; }
AUTHP=$AUTH; command -v cygpath >/dev/null 2>&1 && AUTHP=$(cygpath -w "$AUTH")
# 3) PROBE first — this rung's own validation. A 200 whose body carries no
#    JSON-RPC `result` is NOT a live door; treat it as dead and fall to Step 4.
# BUDGET. `--connect-timeout 5 -m 15` — the same pair `coord-revive.sh` spends
# on this EXACT call (`PROBE_CONNECT_TIMEOUT` / `PROBE_TIMEOUT`, whose comment
# names "the L3/L4 bearer probes against ${COORD_URL}/mcp" as what the 15s
# covers). A probe is the one call in a cascade that must not be allowed to
# hang: this rung is REMOTE, so unlike the Step-2 loopback sweep a black-holed
# host can stall it indefinitely, and it is the last live rung — a hang here
# costs the caller the honest-failure report it was owed. The action calls
# below carry no bound on purpose: once the probe has said the door is live, a
# slow write is still a write, and killing one at 15s would leave a coord-side
# effect nobody read back.
curl -fsS --connect-timeout 5 -m 15 -X POST "$COORD_HTTP_URL/mcp" \
  -H "Content-Type: application/json" \
  -H @"$AUTHP" -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
# list:
curl -fsS -X POST "$COORD_HTTP_URL/mcp" -H "Content-Type: application/json" \
  -H @"$AUTHP" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"coord_list_prompt_documents","arguments":{}}}'
# get one:
curl -fsS -X POST "$COORD_HTTP_URL/mcp" -H "Content-Type: application/json" \
  -H @"$AUTHP" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"coord_get_prompt_document",
        "arguments":{"kind":"<kind>","name":"<name>"}}}'
```

Read the document(s) out of the JSON-RPC `result`, exactly as in Step 2 — this
is the same MCP surface, reached over HTTPS instead of the loopback forwarder.
**A `401` here is a credential verdict, not a coord outage**, and a `404` on
`/mcp` would mean this coord deployment predates the route — say which you got.

⚠️ **Say which you got — and probe a second, independent instance before you say
WHY.** `401` and `404` are measurements and are always reportable. *"this coord
deployment predates the route"*, *"coord is down"*, *"the policy surface is
gone"* are **mechanisms**, and one request from one client cannot establish any
of them. The falsifier is unconditional and needs **no credential at all** —
Step 4's route below answers a bare status code to an unauthenticated caller:

```bash
curl -sS -o /dev/null -w '%{http_code}\n' -m 10 \
  "${COORD_HTTP_URL:-https://coord.qontinui.io}/coord/agent-prompt-documents"
```

This is **not** Step 4 done early: Step 4 needs a tenant-resolvable JWT to
*read* a document, while this needs nothing to *falsify a cause*. A `401` here
is a **pass** — it proves the deployment is up and routing `/coord/…`, so a
`404` on `/mcp` means that one route is unmounted for this caller, not that the
deployment predates it. Only a curl that fails to **connect** leaves the
deployment in question, and then the verdict is **UNKNOWN**, never "coord is
down". Whatever you conclude, name both probes in the rung-3 line of your
report — and never let a rung-3 failure be reported as a policy answer. The
stamped form of that line is `bash .claude/skills/coord-revive/coord-revive.sh --floor-claim`:
it runs this same probe as one door of its cascade and prints a `FLOOR-CLAIM:`
block (probe time, runner build, this box's load, one line per door) — paste
that block; a `verdict=UNKNOWN` there (sampled under own load) means the
rung-3 line is UNKNOWN, and "coord is down" is written from a `verdict=FLOOR`
block or not at all.

### Step 4 — Direct device-authed HTTP, hand-written routes (probe: the GET itself)

`GET $COORD_HTTP_URL/coord/agent-prompt-documents` (inventory) and
`GET $COORD_HTTP_URL/coord/agent-prompt-documents/<kind>/<name>` (one body).
`$COORD_HTTP_URL` defaults to `https://coord.qontinui.io`.

- **NOT `/coord/prompt-documents`** — that is the operator CRUD surface,
  `TenantId`-gated on an operator Cognito context; it **403s for a device or
  agent JWT**. The 403 is expected behaviour, not a defect and not a
  capability floor — the agent door above is the route.
- **The JWT must be TENANT-RESOLVABLE:** a device JWT (tenant from its
  `coord.devices` row) or any JWT carrying a `tenant_id` claim. A bare service
  token passes `require_jwt` but the handler 4xxs with
  `cannot resolve the caller's tenant` (verified live 2026-07-27).
- **Step 3 above already resolved this same credential** — reuse `$DEVICE_JWT`
  and the header it staged rather than resolving a second one. This rung and
  Step 3 take the identical device JWT; only the protocol differs. The
  `/agents/allocate` prohibition in Step 3 applies here unchanged.
- If the session holds no device JWT, **mint one via the documented pair-cli
  cascade** (memory `reference_coord_device_jwt_noninteractive_mint`: admin
  secret → service token → `POST /coord/devices/pair-cli` → ~4h device JWT;
  or extract the live runner's JWT via its page-evaluate door when the runner
  is up). Served policy `production-and-cost` `prod-reads-free`: a mint used
  read-only and discarded counts as an authorized read mint — do not stall on
  the credential's broader nominal scope. **Hygiene bounds are absolute:** mint read-only, use, DISCARD — never
  persist the JWT to disk beyond the staged header tempfile the trap deletes,
  never print it, never put it on any process's argv.

```bash
COORD_HTTP_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
# Stage the JWT header OFF argv (printf is a shell BUILTIN — no process, no
# cmdline to read). If you carried Step 3's shell forward, $DEVICE_JWT, $AUTH
# and $AUTHP are already resolved and staged — skip to the curls. $AUTH + the
# EXIT trap otherwise come from the Step-2 block; in a fresh shell:
#   AUTH=$(mktemp); trap 'rm -f "$AUTH"' EXIT
# The guarded trap MUST also cover $HDR: when this shell was carried forward
# from Step 2 or 3, a trap naming only $AUTH would REPLACE that combined trap
# and leave the live proxy nonce in $TMPDIR after exit (rm -f on an unset
# $HDR is harmless in the fresh-shell case).
[ -n "$AUTH" ] || { AUTH=$(mktemp) || exit 1; trap 'rm -f "$HDR" "$AUTH"' EXIT; }
# An empty $DEVICE_JWT would stage 'Authorization: Bearer ' and coord answers
# 401 — which reads as a coord verdict when the truth is a LOCAL fault. Guard
# before staging:
[ -n "$DEVICE_JWT" ] || { echo "no device JWT in \$DEVICE_JWT — mint first (LOCAL fault, not a coord verdict)" >&2; exit 1; }
printf 'Authorization: Bearer %s\n' "$DEVICE_JWT" > "$AUTH"
[ -s "$AUTH" ] || { echo "cannot stage the JWT header (LOCAL fault)" >&2; exit 1; }
AUTHP=$AUTH; command -v cygpath >/dev/null 2>&1 && AUTHP=$(cygpath -w "$AUTH")
# list:
curl -fsS "$COORD_HTTP_URL/coord/agent-prompt-documents" -H @"$AUTHP"
# get one:
curl -fsS "$COORD_HTTP_URL/coord/agent-prompt-documents/<kind>/<name>" -H @"$AUTHP"
```

The list route returns the full inventory the caller's tenant can see — every
`policy` document plus the other kinds — and the one-doc route returns the
versioned body. **Report the count the route returned; never a remembered
one.** (This line used to hard-code "14 policy documents as of 2026-07-27";
the inventory grows, and a quoted count is the same stale-source defect Step 5
now refuses to commit.) Tenant always derives server-side from the JWT — never
pass a tenant argument.

#### Step 4b — the bootstrap credential: LIVE on this tenant (re-measured 2026-09-10)

**This rung works. Try it before falling through to the mirrors.** Measured
against production coord on 2026-09-04 from `merytshost` and **re-measured
2026-09-10**: the anonymous `POST $COORD_HTTP_URL/agents/credential` answered
**`200`** with `{token, token_exp, token_jti}`, and that bearer read
`GET $COORD_HTTP_URL/coord/agent-prompt-documents` and one policy document at
`200` each — the same two routes answering **`401` without it** — i.e. it
carries a `/policy` read end to end, and the mint is a real authentication
rather than a string that happens to be accepted.

**The route is in coord's source, so this is not probe-only.**
`pub async fn post_credential` (`crates/coord/src/agent_worktrees.rs`) is
registered at `/agents/credential` in `crates/coord/src/routes.rs` on
`qontinui-coord` `origin/main`, commit **`5dd99cc3`** (PR #1850, ff-landed).
Cite the commit for *existence* and a dated probe for *reachability* — they go
stale at different rates, and collapsing them is what produced the error below.

> ⚠️ Until 2026-09-04 this section said the rung "cannot currently succeed on
> any deployment". That was measured wrong, and it sent sessions to a
> drift-unverifiable mirror while a live door was open. **Re-probe before
> quoting any status here** — including this one.

**The problem it is shaped for is real.** The rungs above are less independent
than the numbering suggests: Step 2 *is* the runner, and every mint in the
Step-3/Step-4 credential cascade except the two static files is minted *by* the
runner. One wedged runner takes all of them down at once, and the static files
hold a **~4h** device JWT, so they are expired far more often than not. Measured
2026-08-28: a closeout on such a box walked the whole cascade and reached only
the file mirrors — which answered, and are **read-only and drift-unverifiable**.
For a READ door that is the whole cost: a mirror whose drift is UNKNOWN instead
of a live policy body. Plan
`2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline`.

**The shape:** resolve a `device_id`
(`$QONTINUI_MACHINE_ID` first, else `~/.qontinui/machine.json` `"device_id"`,
falling back to the legacy `"machine_id"`) and POST it **anonymously** to
`$COORD_HTTP_URL/agents/credential` — a dedicated credential-only route, which is
the `pair.rs::pair_via_browser` carve-out shape (anonymous *because it mints the
credential, so requiring one is circular*). It answers `200` with a `token`
field: an EdDSA JWT, `sub=device:<device_id>`, `sub_type=agent`, tenant
resolved, all scopes empty, ~4h TTL.

**If it does NOT answer `200`, say which code you saw.** A `404`/`405` is a
router artefact — `POST /agents/definitely-not-a-route` returns the identical
empty `405` (2026-09-02, re-confirmed 2026-09-04), so "route absent" is spelled
405 here rather than 404 — but since the route answered `200` on 2026-09-04, a
`405` from it now reads as a regression or a different deployment, not as a
known-absent rung. Then fall through to Step 5 with its full staleness
disclosure.

> ⚠️ **A failing route does NOT license `POST /agents/allocate` as a substitute.**
> Step 3's prohibition above is unqualified and three shipped documents carry it.
> The gate this paragraph used to cite as live authority — coord gate
> **`ece99898-30c6-4f8c-be8e-1de5f09abebc`** (`operator_approval`, `gate_class:
> security-surface`) — is **`withdrawn`**; re-verify with `coord_gate_inspect`
> before citing it here or anywhere. Read over the live coord door 2026-09-06,
> the withdrawal reads *"over-broad and superseded by gate
> `3c9b18ca-3300-4dbe-a2f4-1d6db5e5a6d5`"* — inspect that gate too
> (`coord_gate_inspect`): it is open, but anchored to a **different arm**, the
> `agent_tool_access` uncurated-catalog fallback, not the allocate mint.
> **So the allocate question is UNKNOWN — neither still-gated nor cleared.**
> Whether it was among the four arms the withdrawal called "not the operator's to
> decide", or is simply unasked, is settled by neither read — and **UNKNOWN is
> not permission**. The prohibition therefore stands exactly as written: the
> honest outcome of this rung is **fall through to Step 5 with its full staleness
> disclosure** — never a token from `/agents/allocate`.
> (This paragraph said "currently **open**" until 2026-09-06. A withdrawn gate
> reads identically to an open one in prose, which is precisely why the pointer
> above is mandatory and this sentence is not a substitute for asking.)

Three things stay true of this rung now that it is reachable, and they
are `/policy`-specific:

- **The scope fence is a property of `/policy`, not of any transport.** The
  Non-goals at the top of this file are unchanged: this rung fetches exactly
  the two prompt-document reads and nothing else. A credential obtained as a last
  resort is the *worst* one to widen scope with, regardless of how narrow its
  claims are (measured 2026-09-04: every scope in the minted token was empty or
  false — the fence is a rule about what `/policy` does, not a hope about what
  the bearer can reach).
- **Verify before you read, and say what the verification covered.** The control
  read is `GET $COORD_HTTP_URL/coord/agent-findings?limit=1` — `200` with a good
  bearer, `401 {"error":"missing Bearer token"}` without one, both verified live
  2026-09-02. A mint is not an authentication, and a false green here is worse
  than a mirror: it would serve a failed read as a policy answer. What that
  control read does **not** by itself prove is **tenant resolution** — the
  agent-prompt-document routes need a JWT whose tenant coord can resolve. On
  2026-09-04 the bootstrap bearer resolved tenant fine (both
  `/coord/agent-prompt-documents` and one policy document answered `200`), but
  that is those routes' evidence on that day, not a guarantee: a
  `403 cannot resolve the caller's tenant` remains **that route's own verdict**,
  not a refutation of the credential. On that 403, fall to Step 5 **with its full
  staleness disclosure** — never report a 403 as "no policy found".
- **Say which rung carried the read, as always.** When this rung carries
  one, the transport line is `HTTP agent door (bootstrap credential)`, never a
  bare `HTTP agent door`: a reader weighing a policy answer is entitled to know it
  came from the rung of last resort.

**Record that you got this far.** Reaching Step 4b means every ordinary door
failed, and nothing counts how often that happens — the supervisor already
*detects* the underlying wedge (`health_cache.rs` step 3e, `RUNNER WEDGED: …`
every ~5 min, refusing to auto-restart by contract), so the gap is aggregation,
not detection. Append one record to the guard component's existing local
breadcrumb. It is local on purpose: a counter that had to reach coord would be
missing in exactly the outage it measures. **The count is the point whatever the
rung returns** — a rising `l5-reached` rate is the fleet's only
view of how often a session is driven this far.

```bash
. "$ROOT/qontinui-claude-config/scripts/lib/guard-decision-log.sh" 2>/dev/null \
  && guard_decide policy warn l5-reached
```

### Step 4c — the six INTENT kinds only: the local steering cache (disclosed, never silent)

The mirrors in Step 5 cover kind `policy` **exclusively** — a rung-5 `get` for
any other kind is reported unavailable. So until this rung existed, the six
**intent** kinds (`product_intent`, `initiative`, `success_metric`,
`domain_spec`, `audience_profile`, `decision_record`) had **no** last rung at
all: a session with every live door dead could not read what the tenant is
building, only how it must behave. Plan
`2026-09-02-steering-layers-unreadable-without-a-credential` Phase 1f adds the
local **steering cache** — rendered detached at every SessionStart by
`.claude/hooks/render-steering-cache.sh` → `scripts/render-steering-cache.ps1`,
in `$QONTINUI_STEERING_CACHE_DIR` (default `C:/claude/steering-cache`) — and
this rung reads it. **For the six intent kinds only.** A `get` for `policy` or
any other kind skips this rung and falls to Step 5 unchanged.

Only when rungs 1–4 **all** fail, Step 4b included. Same doctrine as Step 5:
a cache is as old as its Rendered stamp, **stale or absent is UNKNOWN, never
empty**, and every answer served from here says so:

```bash
STEERING="${QONTINUI_STEERING_CACHE_DIR:-C:/claude/steering-cache}"
# Absent is a rung-4c FAILURE, not an empty corpus. Say which.
if [ ! -f "$STEERING/STEERING-CACHE.json" ]; then
  echo "rung 4c UNAVAILABLE: no steering cache at $STEERING (never rendered on this box, or a different QONTINUI_STEERING_CACHE_DIR) - UNKNOWN, not 'no intent documents'" >&2
else
  # Provenance FIRST. rendered_at is the last SUCCESSFUL render; the sidecar's
  # render_exit_reason says what the LAST ATTEMPT did, which may be a failure
  # that left this stamp where it was.
  python -c "import json,sys;d=json.load(sys.stdin);print('STEERING CACHE (rung 4c) rendered',d['rendered_at'],'UTC over',d['transport'])" < "$STEERING/STEERING-CACHE.json"
  python -c "import json,sys;d=json.load(sys.stdin);print('  last attempt',d.get('last_attempt_at'),'->',d.get('render_exit_reason'))" < "$STEERING/STEERING-CACHE.state.json" 2>/dev/null || echo "  (no readable sidecar - the last attempt's outcome is UNKNOWN)"
  echo "  DRIFT: UNKNOWN - no live door answered, so nothing here can be compared to served."
  # A drift verdict is only as wide as the doors that were ASKED, so name them
  # beside it. Without this line "no live door answered" reads as "the doors are
  # down", when what actually happened is that ONE host was asked: rungs 1-4b are
  # coord.qontinui.io plus loopback, and this cache rung probes no host at all.
  # api.qontinui.io is a different program on a different host (measured
  # 2026-09-06: 401 anonymous, 200 with a user_id-bearing device JWT), so unless
  # you asked it, it is UNPROBED - not down. Edit the roster to what you ran.
  echo "  axes: hosts=coord.qontinui.io prefixes=/coord/agent-* credentials=proxy-nonce,device-jwt unprobed=api.qontinui.io"
  # list: the six kinds with the SKELETON flag AS RENDERED. Read the flag: a
  # SKELETON row is the unedited seed - UNKNOWN, not intent - and UNKNOWN means
  # the served row carried neither field, which is NOT `authored`.
  python -c "import json,sys;d=json.load(sys.stdin);[print(f\"{x['kind']:18} {x['name']:40} v{x['current_version']} {x['skeleton']}\") for x in d['documents']]" < "$STEERING/STEERING-CACHE.json"  # envelope-ok: the local steering-cache render written by render-steering-cache.ps1, not a fleet door response
  # get one: KIND and NAME are shell variables you set - never a positional,
  # which in a slash-command body is a harness placeholder.
  BODY=$(python -c "import json,sys;d=json.load(sys.stdin);print(next((x['body_file'] or '' for x in d['documents'] if x['kind']==sys.argv[1] and x['name']==sys.argv[2]),''))" "$KIND" "$NAME" < "$STEERING/STEERING-CACHE.json")  # envelope-ok: the local steering-cache render written by render-steering-cache.ps1, not a fleet door response
  if [ -z "$BODY" ]; then
    echo "$KIND/$NAME: NOT in the steering cache rendered $(python -c "import json,sys;print(json.load(sys.stdin)['rendered_at'])" < "$STEERING/STEERING-CACHE.json") - UNKNOWN, not absent (the render may predate it, or its body read failed that run)" >&2
  else
    cat "$STEERING/$BODY"
  fi
fi
```

Rules for this rung, in addition to Step 5's:

- **Quote the Rendered stamp on every answer**, and the transport line reads
  `steering cache (rendered <stamp>)` — never a bare "HTTP agent door".
- **The `skeleton` flag is served as rendered, never recomputed here.** It was
  resolved from the served row at render time — coord's `unedited_seed` verdict
  where that build served one, else the term-by-term fallback, which leaves
  `UNKNOWN` only for a SEEDED row past version 1; a cached `authored` says the
  row was authored *then*.
  `/chart` still refuses cached input for its Step 1.2 filter — a
  ranking against a stale skeleton flag is the confident-wrong shape — so this
  rung feeds a **read**, not a ranking.
- **Dossier heads are in the same cache** (`dossiers/<slug>.md`, resolved by
  client-side title prefix; `STEERING-CACHE.json` says `dossiers_refreshed` and
  `dossier_hits_at_cap`). They are outside `/policy`'s two verbs; read them
  with `coord-read.ps1 steering dossier <slug>`, which serves the same cache
  labelled.
- **A dead proxy binding is not this rung's case.** A coord-mcp that answers
  `401` over a healthy transport wants `/coord-revive`; reaching for a cache
  there serves stale intent over a live door. Earn "no live door" with the
  unauthenticated `curl` Step 5 prescribes before you serve from here.

### Step 5 — LAST RESORT: the file mirrors (disclosed, never silent)

Only when rungs 1–4 **all** fail — **Step 4b included**, since a live read beats
an unverifiable mirror and 4b is the one rung a dead runner cannot take down —
and, for the six intent kinds, after Step 4c — read the mirrors at
`$ROOT/qontinui-dev-notes/prompts/policy-bodies-phase0/*.md` (derive `$ROOT`
exactly as the Step-2 block does). This rung is not an invention:
`policy/session-protocol` blesses exactly this fallback — with a **mandatory
staleness disclosure**.

**Nothing keeps these files fresh.** There is no maintaining process: the
directory is the phase-0 seeding artifact, repaired by hand whenever a session
trips over the drift. Measured 2026-08-06, six days after the most complete
hand regeneration the directory has ever had, **6 of its 14 mirrors were behind
the served store**. So this rung serves a mirror only *with* its provenance,
never as a current answer.

Re-measured 2026-08-30, against the tree as it stood before that day's refresh:
**2 of 14 were behind** (`engineering-priorities` and `escalation-bar`, one
version each). Lower than 08-06 — but the load-bearing half is *which* mirrors
the age signal pointed at. Of the five old enough to trip the ~7-day line, three
were perfectly **current**; meanwhile `coordination`, the fastest-moving document
in the set, was three days old and therefore **silent**. The cause is structural,
and easy to miss because `rendered_at` looks like it means something it does not:

> **`rendered_at` is a re-render timestamp, not a verification timestamp.** The
> renderer rewrites a mirror only when it is behind (or missing the key), so a
> mirror it checked and found *already current* keeps its old date. Age here
> measures **time since this policy last changed**, not **time since this mirror
> was last checked** — and for picking out risk those are close to opposites. A
> stable document looks alarming; a churning one looks fresh.

That is why the snippet below discloses **every time**, and lets age shade only
the wording. Recording the gap rather than quietly closing it: a `verified_at`
key that moved on every successful comparison would make age mean what readers
already assume it means, at the cost of rewriting all 14 headers on every run —
a provenance-contract change spanning the renderer, this rung and the linters
together, not a tweak to any one of them.

> **Never state a mirror count from memory.** This section used to carry a
> hard-coded "N mirrors vs M live policy documents" line, naming a specific
> document as un-mirrored. It was accurate the day it was written and false
> three days later, and it kept telling every reader so for another week. A
> hand-written freshness claim inside the freshness disclosure is the same
> defect one level up. **Count with `ls` at read time, or say nothing** — and
> assert it: this file must contain no literal mirror count, which is a
> one-line grep in review.

#### The serve path (this is the branch that actually runs)

Rung 4 is reached **only when no coord transport is reachable** — so the served
`current_version` is, by construction, **unknowable here**. Do not attempt a
mirror-vs-served comparison on this path; it cannot run. Age and the mirror's
own claim are the only signals available, and **both must appear in every
rung-5 response**, not once at the top:

- Say the mirrors were used, and name the **exact file** read.
- Print the mirror's `Mirrors served version N` stamp and its render date, then
  state plainly: **"cannot verify against served — no coord transport is
  reachable, which is why you are reading a mirror."** Never phrase a mirror as
  current, and never let the absence of a comparison read as agreement.
- Print the stamp's **age in days**, and next to it an **explicit drift-UNKNOWN
  line, unconditionally**. Age must never gate that disclosure: it is a
  re-render date, not a check date (see the box above), so a low age is not
  evidence of freshness and a high one is not evidence of drift. Age shades the
  **wording** only — never whether the reader is told.
- **A mirror whose stamp is missing or unparseable is reported UNAVAILABLE, and
  its body is NOT printed.** A file that cannot say which version it reflects is
  not a policy answer — it is an unattributed string. This is the same
  absence-is-not-zero reading as `verification-and-evidence`
  `silent-empty-is-unknown`.
- `get` for a document that has **no mirror** → say so by name, and report the
  read as unavailable. **Never silently substitute** a different document or
  an older body presented as current.
- The mirror set covers **kind `policy` exclusively** — mirrors are keyed by
  name only, so a rung-5 `get` for any OTHER kind (`agent_playbook`,
  `continuation_rules`, `prompt_template`, `response_prompt`) is reported
  unavailable by kind+name, never served from a same-named policy mirror. The
  six intent kinds have their own last rung, Step 4c, and never reach here.
- `list` from mirrors = the filenames present **counted at read time**, labelled
  as the mirror set, not the live inventory.
- **Say which COMMIT the mirrors came from, not just which version they claim.**
  This rung reads a shared checkout, so there are two independent staleness
  axes: mirror-vs-served (what the stamp is about) and **checkout-vs-origin**
  (what nothing used to report). Prefer `origin/main`'s blob and print the
  checkout's distance; when only the worktree is readable, say so loudly. A
  correctly-stamped mirror on a branch 466 commits behind is still superseded
  policy, and it looks identical to a current one.
- **Never report a worktree comparison as fleet drift.** If you compare mirrors
  to served from a checkout that is behind `origin/main`, you are measuring your
  own checkout, not the fleet. Three sessions have now made exactly that claim.
- **"No coord transport is reachable" is a CAUSE — earn it with one `curl`
  before you serve a mirror on it.** Rungs 1–4 failing is a *measurement* about
  this session's doors: a masked tool, a dead nonce, an unmintable JWT. Reaching
  this rung does **not** establish that coord is unreachable, and the difference
  decides what the reader should do next — re-run `/coord-revive` and get live
  policy, or accept a stamped mirror. Before printing the sentence at the top of
  this section, ask a **second, independent instance** of the door:
  `curl -sS -o /dev/null -w '%{http_code}\n' -m 10 "${COORD_HTTP_URL:-https://coord.qontinui.io}/coord/agent-prompt-documents"`.
  It needs no tool, no nonce and no credential, so nothing about a degraded
  session excuses skipping it, and a `401` is a **pass**: the deployment is up
  and serving, so what you actually have is a *credential* failure, and the
  response must say that instead. Only a connect failure — or a probe you could
  not run — leaves it **UNKNOWN**, which is still not "coord is down". Print the
  probe's own result beside the drift-UNKNOWN line: this rung's whole contract
  is that the reader is never left to infer what was not checked.
- **That probe is the WEAK instance — it shares a host with the thing you are
  accusing. Ask the SEPARATE HOST too, and say which surface answered.**
  Every LIVE rung above, and the probe in the bullet before this one, is
  `coord.qontinui.io` plus loopback — one host, one program, so all of them can
  fail together for one cause that says nothing about the fleet's serving plane
  (rungs 4c and 5 are local files, so they are not a second host either).
  `api.qontinui.io` is a **different program on a different host** (qontinui-web's
  FastAPI backend), so it is the genuinely independent instance:
  `curl -sS -o /dev/null -w '%{http_code}\n' -m 10 "${QONTINUI_WEB_HTTP_URL:-https://api.qontinui.io}/api/v1/plan-library?kind=plan&limit=1"`.
  Measured 2026-09-06: `401` anonymous, **`200`** with a `user_id`-bearing device
  JWT (`~/.qontinui/coord-device-jwt`; an `/agents/allocate` token carries no
  `user_id` and never substitutes, on this rung or any other). **Either code is a
  pass.** What it buys is a *decomposition*, which is why it is worth a second
  `curl`: a `200` here with the same credential that 401s on
  `coord.qontinui.io` says the credential is live and the fault is coord-side;
  a `401` on both says the credential is the fault; a connect failure here leaves
  the web-host axis **UNKNOWN**, which is still not "down".
  ⚠️ **Two discriminators, both measured the same day, both of which read as a
  credential verdict to a careless probe.** (i) **Method, not credential:** `GET
  https://api.qontinui.io/api/v1/memory/query` answers `405` while `POST` answers
  `401` anonymous and `422` with a device JWT — a `422` there means the door is
  OPEN and only the body was wrong. (ii) **Prefix, not credential:** `POST
  https://coord.qontinui.io/agents/allocate` answers `422` anonymous while `POST
  https://coord.qontinui.io/coord/agents/allocate` answers `401` — the same
  capability, one path segment apart.
  It is a **liveness and credential** axis, not a sixth rung: prompt documents
  live in coord and `api.qontinui.io` does not serve them, so a `404` for a
  policy path there is a statement about that program's route table and nothing
  about the document. Do not read one as "the policy does not exist".
  **`/policy` may not report a document, a kind, or the policy surface itself as
  "unavailable" while this axis is unprobed.** An unprobed axis is
  `not attempted`, never `unavailable`. This is the same disclosure discipline
  the rung already applies to a served mirror and to the Step 4c cache: **say
  which surface answered** — `coord.qontinui.io`, `api.qontinui.io`, the steering
  cache, the file mirrors — and say which you never asked.

```bash
MIRRORS="$ROOT/qontinui-dev-notes/prompts/policy-bodies-phase0"
DN="$ROOT/qontinui-dev-notes"

# The directory ABSENT is a rung-5 failure, not "a mirror set of size zero".
# Without this guard the count below prints 0 and reads as "there are no
# mirrors" — absence reported as emptiness, the exact thing this rung is
# supposed to be honest about.
if [ ! -d "$MIRRORS" ]; then
  echo "rung 5 UNAVAILABLE: no mirror directory at $MIRRORS (no qontinui-dev-notes checkout?)" >&2
  echo "this is a LOCAL fault — report it as such, never as 'no policy found'" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# THE CHECKOUT AXIS. Everything else in this rung compares MIRROR to SERVED.
# There is a second axis, and it is the one that actually bit: the path above
# is a plain filesystem read of the WORKING TREE, so it serves whatever branch
# this shared checkout happens to be parked on. Measured 2026-08-31 on
# merytshost: dev-notes sat on a peer's branch 466 commits behind origin/main,
# and rung 5 served verification-and-evidence v2 while BOTH origin/main and the
# served store were at v7. Nothing warned — the version stamp is honest about
# the mirror it came from and says nothing about which COMMIT that mirror is,
# and mtime is checkout time, which is biased FRESH.
#
# So: prefer origin/main's blob, and ALWAYS report the distance. Three separate
# sessions have now reported a stale checkout's distance as FLEET drift
# (dev-notes#364's commit message, and two /unattended closeouts) — that is a
# measurement error this rung should make structurally hard, not a slip.
git -C "$DN" fetch -q origin main 2>/dev/null || true
MIRROR_SRC="worktree"
BEHIND=$(git -C "$DN" rev-list --count HEAD..origin/main 2>/dev/null || echo "?")
if git -C "$DN" cat-file -e "origin/main:prompts/policy-bodies-phase0" 2>/dev/null; then
  MIRROR_SRC="origin/main"
fi
# Read a mirror by NAME through whichever source won. Never inline a shell
# positional here — in a slash-command body those are harness placeholders.
# Reads $MIRROR_NAME (a variable, NOT an argument) and writes the body to
# stdout. It takes no parameter on purpose: a shell positional inside a
# slash-command fence is a HARNESS placeholder, substituted at injection time.
mirror_body() {
  if [ "$MIRROR_SRC" = "origin/main" ]; then
    git -C "$DN" show "origin/main:prompts/policy-bodies-phase0/${MIRROR_NAME}.md" 2>/dev/null
  else
    cat "$MIRRORS/${MIRROR_NAME}.md" 2>/dev/null
  fi
}
echo "mirror source: $MIRROR_SRC (checkout is ${BEHIND} commits behind origin/main)"
if [ "$MIRROR_SRC" = "worktree" ]; then
  echo "  WARNING: reading the WORKING TREE — could not resolve origin/main." >&2
  echo "  A stale checkout serves stale policy with a correct-looking stamp." >&2
fi
if [ "$BEHIND" != "0" ] && [ "$BEHIND" != "?" ]; then
  echo "  NOTE: this checkout is behind origin/main. Any mirror-vs-served drift you" >&2
  echo "  compute from the WORKTREE is this checkout's distance, NOT fleet state." >&2
fi

# list (mirror set — say so in the output; the count is derived, never quoted).
# Listed through the SAME source the bodies come from: a listing taken from a
# stale worktree while bodies come from origin/main would disagree with itself.
mirror_list() {
  if [ "$MIRROR_SRC" = "origin/main" ]; then
    git -C "$DN" ls-tree --name-only "origin/main:prompts/policy-bodies-phase0" 2>/dev/null | grep '\.md$'
  else
    ls "$MIRRORS"/*.md 2>/dev/null | xargs -r -n1 basename
  fi
}
mirror_list
printf 'mirror set (%s): %s files (counted now, NOT the live inventory)\n' \
  "$MIRROR_SRC" "$(mirror_list | wc -l)"

# get one — provenance first, and no body without a readable stamp.
# Materialise through mirror_body so the body comes from origin/main when that
# resolved, and from the worktree only as the disclosed fallback. Every grep
# below then works unchanged against a single file.
MIRROR_NAME="<name>"
f=$(mktemp) || { echo "mktemp failed (LOCAL fault)" >&2; exit 1; }
# ONE trap covering every staged file, for the same reason Steps 2 and 3 spell
# it out: a later `trap ... EXIT` REPLACES the earlier one, so a bare
# `trap 'rm -f "$f"' EXIT` here would silently drop their cleanup and leave a
# live proxy nonce and device JWT in $TMPDIR. `rm -f` on an unset var is
# harmless when this rung runs in a fresh shell.
trap 'rm -f "$HDR" "$AUTH" "$f"' EXIT
mirror_body > "$f" 2>/dev/null || true
if [ ! -s "$f" ]; then
  echo "policy/<name>: NO MIRROR in $MIRROR_SRC — read unavailable (not substituted)" >&2
else
  # `|| true` on every grep: a no-match exits non-zero, and under `set -e` that
  # would abort the snippet BEFORE the withhold branch below prints — failing
  # silent on the one branch whose entire job is to speak up.
  stamp=$(grep -m1 -oE 'Mirrors served version [0-9]+' "$f" || true)
  if [ -z "$stamp" ]; then
    # Unstamped => unattributed. Report and print NOTHING.
    echo "policy/<name>: mirror has no version stamp — UNAVAILABLE, body withheld" >&2
  else
    # Age comes from the explicit `rendered_at:` key the renderer emits — NOT
    # "the first date in the file", which matches the SERVED document's
    # `updated_at` and would report the policy's age as the mirror's. Those two
    # diverge in exactly the cases that matter: an old policy mirrored
    # yesterday, or a policy edited this morning against a month-old mirror.
    rendered=$(grep -m1 -oE 'rendered_at: *[0-9]{4}-[0-9]{2}-[0-9]{2}[T0-9:Z]*' "$f" \
               | sed -E 's/rendered_at: *//' || true)
    src="rendered_at"
    if [ -z "$rendered" ]; then
      # Pre-renderer mirror. mtime is when the file was written HERE (checkout
      # time), which is biased FRESH — always newer than the true render. Label
      # it, and never let it read as a render date.
      rendered=$(date -u -r "$f" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || true)
      src="file mtime — NO rendered_at key; biased fresh, treat as an upper bound"
      [ -n "$rendered" ] || { rendered="UNKNOWN"; src="no rendered_at key and mtime unreadable"; }
    fi

    # Age in days — REQUIRED in the output, so compute it rather than leaving
    # the one value needing arithmetic to be eyeballed.
    age="unknown"
    if [ "$rendered" != "UNKNOWN" ]; then
      then_s=$(date -u -d "$rendered" +%s 2>/dev/null || true)
      now_s=$(date -u +%s)
      [ -n "$then_s" ] && age=$(( (now_s - then_s) / 86400 ))
    fi

    echo "MIRROR READ (rung 5) — $f"
    echo "  claims: $stamp"
    echo "  rendered: $rendered  (${src})"
    echo "  age: ${age} days"

    # UNCONDITIONAL, and deliberately not a threshold. Two separate defects put
    # it here, both measured on the pre-refresh tree (dev-notes 288dd35) on
    # 2026-08-30:
    #
    # 1. Silence read as agreement. The line was gated on `age -gt 7`, so a
    #    younger mirror printed NOTHING next to its version stamp - and nothing
    #    is indistinguishable from "checked, fine". That is
    #    `unknown-must-not-render-as-a-default` (verification-and-evidence) on
    #    the one path whose whole job is being honest about not knowing.
    #
    # 2. Age is anti-correlated with the risk it was standing in for. The
    #    rendered_at key moves only when the body is REWRITTEN; a mirror
    #    confirmed already-at-version is not re-rendered and keeps its old
    #    stamp. So age measures time-since-this-policy-last-CHANGED, not
    #    time-since-this-mirror-was-CHECKED - and for spotting risk those are
    #    close to opposites. Measured: of the 5 mirrors old enough to warn, 3
    #    were CURRENT (implementation-priorities, security-and-autonomy,
    #    session-protocol; 23-24d each), while coordination - the fastest-moving
    #    document in the set, v14, the one most likely to go stale next - sat
    #    silent at 3d. The loud ones were the safe ones.
    #
    # So the reader is told every time, and age only shades the wording.
    echo "  DRIFT: UNKNOWN - this rung cannot compare, and age does not stand in for it."
    echo "         The rendered_at key moves when the body is rewritten, not when"
    echo "         the mirror is checked: a low age is not evidence of freshness."
    if [ "$age" = "unknown" ]; then
      echo "         This mirror's age is unreadable too, so even that weak signal is gone."
    elif [ "$age" -lt 0 ]; then
      # A stamp in the FUTURE is clock skew or a hand-edited header. Either way
      # the provenance is not trustworthy, and letting it fall through as a
      # small number would dress the worst case as the best one.
      echo "         This mirror's stamp is in the FUTURE (${age}d) - skew or a hand"
      echo "         edit; treat its provenance as unreliable, not as fresh."
    elif [ "$age" -gt 7 ]; then
      echo "         This one is ${age}d old, which may only mean the document is"
      echo "         stable and was never re-rendered - old is not the same as behind."
    fi
    echo "  cannot verify against served: no coord transport reachable."
    echo "  to actually compare, regain a coord transport and run: /policy mirrors"
    cat "$f"
  fi
fi
```

To refresh the mirrors once a coord transport IS reachable again, run the
renderer this rung is fed by:

```bash
powershell -NoProfile -ExecutionPolicy Bypass \
  -File "$ROOT/qontinui-claude-config/scripts/render-policy-mirrors.ps1"
```

The `rendered_at:` key is part of the mirror provenance contract — the renderer
emits it, this rung reads it, and nothing else in the header is treated as a
date. A mirror predating the renderer has no such key and falls back to mtime,
**labelled as a fallback**, because a checkout's mtime is when the file was
written *here*, not when it was rendered from coord.

#### `/policy mirrors` — the drift diagnostic (coord reachable)

The mirror-vs-served comparison lives here, **not** on the serve path above,
because it needs a coord transport that rung 5 by definition does not have.
`/policy mirrors` is a diagnostic: it answers *"should these files be
re-rendered?"*, and it is what a session or a CI job runs to decide. It is never
part of a policy read.

Resolve the served inventory over rungs 1–4 (whichever works), read every
mirror's stamp, and print one row per document:

```
document                      mirror   served   drift
<name>                             N        N   —
<name>                             N      N+2   2 BEHIND
<name>                             N      N+1   1 BEHIND
...
<S> served / <M> mirror files — <b> behind, <m> missing, <u> unstamped, <a> ahead
```

*(Placeholders, deliberately. An example with real counts in it is a hard-coded
count — structurally the same defect as the line this section deleted, and a
reader will copy it. Every number in that summary is derived at run time.)*

**This is the same comparison `scripts/render-policy-mirrors.ps1 -CheckOnly`
performs**, and that is the batch/CI form: it prints this table and exits `0`
clean / `2` drift found / `3` could-not-compare. Drop the `-CheckOnly` to
actually re-render. Prefer the script when one is available — it is the same
logic, already written.

Rules for this path:

- A document present in the served inventory with **no mirror file** is
  `MISSING`, not skipped.
- A mirror file with **no stamp** is `UNSTAMPED` — counted separately from
  `BEHIND`, since it cannot even be compared.
- A mirror **ahead** of served is `AHEAD` and is a defect (a hand edit that
  never reached coord), not a rounding error — report it loudly.
- If no coord transport is reachable, `/policy mirrors` reports **"drift
  unknown — no coord transport"** and exits. It never falls back to comparing
  mirrors against each other, and never reports "no drift" from a failed read.
- **Compare `origin/main`'s mirrors, and say so.** Read each stamp from
  `git show origin/main:prompts/policy-bodies-phase0/<name>.md`, not from the
  working tree, and print the checkout's `HEAD..origin/main` distance in the
  summary line. A shared checkout parked on a feature branch makes every row
  read `BEHIND` — that is the checkout's distance, not the fleet's, and
  reporting it as drift sends someone to re-render mirrors that are already
  correct. Measured 2026-08-31: a worktree read gave 13-of-14 BEHIND while
  `origin/main` gave 13-of-14 CURRENT, on the same box, minutes apart. If the
  two sources disagree, the answer is "this checkout is stale", not "the
  mirrors are stale".
- **Probe before you re-derive the drift itself.** A mirror-drift verdict — *"N
  of the policy mirrors are behind"*, some specific fraction of the set — is one
  of the most-repeated wrong answers on this fleet: three separate sessions
  asserted one, and every one of them had measured a stale shared **checkout**
  rather than the mirrors, the same defect the `origin/main` bullet above exists
  to prevent. (Do not copy a fraction out of this bullet either; the no-literal-
  count rule above governs here too.) Before reporting drift, call
  **`coord_recent_findings`** with `topic: "policy-mirrors"`, or with
  `resource_keys` naming the mirror paths
  (`qontinui-dev-notes/prompts/policy-bodies-phase0/<name>.md`). Findings are
  pull-by-relevance — nothing pushes one at you, so a session that never asks is
  told nothing, and a peer's correction from yesterday is invisible while you
  re-derive it. The HTTP twin, for a masked tool or a dead transport, is `GET
  $COORD_HTTP_URL/coord/agent-findings?topic=…&resource_keys=…`; the two filters
  are **OR'd, not AND'd**, so passing both WIDENS the read rather than narrowing
  it. Read `available` **before** `count` — `available: false` is UNKNOWN, not
  "nobody has filed anything" [policy: `verification-and-evidence`
  `silent-empty-is-unknown`]. If a returned finding already covers the drift,
  cite it and stop; if your measurement *corrects* it, that is a finding worth
  posting with `supersedes` set, not a paragraph in a transcript nobody rereads.

### Honest failure (never a silent no-op)

If all rungs fail (mirrors absent too — e.g. no `qontinui-dev-notes`
checkout), **do not pretend**. Report exactly which link failed at each rung:
native tools not visible; per-candidate `.mcp.json` probe results (file → HTTP
code, or "no `.mcp.json` readable anywhere"); the remote MCP door's status
(`POST $COORD_HTTP_URL/mcp` → HTTP code, and whether a DEVICE JWT was
resolvable at all — never an `/agents/allocate` one, on any rung); the HTTP
door's status + whether a tenant-resolvable JWT could be minted; **Step 4b's
bootstrap mint** (`POST $COORD_HTTP_URL/agents/credential` → HTTP code —
measured `200` against production coord on 2026-09-04 and again 2026-09-10, and
present in coord's source since commit `5dd99cc3`, so a non-2xx here is a
REGRESSION worth naming, not a known-absent route; and whatever the code, the
substitute `/agents/allocate` stays prohibited — the gate once cited for it,
coord gate `ece99898-30c6-4f8c-be8e-1de5f09abebc`, reads `withdrawn` as of
2026-09-06 and its successor is anchored to a different arm, so the question is
UNKNOWN, not cleared; **re-verify with `coord_gate_inspect`** rather than
trusting this line); the mirror path checked. Then point at
**`coord doctor`** (runner self-check) for the credential-chain diagnosis.
**Report Step 4b's status rather than omitting it** — a reader who is handed a
mirror is owed the reason there was no live door, and "the credential route
answered <code> when it answered 200 on 2026-09-04" is a different
fact from "coord is down".

**And report the WEB-HOST axis on its own line — it is not one of the rungs
above, and every live one of those is the same host.** Rungs 1–4b and the
same-host falsification probe are `coord.qontinui.io` plus loopback (4c and 5
are local files), so an exhausted cascade is a statement about one program. Add:
`GET https://api.qontinui.io/api/v1/plan-library?kind=plan&limit=1` → HTTP code,
or **NOT ATTEMPTED**. Measured 2026-09-06: `401` anonymous, `200` with a
`user_id`-bearing device JWT — **either is a PASS**, and a pass means the fleet's
serving plane answers, so the honest report is a credential or route failure on
the coord host, never "policy unavailable". An axis you did not probe is
`not attempted`, never `unavailable`; that is the same distinction this door
already draws for a document with no mirror, applied to a host instead of a file.

If a `.coord-mcp-status` breadcrumb sits in your cwd, quote its reason **and its
age** in that report: it is the RUNNER's own record that this workdir's coord-mcp
provisioning was degraded, and six of its thirteen reasons mean that pass wrote
no `.mcp.json` — which names the cause of an exhausted cascade rather than
restating its symptom (a stale config, a foreign one or an unparseable one can
still be sitting there, so rung 2 probing one is not a contradiction). The
other seven are the probe's typed verdicts (`TIMEOUT`, `CONNECT_REFUSED`,
`UNAUTHORIZED (401)`, `CREDENTIAL_REFRESHING (503)`, `HTTP <observed>`,
`HTTP_200_NOT_MCP`, `TRANSPORT`) and mean the opposite: a config WAS written
and gave no usable answer at spawn; only `TIMEOUT` cannot tell a dead port from a busy
one, and says so (`NOT known dead`).

**Age it before you quote it.** Line 2 is a JSON stamp carrying `written_at`,
`workdir`, `port`, `verdict`, `build_id` and `schema`; older runner builds write
line 1 alone. **A stamp older than 30 minutes, an unreadable one, or no line 2
at all is UNKNOWN — never a fault, never health.** Report it as an explanation
of what you observed, not as a conclusion about coord now, and if line 2's
`workdir` is not your cwd, say that you are quoting another directory's
evidence. Its **absence** is UNKNOWN too, not health: a healthy provision writes
nothing either, and the runner writes into the workdir IT provisioned, which
from a linked worktree is often the primary checkout. Reason table, the stamp
and the freshness rule:
`qontinui-claude-config/knowledge-base/qontinui-specific/coord-gates-and-access.md`.

---

## Honesty rules (non-negotiable)

- **Never report a read that did not return a body.** A silent "no such
  tool", a 4xx, or an empty response must never read as a successful read.
- **Always name the transport used** (native MCP / proxy `<url>` via
  `<candidate file>` / remote MCP `$COORD_HTTP_URL/mcp` / HTTP agent door /
  **HTTP agent door (bootstrap credential)** / **steering cache (rendered
  <stamp>)** / file mirror **with the staleness
  disclosure**) alongside the result, so the reader can weigh freshness. The
  bootstrap spelling is distinct on purpose: it is the rung of last resort and a
  reader weighing a policy answer is entitled to know one carried it.
- **A masked/unknown native tool is not the end** — fall through
  Step 1 → 2 → 3 → 4 → 4b → (4c, the six intent kinds only) → 5.
- **Steering-cache reads always disclose** that they are a cache, quote its
  Rendered stamp, and report a document the cache lacks as UNKNOWN.
- **Mirror reads always disclose** that they are mirrors, that mirrors can lag
  the live store, and any requested document that has no mirror.
- **Never report policy — a document, a kind, or the surface — as "unavailable"
  while the WEB-HOST axis is unprobed.** Every rung of this cascade is
  `coord.qontinui.io` plus loopback; `api.qontinui.io` is a separate program on a
  separate host and answers `401` anonymous / `200` with a `user_id`-bearing
  device JWT (measured 2026-09-06). Probe it, **name which surface answered**,
  and name the ones you skipped as `not attempted` — never as `unavailable`.

And the same rule one level up, for the claim this door is most likely to
manufacture: **a capability negative cites a CENSUS, never a probe.** "There is
no door for this kind", "agents cannot read policy here", "that route does not
exist" are searches, and a cascade of probes is a sample:

<!-- detector-reach-fence:start -->
> **A capability negative cites a CENSUS, never a probe.** Before recording
> "no door", "agents cannot", "this route does not exist" or any other claim
> that a capability is ABSENT, run `bash scripts/coord-route-census.sh
> <fragment>` (qontinui-claude-config; reads `origin/main` of BOTH
> `qontinui-coord` and `qontinui-web`, never a working tree and never a live
> host) and paste its trailer verbatim beside the claim:
> `census: fragment=<f> hosts_read=coord.qontinui.io,api.qontinui.io ref=<sha>,<sha> routes=<n> unextracted=<n> unmounted=<n> generated=<ISO time>`
> — the line that parses under `CENSUS_TRAILER_RE` in
> `scripts/detector_reach/__init__.py`. A 401, 404 or 405 on ONE spelling of
> ONE host is a sample, not a search: `/api/v1/memory` refuses on
> `coord.qontinui.io` and answers on `api.qontinui.io`. A claim without the
> trailer is **UNVERIFIED and is not recorded** — not as a finding, not as a
> memory, not as a plan premise. `routes=UNKNOWN` (exit 2) means the census
> could not read a source and settles nothing; `routes=0` with both refs
> resolved is the only honest negative, and `admits=unknown` on a listed row
> means unmeasured, never "operator-only".
<!-- detector-reach-fence:end -->

---

*(Same cascade pattern: `/gate` — gate registration/attest/withdraw, spec
`_gate-registration`. `CLAUDE.md`'s "Autonomous Operation" pointer block names
this door so every session can reach the policies; `/policy` is the executable
form. Server routes: coord `routes.rs` `agent_prompt_documents_list_authed` /
`agent_prompt_documents_one_authed` — deliberately two sub-routers; do not
"simplify" them into one.)*
