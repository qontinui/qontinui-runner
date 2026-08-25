---
description: One transport-agnostic door to register, attest, or withdraw a coord gate — runs the native MCP tool, an auto-discovered loopback proxy (JSON-RPC), or REST writes through the runner's proxy-nonce write forwarder, plus two residual file-independent credentials (a device JWT minted from the local runner's UI Bridge, then an acting-bearer mint from $COORD_AGENT_JWT) — so you never touch ports, nonces, or proxies. Use it whenever coord_register_gate is not a visible tool.
argument-hint: "register|attest|withdraw [args]"
allowed-tools: Read, Bash, Glob, Grep, ToolSearch
---

# Gate — set, attest, or withdraw a coord gate over whatever transport works

`/gate` is the **single executable front door** for coord gate registration,
attestation, and withdrawal. You describe the gate; `/gate` figures out *how*
to reach coord —
native MCP tool, the loopback proxy (MCP JSON-RPC), or REST through the runner's
proxy-nonce write forwarder — and reports which transport it used plus the
returned `gate_id`. You never have to know about ports, nonces, or which JWT the
session holds.

**Be honest about the cascade's shape:** Steps 2 and 3 BOTH key off the **proxy
nonce** (`X-Coord-Mcp-Proxy-Key`, or `Authorization: Bearer <nonce>` on configs
written after the Phase 2 header move — accept BOTH) from the same `.mcp.json` file family — Step 2
speaks MCP JSON-RPC through it, Step 3 speaks REST through the runner's write
forwarder with it. **Two** credential sources are independent of that file
family, and Step 3's residual covers both: an explicitly exported
`$COORD_AGENT_JWT`, and — when that is unset — a **device JWT minted on demand
from the local runner's UI Bridge** (`:9876`), which holds no secret at rest.
The cascade therefore buys protocol redundancy AND one genuinely independent
credential path; what it does not buy is a third *transport* to coord.

⚠️ **A stale `.mcp.json` family does not exhaust the cascade.** Measured
2026-08-19: all **14** candidate `.mcp.json` files answered `401` (the runner had
rotated its nonce), `$COORD_AGENT_JWT` and `$COORD_DEVICE_JWT` were both unset,
and `~/.qontinui/coord-device-jwt` was absent — yet the runner on `:9876` was
healthy and minted a working device JWT on the first try, which carried a
work-unit upsert and a gate registration straight through to
`https://coord.qontinui.io`. An earlier revision of this file called
`$COORD_AGENT_JWT` "the **only** credential source independent of the file
family", which would have sent that session to Step 4's honest-failure report
with a credential sitting one call away. Absence of a nonce is not absence of a
credential.

This skill is the **executable implementation** of the canonical spec
`_gate-registration` (predicate kinds, anchor derivation, `clearance_audience`,
masked-tool honesty). `_gate-registration` stays the spec of record; this is the
door. Keep the two in sync. `/blocked` is the **session-close** procedure (it
calls this same registration); `/gate-sweep` *reports* open/closed gates — both
distinct from `/gate`. Do not merge them.

> **If `coord_register_gate` isn't a visible tool in this session, that is not a
> dead end — run `/gate`.** A masked or absent MCP tool is exactly what the
> cascade below is for.

> **Different failure, different door: if a coord MCP tool was VISIBLE and its
> call returned `"Command failed with no output"`, run `/coord-revive` first.**
> That string is the client's undifferentiated mask for a dead cached transport
> (evicted proxy key, dead port, credential refresh, timeout) — the tools stay
> listed, so it does not read as masked and the cascade above never triggers.
> **Treat such a write as LOST, not slow:** 8 of 8 prod-adjudicable "no output"
> writes were adjudicated lost on 2026-07-26, four of them `coord_register_gate`
> — i.e. this failure lands squarely on the gates `/gate` exists to set.
> `/coord-revive` replaces the mask with a typed verdict and names the door that
> is live right now; re-issue over that door, then **verify by read**. A retry's
> success is never evidence the original landed.

## Arguments — `$ARGUMENTS`

- `register [condition…]` — register a gate (default if a sub-verb is omitted).
- `attest [gate_id|condition…]` — attest an OPEN agent-audience gate you have just
  satisfied.
- `withdraw <gate_id> "<reason>"` — cancel a gate **you registered** that is
  erroneous/superseded (see "Withdraw" below — **LIVE**: coord PR #1247 landed,
  so an erroneous gate no longer has to wait for operator dismissal).

If you arrive here without explicit args, infer the action from context: you are
**deferring/blocking** work on an observable condition → `register`; you just
**completed** the work a gate was watching → `attest`; you registered a gate
this session that turned out **wrong/superseded** → `withdraw`.

---

## Part A — build the typed gate (transport-independent)

Do this first, regardless of which transport ends up carrying it.

### A1. Pick the predicate kind (the forcing function)

`coord_register_gate` deserializes input into a **typed `GatePredicate`** and
**rejects kind-less prose**. Map what you wait on to exactly one kind:

