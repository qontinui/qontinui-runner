---
name: coord-revive
description: Recover a dead coord-mcp transport with a TYPED verdict instead of the client's "Command failed with no output" mask. Runs the door cascade (L1 re-read own .mcp.json key, L2 sibling-key sweep, L3 acting-bearer fallback, L4 device-JWT bearer from $COORD_DEVICE_JWT / ~/.qontinui/coord-device-jwt / a runner mint), reports which door is LIVE and whether that LIVE is PARTIAL, and enforces the lost-write doctrine — a "no output" coord write is presumed LOST; re-issue over the live door and verify by read.
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
| L4 | **Device-JWT bearer**, three sources in the fleet's documented order — `$COORD_DEVICE_JWT`, then `~/.qontinui/coord-device-jwt`, then a **mint** from the runner's UI Bridge — against the same public coord MCP door | Independent of BOTH: none of them cares that every proxy key rotated, and none needs `$COORD_AGENT_JWT` (unset on this fleet) |

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
| `TIMEOUT` | Port answers, request never returns | Do not hammer it; move down the cascade |
| `NO_RUNNER` | L4 mint: nothing answered at that origin (connection refused, or no status at all) | Runner down, moved, or never started; set `$QONTINUI_RUNNER_URL` |
| `RUNNER_TIMEOUT` | L4 mint: the port **accepted** the connection but produced no response in time | Often **saturation**, not a dead runner — do NOT restart it on this alone (served policy `production-and-cost` `runner-lifecycle`). Re-run, or use another door |
| `RUNNER_EVAL_FAILED` | L4 mint: the UI Bridge **answered**, but not with a well-formed evaluate result — a non-2xx, a route-absent 404, or a `success:false` body. The verdict quotes the response's own error string **when the body carried one** | **Not** a sign-in problem. Read the quoted error. A 4xx means the route moved or something else answers on that port; a 5xx means the route is present and failed server-side |
| `RUNNER_TIER_TOO_LOW` | L4 mint: the runner is **Tier 0/1** (`Local` / `LocalProvider`), where the Qontinui account commands do not exist at all | Change the runner's tier (Settings → Account), or use another door. The runner is **not** signed out and signing in will not help |
| `RUNNER_TIER_UNKNOWN` | L4 mint: the runner could not resolve its own tier — a corrupt or unreadable `settings.json`. Its account state is unchanged | Repair `settings.json`. A sign-in CTA here is the exact mistake the runner's own `NO-DOWNGRADE (C4)` comment records |
| `RUNNER_SIGNED_OUT` | L4 mint: it answered and genuinely holds no token — either a non-JWT-shaped value, or its own `Not authenticated` error | Sign the runner in. The shape check fires before the token is ever sent, so this is never reported as coord rejecting you |
| `ENV_UNSET` / `FILE_ABSENT` | L4: that static credential source is simply not present — a statement of **absence**, not a fault | Nothing to do unless you meant to provide one; the cascade moves to the next source |
| `HOME_UNRESOLVED` | L4: neither `$HOME` nor `$USERPROFILE` is set, so source 2 has no path to read | A **local** environment fault; it says nothing about whether the credential exists |
| `DEVICE_JWT_ENV_MALFORMED` | L4: `$COORD_DEVICE_JWT` is set but is not JWT-shaped (3 dot-separated base64url parts) | Not sent — an unshaped bearer would draw a 401 this script would then blame on coord. Fix or unset the variable |
| `DEVICE_JWT_FILE_MALFORMED` | L4: `~/.qontinui/coord-device-jwt` is readable but its contents are not JWT-shaped — a whole JSON response left in the file fails here too, by design | Same treatment — not sent. Re-mint the file, or delete it |
| `DEVICE_JWT_UNAUTHORIZED` | L4: coord rejected a device JWT | Expired, or bound to another tenant. From a **static** source this is expected and **not terminal** — the cascade falls through to the next source |

**Why the last five rows exist: `RUNNER_SIGNED_OUT` used to absorb all of them.**
The response reader returns the **empty string** for every body it cannot parse
into `.data.result.value`, and an empty string reads at the call site as a
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

Every run now reads **`.coord-mcp-status`** in your cwd and reports it — as a
`BREADCRUMB:` line on stderr before L1, and again inside the `DEAD` verdict
block so a pasted verdict carries it.

That file is the **runner's** one-line record that it could not give this
workdir a working coord-mcp (`coord_mcp.rs`, `write_degraded_breadcrumb`;
`/gate`, `/policy` and `qontinui-pr` read it too — none of them writes it). It
matters here because **four of its five reasons say that provisioning pass wrote
no `.mcp.json`**: no device JWT in the runner's access_token slot, an
unresolvable bound API port (device or agent path), or an agent JWT with no
parseable `sub`. In those cases L1 reporting nothing in your own cwd is not a
second mystery to chase — it is the documented consequence of a fault the runner
already diagnosed.

