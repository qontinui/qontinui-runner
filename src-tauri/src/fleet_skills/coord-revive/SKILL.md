---
name: coord-revive
description: Recover a dead coord-mcp transport with a TYPED verdict instead of the client's "Command failed with no output" mask. Runs the door cascade (L1 re-read own .mcp.json key, L2 sibling-key sweep, L3 acting-bearer fallback, L4 device-JWT bearer from $COORD_DEVICE_JWT / ~/.qontinui/coord-device-jwt / a runner mint, L5 the runner-independent bootstrap credential — an anonymous POST to coord's dedicated /agents/credential route, measured LIVE on this tenant 2026-09-04 (HTTP 200, a device-subject agent JWT) and on that day the ONLY live rung), reports which door is LIVE and whether that LIVE is PARTIAL, and enforces the lost-write doctrine — a "no output" coord write is presumed LOST; re-issue over the live door and verify by read. Two execute verbs — `coord-revive.sh call <tool> '<json>'` and `coord-revive.sh tools` — run a coord MCP tool over the session's OWN .mcp.json nonce (never minting one), so a diagnosed session can act, not only diagnose.
user-invocable: true
---

# coord-revive

Revive a session whose coord-mcp transport died mid-session. The native client
reads `.mcp.json` **once at startup and never again**, and it collapses every
transport failure — evicted proxy key, dead port, credential refresh, timeout —
into the single undifferentiated string **"Command failed with no output"**
while keeping the tools listed. This skill replaces that mask with a typed
diagnosis and finds the door that actually works right now.

## The lost-write doctrine (why you MUST run this)

**When ANY coord MCP tool returns "Command failed with no output", treat the
write as LOST.** The 2026-07-26 flake investigation adjudicated every such
write against prod rows: **8 of 8 prod-adjudicable "no output" writes were
LOST** (`coord_work_unit_add_citation` ×2, `coord_register_gate` ×4,
`coord_post_finding` ×2) — zero landed, every healthy-transport retry returned
a NEW id, and coord's app layer never emits that string (healthy doors always
return typed results). The recovery contract:

1. Treat the write as **LOST** — never as "slow but landed".
2. Run `/coord-revive` (the script below).
3. **Re-issue** the call over the door it reports LIVE.
4. **If it reports `DEAD`, do not stop there.** That verdict covers only the
   out-of-band doors and never probes your native `coord_*` tools (see
   "`DEAD` is scoped" below). Issue one cheap native read — `coord_gate_inspect`
   on any known gate_id. If it answers, coord is REACHABLE: re-issue over the
   native tools and continue to step 5. Only `DEAD` *plus* a failed native call
   means you are actually blocked.
5. **Verify by read** — an independent read of the resulting state (list the
   gate / finding / citation and see the row) is the ONLY trustworthy closure.
   For CAS-guarded writes (`work_unit_transition` with `from_status`) read the
   current state first: the write may be moot. For last-writer-wins tools
   (`report_status`, `claims/*`) re-issue freely, but the retry's success is
   never evidence the original landed.

Memory: `reference_coord_no_output_write_is_presumed_lost_verify_by_read`
(REVERSES the retired "post_finding returns no output but succeeds" belief).

## What the script does

Run `coord-revive.sh` (it sits next to this SKILL.md) with Bash from your REAL
working directory — L1 depends on `$PWD`:

```bash
bash <path-to-this-skill-dir>/coord-revive.sh
```

Cascade — stops at the first LIVE door:

| Step | Door | Why it can win when the session's transport is dead |
|---|---|---|
| L1 | Own cwd's `.mcp.json`, re-read fresh | The runner rotates the one-slot workdir key in place; the FILE holds the current key while the session still holds the startup snapshot |
| L2 | Sibling sweep: `<workspace-root>/.mcp.json` + every `<workspace-root>/*/.mcp.json` | A sibling repo's config often holds the live key/port when yours was evicted (same loop as `/gate` Step 2) |
| L3 | `coord-acting-bearer.sh` → direct coord MCP over HTTPS | Independent of the whole `.mcp.json` family; needs `$COORD_AGENT_JWT` |
| L4 | **Device-JWT bearer**, three sources in the fleet's documented order — `$COORD_DEVICE_JWT`, then `~/.qontinui/coord-device-jwt`, then a **mint** from the runner: its **in-process invoke door first** (`POST /ui-bridge/invoke/get_access_token_for_websocket`, answers headless), the WebView eval mint (`/ui-bridge/control/page/evaluate`) **only** when that build answers the allowlist 400 — against the same public coord MCP door. The door name says which (`source=runner-invoke` / `source=runner-eval`) | Independent of BOTH: none of them cares that every proxy key rotated, and none needs `$COORD_AGENT_JWT` (unset on this fleet) |
| L5 | **Bootstrap credential** — an anonymous `POST $COORD_HTTP_URL/agents/credential` carrying a `device_id` read from a static local file, then a **control read** to prove the token before it is called LIVE. ✅ **Measured LIVE 2026-09-04** — `200` with a device-subject agent JWT | The only rung that needs **no runner at all** — every rung above it either IS the runner (L1/L2) or spends a credential the runner minted (L4 source 3/4), and L3 needs `$COORD_AGENT_JWT`, unset on this fleet. On 2026-09-04 it was the **only** live rung on merytshost: static device JWT `401`, invoke mint `400`, eval mint `400` |

**L4 is the rung that was missing.** On 2026-08-08 all 14 probeable doors
answered 401 — the one-slot workdir key had rotated under every config — and L3
answered `NO_TOKEN`, so the cascade printed `VERDICT: DEAD`. coord was reachable
the whole time: a device JWT minted from the runner's UI Bridge drove
`withdraw_gate`, `register_gate` and `add_citation` to completion moments later.
L3's helper calls `$COORD_AGENT_JWT` "the ONLY source", which is true of *that
helper* and false of the *session*. Since this skill sells its `DEAD` line as
honest blocked-evidence, a rung that recovers where the others fail belongs in
the cascade rather than in an agent's head.

L4's mint takes its runner origins from the configs L1/L2 already read, so a
runner on a non-default port is found without configuration;
`$QONTINUI_RUNNER_URL` overrides and `http://127.0.0.1:9876` is the documented
default.

**L4's two STATIC sources are tried before the mint**, which is the fleet's
canonical device-JWT order (`CLAUDE.md`; `scripts/render-memory-cache.ps1`
implements the same three). L4 used to implement only the mint, so on a box
with **no runner but a valid `$COORD_DEVICE_JWT`** — a CI box, a headless
agent, a session that exported one deliberately — the cascade printed
`VERDICT: DEAD` **over a credential sitting in the environment**: the same
false-`DEAD` class L4 was created to close, reached by a different route.

Because those two sources are *static* while device JWTs live ~4h, **a 401 from
one of them is expected and is NOT terminal** — it is recorded and the cascade
falls through to the next source. Only the runner mint's 401 ends L4.

### Executing over the door — `call` and `tools`

Diagnosis used to be where this skill stopped: it reported which door is LIVE
and a session that had diagnosed correctly still had no way to **act** in bash
short of hand-rolling a JSON-RPC client (the 2026-09-02 session did exactly
that). The two verbs are that client, once — a port of
`lib/coord-credential.psm1`'s `Invoke-CoordProxyTool`, so there is one JSON-RPC
shape per language and no third. Plan
`2026-09-02-steering-layers-unreadable-without-a-credential`, Phase 1c.

```bash
bash <path-to-this-skill-dir>/coord-revive.sh tools
bash <path-to-this-skill-dir>/coord-revive.sh call coord_memory_search '{"query_text":"DOSSIER coord-merge-throughput","kinds":["mental_model"]}'
```

- **The door is the caller's OWN nonce.** The verbs read the nearest
  proxy-shaped `.mcp.json` walking **up** from `$PWD` (a linked worktree's
  config sits one or two levels above it), accept both header shapes
  (`Authorization: Bearer <nonce>` and the legacy `X-Coord-Mcp-Proxy-Key`), and
  replay it verbatim. The nonce is staged into a private header file, never
  argv, never stdout.
- **They NEVER mint.** `POST /coord-mcp/provision-session` re-provisions the
  one-slot workdir key and evicts the live peer's binding — the failure class
  Phase 1a of that plan exists to end. The cascade's own mint (L4 source 3) is
  bounded to run only after L1 and L2 have *probed* this workdir's key dead; a
  verb that runs on every call has no such bound, so it has no mint at all.
- **They execute whatever the nonce is allowed — writes included.** `call` is
  not a read: a `coord_memory_record` or `coord_work_unit_upsert` over it is a
  real write, and the lost-write doctrine above is yours — verify by read.
- **Output and exit codes.** The JSON-RPC `result` on stdout (pipe to `jq`); a
  one-line `coord-revive: <verb> -> OK …` on stderr naming the file and header
  the nonce came from. `0` the tool answered; `3` the tool answered with a
  JSON-RPC **error** (printed verbatim on stderr — that is *its* answer, the
  door carried the call); `1` the door did not carry the call, with a typed
  reason: `NO_PROXY_CONFIG` (no provisioned nonce on the walk up — a statement
  about this filesystem), `COORD_MCP_PROXY_UNAUTHORIZED` (below),
  `CONNECT_REFUSED` / `TIMEOUT` / the classifier's other verdicts; `4` usage
  (arguments must be ONE JSON **object**, parsed and refused locally before any
  request is sent).