| What you are waiting on | Predicate kind | Shape / notes |
|---|---|---|
| A PR merging | `pr_merged` | `{repo, pr_number}` — works on coord-orchestrated repos too: since the land-aware `pr_merged_verdict` shipped it clears from coord's OWN ff-land provenance (`close_cause`), not a GitHub merge event, so the clear can lag GitHub's close slightly. Registration emits an informational steer, not a rejection. (The older "never fires on a coord-orchestrated repo" advice is STALE — corrected 2026-08-03 against `gates.rs` `pr_merged_verdict`.) |
| Work landing on main of a **coord-orchestrated repo** | `commit_live` | `{repo, commit_sha, on_ref?}` — ancestor-of-main check; anchor a **post-land main SHA** (or use `unit_status` — **not `file_exists`, which is broken**), NEVER the pre-land branch-head SHA — rebase-land rewrites SHAs and the gate rots open |
| A deploy going healthy | `deploy_healthy` | `{service, expected_rev}` — BOTH required; clears only when the service is healthy AND the deployed rev includes `expected_rev` (fail-closed if the deployed rev is unknown) |
| A claim going terminal | `claim_terminal` | claim-anchored (`claim_kind`+`resource_key`) |
| A human decision / judgment | `operator_approval` | `{prompt}` — notify-only; the human escape hatch |
| CI going green | `ci_green` | `{repo, head_sha}` — a FIXED head SHA, not a branch name: the evaluator matches `coord.pr_check_runs_latest` rows BY head SHA, so a branch name matches nothing and the gate stays open forever |
| A git ref/tag appearing | `ref_exists` | the ref (refs, **not** file contents) |
| A metric crossing a threshold | `metric_threshold` | `{metric, labels, op, value, window_secs?}` — name `labels` explicitly |
| A time window / burn-in elapsing | `time_elapsed` | `{since (default now), duration_secs}` |
| **A vetted plan that is ready, dispatchable work** | `unit_ready` | `{work_unit_id, ready_status}` — auto-clears when the unit reaches `ready_status` + sibling gates cleared; **NOT** `operator_approval` |
| A schema/alembic reaching head | `migration_at_head` | `{schema}` — live schema observer |
| Infra drift / active-negation clearing | `infra_drift_clear` | `{}` — live infra observer |
| A repo file / workflow / migration file existing | ⛔ `file_exists` — **KNOWN BROKEN 2026-08-05, do not register one** (403s fleet-wide on the contents API, control-probed; the gate can never clear). Use `commit_live` with a post-land SHA, or `unit_status`. | `{repo, path, on_ref?}` — file contents/presence |
| A coord data count crossing a bound | `sql_count` | `{query_id, op, n}` — whitelisted `query_id` only (`devices_null_tenant`\|`open_gates`\|`draft_plans`), never raw SQL |
| An umbrella plan (work unit) reaching a status | `unit_status` | `{work_unit_id, status}` — reads the work unit's `status` |
| Another, cross-anchor gate clearing | `gate_cleared` | `{gate_id}` — composition |
| Sensitive / security / billing / strategy | `operator_approval` + notify | see "Sensitive" below |

**`unit_ready` vs `operator_approval` — do not mismodel a work queue as a human
decision.** Ready, dispatchable, vetted work is `unit_ready`
(`{"kind":"unit_ready","work_unit_id":"<uuid from upsert>","ready_status":"<what landed>"}`).
`operator_approval` is for genuine human decisions only.
⚠️ Transition the unit FIRST, then set `ready_status` to the status that actually
landed. Do not hardcode an **Attested** value (`vetted`/`superseded`/`obsolete`) on
a unit you own: the upsert that created it made you its owner, an owner may not
attest, and the gate would pin open forever. `/vet-plan` §5.4 attempts `vetted` and
falls back to the Free status `vetted_unattested`. (Canonical: `_gate-registration`
→ "`unit_ready` vs `operator_approval`".) **No kind fits?** Either it is a real human
decision → `operator_approval{prompt}`, or it has **no observable trigger** → it
is *not a gate*; leave it in your report. Never register prose as a predicate.

### A2. Derive the anchor (zero user input)

Every gate needs exactly ONE anchor:

- **Plan-anchored (usual):** `(work_unit_id, phase_name)` — a plan tracked as a
  work unit.
  - `work_unit_id` — `POST $COORD_HTTP_URL/coord/work-units/upsert` `{ "slug":"<stem>",
    "title":"<plan H1>" }` (idempotent on slug = plan filename stem) → **capture
    `work_unit_id`** (a UUID) from the response, OR
    `GET $COORD_HTTP_URL/coord/agent-work-units/<slug>` to read an existing id.
    This UUID — not the slug — is what the `unit_ready`/`unit_status` predicates
    take.
  - This is a **separate FIRST call**: the `register-gate` route does NOT upsert
    (it 404s `work_unit_not_found` if the slug is absent).
- **Claim-anchored:** `(claim_kind, resource_key)` — only when the gate is bound
  to a specific coord claim, not a plan phase.

