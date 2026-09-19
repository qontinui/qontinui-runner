# Verify Web — Autonomous Local Web UI-Bridge Verification

Bring up an **instrumented, authenticated, admin-capable** local qontinui-web
session and drive it through the UI Bridge — the web analogue of a temp runner
(`POST :9875/runners/spawn-test`). Use this to verify a web frontend change (an
`(app)` page, an admin page) the same autonomous way a runner UI change is
verified. **Work completely autonomously — never ask the user to log in, restart,
or rebuild anything.**

**Why this exists:** the interesting web pages (e.g. `/admin/coord/onboarding-status`)
sit behind the Cognito `(app)` auth wall, so a bare UI-Bridge relay call returns
a `/login` DOM or a `503 NO_BROWSER_CONNECTED`. This flow stands up a hermetic
local IdP + a local-auth backend + a freshly-built local coord, then opens a tab
that passes BOTH auth gates with no human in the loop.

Sibling skill: `/manual-test` (general UI-Bridge testing, runner + prod web).
This skill is the **local admin-capable web** specialization.

---

## Prerequisites (checked, not assumed)

1. **Canonical qontinui-stack up** — the local coord + local-auth backend both
   run against the canonical Postgres (host `:5433`, container-net `postgres:5432`)
   on the docker network `qontinui-stack_default`, plus canonical Redis + NATS.
   Confirm: `docker ps` shows `qontinui-canonical-postgres`, `qontinui-canonical-redis`,
   `qontinui-canonical-nats-1`. If absent, start the stack first.
2. **Dedicated port block — coexists with the canonical dev stack.** The whole
   verify-web stack runs on ports chosen NOT to collide with canonical services:
   local coord `:9871` (canonical fleet coord is `:9870` — never fought), local
   backend `:8011` (canonical `:8000`), local frontend `:3011` (canonical
   `:3001`), local IdP `:8770`. This flow runs its OWN coord container
   (`qontinui-verifyweb-coord`) on `:9871`; only one container can publish that
   port. `Start-LocalCoord` refuses (with an actionable message) rather than
   killing anything that already holds it — but since `:9871` is dedicated, that
   guard should almost never trip. The canonical coord on `:9870` is left
   untouched.
3. **qontinui-web Phase-1 local-auth seam.** The backend flag
   `QONTINUI_DEV_LOCAL_AUTH` + the client dev-auth helper (gated by
   `NEXT_PUBLIC_ENABLE_DEV_LOCAL_AUTH`) are the load-bearing seam. See "Phase-1
   contract" at the bottom — if the helper's localStorage key differs, update
   `DEV_TOKEN_LS_KEY` in `scripts/verify-web-open-tab.sh`.

---

## The happy path

### Step 1 — Bring up the stack

```powershell
# From the project root (dev-start.ps1 is symlinked there):
.\dev-start.ps1 -VerifyWebStack
```

This runs, in order: docker deps → local IdP (mints the dev token + serves JWKS
on `0.0.0.0:8770`) → **local coord** (built from current source, `coord_local`
DB migrated by web alembic, trusting the local IdP, admin granted to
`dev-local@no-reply.qontinui.io` via `COORD_SSO_BOOTSTRAP_ADMIN_EMAILS`) →
**web backend in local-auth mode** on `:8011` (`QONTINUI_DEV_LOCAL_AUTH=1`,
`COGNITO_ISSUER=http://127.0.0.1:8770`, `COORD_URL=http://localhost:9871`,
isolated `qontinui_web_local` DB) → **frontend** on `:3011`
(`NEXT_PUBLIC_ENABLE_DEV_LOCAL_AUTH=1`, `NEXT_PUBLIC_API_URL=http://localhost:8011`).
`-Status` shows the `Local IdP`, `Local Coord`, `Local Backend`, and
`Local Frontend` rows.

`-VerifyWebStack` exits **5** when a stage or a liveness probe fails; read the
`VERIFYWEB-STACK FAILED: ...` line in `.dev-logs/verifyweb-stack.log` for which.

### Step 2 — Liveness preflight (turn the silent 502 into a clear message)

`/operations/*` (which admin pages call) proxies to the local coord; if nothing
listens there the backend returns a bare `502 coord is not reachable`. Probe
the stack first. On Windows, where `dev-start.ps1` runs:

```powershell
.\dev-start.ps1 -VerifyWebProbe
# Read-only. One PROBE <svc> PASS|FAIL line each for idp (:8770 JWKS + minted
# token), coord (:9871 /health), backend (:8011 /health) and frontend (:3011 /),
# plus VERIFYWEB-PROBE markers in .dev-logs/verifyweb-stack.log.
# Exit 0 = all pass. Exit 5 = a FAIL: do NOT proceed to admin-page assertions.
```

`dev-start.ps1` is Windows-by-substance, so off Windows check at least the coord
by hand (the local coord is on the dedicated `:9871`, NOT canonical `:9870`):

```bash
if curl -fsS http://localhost:9871/health >/dev/null 2>&1; then
  echo "local coord OK"
else
  echo "local coord NOT reachable — start it:  dev-start.ps1 -VerifyWebStack" >&2
  # do NOT proceed to admin-page assertions; they will 502.
fi
```

(`dev-start.ps1 -Status` performs the same check via `Test-LocalCoordHealth`.)

### Step 3 — Open the authenticated tab

```bash
RESULT=$(bash qontinui-claude-config/scripts/verify-web-open-tab.sh \
  --page /admin/coord/onboarding-status)
echo "$RESULT"

# jfield — read one top-level field from the result line. The opener runs
# without jq (it falls back to python); this reader must too, or the workflow
# still dies on a box with no jq even though the script it consumes now works.
#
# The key comes in through the NAMED variable JF_KEY, set as a prefix assignment
# on the call itself (see the three call sites below). It must NEVER be a shell
# positional parameter — a dollar sign followed by a single digit in a
# slash-command markdown body is a HARNESS ARGUMENT PLACEHOLDER, not a shell
# positional: Claude Code substitutes the invocation's argument words into this
# body BEFORE injecting it into the session, indexed from ZERO (the zeroth
# placeholder is the FIRST word), and leaves unfilled positions LITERAL.
#
# Be precise about what that cost here, because the honest version is what makes
# the rule stick. /verify-web declares no arguments today, so every real
# invocation left the old placeholder unfilled, it stayed literal, and — because
# it sat inside a shell FUNCTION, where it read that function's own argument —
# the reader worked. It was LATENT, not live-broken. One stray argument word is
# all it took to break, and then it broke QUIETLY: the filter read a field named
# after an argument word, so an ordinary word made jq return the literal string
# `null`, which is what would have been assigned to the tab id, the inject pid
# and the relay; only a keyword- or flag-shaped word (measured: a word starting
# with a dash) errors out loudly, and the python fallback returns the empty
# string instead. "This command takes no arguments" is not a reason to leave a
# placeholder in an executable body. Named variables are not substituted at all.
# (This comment deliberately spells no such sequence of its own — a literal one
# here would be substituted too, garbling the warning.)
jfield() {
  # Fail LOUD on an unset key rather than silently reading the whole object: an
  # empty jq filter is `.`, which would assign the ENTIRE result JSON to TAB_ID.
  # That is the silent-wrong class this whole change exists to remove, so the one
  # way back into it — someone restoring the old `jfield <key>` call form from
  # muscle memory — gets a named cause instead. Same discipline as the named-var
  # precondition in the red-main detector.
  [ -n "${JF_KEY:-}" ] || { echo "jfield: JF_KEY is unset — the key is a NAMED variable, never a positional argument (call it as: JF_KEY=tabId jfield)" >&2; return 2; }
  if command -v jq >/dev/null 2>&1; then jq -r ".$JF_KEY"
  else python -c 'import json,sys; print(json.loads(sys.stdin.buffer.read().decode("utf-8")).get(sys.argv[1],""))' "$JF_KEY"
  fi
}
TAB_ID=$(printf '%s' "$RESULT"     | tail -n1 | JF_KEY=tabId jfield)
INJECT_PID=$(printf '%s' "$RESULT" | tail -n1 | JF_KEY=injectPid jfield)
RELAY=$(printf '%s' "$RESULT"      | tail -n1 | JF_KEY=relay jfield)
```

The opener reads the dev token from `.dev-logs/verifyweb/id_token.jwt`, seeds a
Playwright storageState (the `qontinui_auth` marker cookie + the token in
localStorage for the Phase-1 helper), launches Chromium via the shipped
`ui-bridge-inject` CLI, registers the tab against
`http://localhost:3011/api/ui-bridge`, and returns a `tabId` + `injectPid`. The
CLI **parks** (keeps the tab live) until SIGTERM.