- **A 401 names its recovery, per caller.** The binding was superseded or
  never registered while the transport stayed healthy. A hand-rolled caller
  re-reads `.mcp.json` — the verb just did, so the key on disk is stale too:
  run the cascade (`coord-revive.sh` with no verb) for a sibling key or a
  bearer. The native MCP client **cannot** re-read the file (served policy
  `coordination` `mcp-reconnect-is-not-agent-invocable`), so for it the
  recovery is a **new session** — **never** a runner restart.
- `COORD_REVIVE_CALL_TIMEOUT` (default 60s) bounds one call; a tool can be
  legitimately slow, and this is not a probe.

The PowerShell twin is `scripts/coord-read.ps1 call` / `tools`, over the same
own-config rule (`Get-CoordOwnProxyConfig`), for pi, Codex and CI.

### L5 — the bootstrap credential: WIRED, PROBED, and LIVE

> ✅ **L5 works. Measured against production coord 2026-09-04 from
> `merytshost`: the anonymous `POST /agents/credential` answered **`200`**.**
> Body `{token, token_exp, token_jti}`; the token an EdDSA JWT claiming
> `iss=qontinui-coord`, `sub=device:<device_id>`, `sub_type=agent`, a resolved
> `tenant_id`, **no `agent_id` claim**, **all scopes empty/false**, TTL 14400s
> (~4h). The control read `GET /coord/agent-findings?limit=1` answered `200`
> over it (`401` with no bearer), so `coord-revive.sh` reports
> **`L5: bootstrap-credential -> LIVE`**.
>
> ⚠️ **Until 2026-09-04 this section, the skill description, the script's own
> comments and three other documents all said the route was NOT DEPLOYED and
> that L5 "always" answers `BOOTSTRAP_ROUTE_ABSENT`.** That was measured wrong
> — and it had *already* been corrected a day earlier in
> `.claude/commands/manual-test-coord.md` (measured twice 2026-09-03, coord
> finding `d315ad53-4d8a-4d22-851d-4e21641792a4`, naming
> `narrow_for_anonymous_mint` in `jwt.rs` for the empty scopes), which was the
> only copy telling the truth. It is the same defect class as a stale citation:
> a rung documented as permanently shut is a rung nobody tries. On the day it was corrected here, a
> full cascade run to exhaustion on this box had L5 as the **only** live door —
> the static `~/.qontinui/coord-device-jwt` `401`, the UI-Bridge invoke mint
> `400` (not on the build's allowlist), the WebView eval mint `400` (CSP
> `unsafe-eval`). The file was telling a stranded session not to bother.
>
> Re-measure before trusting either direction. `BOOTSTRAP_ROUTE_ABSENT` is
> still a wired arm below, because another deployment or a rollback can answer
> `404`/`405` — but on this tenant reaching it is a **regression**, not the
> norm.

**L1–L4 are only nominally four deep.** L1 and L2 *are* the runner (it serves the
loopback proxy); L4 sources 3 and 4 are credentials the runner mints; L3 needs
`$COORD_AGENT_JWT`, which this skill's own L3 text says is unset on this fleet.
So one wedged runner takes the whole cascade down at once, and the two static
device-JWT sources — the only survivors — hold a **~4h** token, i.e. they are
expired more often than not. Measured 2026-08-28 during an `/unattended` closeout
on a box whose runner had wedged: MCP tools absent, the loopback proxy HTTP 000
on all 5 `.mcp.json` candidates, `https://coord.qontinui.io` + device JWT
`401 invalid token` with the runner mint timing out at 60s / 0 bytes, and the
policy mirrors answering **read-only**. The session had to report its own work as
DROPPED. (Plan
`2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline`.)

**L5 is the rung whose only input is a file on disk.** Resolve a `device_id` —
env `QONTINUI_MACHINE_ID` first, else `~/.qontinui/machine.json` `"device_id"`,
falling back to the legacy `"machine_id"` spelling — and POST it **anonymously**
(no `Authorization` header of any kind) to
`$COORD_HTTP_URL/agents/credential`, the dedicated credential-only route.
`$COORD_HTTP_URL` defaults to `https://coord.qontinui.io`.
`COORD_REVIVE_NO_BOOTSTRAP=1` turns L5 off entirely.

**That route answers `200` here.** Measured against production coord
2026-09-04: `POST /agents/credential` with `{"device_id": "<uuid>"}` and no
`Authorization` header of any kind returns **`200`** and a usable bearer (claim
shape above). The 2026-09-02 reading of **`405` with an empty body** that this
section used to assert is superseded — whatever it measured, the door is open
today.

**The `404`/`405` arm is kept, and "absent" is still spelled `405` when it
happens.** `POST /agents/definitely-not-a-route` returns an identical empty
`405` (re-confirmed 2026-09-04), so an unregistered POST anywhere under
`/agents/` gets 405 rather than 404 on this router. Reading that as a refusal
would be a confidently wrong verdict about the *device* for a fact about the
*router*, so both codes still classify as `BOOTSTRAP_ROUTE_ABSENT` — but on
this tenant that arm now means a regression, a rollback or a different
deployment, and should be reported as one. (`GET /agents/credential` answered
`403 tenant_not_resolved` on 2026-09-02 and `405` on 2026-09-04; either is a
router artefact and not a device verdict — this rung only ever POSTs.)

**`coord-revive.sh` probes that route and NOTHING else.** In particular it does
not substitute `POST $COORD_HTTP_URL/agents/allocate`, which mints the same class
of token today. Three shipped documents forbid carrying a coord rung on that door
(`/gate` and `/policy` both carry it on their **generic remote MCP** rung, and
`knowledge-base/qontinui-specific/coord-gates-and-access.md` restates it), and
whether it may
ever be used this way is an **open operator ruling**, escalated as coord gate
**`ece99898-30c6-4f8c-be8e-1de5f09abebc`** (`operator_approval`, `gate_class:
security-surface`, currently **open**). An agent does not pre-empt a ruling that
has just been asked for.

**So what IS the honest outcome when the cascade is exhausted?** With L5 live,
an exhausted cascade should now be rare — L5 answering `LIVE` *is* the outcome
in the ordinary runner-wedged case, and the recovery is to spend that bearer on
the device-authed `${COORD_HTTP_URL}/coord/...` REST routes. When L5 too comes
back dead (`BOOTSTRAP_ROUTE_ABSENT`, `BOOTSTRAP_UNREACHABLE`,
`BOOTSTRAP_TOKEN_UNVERIFIED`, no device_id), the answer is a `DEAD` verdict
**plus a durably recorded blocker** — not a token from the forbidden door.
Write the gate or finding SPEC verbatim so a peer with a working transport can
carry it, exactly as `_gate-registration`'s transport-floor rule already
requires, and **name the verdict L5 actually returned**. **That is a materially
different report from "coord is down."**

**Why the shape is right even though it does not work yet.** The design question
this rung answers is whether a *credential-minting* route may be anonymous at
all. Shipped plan `2026-08-14-runner-unauthenticated-coord-writers` drove bare
coord writes 63 → 0 and left `src-tauri/tests/coord_auth_pin.rs` as the durable
guard — and it sanctioned exactly one carve-out shape,
`pair.rs::pair_via_browser`, which is anonymous **because it mints the
credential, so requiring one is circular.** A credential-only route is that
shape, and the pin objects to an unauthenticated *write*, not to *using an issued
token*: everything L5 would do after a mint carries a bearer. The exposure that
remains open is a property of the **sibling** `/agents/allocate` route, not of
this shape — it is unauthenticated by deliberate, documented design and mints a
~4h agent JWT with a real `agent_id`. (Measured 2026-09-04, that token's
`merge_propose` is `false` and its `git_push` is scoped to the reserved branch
alone, plus agent NATS subjects — narrower than "carrying `merge_propose` and
`git_push`" implied, and still wider than the bootstrap token's all-empty
scopes.) Plan
`2026-08-31-coord-mcp-credential-selection-by-binding-provenance` Phase 8
surfaced that and refused to close it; the gate above is the escalation that
asks for the ruling. Full chain and citations:
`knowledge-base/qontinui-specific/coord-gates-and-access.md` → "The
`/agents/allocate` exposure and its open ruling".

**The credential is verified with a control read before L5 is called LIVE — and
that arm now runs for real.** (Measured 2026-09-04: mint `200`, control read
`200`, verdict `LIVE`.) A minted token that does not actually authenticate is a false green, and this whole rung
exists because a false green cost a closeout its output. The control read is
`GET $COORD_HTTP_URL/coord/agent-findings?limit=1`: `200` with a good bearer,
`401` without one (registered behind `require_jwt`, coord `routes.rs`
`agent_findings_authed`). It is cheap, it is read-only, and it proves the exact
property the rung asserts — that this bearer authenticates against coord *now*.
**A mint that answered `200` and a control read that did not is `BOOTSTRAP_TOKEN_UNVERIFIED`,
not LIVE.**