> **The work-unit WRITES are device-authed — no more upsert wall.** The work-unit
> upsert + register routes live on coord's `require_jwt` sub-router, so a **device
> JWT** (the proxy/HTTP-acting transports' identity) resolves `tenant_id`
> server-side and CAN upsert + register — the old "a device principal cannot upsert
> `coord.plans`" wall is gone. **The reads are on a different path:**
> `GET /coord/work-units/<slug>` is the operator dashboard's `TenantId`-tier route
> and answers a device JWT with **403 `tenant_not_resolved`** — read through the
> device-authed door `GET /coord/agent-work-units/<slug>` instead (it returns
> `{work_unit, recent_history, citations}`). If for some reason you still cannot mint/resolve a
> `work_unit_id` (no JWT reaches coord at all), do NOT block — anchor on a
> **`file_glob` claim** instead: `claim_kind: "file_glob"`,
> `resource_key: "<plan-or-file path>"`, and note in your report that it is
> claim-anchored on the path rather than plan-anchored.

`$COORD_HTTP_URL` defaults to `https://coord.qontinui.io`. Tenant **always**
derives server-side from the JWT — never pass a tenant argument.

### A3. `clearance_audience`

- **`agent`** — an agent-verifiable fact a later session can attest
  (`coord_attest_gate`). Set this explicitly for agent-fact gates.
- **`operator`** — needs business/judgment/strategy or on-page human verification,
  or anything **security / credential / billing / strategy-sensitive**. **Default
  when omitted.** Sensitive work ALWAYS `operator` + notify-only, never
  auto-resuming — even under auto mode. When in doubt, treat as sensitive.

### A4. `gate_class` (optional — LIVE, send it)

Optionally classify the gate: **`security-surface`** | **`routine-review`** |
**`ops-confirm`** (recommended vocabulary; freeform TEXT, no CHECK). coord
#1246/#1249 and web #872 all landed and the round trip is verified in production
(2026-08-03), so **pass it** through whichever transport carries the
registration — it rides the same argument/body as `clearance_audience` on every
step below (the `gate_class?` markers in the Step 2/3 body sketches are this
same field).

**When to classify:** `security-surface` when the deferred work this gate guards
would itself fire a `security-and-autonomy` glob or content trigger;
`ops-confirm` for deploy / sweep / migration / config confirmations;
`routine-review` for mechanical follow-ups judgeable on the diff alone. **Omit
when none applies** — omitting is safe and is never a loophole, and a guessed
class is worse than none.

Coord's per-tenant clearance-authority matrix (`policy_rules` v2 rows,
`decision_domain = 'gate_clearance'`) matches rules on the **exact class string**
and resolves who may attest/reject: `operator_only` | `agent_non_author` |
`agent_any` (waives self-approval protection for that class). ⚠️ **Do not ask
for `agent_non_author` on this fleet** — `registered_by_agent_id` is null on
every live gate (device JWTs carry `agent_id: None`), so the resolver falls to
its single-device floor and fails closed, meaning "nobody may attest". Use
`operator_only` for separation of duties until plan #1031 ships. **NULL — or an
unmatched class — falls in the default bucket, never more permissive than today;
unclassified is never a loophole. This tenant has zero configured
`gate_clearance` rules as of 2026-08-03, so behavior is byte-identical to today
until the operator authors one.** (Canonical: `_gate-registration` →
"`gate_class`".)

### A5. Continuation (auto-resume) — OFF by default

**Omit the continuation ENTIRELY unless the follow-up will outlive this session
— no `continuation` and no `continuation_prompt` (MCP `coord_register_gate`),
and no `continuation_spawn` (HTTP `register-gate`).** These are three spellings
of **one knob**: coord materializes both MCP fields into the DB's
`continuation_spawn` column and both spawn, so "omitting `continuation_spawn`"
while still passing `continuation_prompt` produces exactly the duplicate,
parallel run this default exists to prevent. The default is **omission**
(`continuation_spawn` NULL) — *not* the typed `{"action":"notify_only"}`, which
stores a payload and is a different DB state; reach for `notify_only` only when
you deliberately want an explicit typed no-op.

Under charter rule 10 ("Finish to zero") a session finishes its own follow-ups
in-session, so a redundant continuation queues a duplicate, parallel run of the
same work. Attach one only for: a wait longer than rule 10's ≲2h monitor window;
a session closing WITHOUT dispatching the follow-up (`/blocked`); an
`operator_approval` / human-decision gate (unbounded in time); or a cross-session
chain owned by another work unit or device. **Sensitive** gates (security /
credential / billing / strategy) stay notify-only unconditionally.
(Canonical: `_gate-registration` → "Continuation policy".)

**When you do attach one, all three spellings are that same knob — pick ONE.**
Prefer the typed `continuation` on MCP (e.g.
`{"action":"run_skill","skill":"implement-phase","args":["<stem>","Phase N"]}`);
**`continuation.args` MUST be a JSON ARRAY**, not a string. The legacy
`continuation_prompt` (e.g. `run /implement-phase <stem> "Phase N"`) still works
but hardcodes `"repos": []`, dropping the spawned terminal's cwd onto the shared
root uncoordinated. Over HTTP the field is `continuation_spawn`, where you can
populate `repos` yourself. Delivery is currently a live defect — continuations
are being dispatched but never consumed, and coord's 24h pending window drops
them permanently — so treat any spawn as best-effort and read
`continuation_consumed_outcome` rather than assuming a `consumed` continuation
actually ran (a **null** outcome means never claimed, which is worse than a
recorded `spawn_failed`).

