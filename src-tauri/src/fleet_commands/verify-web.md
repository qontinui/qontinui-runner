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

### Step 2 — Coord preflight (turn the silent 502 into a clear message)

`/operations/*` (which admin pages call) proxies to the local coord; if nothing
listens there the backend returns a bare `502 coord is not reachable`. Probe
first (the local coord is on the dedicated `:9871`, NOT canonical `:9870`):

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

### Step 5 — Teardown (ALWAYS)

```bash
kill -TERM "$INJECT_PID" 2>/dev/null || true   # release the Chromium tab
```
```powershell
.\dev-start.ps1 -StopVerifyWeb                 # backend + frontend + local coord + IdP
# Docker deps are left running; -StopDocker to stop them too.
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
- Stack bring-up/teardown: `dev-start.ps1 -VerifyWebStack` / `-StopVerifyWeb`
- KB: `knowledge-base/qontinui-specific/ui-bridge.md` → "Local web admin-page verification"
- Injected-transport internals: `.claude/commands/manual-test.md` (lines ~151-298)