**Every fall-through TO L5 is counted, because the fleet cannot see this
frequency any other way.** Detection of the underlying wedge already exists — the
supervisor's `health_cache.rs` step 3e emits `RUNNER WEDGED: …` every ~5 min and
refuses to auto-restart by contract — so what is missing is not a detector but an
**aggregate**: nothing counts "closeouts that could not write". Reaching L5 means
the ordinary doors already failed, so the counter must not depend on one of them.
It is therefore the **local** breadcrumb the guard component already ships,
`scripts/lib/guard-decision-log.sh` → `guard_decide`, whose records land in
`~/.qontinui/logs/guard-decisions.log` (`$QONTINUI_GUARD_DECISION_LOG` /
`$QONTINUI_LOG_DIR` override) and whose `tag` field is documented there as *"the
field you grep and count"*. It spawns nothing, it never fails, and
`rotate_log_if_large` in `scripts/session-id-stamp.sh` already bounds the file at
SessionStart. **The count is the point whatever the rung ends in** —
`l5-reached` fires on entry, before any probe, so a
rising rate measures how often a session is driven this far regardless of what
the last rung can do about it. That rate is precisely the signal the plan asks
for. Two records per run:

| Record | When | What it counts |
|---|---|---|
| `coord-revive  warn  l5-reached` | L1–L4 all missed and L5 is about to run | **the rate this rung is needed** — a rising count is itself the signal that something upstream is wedging |
| `coord-revive  allow\|unknown  l5-<outcome>` | after L5 resolves | which arm it ended in (`l5-live`, `l5-route-absent`, `l5-device-rejected`, `l5-token-unverified`, `l5-no-device-id`, `l5-unreachable`) |

Count them with `cut -f5 ~/.qontinui/logs/guard-decisions.log | grep -c '^l5-reached$'`.
A missing log is **UNKNOWN, not zero** — the same reading `guard-shadow-report.sh`
prints for its own absent log.

**Worktree-safe:** the workspace root L2 sweeps is derived from
`git rev-parse --git-common-dir`, so running from a linked git worktree
(`agent-worktrees/<uuid>/…`, `.claude/worktrees/…` — the fleet default under
`QONTINUI_AGENT_WORKTREE_MODE=1`) still resolves to the real workspace root
rather than the worktree container. Set `$QONTINUI_ROOT` to override; from a
non-git cwd the script says so on stderr and assumes `$PWD`.

Every failed probe prints a **typed verdict** (the local substitute for the
client's mask):

| Verdict | Meaning | Next move |
|---|---|---|
| `COORD_MCP_PROXY_UNAUTHORIZED` | Stale/evicted proxy key (HTTP 401) — the one-slot workdir key rotated | Use the door the cascade finds; the file that 401'd is stale |
| `CREDENTIAL_REFRESHING` | Proxy up, deliberately withholding while its device JWT refreshes (HTTP 503) | Retry-safe — the script itself re-probes once; transient |
| `CONNECT_REFUSED` | Dead port, no listener | Runner gone/moved; a sibling config or L3 must carry it |
| `TIMEOUT` | **Nothing reached this box** inside the probe budget — the request was abandoned client-side, so whether the proxy answered at all is UNKNOWN | Retry-safe; the script re-probes once. Often saturation. Do not hammer it, and do NOT restart the runner on this alone |
| `TIMEOUT_UPSTREAM` | The opposite half of the same word: the **proxy ANSWERED** (`408`/`504`) and reported that ITS upstream did not. The local door is provably alive; the hop behind it hung | Retry-safe (it shares the `TIMEOUT*` prefix deliberately, so it inherits the one re-probe). The local proxy is **not** the fault — do not touch the key or the runner |
| `PROXY_LIVE_UPSTREAM_DEAD` | HTTP `502` carrying one of the runner's typed upstream codes (`COORD_MCP_PROXY_UPSTREAM_UNREACHABLE` / `_READ_FAILED` / `_NON_JSON_ERROR`). **Proxy LIVE, coord `/mcp` hop DEAD** — the F2 class. Matched on the envelope's `code` field, never on its message text | Re-probed once; a second `502` is final and the door is **not** offered as working. Retryable-unknown: coord's side, not your key's. Move down the cascade, and re-run in a few minutes |
| `LIVE_APP_ERROR` | **A LIVE verdict, not a failure.** The end-to-end `tools/call` was carried and the TOOL answered `isError:true` | Re-issue over this door. See "`isError` is the TOOL's verdict" below — never read it as a dead door |
| `PROXY_LIVE_E2E_UNVERIFIED` | **Also LIVE**, and honest about how far it was measured: `tools/list` answered and the end-to-end probe did not run (`$COORD_REVIVE_E2E=0`) | Usable, but read the ANSWER to your re-issued call rather than treating this as end-to-end proof. Unset `$COORD_REVIVE_E2E` to measure it |
| `SKIPPED_SHARED_UPSTREAM_REFRESHING` | **A skip, not a verdict about that door** — nothing probed it. A sibling on the same `host:port` already settled on `CREDENTIAL_REFRESHING` after its own retry, and they share one upstream process | Nothing to do: the fact is already established by the sibling. `$COORD_REVIVE_NO_UPSTREAM_SKIP=1` probes every sibling anyway |
| `SKIPPED_BUDGET_EXCEEDED` | **Also a skip** — the `$COORD_REVIVE_TOTAL_BUDGET` sweep budget ran out before this door was reached. UNKNOWN, never dead | Raise the budget to finish the sweep, or probe the named door by hand |
| `NO_RUNNER` | L4 mint: nothing answered at that origin (connection refused, or no status at all) | Runner down, moved, or never started; set `$QONTINUI_RUNNER_URL` |
| `RUNNER_TIMEOUT` | L4 mint: the port **accepted** the connection but produced no response within `COORD_REVIVE_MINT_TIMEOUT` (60s) | Often **saturation**, not a dead runner — do NOT restart it on this alone (served policy `production-and-cost` `runner-lifecycle`). Re-run, or use another door |
| `RUNNER_EVAL_FAILED` | L4 mint (the door name says which of the two — `source=runner-invoke` or `source=runner-eval`): the runner **answered**, but not with a well-formed mint result — a non-2xx, a route-absent 404, or a `success:false` body. The verdict quotes the response's own error string **when the body carried one** | **Not** a sign-in problem. Read the quoted error. A 4xx means the route moved or something else answers on that port; a 5xx means the route is present and failed server-side |
| `INVOKE_MINT_ROUTE_ABSENT` (stderr line, not a final verdict) | L4 mint: the in-process invoke door answered the allowlist **400** (or 404) — this runner build predates the entry | The verb falls back to the WebView eval mint on its own. The next runner **start** picks the entry up; never restart a running runner over it |
| `RUNNER_TIER_TOO_LOW` | L4 mint: the runner is **Tier 0/1** (`Local` / `LocalProvider`), where the Qontinui account commands do not exist at all | Change the runner's tier (Settings → Account), or use another door. The runner is **not** signed out and signing in will not help |
| `RUNNER_TIER_UNKNOWN` | L4 mint: the runner could not resolve its own tier — a corrupt or unreadable `settings.json`. Its account state is unchanged | Repair `settings.json`. A sign-in CTA here is the exact mistake the runner's own `NO-DOWNGRADE (C4)` comment records |
| `RUNNER_SIGNED_OUT` | L4 mint: it answered and genuinely holds no token — either a non-JWT-shaped value, or its own `Not authenticated` error | Sign the runner in. The shape check fires before the token is ever sent, so this is never reported as coord rejecting you |
| `ENV_UNSET` / `FILE_ABSENT` | L4: that static credential source is simply not present — a statement of **absence**, not a fault | Nothing to do unless you meant to provide one; the cascade moves to the next source |
| `HOME_UNRESOLVED` | L4: neither `$HOME` nor `$USERPROFILE` is set, so source 2 has no path to read | A **local** environment fault; it says nothing about whether the credential exists |
| `DEVICE_JWT_ENV_MALFORMED` | L4: `$COORD_DEVICE_JWT` is set but is not JWT-shaped (3 dot-separated base64url parts) | Not sent — an unshaped bearer would draw a 401 this script would then blame on coord. Fix or unset the variable |
| `DEVICE_JWT_FILE_MALFORMED` | L4: `~/.qontinui/coord-device-jwt` is readable but its contents are not JWT-shaped — a whole JSON response left in the file fails here too, by design | Same treatment — not sent. Re-mint the file, or delete it |
| `DEVICE_JWT_UNAUTHORIZED` | L4: coord rejected a device JWT | Expired, or bound to another tenant. From a **static** source this is expected and **not terminal** — the cascade falls through to the next source |
| `BOOTSTRAP_NO_DEVICE_ID` | L5: no `device_id` resolvable — `$QONTINUI_MACHINE_ID` unset **and** `~/.qontinui/machine.json` absent or unreadable. A statement of **absence**, and a LOCAL one | Nothing about coord. Export `$QONTINUI_MACHINE_ID`, or pair this machine so the runner writes `machine.json`. Nothing is sent — an empty `device_id` would draw a 4xx this script would then blame on coord |
| `BOOTSTRAP_MACHINE_FILE_MALFORMED` | L5: `~/.qontinui/machine.json` is readable but carries neither a `device_id` nor the legacy `machine_id` (or is not JSON) | Also LOCAL, and kept apart from the row above on purpose: "the file is not there" and "the file is there and says nothing" have different fixes. Repair the file; nothing was sent |
| `BOOTSTRAP_ROUTE_ABSENT` | L5: the dedicated `POST /agents/credential` answered **404 or 405** — the route is not answering on this coord. `405` is how an unregistered POST under `/agents/` reads on this router (measured 2026-09-02 and re-confirmed 2026-09-04 with `POST /agents/definitely-not-a-route`), so it is classified here and NOT as a refusal. ⚠️ **This is NOT the expected outcome any more** — the same POST answered `200` against production coord on 2026-09-04, so hitting this arm means a regression, a rollback, or a different deployment | Report it as a regression, naming the code you saw. Still **not a licence to substitute `POST /agents/allocate`** — three shipped documents forbid carrying a coord rung on that door, and whether it may ever be used this way is open operator ruling `ece99898-30c6-4f8c-be8e-1de5f09abebc`. With no other door, report `DEAD` **plus a durably recorded blocker**: write the gate/finding SPEC verbatim for a peer with a working transport |
| `BOOTSTRAP_DEVICE_REJECTED` | L5: the route answered and **refused this device** — a non-2xx that is neither 404 nor 405 (an unknown or unregistered `device_id`, or a malformed UUID) | A verdict about the DEVICE, not about coord's health. Check the `device_id` you resolved is the one coord knows; the response's own error string is quoted in the verdict |
| `BOOTSTRAP_NO_TOKEN_IN_RESPONSE` | L5: the route answered `2xx` but the body carried no JWT-shaped token (3 dot-separated base64url parts) | The route's response shape changed, or something else answers on that host. NOT sent onward — an unshaped bearer would draw a 401 this script would then report against coord |
| `BOOTSTRAP_TOKEN_UNVERIFIED` | L5: a token WAS minted, and the control read `GET /coord/agent-findings?limit=1` did not answer `200`. **The whole point of the rung's verification half** | Never report LIVE on this. A mint is not an authentication: say the mint succeeded AND the credential did not verify, and name the control read's status — the two facts together are the diagnosis |
| `BOOTSTRAP_UNREACHABLE` | L5: the mint POST never completed — connect refused, DNS, TLS, or a timeout on `$COORD_HTTP_URL`. Carries curl's own `[curl: …]` line | The one L5 verdict that IS about coord (or this box's network). Every other L5 verdict is about a local file, the device, or the token |