---

## Part B — the transport cascade (try in order; stop at the first that works)

**Each step is validated by a cheap probe before you trust it.** Always report
which step carried the gate and the returned `gate_id`.

### Step 1 — Native MCP tool `coord_register_gate` (probe: tool present)

If `coord_register_gate` is in this session's tool set, call it with the anchor,
typed predicate, `clearance_audience`, optional `gate_class`, and optional
continuation. (Load it via `ToolSearch` if it is a deferred tool name.) Tenant
derives server-side.

- **Probe:** the tool exists / `tools/list` shows it. If the call returns
  **unknown / method-not-found**, the tool is masked → **do not stop**; fall to
  Step 2. A masked tool reading as "no such tool" is the trigger for the cascade,
  not a failure to report.

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
**stale/mis-ported** (dead port or evicted nonce → 401) while a **sibling repo's**
`.mcp.json` (e.g. `qontinui-coord/.mcp.json`) holds the **live** key/port. So
**probe every candidate and use the first whose `tools/list` returns HTTP 200.**

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
# MSYS pathconv is off.) Same rule for the acting bearer in the block below.
HDR=$(mktemp) || { echo "mktemp failed — cannot stage the nonce off argv" >&2; exit 1; }
AUTH=""   # Step 4 stages the acting bearer here; ONE trap must cover both, or
          # the later `trap … EXIT` silently replaces this one and leaves a live
          # nonce in $TMPDIR after exit.
trap 'rm -f "$HDR" "$AUTH"' EXIT
hdrp() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$HDR" || printf '%s' "$HDR"; }

# jq is NOT guaranteed to exist — it is ABSENT on the Windows operator box
# (verified 2026-08-06). With `jq ... 2>/dev/null` inline, a missing binary is
# indistinguishable from an empty field: url/key come back EMPTY for EVERY
# candidate, the `continue` below skips them all, and the sweep reports "no live
# proxy" while a door is live — the same false-exhausted-cascade this Step exists
# to prevent, from a different cause. Pick a reader up front and fail LOUD if
# neither exists, so a missing tool can never read as a coord verdict.
# NEVER use a shell positional parameter — a `$` followed by a single digit —
# anywhere in these fences. In a slash-command markdown body those are HARNESS
# ARGUMENT PLACEHOLDERS, not shell positionals: Claude Code substitutes the
# invocation's argument words into the body BEFORE injecting it, indexed from
# ZERO (the zeroth placeholder is the FIRST word), and leaves unfilled positions
# LITERAL. `/gate` is argument-taking (`/gate register …`), so the first-index
# placeholder these readers used became the SECOND word of the invocation; both
# opened a file by that name which does not exist; url AND key came back EMPTY
# for every candidate; and the cascade reported an exhausted door over a LIVE
# one — a silent-empty failure. Read the named `$MCP_CFG` set by the sweep loop
# below instead, and never reintroduce a positional. (This comment spells no
# `$`-digit of its own on purpose: it would be substituted too, garbling the
# warning.)
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
  # the same false-exhausted-cascade this Step exists to prevent. Same fix PR
  # #171 made in pr-status.sh's sweep.
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

If a live proxy is found, register/attest via raw JSON-RPC `tools/call` against it.
The proxy carries MCP JSON-RPC only, so use the **MCP tools** here — `coord_register_gate`
folds the work-unit upsert (or call `coord` work-unit upsert first and pass the
captured `work_unit_id`):

```bash
# $HDR/hdrp() still hold the winning nonce from the sweep above (same shell). In
# a fresh shell, re-stage it — never inline it on argv:
#   HDR=$(mktemp); trap 'rm -f "$HDR"' EXIT
#   printf '%s: %s\n' "$LIVE_HDR" "$LIVE_KEY" > "$HDR"   # $LIVE_HDR = the header name the sweep found the nonce under
#   hdrp() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$HDR" || printf '%s' "$HDR"; }
curl -fsS -X POST "$LIVE_URL" -H "Content-Type: application/json" \
  -H @"$(hdrp)" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name":"coord_register_gate",
        "arguments":{
          "work_unit_id":"<uuid>", "phase_name":"P3 web gate_class forward (trigger 5)",
          "predicate":{"kind":"commit_live","repo":"qontinui/qontinui-web",
                       "commit_sha":"<POST-LAND main sha>"},
          "clearance_audience":"agent",
          "gate_class":"security-surface"
        }}}'
# ^ commit_live anchors a POST-LAND main sha, so it is immune to rebase-ff SHA
#   rewrites. `pr_merged` would ALSO clear here (coord's ff-land provenance) —
#   commit_live is the sturdier choice, not the only working one.
#   security-surface because the guarded work changes who may clear a gate —
#   `security-and-autonomy` content trigger 5, named in phase_name above.
# ^ NO continuation: omitting is the default (A5). Add one ONLY when the
#   follow-up must outlive this session.
```