### Step 4 — Assert the admin-page DOM (the PASS gate)

```bash
# NOTE: the two filters below are the only remaining jq dependency in this
# workflow, and they are human-inspection aids rather than gates. On a box with
# no jq, pipe the same curl output to
#   python -c 'import json,sys; d=json.load(sys.stdin); print(d["route"], len(d["elements"]), sum(1 for e in d["elements"] if "connected organizations" in (e.get("text") or "").lower()))'
# for the first, and `python -m json.tool` for the second.
#
# Snapshot is AUTHORITATIVE — must NOT be a 503 NO_BROWSER_CONNECTED and must
# NOT read as /login. Assert the route AND that the Connected Organizations card
# is in the DOM (grep the snapshot elements — this is the reliable check).
curl -s "$RELAY/control/snapshot?tabId=$TAB_ID" \
  | jq '{route, elementCount: (.elements|length),
         connectedOrgs: ([.elements[]? | select(.text? // "" | test("Connected organizations"; "i"))] | length)}'

# Optional secondary probe — the find route is POST /control/find (NOT
# /control/ai/find, which is UNKNOWN_ROUTE). Note its query semantics can return
# 0 matches even when the card is present, so the snapshot above is the gate.
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"query":"Connected organizations"}' \
  "$RELAY/control/find?tabId=$TAB_ID" | jq '.'
```