**Why the last five rows exist: `RUNNER_SIGNED_OUT` used to absorb all of them.**
The response reader returns the **empty string** for every body it cannot parse
into a token (`.data.value`, then `.data.result.value` as a fallback), and an
empty string reads at the call site as a
*value* rather than as an error. So three genuinely different faults all
reported "sign the runner in":

- an evaluate fault — measured 2026-08-13, the UI Bridge answers
  `HTTP 400 {"success":false,"error":"JS evaluation error: …"}`;
- a **route-absent 404**, whose body is *non-empty*, so the `NO_RUNNER`
  fast-path does not fire either;
- a genuinely signed-out runner — the only one for which the advice was right.

Worse, `get_access_token_for_websocket_impl` calls `require_tier_2()` as its
**first statement, before any keychain read**
(`qontinui-runner/src-tauri/src/commands/auth.rs`), and that gate has two
failure arms — Tier 0/1 (`AuthError`) and unresolvable tier (`ConfigError`).
Both arrive here as the same `success:false` shape, so a **perfectly healthy
Tier-1 runner was told to sign in**, which cannot help. The runner's own
`NO-DOWNGRADE (C4)` comment documents exactly that wrong-CTA mistake and fixed
it internally; this script was reproducing it one layer out. The error string
is now matched (on a stable ASCII substring) **before** the status code is
consulted, because the tier errors arrive on a non-2xx. An unrecognised error
falls back to `RUNNER_EVAL_FAILED` with the error quoted — never back to
`RUNNER_SIGNED_OUT`.

### The spawn-time breadcrumb: the runner's own reason, read before L1

Every run now reads **`.coord-mcp-status`** in your cwd and reports it **with
an age** — as a `BREADCRUMB (age …, verdict …, workdir …):` line on stderr
before L1, and again inside the `DEAD` verdict block, so a pasted verdict
carries both the reason and how old it is.

That file is the **runner's** record that it could not give this workdir a
working coord-mcp (`coord_mcp.rs`, `write_degraded_breadcrumb`;
`/gate` and `/policy` tell an agent to read it and `qontinui-pr` points at it in
its no-credential error — none of them writes it, and none of them opens it the
way this script does). It matters here because **six of its thirteen reasons
say that provisioning pass wrote no `.mcp.json`**: no device JWT in the runner's
access_token slot, a bearer whose `sub_type` is neither `device` nor `agent`, a
workdir the non-clobber guard refused, an unresolvable
bound API port (device or agent path), or an agent JWT with no parseable `sub`.
In those cases L1 reporting nothing in your own cwd is not a second mystery to
chase — it is the documented consequence of a fault the runner already
diagnosed. (The non-clobber reason is the one worth holding separately: it
covers a foreign `.mcp.json`, an unparseable one, and — at a secondary runner's
umbrella root — no file at all, because the shared-root arm is checked before
the file is read. Look at what is on disk rather than assuming the first.)

**Count the reasons from the source, never from this paragraph.** Two of the six
landed in runner `38c337ba5` on 2026-08-19 and were missing from every document
in this repo until 2026-08-28 — including this one, which said "four of its
five". `scripts/breadcrumb-reason-drift.py` re-derives the set in one command.

**That is "this pass wrote none", not "there is no config".**
`coord_mcp_safe_to_write` passes a workdir whose file is absent *or* holds only
our own `coord-mcp` config. Three of the fourteen call sites return BEFORE that
guard is consulted at all and the rest return after it; either way none deletes
anything — so a
re-provision leaves an earlier, stale `.mcp.json` sitting there. L1 probing it
into a `CONNECT_REFUSED` or a `COORD_MCP_PROXY_UNAUTHORIZED` while the
breadcrumb says "NOT written" is a consistent pair, not a contradiction.

The remaining **seven** reasons are the probe's typed verdicts, and they mean the
opposite: a `.mcp.json` WAS written and did not answer at spawn. They are
`TIMEOUT` (the 12 s budget expired — *NOT known dead*), `CONNECT_REFUSED`,
`UNAUTHORIZED (401)`, `CREDENTIAL_REFRESHING (503)`, some other `HTTP <status>`,
`HTTP_200_NOT_MCP`, and an unclassified `TRANSPORT` error — the same vocabulary
this script's own per-door table uses, reused on purpose rather than invented
twice.

**A breadcrumb still quoting `(dead port | 401 stale nonce | coord down)` came
from a runner build predating those verdicts, and it is not a diagnosis.** That
string was written on a **3-second** budget with every transport error collapsed
to "not reachable", so *the runner was merely slow* was a fourth cause it never
named, on a box where CLAUDE.md records `:9876/health` sampled between 296 ms
and 10120 ms. On 2026-08-20 it named a dead port while `:9876` answered
`/health` `derived_status healthy` with 59 live terminals in the same session
(plan `2026-08-20-worktree-spawn-autonomy-and-trust-preconditions`, finding 18);
on the same plan, finding 75, a different session read the identical string and
it was accurate. Read the legacy string as "no 2xx within 3s" and nothing more —
which cause it actually was is exactly what the typed verdicts below settle, and
that is the cascade's whole job.

**It never changes the verdict, and it is not a probe.** Three limits, all
stated in the output rather than left for the reader to infer:

- **Presence is spawn-time — and this script now says HOW OLD.** The runner
  clears it on a successful probe, but whether anything re-evaluates it between
  provisioning passes is a property of the runner **build**, so a session that
  recovered on its own can keep a stale file; a session that lost coord later
  grows no new one either way. (A re-provision of the same workdir — a second
  terminal, a looping agent — can clear or rewrite it on any build.)
- **Freshness is read from line 2, and an unaged breadcrumb is UNKNOWN.** A
  stamping runner appends one JSON object — `written_at`, `workdir`, `port`,
  `verdict`, `build_id`, `schema`. The script reads line 1 and line 2
  **separately**, because the flattening helper it used to pipe the whole file
  through maps every newline to a space, which would paste raw JSON onto the end
  of the `BREADCRUMB:` line and into the `DEAD` block that gets pasted as
  evidence. Then: **within 30 minutes** it prints the age and the breadcrumb
  stays spawn-time evidence about one provisioning pass; **older than 30
  minutes** it prints `STALE - NOT evidence of the current state`; **no line 2,
  or a `written_at` that will not parse** it prints `LEGACY` / `UNSTAMPED AGE`
  and treats it identically — UNKNOWN age, never fresh, which is the common case
  while older builds are still on the fleet. A stale breadcrumb is never
  rendered as a fault and never as health: the cascade probes, the breadcrumb
  only explains. If line 2's `workdir` is not your cwd the script says so, since
  that is another directory's evidence.
- **Absence is UNKNOWN, never health.** A healthy provision writes nothing —
  and so does a hand-typed session, a workdir the runner never provisioned, and
  a runner build predating the breadcrumb. One silence is *deliberate*: when the
  non-clobber guard declines a workdir that already declares a coord-mcp (a
  foreign agent-JWT config, or a secondary runner leaving alone a primary
  shared-root config that declares one) the runner writes nothing on purpose,
  so an absent breadcrumb beside an `.mcp.json` L1 could not revive is a
  diagnosed shape rather than an unexplained one. *Declared* is not
  *answering* — the predicate is a `/mcpServers/coord-mcp` key test — which is
  why a dead entry lands in exactly this silence. A bearer whose `sub_type` is neither `device`
  nor `agent` is **not** an absence case — that arm breadcrumbs unconditionally,
  and this list said the opposite until 2026-08-28. The absent case is reported
  in those words for that reason; "no breadcrumb, so coord-mcp was fine at
  spawn" is the silent-empty-is-unknown error this script exists to stop
  making.
