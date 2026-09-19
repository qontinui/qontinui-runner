# Manual Testing via UI Bridge

Perform hands-on manual testing of the application using the UI Bridge SDK. **Work completely autonomously — never ask the user for input, never ask the user to restart or rebuild anything, never report that something "needs a rebuild" and stop.**

**CRITICAL: Never restart or kill active (non-temp) runners.** The primary runner and any named/protected runners must not be stopped. Always spawn a **temporary test runner** via the supervisor API (port 9875) for runner testing. The temp runner is built with the latest code, runs on a separate port, and is automatically cleaned up when stopped.

**CRITICAL: The supervisor (port 9875) is INDEPENDENT of the primary runner (port 9876).** The supervisor has its own parallel build pool and spawns temp runners directly — it does NOT depend on the primary runner being up, healthy, or recently rebuilt. You NEVER need to rebuild the primary runner to test code changes. Always use the supervisor's `POST /runners/spawn-test` instead. The supervisor builds from source into its own target directories (`target-pool/slot-{0,1,2}/`) and copies the binary for each temp runner.

**CRITICAL: Edit work runs in an allocated worktree, never the primary checkout.** Sibling rule to "never touch the primary runner" — same shape (don't edit the shared primary), different substrate (git worktree vs supervisor temp runner). If this skill needs to apply a fix mid-test (it usually doesn't — Phase 6 produces a Remediation Plan handed off to `/manual-test-loop`, which is the actual editing surface), allocate an isolated git worktree **through coord** — `bash <workspace-root>/qontinui-claude-config/scripts/allocate-worktree.sh --repo <repo> --intent "<what for>"`, which POSTs the literal, anonymous `https://coord.qontinui.io/agents/allocate` `{device_id, repos:[{repo}], intent}` and does the rest; by hand you would re-root the RELATIVE `worktrees[].worktree_path` under the workspace root, create it on coord's reserved `worktrees[].branch` from `worktrees[].parent_sha`, and use that as your repo root. Handle `isolation.mode` of `wait` / `shared_branch` rather than forcing a worktree. Only on HTTP 409 `repo_not_registered` (or an unreachable coord) fall back to `git -C <repo> worktree add -b <branch> <workspace-root>/<repo>-wt-<slug> origin/main`, and say in your report that the worktree is undeclared. (The HTTP `POST /agents/allocate-local` face was removed as dead code in runner #443.) A raw `git worktree add` produces a worktree coord cannot attribute, pin, or drain — see plan `2026-08-18-undeclared-worktree-exposure-and-classification`. **Why:** see `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`.

### Runner Coordination (multi-session safety)

Multiple Claude sessions share the same runner infrastructure.

> **There is no `runner_coordination/` protocol.** That directory,
> `runner_lock.py`, and `runner_status.py` do **not** exist anywhere in the
> workspace (verified 2026-07-21 by exhaustive search). There is therefore **no
> exclusive-access lock for the primary runner** — the "4-hour TTL / stale lock
> detection" this section used to promise was describing infrastructure that
> isn't there. Do not try to acquire it, and do not treat its absence as
> permission to proceed.

**Temp runners (default — and effectively mandatory):** Temp runners are
isolated by design and need no lock. Since no primary-runner lock exists, a
temp runner is the ONLY way to test safely while other sessions may be active.

**Primary runner (last resort):** If you genuinely cannot use a temp runner
(supervisor or build pool down), you cannot serialize against other sessions —
so treat the primary as shared and read-only wherever possible. Before touching
it, confirm it is idle using signals that actually exist:

```bash
# Is anything running on the primary right now?
# -fsS (not -s) so a transport error is VISIBLE and an HTTP error is a non-zero
# exit, and --max-time so it ends.
curl -fsS --max-time 25 http://127.0.0.1:9876/restart-readiness   # safe_to_restart
curl -fsS --max-time 20 http://127.0.0.1:9875/runners             # running / derived_status
```

**Not `/task-runs/running`.** It is a port-filtered *workflow task-run* ledger,
not a session census: measured 2026-08-29 it returned `[]` while 25 agent
sessions were live on the box. `/sessions/history` is DISPLAY-only history of
*closed* terminal sessions and is no better. `/restart-readiness` is the one
surface named for the question, and it reports the terminal and AI planes
separately — `POST /drain` covers only the AI one. Plan
`2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner`.

**An empty result or a non-zero exit here is UNKNOWN, not idle.** `curl -s`
swallows transport errors, so a timed-out probe prints nothing at all — which
reads exactly like an idle verdict and would tell you the runner is free when it
is busy.
`-f` closes the other half: without it curl exits **0** on a 5xx, so "read the
exit code" — the instruction this block gives — would itself report a broken
supervisor as a clean read.
That matters here more than almost anywhere: the thing you are about to treat as
idle hosts live Claude Code sessions. `127.0.0.1`, not `localhost`: the runner
binds the IPv4 loopback only while Windows resolves `localhost` to `::1` first,
and on a loaded box that extra ~2s is enough to lose the race (`/health` has
measured 296ms–10120ms), and `/restart-readiness` is heavier still — it computes
a fresh process-table pass per request. Only an explicit
`"safe_to_restart": true` is a go-ahead; a 404 means the runner's build predates
the endpoint, which is an UNKNOWN and not an idle reading.

> ⚠️ **Probe a second, independent instance before you name a cause.** Everything
> above is a *measurement* and is always reportable: "`:9876/restart-readiness` timed
> out", "the supervisor answered 404", "nothing is listening on 9876". The moment you
> go one step further — *the runner is down*, *this build predates the endpoint*, *the
> UI Bridge is not served* — you owe a **second, independent instance** of that door
> first, because every one of those sentences is a claim about a BINARY and you have
> polled exactly one.
>
> Here the second instance is a **temp runner from the supervisor**, never the primary
> on `:9876` — which is the move this file already prescribes, and it costs a build
> slot rather than a restart:
>
> ```bash
> curl -fsS --max-time 20 http://127.0.0.1:9875/runners     # every runner, not just yours
> curl -fsS --max-time 25 http://127.0.0.1:$PORT/health     # the temp runner's own build
> ```
>
> A temp runner built from current code that DOES serve the route settles the question
> against the *binary* rather than the *code*, and that changes the remedy completely:
> wait for the operator's next primary restart, instead of writing a fix for an absence
> that does not exist. A `401` or a `404` from that second instance still proves the
> route is **served**, which is exactly the claim a local timeout was about to be used
> to deny. **Never stop, restart or rebuild the primary to settle this** (CLAUDE.md →
> "Runner lifecycle") — the temp runner exists so you never have to.
>
> **Where coord is reachable, ask what has already been diagnosed:**
> `coord_recent_findings` for the `resource_keys` you are about to touch, or the
> `topic` when you know the subsystem before you know the files. `coord.findings` is
> pull-by-relevance, so nothing pushes a peer's diagnosis at you. Masked tool or dead
> transport: `GET /coord/agent-findings?resource_keys=…&topic=…&limit=…`.
> ⚠️ `resource_keys` and `topic` are **OR'd, not AND'd** (`f.resource_keys && $2 OR
> ($3 IS NOT NULL AND f.topic = $3)`, qontinui-coord `crates/coord/src/findings.rs`),
> so passing both **widens** the read.
>
> **If the second instance cannot be reached either, that is UNKNOWN** — not
> confirmation of the local mechanism. Two silent doors are two silent doors: say
> UNKNOWN and name both probes you ran. Measured 2026-09-01: a session called a door
> dead for ~5h45m on a single local probe while it was live, with a correctly-keyed
> finding already six hours old in `coord.findings` (plan
> `2026-08-28-probe-first-belongs-in-the-diagnosing-commands`).

```powershell
Get-Process -Name 'qontinui-runner*' -ErrorAction SilentlyContinue
Get-NetTCPConnection -State Listen | ? { $_.LocalPort -eq 9876 }   # locale-independent
```

`/restart-readiness` must report `safe_to_restart: true`. A runner that is
process-absent, not listening, AND reported `offline` by the supervisor has no
sessions to lose.

**To coordinate with other sessions, use coord** — that IS the live mechanism:
`coord_declare_intent` before you start, and coord claims
(`coord_claim_acquire` / `coord_claim_check`) for exclusive resources. See the
"Coord access & gates" and "Symbol-Conflict Warning" sections of CLAUDE.md.

**Never restart the primary runner yourself** — see CLAUDE.md "Runner
Lifecycle". Only the operator may restart an active runner.

```bash
# Spawn a temp runner (builds latest code, returns port).
# The supervisor runs a parallel build pool (default N=3), so spawn-test is
# BLOCKING by default — it holds the HTTP request open until a slot frees.
curl -fsS -X POST http://localhost:9875/runners/spawn-test \
  -H "Content-Type: application/json" \
  -d '{"rebuild": true, "requester_id": "manual-test", "queue_timeout_secs": 600}'
# Returns: {"id": "test-...", "port": 9877, "api_url": "http://localhost:9877", "ui_bridge_url": "..."}

# Use the temp runner's port for all testing
RUNNER_BASE="http://localhost:$PORT/ui-bridge"

# Stop when done (auto-removed). -f so a 404 (wrong/already-dead id) is a
# non-zero exit rather than a silent leak of a build-pool slot.
curl -fsS -X POST http://localhost:9875/runners/$ID/stop
```

## Rebuilding & Restarting Non-Runner Services

**You SHOULD rebuild non-runner services when code has changed.** Only active runners are protected from restarts.

Non-runner services (backend, frontend, mobile) are **managed by the primary runner's Process Manager**. The preferred way to restart them is through the runner's Process Manager API, which ensures logs are captured and health is monitored. Secondary/temp runners can read these logs via HTTP proxy to the primary.

### Preferred: Restart via Runner Process Manager API
```bash
# Restart backend (stops, rebuilds if needed, restarts with log capture)
curl -fsS -X POST http://127.0.0.1:9876/processes/backend/restart

# Restart frontend
curl -fsS -X POST http://127.0.0.1:9876/processes/frontend/restart

# Rebuild and restart (e.g., after dependency changes)
curl -fsS -X POST http://127.0.0.1:9876/processes/backend/rebuild-and-restart

# Check process status
curl -fsS http://127.0.0.1:9876/processes/status
```

`-f` on all four for the same reason as the supervisor recipes: a restart that
returned 500 must not read as a restart that happened, or the next hour is spent
debugging the old binary. `-S` still names the status (`curl: (22) … 500`), but
`-f` DISCARDS the Process Manager's error envelope — so when you need the reason
rather than the code, swap that one call to `--fail-with-body` (curl 7.76+),
which fails the shell and keeps the body.

### Alternative: Restart via dev-start.ps1
Use this if the runner's Process Manager is unavailable:
```bash
.\dev-start.ps1 -Backend   # Restart backend only
.\dev-start.ps1 -Frontend  # Restart frontend only
# There is no -Web switch: run both switches, or use -All for the whole stack
```

### Database migrations
If new models/tables were added, run migrations before restarting:
```bash
cd qontinui-web/backend && poetry run alembic upgrade head
```

### Mobile app (Expo/React Native)
The mobile app is not managed by the runner Process Manager. Restart it directly:
```bash
cd qontinui-mobile && npx expo start --clear
# Or for type checking only:
cd qontinui-mobile && npx tsc --noEmit
```

### Runner frontend (embedded in Tauri)
The runner frontend is embedded in the Rust binary. To test frontend changes:
```bash
# Build frontend assets (required before spawning a test runner)
cd qontinui-runner && npm run build

# Then spawn a temp runner with rebuild to embed the new frontend.
# spawn-test is blocking by default — it waits for a free slot in the
# supervisor's parallel build pool (no retry loop needed).
curl -fsS -X POST http://localhost:9875/runners/spawn-test \
  -H "Content-Type: application/json" \
  -d '{"rebuild": true, "requester_id": "manual-test", "queue_timeout_secs": 600}'
```