**PASS = the authed admin DOM observed ON THE PAGE:** `route` is
`/admin/coord/onboarding-status` (NOT `/login`), and the snapshot's
`connectedOrgs` count is ≥1 (the `ConnectedOrgs` card — "Connected
organizations", with either "N repositories enrolled" or "connected · no
repositories enrolled yet"). A 2xx alone is NOT a pass — read the returned DOM.
(`POST /control/find` is a best-effort convenience; a 0-match result there does
NOT fail the gate when the snapshot shows the card.)

That gate answers **"is the right page there?"**. It does not answer **"does it
look right?"** — it is a hand-written substring check on one snapshot, and it
will happily pass a page whose controls overlap, whose text is clipped, or whose
targets are unreachably small. Step 5 is the half that answers the second
question.

### Step 5 — Run the analyzer for a real verdict (the second half of the gate)

`scripts/verify-page-verdict.sh` carries an arbitrary route through to a
`vision-audit` verdict: it captures the page headlessly through the injected
transport, normalizes the snapshot with qontinui-web's own shipped
`normalize.ts`, runs the analyzer **built from the SHA `style-gate.lock` pins**,
and prints machine JSON on stdout plus a summary on stderr. This is the step
that turns "a tab" into "a verdict", and it is the reason a headless session no
longer has to ship a UI change on unit tests alone.

```bash
# Same authenticated origin, same storageState the opener already seeded.
# --storage-state is what carries the (app) auth wall past; everything else is
# identical to auditing a bare public page.
bash qontinui-claude-config/scripts/verify-page-verdict.sh \
  --url "http://localhost:3011/admin/coord/onboarding-status" \
  --storage-state .dev-logs/verifyweb/storage-state.json \
  --expect-selector '[data-testid=connected-orgs]' \
  > /tmp/verify-web-verdict.json
VERDICT_RC=$?
```

Exit codes are distinct on purpose — read the status, do not just grep the JSON:

| Exit | Meaning | What to do |
|---|---|---|
| `0` | PASS — no gated finding at or above `--fail-on` | proceed |
| `2` | **GATE FAILED** — real findings; each names the offending element ids | fix the page, re-run |
| `3` | **CAPTURE FAILURE** — the page was never observed | never read this as a pass; see the failure-mode table |
| `4` | analyzer unavailable at the pinned SHA | the error names the exact build command |
| `127` | local environment fault (node too old, normalize.ts absent) | fix the box |

The verdict JSON stamps `analyzer.pinnedSha` and `analyzer.shaVerified`. **Quote
the SHA in your report.** A verdict from an unpinned analyzer answers a
different question than the Style Gate does, and `shaVerified:false` says so.

**Interactions.** A state only reachable by clicking is reachable here too:
`--step '<action> <json>'` and `--capture <id>` interleave in order, so one page
load can yield several audited states (a closed panel, the open panel, the panel
with details expanded). The transport is `ui-bridge-inject --exec-stdin`; the
action surface is the shipped one in
`ui-bridge/packages/ui-bridge/src/react/commandHandlers.ts`. **If the action you
need does not exist, that is a UI Bridge gap — fix it in the Bridge and report
it. Never route around it with a Playwright locator.**

**`analyze` is the default and needs no spec** — that is the point: you can
point it at the change you just made with nothing prepared. `vision-audit
assert` is opt-in via `--assertions <file>`, and it presupposes an *authored*
spec against *known element ids* (`{"type":"no_overlap","elements":["a","b"]}`);
omit the ids and it is a **parse error, exit 1** — not a soft skip and not a gate
failure. An `assert` PASS means "every assertion in the spec held", never "the
page is fine", and the arm says so in its own `coverageNotes`. A fresh route has
no committed baseline, so `no_layout_shift_since` has to be authored and
baselined before it can say anything at all.

Colour is deliberately **reported but never gated**: only the `color` analyzer
reads pixels, this arm ships no screenshot capability (it feeds an obviously
named 1×1 placeholder frame), and the parent plan's burn-in measured
pixel-sampled contrast producing false 1.01:1 criticals. Layout, typography and
elements are pure geometry over the snapshot and need no pixels at all.

### Step 6 — Teardown (ALWAYS)

```bash
kill -TERM "$INJECT_PID" 2>/dev/null || true   # release the Chromium tab
```
```powershell
.\dev-start.ps1 -StopVerifyWeb                 # backend + frontend + local coord + IdP
# Docker deps are left running; -StopDocker to stop them too.
# Exit 5 = a port (:3011/:8011/:8770) is still held, or ownership could not be proven.
```

Leaving `injectPid` running leaks a Chromium process — always SIGTERM it, and
report the teardown in your final summary.

---

## Failure modes (recognize, don't re-debug)

| Symptom | Meaning | Fix |
|---|---|---|
| `control/snapshot` → **503 `NO_BROWSER_CONNECTED`** | No relay client registered for that `tabId` — the tab never registered or already exited. | Re-check `.dev-logs/verifyweb/inject-cli.err`; confirm the frontend is up on `:3011` and `injectPid` is still alive. Re-run Step 3. |
| Snapshot `route` reads **`/login`** | The tab bounced past the edge cookie gate but the client `AppAuthGate` never got a user — the Phase-1 dev-auth helper didn't run or didn't find the token. | Confirm the frontend was started with `NEXT_PUBLIC_ENABLE_DEV_LOCAL_AUTH=1` (it is under `-VerifyWebStack`), and that `DEV_TOKEN_LS_KEY` matches the helper's key. |
| Admin data call → **502 `coord is not reachable`** | Local coord isn't listening on `:9871`. | Step 2 preflight; `dev-start.ps1 -VerifyWebStack` (the local coord binds the dedicated `:9871`, not canonical `:9870`). |
| Admin data call → **403** | Coord is up but the dev identity isn't a tenant admin. | Confirm coord ran with `COORD_SSO_BOOTSTRAP_ADMIN_EMAILS=dev-local@no-reply.qontinui.io` (it does under `Start-LocalCoord`). Admin is granted on first login — re-open the tab so a fresh login runs. |
| inject-cli err shows `INJECTED_EXPECT_SELECTOR_UNMET` | The `--expect-selector` you passed never mounted before the settle cap. | Raise `--settle-timeout`, or drop `--expect-selector` and snapshot after a short wait. |
| `verify-page-verdict.sh` → **exit 3, "produced NO output (exit 0)"** | The inject CLI was invoked through its **bin symlink**. `isMain` compares `process.argv[1]` to the module URL without realpath, so under `node_modules/.bin/ui-bridge-inject` (= what `npx … ui-bridge-inject` resolves to) `main()` never runs: exit 0, zero output. | The script dereferences symlinks by default, so this only appears with `--no-deref-cli` or a wrapper that re-symlinks. Point `--inject-cli` at the real `dist/inject-cli.cjs`. **Never read the exit-0-no-output case as a pass** — that is what exit 3 exists to prevent. |
| `verify-page-verdict.sh` → **exit 3, "N step(s) errored"** | A `--step` could not be performed (commonly `ELEMENT_NOT_FOUND`). The state analyzed would not be the state requested. | Fix the step's element id (read one from a no-step run's `textSample`/snapshot). If the *action* is missing rather than the element, that is a UI Bridge gap — fix the Bridge, do not substitute a locator. |
| `-StopVerifyWeb` → **exit 5**, with `ERROR: ... NOT stopped -- port 8011 held by PID <n> (...)`, `ownership UNKNOWN -- ...` or `sweep ABORTED -- ...` | A port is still held, or dev-start refused to act. It stops the backend or frontend only when the recorded launcher is **alive**, matches its recorded `Win32_Process` creation time to the microsecond (a record from before this boot is proven only by that live match; otherwise it is STALE). It aborts the whole sweep if the tree contains a shell, terminal, editor, the desktop or Claude Code. Two records converge on their own and never need a hand: **a pid file with a creation time from before this boot** is deleted (unless its pid is still alive with exactly that creation time, which is the proof whatever the reported boot time says, and is stopped normally), and a **dead launcher with no live children over a free port** counts as stopped and its pid file is deleted. A **held port is always exit 5, with the pid file kept** unless the record was pre-boot (then it is already deleted and the holder is not the record's). What stays UNKNOWN (pid file kept) is a dead launcher with a live child, a live but unproven pid, or a protected process in the tree. The PID netstat names may itself be **dead**, because its socket is held by a live child that outlived it. The existing by-port stop still runs after a refusal. | Read the holder listing printed under the ERROR. It shows each live process whose `ParentProcessId` is the recorded or named pid, with its command line. A protected one reads `PROTECTED (...) -- do not stop`; every other one carries a per-pid `Stop-Process` line. Or query it yourself: `Get-CimInstance Win32_Process -Filter "ParentProcessId=<n>" \| Select-Object ProcessId,Name,CommandLine`. Stop a listed process by that pid only when its command line is this stack's backend (`python run.py`, `spawn_main`) or frontend (`next dev`). **Never kill by image name.** **Re-running `-StopVerifyWeb` converges** once those holders are gone. The one hand-delete case is a record (**verified, legacy or unverified**) whose pid is now held by a **live** process that is not this stack's launcher: it stays UNKNOWN on every run, so once the listing shows that pid alive as something that is not this stack's launcher, with nothing of this stack under it, delete that pid file. |
| `verify-page-verdict.sh` → **exit 4** | No `vision-audit` built from the SHA `style-gate.lock` pins. | Run the build command the error prints, or `--build-pinned`. Do **not** reach for a `qontinui-schemas` HEAD build — it answers a different question; `--allow-unpinned-analyzer` exists only for a deliberate, stamped exception. |

---

## Phase-1 contract (qontinui-web) — the one coupling

A Playwright storageState **cannot carry sessionStorage**, and the app's
`TokenStorage` restores the bearer FROM sessionStorage (`auth_bearer_access_token`).
So the sessionStorage bearer must be bootstrapped in-page. The seam is the
**Phase-1 client dev-auth helper** (gated by `NEXT_PUBLIC_ENABLE_DEV_LOCAL_AUTH=1`,
mirroring `NEXT_PUBLIC_ENABLE_SPEC_CI`): on boot it reads localStorage
`qontinui_dev_local_auth_token` and calls `TokenStorage.setTokens(token)`, which
sets the sessionStorage bearer + the marker cookie + the `user`. The tab-opener
seeds that localStorage key + the `qontinui_auth` cookie; the helper does the
rest. If Phase-1 lands with a different key name, update `DEV_TOKEN_LS_KEY` in
`scripts/verify-web-open-tab.sh` and the localStorage seed there.

## References
- Plan: `plans/2026-07-24-local-web-uibridge-verification-onramp.md`
- Tab-opener: `qontinui-claude-config/scripts/verify-web-open-tab.sh`
- Verdict arm: `qontinui-claude-config/scripts/verify-page-verdict.sh` (`--help`)
- Plan: `plans/2026-08-26-headless-ui-bridge-verification-an-agent-can-run.md`
- Stack bring-up/teardown: `dev-start.ps1 -VerifyWebStack` / `-StopVerifyWeb`
- KB: `knowledge-base/qontinui-specific/ui-bridge.md` → "Local web admin-page verification"
- Injected-transport internals: `.claude/commands/manual-test.md` (lines ~151-298)