Read the `gate_id` out of the JSON-RPC `result`. (For attest, use
`"name":"coord_attest_gate","arguments":{"gate_id":"<id>"}`.) Remember the
claim-anchor fallback from A2 here: if a `work_unit_id` can't be minted at all,
anchor `file_glob` + `resource_key`.

### Step 3 — REST through the runner's write forwarder, same proxy nonce (probe: `tools/list` → HTTP 200)

Step 3 is Step 2's **REST twin**: the same proxy nonce, but plain HTTP routes on
the runner's write forwarder instead of MCP JSON-RPC — for when the JSON-RPC
`tools/call` surface misbehaves (masked tool, RPC framing errors) while the
runner's HTTP API is fine. The forwarder authenticates on the registered
`X-Coord-Mcp-Proxy-Key` nonce — or the same nonce under `Authorization: Bearer`
on configs written after the Phase 2 header move — and injects a fresh device JWT upstream per
request — no static bearer, no TTL worry.

**Finding the nonce:** hunt the first `.mcp.json` whose coord-mcp entry is
**proxy-shaped** (`url` + `headers["X-Coord-Mcp-Proxy-Key"]`, **or** `url` +
`headers.Authorization` carrying a raw nonce) and whose
`tools/list` probe returns HTTP 200 — reuse `$LIVE_URL`/`$LIVE_KEY` from Step 2
if that sweep already found one; otherwise re-run the Step-2 sweep code above.
A stale nonce **401s** — a runner restart rotates the key and can move the
port, so probe, never trust a file. If `tools/list` itself won't parse but the
port answers, try a forwarder POST directly — a non-401 means the nonce is
live even though the JSON-RPC surface is broken.
Then POST the forwarder routes (`$LIVE_URL` ends in `/coord-mcp`):

```bash
# $HDR/hdrp() carry the nonce off argv — staged by the Step-2 sweep above, in
# the SAME shell. Starting fresh? Re-stage per the comment in the block above.
# 1. plan-anchored: upsert the work unit (capture work_unit_id), then register:
WU=$(curl -fsS -X POST "$LIVE_URL/work-units/upsert" \
  -H @"$(hdrp)" -H "Content-Type: application/json" \
  -d '{"slug":"<stem>","title":"<plan H1>"}')   # work_unit_id is in $WU
curl -fsS -X POST "$LIVE_URL/work-units/<stem>/register-gate" \
  -H @"$(hdrp)" -H "Content-Type: application/json" \
  -d '{ "phase_name":"deploy confirmation",
        "predicate":{"kind":"deploy_healthy","service":"coord",
                     "expected_rev":"<sha the deploy must INCLUDE>"},
        "clearance_audience":"agent",
        "gate_class":"ops-confirm" }'
# 2. claim-anchored (no slug) — the NEW forwarder route:
curl -fsS -X POST "$LIVE_URL/gates/register" \
  -H @"$(hdrp)" -H "Content-Type: application/json" \
  -d '{ "claim_kind":"file_glob", "resource_key":"qontinui-coord/src/gates.rs",
        "predicate":{"kind":"ci_green","repo":"qontinui/qontinui-coord",
                     "head_sha":"<the sha CI must be green for>"},
        "clearance_audience":"agent",
        "gate_class":"security-surface" }'
# ^ security-surface: the guarded file IS the gate-clearance authority code.
#   ci_green takes a FIXED head_sha — never a branch name.
# attest (unchanged — keyed by gate_id):
curl -fsS -X POST "$LIVE_URL/gates/<gate_id>/attest" \
  -H @"$(hdrp)"
```