- **The reader looks in your cwd only, deliberately.** L2 sweeps siblings for
  `.mcp.json` because any live door serves you; a breadcrumb is the opposite —
  it describes ONE workdir's provisioning, so quoting a sibling's copy would be
  another session's evidence. But the runner writes into the workdir IT
  provisioned, which from a linked worktree is often the primary checkout, and
  the worktree copy is the less durable one (finding 18 measured it gone 29 h
  later while the checkout's survived). The absent-case line says so rather than
  implying the file exists nowhere.

Full reason table and the writer's call sites:
`qontinui-claude-config/knowledge-base/qontinui-specific/coord-gates-and-access.md`
→ "`.coord-mcp-status` — the runner's degraded-provisioning breadcrumb".

### The APPROVAL half: a LIVE door is not a loaded tool

Every rung of the cascade probes the **declaration** half of the wiring — a
`.mcp.json` naming a door, and whether that door answers. There is a second
half. Claude Code will not load a **project-scoped** MCP server it has not
**approved**, that approval lives in a settings key rather than in `.mcp.json`,
and the two halves fail independently. Only the declaration half leaves a
`.coord-mcp-status` breadcrumb, so an absent `coord_*` tool has at least two
causes and everything above this section distinguishes exactly one of them.

That gap was named, not discovered: PR #370 restored the approval key after
PR #256 deleted it and closed by recording that **`/coord-revive` could not name
this failure mode** — "its L1 door re-reads `.mcp.json`, which comes back healthy
in exactly this case, so the cascade would report a LIVE transport while tools
stay masked". A recovery tool reporting LIVE at the moment an agent has no coord
tools is the client's mask wearing a success label: the same defect class as the
false `DEAD`s L4 and the `--git-common-dir` fix closed, with the sign flipped.

Every run now reads the approval half and prints `APPROVAL:` lines — the
per-layer readings on stderr before L1, and the summary again on **stdout**
beside the verdict, `LIVE` as well as `DEAD`, so a pasted verdict carries it.

<!-- APPROVAL-SUMMARY-ROSTER: begin. Every token `coord-revive.sh` can assign to
     $APPROVAL_VERDICT must have a row here, and every row must name a token the
     script can actually emit. `approval-half-test.sh` asserts BOTH directions
     against these markers -- located by marker rather than by document shape,
     because a table found by "the first table after a heading" silently
     re-points itself the first time someone adds a heading above it. -->

| Summary | Meaning | Next move |
|---|---|---|
| `REJECTED` | `coord-mcp` is named in a `disabledMcpjsonServers`. This rejects the server in every permission mode **and in an untrusted folder**, so it outranks any approval beside it | Remove the entry. No door in the cascade can work around it |
| `NOT_APPLICABLE` | No `coord-mcp` declared in this cwd — nothing here for a settings key to approve | The missing half is the **declaration**; read the breadcrumb line, not this one |
| `DECLARATION_UNKNOWN` | This cwd's `.mcp.json` was absent, unreadable or unparseable, so whether there is anything here to approve could not be established | A statement about **this directory only**; the per-layer `APPROVAL:` readings above the summary still stand. Running from a sibling repo with no `.mcp.json` reaches this legitimately |
| `APPROVED_UNGATED` | An approval sits in a layer workspace trust does not gate — the **user settings file** (`$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`) | The approval half is fine. Keep reading the doors |
| `APPROVED_TRUST_GATED` | The only approval found is repository-supplied, and this folder **is** trusted, so it counts | Fine here — and it would stop counting in a folder that is not |
| `APPROVAL_HELD_UNTRUSTED` | The only approval found is repository-supplied and the folder **has** a `projects` entry whose `hasTrustDialogAccepted` is not `true`. Interactively, "the repository's own approvals don't count" | Trust the folder, or move the approval to a layer trust does not gate. **Not** a prediction that your tools are masked — see below |
| `APPROVAL_TRUST_UNKNOWN` | The only approval found is repository-supplied and the folder's trust could not be established at all — no `projects` entry, no readable map, or unparseable | **UNKNOWN, not a refusal**: nothing here observed a withheld trust. Read the `APPROVAL: trust` line for which of the three it was |
| `NO_APPROVAL_FOUND` | Declared, and no readable layer names it | UNKNOWN, not proof: managed settings and a `--settings` file are real approval sources this script does not guess at a path for |

<!-- APPROVAL-SUMMARY-ROSTER: end -->

**`APPROVAL_TRUST_UNKNOWN` and `APPROVAL_HELD_UNTRUSTED` are kept apart on
purpose.** A folder Claude Code has no record of being started in is not a folder
whose trust was refused, and only the second is evidence of anything. Collapsing
them — which the summary did until pre-PR review — would report a never-visited
path as a refusal in the one token an agent greps for.

**The user store (`.claude.json`) counts as an approval but is *not* ungated.**
It is where the interactive approval lands and it is plainly user-level, so
filing it beside the user settings file is tempting — but what is *documented* as
still applying in an untrusted folder is `~/.claude/settings.json`, managed
settings and `--settings`, and not this. An approval found only in the store
therefore reports `APPROVAL_TRUST_UNKNOWN` rather than a confident
`APPROVED_UNGATED`: the conservative direction, and it names the fact it is
missing instead of extending a citation.

**Both user-level files move with `$CLAUDE_CONFIG_DIR`, and they do not move
together.** Unset, the settings file is at `~/.claude/settings.json` while the
store is at `~/.claude.json` — a directory apart. Set, **both** sit directly in
`$CLAUDE_CONFIG_DIR`. Reading the home-derived paths on a machine that sets it is
not a near miss but a different file: measured on the operator box 2026-08-30,
`~/.claude.json` was six weeks stale with 7 `projects` and no entry for this repo,
while the live store had 12 entries and this repo trusted. The summary read the
stale one until that date, reporting `APPROVAL_TRUST_UNKNOWN` for a folder whose
trust *is* recorded.

**The layer readings are unioned, not precedence-resolved.** An approval in any
readable layer counts, so a higher-precedence `enableAllProjectMcpServers: false`
is printed and then not subtracted; only `disabledMcpjsonServers` is decisive,
and only because it is documented as rejecting the server from any settings file.
Where the layers disagree, read the per-layer lines rather than the token.

**Three things it deliberately does not do.**

- **It never changes the verdict**, for the same reason the breadcrumb does not:
  the doors are a different transport from your native `coord_*` tools, and the
  approval half governs only the latter. A LIVE door beside a withheld approval
  is a coherent pair — and it is the pair no line here could print before.
- **It does not reproduce Claude Code's resolution.** It reports what it read.
  In a `claude -p` run or an SDK session the trust dialog never appears and
  project servers are "connected without asking, approved or not", so
  `APPROVAL_HELD_UNTRUSTED` is a statement about one input in a session type a
  shell cannot observe from the inside.
- **It does not guess a path for managed settings or `--settings`.** Both are
  genuine approval sources whose locations are platform- and
  invocation-specific; inventing one to report "absent" for would be a
  named-but-wrong cause, which the named-cause invariant does not buy.

**Trust is keyed on the repository root — and from a linked worktree, on the
MAIN checkout's root.** That is why the trust key is resolved from
`--git-common-dir` rather than from `$PWD`, and it is a *deliberate divergence*
from the `resolve_root()` used by L2: that helper falls back to `$HERE` because
any anchor that finds the fleet's workspace root is as good as another, while
trust is a property of the folder the **session** is in. `$HERE` is always
inside `qontinui-claude-config`, so accepting it would report that repository's
trust for a session running elsewhere. Outside a repository the key is the
directory you started from. Reading the worktree's own path instead would look
up a `projects` key that has never existed and report `noentry` — "no record of
this folder", which the output states as UNKNOWN and never as a refusal — for a
repository whose trust was granted long ago.

Self-test: `.claude/skills/coord-revive/approval-half-test.sh`, on the guard
roster. Every summary token is paired with a negative control, because a
classifier that only ever answers "yes" is indistinguishable from one that
cannot answer "no" — a disable list naming *another* server must not reject, a
string-valued `enabledMcpjsonServers` must report `badtype` rather than approve,
and two runs differing only in the approval fixture must agree on the `VERDICT`
line and the exit code.

It also pins **the table above against the script, in both directions** — a
token the script can emit with no row here is an undocumented output, and a row
naming a token the script cannot produce sends a reader hunting for a string that
never appears. That check exists because this table shipped two tokens short:
`DECLARATION_UNKNOWN` and `APPROVAL_TRUST_UNKNOWN` were added to the summary
during pre-PR review and never reached the roster, leaving the most common
reading on this fleet as the one an agent could not look up.

### A MIXED verdict set on ONE port is a RUNNER WEDGE, not a key fault

**Read the verdict SET before acting on any single verdict.** The table above
maps each verdict in isolation, and for a single-cause fault that is correct.
But the runner can fail in a way that produces *several different verdicts on
the same port within seconds*, and the per-verdict reading is then actively
harmful.

Observed on the operator box during a runner wedge (2026-08-08), probing the
**same port** inside ~60 s:

    401  →  TIMEOUT (15s)  →  CONNECT_REFUSED  →  TIMEOUT  →  401

**That set is non-monotonic and self-contradictory, and no key or config fault
can produce it.** A stale key 401s *every* time. A dead port refuses *every*
time. Mixing HTTP-level answers (`401`, `200`) with transport-level failures
(`TIMEOUT`, `CONNECT_REFUSED`) means the process is **intermittently
accepting** — i.e. the runner is up and its HTTP surface is starved, not gone
and not misconfigured.

| If the probes show | Verdict | Next move |
|---|---|---|
| The **same** verdict every time | Trust the table above | Per-verdict move |
| A **transport-plane** verdict (`TIMEOUT` / `CONNECT_REFUSED` / `UNREACHABLE` — no HTTP status came back at all) **interleaved** with an **HTTP-plane** one (a `401`-class answer, a `LIVE`-class answer, `503`, `TIMEOUT_UPSTREAM`, any `HTTP_<code>`) on one `host:port` | **`RUNNER_WEDGED`** | Do NOT re-provision the key. See below. |

**The script now computes this itself and prints `VERDICT: RUNNER_WEDGED
endpoint=<host:port>`** — accumulated from the per-endpoint verdicts the sweep
already pays for, so it costs no extra probe. It is a **second verdict, about a
PORT**, printed beside the primary one, which is about a **door**; it never
suppresses a door and never changes the primary verdict, and it is emitted on
the `LIVE` path too (another door carrying your call does not un-wedge the
wedged one). **On that path the re-provision advice is suppressed** — the `Next:`
line stops naming `/coord-mcp/provision-session` at all, because naming the route
is what a reader acts on, and acting on it during a wedge evicts a live peer's
key to fix a key that was never broken.

Until 2026-09-05 this section was a **promise the tool could not keep**: PR #259
landed the doctrine and the table row, and the string `RUNNER_WEDGED` appeared
**nowhere in `coord-revive.sh`**, so a reader who followed this page waited for
output that would never arrive. Note the plane split is what the detector keys
on, and `TIMEOUT_UPSTREAM` is read as an **HTTP**-plane answer despite the name:
a `504` is the proxy *answering*, so counting it as transport evidence would
manufacture wedges that are not happening.

**Scoped to LOOPBACK endpoints, deliberately.** The verdict is a statement about
*the runner*, which serves the proxy on its own loopback port. `L3`/`L4` probe
the **public coord host** under several different credentials, where a `401`
from one bearer beside a timeout from another is ordinary internet — labelling
that a wedged runner would be a confident verdict about a machine nobody
measured. The stated cost: a runner reached over a non-loopback address is never
reported wedged. That is the safe direction — a missed wedge costs a diagnosis,
a fabricated one costs a live peer's key.

**Why this matters more than a nicer error message.** The default reading of
that leading `401` is `COORD_MCP_PROXY_UNAUTHORIZED`, whose remedy is to
re-provision the proxy key — **which evicts a live peer's key** (this skill's
own one-slot warning). So during a wedge the diagnostic actively makes things
worse: it breaks a working peer session to fix a key that was never broken.
That is a false *dead-key* verdict, the mirror image of the false *live*
verdict the coord-mcp corpus already tracks as F1.