**That is "this pass wrote none", not "there is no config".**
`coord_mcp_safe_to_write` passes a workdir whose file is absent *or* holds only
our own `coord-mcp` config, and each refusal arm returns after that check — so a
re-provision leaves an earlier, stale `.mcp.json` sitting there. L1 probing it
into a `CONNECT_REFUSED` or a `COORD_MCP_PROXY_UNAUTHORIZED` while the
breadcrumb says "NOT written" is a consistent pair, not a contradiction.

The fifth reason (`port :N probe failed`) is the opposite: a `.mcp.json` WAS
written and did not answer 2xx at spawn. **Do not read its parenthetical
(`dead port | 401 stale nonce | coord down`) as a diagnosis** — and note that it
is short a disjunct. The probe carries a **3-second** timeout and collapses
every transport error to "not reachable", so *the runner was merely slow* is a
fourth cause it never names, on a box where CLAUDE.md records `:9876/health`
sampled between 296 ms and 10120 ms. On 2026-08-20 the string named a dead port
while `:9876` answered `/health` `derived_status healthy` with 59 live terminals
in the same session (plan
`2026-08-20-worktree-spawn-autonomy-and-trust-preconditions`, finding 18 — whose
"typed cause, never a disjunction" demand is a constraint on that plan's OWN
Phase-1 instrument; nothing on any phase rewrites this string). Which cause it
actually was is exactly what the typed verdicts below settle, which is the
cascade's whole job.

**It never changes the verdict, and it is not a probe.** Three limits, all
stated in the output rather than left for the reader to infer:

- **Presence is spawn-time.** The runner clears it only on a successful probe
  *during provisioning*, so a session that recovered on its own keeps a stale
  file and a session that lost coord later grows no new one. (A re-provision of
  the same workdir — a second terminal, a looping agent — can clear or rewrite
  it.)
- **Absence is UNKNOWN, never health.** A healthy provision writes nothing —
  and so does a hand-typed session, a workdir the runner never provisioned, a
  bearer whose `sub_type` is neither device nor agent, a secondary-instance
  write refusal, and a runner build predating the breadcrumb. The absent case is
  reported in those words for that reason; "no breadcrumb, so coord-mcp was fine
  at spawn" is the silent-empty-is-unknown error this script exists to stop
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
| `TIMEOUT` and/or `CONNECT_REFUSED` **interleaved** with `401`/`200`/`503` on one port | **`RUNNER_WEDGED`** | Do NOT re-provision the key. See below. |

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

On success it prints `VERDICT: LIVE door=<file> url=<url> transport=...` —
re-issue your lost call as a raw JSON-RPC `tools/call` against that door (for a
loopback door, the proxy nonce from that file — carried as
`X-Coord-Mcp-Proxy-Key: <nonce>` on older configs and as
`Authorization: Bearer <nonce>` on ones written after the header move; the
script reports which header name it used. The minted bearer for L3), then
verify by read. On total failure it prints `VERDICT: DEAD` naming
every exhausted door and its typed reason; run `coord doctor` next.

### `LIVE` can be `PARTIAL` — a device-JWT door does NOT carry fleet authority

**If the verdict line ends in `PARTIAL`, the door works but its *reach* is
limited, and the `PARTIAL:` lines under it say how.** `L1`/`L2`/`L3` print an
unqualified `LIVE`; only the L4 device-JWT doors qualify it.

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

- **Self-bound:** max 2 probes per door, and the second fires only on
  `CREDENTIAL_REFRESHING` (the one retry-safe verdict). No loops, no polling —
  coord has no auth-path rate limiting, so the bounding lives client-side.
- **Read-only:** the script only READS `.mcp.json` files; it never writes any
  config.
- **Never mints:** it NEVER calls `/coord-mcp/provision-session` — minting
  re-provisions the one-slot workdir key and evicts the live peer's key, the
  exact failure class this skill exists to recover from.
- **Never prints key material** — file paths and URLs only. Nothing that reads a
  credential ever echoes it: the L4 shape checks report only *that* the value
  was not JWT-shaped, never the value, and the response reader that names an
  evaluate failure reads error fields exclusively, never `.data.result.value`.
- **A qualified `LIVE` always names its limit** — `LIVE … PARTIAL` is never
  emitted without `PARTIAL:` lines saying what the door cannot do. That is the
  reach-side half of the script's own named-cause invariant ("every path returns
  a NAMED cause"); a `PARTIAL` with no reason would be the client's mask wearing
  a success label.