A successful register returns **`201` with `{ "gate_id": "<uuid>" }`**.
`register-gate` does NOT upsert (404s `work_unit_not_found` if you skip the
upsert). The claim-anchored `POST {runner}/coord-mcp/gates/register` (forwarding
to coord's device-authed `POST /coord/gates/register-agent`) requires the
Phase-1a/1b PRs of `2026-07-21-gate-cascade-step3-proxy-rebase` to be deployed
in coord + the running runner — a 404 from either hop means they aren't yet;
until then a claim-anchored REST registration falls back to the acting-bearer
path below (or Step 2's JSON-RPC `coord_register_gate`, which takes claim
anchors today).

**Residual last resort — two file-independent credentials, in order.** When no
candidate `.mcp.json` yields a live proxy, nothing writes a bearer into any
`.mcp.json` anymore (every config is proxy-shaped; the old
`headers.Authorization` hunt is deleted), so the remaining credentials are:

**(a) A device JWT minted from the local runner's UI Bridge — try this FIRST; it
needs no prior export.** This is the same mint `render-memory-cache.ps1` uses; the
runner holds no secret at rest and the token never touches disk or argv. It
authenticates as a **device principal**, which is exactly the tier the work-unit
upsert + `register-gate` routes want:

```powershell
$evalBody = @{
  expression    = 'window.__TAURI__ ? window.__TAURI__.core.invoke("get_access_token_for_websocket") : invoke("get_access_token_for_websocket")'
  await_promise = $true
} | ConvertTo-Json -Compress
$r = Invoke-RestMethod -Uri 'http://127.0.0.1:9876/ui-bridge/control/page/evaluate' `
     -Method Post -ContentType 'application/json' -Body $evalBody -TimeoutSec 60
# `data.value`, NOT `data.result.value`. The runner unwraps the frontend's
# `result` envelope before it reaches HTTP (qontinui-runner
# `ui_bridge/page.rs` -> `Ok(resp.result.unwrap_or(...))`), so the live answer is
# {"success":true,"data":{"value":"<jwt>","type":"scalar"}} with no `result` key.
# The old path read $null off a HEALTHY runner and then threw 'signed out?' —
# telling the operator to sign in a runner that was already holding a valid
# token. Fallback kept so this resolves against either envelope.
$jwt = [string]$r.data.value
if (-not $jwt -and $r.data.result) { $jwt = [string]$r.data.result.value }
$jwt = $jwt.Trim()
# Shape-check before trusting it: a SIGNED-OUT runner answers 200 with an empty
# or non-token value, and sending that as a bearer turns a missing credential
# into a 401 the caller then has to decode. Reaching this with an EMPTY $jwt now
# means the runner really did answer without a token, not that the read missed.
if ($jwt.Split('.').Count -ne 3) { throw 'runner returned a non-JWT (signed out?)' }
Invoke-RestMethod -Uri 'https://coord.qontinui.io/coord/work-units/upsert' -Method Post `
  -Headers @{Authorization="Bearer $jwt"} -ContentType 'application/json' `
  -Body '{"slug":"<stem>","title":"<plan H1>"}'
# then POST .../coord/work-units/<stem>/register-gate with the same header.
```

⚠️ **Use `127.0.0.1`, never `localhost`** — Windows resolves `localhost` to `::1`
first and the runner binds IPv4 only, so you pay a doomed IPv6 connect first.
⚠️ **An operator Cognito bearer does NOT work here** — the work-unit/gate routes
sit on coord's device-authed `require_jwt` router and answer a Cognito IdToken
with `401 {"error":"invalid token"}`. That token is for the tenant-scoped
operator routes (`/pr-merge/health`), not these. Do not read that 401 as "coord
rejected my gate".

**(b) An explicitly exported `$COORD_AGENT_JWT`** — mint an acting-user Service
bearer and POST coord's routes directly:

```bash
COORD_HTTP_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
# Workspace root as in Step 2 — `--git-common-dir`, NOT `--show-toplevel`, so a
# linked worktree resolves to the real root instead of its container. Getting
# this wrong here doesn't just mis-sweep: $ROOT locates the helper script below,
# so a worktree session silently reports "no acting bearer" for a missing PATH
# rather than a missing credential.
ROOT="${QONTINUI_ROOT:-}"
if [ -z "$ROOT" ]; then
  GC="$(git rev-parse --git-common-dir 2>/dev/null)"
  [ -n "$GC" ] && GC="$(cd "$GC" 2>/dev/null && pwd)"
  [ -n "$GC" ] && ROOT="$(dirname "$(dirname "$GC")")"
fi
{ [ -n "$ROOT" ] && [ "$ROOT" != "." ]; } || ROOT="$PWD"
# STOP on a failed or empty mint — do not fall through. An empty $BEARER stages
# "Authorization: Bearer " and coord answers 401, which reads as "coord rejected
# the bearer" when the truth is "there was no bearer".
BEARER=$(bash "$ROOT/qontinui-claude-config/scripts/coord-acting-bearer.sh") || \
  { echo "no acting bearer (no agent JWT / coord down) — see Step 4"; exit 1; }
[ -n "$BEARER" ] || { echo "acting-bearer mint returned empty — see Step 4"; exit 1; }
# Stage the bearer off argv, exactly as the mint itself does (PR #160): process
# cmdlines are world-readable on this multi-session machine. If you carried
# Step 2's shell forward, $AUTH is already covered by the trap set there; in a
# fresh shell set `trap 'rm -f "$AUTH"' EXIT` here.
AUTH=$(mktemp) || { echo "mktemp failed — cannot stage the bearer off argv" >&2; exit 1; }
printf 'Authorization: Bearer %s\n' "$BEARER" > "$AUTH"
AUTHP=$AUTH; command -v cygpath >/dev/null 2>&1 && AUTHP=$(cygpath -w "$AUTH")
# 1. upsert the work unit (capture work_unit_id):
WU=$(curl -fsS -X POST "$COORD_HTTP_URL/coord/work-units/upsert" \
  -H @"$AUTHP" -H "Content-Type: application/json" \
  -d '{"slug":"<stem>","title":"<plan H1>"}')   # work_unit_id is in $WU
# 2. register the gate (the route's <stem> resolves the work unit; the body
#    carries phase_name. Only unit_ready/unit_status predicates additionally
#    carry the work_unit_id UUID inside the predicate itself):
curl -fsS -X POST "$COORD_HTTP_URL/coord/work-units/<stem>/register-gate" \
  -H @"$AUTHP" -H "Content-Type: application/json" \
  -d '{ "phase_name":"docs sync follow-up",
        "predicate":{"kind":"commit_live","repo":"qontinui/qontinui-coord",
                     "commit_sha":"<POST-LAND main sha>"},
        "clearance_audience":"agent",
        "gate_class":"routine-review" }'