**On `RUNNER_WEDGED`, the correct moves are:**

1. **Do not re-provision or rotate any proxy key.** The key is fine.
2. **Do not restart or kill the runner** — served policy
   (`production-and-cost` `runner-lifecycle`), and a restart destroys
   in-flight sessions and the wedge evidence with them.
3. **Probe `/livez`** on the runner port. It is dependency-free, so it
   discriminates "one handler is stuck" from "the whole runtime is starved".
   A `404` means the runner predates that endpoint — inconclusive, not
   healthy.
4. **Treat every in-flight coord write as LOST.** The coord-mcp proxy is
   served *by* the runner, so a wedge takes coord-mcp down for every session
   on the box at once. Verify by independent read and re-issue — the standard
   no-output-write doctrine, for a cause the doctrine did not previously name.
5. **Expect self-recovery.** Observed wedges have resolved without
   intervention. Waiting is a legitimate move; re-provisioning is not.

Additional typed verdicts: `HTTP_200_NOT_MCP` (HTTP 200 without a JSON-RPC
result — a broken door, never LIVE), `UNREACHABLE` / `HTTP_<code>`
(unclassified), and L3-specific causes `NO_TOKEN` / `MINT_FAILED` /
`HELPER_NOT_FOUND` / `HELPER_DEPS_MISSING` (mapped from
`coord-acting-bearer.sh`'s exit codes). Every non-LIVE verdict carries curl's
own one-line explanation as `[curl: …]`.

**LOCAL faults are named as local and never dressed up as a coord verdict** —
`AUTH_HEADER_STAGING_FAILED` (the script could not write its own header file)
and `AUTH_HEADER_UNREADABLE` (curl could not open it). Both mean *this script's
plumbing broke* and say nothing about coord — re-run, and if it repeats check
`$TMPDIR`. The script also refuses to probe a door with an empty header file:
that would draw a 401 and get misreported as a stale proxy key on a door that
is fine. A diagnostic that blames the remote for its own broken plumbing is
worse than no diagnostic.

On success it prints `VERDICT: <live-verdict> door=<file> url=<url>
transport=...`, where the verdict is one of **three live-class names** — `LIVE`,
`LIVE_APP_ERROR` or `PROXY_LIVE_E2E_UNVERIFIED` (the two sections below) — and
never a bare `LIVE` the run has not earned. Exit `0` means the same thing it
always did: a door works. Then
re-issue your lost call as a raw JSON-RPC `tools/call` against that door (for a
loopback door, the proxy nonce from that file — carried as
`X-Coord-Mcp-Proxy-Key: <nonce>` on older configs and as
`Authorization: Bearer <nonce>` on ones written after the header move; the
script reports which header name it used. The minted bearer for L3), then
verify by read. On total failure it prints `VERDICT: DEAD` naming
every exhausted door and its typed reason; run `coord doctor` next.

### A `tools/list` green is NOT an end-to-end green

`tools/list` is answered by the **proxy**. It proves the local hop, the nonce and
the JSON-RPC framing, and it says **nothing whatever** about whether coord is
behind them. A door can list its whole tool surface and then `502` on every
actual call — the F2 class — and a `tools/list`-only cascade reports that as a
clean `LIVE`.

So every probe now runs a **second, end-to-end step**: `tools/call
coord_query_identity {}` over the same door. The tool choice is measured, not
incidental — zero required arguments (so `{}` is a valid call rather than a
validation error), a cheap read (~0.34 s), and present in both the device tool
set and the runner's proxy allowlist, so it is callable over every rung the
cascade probes. `coord_can` was the obvious candidate and is **wrong**: with `{}`
it answers `isError:true`, which would make every healthy door report
`LIVE_APP_ERROR` and teach readers to ignore that verdict.

- **`LIVE`** — the end-to-end call answered `isError:false`. Proven end to end.
- **`LIVE_APP_ERROR`** — it answered `isError:true`. Still proven; see below.
- **`PROXY_LIVE_E2E_UNVERIFIED`** — the end-to-end step did not run
  (`$COORD_REVIVE_E2E=0`). Live as far as it was measured, and it says so
  instead of overstating. **Never a bare `LIVE`.**
- **`PROXY_LIVE_UPSTREAM_DEAD`** — the proxy answered `502` with a typed
  upstream code. Re-probed **once**; a second `502` is final. This one is *not*
  live-class: the proxy is up and no call can be carried over it.

All three live-class verdicts take the same exit-0 path. That is stated here
because it is the trap: the script's own `probe_door()` had a single
`[ "$verdict" = "LIVE" ]` comparison, and adding verdicts without widening it
would have dropped two of the three on the floor and reported `DEAD` **over a
door that had just answered** — the exact false-`DEAD` class this whole skill
exists to prevent, reintroduced by the change meant to sharpen it.

### `isError` is the TOOL's verdict, never the DOOR's

**Rule: an MCP tool answering `isError: true` is evidence the transport WORKS.**
The door carried the call, coord ran the tool, and the tool declined — on its
arguments, on this principal's authority, or on its own state. Reading that as a
dead door is a category error, and it is a *tempting* one: the obvious way to
"harden" a liveness check is to stop counting a failed response as a result.

Do not do it, in this script or anywhere else that classifies a coord door. The
classifier requires a well-formed envelope (an object carrying `content` **and**
`isError == true`) precisely so a tool whose own *data* happens to contain a
field named `isError` cannot be mistaken for one — and it maps the match to
`LIVE_APP_ERROR`, a **live** verdict, never to a dead one.

The same rule has a caller-side half: `/pr-status` used to classify such a
response `LIVE` and then print `result.content[0].text` — the tool's **error
message** — on stdout, where its caller renders PR status cards. A status
surface must never render an error string as if it were a card. It now reports
"transport OK, the tool declined" and stops.

### Tool counts: measure the surface, never cite a number

**Never write down how many tools a coord door exposes.** Sampled on this
tenant, the same door's `tools/list` has returned **50, then 56, then 58, then
59** across four samplings — the surface grows with every shipped tool, and a
number written into a document is wrong within days while reading like a fact.

A count is not evidence of anything anyway: a door listing "the right number" of
tools can still `502` on every call (see the F2 section above), and a door
listing fewer than you expected is reporting the **running build's** allowlist,
not a boundary. So when the count matters — proving a tool is reachable, or
diagnosing a `-32601` — **measure it, at the moment you need it**:

```bash
bash <path-to-this-skill-dir>/coord-revive.sh tools
```

and say what you measured, when. Cite the measurement, never a remembered
number.

### `LIVE` can be `PARTIAL` — a device-JWT door does NOT carry fleet authority

**If the verdict line ends in `PARTIAL`, the door works but its *reach* is
limited, and the `PARTIAL:` lines under it say how.** `L1`/`L2`/`L3` print an
unqualified `LIVE`; the L4 device-JWT doors and L5 qualify it.

For the **runner mint** the limit is measured, not hypothetical. Get the
terminology right, because it is what makes the caveat land: L4's mint does
**not** obtain a "coord device JWT" the way L3 obtains an acting bearer.
`get_access_token_for_websocket` returns the runner's **Cognito access token**
(`qontinui-runner/src-tauri/src/commands/auth.rs` →
`AuthManager::get_access_token()`). coord accepts it as a bearer, but it
authenticates as **the operator's own Cognito user and tenant** — not as a fleet
service identity. That is precisely why the fleet's `canonical_repos` authority
rows are absent over it: they are not this tenant's rows. The door is real; the
**identity** is different.

Measured 2026-08-13 over a live L4 mint door:

| Call class | Result over the L4 door |
|---|---|
| `coord_query_merge_economics` (tenant-scoped **authority** read) | `"qontinui-<repo> is not in your tenant's coord authority (canonical_repos tenant/global rows ∪ tenant_repos) — no economics computed"` — for **all six** fleet repos |
| `coord_pr_status` (**path-keyed** read) | normal |
| `POST /pr-merge/prs/qontinui/qontinui-web/956/reevaluate` (**path-keyed**) | normal — `refreshed_from_github: true` |

**The adjudication rule, and it is the whole point of the marker: a vacuous or
empty authority answer over this door is UNKNOWN, NEVER ZERO.** An agent that
reads `no economics computed` as "no merge activity" draws exactly the wrong
conclusion — it will report a healthy train as idle, or an idle one as fine.
Re-ask over a door that *has* fleet authority, or say UNKNOWN. This is the same
rule as served policy `verification-and-evidence` `silent-empty-is-unknown`, and
the same class as memory
`reference_coord_query_metric_follower_zero_is_vacuous_for_leader_gated_counters`.

For the two **static** sources the `PARTIAL` block is deliberately weaker and
says so: a token in `$COORD_DEVICE_JWT` or `~/.qontinui/coord-device-jwt` may be
a genuine coord-issued device JWT with fleet authority, or a copy of the same
operator-tenant token — the script cannot tell from the bearer alone and does
not guess. Check the answer itself before trusting any authority read over it;
the UNKNOWN-never-zero rule applies unchanged.

**L5's `PARTIAL` block says something different again, and it is about PROVENANCE
rather than reach — starting with the fact that its `url=` is the MINT, not a
door to re-issue a write over.** This rung hands back a *bearer*; spend it on
`$COORD_HTTP_URL/mcp` (coord tools by name) or on the device-authed hand-written
`/coord/…` REST routes, which `/gate`'s **write-forwarder REST** rung spells
out. It is **not** carried
onto `$COORD_HTTP_URL/mcp`: that door's device-JWT-only constraint is unchanged. The bootstrap token is an **agent** principal minted against
a device UUID, not a device principal — so it is scoped by whatever coord grants
an agent in this tenant, and this script asserts nothing beyond what its control
read measured: that the bearer authenticates. It verified `GET
/coord/agent-findings`; it did **not** probe `$COORD_HTTP_URL/mcp`, so a `401` or
a `-32601` there is that door's own verdict, not a refutation of L5. And it is a
**short-lived, over-broad** credential (~4h, carrying scopes far wider than any
one recovery write) obtained from a route whose exposure is an open operator
decision — use it for the write you came for and discard it.

A bare `LIVE` that overstates its own reach is the same defect as a false
`DEAD`, one level down: `DEAD` was made falsifiable by L4 and by the `SCOPE:`
epilogue, while nothing cross-checked a `LIVE` verdict against what that door
can actually *do*. The `PARTIAL` marker is the reach half of the same honesty
property the failure half already enforced.

### `DEAD` is scoped: it never probes your NATIVE tools

L4 closed the missing-*door* gap. This is the separate, remaining one — what
`DEAD` actually licenses you to conclude.

**`DEAD` means "no OUT-OF-BAND door". It is NOT by itself proof that coord is
unreachable.** Every rung here probes a loopback proxy or an HTTPS bearer; none
of them touches the session's own `coord_*` MCP tools, which are a *different
transport*. The two genuinely disagree. On a **stdio-configured** session (root
`.mcp.json` is `{"mcpServers":{"coord":{"command":…}}}`) the whole `.mcp.json`
proxy family can be dead — L1 structurally cannot match, every sibling nonce
evicted by a runner restart, no `$COORD_AGENT_JWT` — while the native tools
answer normally, because they never went through any of it. Observed 2026-08-08:
`VERDICT: DEAD` and a successful `coord_gate_inspect` in the same minute.

So `DEAD` is honest blocked-evidence only *together with* a failed native call.
Before applying the lost-write doctrine, issue one cheap native read
(`coord_gate_inspect` on any known gate_id). If it answers, coord is REACHABLE —
re-issue over the native tools and verify by read, rather than presuming the
write lost. This is the same principle L4 was added for: a false `DEAD` is the
worst thing this script can emit, and "no door" reported as "no coord" is a false
`DEAD` reached by inference instead of by a missing rung.

A corollary worth stating outright, because it is the most common reason someone
runs this skill: **`/coord-revive` can never restore native coord-mcp.** The MCP
client reads `.mcp.json` once at launch and never re-reads, so no in-band action
reconnects it. This skill finds a door to keep working *through*; it does not
repair the client's own transport.

### `/mcp` → Reconnect is NOT the recovery — it makes things worse

**Do not tell the operator to run `/mcp` and reconnect.** That instruction stood
here (and in served policy `coordination` →
`mcp-reconnect-is-not-agent-invocable`) until it was measured, and the
measurement falsified it. Spike `2026-08-20-coord-mcp-dcr-phase1-spike`
(client **2.1.236**, re-confirmed at **2.1.237** on 2026-08-20) reproduced the
operator's 2026-08-19 failure byte-for-byte:

1. The runner's dead-key rejection is a **bare `401` with no `WWW-Authenticate`
   and no protected-resource metadata**, and the `.mcp.json` carries a *custom*
   header (`X-Coord-Mcp-Proxy-Key`), which makes the client attach an OAuth auth
   provider. A `401` therefore does not fail — it **escalates**.
2. The client probes four discovery URLs
   (`/.well-known/oauth-protected-resource/mcp`,
   `/.well-known/oauth-protected-resource`,
   `/.well-known/oauth-authorization-server`,
   `/.well-known/openid-configuration`), finds nothing, and falls back to
   **RFC 7591 Dynamic Client Registration against the URL origin**:
   `POST http://127.0.0.1:<port>/register`.
3. The runner's axum router has no `/register`, so its fallback answers
   **`404 {"success":false,"error":"No route for POST /register","code":"NOT_FOUND"}`** —
   which is exactly the string the operator saw.

So a reconnect from a stale-credential `401` terminates in a DCR 404, not in a
working transport. Two further measured facts close the door:

- **The client's automatic in-process reconnect does not re-read `.mcp.json`.**
  Measured end-to-end: with the config file rewritten on disk to a *fresh valid*
  key between turns, the very next request still carried the **old** key. A
  runner restart therefore orphans a running session permanently.
- **A failed DCR persists an `mcpOAuth` entry** into
  `<CLAUDE_CONFIG_DIR>/.credentials.json`, keyed
  `<serverName>|sha256({type,url,headers}).slice(0,16)`. **Today's nonce lives
  *inside* that hashed `headers` map**, so every nonce rotation mints a
  *brand-new* entry against the same server. The 17 live `coord-mcp|<hash>`
  entries are therefore **one unbounded accumulator, not 17 separate
  incidents** — and nothing prunes it.

**UNVERIFIED, and deliberately not asserted either way:** whether the operator's
manual `/mcp` → **Reconnect** *button* re-reads `.mcp.json` from disk. It is not
drivable non-interactively (`claude mcp list` rejects `--mcp-config`; `-p "/mcp"`
exposes no Reconnect action; `winpty` aborts with no console), and a
SendKeys-driven console was declined — this box runs ~9 concurrent sessions and a
mis-targeted `AppActivate` types into a live one. Everything above is measured
about the *automatic* reconnect and about the 401→DCR escalation, which the
manual button shares. Do not upgrade this to "Reconnect works".

### What actually recovers a session

In priority order, and none of them is `/mcp`:

1. **Keep working through a door.** Run this skill; re-issue over whatever it
   reports `LIVE` and verify by read. This is the in-session answer.
2. **Start a new session for native coord-mcp.** The client reads `.mcp.json`
   exactly once, at launch, and the runner writes a fresh proxy config on every
   session/agent/terminal spawn (`coord_mcp.rs` →
   `provision_coord_mcp_for_session`). A new session is therefore the only thing
   that puts a *live* key into a *live* client. **Never restart the runner to
   force this** — served policy `production-and-cost` `runner-lifecycle`.
3. **Only if the client reports the server as `needs-auth` and sends it zero
   requests** — i.e. a poisoned credential cache — clear it with
   **`claude mcp logout coord-mcp`**. Verified 2026-08-20 at client 2.1.237
   against a throwaway server in an isolated `CLAUDE_CONFIG_DIR`: it deletes the
   `mcpOAuth` entry and the *next* launch connects on the first attempt, where
   the run before it sent the healthy server **no requests at all**. Three
   limits, all measured: it removes **only the entry whose hash matches the
   header currently in the config**, so the accumulator's earlier-nonce siblings
   survive it; it does **not** touch `mcp-needs-auth-cache.json`; and it refuses
   a server supplied by `--mcp-config` (`No MCP server named "…"`), so it only
   reaches servers registered in a config the CLI manages.

**When does the poisoned cache actually bite?** Measured at 2.1.237, the
connection is skipped only when **all** of these hold: the server is
`type: http`/`sse`; its config carries **no `headers` and no `headersHelper`**;
and the stored entry has no token and `discoveryState.oauthMetadataFound: true`.
A coord-mcp proxy config always carries `headers` (`X-Coord-Mcp-Proxy-Key`, or
`Authorization: Bearer <nonce>` after the header move), and the runner serves no
OAuth metadata — so at **2.1.237** the 17 stale entries are **inert**, verified
by a control run that connected normally with a poisoned entry in place. That is
a **version-pinned** result, exactly like the `headersHelper` negative before it:
the gate is client-internal and can change in any release. Step 3 stays in the
list because the *symptom* — `needs-auth` with zero outbound requests — is
observable, and it is the only thing that clears it.

## Hard bounds and guarantees

- **Self-bound:** max 2 attempts per stage, and the second fires only on a
  retry-safe verdict — `CREDENTIAL_REFRESHING`, `TIMEOUT` since 2026-09-02
  (a curl exit 28 says nothing about the door, only that this box got no answer
  inside the budget), or `PROXY_LIVE_UPSTREAM_DEAD` since 2026-09-05. There are
  now two stages (`tools/list`, then the end-to-end `tools/call`), and the second
  **suppresses the outer retry** once it has run, so a door costs at most **3
  requests**: 2 `tools/list`, or 1 `tools/list` + 2 `tools/call`. Worst case per
  timing-out door is `2 × PROBE_TIMEOUT + 3s` = 33s at the default. Note the dedup added with it
  collapses only LOOPBACK candidates: L3 and the L4 bearer sources all probe the
  same `${COORD_URL}/mcp`, but under DIFFERENT credentials, so they carry
  distinct signatures and each still pays its own budget. The 2026-08-08 shape
  described above — 14 doors with the keys ROTATED under all of them — is
  precisely the case nothing dedups. No loops, no polling —
  coord has no auth-path rate limiting, so the bounding lives client-side.
- **Four time budgets, not one — and all four are env-overridable.** Three bound
  ONE call; the fourth bounds the whole sweep. A cheap
  loopback probe and a WebView-driven mint are orders of magnitude apart, so
  spending one number on both is what produced a `VERDICT: DEAD` over a healthy
  credential (fixed 2026-08-31, #547):

  | Variable | Default | What it bounds |
  |---|---|---|
  | `COORD_REVIVE_CONNECT_TIMEOUT` | 5s | TCP connect only, every rung |
  | `COORD_REVIVE_PROBE_TIMEOUT` | 15s | one `tools/list` or end-to-end `tools/call` probe, start to finish — L1/L2 loopback and the L3/L4 bearer probes against `$COORD_HTTP_URL/mcp` |
  | `COORD_REVIVE_MINT_TIMEOUT` | 60s | the L4 source-4 WebView eval mint, and nothing else |
  | `COORD_REVIVE_TOTAL_BUDGET` | 60s | **the whole sweep.** Sampled between doors (never mid-request — killing a probe in flight would leave a door with no verdict at all, which is the silence this script replaces) |

  Raise `COORD_REVIVE_MINT_TIMEOUT` on a saturated box; raise
  `COORD_REVIVE_PROBE_TIMEOUT` only if you mean to make a dozen-door sweep that
  much slower. **A trip on any of the first three is reported AS a timeout**,
  with the budget that expired named in the verdict string — never as a
  credential verdict, which is the whole point of splitting them.

  A trip on the **fourth** is reported as `VERDICT: BUDGET_EXCEEDED`, naming the
  doors probed and the doors skipped — **never as `DEAD`**, because `DEAD`
  asserts every door was probed and a budget trip means some were not. The
  skipped doors appear in the same list as `SKIPPED_BUDGET_EXCEEDED`. Stated
  honestly: the bound is the budget **plus at most one door's worst case**, since
  a door already in flight when the budget runs out is allowed to finish rather
  than be cut off mid-request.
- **The correlated case is short-circuited, and that is the real fix — the budget
  is only the backstop.** Every canonical door on this fleet now resolves to the
  **same runner port**, so a device-JWT refresh makes all ~12 answer
  `CREDENTIAL_REFRESHING` together, each paying `15 + 3 + 15 ≈ 33s` for a fact
  the first one already established — ~400 s to learn one thing. Once a door
  **settles** on `CREDENTIAL_REFRESHING` (still says so *after* its own retry),
  siblings resolving to that `host:port` are reported
  `SKIPPED_SHARED_UPSTREAM_REFRESHING` instead of re-probed. "Settled" and not
  "once" is deliberate: a door that says `503` and then something else on the
  retry is not a withholding proxy, it is a **wedge**, and a wedge must not be
  collapsed into one skipped verdict — its evidence *is* the mixed set.
  `$COORD_REVIVE_NO_UPSTREAM_SKIP=1` probes every sibling anyway.
- **`$COORD_REVIVE_E2E=0` turns the end-to-end probe off**, and that choice is
  reported as one: the verdict becomes `PROXY_LIVE_E2E_UNVERIFIED`, never an
  unearned `LIVE`.
- **Read-only:** the script only READS `.mcp.json` files; it never writes any
  config — including the one the L4 mint returns, which is used in memory and
  never persisted.
- **Mints only at L4, and only for this workdir.** This bullet used to read
  "never mints", because minting re-provisions the ONE-SLOT workdir key and
  would evict a live peer's — the exact failure class this skill exists to
  recover from. It is now **narrowed, not dropped**, and the narrowing is what
  keeps it safe: L4 source 3 is reachable only after L1 (this workdir's own
  `.mcp.json`) and L2 (every sibling) have each been probed and found **dead**,
  and it mints for `$PWD` — the very workdir whose key L1 just measured dead, so
  there is no live slot here left to evict. Sibling workdirs are untouched (the
  slot is per-cwd). `COORD_REVIVE_NO_MINT=1` restores the absolute guarantee.
  It was narrowed because the alternative was worse: on a **headless** runner
  the UI-Bridge mint cannot answer at all, so a box holding a live device
  credential with no sibling `.mcp.json` lying around had no door whatsoever and
  this skill printed `DEAD` — confidently wrong. What the mint hands back is a
  **nonce**, never a bearer: a local capability token paired to this runner's
  own bound port, with the runner injecting a freshly-read device JWT per
  forwarded request. (Plan
  `2026-08-24-headless-box-has-no-working-coord-credential-door`.)
- **L5 mints too — from COORD, not from the runner, and it evicts nothing.**
  The one-slot warning above is a property of the runner's per-cwd proxy key and
  has no analogue here: `POST /agents/credential` issues a fresh token and
  disturbs no peer session's credential. What it does have is the opposite
  hazard — the token is a real coord credential in your hands, so it never
  touches disk, never reaches argv, and is discarded after the write. L5 runs
  only after L1–L4 have each been probed and failed; `COORD_REVIVE_NO_BOOTSTRAP=1`
  turns it off. The script probes the dedicated credential route and **nothing
  else** — it never substitutes `/agents/allocate`, which three shipped documents
  forbid and which is the subject of open operator ruling
  `ece99898-30c6-4f8c-be8e-1de5f09abebc`.
- **Reaching L5 is COUNTED, and the counter is local.** Two `guard_decide`
  records per run into `~/.qontinui/logs/guard-decisions.log` (see the L5
  section's table). A counter that had to reach coord would be missing in
  precisely the outage it exists to measure, which is why it is a breadcrumb and
  not a finding.
- **A headless runner is named as such, never as "signed out".** A
  `page/evaluate` timeout used to land in the same bucket as a signed-out
  runner. L4's `RUNNER_HEADLESS` arm keys on `/health.frontendReady` — a fact
  the runner states — and deliberately not on the timeout string, which cannot
  tell a WebView-less runner apart from one that is merely slow to boot its
  WebView. A `/health` probe that failed leaves the verdict `unknown`, and
  `unknown` never suppresses the mint. Since 1f the arm is scoped to the eval
  **fallback**: the in-process invoke door is tried on every runner first,
  headless or not, and only a build that lacks the entry reaches the arm.
- **The `call` / `tools` verbs never mint, and never print the nonce.** They
  ride the caller's own `.mcp.json` key only; a missing one is
  `NO_PROXY_CONFIG`, a rejected one is a 401 with its recovery named — never a
  `/coord-mcp/provision-session` mint, which would evict the live peer holding
  that workdir slot.
- **Never prints key material** — file paths and URLs only. Nothing that reads a
  credential ever echoes it: the L4 shape checks report only *that* the value
  was not JWT-shaped, never the value, and the response reader that names an
  evaluate failure reads error fields exclusively, never the token fields
  (`.data.value` / `.data.result.value`).
- **A qualified `LIVE` always names its limit** — `LIVE … PARTIAL` is never
  emitted without `PARTIAL:` lines saying what the door cannot do. That is the
  reach-side half of the script's own named-cause invariant ("every path returns
  a NAMED cause"); a `PARTIAL` with no reason would be the client's mask wearing
  a success label.
