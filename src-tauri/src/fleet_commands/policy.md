---
description: One transport-agnostic read-only door to list or fetch coord prompt documents (the fleet policies) — runs the native MCP tool, an auto-discovered loopback proxy (JSON-RPC), or the device-authed HTTP agent routes, with the qontinui-dev-notes policy mirrors as a disclosed last resort (and `mirrors` to report their drift) — so you never touch ports, nonces, or proxies. Use it whenever coord_list_prompt_documents is not a visible tool.
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
native MCP tool, the loopback proxy (MCP JSON-RPC), the device-authed HTTP
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
  `/policy get policy escalation-bar`.
- `mirrors` — **diagnostic only.** Compare every rung-4 mirror's version stamp
  against the served `current_version` and print the drift table (Step 4).
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
  Step-4 mirrors is **not** caching — this skill writes nothing, and every
  mirror it serves carries its own version stamp plus an explicit statement
  that the stamp could not be verified.
  > *Corrected 2026-08-06.* This bullet used to say the mirrors "are maintained
  > elsewhere." **There is no elsewhere** — nothing maintains them, and 6 of 14
  > were behind the served store six days after a full hand regeneration. The
  > disclosure, not a maintainer, is what makes rung 4 safe; that is why Step 4
  > withholds the body of any mirror that cannot state its own version.

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
# MSYS pathconv is off.) Same rule for the device-JWT header in Step 3 below.
HDR=$(mktemp) || { echo "mktemp failed — cannot stage the nonce off argv" >&2; exit 1; }
AUTH=""   # Step 3 stages the device-JWT header here; ONE trap must cover both,
          # or a later `trap … EXIT` silently replaces this one and leaves a
          # live nonce in $TMPDIR after exit.
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
  # The FILE PATH on argv is fine — it is not key material. The KEY still never
  # reaches any argv: it is returned on stdout into a shell variable, exactly as
  # the jq arm does. Same no-positionals rule as the jq arm: `$MCP_CFG`, never `$N`.
  mcp_url() { python -c "import json,sys;print(json.load(open(sys.argv[1],encoding='utf-8')).get('mcpServers',{}).get('coord-mcp',{}).get('url',''))" "$MCP_CFG" 2>/dev/null; }
  mcp_key() { python -c "import json,sys;h=json.load(open(sys.argv[1],encoding='utf-8')).get('mcpServers',{}).get('coord-mcp',{}).get('headers',{});print(h.get('Authorization') or h.get('X-Coord-Mcp-Proxy-Key','') or '')" "$MCP_CFG" 2>/dev/null; }
  mcp_keyhdr() { python -c "import json,sys;h=json.load(open(sys.argv[1],encoding='utf-8')).get('mcpServers',{}).get('coord-mcp',{}).get('headers',{});print('Authorization' if h.get('Authorization') else 'X-Coord-Mcp-Proxy-Key')" "$MCP_CFG" 2>/dev/null; }
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

### Step 3 — Direct device-authed HTTP (probe: the GET itself)

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
# cmdline to read). $AUTH + the EXIT trap come from the Step-2 block when you
# carried that shell forward; in a fresh shell:
#   AUTH=$(mktemp); trap 'rm -f "$AUTH"' EXIT
# The guarded trap MUST also cover $HDR: when this shell was carried forward
# from Step 2, a trap naming only $AUTH would REPLACE Step 2's combined trap
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
the inventory grows, and a quoted count is the same stale-source defect Step 4
now refuses to commit.) Tenant always derives server-side from the JWT — never
pass a tenant argument.

### Step 4 — LAST RESORT: the file mirrors (disclosed, never silent)

Only when rungs 1–3 **all** fail (no coord transport reachable at all), read
the mirrors at
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
rung-4 response**, not once at the top:

- Say the mirrors were used, and name the **exact file** read.
- Print the mirror's `Mirrors served version N` stamp and its render date, then
  state plainly: **"cannot verify against served — no coord transport is
  reachable, which is why you are reading a mirror."** Never phrase a mirror as
  current, and never let the absence of a comparison read as agreement.
- Print the stamp's **age in days**. An age past ~7 days is called out as
  likely stale on the measured drift rate above.
- **A mirror whose stamp is missing or unparseable is reported UNAVAILABLE, and
  its body is NOT printed.** A file that cannot say which version it reflects is
  not a policy answer — it is an unattributed string. This is the same
  absence-is-not-zero reading as `verification-and-evidence`
  `silent-empty-is-unknown`.
- `get` for a document that has **no mirror** → say so by name, and report the
  read as unavailable. **Never silently substitute** a different document or
  an older body presented as current.