# claim-anchored: POST $COORD_HTTP_URL/coord/gates/register
#   with claim_kind + resource_key + predicate (same bearer)
# attest (unchanged — keyed by gate_id):
curl -fsS -X POST "$COORD_HTTP_URL/coord/gates/<gate_id>/attest" \
  -H @"$AUTHP" -H "Content-Type: application/json"
```

The bearer is **agent-scoped** (work-unit writes + `register` only — never
approve/reject). `coord-acting-bearer.sh` reads `$COORD_AGENT_JWT` — its ONLY
source; there is no file sweep — and always prints `credential from <source>` to
stderr (the source name, never the token). Exit codes: `0` ok; `2` no
`$COORD_AGENT_JWT`; `3` mint failed; `127` missing jq/curl. Same claim-anchor
fallback applies (A2).

> **Step 3 is NOT independent of Step 2 — know what its failure means.** Both
> key off the **same proxy nonce** from the same `.mcp.json` family at call time
> (Step 2 spends it on MCP JSON-RPC, Step 3 on the forwarder's REST routes). So
> **if no `.mcp.json` is readable anywhere, Steps 2 and 3 are both down for one
> reason** — say so explicitly rather than walking the cascade to learn nothing;
> keep the per-candidate diagnostics distinct ("no `.mcp.json` readable
> anywhere" vs "N distinct files probed: file → HTTP code"). Step 1 is a partial
> exception: an already-connected coord-mcp client keeps working mid-session,
> but `/mcp` cannot re-establish it until the file is back. Fix it at the source
> — run `/mcp` (which rewrites the file with the live port + rotated key), export
> `$COORD_AGENT_JWT`, or **mint a device JWT from the local runner's UI Bridge**
> (see the residual block above): the latter two are independent of the file
> family, and the runner mint needs no prior export, so a healthy runner alone is
> enough to register a gate. Reads are unaffected
> (`https://coord.qontinui.io/health` still answers); only coord *writes* block.
>
> **Which file wins decides which tenant you register under.** With several
> accounts and session-scoped tenancy on one machine, a sibling repo's
> `.mcp.json` may hold a **different account's** proxy nonce (the forwarder's
> injected device JWT — and so the tenant — follows the nonce). Sweep your own
> worktree / `$PWD` first when it matters, and check which candidate file won
> (the `live proxy: <file>` line) before trusting the attribution.

### Step 4 — Honest failure (never a silent no-op)

If all three steps fail, **do not pretend**. Report exactly which link is missing
and point at the self-check:

> **gate NOT registered.** No transport reached coord:
> native `coord_register_gate` not in this session's tool allow-set; no live
> proxy nonce found in any candidate `.mcp.json` (root `:PORT` → HTTP CODE,
> siblings → …) — which downs Step 2 (JSON-RPC) and Step 3 (forwarder REST)
> together; **runner UI Bridge mint unavailable** (`127.0.0.1:9876` unreachable,
> or the runner is signed out and returned a non-JWT); acting-bearer mint failed
> (exit N: `$COORD_AGENT_JWT` unset / coord down).
> Run **`coord doctor`** (runner self-check — names the one failing link + its
> fix) to diagnose the missing credential, then re-run `/gate`.

**All FOUR lines must be true before you report this.** The runner-mint line is
the one most often skipped, and it is the one that most often would have worked:
a rotated nonce 401s every `.mcp.json` at once while the runner itself is
perfectly healthy. If you did not probe `:9876`, you have not exhausted the
cascade — say "not attempted", never "unavailable".

(If a `.coord-mcp-status` breadcrumb exists in your cwd, quote its reason here.
It is the RUNNER's own one-line record of why this workdir got no working
coord-mcp, and **four of its five reasons mean that provisioning pass wrote no
`.mcp.json`** — which explains the missing door rather than adding a second
fault to chase. Read "NOT written" as a fact about that pass, not the workdir: a
re-provision leaves an earlier stale config in place. Its **absence proves
nothing**: a healthy provision and a workdir the runner never provisioned both
write nothing at all, and the runner writes into the workdir IT provisioned,
which from a linked worktree is often the primary checkout rather than your cwd.
Reason table and writer:
`qontinui-claude-config/knowledge-base/qontinui-specific/coord-gates-and-access.md`.)

---

## Attest — close the loop