### General approach
- **Backend/frontend:** Restart via runner Process Manager API (preferred) or dev-start.ps1 (fallback)
- **Mobile:** Restart directly with `npx expo start`
- **Runners:** NEVER restart active runners — always spawn temp runners for testing
- **Shared packages** (ui-bridge-auto, workflow-ui, etc.): Build with `npm run build` before rebuilding consumers
- **Terminal StatusStrip session buckets (working/idle/needs-input/error/completed):** drive them deterministically with the debug test seam — `POST /ui-bridge/test/seed-terminal-scenario {"working":1,"idle":2}` (atomic clear-then-seed; teardown `POST /ui-bridge/test/clear-injected`), then read the strip via DOM: `POST /ui-bridge/control/page/read-value {"selector":"[data-page-element=status-strip]"}` (NOT OCR — the vision cache serves stale reads after UI-only re-renders unless `force:true`). No real PTYs, no 60s waits; `idle` needs ≤1 sweep tick (~2s); count pills render only when `sessionCount > 1`. Contract: `qontinui-runner/src-tauri/src/mcp/test_fixtures.rs` module docs (shipped runner #420).

## Target Application

Determine the target from `$ARGUMENTS`. If no target is specified, test **both** the Runner UI and the Web frontend.

| Application | Base URL | Description |
|-------------|----------|-------------|
| **Web frontend** (Next.js) | `https://qontinui.io/api/ui-bridge` | qontinui-web frontend |
| **Runner UI** (Tauri) | `http://127.0.0.1:9876/ui-bridge` | Runner's React frontend |
| **Mobile app** (React Native) | `http://localhost:8087/ui-bridge` | qontinui-mobile — requires an active transport, see below |

```bash
WEB_BASE="https://qontinui.io/api/ui-bridge"
RUNNER_BASE="http://127.0.0.1:9876/ui-bridge"
MOBILE_BASE="http://localhost:8087/ui-bridge"
```

The rest of this runbook reads **`$BASE`**, and nothing defaults it. It is
bound **inside the arm you select below, after that arm's own setup** — never
here:

- the **temp-runner arm** binds it right after reassigning `RUNNER_BASE` to the
  spawned runner's `$TEST_PORT`. A `BASE="$RUNNER_BASE"` taken at *this* point
  would be a by-value snapshot of the line above, aimed at the **primary**
  runner — the one that arm forbids touching — while every other phase drove
  the temp one.
- the **relay arm** binds it to `$RELAY_BASE` (`${ORIGIN}/api/ui-bridge`),
  which is not one of the three defined here.
- auditing **prod web or mobile directly**, with no arm to bind it for you? Set
  `BASE="$WEB_BASE"` (or `"$MOBILE_BASE"`) yourself before Phase 3.5.

Left unbound, `curl -s -X POST $BASE/control/discover` sends no request at all:
curl exits 3, and `-s` suppresses its own complaint, so stdout *and* stderr come
back empty. **Verify by exit code, not by output** — an empty result here is
indistinguishable from a page that reported no elements.

### Arm selection — RUN THE PREDICATE, do not exercise judgement

There are three arms, and which one applies is a **fact about the box and the
target**, not a preference. Evaluate the rows top-down and take the first match.
A session that guesses here is the failure this section exists to remove: on a
box with no display and no `pwsh`, sessions used to conclude "UI cannot be
verified from here" and ship the change on unit tests alone. That conclusion is
now wrong — row 3 works there.

```bash
# One probe per precondition. Answer them BEFORE choosing an arm.
# -f matters here: this line's whole verdict IS curl's exit code, and without it
# a supervisor answering 503 is reported "up" — then the arm it selects fails.
curl -fsS --max-time 20 http://127.0.0.1:9875/runners >/dev/null 2>&1 && echo "supervisor: up" || echo "supervisor: DOWN/UNKNOWN"
command -v node >/dev/null && node -p 'process.versions.node' || echo "node: ABSENT"
```

| # | If the target is… | Arm | Needs |
|---|---|---|---|
| 1 | the **Runner UI** (Tauri) | **temp-runner arm** — `POST :9875/runners/spawn-test`, everything below unchanged. **Never the primary runner.** | supervisor on `:9875` |
| 2 | a **web/mobile page you need to DRIVE interactively over many turns** (a parked, relay-registered tab another agent can also drive) | **relay arm** — `--transport=injected`, Phase 0 → Step 1.5 | a reachable relay on the page's own origin (+ `--auth-token` on prod) |
| 3 | **any URL you need a VERDICT on** — "does my change look right?", a regression check, a headless box | **headless verdict arm** — `verify-page-verdict.sh`, below | node + a Playwright Chromium + the URL. **No display, no `pwsh`, no `docker`, no stack.** |

Rows 2 and 3 are not rivals: row 2 gives you a *tab*, row 3 gives you a
*verdict*. If you are verifying your own UI change and then tearing down, you
want row 3.

### The headless verdict arm (`scripts/verify-page-verdict.sh`)

Carries an arbitrary URL — plus optional UI-Bridge interaction steps — through to
a machine-readable verdict: inject the Bridge engine into the page headlessly,
capture the element snapshot, normalize it with qontinui-web's own shipped
`normalize.ts`, and run the `vision-audit` analyzer **built from the SHA
`qontinui-web/style-gate.lock` pins**.

```bash
# A page that ships ZERO UI Bridge code is fine — the engine is injected.
bash qontinui-claude-config/scripts/verify-page-verdict.sh --url "$TARGET_URL"

# A state only reachable by clicking. --step and --capture interleave IN ORDER,
# so one page load yields several audited states; an `initial` capture is taken
# before the first step automatically, so the effect of a click is demonstrable
# rather than asserted.
bash qontinui-claude-config/scripts/verify-page-verdict.sh \
  --url "$TARGET_URL" \
  --step 'executeElementAction {"id":"dom-open-roster","request":{"action":"click"}}' \
  --capture after-open \
  --step 'executeElementAction {"id":"dom-expand-details","request":{"action":"click"}}' \
  --capture after-expand
```

Machine JSON on stdout, human summary on stderr, and **distinct exit codes** —
read the status, do not just grep the JSON:

| Exit | Meaning |
|---|---|
| `0` | PASS |
| `2` | **GATE FAILED** — real findings, each naming the offending element ids |
| `3` | **CAPTURE FAILURE** — the page was never observed, or a step could not be performed. This is never a pass. |
| `4` | no `vision-audit` at the pinned SHA (the error prints the build command) |
| `127` | local environment fault |

Notes that matter, all of them measured:

- **The transport is `ui-bridge-inject --exec-stdin`** — NDJSON, one
  `{"action":…,"payload":…}` per line. No relay, so neither relay auth nor LNA
  applies. Payload shape for element actions is `{id, request:{action, value?}}`
  (`ui-bridge/packages/ui-bridge/src/react/commandHandlers.ts:1090`); sub-actions
  include click, hoverClick, focus, blur, hover, scrollIntoView, scroll, toggle,
  doubleClick, type, clear, setValue, select, check, uncheck, submit, reset,
  sendKeys, drag, alongside top-level `clickByText`, `clickBySelector`,
  `typeInto`, `readValue`, `sendKeysToPage`, `expectElement`, `find`, `discover`,
  `getElementTree`. **Read `commandHandlers.ts` for the authoritative list —
  and if the action you need is not there, that is a UI Bridge gap: fix it in the
  Bridge and report it. Never route around it with a Playwright locator.**
- **An action that FAILS does not come back on the CLI's `error` key.** A Bridge
  command that *returns* a failure (the common case, e.g. `ELEMENT_NOT_FOUND`)
  arrives as an ordinary result carrying `success:false`, and the CLI still
  exits 0. The script treats that as a capture failure (exit 3) — because the
  state analyzed would not be the state requested, and reporting findings
  against it would be a confidently wrong answer.
- **`analyze` is the default and needs no spec.** `vision-audit assert` is
  opt-in (`--assertions <file>`, plus `--baseline-dir` for
  `no_layout_shift_since`) and presupposes an authored spec against known element
  ids — omit them and it is a parse error, **exit 1**, not a gate failure. An
  `assert` PASS means only "every assertion in the spec held"; the arm prints
  `coverageNotes` saying what the spec did not cover, because a fresh route has
  no committed baseline and a green result there checked nothing about layout
  shift.
- **Colour is reported but never gated.** Only the `color` analyzer reads pixels;
  this arm ships no screenshot capability and feeds an obviously named 1×1
  placeholder frame. Layout, typography and elements are pure geometry over the
  snapshot. The parent plan's burn-in measured pixel-sampled contrast producing
  false 1.01:1 criticals — do not gate on colour.
- **The verdict is only as pinned as the analyzer.** The output stamps
  `analyzer.pinnedSha` and `analyzer.shaVerified`; quote them. A build from
  `qontinui-schemas` HEAD answers a different question than the Style Gate does.
- **What this arm does NOT reach:** a page behind qontinui-web's `(app)` auth
  wall. That needs the `/verify-web` stack (`dev-start.ps1 -VerifyWebStack`,
  which does need `pwsh` + `docker`) to mint the dev token and seed a
  storageState — then pass it through with `--storage-state` and the verdict half
  is identical. Everything else on this box needs only node + Chromium + a URL.

### Injected transport — driving a bare pre-auth page (`--transport=injected`)

Some pages ship **zero UI Bridge code** — the sign-in / register / forgot-password pages on prod and staging are plain HTML with no instrumentation. The standard targets above all assume the page already embeds the SDK, so they cannot drive a bare login page. The **injected transport** closes that gap: a Node-side CLI (`ui-bridge-inject`, shipped in `@qontinui/ui-bridge-wrapper`) launches a Chromium tab, navigates to the bare page, injects the UI Bridge engine bundle into it, and (by default) registers that tab as a **relay tab against the qontinui-web relay on the target page's own origin** (`<origin>/api/ui-bridge` — prod `https://qontinui.io/api/ui-bridge`, local dev `http://localhost:3001/api/ui-bridge`) — so the existing `/control/*` plane drives it exactly like any instrumented page.

> **A runner relay IS available now — this note used to say the opposite, and that was stale for ~3 months.** The blockquote here read *"Runners have **no relay-tab protocol**: `GET /ui-bridge/tabs` → 404 … `POST /ui-bridge/heartbeat` is an IPC forward … Registration against a runner can never succeed"*, with the protocol described as "planned … once it ships, re-point this section's `RELAY_BASE` at the temp runner". **It shipped ~7 hours after that sentence was written** — qontinui-runner **#560** (`f58602ff2`, 2026-06-13), the remediation plan's own item 6(a) — and the note was never re-pointed. All three 404 claims are false today. This is the re-point.
>
> A runner now serves the full web-relay wire contract at `http://127.0.0.1:<port>/ui-bridge` (`src-tauri/src/mcp/ui_bridge/relay.rs`, registered in that family's `routes()` + `route_entries()` and classified in `CONTRACT.md`):
>
> | Route | What it does |
> |---|---|
> | `GET /ui-bridge/commands/stream?tabId=` | SSE command delivery. First event `{"type":"connected","tabId":…}`, then queued command frames; `: heartbeat` keep-alive every 15s. |
> | `POST /ui-bridge/commands` | The tab posts its result envelope `{commandId, success, result, tabId, error?}`. |
> | `POST /ui-bridge/heartbeat` | **Tab-registry upsert** — no longer an IPC forward. Response carries `data.tabRegistered`, so the client can detect a silent stream drop and force a reconnect. |
> | `GET /ui-bridge/tabs` | Registry listing — this is what `ui-bridge-headless`'s `waitForUiBridgeRegistration` polls (it reads `body.data.tabs[].tabId`). |
> | `POST /ui-bridge/relay/dispatch` | Queue a command to a registered tab and await its result, with typed routing errors and **no silent cache fallback**. |
>
> **Two behavioural differences from the qontinui-web relay, both deliberate.** Heartbeats are **LENIENT** — the runner is a single-operator loopback surface, so `registrationMetadata` (`{userId, sessionId}`) is stored when present but never required, and there is no `--auth-token` to mint. And a tab with no live SSE listener is **evicted after 60s** (`STALE_TAB_EVICT_MS`; heartbeats arrive every 10s, so that is 6 missed beats).
>
> **Which relay to point at, then:** a **temp runner** for local/CI drives of arbitrary pre-auth pages — that is the isolation the protocol was built for, and it needs no prod relay and no Cognito dependency. The **qontinui-web relay** for anything on `https://qontinui.io` (see the scheme table below): a prod page can only reach its own origin, so the runner relay is not a substitute there.

Select it with:

```
/manual-test --transport=injected --target-url=<http(s)://pre-auth-page>
```

| Flag | Meaning |
|---|---|
| `--transport=injected` | Use the inject-CLI path instead of an already-instrumented target. |
| `--target-url=<url>` | The bare pre-auth page to drive (e.g. `https://qontinui.io/login`). |

**Decision point — branch on the target page's scheme BEFORE launching (it decides the relay + auth):**

| `--target-url` scheme | Drive path |
|---|---|
| `http://` local dev (e.g. `http://localhost:3001/login`) | **Relay mode (Variant B, the default)** — inject + register the tab against the SAME-ORIGIN local web relay `http://localhost:3001/api/ui-bridge`, drive via `/control/*`. No `--auth-token` needed while the local gate (`UI_BRIDGE_REQUIRE_AUTH`) is off. Full recipe: Phase 0 → Step 1.5. |
| `https://` prod/staging (e.g. `https://qontinui.io/login`) | **Relay mode against the SAME-ORIGIN prod web relay** `https://qontinui.io/api/ui-bridge`, **with `--auth-token <Cognito operator IdToken>`** — the prod relay is auth-gated (Bearer-only). LNA never fires on this path: the page fetches its own https origin, no loopback involved. For quick snapshot/find-level checks without a parked session, **Variant A** (`--exec` one-shots, end of Step 1.5) also works; when you need a full login + parked authed session driven by another agent, the **`ui-bridge-login-web` flow** (`manual-test-coord.md` Phase 0.6) remains valid. |

**One dead end remains here, and it is narrower than this section used to claim.** It listed two; the first is gone.

1. ~~**Runner relay (any scheme)** — "can never work, the runner has no relay-tab protocol".~~ **Retracted.** A runner has served `/ui-bridge/tabs`, `/ui-bridge/commands/stream` and `/ui-bridge/relay/dispatch` since qontinui-runner #560 (2026-06-13) — see the re-pointed note above. If `curl http://127.0.0.1:<TEST_PORT>/ui-bridge/tabs` returns 404 today, that is a **stale runner binary**, not the protocol's absence: check the build with `curl -s http://127.0.0.1:9876/health` and drive a temp runner built from current `main` instead. (Never restart the primary runner to fix this — served policy `production-and-cost` `runner-lifecycle`.)
2. **LNA (https page → loopback relay)** — still real, but **`ui-bridge-inject` now handles it for you.** Chrome Local Network Access blocks a secure-context (https) page from fetching loopback/private addresses, so the injected bundle's `startRelayClient` POST never lands. Since ui-bridge #104 the CLI **auto-appends** `--disable-features=LocalNetworkAccessChecks,PrivateNetworkAccessSendPreflights,PrivateNetworkAccessRespectPreflightResults` whenever `--url` is https **and** `--relay` resolves to a loopback host (`inject-cli.ts` `buildLaunchArgs` / `isLoopbackHost`), merging into any `--disable-features` you passed rather than duplicating the flag; it logs when it fires. So an https page against a **temp-runner** relay is a supported combination — you need no manual flag and no `node -r` preload patch.
   **LNA error signature (recognize it, don't re-debug it)** — if you see it anyway (an older CLI build, or a non-loopback private address the auto-detect does not match): the inject CLI launches Chromium, navigates, and injects fine; `inject-cli.err` stays clean; the block is visible only in the page's DevTools console, in one of two forms depending on Chrome version — the older Private-Network-Access CORS wording, `...has been blocked by CORS policy: The request client is not a secure context and the resource is in more-private address space 'loopback'`, or the newer `net::ERR_BLOCKED_BY_PRIVATE_NETWORK_ACCESS_CHECKS` / "Blocked by Local Network Access checks" (the in-page fetch rejects as `TypeError: Failed to fetch`). Either form means the LNA flags did not reach Chromium — check the CLI's launch-args log line, or pass them yourself with `--launch-arg`, before falling back to the same-origin web relay row above.

### VERIFIED authed-web drive recipe — prod `qontinui.io` (2026-07-22)

This is the copy-paste path that produced a live, drivable, authenticated tab on
the prod relay. It supersedes the "no authed web surface" blocked-task note that
`/manual-test-loop` carried through iterations 2 and 3. **Run it as-is; do not
re-derive it.** The four ingredients that were previously wrong or missing are
called out inline — each was an independent blocker. The `-p playwright` peer
below is **Playwright-as-host** — it only gives the UI Bridge a headless browser
to attach to, never a locator to assert with; the full split is
`qontinui-claude-config/knowledge-base/qontinui-specific/ui-bridge.md` →
"The UI Bridge Is the Only Frontend-Inspection Tool".

```bash
# ── 1. IDENTITY. Use the SECONDARY operator. tester2 is a dedicated test
#    account in its OWN tenant, so it cannot touch operator data and cannot
#    hijack the operator's parked browser tab — that isolation is the whole
#    reason for the choice, and it stands on its own. (The primary's prod
#    password IS reachable: SSM /qontinui/operator/{email,password} are
#    populated in eu-central-1 — verified 2026-07-28, mod. 2026-05-24 /
#    2026-06-13 — but Git-Bash reads MUST be MSYS_NO_PATHCONV=1-guarded like
#    the fetches near the end of this file; an UNGUARDED probe gets the
#    leading-/ name path-converted and a false ParameterNotFound in every
#    region, which is how an earlier revision of this comment came to claim
#    the params were "empty in every region as of 2026-07-22".
#    VITE_DEV_PASSWORD remains the LOCAL dev password only:
#    qontinui-web/dev-credentials.json says so in its own _comment.)
EMAIL="$QONTINUI_OPERATOR2_EMAIL"        # tester2@qontinui.io
#    THE SESSION ENV IS NOT A CREDENTIAL SOURCE ANY MORE. The runner scrubs
#    QONTINUI_OPERATOR2_PASSWORD (and QONTINUI_TEST_LOGIN_PASSWORD,
#    QONTINUI_TEST_AUTO_LOGIN_PASSWORD) out of every session it spawns - they
#    were plaintext passwords sitting in the environment of ~9 concurrent
#    sessions, and the habitual JWT/KEY/TOKEN/SECRET redaction filter matches
#    none of them. So `printenv QONTINUI_OPERATOR2_PASSWORD` now returns EMPTY
#    BY DESIGN inside a runner-spawned session. That is not "the account is
#    unprovisioned" and not a bug to debug - see the explicit failure branch
#    below, which exists so an empty string cannot read as either.
PASS="$(printenv QONTINUI_OPERATOR2_PASSWORD 2>/dev/null)"
if [ -z "$PASS" ] && command -v powershell >/dev/null 2>&1; then
  #    ── STATED TRADE-OFF (decided, not incidental) ──────────────────────────
  #    This read recovers, inside a runner-spawned session, exactly the value
  #    the scrub removed from that session. That is DELIBERATE, because the
  #    scrub's threat model is ACCIDENTAL BULK EXPOSURE - an `env` dump printing
  #    three plaintext passwords into a transcript, which the habitual
  #    JWT|KEY|TOKEN|SECRET filter does not redact because none of them match
  #    `PASSWORD`. It was never denial to a determined caller: the operator owns
  #    the User hive, so a deliberate read by a command that genuinely needs the
  #    credential is in scope and permitted.
  #    THE BOUNDARY IS THE SHAPE OF THE READ. It must stay a NAMED
  #    SINGLE-VARIABLE read. Never widen it to an enumeration
  #    (`Get-ChildItem Env:`, `[Environment]::GetEnvironmentVariables('User')`,
  #    a registry dump of the Environment key) - that reinstates precisely the
  #    bulk exposure the scrub exists to prevent, and does so under the cover of
  #    a sanctioned mechanism.
  #    ───────────────────────────────────────────────────────────────────────
  #    Resolution 1: the USER environment scope. The scrub removes the variable
  #    from the SPAWNED SESSION's environment; it stays a registry-backed
  #    per-user Windows environment variable, which is where the runner process
  #    itself inherited it from. Verified 2026-08-18: present at User scope,
  #    absent at Machine scope. Only the NAME crosses a command line here - the
  #    VALUE comes back on stdout into a shell variable, never onto any argv.
  PASS="$(powershell -NoProfile -Command "[Environment]::GetEnvironmentVariable('QONTINUI_OPERATOR2_PASSWORD','User')" 2>/dev/null | tr -d '\r')"
fi
if [ -z "$PASS" ]; then
  #    Resolution 2: the operator supplies it to THIS command. There is no
  #    runner route that vends this credential to a session, so do not go
  #    looking for one and do not fall back to VITE_DEV_PASSWORD (local dev
  #    only). Stop here with a named cause instead of authenticating as nobody.
  echo "SETUP_GAP: QONTINUI_OPERATOR2_PASSWORD is not in this session's env"
  echo "  (scrubbed at spawn BY DESIGN) and not readable at the User scope."
  echo "  Either run this step outside a runner-spawned session, or have the"
  echo "  operator paste the tester2 password into this one command's shell."
  exit 0
fi

# ── 2. RELAY BEARER. The prod relay is Bearer-gated. Mint a Cognito IdToken via
#    USER_PASSWORD_AUTH on the RUNNER app client (the web SPA client is
#    PKCE-only). Lifetime is 3600s — call `refresh_idt` PER BATCH; an expired
#    token comes back as a flat `UNAUTHENTICATED`, which reads exactly like a
#    bad token.
#
#    BOTH HALVES of the credential travel in FILES, never on curl's argv:
#    process cmdlines are world-readable on this multi-session machine, so every
#    peer session could otherwise read them out of the process list. The token
#    goes via `curl -H @file`; the PASSWORD THAT MINTS IT goes via
#    `curl --data-binary @file` — the same door. Staging only the token would
#    protect the half that expires in 3600s and leave the half that does not,
#    and that can re-mint tokens at will, sitting on the command line three
#    lines above it.
#
#    `printf` and `python` below are safe channels: printf is a SHELL BUILTIN
#    (no process is spawned, so nothing reaches a cmdline), and python reads the
#    secrets from its ENVIRONMENT, never from argv.
#
#    Minting and staging are ONE function on purpose — every call below reads
#    $IDT_HDRP, so a bare re-mint of $IDT would change nothing and keep sending
#    the EXPIRED token.
MINT_BODY=$(mktemp) || { echo "mktemp failed — cannot stage the password off argv"; exit 1; }
IDT_HDR=$(mktemp)   || { echo "mktemp failed — cannot stage the token off argv"; exit 1; }
# ONE trap covering BOTH files. A second `trap … EXIT` REPLACES this one rather
# than adding to it, and would leave a live credential behind in $TMPDIR.
trap 'rm -f "$MINT_BODY" "$IDT_HDR"' EXIT
# `cygpath -w` — a native curl.exe cannot open mktemp's POSIX path when MSYS
# pathconv is off. Every call below uses the *_P spellings.
IDT_HDRP=$IDT_HDR;     command -v cygpath >/dev/null 2>&1 && IDT_HDRP=$(cygpath -w "$IDT_HDR")
MINT_BODYP=$MINT_BODY; command -v cygpath >/dev/null 2>&1 && MINT_BODYP=$(cygpath -w "$MINT_BODY")
# Stage INSIDE the function and delete right after the POST, so the password's
# file exists for one HTTP call rather than for the whole session. The trap is
# the backstop for a mint that dies mid-way, not the routine cleanup.
# json.dumps also fixes the escaping the old inline body got wrong: it
# interpolated $PASS straight into a JSON string literal, so a password
# containing `"` or `\` produced malformed JSON and Cognito's parse error read
# exactly like a rejected credential.
mint_idt() {
  MT_EMAIL="$EMAIL" MT_PASS="$PASS" python -c 'import json,os,sys
sys.stdout.write(json.dumps({"AuthFlow":"USER_PASSWORD_AUTH",
  "ClientId":"67f2a1a0cmgileob23lniud5t7",
  "AuthParameters":{"USERNAME":os.environ["MT_EMAIL"],"PASSWORD":os.environ["MT_PASS"]}}))' > "$MINT_BODY"
  [ -s "$MINT_BODY" ] || { echo "could not stage the Cognito mint body (LOCAL fault)" >&2; return 1; }
  curl -s -m 20 -X POST "https://cognito-idp.us-east-1.amazonaws.com/" \
    -H "Content-Type: application/x-amz-json-1.1" \
    -H "X-Amz-Target: AWSCognitoIdentityProviderService.InitiateAuth" \
    --data-binary @"$MINT_BODYP" \
    | python -c "import sys,json; print(json.load(sys.stdin)['AuthenticationResult']['IdToken'])"
  : > "$MINT_BODY"   # truncate immediately: the body held the operator password
}
refresh_idt() {
  IDT=$(mint_idt)
  [ -n "$IDT" ] || { echo "Cognito returned no IdToken — relay calls cannot authenticate"; return 1; }
  printf 'Authorization: Bearer %s\n' "$IDT" > "$IDT_HDR"
  [ -s "$IDT_HDR" ] || { echo "could not stage the token header (LOCAL fault)"; return 1; }
}
refresh_idt || exit 1

# ── 3. CO-PILOT PREFERENCE (once per account — THE step everyone misses).
#    Without it the login succeeds, the consent modal never renders,
#    CommandRelayListener never mounts, and `GET /tabs` returns 0 tabs. That
#    zero-tabs reading was previously misdiagnosed as "no operator tab parked".
curl -s -m 20 -X PUT -H @"$IDT_HDRP" -H "Content-Type: application/json" \
  -d '{"ui_bridge_co_pilot_enabled": true}' \
  "https://api.qontinui.io/api/v1/users/me/preferences"

# ── 4. PARK AN AUTHED TAB. --post-login-click grants the per-session consent;
#    --keep-open parks it. MSYS_NO_PATHCONV=1 keeps --success a URL path.
LOGIN_WEB="npx -y -p @qontinui/ui-bridge-wrapper -p @qontinui/ui-bridge -p @qontinui/ui-bridge-headless -p playwright ui-bridge-login-web"
MSYS_NO_PATHCONV=1 UIB_LOGIN_EMAIL="$EMAIL" UIB_LOGIN_PASSWORD="$PASS" \
  $LOGIN_WEB --url "https://qontinui.io/login?next=%2Fbuild%2Fworkflows" \
  --success /build/workflows \
  --post-login-click "[data-testid='co-pilot-consent-allow']" --keep-open \
  > park.json 2> park.log &
PARK_PID=$!
# park.json must show ok:true AND postLoginClicked:true. postLoginClicked:false
# means step 3 did not take — fix that, do not retry the login.

# ── 5. CAPTURE OUR TAB ID and drive. Registration lands ~10-20s after the
#    consent click.
TAB=$(curl -s -H @"$IDT_HDRP" "https://qontinui.io/api/ui-bridge/health" \
  | python -c "import sys,json; print((json.load(sys.stdin)['data']['connectedTabs'] or [''])[-1])")
curl -s -H @"$IDT_HDRP" "https://qontinui.io/api/ui-bridge/control/snapshot?tabId=$TAB"
curl -s -X POST -H @"$IDT_HDRP" -H "Content-Type: application/json" \
  -d '{"query":"New Workflow"}' "https://qontinui.io/api/ui-bridge/ai/find?tabId=$TAB"
curl -s -X POST -H @"$IDT_HDRP" -H "Content-Type: application/json" \
  -d "{\"url\":\"/observations\",\"mode\":\"soft\",\"targetTabId\":\"$TAB\"}" \
  "https://qontinui.io/api/ui-bridge/control/page/navigate?tabId=$TAB"

# ── 6. TEARDOWN. kill $PARK_PID (SIGTERM) — otherwise a Chromium leaks.
```

**Measured result of the run that verified this (2026-07-22).**
`/build/workflows` snapshot: `success:true`, `route:"/build/workflows"`, 45
elements including `workflow-list-sidebar` → `"WORKFLOWS 0 New Workflow No
workflows yet"`, `button-new-workflow`, `input-search`, and
`co-pilot-active-state`; the literal string `UNAUTHORIZED` absent from the whole
payload. Then the co-pilot `page-memory-search` dispatch (`pageMap.ts:214` maps
it to `observations`; `relayExecutor.ts:253-276` turns that into
`POST /control/page/navigate {url:"/observations",mode:"soft"}`) moved the SAME
tab to `route:"/observations"` with 39 authed elements.

**Two non-causes — stop chasing them.**
1. *The 401/400/429 resource cascade in the browser console is NOISE.* Every
   successful login above logged it. It never prevented the login. The relay's
   own `_auth.ts` deliberately distinguishes a 429 (`upstream_error`) from an
   auth verdict (`unauthenticated`) precisely so this isn't misread.
2. *The prod login page being "uninstrumented" is not a blocker either.* The
   injected transport instruments it at runtime; `login-web` located
   `#signInFormUsername` / `#signInFormPassword` on the Cognito hosted UI and
   submitted without any `--expect-selector` tuning.

**Relay-free variant (no parked tab, no relay bearer).** Capture the session
once, then replay it per check. Prefer this when the loop just needs a
snapshot-level assertion, or when the relay is unreachable:

```bash
# Same Playwright-as-host carve-out as above: the browser, never the assertion.
$LOGIN_WEB --url "https://qontinui.io/login?next=%2Fbuild%2Fworkflows" \
  --success /build/workflows --storage-state-out auth.json \
  --expect-text "New Workflow,Search..." --screenshot wf.png
npx -y -p @qontinui/ui-bridge-wrapper -p @qontinui/ui-bridge -p @qontinui/ui-bridge-headless -p playwright \
  ui-bridge-inject --url "https://qontinui.io/build/workflows" \
  --storage-state auth.json --exec 'getControlSnapshot {}'
```

> **⚠️ Replay version floor — higher than capture, and on TWO packages.** A storage
> state can carry `__uiBridge_tabId` in sessionStorage; restore it and the injected
> tab re-registers under the **operator's live tab id**. Two independent halves
> stop that, so floor both: `@qontinui/ui-bridge-wrapper` ≥ **0.7.0** keeps the key
> out of artifacts you capture, and `@qontinui/ui-bridge-headless` ≥ **0.4.0**
> scrubs it at restore — which is the half that covers an artifact captured
> *earlier*, or by someone else. Either one alone is enough for its own case;
> neither covers the other's, and the hijack needs only one gap. The wrapper's
> peer range on headless is `>=0.3.0 <1`, so it does **not** force 0.4.0. Today a
> fresh `npx -y` resolves headless `latest` = 0.4.0 and is fine; a pin, a lockfile
> or a warm npx cache is what holds it at 0.3.0, which replays `--storage-state`
> without scrubbing. Below either floor, pass `--tab-id` or strip the key by hand.
> Derivation: `knowledge-base/qontinui-specific/ui-bridge.md` → "Tab-identity
> safety on storage-state replay".

`--expect-text` is itself a valid on-page PASS gate — the verified run returned
`expectFound:["New Workflow","Search..."], expectMissing:[]`. Give
`--screenshot` / `--storage-state-out` **Windows-style** paths (`D:/...`); a
Git-Bash `/c/Users/...` path is written literally by Windows Node and the file
lands somewhere you won't find it. If the replayed `--exec` snapshot comes back
with only 1-2 elements the page had not painted yet — raise `--settle-timeout`
or add `--expect-selector`; it is NOT an auth failure (check `route` first, it
will read `/build/workflows` rather than `/login`).

**Never replay a storage-state artifact into the OPERATOR's browser** — that
hijacks their parked tab. The artifact above is tester2's, in tester2's tenant,
which is why this recipe is safe to re-run unattended.

Key architecture facts (do not violate):

- **The runner cannot launch Chromium** — it's a Tauri app. The Node-side `ui-bridge-inject` CLI does all the browser work; the skill drives the resulting tab purely through the relay `/control/*` HTTP API.
- **`--relay` is the qontinui-web relay on the TARGET PAGE's origin** (`<origin>/api/ui-bridge`), NOT a runner and NOT a bare `/ui-bridge` path. The injected bundle's `startRelayClient` POSTs to `--relay` to register the tab. Pointing `--relay` at a temp runner (`http://127.0.0.1:<TEST_PORT>/ui-bridge`) is **no longer a mistake in itself** — a runner has served the relay-tab protocol since qontinui-runner #560 — but it is the wrong relay *for a prod page*, which can only reach its own origin. Use a temp-runner relay when you are driving a LOCAL or CI page; use the web relay for anything on `https://qontinui.io`. The #2 mistake is `https://qontinui.io/ui-bridge` (missing the `/api` prefix) — the relay lives under `/api/ui-bridge`. Nothing is injected into the prod artifact; what prod DOES need is relay auth: pass `--auth-token <Cognito operator IdToken>` and send `Authorization: Bearer` on every `/control/*` call (the prod relay is Bearer-gated).
- **PASS = the authed DOM observed ON THE PAGE.** After you fill creds and submit, the PASS gate is a `snapshot`/`find` showing the post-login authed DOM rendered in the tab (e.g. a known post-login nav item). A 2xx / redirect / registration / log signal is NOT a PASS (see the Phase 5 verification binding block).
- **Prod safety.** Filling creds into `qontinui.io/login` is read-ish, but **NEVER** complete a destructive register/signup, and gate any prod-`--target-url` run behind explicit operator confirmation (honor production-only-work + read-only-nav rules). Credentials come from SSM (`/qontinui/operator/*`), same pattern the rest of this skill family uses.

The full launch + drive recipe lives in **Phase 0 → "Injected transport launch"** and **Phase 1**; the cleanup (SIGTERM the CLI) is in Phase 0 Step 3.

**Mobile precondition — DO NOT SKIP.** `localhost:8087` only resolves to the device's bridge after one of three transports has been set up. Just installing/launching the mobile app on a phone is NOT enough. Probe before testing:

```bash
# Returns 0 (forwarded — can hit localhost:8087) or 1 (no transport active)
adb forward --list | grep -q "tcp:8087" || \
  curl -s http://127.0.0.1:9876/ui-bridge/devices | python -c "import sys,json; d=json.load(sys.stdin)['data']; sys.exit(0 if d.get('count',0) else 1)"
```

If the probe fails: the mobile app is unreachable from this host. Either set up a transport (USB: `adb forward tcp:8087 tcp:8087`; LAN/cloud: pair via the in-app Connection Wizard), or limit the test to server-side artifact verification — manifest meta-data grep on the AAB, `eas-cli channel:view production`, etc. **Do not waste time probing localhost ports** — only one is the right answer (8087) and it only works after the transport is up. See `runner-features.md` "Mobile transport paths" for the full reference.

**AAB build/roll lag.** The installed AAB on a phone may be older than the current `qontinui-mobile/master`. New mobile features only become testable on-device after `/build-mobile-aab` runs and the resulting AAB is uploaded to the Play Console and installed by the operator. Always check `GET <mobile-base>/health` (or `/ui-bridge/health`) for `data.uiBridge.version` — if it does not match the latest `@qontinui/ui-bridge-native` version in `qontinui-mobile/package.json`, the AAB is stale and any "new" feature you're trying to verify is not yet on the device. Report stale-AAB findings as deferred to the next Play roll rather than re-litigating them.

### Mobile-on-Windows — adb / Expo Router / re-pair gotchas

These bit hard during the 2026-05-03 phone smoke. Skip the rediscovery.

1. **Never use `adb exec-out screencap -p > file.png` on Windows.** Windows adb translates `\n` → `\r\n` in the binary stdout stream, corrupting the PNG past the header. The file ends up the right size but invalid; the Anthropic image upload returns `400 Could not process image`. Use file-mode instead:
   ```bash
   adb -s "$DEVICE_ID" shell screencap -p //sdcard/x.png
   MSYS_NO_PATHCONV=1 adb -s "$DEVICE_ID" pull /sdcard/x.png "<workspace-root>/tmp_phone_screen.png"
   ```
   Two Git-Bash specifics: prefix the device path `//sdcard/` (single slash gets rewritten to `C:/Program Files/Git/sdcard/`); use `MSYS_NO_PATHCONV=1` on the `pull` so the source path passes through verbatim. Validate the header (`89 50 4E 47`) before relying on the PNG.

2. **Expo Router deeplinks: the `(tabs)` group is invisible in URLs.** `qontinui://tabs/terminal` produces an "Unmatched Route" screen. Use `qontinui:///terminal` (or `qontinui://terminal`). General rule: any URL segment wrapped in parentheses in the file-system route is omitted from the URL.

3. **After cold-starting the mobile app (`am start -n io.qontinui.mobile/.MainActivity`), the device proxy needs ~5–10s to re-pair.** The supervisor's `/ui-bridge/devices` will report `transports: [], healthState: unreachable` and `/ui-bridge/control/snapshot` will return `{elements: [], route: null}` for several seconds. Loop the snapshot probe with backoff before declaring the bridge broken:
   ```bash
   for i in $(seq 1 10); do
     count=$(curl -s "http://127.0.0.1:${PROXY_PORT}/ui-bridge/control/snapshot" | python -c "import sys,json; print(len(json.load(sys.stdin).get('elements',[])))")
     [ "$count" -gt 0 ] && break
     sleep 2
   done
   ```

4. **React Navigation tab buttons in qontinui-mobile are instrumented (`tab-runs`, `tab-terminal`, …)** via `BridgeTabButton` (`app/(tabs)/_layout.tsx`). Drive tab switches with `POST /control/element/tab-{name}/action {"action":"press"}` — `performPress` reads `props.onPress`. Snapshot's `actions: None` field is layout metadata, not a missing capability.

## UI Bridge Command Reference

**Canonical reference:** `<workspace-root>/ui-bridge/docs-site/docs/api/runner-features.md`
(public version: `https://github.com/qontinui/ui-bridge/blob/main/docs-site/docs/api/runner-features.md`).

Read that doc once at the start of a testing session — it covers every
endpoint and gotcha listed below in full detail. The cheatsheet here is
just enough to navigate without re-reading the canonical reference for
common operations. **When in doubt, defer to the canonical reference**.

### Cheatsheet — endpoints by category

`Mobile?` column: ✅ = supported on `qontinui-mobile`; ❌ = `NOT_SUPPORTED` envelope or 404/501 on mobile (see "Mobile UI Bridge SDK gaps" below for detail and workarounds); ⚠ = supported with a known mobile-side caveat.

| Category | Endpoint | Mobile? | Purpose |
|---|---|---|---|
| Snapshot | `GET /control/snapshot[?visibleOnly][&currentRouteOnly]` | ⚠ | full state; carries `route`, `activeTab`, `registration` metadata. Mobile `visibleOnly=true` filter is broken pre-Phase-6 — see gaps below |
| | `POST /control/discover` | ✅ | force re-scan |
| Element read | `GET /control/element/:id` | ✅ | dynamic state at `data.state.value`, NOT `data.value` |
| Element write | `POST /control/element/:id/action` | ✅ | type/clear/setValue/click/scroll/check/sendKeys/drag/etc. |
| Page nav | `POST /control/page/navigate {url, mode?: "soft"\|"hard"}` | ✅ | soft preserves globals + SDK state |
| | `POST /control/page/refresh` | ✅ | full reload |
| | `POST /control/page/close-request` | ❌ | runner-only window-X exercise |
| Tabs (runner) | `GET /control/tabs` | ❌ | list + activeTab (runner-only) |
| | `POST /control/tab/activate {tabId}` | ❌ | switch (HTTP 400 + knownTabs on unknown) |
| Network stubs | `POST/GET/DELETE /control/network/stubs[/:id]` | ❌ | substring match, times: 1\|always |
| | `POST /control/network/verify-stub` | ❌ | non-consuming peek |
| Wait | `POST /ai/wait-for-element {elementId,state,timeoutMs?,pollMs?}` | ❌ | 9 state predicates; on mobile fall back to manual `sleep`+`discover` loop |
| | `GET /ai/idle-status` / `POST /ai/wait-for-idle` | ❌ | weighted page-idle signals |
| AI find | `POST /ai/find {query}` | ✅ | matches label > text > value > aria-label > placeholder > name |
| | `GET /ai/forms` | ❌ | DOM walk; finds inputs `ai/find` misses. On mobile returns `NOT_SUPPORTED` (or HTTP 404 on pre-Phase-5 AABs) |
| Change tracking | `POST /ai/change-buffer/{enable,drain,disable}` | ❌ | DOM mutation buffer |
| | `POST /ai/bookmarks {name}` then `GET /ai/bookmarks/:name/diff` | ❌ | named-snapshot diff |
| Components | `GET /control/components` and `/control/component/:id` | ⚠ | high-level actions with paramSchema. Mobile registers very few — `/control/components` typically returns 0 |
| | `POST /control/component/:id/action/:actionName` | ⚠ | ⭐ prefer over fighting button-* IDs (web/runner) |
| Eval (runner) | `POST /control/page/evaluate {expression}` | ❌ | rejects literal `fetch(` — use `window["fet"+"ch"]` |
| Console / SDK | `GET /control/console-errors` and `/sdk/network-requests` | ⚠ | observability; mobile coverage partial |
| Control / power | `POST /control/keep-awake {enabled}` | ⚠ | mobile-only feature; returns `NOT_SUPPORTED` unless a `KeepAwakeProvider` is registered by the host app (qontinui-mobile registers one as of `master@77c2417`) |
| Design | `POST /ai/design-audit` | ❌ | requires style guide; flat HTTP 400 if none |

### Mobile UI Bridge SDK gaps

`qontinui-mobile` runs the `@qontinui/ui-bridge-native` SDK, which is a different surface from the runner/web `@qontinui/ui-bridge` core. Several endpoints common on runner/web are unimplemented or stubbed on mobile. Don't waste a manual-test cycle re-discovering these — pick the workaround on the right.

| Endpoint | Mobile behaviour | Workaround |
|---|---|---|
| `GET /ai/forms` | HTTP 501 + `{code: "NOT_SUPPORTED"}` envelope on a current AAB. On pre-Phase-5 AABs it 404s (route not registered). | Use `POST /ai/find` with the visible field label/placeholder, OR snapshot and filter `elements[]` client-side by `kind: "input"`. |
| `GET /ai/idle-status` and `POST /ai/wait-for-idle` | `NOT_SUPPORTED` envelope. Mobile has no equivalent of the web/runner idle-signal aggregator. | Insert deterministic `sleep 2` after a navigation/action, then re-`discover` + snapshot. For network-bound waits, poll `sdk/network-requests` if instrumented; otherwise sleep + retry. |
| `POST /ai/change-buffer/{enable,drain,disable}` (full family) | `NOT_SUPPORTED`. No DOM-mutation buffer on RN. | Take a baseline snapshot, perform the action, take a second snapshot, diff `elements[]` by `id` client-side. |
| `POST /ai/wait-for-element {elementId,state,timeoutMs?,pollMs?}` | `NOT_SUPPORTED`. | Loop `GET /control/snapshot` (or `/control/element/:id`) with `sleep 0.5` and a max-iteration cap; check the desired predicate (`state.value`, `state.visible`, `state.enabled`, etc.) yourself. |
| `POST /control/keep-awake {enabled}` | `NOT_SUPPORTED` unless the host app registers a `KeepAwakeProvider` on `UIBridgeNativeProvider`. qontinui-mobile registers one as of `master@77c2417`, but older installed AABs return the error envelope. | If AAB is stale, defer to next `/build-mobile-aab` roll. If current AAB but provider still missing, the host app needs a code change — not a UI Bridge limitation. |
| `GET /control/snapshot?visibleOnly=true&currentRouteOnly=true` | Pre-Phase-6 RN SDK reports `visibility: "unknown"` on every element, so the filter eliminates the entire set and returns `elements: []`. Post-Phase-6 the filter works correctly. | Pre-fix: omit `visibleOnly`, fetch the full snapshot, and filter client-side by ID prefix matching the current route's screen (e.g. `id` starts with `dashboard-` while on Dashboard). Post-Phase-6 fix: the filter works as on web/runner. |

If a feature you need is in the right-hand column, do not file a remediation; if it is in the middle column and you have no workaround, log it and move on rather than retrying the same endpoint shape.

### Critical gotchas (the things that have burnt me)

1. **`state.value`, not `value`.** Element dynamic state lives nested
   under `state`. Top-level `value` is null on inputs even when typed.
2. **Soft-nav preserves globals; hard reloads them on real SPAs.** On
   the Tauri runner, both modes use `pushState` (a full reload would
   kill the webview); on the supervisor dashboard / Next.js, hard does
   reload and wipes injected fetch patches / bookmarks.
3. **`page/evaluate` rejects `fetch(`.** Dodge via `window["fet"+"ch"]`.
4. **`ai/find` is text-based.** Use phrases the user actually sees on
   screen — labels, placeholders, button text. Don't search by abstract
   words like "prompt input" if those words don't appear anywhere.
5. **Use `visibleOnly=true` after tab switches.** Inactive tabs stay
   mounted in the registry — raw snapshot will return mixed state.
6. **Prefer component actions over `button-*` IDs.** Components expose
   high-level actions with `paramSchema`; button IDs drift.

For full curl examples and response shapes, see the canonical reference.

## Phase 0: Spawn Test Runner & Health Check

**Always spawn a temporary test runner via the supervisor (port 9875)** for manual testing. The supervisor builds and spawns temp runners independently — it does NOT require the primary runner to be running, rebuilt, or restarted. Never rebuild the primary runner to test changes.

### Auto-login for temp test runners (authenticated pages)

Temp test runners normally auto-authenticate via the `VITE_DEV_EMAIL` / `VITE_DEV_PASSWORD` credentials baked into the runner's `dist/` at `npm run build` time (from `qontinui-runner/.env`). This means a freshly spawned temp runner typically lands past the LoginScreen within a couple of seconds — no extra setup needed.

Resolution order for the credentials the supervisor forwards to every spawned non-primary runner (as `QONTINUI_TEST_AUTO_LOGIN_EMAIL` / `QONTINUI_TEST_AUTO_LOGIN_PASSWORD`):

1. Runtime override via `POST /test-login` on the supervisor.
2. Supervisor process env vars: `QONTINUI_TEST_LOGIN_EMAIL` + `QONTINUI_TEST_LOGIN_PASSWORD`.
3. `qontinui-runner/.env` → `VITE_DEV_EMAIL` + `VITE_DEV_PASSWORD` (the default, same account the baked frontend points at).

The runner's `AuthProvider` invokes the `get_test_auto_login` Tauri command on mount; if it returns credentials, the runner auto-logs in via the same flow as the baked `VITE_DEV_*` auto-login. Primary runners are never touched by the forwarding logic.

### Step 0: Checkout-staleness check (NON-BLOCKING — warn only)

**Before building/spawning, warn (do not gate) if any local checkout that feeds the build is behind its `origin/main`.** The injected→loopback test path builds from the **local working-tree dist** (e.g. `ui-bridge` → `dist/inject-cli.cjs`, `ui-bridge-wrapper`, `ui-bridge-headless`; the runner frontend → `qontinui-runner/dist`). If that checkout sits on a stale branch, the **built dist can lack already-merged fixes**, so the test silently runs pre-fix code and passes/fails against the wrong bytes with no signal. (Observed 2026-06-13: the local `ui-bridge` checkout was on `manual-test-loop/mobile-vision-bbox`, 31 commits behind `origin/main`, so its built `dist/inject-cli.cjs` had **zero** of the LNA fix even though `f4aaa0f` was correctly on `origin/main`.)

A related stale surface used to be the supervisor's reused **`.spawn-<ref>` container** (`<workspace-root>/.spawn-<ref>/`): `prepare_worktree` force-resets the checkout to the ref but never touched `node_modules/`, so a spawn could build new source against old deps (observed 2026-07-13 as a phantom-red `TS2339 hasOwnPage` on a healthy `origin/main`). **Self-healing since supervisor `d685d03`** — `dep_install_reason()` re-runs `pnpm install --frozen-lockfile` whenever the SHA-256 of `pnpm-lock.yaml`/`package.json`/`pnpm-workspace.yaml` differs from the container's recorded hash. No operator action needed; if you see a container-only frontend build failure anyway, suspect this gate before suspecting main.

For **each repo whose code is on the injected/loopback test path** (the repo you edited + every repo whose dist gets rebuilt — at minimum `ui-bridge` for any `--transport=injected` run, plus the runner if you're testing runner code), check how far behind `origin/main` the checkout that will be BUILT is:

```bash
# Repos whose built dist is on the test path for THIS run. Add/remove as needed.
STALE_CHECK_REPOS=(
  "<workspace-root>/ui-bridge"        # inject-cli / wrapper / headless dist
  # "<workspace-root>/qontinui-runner"  # add if testing runner frontend/Rust
)
STALE_WARN_THRESHOLD=5   # small threshold — warn when behind by more than this

for repo in "${STALE_CHECK_REPOS[@]}"; do
  [ -d "$repo/.git" ] || git -C "$repo" rev-parse --git-dir >/dev/null 2>&1 || { echo "  (skip $repo — not a git checkout)"; continue; }
  git -C "$repo" fetch --quiet origin main 2>/dev/null || true
  # `git rev-list --left-right --count HEAD...origin/main` prints "<ahead>\t<behind>":
  #   LEFT  = commits on HEAD not in origin/main  (how far AHEAD the checkout is)
  #   RIGHT = commits on origin/main not in HEAD  (how far BEHIND the checkout is)
  # so the SECOND number is the behind-count we care about. (e.g. "0\t31" = 0 ahead, 31 behind.)
  read AHEAD BEHIND <<<"$(git -C "$repo" rev-list --left-right --count HEAD...origin/main 2>/dev/null)"
  BRANCH=$(git -C "$repo" rev-parse --abbrev-ref HEAD 2>/dev/null)
  if [ "${BEHIND:-0}" -gt "$STALE_WARN_THRESHOLD" ]; then
    echo "  WARNING: $repo is on '$BRANCH', ${BEHIND} commits BEHIND origin/main (ahead ${AHEAD})."
    echo "           Its built dist may NOT contain merged fixes — the injected/loopback test"
    echo "           could silently run pre-fix code. Remedy (OPERATOR'S CHOICE, this skill"
    echo "           must NOT do it automatically): fast-forward the checkout to origin/main, OR"
    echo "           re-point the build at an up-to-date worktree, then rebuild that repo's dist."
  else
    echo "  OK: $repo on '$BRANCH' (ahead ${AHEAD:-0}, behind ${BEHIND:-0})."
  fi
done
```

**This is a non-blocking advisory — never a hard gate.** A behind checkout may hold in-progress work, so fast-forwarding or re-pointing it is the operator's call; the skill only surfaces the risk and the remedy, then proceeds. Emit the warning and continue to Step 1.

### Step 1: Spawn temp runner — LKG-first, rebuild only if needed

**Default to LKG; only rebuild if your changes aren't in the LKG yet.** A cold cargo build takes ~3 minutes. The supervisor maintains a Last-Known-Good (LKG) binary at `qontinui-runner/target-pool/lkg/qontinui-runner.exe` after every successful build, and `GET /lkg/coverage` answers the spawn-mode question in one call. Skip the rebuild when you can — it's wasted clock time when another agent or session just built.

⚠️ **The route answers TWO different questions and only one of them is about commits. Do not use the mtime answer for the other.**

| Question | Field to read | Basis |
|---|---|---|
| *"Are my UNCOMMITTED edits compiled into this binary?"* — the spawn-mode decision below | `all_covered` / per-file `covered` / `file_newer_than_lkg_secs` | **mtime**: `max(file_mtime) <= lkg.built_at` |
| *"Is COMMIT `<sha>` in this binary?"* — the deploy-lag-vs-regression decision | `commit_provenance.contains` (ask with `?contains=<sha>`) | **git**: `git merge-base --is-ancestor <sha> <lkg_sha>`, computed server-side |

An mtime cannot tell you which *commits* a binary holds — `git checkout` rewrites
mtimes wholesale, so a binary built from a branch parked 72 commits behind
`origin/main` looks perfectly fresh while missing all 72. That is exactly how a
landed fix came to read as a regression (plan
`2026-07-13-manual-test-loop-iter3-remediation`, item 1). The supervisor grew the
commit-based answer for it — `src/git_provenance.rs` + the `commit_provenance`
block on this route — so use it, and never infer commit containment from
`covered`.

`commit_provenance` (present whenever an LKG exists) carries:

| Field | Meaning |
|---|---|
| `lkg_sha` | full 40-char sha the LKG binary was built from; `null` when the build-time git probe failed or the record predates sha recording |
| `lkg_source` | how that tree was classified — `live_tree` / `origin_main` / `override` |
| `behind_origin_main` | commits `origin/main` is ahead of `lkg_sha` |
| `is_ancestor_of_origin_main` | `false` ⇒ the LKG was built from a feature branch — the dangerous case |
| `contains_query` / `contains` | echo of `?contains=<sha>` and its answer |

⛔ **`contains: null` means NOT COMPUTABLE and must NEVER be read as `false`** —
no `contains` was asked, there is no `lkg_sha`, or one of the two shas is unknown
to the local object database. `null` is UNKNOWN; say so rather than concluding the
commit is absent [policy: `verification-and-evidence` `unknown-must-not-render-as-a-default`].

```bash
# 1. Identify the source files that contain the change you want to test.
#    Pass them relative to qontinui-runner/src-tauri/ (the supervisor's project_dir).
#    For "test the runner generally with no specific file changes," use src/main.rs
#    as a sentinel — if main.rs is older than the LKG, every other file is too.
CHANGED_FILES=(
  "src/main.rs"
  # add specific files you've edited, e.g.:
  # "src/mcp/foo.rs"
  # "src/workflow_generation/bar.rs"
)
QUERY=$(printf "&path=%s" "${CHANGED_FILES[@]}" | sed 's/^&//')
# OPTIONAL but strongly recommended: the commit you are testing. Set it whenever
# you are verifying a fix that has LANDED (a merge sha, or the commit under test),
# and the probe additionally answers `git merge-base --is-ancestor $UNDER_TEST_SHA
# <lkg_sha>` server-side. Leave empty when you are only testing uncommitted edits.
UNDER_TEST_SHA=""            # e.g. "8d1e188e" or a full 40-char sha
# `if`, not `[ -n ... ] && ...` -- the one-liner returns non-zero on an empty sha,
# which aborts the recipe under `set -e` for the ordinary no-sha case.
if [ -n "$UNDER_TEST_SHA" ]; then QUERY="${QUERY}&contains=${UNDER_TEST_SHA}"; fi
# -f AND a checked status. Without -f, an error envelope parses to
# all_covered=None, which is not "True", so a 500 silently selects the
# ~25-minute rebuild path. -f alone only makes that loud -- the `||` is what
# stops it, because guessing the spawn mode off a failed probe is the whole
# defect class this recipe is guarding against.
COVERAGE=$(curl -fsS "http://localhost:9875/lkg/coverage?${QUERY}") \
  || { echo "ERROR: LKG coverage probe failed -- refusing to guess the spawn mode" >&2; exit 1; }
ALL_COVERED=$(echo "$COVERAGE" | python -c "import sys,json; print(json.load(sys.stdin).get('data',{}).get('all_covered'))")
echo "LKG coverage: all_covered=$ALL_COVERED"
# Per-file dump. file_newer_than_lkg_secs sign convention (self-documenting):
#   > 0 ⇒ file was edited AFTER the LKG was built ⇒ NOT covered (rebuild needed).
#   = 0 ⇒ exact tie ⇒ covered.
#   < 0 ⇒ file is older than the LKG ⇒ covered (its content is in the binary).
# `reason` carries a stable string ("covered_file_at_or_before_lkg",
# "not_covered_file_newer_than_lkg", "no_lkg_yet", "file_not_found") if you
# prefer machine-readable reasons over the boolean.
echo "$COVERAGE" | python -c "
import sys,json
d=json.load(sys.stdin).get('data',{})
print('  lkg_built_at:', d.get('lkg_built_at'))
for f in d.get('files',[]):
    secs = f.get('file_newer_than_lkg_secs')
    print(f\"  {f.get('path'):55s} covered={f.get('covered')} file_newer_than_lkg_secs={secs}s reason={f.get('reason')}\")
# COMMIT provenance — the only fields that answer 'which commits are in this
# binary'. Everything above is mtime and cannot. contains=None is UNKNOWN.
cp = d.get('commit_provenance') or {}
print('  commit_provenance: lkg_sha=', cp.get('lkg_sha'),
      ' source=', cp.get('lkg_source'),
      ' behind_origin_main=', cp.get('behind_origin_main'),
      ' is_ancestor_of_origin_main=', cp.get('is_ancestor_of_origin_main'),
      ' contains(', cp.get('contains_query'), ')=', cp.get('contains'), sep='')"

# 2. Decide spawn mode based on coverage.
REQUESTER="manual-test-$(date +%s)"
if [ "$ALL_COVERED" = "True" ]; then
  echo "All changes covered by LKG — spawning from cached binary (~5s)"
  SPAWN_BODY="{\"rebuild\": false, \"use_lkg\": true, \"wait\": true, \"wait_timeout_secs\": 90, \"requester_id\": \"${REQUESTER}\"}"
else
  # 2a. LKG misses some files — build the runner frontend first (Tauri embeds it
  #     at cargo build time), then trigger a rebuild via the build pool.
  echo "LKG missing some changes — frontend build + cargo rebuild required"
  cd <workspace-root>/ui-bridge-auto && npm run build 2>&1 | tail -3 || true
  cd <workspace-root>/qontinui-runner && npm run build 2>&1 | tail -3
  SPAWN_BODY="{\"rebuild\": true, \"requester_id\": \"${REQUESTER}\", \"queue_timeout_secs\": 600}"
fi

# 3. Probe the supervisor build pool BEFORE blocking — useful both for LKG
#    spawns (if a build is queued ahead, even use_lkg can be slow if the
#    supervisor is congested) and for rebuild spawns (warns if jammed).
# Informational, so a failure here is reported and NOT fatal. Note the defaults:
# a failed probe must render UNKNOWN, never "0/3 permits free" — that reads as a
# measured full pool, and it is the reading that makes the next step's 503 look
# expected. A shrug and a measurement are not the same value.
BUILDS=$(curl -fsS http://localhost:9875/builds) \
  || { echo "Build pool probe FAILED — reporting UNKNOWN, falling through to the fail-fast probe below" >&2; BUILDS='{}'; }
PERMITS=$(echo "$BUILDS" | python -c "import sys,json; print(json.load(sys.stdin).get('available_permits','UNKNOWN'))")
QUEUED=$(echo "$BUILDS" | python -c "import sys,json; print(json.load(sys.stdin).get('queued','UNKNOWN'))")
echo "Build pool: ${PERMITS}/3 permits free, ${QUEUED} queued"

# 4. Fail-fast probe with X-Queue-Mode: no-wait. If pool full AND all active
#    builds elapsed > 300s, bail (jammed). Otherwise fall through to blocking.
PROBE=$(curl -s -w "\n%{http_code}" -X POST http://localhost:9875/runners/spawn-test \
  -H "Content-Type: application/json" \
  -H "X-Queue-Mode: no-wait" \
  -d "$SPAWN_BODY")
PROBE_CODE=$(echo "$PROBE" | tail -n1)
PROBE_BODY=$(echo "$PROBE" | sed '$d')

if [ "$PROBE_CODE" = "200" ]; then
  SPAWN_RESULT="$PROBE_BODY"
elif [ "$PROBE_CODE" = "503" ]; then
  ALL_STUCK=$(echo "$PROBE_BODY" | python -c "
import sys, json
d = json.load(sys.stdin)
builds = d.get('active_builds', [])
print('true' if builds and all(b.get('elapsed_secs', 0) > 300 for b in builds) else 'false')
")
  if [ "$ALL_STUCK" = "true" ]; then
    echo "ERROR: Build pool appears jammed — all slots have elapsed_secs > 300s:"
    echo "$PROBE_BODY"
    exit 1
  fi
  echo "Build pool busy — falling back to blocking spawn..."
  SPAWN_RESULT=$(curl -fsS -X POST http://localhost:9875/runners/spawn-test \
    -H "Content-Type: application/json" \
    -d "$SPAWN_BODY") \
    || { echo "ERROR: blocking spawn-test failed -- there is no temp runner to test against" >&2; exit 1; }
else
  echo "ERROR: unexpected spawn-test response (HTTP $PROBE_CODE):"
  echo "$PROBE_BODY"
  exit 1
fi
echo "$SPAWN_RESULT"

# Extract the port and ID
TEST_PORT=$(echo "$SPAWN_RESULT" | python -c "import sys,json; print(json.load(sys.stdin)['port'])")
TEST_ID=$(echo "$SPAWN_RESULT" | python -c "import sys,json; print(json.load(sys.stdin)['id'])")
USED_LKG=$(echo "$SPAWN_RESULT" | python -c "import sys,json; print(json.load(sys.stdin).get('used_lkg', False))")
RUNNER_BASE="http://localhost:${TEST_PORT}/ui-bridge"
# Bind $BASE HERE, after RUNNER_BASE has been re-pointed at the TEMP runner.
# Taken any earlier it would snapshot the primary runner's :9876, which this
# arm forbids touching.
BASE="$RUNNER_BASE"
echo "Test runner at port $TEST_PORT (ID: $TEST_ID, used_lkg=$USED_LKG)"
```

**LKG path resolution:** the supervisor's `--project-dir` is `qontinui-runner/src-tauri/`, so paths in `?path=...` are resolved against that root. Pass `src/foo.rs`, NOT `qontinui-runner/src-tauri/src/foo.rs`. Absolute paths also work. Path-traversal escapes (`../../etc/passwd`) report `exists: false` rather than 400, so a list call doesn't bail mid-batch.

**When to rebuild even when LKG covers your files:** if you're testing the runner *frontend* (TS/React) — the embedded `dist/` is baked into the binary at build time, so a TS-only change still needs a `cargo build` to take effect. The LKG check above only covers Rust files; pass a frontend file path too if you want to be safe.

**Before you call a landed fix a REGRESSION, prove the binary contains it.** A
fix that is on `origin/main` but absent from the binary you are driving is
**deploy lag**, not a regression, and the two demand opposite responses — one is
"rebuild and re-test", the other is "reopen the bug". The mtime fields cannot tell
them apart; `commit_provenance` can:

```bash
# The commit under test — the merge sha, or the fix commit itself.
UNDER_TEST_SHA="<sha>"
# A failed probe is UNKNOWN and must SAY so -- feeding the empty body to json.load
# would die with a traceback, which reads as a broken recipe rather than a verdict.
PROV=$(curl -fsS "http://127.0.0.1:9875/lkg/coverage?contains=${UNDER_TEST_SHA}") \
  || { echo "UNKNOWN: provenance probe failed -- cannot classify deploy-lag vs regression" >&2; PROV=""; }
if [ -z "$PROV" ]; then echo "UNKNOWN: no provenance body"; else
echo "$PROV" | python -c "
import sys,json
cp=(json.load(sys.stdin).get('data',{}).get('commit_provenance') or {})  # envelope-ok: absence is the point -- an empty envelope collapses to {} and every field below then reads None, which this snippet's own table classifies as UNKNOWN rather than false
c=cp.get('contains')
print('lkg_sha=',cp.get('lkg_sha'),' behind_origin_main=',cp.get('behind_origin_main'),
      ' is_ancestor_of_origin_main=',cp.get('is_ancestor_of_origin_main'),sep='')
print({True:'IN THE BINARY -- an absent behaviour here IS a finding',
       False:'NOT IN THE BINARY -- deploy lag, NOT a regression; rebuild before judging',
       None:'UNKNOWN -- not computable; classify as UNKNOWN, never as either'}[c])"
fi
```

For a running runner rather than the LKG, the same question is answered by
`build_sha` on the supervisor's runner-status payload — `git merge-base
--is-ancestor <fix-sha> <build_sha>` — with `build_source_warning` non-null
whenever the compiled tree is behind or diverged from `origin/main`.

Three verdicts, three responses, and the third is not one of the first two:

| `contains` | Verdict | Response |
|---|---|---|
| `true` | the commit IS compiled in | an absent behaviour is a real finding — file it |
| `false` | the commit is provably ABSENT | **deploy lag.** Rebuild (`{"rebuild": true}`) and re-test before classifying anything; do not file a regression |
| `null` | **not computable — UNKNOWN** | classify as UNKNOWN and say which of the three causes applies (no `contains` asked / no `lkg_sha` / sha unknown to the local odb). Never collapse it into `false` |

**Build pool behavior:** The supervisor runs N=3 concurrent cargo builds (configurable via `QONTINUI_SUPERVISOR_BUILD_POOL_SIZE`), each in its own `CARGO_TARGET_DIR` (`qontinui-runner/target-pool/slot-{k}/`). `POST /runners/spawn-test` **blocks by default** until a slot frees — no retry loop needed. Pass `queue_timeout_secs` to bound the wait, or send `X-Queue-Mode: no-wait` to opt out of blocking (returns HTTP 503 with `{error: "build_pool_full", queue_position, active_builds: [...]}`). Use `GET /builds` for a live snapshot of pool occupancy.

Wait for the runner to be ready (poll health):
```bash
for i in $(seq 1 40); do
  result=$(curl -s -m 3 http://localhost:${TEST_PORT}/health 2>/dev/null)
  responsive=$(echo "$result" | python -c "import sys,json; print(json.load(sys.stdin).get('data',{}).get('responsive',False))" 2>/dev/null)
  if [ "$responsive" = "True" ]; then echo "Test runner ready!"; break; fi
  sleep 15
done
```

### Step 1.5: Injected transport launch (`--transport=injected` only)

Skip this step unless `--transport=injected` was passed. **This recipe drives a PROD page, so it registers against the qontinui-web relay on the target page's own origin** (`<origin>/api/ui-bridge`) — a prod page can only reach its own origin, so the Step-1 temp runner is not part of *this* drive path. You may skip the Step-1 spawn entirely for a pure injected prod run, or keep the temp runner for the other phases.

> **For a LOCAL or CI page, the temp runner IS a supported relay** — `--relay http://127.0.0.1:<TEST_PORT>/ui-bridge`, served since qontinui-runner #560 (see the note under "Injected transport" above). That is the per-runner-isolated path: no prod relay, no Cognito `--auth-token`, and the runner's heartbeat is lenient. Substitute that for `$RELAY_BASE` below and drop the `CURL_AUTH` header; everything else in the recipe is unchanged.

**Scheme branch (from the decision point above): both schemes use relay mode — the scheme decides the relay origin and whether auth is needed.** `http://localhost:3001` targets register against the local web relay with no token (local `UI_BRIDGE_REQUIRE_AUTH` gate is off); `https://` prod/staging targets register against the same-origin prod relay and REQUIRE `--auth-token` (the prod relay is Bearer-gated).

```bash
# REQUIRED precondition for injected mode: TARGET_URL (from --target-url).
WRAPPER_CLI="<workspace-root>/ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs"
# SAME-ORIGIN web relay — never a runner, never a foreign loopback, never bare /ui-bridge.
ORIGIN=$(python -c "from urllib.parse import urlparse; u=urlparse('$TARGET_URL'); print(f'{u.scheme}://{u.netloc}')")
RELAY_BASE="${ORIGIN}/api/ui-bridge"
# Bind $BASE HERE — in this arm the page under audit is the relay's tab, not
# any of the three bases defined at the top, and Phase 3.5 reads $BASE.
BASE="$RELAY_BASE"

# Auth branch: prod/staging relay is auth-gated (UI_BRIDGE_REQUIRE_AUTH=1, Bearer-only).
# Mint a Cognito operator IdToken via SRP (pycognito) from SSM creds. Do NOT echo values.
AUTH_ARGS=(); CURL_AUTH=()
case "$TARGET_URL" in
  https://*)
    export OP_EMAIL=$(MSYS_NO_PATHCONV=1 aws ssm get-parameter --name /qontinui/operator/email \
      --with-decryption --region eu-central-1 --query Parameter.Value --output text)
    export OP_PASSWORD=$(MSYS_NO_PATHCONV=1 aws ssm get-parameter --name /qontinui/operator/password \
      --with-decryption --region eu-central-1 --query Parameter.Value --output text)
    ID_TOKEN=$(python -c "
import os
from pycognito import Cognito
u = Cognito('us-east-1_rgTB9dbZ1', 'q6ns1a8bokf2np1mj8v8arl31', username=os.environ['OP_EMAIL'])
u.authenticate(password=os.environ['OP_PASSWORD'])
print(u.id_token)
")
    # NOTE: --auth-token puts the token on the inject CLI's argv — unavoidable
    # today (the CLI has no file/env door), and a known residual of the
    # credentials-off-argv rule. The curl half CAN be staged, so it is.
    AUTH_ARGS=(--auth-token "$ID_TOKEN")
    CURL_AUTH_HDR=$(mktemp); trap 'rm -f "$CURL_AUTH_HDR"' EXIT
    printf 'Authorization: Bearer %s\n' "$ID_TOKEN" > "$CURL_AUTH_HDR"
    CURL_AUTH_HDRP=$CURL_AUTH_HDR
    command -v cygpath >/dev/null 2>&1 && CURL_AUTH_HDRP=$(cygpath -w "$CURL_AUTH_HDR")
    CURL_AUTH=(-H @"$CURL_AUTH_HDRP")
    ;;
esac

# Prod-target safety gate: if TARGET_URL points at prod (qontinui.io), the run is
# read-ish (fill creds, observe authed DOM) but must NOT complete a destructive
# register/signup, and needs explicit operator confirmation before launching.
# (honor production-only-work — read-only nav.)

# Launch the inject CLI in the BACKGROUND (Variant B / relay mode is the default):
# it injects the engine into TARGET_URL, registers the tab against $RELAY_BASE,
# prints ONE JSON line to stdout {"tabId":..,"uiBridgeRegistered":..,"url":..} once
# ready, then STAYS ALIVE until SIGTERM/SIGINT so we can drive the tab via /control/*.
node "$WRAPPER_CLI" \
  --url "$TARGET_URL" \
  --relay "$RELAY_BASE" \
  "${AUTH_ARGS[@]}" \
  --ready-timeout 30000 \
  > /tmp/inject-cli.out 2> /tmp/inject-cli.err &
INJECT_PID=$!
echo "inject-cli PID=$INJECT_PID (relay=$RELAY_BASE, url=$TARGET_URL)"

# Parse the tabId from the CLI's single stdout JSON line (poll the file until it lands).
# FIXED WART (ui-bridge-headless >= 0.3.0, 2026-07-13): the launcher's internal
# registration-confirmation poll (waitForUiBridgeRegistration in
# packages/ui-bridge-headless/src/launcher.ts -> GET <relay>/tabs) now sends the
# --auth-token Bearer and surfaces a terminal 401/403 on stderr. On OLDER cached
# headless versions the poll was anonymous, so on an auth-gated relay stdout could
# report uiBridgeRegistered:false / tabId:null even though the in-page client DID
# register. The authenticated /tabs read below remains the truth either way —
# don't bail on a null stdout tabId.
INJECT_TAB_ID=""
for i in $(seq 1 20); do
  INJECT_TAB_ID=$(python -c "
import json, sys
try:
    for line in open('/tmp/inject-cli.out'):
        line=line.strip()
        if not line: continue
        d=json.loads(line)
        if d.get('tabId'): print(d['tabId']); break
except Exception:
    pass
" 2>/dev/null)
  [ -n "$INJECT_TAB_ID" ] && break
  sleep 2
done

# Fallback + confirmation: authenticated tabs read on the WEB relay (data.tabs[].tabId).
if [ -z "$INJECT_TAB_ID" ]; then
  INJECT_TAB_ID=$(curl -s "${CURL_AUTH[@]}" "$RELAY_BASE/tabs" \
    | python -c "import sys,json; t=json.load(sys.stdin).get('data',{}).get('tabs',[]); print((t[-1].get('tabId') or t[-1].get('id','')) if t else '')" 2>/dev/null)
fi

if [ -z "$INJECT_TAB_ID" ]; then
  echo "BLOCKED: inject-cli never registered a tab on the web relay within ~40s."
  echo "  Check /tmp/inject-cli.err and triage in this order:"
  echo "  1. 401s from the relay -> missing/expired --auth-token (prod relay is Bearer-gated;"
  echo "     IdTokens expire ~1h — re-mint and relaunch)."
  echo "  2. curl \$RELAY_BASE/tabs returns 404 -> wrong path, or a runner build predating #560."
  echo "     For a PROD page the relay is <page origin>/api/ui-bridge. A temp-runner relay"
  echo "     (http://127.0.0.1:<TEST_PORT>/ui-bridge) is valid for LOCAL/CI pages since"
  echo "     qontinui-runner #560 — a 404 there means a stale runner binary, so check"
  echo "     curl -s http://127.0.0.1:9876/health and drive a runner built from current main."
  echo "  3. inject-cli.err CLEAN + page console shows 'more-private address space' /"
  echo "     net::ERR_BLOCKED_BY_PRIVATE_NETWORK_ACCESS_CHECKS -> LNA on an https page against"
  echo "     a loopback relay. Since ui-bridge #104 the CLI auto-appends the LNA-disable flags"
  echo "     for exactly this case, so seeing it means they did not reach Chromium: check the"
  echo "     CLI's launch-args log line, or pass them yourself with --launch-arg."
  kill -TERM "$INJECT_PID" 2>/dev/null || true
  exit 1
fi
echo "Injected tab registered: tabId=$INJECT_TAB_ID — drive it via $RELAY_BASE/control/* (pin ?tabId=$INJECT_TAB_ID)"
```

Drive surface for the rest of the run (injected mode): every `snapshot` / `discover` / `element/<id>/action` / `page/navigate` / `page/evaluate` call goes to `$RELAY_BASE/control/*` (the web relay — NOT the temp runner), with `"${CURL_AUTH[@]}"` on every call when the gate is on. **Always pin `?tabId=$INJECT_TAB_ID`** — the prod relay is shared, so other tabs (the operator's own browser, a hidden primary tab) may be registered. Pinning caveat (ui-bridge plan item 1, fix in flight): a pinned read whose relay leg fails can today silently fall back to ANOTHER tab's cached elements under `success:true` — sanity-check responses against a tab-unique element (e.g. the login form) before trusting them.

> **Variant A (relay-free one-shot — optional, any scheme).** *If what you want is a **verdict** rather than raw action output, use the headless verdict arm above — it is Variant A plus normalization plus `vision-audit`, with a real exit code.* For a quick CI-style smoke that doesn't need a live parked tab — or when you can't mint an operator token — run the CLI with one or more `--exec '<action> <json>'` flags (repeatable) instead of `--relay`; it runs each action via the injected runtime in-page (no relay round-trip at all, so neither relay auth nor LNA applies) and prints `{"action","result"}` JSON lines, then exits. Use Variant A only for snapshot/find-level checks; the PASS-on-page gate still applies — a relay-free exec snapshot must itself show the authed DOM. If a run needs a full login + a parked, drivable authed session driven by another agent, the `ui-bridge-login-web` flow (`manual-test-coord.md` Phase 0.6) also registers against the page's same-origin relay.

### Step 2: Health checks

```bash
# Check temp Runner API health
curl -s http://localhost:${TEST_PORT}/health

# Check temp Runner UI Bridge
curl -s http://localhost:${TEST_PORT}/ui-bridge/control/snapshot

# Check Web frontend UI Bridge (if testing web)
curl -s https://qontinui.io/api/ui-bridge/control/snapshot
```

**If the temp runner fails to start:** Check supervisor logs, try spawning again. If the supervisor is down, use `dev-start.ps1 -Supervisor` as a last resort.

**If the Web frontend is unresponsive:** Use `dev-start.ps1 -Frontend` as a fallback.

### Step 3: Cleanup (ALWAYS do this when testing is complete)

```bash
# Injected transport only: SIGTERM the inject-cli first so its Chromium tab
# unregisters cleanly from the relay before we tear the runner down.
if [ -n "${INJECT_PID:-}" ]; then
  kill -TERM "$INJECT_PID" 2>/dev/null || true
  echo "inject-cli ($INJECT_PID) signalled to stop (clean teardown on SIGTERM)"
fi

# Stop the temp runner (auto-removed). -f so the echo below cannot claim a stop
# that returned 404/500 — a leaked temp runner holds a build-pool slot.
# if/then/else, not `a && b || c`: the exit code is the contract now, and in the
# `&&`/`||` form a failing `echo` would report a SUCCESSFUL stop as a failure.
if curl -fsS -X POST http://localhost:9875/runners/${TEST_ID}/stop; then
  echo "Test runner stopped"
else
  echo "WARNING: stop FAILED for ${TEST_ID} — it may still be running; re-check GET :9875/runners" >&2
  false
fi
```

**IMPORTANT:** Always stop the temp runner when testing is complete. Include the stop command in your final report. In injected mode, **also SIGTERM the inject-cli** (it stays alive until signalled) — leaving it running leaks a Chromium process.

## Phase 1: Explore the Application

### Injected transport (`--transport=injected`) — drive the bare page

When `--transport=injected` is active, the "application" under test is the bare pre-auth page now driven through the injected tab on the **web relay** (`$RELAY_BASE/control/*` — NOT the temp runner; pin `?tabId=$INJECT_TAB_ID` and send `"${CURL_AUTH[@]}"` on every call when the gate is on). Run this flow instead of the generic explore-then-task loop:

1. **Snapshot / discover the page.** `GET $RELAY_BASE/control/snapshot?tabId=$INJECT_TAB_ID` and `POST $RELAY_BASE/control/discover {}` (pin the tab in the body or query). The injected runtime waits for the DOM to **settle** (content painted + quiet, or a hard cap) before `ready()` returns, so on a client-rendered SPA (e.g. prod `qontinui.io/login`, a Next.js page) the first snapshot right after the CLI's ready line already sees the pre-auth controls — no manual poll needed. (Tune via `--settle-quiet`/`--settle-timeout`; `--no-settle` reverts to the old ready-only gate, which would need a poll.)
   - **If the target control mounts *lazily* (after unrelated chrome has already painted and settled — lazy-loaded login, SSR streaming, spinner-then-swap)**, settle can fire on the chrome before the form exists. Pass **`--expect-selector '<css>'`** (e.g. `#login`, `input[type=password]`) so the launcher waits for *that* element specifically. If it never appears before the settle cap, `ready()` fails with **`INJECTED_EXPECT_SELECTOR_UNMET`** — a clean BLOCKED signal, not a control-less page.
   - If the snapshot still comes back empty (`registration.totalRegistered: 0`, `elements: []`) or `ready()` throws `INJECTED_EXPECT_SELECTOR_UNMET` / `INJECTED_RUNTIME_NOT_SETTLED`: the inject failed, the page hydrates slower than the cap (raise `--settle-timeout`), or `--expect-selector` is wrong — re-check `/tmp/inject-cli.err` and that `--relay` was `<page origin>/api/ui-bridge` (never a runner) with `--auth-token` on a gated relay; treat "the expected control never appears" as **BLOCKED/UNVERIFIED**, not a pass. (Same observe-the-goal-on-the-page discipline the Phase 5 / step-5 PASS gate uses for the authed DOM.)
2. **Locate the fields** — `POST $RELAY_BASE/ai/find {"query":"Email or username"}` and `{"query":"Password"}`; match by visible label/placeholder (don't hardcode ids — they drift). For the submit, `{"query":"Sign In"}` (try "Log in"/"Continue" fallbacks).
3. **Fill credentials (from SSM)** — `POST $RELAY_BASE/control/element/<EMAIL_ID>/action {"action":"type","params":{"text":"<email>"}}` and the same for the password field. Use `/qontinui/operator/*` SSM creds (already exported as `OP_EMAIL`/`OP_PASSWORD` in Step 1.5 for https runs); never echo the values into the transcript.
4. **Submit** — `POST $RELAY_BASE/control/element/<SUBMIT_ID>/action {"action":"click"}`. Wait ~3s, then `discover` + `snapshot`.
5. **PASS gate = authed DOM ON THE PAGE.** The test PASSes **only** when the post-login `snapshot`/`find` shows the authed DOM rendered in the tab — a known post-login landmark (e.g. a "Workflows"/"Runs"/"Sign out" nav item or the operator avatar). A 2xx/redirect/registration/log signal is NOT a PASS. If the authed surface never renders (auth wall, relay drop, tab pruned), report **UNVERIFIED**, not PASS. This is the same binding rule as Phase 5's verification block — observe the goal on the page, never infer it.

**Prod safety reminder:** fill-and-observe on `qontinui.io/login` is acceptable read-ish behavior under explicit confirmation; **never** complete a destructive register/signup against prod.

If `--transport=injected` is NOT active, ignore this subsection and continue with the standard explore flow below.

Start by understanding what's available in the UI:

1. **Take a snapshot** of the current page state
2. **Discover all elements** with `interactive_only: false`
3. **Identify the current page/view** — what navigation items exist, what's active
4. **Map out the navigation** — click through nav items to understand the app's pages
5. **Note interactive elements** — buttons, inputs, forms, dropdowns, toggles, tabs

After each navigation action, wait 2 seconds, then re-discover and re-snapshot.

Build a mental map of the application:
- What pages exist
- What features are accessible
- What forms and workflows are available
- What data is displayed

## Phase 2: Perform Real Tasks

Using the application's UI, attempt to perform meaningful tasks. Choose tasks based on what the page offers. Examples:

### If on a Workflow Builder page:
- Create a new workflow
- Add steps to the workflow
- Configure step settings (fill form fields, toggle options)
- Save the workflow
- Navigate away and back to verify persistence

### If on a Dashboard/List page:
- Use filters and search
- Sort columns
- Click into detail views
- Use pagination

### If on a Settings/Config page:
- Change settings via dropdowns, toggles, text inputs
- Save settings
- Verify changes persisted after page refresh

### If on the Runner UI:
- Navigate between sections (terminal, workflows, settings)
- Interact with the terminal if available
- Browse workflow runs
- Check task run details

### General approach:
1. **Before each action:** Snapshot to know the current state
2. **Perform the action** via UI Bridge interaction endpoints
3. **After each action:** Wait, re-snapshot, verify the expected change occurred
4. **Track what worked and what didn't** — keep a running log

If `$ARGUMENTS` specifies a particular area or task to test, focus on that. Otherwise, exercise as many features as practical.

## Phase 3: Edge Cases & Stress Points

After basic tasks, probe edge cases:

- **Empty states:** Clear filters/inputs and see how the UI handles no results
- **Long text:** Type very long strings into inputs
- **Rapid actions:** Click the same button multiple times quickly
- **Navigation during loading:** Try navigating while data is loading
- **Form validation:** Submit forms with missing required fields
- **Disabled elements:** Verify disabled buttons/inputs cannot be interacted with
- **Scroll containers:** Find elements in scroll containers, verify `scrollIntoView` works
- **Console errors:** Check for JavaScript errors after each major interaction

## Phase 3.5: Visual Integrity (run on EVERY page you visit)

Phases 2 and 3 exercise behaviour; nothing in them looks at whether the page is
legible. A page can pass every functional check while a widget sits on top of
its labels — this phase exists because that is not hypothetical.

Run on each page, before moving on:

```bash
# 1. Occlusion sweep — what is covering what.
#    ⚠️ ASK IT OF THE PAGE YOU ARE AUDITING — hence $BASE, never a fixed
#    base. This endpoint answers about the surface it is SENT to, so pointing
#    it at $WEB_BASE while $BASE is the runner or mobile does NOT rescue the
#    check: it returns a healthy-looking 200 describing qontinui.io, a
#    confident verdict about a different application. That is worse than the
#    404 in the table below, because nothing in the response marks it as
#    off-target.
#    Only web surfaces serve the route. RUN THE PREDICATE — it derives that
#    from $BASE itself, so there is no second variable to keep in agreement.
#    The primary runner, a temp runner, mobile and an unbound $BASE all land
#    in the skip arm — PROVIDED the bases above were defined. BOTH patterns
#    carry a non-empty default for exactly that reason: `case` patterns are
#    literals, so with $BASE and $WEB_BASE both unbound, empty would match
#    empty and the probe would fire on the one setup this section warns about.
#    A web base you reached some other way belongs in $WEB_BASE or
#    $RELAY_BASE — set one of those, rather than editing this predicate.
#    Exit code settles TWO of the three UNVERIFIED outcomes: a skip and a 404
#    are both non-zero on purpose. It does NOT settle the third —
#    `unknown_empty_registry` is an HTTP 200, so read the `verdict` field too.
#    In the relay arm this probe also inherits that arm's pinning contract:
#    add "${CURL_AUTH[@]}" and ?tabId=$INJECT_TAB_ID, or a shared relay may
#    answer about whichever other tab it picked.
case "${BASE:-}" in
  "${WEB_BASE:-__no_web__}"|"${RELAY_BASE:-__no_relay__}")
    # -f so an HTTP error is a non-zero exit, -S so it still says why under -s.
    curl -fsS -X POST "$BASE/control/visibility" \
      -H 'Content-Type: application/json' -d '{"minRatio":0.02}' ;;
  *)
    echo "step 1 SKIPPED — \$BASE is not a web surface: occlusion UNVERIFIED, use step 2" >&2
    false ;;
esac
```

**Which surfaces serve `/control/visibility`:**

| Surface | Served? |
|---|---|
| Web SDK / qontinui-web relay (`$WEB_BASE`, `:3001/api/ui-bridge`) | **yes** — `@qontinui/ui-bridge` handler + relay twin. The relay variant uses the hit-test half only, so its findings are a SUBSET of the in-page endpoint's |
| Runner `:9876` (`$RUNNER_BASE`, Tauri WebView) | **no — 404.** The runner serves its own **Rust** route table (`qontinui-runner/src-tauri/src/mcp/ui_bridge/routing.rs`); the frontend's `@qontinui/ui-bridge` npm dependency does not put routes on that port |
| Mobile / React Native (`$MOBILE_BASE`, `@qontinui/ui-bridge-native`) | **no — 404**, pinned by `packages/ui-bridge-native/src/server/__tests__/http-status-mapping.test.ts` |

The two `404` rows describe what those bases return **if you send the probe
there anyway**. They are not an instruction to send it, and the fix for them is
not to retarget at `$WEB_BASE` — that swaps a loud 404 for a quiet answer about
the wrong application. On the runner and on mobile, step 1 is **skipped**.

- `verdict: "occlusions_found"` → each entry names the occluder, the occluded,
  and the fraction covered. An entry with `hidesText: true` is a **FAIL**, not a
  warning: a covered label destroys information the reader cannot recover.
- `verdict: "unknown_empty_registry"` → the registry was empty. UNVERIFIED, not
  clear.
- **A 404 is UNVERIFIED, never a pass.** On the runner and on mobile the route
  does not exist, and a web surface running an `@qontinui/ui-bridge` build older
  than 2026-08-27 (`4284cd29`) 404s it too. Do not drop the requirement: record
  the page's occlusion state as UNVERIFIED and get the signal from step 2's
  `analyze` `occlusion` finding instead, which works on every surface that can
  produce a snapshot.
- ⚠️ **Occlusion by a tracked modal/dropdown is NOT filtered out.**
  `includeExpected` is accepted and echoed back but never applied: commit
  `ccb77d2` deleted the `if (occ.isExpectedOverlay && !includeExpected) continue;`
  filter along with the `computeVisibility` import (a real package cycle —
  `ui-bridge-auto` depends on `ui-bridge`), and both the handler and its relay
  twin now hardcode `isExpectedOverlay: false`. **An open modal therefore reports
  `occlusions_found` with `hidesText: true`.** That is the tool working as
  currently built, not a page defect — triage such entries by hand against what
  you know is open before calling a FAIL. Restoring the filter needs a design
  decision about where `isExpectedOverlay` is computed; tracked in plan
  `2026-08-27-mobile-relay-followups-observability-and-sdk-contracts`.

For a page reachable **by URL**, prefer `scripts/verify-page-verdict.sh` —
it runs this same chain through the shipped qontinui-web adapter and cannot
drift from CI. The projection below is for the surfaces it cannot reach:
the runner's own Tauri WebView, where `/ui-bridge/control/discover` is the
only door to the element tree. Steps 2 and 3 below DO work against
`$RUNNER_BASE` — only step 1 is web-only, and on the runner and on mobile it is
**skipped**, never retargeted.

`verify-page-verdict.sh` covers the **step 2** analyzer chain. It produces no
`/control/visibility` verdict — the string appears nowhere in it — so on a web
page where you preferred it, step 1 is still yours to run (or to record as
UNVERIFIED). It is also not established to cover step 3: it gates on
`layout,typography,elements` and carries no truncation check of its own, and
`vision-audit`'s `text_fits_container` / `no_clipping` assertions — which it can
invoke via `--assertions` — are `assert`-mode, opt-in and needing an authored
spec.

```bash
# 2. Page-wide layout analysis (occlusion, overlap, zero-area, alignment).
#    NOTE the projection step — a raw discover payload piped into `analyze`
#    returns ZERO findings on a broken page. See the /visual-audit skill.
curl -s -X POST $BASE/control/discover -H 'Content-Type: application/json' \
     -d '{"interactive_only":false}' \
  | python3 "$ROOT/qontinui-claude-config/scripts/uibridge-to-elementsnapshot.py" \
      --stats > /tmp/snap.json
python3 -c 'import json;print(json.dumps({"analyzer":"layout","snapshot":json.load(open("/tmp/snap.json"))}))' \
  | curl -s -X POST $BASE/vision/analyze -H "Content-Type: application/json" -d @-
```

Read the `--stats` line on stderr. `0 with geometry` means every geometric
check reported nothing, which is **not** the same as no problems; `0 with
stacking order` means occlusion is UNKNOWN. An `occlusion_unknown` finding in
the output is likewise a statement of ignorance, never a pass.

```bash
# 3. Truncated labels — any element whose text is cut off.
python3 -c '
import json,sys
els = json.load(open("/tmp/snap.json"))["elements"]
bad = [e for e in els if e.get("scroll_width_px") and e.get("text")]
for e in bad:
    print(f"TRUNCATED {e[\"id\"]}: {e[\"text\"]!r} needs {e[\"scroll_width_px\"]}px, has {e[\"bbox\"][\"w\"]}px")
print(f"{len(bad)} truncated element(s)")'
```

A truncated label is a finding whenever the hidden part carries identity — a
session name, a file path, a branch. Report it with the full text and the
pixel shortfall; "the name is cut off" without those is not actionable.

This phase needs **no runner window**: the geometric checks are pure snapshot
work and the endpoints degrade rather than fail when frame capture is
impossible. A `frameError` in the response is informational, not a reason to
skip the phase.

## Phase 4: Cross-Feature Testing

If both Runner and Web are available:
- Compare the same features across both apps
- Verify shared UI components behave consistently
- Test SDK connection from Runner to Web frontend:
  ```bash
  curl -s -X POST http://127.0.0.1:9876/ui-bridge/sdk/connect -H "Content-Type: application/json" -d '{"url": "https://qontinui.io/api/ui-bridge"}'
  ```

## Phase 5: Report & Evaluate

> **Complete verification (binding).** A task is only PASS when the user-specified
> goal is **observed on the actual UI surface via the UI Bridge** — navigate to the
> page the goal concerns, `discover`/`snapshot`, and confirm the expected content is
> rendered there. NEVER infer PASS from proxy signals: API/HTTP responses, DB rows,
> coord/session/device registration, status endpoints, logs, or heartbeats. Those
> confirm *plumbing*, not the *goal*, and they routinely disagree with what the page
> shows (e.g. a coord API returning 3 sessions while the Live Sessions page renders
> "0 sessions" because of a tenant-scope mismatch). If the goal is "X appears on page
> Y," verification = seeing X on page Y through the UI Bridge — full stop. If the
> surface can't be reached (relay down, no connected tab, auth wall), report the goal
> as **UNVERIFIED**, not PASS — never substitute a backend check for the visual one.
>
> **PRESENT IS NOT VISIBLE.** `discover` returning an element is not proof a human
> can read it. An element covered by a floating widget is present in the payload
> with a valid bbox and correct text; so is one truncated to `qontinui-web-fro…`
> by a `truncate` class. This binding used to say "confirm the expected content is
> rendered there", and under that reading a page whose session names were hidden
> behind an overlay **passed** — which is exactly what happened to the runner's
> Terminal page, repeatedly, until a human noticed.
>
> So PASS additionally requires the element to be **unoccluded and untruncated**:
> `state.visible === true`, no `state.occludedBy`, and `scrollWidth <= clientWidth`
> on any element whose text carries the goal. Phase 3.5 above runs this as a sweep;
> for a single element the fields are on the `discover` payload directly.

After testing is complete (or after hitting significant blockers), produce a comprehensive report.

### Test Results

For each task attempted, report:

```
## Test Results

### Task: [Description]
- **Target:** Runner UI / Web Frontend
- **Steps taken:** [numbered list of UI Bridge interactions]
- **Result:** PASS / FAIL / PARTIAL / BLOCKED
- **Notes:** [what happened, any unexpected behavior]
```

### UI Bridge Evaluation

This is the most important section. Provide honest, specific feedback:

#### What Worked Well
- Which UI Bridge features were most useful for manual testing?
- Which interactions worked smoothly and reliably?
- What patterns made testing efficient?

#### Pain Points & Friction
- What was difficult or frustrating when using the UI Bridge for testing?
- Where did you have to work around limitations?
- What took more steps than it should have?
- Were there moments where you were unsure which endpoint to use?

#### Bugs & Issues Found
- Any UI Bridge endpoints that returned errors or unexpected results
- Any elements that couldn't be interacted with
- Any timing/staleness issues
- Any discrepancies between reported state and actual state

#### Missing Functionality — What Would Make Testing Easier
Be specific and actionable. For each suggestion:
- **What's missing:** Describe the capability
- **Why it matters:** What task was harder without it
- **Proposed solution:** How it could work

Examples of things to evaluate:
- Is there a way to wait for a specific element to appear? If not, should there be?
- Can you assert on element state without manual JSON parsing?
- Is form filling efficient or does it require too many individual calls?
- Can you easily test keyboard navigation and tab order?
- Is there a way to test responsive behavior?
- Can you test animations, transitions, or loading states?
- Is error recovery smooth when the app becomes unresponsive?
- Would a "record and replay" mode be useful?
- Would batch action execution help (multiple actions in one call)?
- Is the element discovery reliable or do elements get missed?

#### Discoverability Rating (1-5)
How easy was it to figure out which UI Bridge commands to use for each task?
- 5: Intuitive, obvious which command to use
- 4: Minor confusion, but commands are findable
- 3: Required reading docs carefully, some trial and error
- 2: Significant confusion, many wrong attempts
- 1: Could not figure out how to do basic tasks

#### Effectiveness Rating (1-5)
How effective was the UI Bridge at performing manual testing tasks?
- 5: Could do everything needed, no limitations
- 4: Minor gaps, but workarounds exist
- 3: Some tasks were difficult or impossible
- 2: Major limitations prevented meaningful testing
- 1: UI Bridge was not useful for manual testing

#### Overall Summary
A concise paragraph summarizing:
- The overall health of the application tested
- The most significant issues found (if any)
- The top 3 improvements that would make the UI Bridge better for manual testing

## Phase 6: Design Remediation Plan

**This is the final step and is mandatory.** After the evaluation in Phase 5, design a concrete plan to address **every deficiency** discovered during testing — both in the tested repositories (runner, web frontend, mobile, backend) and in the UI Bridge itself. The goal is a plan that can be handed to an implementation agent (or the user) and executed without further discovery.

### Inputs to the plan

Draw from everything surfaced in Phases 1–5:
- **Bugs & issues found** (Phase 5: Bugs & Issues Found) — broken endpoints, interaction failures, stale state, console errors, crashes
- **Pain points & friction** (Phase 5: Pain Points & Friction) — workarounds, multi-step sequences that should be one call, discoverability gaps
- **Missing functionality** (Phase 5: Missing Functionality) — capabilities the UI Bridge lacks that would make testing easier
- **Task failures** (Phase 5: Test Results) — any task that came back FAIL / PARTIAL / BLOCKED

If Phase 5 turned up no deficiencies, say so explicitly and skip to the "No deficiencies" note below.

### Plan structure

Produce a plan with the following sections. Group items by **which codebase owns the fix**, not by severity — the owner is what an implementer needs first.

```
## Remediation Plan

### Summary
- Total deficiencies: N (X bugs, Y friction points, Z missing features)
- Repositories touched: [list]
- Estimated ordering: [what should be done first, and why]

### qontinui-runner (Rust / Tauri)
For each deficiency owned by the runner:
- **Issue:** [one-line description, referencing the Phase 5 finding]
- **Root cause (hypothesis):** [where in the code this likely lives — file paths / modules if known]
- **Proposed fix:** [concrete change — new endpoint, modified handler, fixed state, etc.]
- **Verification:** [how to confirm the fix works — specific UI Bridge call + expected response]
- **Priority:** P0 (blocker) / P1 (fix soon) / P2 (nice-to-have)

### qontinui-web (Next.js / FastAPI)
[same structure]

### qontinui-mobile (React Native)
[same structure]

### UI Bridge (ui-bridge-auto / ui-bridge-mcp / runner's ui-bridge endpoints)
Treat this as first-class — the UI Bridge is the testing surface itself, and gaps here compound across every other repo.
[same structure, plus:]
- **Category:** new endpoint / new action / better filtering / better error messages / discoverability / batching / waiting primitives / etc.

### Shared packages (workflow-ui, workflow-utils, shared-types)
[only if applicable]

### Cross-cutting / Documentation
- Doc updates needed (CLAUDE.md, knowledge-base, slash command files)
- Memory entries worth saving for future testing sessions
```

### Sequencing

After the per-repo sections, add an **Execution order** section — a numbered list showing the recommended order of operations. Consider:
- **Dependencies:** UI Bridge gaps that block verifying other fixes go first
- **Blast radius:** Schema / shared-types changes go before consumers
- **Blocker severity:** P0 bugs jump the queue regardless of repo
- **Batching:** Changes to the same file / module should be grouped into one PR

### Output rules

- **Be concrete, not aspirational.** "Add a `/control/wait-for-element` endpoint with `{selector, timeout_ms}` params that polls the registry" — not "improve waiting support."
- **Reference real file paths** where possible. If you touched a file during testing or saw it in an error, cite it. If you don't know, say "location unknown — to be identified."
- **Do not implement the plan.** Phase 6 designs the plan only. Implementation is a separate task unless the user explicitly asks to execute.
- **Do not scope-creep.** Only include items grounded in a Phase 1–5 finding. Do not add speculative improvements you didn't encounter.
- **If no deficiencies were found,** write: "No deficiencies surfaced during this test run. No remediation plan is needed." and stop — do not invent work.

### Delivery

**Write the remediation plan to a plan file, then vet it.** Do NOT merely append it to the chat — persist it so it survives the session and can feed `/implement-plan`.

1. **Pick a path.** `$QONTINUI_PLANS_DIR/<YYYY-MM-DD>-<kebab-slug>.md`, where the slug summarizes the remediation (e.g. `2026-05-30-sessions-lifecycle-cleanup`). Use today's date.

   `$QONTINUI_PLANS_DIR` is the directory plans live in — the same one `/implement-plan` and `/vet-plan` read from. The qontinui runner injects it into agent sessions from its `paths.plans_dir` setting; a session launched outside the runner will not have it. **If it is unset, ask the user once where plans live, or DISCOVER one: from the workspace root, `ls -d plans */plans 2>/dev/null` and use the directory that actually exists** — say which, and ask when it finds none or more than one. Never fall back to a directory you have not confirmed is there; a named fallback fails silently on every machine that does not have it. Never assume an absolute path from another machine, and say in your report which directory you actually wrote to. Resolve it to a real absolute path before step 3 — the vetting subagent must receive a concrete path, not an unexpanded variable.

2. **Write the file with the Write tool.** It MUST begin with an H1 title and a single status blockquote so `/vet-plan` and `/implement-plan` can parse it:
   ```markdown
   # <Remediation plan title>

   > **Status: DRAFT <YYYY-MM-DD>.** Remediation plan authored from a
   > /manual-test run of <target> on <date>. <one-line scope summary>.

   ## Origin
   <2-4 lines: what was tested, the user-visible goal, and the PASS/FAIL verdict that motivated this plan — cite the Phase 5 finding(s).>
   ```
   Follow that with the FULL Phase 6 plan body — the `## Remediation Plan` structure above (Summary, per-repo sections grouped by owning codebase, and the **Execution order** section). Keep every item grounded in a Phase 1–5 finding (the Output rules still apply). If you already drafted the plan text in your Phase 5/6 response, write that same content to the file — don't re-derive it.

3. **Vet it with a subagent.** Once the file is written, launch an Agent (general-purpose) to run the `/vet-plan` skill on the absolute plan path. The agent prompt must: invoke the `/vet-plan` Skill with the plan file path as its argument, let it run to completion, and report back the vet verdict (the status block `/vet-plan` stamped — VETTED / DRAFT-with-issues / etc.) plus any open questions or blocking concerns it surfaced. Use a subagent (not an inline Skill call) so the vetting work stays out of the main test-report context.

4. **Relay the outcome.** In your final response, give the plan file path, a condensed summary of the plan (repos touched + ordered steps), and the subagent's vet verdict. Do not implement the plan — Phase 6 designs and vets only; implementation is a separate `/implement-plan` run.

**No-deficiencies case:** if Phase 5 surfaced no deficiencies, write NO plan file and do NOT vet — just state "No deficiencies surfaced during this test run. No remediation plan is needed." (per the Output rules above) and stop.

## Related Skills

- **`/manual-test-coord`** — runner↔coord integration test. Drives the operator dashboard at `demo.staging.qontinui.io` via UI Bridge through pair-confirm, heartbeat, WS, tenant isolation, and dispatch round-trip. Cycles slower (~5–10 min/iter; touches ECS staging + RDS). Two-machine concurrent-session model with cross-tenant isolation invariant via `--rendezvous-slug`. Use for verifying multi-device coordination, not single-runner UI correctness.
- **`/manual-test-coord-loop`** — operator-triggered loop wrapper for `/manual-test-coord`. Caps at 4 iterations.
- **`/verify-web`** — the local **authenticated** qontinui-web specialization: stands up a hermetic stack + IdP, opens an admin-capable tab, and runs the same `verify-page-verdict.sh` analyzer arm against it. Use it when the page you need is behind the `(app)` auth wall; use the headless verdict arm here for everything else.
- **`/visual-audit`** — the declarative form of Phase 3.5: five analyzers plus the
  assertion DSL, with baselines for regression comparison. Use it when you want to
  PIN a page's layout, not merely sweep it once.
- **`/page-health`** — first-glance page sanity (empty content areas, stuck loading,
  error signals). Complementary: it has no geometric pair check, so it will not see
  one element covering another.

## Rules

- **NEVER infer the user's goal from backend data** — PASS requires observing the goal rendered on the actual page via the UI Bridge (`discover`/`snapshot`). An API/DB/registration/coord/log signal confirms plumbing, not the goal; when they disagree, the page wins. Goal unreachable on the surface → report UNVERIFIED, never PASS.
- **NEVER conclude "UI cannot be verified from this box".** No display, no `pwsh` and no `docker` rule out the temp-runner and `/verify-web` arms — they do **not** rule out the headless verdict arm, which needs only node, a Playwright Chromium and a URL. Run the arm-selection predicate; do not decide by feel. Shipping a UI change on unit tests alone because the box looked bare is the specific failure this skill now forecloses.
- **NEVER restart, kill, or rebuild the primary runner** — the supervisor spawns temp runners independently
- **NEVER rebuild the primary runner to test changes** — the supervisor has its own build pool and builds from source directly
- **ALWAYS spawn a temp runner via the supervisor (port 9875)** at the start of testing and stop it when done
- **NEVER ask the user for input** — make reasonable assumptions and proceed
- **NEVER report "needs a rebuild" and stop** — spawn a temp runner with `{"rebuild": true}` on the supervisor
- **ALWAYS use the temp runner's port** for all Runner UI Bridge calls (not 9876)
- **ALWAYS stop the temp runner** when testing is complete — include the stop command in your report
- **ALWAYS snapshot before and after interactions** to track changes
- **ALWAYS run Phase 3.5 on every page you visit** — an element being in the
  `discover` payload is not evidence a human can read it, and no functional
  check in Phase 2 or 3 looks at legibility
- **ALWAYS call discover** if snapshot returns empty or stale data
- **ALWAYS wait 2 seconds after navigation/clicks** that change the view, then re-discover
- **ALWAYS use scrollIntoView** before interacting with off-screen elements
- **If the temp runner crashes:** Spawn a new one — don't touch the primary runner
- **If an element can't be found:** Try discover with `include_hidden: true`, or use AI find
- **If an action fails:** Check console errors, try alternative approaches, log the failure
- **Be thorough but practical** — don't spend more than 3 attempts on a single failing interaction before logging it and moving on
- **Track everything** — every interaction attempted, every result observed
- **Be fully autonomous** — the user should not need to intervene at any point during the test

### Runner Coordination Rules
- **There is NO primary-runner lock.** `runner_lock.py` / `runner_status.py` / the whole `runner_coordination/` directory do not exist (verified 2026-07-21). Do not attempt to acquire or release a lock, and do not report having done so.
- **Default to a temp runner.** With no lock available it is the only way to test without racing other sessions.
- **If you must touch the primary:** confirm idle via `GET /restart-readiness` (`safe_to_restart` must be `true`) plus supervisor `GET /runners`, and declare scope with `coord_declare_intent` / coord claims so peers can see you. Not `/task-runs/running` — it returned `[]` with 25 sessions live (2026-08-29).
- **NEVER restart the primary runner yourself** — see CLAUDE.md "Runner Lifecycle". Only the operator may restart an active runner.
- **Temp runners do not need coordination** — they are isolated by design, but note `settings.json` is machine-shared (see CLAUDE.md), so avoid gratuitous temp runners while the primary is live.

## Test Focus

$ARGUMENTS