- The mirror set covers **kind `policy` exclusively** — mirrors are keyed by
  name only, so a rung-4 `get` for any OTHER kind (`agent_playbook`,
  `continuation_rules`, `prompt_template`, `response_prompt`) is reported
  unavailable by kind+name, never served from a same-named policy mirror.
- `list` from mirrors = the filenames present **counted at read time**, labelled
  as the mirror set, not the live inventory.

```bash
MIRRORS="$ROOT/qontinui-dev-notes/prompts/policy-bodies-phase0"

# The directory ABSENT is a rung-4 failure, not "a mirror set of size zero".
# Without this guard the count below prints 0 and reads as "there are no
# mirrors" — absence reported as emptiness, the exact thing this rung is
# supposed to be honest about.
if [ ! -d "$MIRRORS" ]; then
  echo "rung 4 UNAVAILABLE: no mirror directory at $MIRRORS (no qontinui-dev-notes checkout?)" >&2
  echo "this is a LOCAL fault — report it as such, never as 'no policy found'" >&2
  exit 1
fi

# list (mirror set — say so in the output; the count is derived, never quoted):
ls "$MIRRORS"/*.md
printf 'mirror set: %s files (counted now, NOT the live inventory)\n' \
  "$(ls "$MIRRORS"/*.md 2>/dev/null | wc -l)"

# get one — provenance first, and no body without a readable stamp.
f="$MIRRORS/<name>.md"
if [ ! -r "$f" ]; then
  echo "policy/<name>: NO MIRROR — read unavailable (not substituted)" >&2
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

    echo "MIRROR READ (rung 4) — $f"
    echo "  claims: $stamp"
    echo "  rendered: $rendered  (${src})"
    echo "  age: ${age} days"
    [ "$age" != "unknown" ] && [ "$age" -gt 7 ] && \
      echo "  WARNING: older than 7 days — 6 of 14 mirrors drifted within a week when last measured; treat as likely stale"
    echo "  cannot verify against served: no coord transport reachable."
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
because it needs a coord transport that rung 4 by definition does not have.
`/policy mirrors` is a diagnostic: it answers *"should these files be
re-rendered?"*, and it is what a session or a CI job runs to decide. It is never
part of a policy read.

Resolve the served inventory over rungs 1–3 (whichever works), read every
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

### Honest failure (never a silent no-op)

If all four rungs fail (mirrors absent too — e.g. no `qontinui-dev-notes`
checkout), **do not pretend**. Report exactly which link failed at each rung:
native tools not visible; per-candidate `.mcp.json` probe results (file → HTTP
code, or "no `.mcp.json` readable anywhere"); the HTTP door's status + whether
a tenant-resolvable JWT could be minted; the mirror path checked. Then point
at **`coord doctor`** (runner self-check) for the credential-chain diagnosis.

If a `.coord-mcp-status` breadcrumb sits in your cwd, quote its reason in that
report: it is the RUNNER's own record that this workdir's coord-mcp provisioning
was degraded, and four of its five reasons mean that pass wrote no `.mcp.json`
— which names the cause of an exhausted cascade rather than restating its
symptom (an earlier stale config can still be sitting there, so rung 2 probing
one is not a contradiction). Its **absence** is UNKNOWN, not health: a healthy
provision writes nothing either, and the runner writes into the workdir IT
provisioned, which from a linked worktree is often the primary checkout. Reason
table:
`qontinui-claude-config/knowledge-base/qontinui-specific/coord-gates-and-access.md`.

---

## Honesty rules (non-negotiable)

- **Never report a read that did not return a body.** A silent "no such
  tool", a 4xx, or an empty response must never read as a successful read.
- **Always name the transport used** (native MCP / proxy `<url>` via
  `<candidate file>` / HTTP agent door / file mirror **with the staleness
  disclosure**) alongside the result, so the reader can weigh freshness.
- **A masked/unknown native tool is not the end** — fall through
  Step 1 → 2 → 3 → 4.
- **Mirror reads always disclose** that they are mirrors, that mirrors can lag
  the live store, and any requested document that has no mirror.

---

*(Same cascade pattern: `/gate` — gate registration/attest/withdraw, spec
`_gate-registration`. `CLAUDE.md`'s "Autonomous Operation" pointer block names
this door so every session can reach the policies; `/policy` is the executable
form. Server routes: coord `routes.rs` `agent_prompt_documents_list_authed` /
`agent_prompt_documents_one_authed` — deliberately two sub-routers; do not
"simplify" them into one.)*