`attest` is the same cascade: prefer MCP `coord_attest_gate` (pass `gate_id` —
works from a device session since attest takes no upsert); fall back to the device
loopback forwarder `POST http://127.0.0.1:{runner_port}/coord-mcp/gates/{gate_id}/attest`
(header `X-Coord-Mcp-Proxy-Key`, or `Authorization: Bearer <nonce>` on newer
configs; no body bearer — the maskless tier: a masked
session has only the proxy nonce, not a raw device JWT; runner `mcp_api.rs`
`CoordWriteTarget::AttestGate` forwards it to coord's unchanged
`POST /coord/gates/{gate_id}/attest`), then the direct device-authed
`POST $COORD_HTTP_URL/coord/gates/:gate_id/attest` (HTTP). It is legal **only** on an OPEN
`operator_approval` gate whose `clearance_audience = 'agent'`, in your own tenant
— a cross-tenant, already-cleared, `operator`-audience, or non-approval gate is
not agent-attestable; leave those for the operator. Discover the gate by the
`gate_id` you recorded at registration, or look it up by anchor:
`GET $COORD_HTTP_URL/coord/agent-gates?work_unit_id=<id>&phase_name=<name>` (or
the claim-anchored query) and find the OPEN gate your work satisfied. Use the
**`agent-`** path — the operator `GET /coord/gates` is `TenantId`-only and 403s
a device JWT; `/coord/agent-gates` takes the same query params but is
**read-only** (every gate verb stays on the operator routes).

**Attest verdict (LIVE — coord PR #1249 landed):** attest takes
`verdict: "met" | "not_met"` (default `"met"`; `reason` REQUIRED for
`not_met`) on the same cascade. `not_met` is the reject door — use it when you
verified the gate's condition is **unsatisfiable**: the gate goes to `failed`
with your identity stamped, and deliberately does NOT page. Who may attest —
and whether the hardcoded agent-audience rule above still applies — is
resolved per `gate_class` from the tenant's clearance-authority matrix (A4);
this tenant has zero configured rules as of 2026-08-03, so the paragraph above
remains exactly the behavior until the operator authors one.

---

## Withdraw — cancel your own erroneous gate (LIVE — coord PR #1247 landed)

`withdraw <gate_id> "<reason>"` cancels a gate **this registrant set** that is
erroneous or superseded — the primitive that replaces "await operator
dismissal" for such gates (cancelling your own request is not self-approval).
**Registrant-only** under the device-floor rule, any audience, no config
consulted; reason required. Sets terminal verdict **`withdrawn`**, cancels a
dispatched-but-unconsumed continuation, resolves the gate's alerts, never fires
the all-cleared fanout.

Same cascade shape, narrower transports (there is no runner-forwarder REST twin
for withdraw):

1. **Native MCP `coord_withdraw_gate(gate_id, reason)`** — preferred.
2. **Live loopback proxy** (Step-2 sweep) — JSON-RPC `tools/call` with
   `"name":"coord_withdraw_gate","arguments":{"gate_id":"<id>","reason":"<why>"}`.
3. **Direct device-authed HTTP** — `POST $COORD_HTTP_URL/coord/gates/<gate_id>/withdraw`
   with body `{"reason":"<why>"}` (raw device JWT). The acting bearer is
   register-scoped and does NOT carry withdraw.

A 403 means you are not the registrant (a different device — or a different
agent pair on agent-token sessions): leave the gate for its registrant or the
operator. Honesty rule as ever: never report a gate withdrawn without the
returned verdict/`gate_id` from the response; a 404/4xx never reads as
success.

---

## Honesty rules (non-negotiable)

- **Never report a gate registered, attested, or withdrawn without a returned
  `gate_id`** (a *cleared* `gate_id` for attest; the returned `withdrawn`
  verdict for withdraw). A silent "no such tool", a 4xx, or any response
  missing the id must **never** read as success.
- **A `gate_id` is necessary, NOT sufficient — read the `warnings`**
  [policy: `coordination` `gate-warnings-mean-not-usable`]. A non-empty
  `warnings[]`, or an `initial_verdict_reason` containing **"cannot evaluate"**,
  means **REGISTERED-BUT-NOT-USABLE**: the row exists and the gate can never
  clear. Do not report the item gated. Instead: re-check with
  `coord_check_gate_predicate {predicate}` **against a control whose answer you
  already know** (identical output on the control proves the *predicate* is dead,
  not your anchor), re-register on a predicate coord can evaluate, withdraw the
  unusable one, and quote the NEW `gate_id`. An unevaluable gate looks exactly
  like a patient one and nothing escalates on it — the wrapper fails open, so it
  sits `open` until the ~7-day rot check. Full rule: `_gate-registration` →
  "Registration warnings".
- **A masked/unknown native tool is not the end** — fall through Step 1 → 2 → 3.
- **Always name the transport used** (native MCP / proxy `<url>` / HTTP acting
  bearer) alongside the `gate_id`, so the operator can see how it landed.
- **Never claim work done on a gate** without either a cleared `gate_id` or an
  explicit "gate not found" note.

---

*(Canonical registration spec: `_gate-registration` — keep this door and its
predicate/anchor/honesty rules in sync with that file. Session-close emit
procedure: `/blocked`. Open/closed gate report: `/gate-sweep`.)*
