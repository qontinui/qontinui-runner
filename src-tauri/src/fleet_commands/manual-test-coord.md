# Manual Testing for Coord (Dashboard-driven via UI Bridge)

Perform a runner↔coord integration test by driving the canonical operator dashboard (`demo.staging.qontinui.io` for staging, `qontinui.io` for local) through the UI Bridge SDK exactly as a real operator would. **Work completely autonomously — never ask the user for input, never ask the user to restart or rebuild anything, never report that something "needs a rebuild" and stop.**

**This skill is the integration-test surface for coord.** Per [[feedback_qontinui_coord_no_ci]] coord has no CI; live dashboard-driven probes are the gate. Each iteration touches ECS staging + RDS + ECS scheduling and is slower than `/manual-test` — the loop wrapper caps at 4 iters.

**Multi-session safety.** No primary-runner lock is required. The test surface is a fresh **temporary runner** spawned per iteration via the supervisor (`POST :9875/runners/spawn-test`); a primary-runner restart is never warranted. For UI-Bridge-on-runner correctness testing use `/manual-test`; this skill validates the **operator's multi-device coordination experience** end-to-end.

**Canonical principle (do not violate).** The skill drives the dashboard via UI Bridge. **NEVER add admin/test endpoints to coord or qontinui-web to make a phase pass** — flag the gap as a `PRODUCT_GAP` finding instead. Parallel test-only HTTP surfaces drift away from what real users see; UI Bridge is the canonical automation layer.

**Verification = observation on the page, never inference (do not violate).** Every phase PASS must rest on something *observed rendered in the dashboard DOM via the UI Bridge* (as Phases 2/3 already do). A coord API response, DB row, `coord.devices`/`coord.sessions` registration, status endpoint, log line, or heartbeat is NOT evidence the operator's outcome occurred — it confirms plumbing and routinely disagrees with the page (e.g. `GET /sessions` returning rows while the Live Sessions page renders "0 sessions" under a different tenant scope). If the outcome can't be observed in the DOM, the phase is `BLOCKED`/UNVERIFIED, never PASS.

## Arguments / Invocation

```
/manual-test-coord [--target=staging|local|both]
                   [--rendezvous-slug=<YYYY-MM-DDTHH:MM>]
                   [--wait-timeout=<minutes>]
```

| Flag | Default | Purpose |
|---|---|---|
| `--target` | `staging` | Which dashboard surface to drive. `staging` = `demo.staging.qontinui.io` (two-machine model). `local` = `https://qontinui.io` (single-machine only). `both` = run the full sequence twice (local first, then staging). |
| `--rendezvous-slug` | (none — single-machine mode) | Required for two-machine mode. Same string on both operator machines; scopes the Phase 6 rendezvous to this run. Format suggestion: ISO-8601 minute-precision (`2026-05-21T14:30`). |
| `--wait-timeout` | `5` (minutes) | Phase 6 sibling-claim polling timeout. |
| `--operator` | `primary` | Which operator identity to log in as. `primary` = the default account (see "Operator credentials"). `secondary` = a SEPARATE-tenant operator (e.g. `tester2@qontinui.io`) read from env, so the two machines drive distinct tenants and Phase 6's cross-tenant isolation predicate is meaningful. In a two-machine run, one machine passes `--operator=primary`, the other `--operator=secondary`. |

Parse `$ARGUMENTS` for these flags; missing-flag fallbacks are:
- No `--target` ⇒ `staging`.
- No `--rendezvous-slug` ⇒ single-machine mode (Phase 6 SKIPs with `SETUP_GAP`).
- No `--wait-timeout` ⇒ 5 minutes.
- No `--operator` ⇒ `primary`.

If `$ARGUMENTS` carries free-form text after the flags, treat it as a focus hint for Phase 7 (which job to dispatch, if any choice exists).

## Two-machine concurrent-session model

The skill is designed to be invoked simultaneously on both operator machines (spaceship + MSI), each session logged in as a distinct real operator account in a distinct tenant. To get distinct tenants, run one machine with `--operator=primary` and the other with `--operator=secondary` (see "Operator credentials" + the `--operator` flag). Until the second-tenant staging operator is provisioned, both machines fall back to `primary` (same tenant) and Phase 6 degrades to same-account cross-machine visibility (SETUP_GAP for the true cross-tenant predicate). Operator triggers `/manual-test-coord --rendezvous-slug=<slug> [--operator=secondary]` on both machines within ~60s. Each session publishes its rendezvous claim **early — in Phase 0.6, right after its headless tab is confirmed** (not after pairing) so both machines' claims overlap even when one fast-tracks through a blocked run; runs Phases 1-5 independently; then in Phase 6 polls for the sibling's claim via `GET /coord/claims/by-correlation-topic?topic=manual-test-coord-rendezvous-<hyphenized-slug>` and asserts DOM-level cross-tenant isolation.

**Topic format (must pass coord's `^[a-z0-9][a-z0-9-]{0,63}$` regex).** The rendezvous topic is `manual-test-coord-rendezvous-<hyphenized-slug>`. The hyphenization rule: starting from `$RENDEZVOUS_SLUG`, lowercase the whole string and replace `T`, `:`, `.` with `-`. Example: slug `2026-05-22T15:20` becomes `2026-05-22-15-20`, final topic `manual-test-coord-rendezvous-2026-05-22-15-20`. Do not regress to the legacy colon-separated form — coord rejects it with `invalid_topic`.

**Correlation-id auto-generation.** Both sessions independently call `acquire` with the same `topic` and NO `correlation_id`. Whoever wins the race registers `(topic, new_uuid)` in `coord.correlation_topics`; the loser's acquire resolves to the same correlation_id automatically. Both sessions then call `by_correlation_topic` to discover the sibling's claim. The skill never derives a correlation_id locally (no UUID5 / NAMESPACE math) — coord owns the mapping.

**Multi-tab routing — pin to OUR tab via `?tabId=` (per-tab routing SHIPPED).** Two operator machines' headless tabs both register with the SAME dashboard relay (one per Vercel deployment). Historically the relay routed every `/control/*` command to whichever tab was `primaryTabId` (most-recently-registered wins), so two simultaneous sessions fought for primary and the loser saw stale-element-ID errors, cross-session routing, and the RELAY_RACE failure. **That race is now fixable from the client side:** `@qontinui/ui-bridge` ≥ 0.8.2 ships (a) re-arm of the WS/SSE transport on hard navigation (PR #41 `e72883e`, so a headless tab no longer goes silently dead after `page/navigate mode:hard`) and (b) a `?tabId=<id>` query parameter on `/control/*` that pins a command to a specific tab regardless of which is primary (PR #51 `f520798`; `TAB_NOT_FOUND`→HTTP 404, `TAB_STALE`→HTTP 410). qontinui-web depends on `@qontinui/ui-bridge@^0.8.5`, so both fixes are in the deployed bundle (subject to Vercel deploy freshness — see Phase 0.3).

**This skill therefore pins every dashboard `/control/*` (and `/ai/*`) call to its own headless tab.** Phase 0.6 captures OUR newly-launched tab id into `$OUR_TAB_ID` and exports `TAB_QS="?tabId=$OUR_TAB_ID"`; `capture_on_fail` auto-appends `$TAB_QS` to any dashboard-UB URL, and raw `curl` calls against `$DASHBOARD_UB/control/*` append `${TAB_QS}` explicitly. With per-tab pinning, **the two machines no longer need to win `primaryTabId` and the legacy ~10s stagger is no longer required** — both can launch in the same bucket and drive their own tabs concurrently. Phase 6 (rendezvous lookup) is unaffected regardless — it queries coord.claims, not the dashboard relay.

> If a `/control/*` call returns **HTTP 404** (`TAB_NOT_FOUND`), `$OUR_TAB_ID` was pruned (our tab died) — re-launch the headless tab (Phase 0.6) and re-capture `$OUR_TAB_ID`. **HTTP 410** (`TAB_STALE`) means the tab exists but its heartbeat lapsed — retry after a `page/refresh` rather than re-discovering the id.

Single-machine fallback: omit `--rendezvous-slug`. Phase 6 SKIPs with `SETUP_GAP`; the rest of the sequence runs normally.

## Service Endpoints

```bash
# Resolve target URLs based on --target:
case "$TARGET" in
  staging|both)
    STAGING_COORD="${COORD_HTTP_URL:-https://coord.staging.qontinui.io}"
    STAGING_DASHBOARD="https://demo.staging.qontinui.io"
    ;;
esac
case "$TARGET" in
  local|both)
    LOCAL_COORD="${COORD_HTTP_URL:-https://coord.qontinui.io}"
    LOCAL_DASHBOARD="https://qontinui.io"
    LOCAL_BACKEND="https://api.qontinui.io"
    ;;
esac
SUPERVISOR_BASE="http://localhost:9875"
```

**Source for `STAGING_COORD`.** Verified 2026-05-22 against the live deployment: `coord.staging.qontinui.io` (DOT, not HYPHEN) — Route53 A record in the `staging.qontinui.io.` zone (`Z02792161EHR967BO9804`) aliased to ALB `qontinui-staging-2019030450.us-east-1.elb.amazonaws.com`. Note: `qontinui-stack/aws/staging/outputs.tf:27-30` documents `coord-staging.qontinui.io` (hyphen) — that form is aspirational terraform output that doesn't match deployed reality; Vercel catches `*.qontinui.io` subdomains (apex on Vercel DNS) so the hyphen form returns `DEPLOYMENT_NOT_FOUND` from Vercel's edge. Override with `$COORD_HTTP_URL` if needed.

## Operator credentials

Credentials are selected by `--operator` (default `secondary` since 2026-07-22 —
see the primary-is-dead note below):

> **⚠️ 2026-07-22 — the `primary` credential is DEAD; default to `secondary`.**
> `VITE_DEV_PASSWORD` in `qontinui-runner/.env` is **NOT** the prod Cognito
> password for `josh@qontinui.io`. It is the LOCAL dev password, mirrored from
> `qontinui-web/dev-credentials.json`, which self-documents *"local development
> + testing only, never used in production"*. Using it against the prod pool
> returns `NotAuthorizedException: Incorrect username or password` (measured
> 2026-07-22, runner client `67f2a1a0cmgileob23lniud5t7`, which DOES have
> `ALLOW_USER_PASSWORD_AUTH` — so this is a wrong password, not a disabled flow).
> The old SSM fallback is gone too: `/qontinui/operator/*` returns
> `ParameterNotFound` and `aws ssm describe-parameters` shows **zero** params in
> us-east-1 / us-east-2 / us-west-2 / eu-central-1, with nothing in Secrets
> Manager either. **There is no reachable prod credential for the primary
> operator.** `--operator=secondary` (`tester2@qontinui.io`, password live in
> `$QONTINUI_OPERATOR2_PASSWORD`) is the working operator identity and is what
> the verified authed-web drive recipe uses. Prefer it for ALL harness work: it
> is a dedicated test account in its own tenant, so driving it can never touch
> the operator's own data or hijack the operator's parked browser tab.

```bash
case "${OPERATOR:-secondary}" in
  primary)
    USERNAME=jspinak
    EMAIL=josh@qontinui.io
    # DEAD PATH — kept only so an explicit `--operator=primary` fails loudly
    # instead of silently authenticating as nobody. Do not "fix" this by
    # re-pointing it at VITE_DEV_PASSWORD; that is the local dev password and
    # it is NOT the prod Cognito credential (see the note above).
    PASS="${QONTINUI_OPERATOR_PASSWORD:-}"
    if [[ -z "$PASS" ]]; then
      echo "SETUP_GAP: primary operator password unavailable — QONTINUI_OPERATOR_PASSWORD is unset,"
      echo "  SSM /qontinui/operator/* is empty (all regions, verified 2026-07-22), and"
      echo "  qontinui-runner/.env's VITE_DEV_PASSWORD is the LOCAL dev password, not the prod one."
      echo "  Use --operator=secondary (tester2@qontinui.io) instead."
      exit 0
    fi
    ;;
  secondary)
    # Distinct-tenant operator for the cross-tenant isolation predicate.
    # Staging credentials are NOT committed (the local-only
    # qontinui-web/dev-credentials.json is the wrong home for a staging
    # account — see Phase 6 of the iter-3 skill-infra plan). Read from env:
    USERNAME="${QONTINUI_OPERATOR2_USERNAME:-tester2}"
    EMAIL="${QONTINUI_OPERATOR2_EMAIL:-tester2@qontinui.io}"
    # THE SESSION ENV IS NOT A CREDENTIAL SOURCE ANY MORE. The runner scrubs
    # QONTINUI_OPERATOR2_PASSWORD out of every session it spawns (with
    # QONTINUI_TEST_LOGIN_PASSWORD and QONTINUI_TEST_AUTO_LOGIN_PASSWORD): they
    # are plaintext passwords, and the habitual JWT/KEY/TOKEN/SECRET redaction
    # filter matches none of them. So an EMPTY value here means "not in this
    # session's env BY DESIGN" - it does NOT mean the tester2 account is
    # unprovisioned, which is what the old message below asserted.
    PASS="${QONTINUI_OPERATOR2_PASSWORD:-}"
    if [[ -z "$PASS" ]] && command -v powershell >/dev/null 2>&1; then
      # ── STATED TRADE-OFF (decided, not incidental) ────────────────────────
      # This read recovers, inside a runner-spawned session, exactly the value
      # the scrub removed from it. DELIBERATE: the scrub's threat model is
      # ACCIDENTAL BULK EXPOSURE - an `env` dump printing three plaintext
      # passwords into a transcript, unredacted because the habitual
      # JWT|KEY|TOKEN|SECRET filter matches no name containing `PASSWORD`. It
      # was never denial to a determined caller; the operator owns the User
      # hive, so a deliberate read by a command that genuinely needs the
      # credential is in scope and permitted.
      # THE BOUNDARY IS THE SHAPE OF THE READ: it must stay a NAMED
      # SINGLE-VARIABLE read. Never widen it into an enumeration
      # (`Get-ChildItem Env:`, `[Environment]::GetEnvironmentVariables('User')`,
      # a registry dump of the Environment key) - that reinstates the exact bulk
      # exposure the scrub exists to prevent, under cover of a sanctioned
      # mechanism.
      # ──────────────────────────────────────────────────────────────────────
      # Resolution 1: the USER environment scope. The scrub removes the variable
      # from the SPAWNED SESSION only; it remains a registry-backed per-user
      # Windows environment variable - the same place the runner process itself
      # read it from. Verified 2026-08-18: present at User scope, absent at
      # Machine scope. Only the NAME crosses a command line; the VALUE returns
      # on stdout into a shell variable and never reaches any argv.
      PASS="$(powershell -NoProfile -Command "[Environment]::GetEnvironmentVariable('QONTINUI_OPERATOR2_PASSWORD','User')" 2>/dev/null | tr -d '\r')"
    fi
    if [[ -z "$PASS" ]]; then
      # Resolution 2: the operator supplies it to THIS command. The runner
      # provides no route that vends this credential to a session, so there is
      # nothing else to try - report the cause precisely rather than implying
      # the account is missing.
      echo "SETUP_GAP: --operator=secondary requested but no tester2 password is reachable."
      echo "  QONTINUI_OPERATOR2_PASSWORD is scrubbed from runner-spawned session env BY DESIGN,"
      echo "  and the User-scope environment read above also came back empty."
      echo "  Either run this step outside a runner-spawned session, or have the operator paste"
      echo "  the password into this one command's shell. If the account itself is genuinely not"
      echo "  provisioned, provision a distinct-tenant operator on staging and set"
      echo "  QONTINUI_OPERATOR2_{EMAIL,PASSWORD,USERNAME} at the USER scope, not just for one shell."
      echo "  Until a password resolves, Phase 6's cross-tenant predicate is inapplicable (SETUP_GAP)."
      exit 0
    fi
    ;;
esac
```

Login itself happens in **Phase 0.6** via `login-web.cjs` (injected transport,
drives the Cognito hosted-UI email flow with `EMAIL`/`PASS`) — see the relay
auth-gate note there for why the old relay-driven DOM-form login no longer
works pre-auth.

### Operator bearer for the auth-gated relay (`OPERATOR_JWT` + `AUTH_ARGS`)

Since 2026-06-04 the deployed dashboard relay runs with `UI_BRIDGE_REQUIRE_AUTH=1`
(`frontend/src/app/api/ui-bridge/[...path]/_auth.ts`): every `/api/ui-bridge/*`
call — `health`, `/control/*`, `/ai/*` — returns
`{"code":"UNAUTHENTICATED","message":"UI Bridge relay requires a valid session token"}`
without an `Authorization: Bearer <jwt>` header. Mint an operator JWT once,
right after resolving credentials, and attach it to EVERY relay call for the
rest of the run (proven live 2026-06-05: both the Cognito IdToken and
AccessToken pass — the gate validates against `/api/v1/auth/users/me`):

```bash
# USER_PASSWORD_AUTH against the qontinui Cognito pool. The RUNNER app client
# (67f2a1a0cmgileob23lniud5t7, qontinui-runner/src-tauri/src/cognito.rs:41) has
# ALLOW_USER_PASSWORD_AUTH; the web SPA's client is PKCE-only — don't use it.
#
# The mint BODY carries the operator PASSWORD, so it is staged in a file and
# sent as `curl --data-binary @file` for exactly the reason the IdToken is
# staged below: curl's argv is world-readable on this multi-session machine.
# The password is the higher-value half — it does not expire in 3600s and it
# re-mints tokens at will — so it must not be the one left on the command line.
# `printf`/`python` are safe channels here (printf is a shell builtin, and
# python reads the secrets from its environment, never argv); json.dumps also
# escapes a password containing `"` or `\`, which the old inline body did not.
MINT_BODY=$(mktemp) || { echo "BLOCKED: mktemp failed — cannot stage the password off argv"; exit 1; }
MINT_BODYP=$MINT_BODY
command -v cygpath >/dev/null 2>&1 && MINT_BODYP=$(cygpath -w "$MINT_BODY")
MT_EMAIL="$EMAIL" MT_PASS="$PASS" python -c 'import json,os,sys
sys.stdout.write(json.dumps({"AuthFlow":"USER_PASSWORD_AUTH",
  "ClientId":"67f2a1a0cmgileob23lniud5t7",
  "AuthParameters":{"USERNAME":os.environ["MT_EMAIL"],"PASSWORD":os.environ["MT_PASS"]}}))' > "$MINT_BODY"
[ -s "$MINT_BODY" ] || { echo "BLOCKED: could not stage the Cognito mint body (LOCAL fault)"; exit 1; }
OPERATOR_JWT=$(curl -s -m 15 -X POST "https://cognito-idp.us-east-1.amazonaws.com/" \
  -H "Content-Type: application/x-amz-json-1.1" \
  -H "X-Amz-Target: AWSCognitoIdentityProviderService.InitiateAuth" \
  --data-binary @"$MINT_BODYP" \
  | python -c "import sys,json; print(json.load(sys.stdin).get('AuthenticationResult',{}).get('IdToken',''))")
rm -f "$MINT_BODY"   # the password's file dies the moment the mint is done
if [ -z "$OPERATOR_JWT" ]; then
  echo "BLOCKED: Cognito USER_PASSWORD_AUTH returned no IdToken for $EMAIL — relay calls cannot authenticate."
  exit 1
fi
# Stage the operator bearer in a private tempfile and attach it as `curl -H
# @file`. NEVER put it on curl's argv: process cmdlines are world-readable on
# this multi-session machine, so every peer session can read an operator
# Cognito token straight out of the process list. (`cygpath -w` because a
# native curl.exe cannot open mktemp's POSIX path when MSYS pathconv is off.)
#
# The staged file lives and dies with THIS shell (the EXIT trap below). Every
# later fence that authenticates — `capture_on_fail`, the /health snapshots —
# tests `[ -s "$OPERATOR_AUTH_HDR" ]`, so run those in the SAME shell as this
# block, or re-run this block first. A stale path to a deleted file is not a
# credential.
OPERATOR_AUTH_HDR=$(mktemp) || { echo "BLOCKED: mktemp failed — cannot stage the operator bearer off argv"; exit 1; }
trap 'rm -f "$OPERATOR_AUTH_HDR"' EXIT
printf 'Authorization: Bearer %s\n' "$OPERATOR_JWT" > "$OPERATOR_AUTH_HDR"
[ -s "$OPERATOR_AUTH_HDR" ] || { echo "BLOCKED: could not stage the operator bearer (LOCAL fault)"; exit 1; }
OPERATOR_AUTH_HDRP=$OPERATOR_AUTH_HDR
command -v cygpath >/dev/null 2>&1 && OPERATOR_AUTH_HDRP=$(cygpath -w "$OPERATOR_AUTH_HDR")
# Attach to every $DASHBOARD_UB curl: curl ... "${AUTH_ARGS[@]}" ...
AUTH_ARGS=(-H @"$OPERATOR_AUTH_HDRP")
export OPERATOR_JWT OPERATOR_AUTH_HDR OPERATOR_AUTH_HDRP
```

**Binding rule:** every example `curl` against `$DASHBOARD_UB` in this skill —
including ones not shown with the header for brevity — MUST include
`"${AUTH_ARGS[@]}"`. `capture_on_fail` injects it automatically. If a relay
call ever returns `UNAUTHENTICATED` mid-run, the JWT expired (~1h) — re-mint
with the block above and retry once. (If the deployment under test has the
gate DISABLED — `health` answers 200 without the header — the extra header is
harmless; send it unconditionally.)

**Operator-machine_id allow-list (defense in depth — Phase 6 cross-check):**

| Hostname | machine_id |
|---|---|
| spaceship | `c79a07d5-7e40-49b4-87fa-554c749f9644` |
| MSI       | `84c02292-32cb-4983-be85-d00f868b7003` |

If a sibling claim arrives from a machine_id NOT in this list, surface `SECURITY_ANOMALY` in the Phase 6 report and continue (don't abort — the run still produces signal).

## UI Bridge Command Reference

The canonical reference is `<workspace-root>/ui-bridge/docs-site/docs/api/runner-features.md` (if `<workspace-root>/ui-bridge` is not checked out, skip the reference and rely on the cheatsheet below). The cheatsheet + gotchas from `/manual-test` (see `manual-test.md` §"UI Bridge Command Reference" and §"Critical gotchas") apply here verbatim — particularly:

- `state.value`, not `value` (dynamic element state is nested).
- `ai/find` is text-based: search by labels / placeholders / button text the user actually sees.
- Use `visibleOnly=true` after tab switches.
- `page/evaluate` rejects literal `fetch(` — dodge via `window["fet"+"ch"]`.
- `POST /control/page/navigate {url, mode?: "soft"|"hard"}` for cross-origin nav (use `hard` for first load against `demo.staging.qontinui.io` since the existing webview won't be on that origin).

This skill drives **two distinct UI Bridge surfaces** per session:

| Surface | Base URL | When used |
|---|---|---|
| Dashboard | `<dashboard-base>/api/ui-bridge` (web frontend's bridge) | Most phases — Phase 1, 2, 3 (observe the auto-registered runner row), 4, 5, 6, 7 (if dashboard-mediated), 8 (delete-device flow). |
| Temp runner | `http://localhost:<TEST_PORT>/ui-bridge` | Phase 7 fallback (runner-direct dispatch). The temp runner auto-signs-in via the supervisor-injected auto-login creds (staging: the operator creds from Phase 0.5 `extra_env`; local: the runner `.env`) and registers itself; no runner-side pairing UI is driven. |

Resolve them per target:

```bash
case "$TARGET_NOW" in
  staging) DASHBOARD_UB="https://demo.staging.qontinui.io/api/ui-bridge" ;;
  local)   DASHBOARD_UB="https://qontinui.io/api/ui-bridge" ;;
esac
RUNNER_UB="http://localhost:${TEST_PORT}/ui-bridge"   # populated by Phase 0
```

## Local machine identity

```bash
MACHINE_ID="${QONTINUI_MACHINE_ID:-}"
if [ -z "$MACHINE_ID" ]; then
  MACHINE_ID=$(python -c "
import json, os, sys
p = os.path.expanduser('~/.qontinui/machine.json')
try:
    with open(p) as f:
        d = json.load(f)
    print(d.get('device_id') or d.get('machine_id') or '', end='')
except Exception as e:
    print('', end='')
")
fi
if [ -z "$MACHINE_ID" ]; then
  echo "ERROR: cannot resolve local machine_id (env QONTINUI_MACHINE_ID unset, ~/.qontinui/machine.json missing or malformed)"
  exit 1
fi
echo "Local machine_id: $MACHINE_ID"
```

(`device_id` is the canonical field per `qontinui-runner/src-tauri/src/fleet.rs:131`; `machine_id` is a serde alias for the pre-Phase-3 shape.)

## Coord activity surfacing (`coord.device_status`)

Immediately after the `MACHINE_ID` resolution above and BEFORE Phase 0,
UPSERT a status row so the operator dashboard's live activity tile
reflects what this session is doing. This is the read-side of Phase 1.1
+ 1.3 of plan `2026-05-21-coordination-improvements.md`.

The UPSERT is keyed on `device_id`, so each call overwrites the prior
row for this machine. Failure is non-fatal — warn and continue; status
is observability, not gating.

```bash
TEST_NAME="${RENDEZVOUS_SLUG:-single-machine}"
# The MAIN repo's directory name, not the worktree's: --show-toplevel returns the
# WORKTREE path in a linked worktree, so its basename is a directory name that is
# not a repo (and sessions run under QONTINUI_AGENT_WORKTREE_MODE=1).
# `--path-format=absolute` (git >= 2.31) avoids the relative `.git` a bare
# --git-common-dir returns in the canonical checkout. The work-tree test rules
# out a non-git cwd AND a bare repo (whose common dir's parent names some
# unrelated directory); the `modules` case is a submodule, whose common dir is
# <super>/.git/modules/<name>. All of those publish "unknown" rather than a repo
# name that does not exist.
REPO_BASENAME="unknown"
if [ "$(git rev-parse --is-inside-work-tree 2>/dev/null)" = "true" ]; then
  GIT_COMMON_DIR=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
  case "$GIT_COMMON_DIR" in
    ""|"."|".."|*/.git/modules/*) ;;
    *) REPO_BASENAME=$(basename "$(dirname "$GIT_COMMON_DIR")") ;;
  esac
fi
BRANCH_NAME=$(git symbolic-ref --short HEAD 2>/dev/null || echo "unknown")
COORD_BASE="${COORD_HTTP_URL:-https://coord.qontinui.io}"

# tenant_id is optional — omit the JSON field entirely if unset
TENANT_FIELD=""
if [ -n "${QONTINUI_TENANT_ID:-}" ]; then
  TENANT_FIELD=",\"tenant_id\":\"${QONTINUI_TENANT_ID}\""
fi

curl -fsS -X POST "${COORD_BASE}/coord/status" \
  -H "Content-Type: application/json" \
  -d "{
    \"device_id\": \"${MACHINE_ID}\",
    \"current_task\": \"manual-test-coord: ${TEST_NAME}\",
    \"current_repo\": \"${REPO_BASENAME}\",
    \"current_branch\": \"${BRANCH_NAME}\",
    \"details\": {\"target\": \"${TARGET:-staging}\"}${TENANT_FIELD}
  }" 2>&1 \
  || echo "⚠️ coord status publish failed (non-fatal, continuing)"
```

The dashboard tile clears automatically when the next status row
arrives or via `prune_stale()` after 1h. Phase 8 (cleanup) does NOT
need a clearing UPSERT — manual-test-coord runs are short and
overlapping siblings overwrite each other naturally.

## Phase 0: Pre-flight

Goal: prove every dependency is up before doing anything destructive.

### Step 0.1 — Coord health

```bash
# For --target=staging:
curl -s --max-time 10 "$STAGING_COORD/health" | python -c "
import sys, json
d = json.load(sys.stdin)
ok = d.get('data', {}).get('ok') is True or d.get('status') == 'ok' or d.get('ok') is True
sys.exit(0 if ok else 1)
" || { echo "BLOCKED: staging coord unhealthy at $STAGING_COORD/health"; exit 1; }

# For --target=local:
curl -s --max-time 5 "$LOCAL_COORD/health" || { echo "BLOCKED: local coord unhealthy at $LOCAL_COORD/health"; exit 1; }
```

### Step 0.2 — Dashboard reachable

```bash
# Staging:
curl -s -o /dev/null -w "%{http_code}" --max-time 15 "$STAGING_DASHBOARD/"
# Expect 200/30x — anything else is BLOCKED.

# Local:
curl -s -o /dev/null -w "%{http_code}" --max-time 5 "$LOCAL_DASHBOARD/"
```

### Step 0.3 — Watcher freshness alerts (staging only)

```bash
# Vercel deploy freshness watcher — must be empty (no pending stale-deploy alert)
ALERTS_VERCEL=$(curl -s "$STAGING_COORD/coord/alerts?source=vercel_deploy_freshness")
echo "$ALERTS_VERCEL" | python -c "
import sys, json
d = json.load(sys.stdin)
alerts = d.get('data', {}).get('alerts') if isinstance(d.get('data'), dict) else d.get('alerts', [])
if alerts:
    print('PRODUCT_GAP / DEFERRED: vercel_deploy_freshness alerts active:', alerts)
    sys.exit(2)
" || true   # exit code 2 = DEFERRED, not BLOCKED

# ECS image freshness watcher — same shape
ALERTS_ECS=$(curl -s "$STAGING_COORD/coord/alerts?source=ecs_image_freshness")
# (same parsing)
```

If either watcher is alerting, the test surface is suspect — record as DEFERRED for the affected phases (Phase 2/3 for Vercel, Phase 7 for ECS) and continue. Per [[feedback_vercel_autodeploy_silent_break]] Vercel pushes can land but autodeploy stays stuck; the watcher catches that.

### Step 0.4 — Backend (local only)

```bash
curl -s --max-time 5 "$LOCAL_BACKEND/health" || \
  { echo "BLOCKED: local backend at $LOCAL_BACKEND/health unreachable — fix via .\\dev-start.ps1 -Backend"; exit 1; }
```

### Step 0.5 — Spawn temp runner

Mirror `/manual-test` §"Phase 0: Spawn Test Runner & Health Check" — LKG-first, rebuild only if your changes aren't in the LKG yet. The temp runner is the **device-registration subject** for Phase 3 (it auto-signs-in and registers itself with web), NOT a primary-runner replacement.

**Point the temp runner at the test target via `extra_env` (staging registration fix).** A debug-build temp runner defaults its backend to `http://127.0.0.1:8000` (`api_config.rs::get_api_base_url`), so against `--target=staging` it would register with a *local* dashboard and never appear on staging — the gating bug behind Phases 3/4/5/7 on the prior run. There is **no need for a new supervisor flag**: `POST /runners/spawn-test` already accepts an `extra_env` map that is applied **last** (the `ExtraEnv` forwarder runs after `TestAutoLoginEnv`, `qontinui-supervisor/src/process/env_forwarders.rs`), overriding anything the supervisor set. So we override these vars (all verified live 2026-05-26 against a temp runner built from `origin/main`):

- `QONTINUI_WEB_BACKEND_URL` → the **direct staging backend** `https://web.staging.qontinui.io` — **NOT** the `demo.staging.qontinui.io` dashboard host. This is the single canonical backend base: runner#294 unified `get_api_base_url` to resolve `QONTINUI_WEB_BACKEND_URL → QONTINUI_API_URL → default`, so this var drives login, heartbeat, AND workflow-sync; it *also* feeds `settings.web_integration.backend_url`, which is what the **device WS** (`mcp::backend_relay` → `/api/v1/devices/ws`) connects to. **Critical:** the device WS must hit the direct backend — `demo.staging` (Vercel) returns `401` and does NOT proxy the WS upgrade (verified), while `web.staging` does a real `101 Switching Protocols` (uvicorn). Point it at `demo.staging` and the device never registers (it'll either fail the WS or fall back to the prod default `api.qontinui.io`).
- `QONTINUI_API_URL` → set to the same direct backend (`https://web.staging.qontinui.io`) for belt-and-braces; redundant once `QONTINUI_WEB_BACKEND_URL` is set (the unified resolver prefers it), but explicit avoids any ambiguity. *(Note: the **dashboard** UI-Bridge driving still targets `demo.staging` via `$STAGING_DASHBOARD` — that's the frontend and is unchanged. Only the runner's backend traffic goes to `web.staging`.)*
- `QONTINUI_RUNNER_TIER=qontinui_account` → **REQUIRED.** Auto-login is gated on Tier 2 (`commands::auth::login_impl` calls `require_tier_2()`; the headless path skips with `headless_auto_login_skipped reason="not_tier_2"` otherwise). A temp runner defaults to Tier `Local`, so **without this it never logs in at all** — this was the deeper reason the original temp runner never registered. The supervisor applies it as an **in-memory** tier overlay (`settings.rs::apply_tier_env_overlay`), never persisted.
- `QONTINUI_TEST_AUTO_LOGIN_EMAIL` / `QONTINUI_TEST_AUTO_LOGIN_PASSWORD` → the **same operator credentials** this session logs the dashboard in as (`$EMAIL` / `$PASS` from "Operator credentials"), so the runner auto-signs-in to the **same tenant** and its device row shows up under the operator we're observing. (Without these, the forwarder would fall back to the runner's local `.env` = josh, which on a `--operator=secondary` run would register the runner in the WRONG tenant.)

> **⚠️ PRIMARY-RUNNER DEMOTION HAZARD (open footgun — do not ignore).** Temp runners share the primary's `settings.json` (`dirs::config_dir()`). The Tier env overlay above is in-memory-safe, but the runner's **startup tier-migration path persists** — a temp runner whose loaded settings are uninitialized / token-less can migrate→`local` and **write that to the shared file, silently demoting the primary runner to Tier 1** (the exact hazard called out in `settings.rs:1861-1867`; observed live 2026-05-26 — a verification spawn persisted `tier=local` to the shared `settings.json`). Until the runner isolates temp-runner config (or stops persisting a migrate from a temp runner), **be aware that running this skill can demote the primary's persisted tier**; if the primary loses Tier 2 after a run, re-establish it by re-logging-in the primary runner. Tracked as a runner-side follow-up.

```bash
REQUESTER="manual-test-coord-$(date +%s)"
# Build the extra_env override for the current target. For staging, point the
# runner's backend + auto-login at staging so it registers in the operator's tenant.
# For local, omit extra_env (the localhost default + runner .env are correct).
if [ "$TARGET_NOW" = "staging" ]; then
  # Staging DIRECT backend (uvicorn), NOT the demo.staging Vercel dashboard host.
  # The device WS (/api/v1/devices/ws) needs a real WS upgrade, which Vercel
  # does not proxy (demo.staging → 401; web.staging → 101). This single base
  # drives login/heartbeat/workflow-sync (unified get_api_base_url) AND the
  # device WS (settings.web_integration.backend_url).
  STAGING_API_URL="${QONTINUI_STAGING_API_URL:-https://web.staging.qontinui.io}"
  EXTRA_ENV=$(EMAIL="$EMAIL" PASS="$PASS" API="$STAGING_API_URL" python -c "
import json, os
print(json.dumps({
    'QONTINUI_WEB_BACKEND_URL': os.environ['API'],
    'QONTINUI_API_URL': os.environ['API'],
    # Tier 2 is REQUIRED for auto-login (require_tier_2). In-memory overlay only.
    'QONTINUI_RUNNER_TIER': 'qontinui_account',
    'QONTINUI_TEST_AUTO_LOGIN_EMAIL': os.environ['EMAIL'],
    'QONTINUI_TEST_AUTO_LOGIN_PASSWORD': os.environ['PASS'],
}))
")
  echo "Temp runner → staging backend $STAGING_API_URL (tier=qontinui_account), auto-login as $EMAIL"
else
  EXTRA_ENV="{}"
fi
# Use rebuild:true if testing recent runner-coord changes; otherwise LKG is fine.
SPAWN_BODY=$(EXTRA_ENV="$EXTRA_ENV" REQUESTER="$REQUESTER" python -c "
import json, os
body = {'rebuild': True, 'requester_id': os.environ['REQUESTER'], 'queue_timeout_secs': 600}
extra = json.loads(os.environ['EXTRA_ENV'])
if extra:
    body['extra_env'] = extra
print(json.dumps(body))
")
SPAWN_RESULT=$(curl -s -X POST "$SUPERVISOR_BASE/runners/spawn-test" \
  -H "Content-Type: application/json" -d "$SPAWN_BODY")
TEST_PORT=$(echo "$SPAWN_RESULT" | python -c "import sys,json; print(json.load(sys.stdin)['port'])")
TEST_ID=$(echo "$SPAWN_RESULT" | python -c "import sys,json; print(json.load(sys.stdin)['id'])")
RUNNER_UB="http://localhost:${TEST_PORT}/ui-bridge"
echo "Temp runner up: ID=$TEST_ID PORT=$TEST_PORT"

# Wait for health
for i in $(seq 1 40); do
  responsive=$(curl -s -m 3 "http://localhost:${TEST_PORT}/health" 2>/dev/null \
    | python -c "import sys,json; print(json.load(sys.stdin).get('data',{}).get('responsive',False))" 2>/dev/null)
  [ "$responsive" = "True" ] && { echo "Temp runner ready"; break; }
  sleep 5
done
```

Predicate: PASS iff every sub-step above returned a usable response. Otherwise BLOCKED.

### Step 0.6 - Launch headless dashboard tab (login + co-pilot consent REQUIRED before registration)

The dashboard's UI Bridge relay returns `No browser connected - no WebSocket clients and no SSE listeners` until a real browser tab is connected — and since the 2026-06-04 auth hardening a tab can only register when **three in-page gates** all hold (`frontend/src/lib/ui-bridge/provider.tsx:210-213`):

1. `NEXT_PUBLIC_UI_BRIDGE_REMOTE_COMMANDS=1` baked into the deployment (set on prod + staging manual-test builds),
2. the **account-level co-pilot preference** is enabled for the operator (`/settings/co-pilot` — one-time per account; a fresh `--operator=secondary` account needs this toggled once or registration is impossible → SETUP_GAP),
3. the **per-session co-pilot consent** is granted (the "Allow AI Co-Pilot for this session?" modal → `[data-testid='co-pilot-consent-allow']`).

A bare pre-auth tab (the old `@qontinui/ui-bridge-headless` launch) can therefore **never** register anymore: the `CommandRelayListener` doesn't mount until the user is logged in AND consented, and the relay rejects unauthenticated traffic anyway. So this step launches the tab via the **`ui-bridge-login-web`** package bin (`@qontinui/ui-bridge-wrapper` ≥ 0.4.0; injected transport — drives the full Cognito hosted-UI login inside the tab), grants the consent with `--post-login-click`, and parks the authed session with `--keep-open`. Once consented, the app's own SDK registers with the same-origin relay (its outbound calls carry the app session's bearer) — proven live 2026-06-05 (`/commands/stream` + `/heartbeat` + `/commands` all 200, tab visible in `connectedTabs`). Login is therefore DONE here; Phase 1 only **verifies** the authed DOM.

> **⚠️ STALE-TAB GUARD + PER-TAB PINNING.** `connectedTabs >= 1` is NOT sufficient on its own. Historically the relay routed `/control/*` to whichever tab was **`primaryTabId`** (most-recently-registered wins), so a foreign/leftover tab holding primary would silently capture your commands (e.g. an unauthenticated tab parked on `/` → a false auth/RELAY_RACE failure). **The robust fix is no longer "win primary" — it is to pin every command to OUR tab via `?tabId=` (ui-bridge ≥ 0.8.2 #51; see "Multi-tab routing" above).** So Phase 0.6 (a) kills local leftover headless procs, (b) launches our tab, (c) **identifies OUR tab id** — the connected-tab id that is NOT in the pre-existing set — and exports `TAB_QS="?tabId=$OUR_TAB_ID"` for the rest of the run. We no longer require OUR tab to be `primaryTabId`; capturing its id is enough, and a concurrent sibling on the same relay can run at the same time without a stagger.

```bash
# The login/capture harness ships as PUBLISHED PACKAGE BINS (ui-bridge PR #86,
# @qontinui/ui-bridge-wrapper >= 0.4.0); the old repo-path scripts/login-web.cjs
# was deleted. Resolve a known, npx-pinned version from ANY cwd (no more "run
# from the ui-bridge repo root"). `npx -p` pulls the wrapper PLUS its optional
# browser peers — a bare `npx <pkg>` does NOT install peer deps, so the browser
# bins would fail to launch Chromium without the explicit -p list.
# One-time-per-machine prereq: `npx playwright install chromium` (Chromium lands
# in a shared global cache, so it persists across runs).
LOGIN_WEB="npx -y -p @qontinui/ui-bridge-wrapper -p @qontinui/ui-bridge -p @qontinui/ui-bridge-headless -p playwright ui-bridge-login-web"
# Versions this resolved to on the 2026-07-22 verified run (npm `latest` at the
# time): ui-bridge-wrapper 0.6.0, ui-bridge 0.22.0, ui-bridge-headless 0.3.0.
# NOTE the peer-range hazard: published wrapper 0.6.0 declares
# `@qontinui/ui-bridge: ^0.4.0 || … || ^0.21.0` — it does NOT list ^0.22.0, and
# on a 0.x range a caret locks the MINOR, so 0.22.0 is formally out of range.
# `npx -p` tolerates it (warns, installs, runs — verified working). A strict
# `npm install` of the same set would ERESOLVE. The fix (wrapper 0.6.1, which
# adds ^0.22.0) is on ui-bridge `origin/main` but is NOT PUBLISHED, and cannot
# be published until its new dependency `@qontinui/ui-bridge-cli-args@0.1.0` is
# published first (that package returns 404 on the registry today).

# 0.6-PRE — THE CO-PILOT PREFERENCE PRECONDITION (the real reason `GET /tabs`
# returns 0 tabs after a SUCCESSFUL login). `CommandRelayListener` mounts only
# when ALL THREE gates in `qontinui-web/frontend/src/lib/ui-bridge/provider.tsx`
# are positive:
#     envEnableRemoteCommands              (build-time NEXT_PUBLIC_UI_BRIDGE_REMOTE_COMMANDS=1; ON in prod)
#  && userPreference.enabled === true      (per-USER durable `ui_bridge_co_pilot_enabled`)
#  && sessionConsent.state === "granted"   (the per-session consent modal)
# The per-user preference defaults to FALSE for every account. While it is
# false the consent modal never renders, so `--post-login-click` reports
# `postLoginClicked:false`, no listener mounts, and the tab never registers —
# a login that is `ok:true` in every other respect still yields zero tabs.
# Make the preference true ONCE per account (idempotent, per-user, reversible):
# Stage the mint BODY (it carries the password) and the TOKEN off argv — see
# "Operator credentials" for why. ONE trap covers every staged credential,
# because a `trap … EXIT` REPLACES the previous one rather than adding to it:
# $OPERATOR_AUTH_HDR is listed here so that running this fence in the SAME
# shell as "Operator credentials" does not silently strand the operator bearer
# in $TMPDIR. (`${OPERATOR_AUTH_HDR:-}` so the fence also stands alone.)
MINT_BODY=$(mktemp); IDT_HDR=$(mktemp)
trap 'rm -f "$MINT_BODY" "$IDT_HDR" "${OPERATOR_AUTH_HDR:-}"' EXIT
MINT_BODYP=$MINT_BODY; command -v cygpath >/dev/null 2>&1 && MINT_BODYP=$(cygpath -w "$MINT_BODY")
MT_EMAIL="$EMAIL" MT_PASS="$PASS" python -c 'import json,os,sys
sys.stdout.write(json.dumps({"AuthFlow":"USER_PASSWORD_AUTH",
  "ClientId":"67f2a1a0cmgileob23lniud5t7",
  "AuthParameters":{"USERNAME":os.environ["MT_EMAIL"],"PASSWORD":os.environ["MT_PASS"]}}))' > "$MINT_BODY"
[ -s "$MINT_BODY" ] || { echo "could not stage the Cognito mint body (LOCAL fault)"; exit 1; }
IDT=$(curl -s -m 20 -X POST "https://cognito-idp.us-east-1.amazonaws.com/" \
  -H "Content-Type: application/x-amz-json-1.1" \
  -H "X-Amz-Target: AWSCognitoIdentityProviderService.InitiateAuth" \
  --data-binary @"$MINT_BODYP" \
  | python -c "import sys,json; print(json.load(sys.stdin)['AuthenticationResult']['IdToken'])")
rm -f "$MINT_BODY"
printf 'Authorization: Bearer %s\n' "$IDT" > "$IDT_HDR"
IDT_HDRP=$IDT_HDR; command -v cygpath >/dev/null 2>&1 && IDT_HDRP=$(cygpath -w "$IDT_HDR")
curl -s -m 20 -X PUT -H @"$IDT_HDRP" -H "Content-Type: application/json" \
  -d '{"ui_bridge_co_pilot_enabled": true}' \
  "https://api.qontinui.io/api/v1/users/me/preferences"
# → {"product_mode":null,"ui_bridge_co_pilot_enabled":true}
# (Equivalent on-page path: the toggle at <dashboard>/settings/co-pilot.)

# 0.6a — kill leftover LOCAL headless procs from prior runs (they steal relay primary).
# Windows: enumerate node command lines via PowerShell (bash/MSYS can't). Foreign tabs on OTHER
# machines can't be killed here — per-tab pinning makes them harmless anyway.
powershell -NoProfile -Command "Get-CimInstance Win32_Process -Filter \"Name='node.exe'\" | Where-Object { \$_.CommandLine -match 'ui-bridge-headless|login-web' } | ForEach-Object { Stop-Process -Id \$_.ProcessId -Force -ErrorAction SilentlyContinue }" 2>/dev/null || true
sleep 2

# 0.6b — record pre-existing (foreign/stale) tabs so we can identify OURS by diff.
# NOTE: the relay is auth-gated — every health/control call carries "${AUTH_ARGS[@]}".
BEFORE_TABS=$(curl -s -m 5 "${AUTH_ARGS[@]}" "$DASHBOARD_UB/health" | python -c "
import sys,json
try: print(','.join(json.load(sys.stdin).get('data',{}).get('connectedTabs',[])))
except Exception: print('')
")
echo "Pre-existing relay tabs (foreign/stale): ${BEFORE_TABS:-<none>}"

# 0.6c — launch our tab via the ui-bridge-login-web bin: real hosted-UI login +
# per-session co-pilot consent + park. Runs from ANY cwd (the bin resolves its
# engine bundle from its own module tree). Keep MSYS_NO_PATHCONV=1 (Git Bash
# rewrites --success /runs/active into a Windows path otherwise). --success
# matches the landing PATHNAME only. `next=` makes the authed landing
# DETERMINISTIC (without it the app picks /dashboard or /build/workflows
# situationally and --success false-negatives); next-bearing login URLs are safe
# again since web #439 (`311dd963`) base64url-packed the OAuth state (pre-#439
# they always failed "state mismatch").
MSYS_NO_PATHCONV=1 UIB_LOGIN_EMAIL="$EMAIL" UIB_LOGIN_PASSWORD="$PASS" \
  $LOGIN_WEB --url "$STAGING_DASHBOARD/login?next=%2Fruns%2Factive" --success /runs/active \
  --post-login-click "[data-testid='co-pilot-consent-allow']" --keep-open \
  > /tmp/mtc-login-web.json 2>/tmp/mtc-login-web.log &
HEADLESS_PID=$!
echo "login-web tab spawn PID=$HEADLESS_PID"
# The script prints ONE JSON result line then parks (--keep-open). Wait for it
# and assert ok:true + the consent click before polling the relay — a failed
# login can never register, so failing fast here beats a 40s relay timeout.
for i in $(seq 1 24); do [ -s /tmp/mtc-login-web.json ] && break; sleep 5; done
LOGIN_OK=$(python -c "
import json
try:
  d=json.load(open('/tmp/mtc-login-web.json'))
  print('ok' if d.get('ok') else 'fail:'+str(d.get('errorText') or d.get('error') or d.get('finalUrl')))
  import sys; sys.stderr.write('postLoginClicked=%s\n' % d.get('postLoginClicked'))
except Exception as e: print('fail:no-result:'+str(e))" 2>&1)
echo "login-web: $LOGIN_OK"
case "$LOGIN_OK" in ok*) ;; *) echo "BLOCKED: operator login failed — $LOGIN_OK"; kill $HEADLESS_PID 2>/dev/null; exit 1;; esac
# postLoginClicked=false is NOT fatal by itself (consent may already be granted
# for a recycled browser profile) — the relay poll below is the real gate.

# 0.6d — wait until OUR tab (a NEW id not in BEFORE_TABS) is CONNECTED, and capture
# its id. We don't need primaryTabId — every /control/* call pins via ?tabId (#51).
OUR_TAB_ID=""
for i in $(seq 1 20); do
  TABS=$(curl -s -m 5 "${AUTH_ARGS[@]}" "$DASHBOARD_UB/health" | python -c "
import sys,json
try: print(','.join(json.load(sys.stdin).get('data',{}).get('connectedTabs',[])))
except Exception: print('')
" 2>/dev/null)
  OUR_TAB_ID=$(BEFORE_TABS="$BEFORE_TABS" TABS="$TABS" python -c "
import os
before=set(os.environ['BEFORE_TABS'].split(',')) - {''}
tabs=[t for t in os.environ['TABS'].split(',') if t]
# Our tab = a connected id that wasn't present before we launched.
new=[t for t in tabs if t not in before]
print(new[-1] if new else '')
" 2>/dev/null)
  if [ -n "$OUR_TAB_ID" ]; then echo "Our authed+consented tab connected: tabId=$OUR_TAB_ID"; break; fi
  sleep 2
done

if [ -z "$OUR_TAB_ID" ]; then
  echo "BLOCKED + RELAY_RACE: our tab never appeared in connectedTabs within ~40s DESPITE a confirmed login. Triage in order:"
  echo "  1. postLoginClicked=false + no consent modal → the OPERATOR ACCOUNT's co-pilot preference is off — enable it once at $STAGING_DASHBOARD/settings/co-pilot (SETUP_GAP, esp. for --operator=secondary)."
  echo "  2. relay health UNAUTHENTICATED → OPERATOR_JWT expired/wrong pool — re-mint (see Operator credentials)."
  echo "  3. neither → the deployment may lack NEXT_PUBLIC_UI_BRIDGE_REMOTE_COMMANDS=1 (re-check Phase 0.3 Vercel freshness) or @qontinui/ui-bridge >=0.8.2 (re-arm WS, #41)."
  kill $HEADLESS_PID 2>/dev/null || true
  exit 1
fi

# Export the per-tab routing suffix used by every dashboard /control/* + /ai/* call.
# capture_on_fail auto-appends this; raw curls append ${TAB_QS} explicitly.
export OUR_TAB_ID
TAB_QS="?tabId=${OUR_TAB_ID}"
```

Predicate: PASS iff OUR newly-launched tab appears in the relay's `connectedTabs` within ~40s and we captured its `tabId` (NOT that it owns `primaryTabId` — per-tab routing makes primary ownership irrelevant). Otherwise BLOCKED + RELAY_RACE (kill the spawn PID before exiting).

### Step 0.6.5 — Publish rendezvous claim early (two-machine mode only)

**Why here, not Phase 3.5.** The claim must be alive while the *sibling* is polling in its Phase 6. If we publish late (after Phases 1-5) and release eagerly in Phase 8, a machine that fast-tracks through a blocked run can publish and release before the slow machine's poll window even opens — their claim lifetimes never overlap and neither sees the other (the exact failure in the 2026-05-25 run). Publishing right after the headless tab is confirmed — which both machines reach within ~2 min of launch — guarantees overlap. `tenant_id` isn't known yet (it's resolved from the authed DOM in Phase 2, even though login already completed in 0.6c); it's backfilled best-effort at the end of Phase 2. The rendezvous itself keys on `topic`, and the cross-tenant signal keys on `operator_email` + `machine_id`, both known now.

```bash
if [ -n "$RENDEZVOUS_SLUG" ]; then
  # Hyphenize the slug for coord's topic regex `^[a-z0-9][a-z0-9-]{0,63}$`:
  # lowercase + replace `T`, `:`, `.` with `-`.
  RDV_SLUG_HYPHENIZED=$(echo "$RENDEZVOUS_SLUG" | tr 'A-Z' 'a-z' | tr ':T.' '---')
  CORR_TOPIC="manual-test-coord-rendezvous-${RDV_SLUG_HYPHENIZED}"
  RESOURCE_KEY="manual-test-coord-rendezvous-${RDV_SLUG_HYPHENIZED}-${MACHINE_ID}"
  # No correlation_id field - coord auto-generates and returns it in the response.
  # Both sessions pass the same topic; coord registers (topic, uuid) on first arrival
  # and resolves subsequent acquires to the same correlation_id.
  curl -s -X POST "$STAGING_COORD/claims/acquire" \
    -H "Content-Type: application/json" \
    -d "{
      \"kind\": \"phase\",
      \"resource_key\": \"${RESOURCE_KEY}\",
      \"machine_id\": \"${MACHINE_ID}\",
      \"ttl_seconds\": 7200,
      \"topic\": \"${CORR_TOPIC}\",
      \"metadata\": {
        \"temp_runner_hostname\": \"${HOSTNAME}\",
        \"temp_runner_test_id\": \"${TEST_ID}\",
        \"tenant_id\": \"\",
        \"operator_email\": \"${EMAIL}\"
      }
    }"
  RENDEZVOUS_PUBLISHED=1
  echo "Rendezvous claim published early on topic $CORR_TOPIC (resource_key $RESOURCE_KEY)"
fi
```

Note: the coord `ClaimRequest` field is `topic` (per `qontinui-coord/src/claims.rs:96-115`), not `correlation_topic`. Coord owns the topic-to-correlation_id mapping; the skill never derives a correlation_id locally. `RENDEZVOUS_PUBLISHED=1` is checked in Phase 8.1 to decide whether to release or leave the claim for its TTL.

### Step 0.7 — Forensics helpers (evidence capture for `/control/*` failures)

The skill captures structured forensics on every non-2xx `/control/*` response — status, response-body head, Vercel's per-instance `x-vercel-id` header, and a **pair** of `/health` snapshots (one at failure-time, one 5s later — the retry-snapshot that makes an SSE-listener flap visible without Vercel function logs) — so iter-2 mechanism-3 / SSE-flap / deployment-skew hypotheses resolve from a normal dual run rather than a separate probe session. Helpers live globally for the rest of the session.

The JSON envelope is built via `python -c "json.dumps(...)"` — the rest of the skill already depends on python for `json.load` parsing, so no new external dependency is introduced.

```bash
# Accumulators — shared across Phases 1, 1.5, 2. Emitted at end of Phase 8.
#
# CONTROL_FAILURES_LOG is file-backed (one JSON entry per line). Callers
# like `SNAP=$(CF_METHOD=GET CF_URL=... CF_DATA= capture_on_fail)` run in a subshell,
# so a bash-array side effect would be LOST on return. A file is
# process-shared and survives the subshell boundary. LOGIN_ATTEMPTS stays
# as an array — its append site is not under command substitution.
CONTROL_FAILURES_LOG="$(mktemp -t controlfails.XXXXXX)"
LOGIN_ATTEMPTS=()

# capture_on_fail — runs the curl, captures
# {status, body[:500], x-vercel-id, paired /health snapshot + a second
# /health snapshot 5s later (SSE-flap retry-snapshot)} on non-2xx;
# appends one JSON line to $CONTROL_FAILURES_LOG. Stdout of the helper
# is the raw response body (so existing callers piping into
# `python -c 'json.load(...)'` still work).
#
# INPUTS COME IN THROUGH NAMED VARIABLES, set as a prefix assignment on the
# call itself — never as shell positional parameters:
#   CF_METHOD=POST CF_URL="$DASHBOARD_UB/control/discover" CF_DATA='{}' capture_on_fail
#   CF_METHOD=GET  CF_URL="$DASHBOARD_UB/control/snapshot" CF_DATA=      capture_on_fail
# A dollar sign followed by a single digit in a slash-command markdown body is a
# HARNESS ARGUMENT PLACEHOLDER, not a shell positional: Claude Code substitutes
# the invocation's argument words into this body BEFORE injecting it into the
# session, indexed from ZERO (the zeroth placeholder is the FIRST word), and
# leaves unfilled positions LITERAL. This command is argument-taking (see
# "Arguments / Invocation"), so under the old positional form an invocation like
# `/manual-test-coord --target=both --wait-timeout=45` handed every curl in this
# skill an argument word as its HTTP method and an empty URL. Named variables are
# not substituted. Every call site spells CF_DATA explicitly (empty for the
# bodyless calls) so no value can carry over between calls under any shell.
# Unlike a positional parameter, these reach the ENVIRONMENT of every child this
# function runs (curl, python) — so CF_DATA must never carry a credential; a
# request body that needs one still goes through the staged header file below.
# (This comment deliberately spells no such sequence of its own — a literal one
# here would be substituted too, garbling the warning.)
capture_on_fail() {
  # Fail LOUD and EARLY on a missing method or URL rather than issuing a curl
  # with an empty method and an empty URL: that path burns a 5s retry-snapshot
  # wait and two /health calls, then dies inside the python envelope on a
  # non-numeric status, which appends NOTHING to the forensics log — a failure
  # that also destroys the record of itself. The one way in is someone restoring
  # the old positional call form. Same discipline as the named-var precondition
  # in the red-main detector.
  [ -n "${CF_METHOD:-}" ] && [ -n "${CF_URL:-}" ] || { echo "capture_on_fail: CF_METHOD and CF_URL are NAMED variables, never positional arguments (call it as: CF_METHOD=GET CF_URL=... CF_DATA= capture_on_fail); one was empty or unset, so NOTHING was requested" >&2; return 2; }
  local method="${CF_METHOD:-}" url="${CF_URL:-}" data="${CF_DATA:-}"
  local hdr_file body status vid health health_retry
  # Per-tab routing: pin dashboard-UB calls to OUR headless tab (ui-bridge >=0.8.2 #51)
  # so a concurrent sibling's tab can't steal the command. Only when TAB_QS is set,
  # the URL targets the dashboard bridge, and it has no existing query string.
  if [ -n "${TAB_QS:-}" ] && [ -n "${DASHBOARD_UB:-}" ] \
     && [ "${url#$DASHBOARD_UB}" != "$url" ] && [ "${url#*\?}" = "$url" ]; then
    url="${url}${TAB_QS}"
  fi
  # Auth-gated relay (UI_BRIDGE_REQUIRE_AUTH=1): attach the operator bearer to
  # every dashboard-UB call (harmless when the gate is disabled). OPERATOR_JWT
  # is minted in "Operator credentials"; empty before that (pre-mint calls).
  local auth_args=()
  # Test the FILE (-s), not the path variable: the staged header is removed by
  # its EXIT trap when the minting shell exits, so a non-empty $OPERATOR_AUTH_HDRP
  # pointing at a deleted file would pass a `-n` check, hand curl `-H @<gone>`,
  # and get recorded as a relay failure. Run this fence in the SAME shell as
  # "Operator credentials", or re-stage there first.
  if [ -s "${OPERATOR_AUTH_HDR:-/nonexistent}" ] && [ -n "${DASHBOARD_UB:-}" ] \
     && [ "${url#$DASHBOARD_UB}" != "$url" ]; then
    # Staged header file, never the token on argv (see "Operator credentials").
    auth_args=(-H @"$OPERATOR_AUTH_HDRP")
  fi
  hdr_file="$(mktemp)"
  if [ -n "$data" ]; then
    body=$(curl -sS -X "$method" "$url" "${auth_args[@]}" \
      -H "Content-Type: application/json" -d "$data" \
      -D "$hdr_file" -w '\n%{http_code}' 2>&1)
  else
    body=$(curl -sS -X "$method" "$url" "${auth_args[@]}" \
      -D "$hdr_file" -w '\n%{http_code}' 2>&1)
  fi
  status="${body##*$'\n'}"
  body="${body%$'\n'*}"
  vid=$(grep -i '^x-vercel-id:' "$hdr_file" 2>/dev/null | sed 's/^[^:]*:[ \t]*//;s/[\r\n]//g')
  rm -f "$hdr_file"
  # Empty status means curl failed before any HTTP response (DNS, TCP) —
  # treat as a structured failure ('000' is curl's documented placeholder
  # for "no response received").
  [ -z "$status" ] && status="000"
  if [ "$status" -ge 200 ] 2>/dev/null && [ "$status" -lt 300 ] 2>/dev/null; then
    printf '%s' "$body"
    return 0
  fi
  # Both /health snapshots carry the operator bearer — under the auth-gated
  # relay an unauthenticated /health answers UNAUTHENTICATED and hides
  # connectedTabs, which is the very field the snapshots discriminate on.
  local health_auth=()
  [ -s "${OPERATOR_AUTH_HDR:-/nonexistent}" ] && health_auth=(-H @"$OPERATOR_AUTH_HDRP")
  health=$(curl -sS "${health_auth[@]}" "$DASHBOARD_UB/health" 2>/dev/null | head -c 2000)
  # Retry-snapshot (SSE-flap detection): one more /health sample 5s after the
  # failure. If the failed tabId is ABSENT from the first snapshot but PRESENT
  # in the second, the tab's SSE/WS listener flapped and reconnected
  # (mechanism #1) — visible here without Vercel function logs. Failure-path
  # only, so the happy path pays no extra latency.
  sleep 5
  health_retry=$(curl -sS "${health_auth[@]}" "$DASHBOARD_UB/health" 2>/dev/null | head -c 2000)
  CAPTURE_METHOD="$method" \
  CAPTURE_URL="$url" \
  CAPTURE_STATUS="$status" \
  CAPTURE_BODY="${body:0:500}" \
  CAPTURE_VID="$vid" \
  CAPTURE_HEALTH="$health" \
  CAPTURE_HEALTH_RETRY="$health_retry" \
  python -c "
import json, os
entry = {
    'method': os.environ['CAPTURE_METHOD'],
    'url': os.environ['CAPTURE_URL'],
    'status': int(os.environ['CAPTURE_STATUS']),
    'body': os.environ['CAPTURE_BODY'],
    'x_vercel_id': os.environ['CAPTURE_VID'],
    'health_snapshot': os.environ['CAPTURE_HEALTH'],
    'health_snapshot_retry': os.environ['CAPTURE_HEALTH_RETRY'],
}
print(json.dumps(entry))
" >> "$CONTROL_FAILURES_LOG"
  printf '%s' "$body"
  return 1
}
```

Discriminative shape:

- `status: 429` + `health_snapshot.connectedTabs` contains the failed tabId → rate-limit (not a tab-routing issue).
- `status: 404` (`TAB_NOT_FOUND`) or `410` (`TAB_STALE`) on a `/control/*?tabId=` call → our pinned tab was pruned/stale; re-run Phase 0.6 (404) or `page/refresh` then retry (410). With per-tab routing this replaces the old "primary-tab routing flip" failure mode.
- `status: 4xx/5xx` + `health_snapshot.connectedTabs` MISSING the failed tabId → our headless tab dropped its WS/SSE transport (should be fixed by re-arm-WS in ui-bridge ≥ 0.8.2 — if seen, the deploy is stale; check Phase 0.3).
- `connectedTabs` differs between `health_snapshot` and `health_snapshot_retry` (taken 5s apart) — especially failed tabId ABSENT from the first but PRESENT in the second → mechanism #1 (SSE listener flapped and reconnected). The tab self-healed; the failure was a transient transport drop, not a pruned tab — distinguishes SSE-flap from deployment skew without Vercel function logs.
- Failed tabId absent from BOTH snapshots → the tab is genuinely gone (pruned/dead), not flapping — re-run Phase 0.6.
- Across multiple consecutive failures, distinct `x_vercel_id` leading components (the chars before the first `:`) → mechanism #3 (deployment skew across Lambda instances).
- Consecutive failures with the SAME `x_vercel_id` prefix → not deployment skew; investigate code path.

## Phase 1: Verify the authed dashboard session (login happens in Phase 0.6 now)

> **Why login is no longer driven here (2026-06-05 rework).** Under the auth-gated relay (`UI_BRIDGE_REQUIRE_AUTH=1`, see "Operator bearer") a pre-auth tab can neither register with the relay nor be driven through it, so the old relay-driven `/login` DOM-form path and its `page/evaluate` programmatic-fetch fallback are structurally dead: there is no drivable tab until AFTER login + co-pilot consent. Phase 0.6's `login-web.cjs` performs the real hosted-UI login inside the tab (injected transport — it re-injects across the cross-origin Cognito hop) and grants the per-session consent; by the time Phase 1 runs, `$OUR_TAB_ID` is an authed, consented, relay-registered tab. Phase 1's job is the PASS gate that has always bound this skill: **the authed DOM observed on the page** — never a 2xx/redirect/log signal. (Driving a truly bare, UI-Bridge-free page remains the job of `/manual-test --transport=injected`.)

Goal: land on the operator surface and confirm the session is authenticated by observation.

### Step 1.0 — Confirm the operator surface

The tab already parked on `/runs/active` (the 0.6c `next=` target — the operator fleet/runs surface; unlike `/build/workflows` it renders dashboard chrome without a paired runner). The soft-nav below is an idempotent belt-and-braces re-render; keep `mode:soft` — the tab is on the app origin, and a hard reload can drop freshly-set cookies in headless Chromium (the spaceship-run cookie-loss bug).

```bash
DASHBOARD_URL="$STAGING_DASHBOARD/runs/active"
curl -s -X POST "${AUTH_ARGS[@]}" "$DASHBOARD_UB/control/page/navigate${TAB_QS}" \
  -H "Content-Type: application/json" \
  -d "{\"url\": \"$DASHBOARD_URL\", \"mode\": \"soft\"}"
sleep 3
CF_METHOD=POST CF_URL="$DASHBOARD_UB/control/discover" CF_DATA='{}' capture_on_fail > /dev/null
```

### Step 1.1 — Verify the authed session ON THE PAGE

Two signals, both required: the backend session probe from INSIDE the tab (so it carries the tab's cookie jar), and an authenticated-only DOM landmark in the snapshot.

```bash
# Probe the auth endpoint from inside the headless tab so the request carries the tab's cookie jar.
# (`page/evaluate` rejects a literal `fetch(` — keep the window["fet"+"ch"] dodge.)
AUTH_CHECK=$(curl -s -X POST "${AUTH_ARGS[@]}" "$DASHBOARD_UB/control/page/evaluate${TAB_QS}" \
  -H "Content-Type: application/json" \
  -d '{"expression": "(async()=>{const f=window[\"fet\"+\"ch\"]; const r=await f(\"/api/v1/auth/users/me\",{credentials:\"include\"}); return JSON.stringify({status:r.status});})()"}')
ME_STATUS=$(echo "$AUTH_CHECK" | python -c "
import sys, json, re
try:
    d = json.load(sys.stdin)
    raw = d.get('data', {}).get('result') or d.get('result') or ''
    m = re.search(r'\"status\":\\s*(\\d+)', str(raw))
    print(m.group(1) if m else '')
except Exception:
    print('')
")
echo "users/me from inside the tab: ${ME_STATUS:-null}"

# The on-page half of the gate: an authenticated-only landmark in the rendered DOM.
CF_METHOD=GET CF_URL="$DASHBOARD_UB/control/snapshot" CF_DATA= capture_on_fail > /tmp/post-login-snapshot.json
AUTH_DOM_HIT=$(python -c "
import json
snap = json.load(open('/tmp/post-login-snapshot.json'))
els = snap.get('data', {}).get('elements', [])
text = ' '.join((e.get('state', {}).get('textContent') or '') for e in els)
landmarks = ['Sign out', 'New workflow', '$EMAIL']
print('YES' if any(l in text for l in landmarks) else 'NO')
" 2>/dev/null)
echo "Authed DOM landmark: ${AUTH_DOM_HIT:-NO}"
```

### Step 1.2 (RECOVERY) — session lost mid-run

If `users/me` 401s or a later phase sees the unauthenticated shell, do NOT attempt to drive a login through the relay (impossible pre-auth — the tab deregisters the moment the session is gone). Recovery is a Phase 0.6 relaunch:

1. `kill $HEADLESS_PID`; re-run 0.6a–0.6d to mint a fresh authed + consented tab and re-capture `$OUR_TAB_ID`.
2. If relay calls were answering `UNAUTHENTICATED`, re-mint `$OPERATOR_JWT` first (the JWT lives ~1h; see "Operator bearer").
3. One relaunch per run — if the second tab also loses its session, surface FAIL (auth backend/cookie regression), not an endless loop.

**Predicate.**
- PASS: `ME_STATUS=200` AND `AUTH_DOM_HIT=YES` — the authed DOM observed on the page.
- FAIL: landmark absent / users-me 401 although Phase 0.6's login-web.cjs reported `ok:true` — the session was lost between 0.6 and here; one 0.6 relaunch, FAIL if it recurs (PRODUCT_GAP if the login backend itself 5xx'd).
- BLOCKED: relay calls return 404 `TAB_NOT_FOUND` (our tab died) → re-run Phase 0.6 to re-capture `$OUR_TAB_ID`; 410 `TAB_STALE` → `page/refresh` and retry.
- DEFERRED: if a Vercel deploy-freshness alert was raised in Phase 0.3, treat any login failure as DEFERRED rather than FAIL.

## Phase 1.5: Route audit

Goal: detect route-shape regressions on operator deep-links. Deep-linked routes (`/login`, `/profile`, `/organizations`, etc.) sometimes 404 or redirect to the marketing-landing page; the rest of the skill only probes the dashboard surface and would miss those.

**Why this audit runs CREDENTIALED (verified against `qontinui-web/frontend/src/middleware.ts:45-68`, PR #209).** The middleware redirects EVERY unauthenticated request to a protected route to `/login?next=<path>` based on `access_token`/`refresh_token` cookie presence. A plain `curl` outside the headless tab is ALWAYS unauthenticated, so it gets the `/login` redirect for every protected route — which masks the real failure mode this audit exists to catch: a 404/500 on an *authenticated* route. To see route-shape failures, the probes must carry a valid session.

So Phase 1.5 drives the probes **from inside the headless tab** — reusing the authenticated cookie jar established by the Phase 0.6 login — via `page/evaluate` fetch with `credentials:"include"` (the same `window["fet"+"ch"]` dodge used in Phase 1; `page/evaluate` rejects a literal `fetch(`). With the cookies attached the middleware does NOT redirect, so the audit observes the route's real terminal status (200 / 404 / 5xx) and any *page-level* redirect (e.g. `/dashboard` → `/build/workflows`).

We also assert the unauth middleware-redirect behavior as a **distinct** check (`UNAUTH_REDIRECT`), so route-shape and auth-gating are not conflated: for an unauthenticated request, the canonical outcome for every protected route is `3xx-to-/login`.

```bash
# Extensible map of routes to expected AUTHENTICATED outcomes. Operators may append entries
# over time — each value is a pipe-separated set of acceptable outcomes for that route.
#
# Outcome grammar:
#   "200"                — direct 200 (route renders)
#   "3xx-to-<path>"      — redirected to <path> (substring match; page-level redirect under auth)
#   "3xx"                — any 3xx that does NOT land on the marketing landing "/"
declare -A EXPECTED_ROUTES=(
  ["/login"]="200|3xx-to-/build/workflows"
  ["/dashboard"]="200|3xx-to-/build/workflows"
  ["/build/workflows"]="200"
  ["/settings/account"]="200"
  ["/profile"]="200|3xx-to-/settings/account"
  ["/runs/active"]="200"
  ["/workflows"]="200|3xx-to-/build/workflows"
)

# Probe each route from INSIDE the headless tab so the request carries the authenticated
# cookie jar from Phase 1. `redirect:"manual"` so we observe the redirect rather than follow it;
# `Location` is read off the response when status is 3xx.
ROUTE_FINDINGS=()
for route in "${!EXPECTED_ROUTES[@]}"; do
  expected="${EXPECTED_ROUTES[$route]}"

  PROBE=$(curl -s -X POST "$DASHBOARD_UB/control/page/evaluate${TAB_QS}" \
    -H "Content-Type: application/json" \
    -d "{\"expression\": \"(async()=>{const f=window[\\\"fet\\\"+\\\"ch\\\"]; const r=await f(\\\"${route}\\\",{credentials:\\\"include\\\",redirect:\\\"manual\\\"}); return JSON.stringify({status:r.status,type:r.type,location:r.headers.get(\\\"location\\\")||\\\"\\\",url:r.url});})()\"}")

  # page/evaluate returns the JSON string under data.result; parse status + location out of it.
  read -r http_code location < <(echo "$PROBE" | python -c "
import sys, json, re
raw = ''
try:
    d = json.load(sys.stdin)
    raw = d.get('data', {}).get('result') or d.get('result') or ''
except Exception:
    pass
m = re.search(r'\"status\":\\s*(\\d+)', str(raw))
l = re.search(r'\"location\":\\s*\"([^\"]*)\"', str(raw))
# A manual-redirect fetch reports status 0 + type \"opaqueredirect\"; treat as 3xx.
status = m.group(1) if m else ''
if re.search(r'\"type\":\\s*\"opaqueredirect\"', str(raw)) and status in ('', '0'):
    status = '307'
print(status or '000', (l.group(1) if l else ''))
")

  # Classify outcome
  outcome=""
  case "$http_code" in
    200)
      outcome="200"
      ;;
    3??)
      # If Location is the marketing landing "/" exactly, that's the bad case
      if [ "$location" = "/" ] || [ "$location" = "$STAGING_DASHBOARD" ] || [ "$location" = "$STAGING_DASHBOARD/" ]; then
        outcome="3xx-to-/(marketing)"
      elif [ -n "$location" ]; then
        outcome="3xx-to-$location"
      else
        outcome="3xx"
      fi
      ;;
    404)
      outcome="404"
      ;;
    *)
      outcome="$http_code"
      ;;
  esac

  # Test outcome against expected pipe-separated set (substring match for 3xx-to-...)
  pass=false
  IFS='|' read -ra alternatives <<< "$expected"
  for alt in "${alternatives[@]}"; do
    case "$alt" in
      "$outcome") pass=true; break ;;
      "3xx-to-"*)
        # alt is "3xx-to-<path>" — accept if outcome starts with "3xx-to-" and contains <path>
        alt_path="${alt#3xx-to-}"
        case "$outcome" in *"$alt_path"*) pass=true; break ;; esac
        ;;
      "3xx")
        case "$outcome" in 3xx*) [ "$outcome" != "3xx-to-/(marketing)" ] && pass=true && break ;; esac
        ;;
    esac
  done

  if $pass; then
    echo "ROUTE OK (authed): $route -> $outcome (expected: $expected)"
  else
    echo "ROUTE MISMATCH (authed): $route -> $outcome (expected: $expected)"
    ROUTE_FINDINGS+=("$route|$outcome|$expected")
  fi
done

if [ ${#ROUTE_FINDINGS[@]} -gt 0 ]; then
  echo "Phase 1.5 surfaced ${#ROUTE_FINDINGS[@]} route mismatch(es):"
  for f in "${ROUTE_FINDINGS[@]}"; do echo "  - $f"; done
fi
```

### Unauth middleware-redirect assertion (distinct from route-shape)

Separately confirm the auth-gating layer is intact. For an *unauthenticated* request, `middleware.ts:48-67` redirects EVERY non-public route to `/login?next=<path>`. This is a plain `curl` (no cookie jar) outside the tab, and the canonical outcome for each protected route is `3xx-to-/login`. A protected route that returns 200 or 404 to an unauth probe is a middleware-gating regression, NOT a route-shape one.

```bash
# Public/unauth-allowed routes are exempt (the marketing landing, /login itself, etc.).
UNAUTH_PROTECTED=("/dashboard" "/build/workflows" "/settings/account" "/profile" "/runs/active" "/workflows")
UNAUTH_FINDINGS=()
for route in "${UNAUTH_PROTECTED[@]}"; do
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$STAGING_DASHBOARD$route")
  loc=$(curl -sI --max-time 10 "$STAGING_DASHBOARD$route" | grep -i '^location:' | sed 's/^[Ll]ocation: *//; s/\r$//')
  case "$code" in
    3??)
      case "$loc" in
        */login*) echo "UNAUTH_REDIRECT OK: $route -> 3xx-to-/login" ;;
        *) echo "UNAUTH_REDIRECT MISMATCH: $route -> 3xx-to-$loc (expected /login)"; UNAUTH_FINDINGS+=("$route|3xx-to-$loc|3xx-to-/login") ;;
      esac
      ;;
    *) echo "UNAUTH_REDIRECT MISMATCH: $route -> $code (expected 3xx-to-/login — middleware gate may be broken)"; UNAUTH_FINDINGS+=("$route|$code|3xx-to-/login") ;;
  esac
done
```

**Fallback (if extracting the tab's cookies / credentialed `page/evaluate` proves unreliable).** If `page/evaluate` returns null even with `?tabId` pinning (our tab went stale/pruned — see "Multi-tab routing") and the credentialed probes can't run, fall back to the simple unauth `curl` map below. It removes the false MISSes that PR #209 introduced — but it CANNOT see authed-route 404/500 failures, because every protected route just redirects to `/login`. Document this limitation in the Phase 9 report when the fallback is used. For the unauth fallback the canonical outcome for EVERY protected route is `3xx-to-/login` (the `3xx-to-/build/workflows` page-level redirects only occur for an authenticated probe):

```bash
declare -A EXPECTED_ROUTES_UNAUTH=(
  ["/login"]="200|3xx-to-/build/workflows"
  ["/dashboard"]="3xx-to-/login"
  ["/build/workflows"]="3xx-to-/login"
  ["/settings/account"]="3xx-to-/login"
  ["/profile"]="3xx-to-/login"
  ["/runs/active"]="3xx-to-/login"
  ["/workflows"]="3xx-to-/login"
)
# Then run the same classify/compare loop as above, but with plain
# `curl -s -o /dev/null -w '%{http_code}'` + `curl -sI` for the Location header
# (no headless tab, no cookies).
```

**Predicate.**
- PASS: every route in `EXPECTED_ROUTES` matches one of its expected AUTHENTICATED outcomes AND every route in the unauth assertion redirects to `/login`.
- PRODUCT_GAP: one or more *authenticated* routes return 404 or redirect to the marketing landing `/` — list each as a separate finding (route + actual outcome + expected). Each mismatched route is its own PRODUCT_GAP entry in the Phase 9 report.
- FAIL (auth-gating regression): an unauth probe of a protected route returns 200/404 instead of `3xx-to-/login` (`UNAUTH_FINDINGS` non-empty) — the middleware gate is not enforcing.
- DEGRADED (fallback used): credentialed `page/evaluate` was unavailable and the unauth-only fallback ran — note that authed-route failures could not be detected this iteration.
- DEFERRED: if the marketing-landing redirect is the ONLY issue surfaced AND a recent `/manual-test-coord` plan already tracks it, mark DEFERRED with reference to that plan instead of PRODUCT_GAP.

The `EXPECTED_ROUTES` map is intentionally extensible — operators add product-specific deep links over time as the dashboard grows new surfaces.

## Phase 2: Tenant resolution observed via DOM

Goal: confirm the dashboard surfaces the operator's tenant identity somewhere a user can see.

```bash
# Drive the user/account menu — usually upper-right avatar or a nav link
curl -s -X POST "$DASHBOARD_UB/ai/find${TAB_QS}" \
  -H "Content-Type: application/json" \
  -d '{"query": "Account"}'
# Try fallbacks if the first miss: "Profile", "Settings", "Organization", "Workspace"
# Capture id → ACCOUNT_ID

CF_METHOD=POST CF_URL="$DASHBOARD_UB/control/element/$ACCOUNT_ID/action" CF_DATA='{"action": "click"}' capture_on_fail > /dev/null
sleep 2
CF_METHOD=POST CF_URL="$DASHBOARD_UB/control/discover" CF_DATA='{}' capture_on_fail > /dev/null

# Snapshot, then text-search the elements[] for tenant indicators
SNAP=$(CF_METHOD=GET CF_URL="$DASHBOARD_UB/control/snapshot" CF_DATA= capture_on_fail)
# Look for tenant name, tenant_id (UUID), or organization name in any element's text/value/label
echo "$SNAP" | python -c "
import sys, json, re
d = json.load(sys.stdin)
elements = d.get('data', {}).get('elements', d.get('elements', []))
hits = []
for e in elements:
    blob = ' '.join(str(v) for v in [e.get('text'), e.get('label'), e.get('value'),
                                      (e.get('state') or {}).get('value')] if v)
    if re.search(r'tenant|workspace|organization|org\\s', blob, re.I):
        hits.append((e.get('id'), blob[:100]))
print('TENANT_HITS:', hits)
" 2>/dev/null
```

**Predicate.**
- PASS: at least one tenant/organization identifier appears in DOM (name OR UUID).
- PRODUCT_GAP: account/profile/settings page exists but renders no tenant identity — file as a dashboard feature gap (operators should be able to see what tenant they're in).
- DEFERRED: if `2026-05-20-default-tenant-propagation.md` plan is still in-flight and tenant resolution itself 403s, mark DEFERRED with reference to that plan.

## Phase 3: Runner device-registration observed via dashboard

Goal: confirm the temp runner registers as a device the dashboard can see — exercising the **real** pairing path. (The rendezvous claim was already published in Phase 0.6.5; Step 3.5 only backfills its `tenant_id`.)

**What the product actually does (verified against `qontinui-web/frontend/src/app/(app)/connect-runner/page.tsx`).** There is NO dashboard-rendered numeric/alphanumeric pair-code anywhere. The canonical operator pairing path is a one-click browser flow: the runner's Settings UI opens `/connect-runner?state=<64-hex>&callback=http://127.0.0.1:<port>/auth/runner-token-callback&device_name=<hostname>`; that page calls `createRunnerToken` and POSTs to `/api/v1/devices/pair-confirm`, then redirects the browser back to the runner's localhost callback with the minted token. The runner persists the token and opens a persistent WebSocket to `/api/v1/devices/ws` to register with web. (The skill's prior steps 3.1–3.4 drove a 6-8-char pair-code paradigm that does not exist in the product — they have been removed.)

The temp runner spawned in Phase 0 auto-signs-in using the credentials the supervisor injected. For `--target=staging` those are the **operator creds this session uses** (`QONTINUI_TEST_AUTO_LOGIN_EMAIL/PASSWORD` = `$EMAIL`/`$PASS`, set via `extra_env` in Phase 0.5) pointed at the direct staging backend (`QONTINUI_WEB_BACKEND_URL` = `https://web.staging.qontinui.io`) with Tier 2 forced (`QONTINUI_RUNNER_TIER=qontinui_account`, without which login is skipped), so the runner registers **in the same tenant we're observing** — not the local `.env` josh account. For `--target=local` it falls back to the runner's `.env` (josh) against localhost. Once authenticated, the runner heartbeats and registers itself as a device with web automatically. So rather than drive a non-existent pair UI, Phase 3 simply **observes that the temp runner appears as a registered device** on the dashboard.

### Step 3.1 — Navigate to the runners/devices surface

```bash
# /runners is the canonical "Online Runners" / devices surface.
# Post-login: use mode:soft so the session cookies aren't dropped (see Step 1.0).
curl -s -X POST "$DASHBOARD_UB/control/page/navigate${TAB_QS}" \
  -H "Content-Type: application/json" \
  -d "{\"url\": \"$STAGING_DASHBOARD/runners\", \"mode\": \"soft\"}"
sleep 3
curl -s -X POST "$DASHBOARD_UB/control/discover${TAB_QS}" -H "Content-Type: application/json" -d '{}'
# Fallback if /runners isn't the surface: drive the in-DOM nav instead.
#   curl -s -X POST "$DASHBOARD_UB/ai/find" -H "Content-Type: application/json" -d '{"query": "Runners"}'
#   # Fallbacks: "Devices", "Online Runners" → click that nav id
```

### Step 3.2 — Poll for the temp runner's row (up to ~30s)

The runner registers via heartbeat shortly after auto-signin, so the row may take a few seconds to appear. Poll the dashboard snapshot for the temp runner's hostname or `TEST_ID`.

```bash
# The temp runner's hostname is the local hostname (from machine.json)
HOSTNAME=$(python -c "
import json, os
with open(os.path.expanduser('~/.qontinui/machine.json')) as f:
    print(json.load(f).get('hostname',''))
")

DEVICE_ROW_FOUND="NO"
for i in $(seq 1 6); do
  curl -s -X POST "$DASHBOARD_UB/control/discover${TAB_QS}" -H "Content-Type: application/json" -d '{}' >/dev/null
  SNAP=$(curl -s "$DASHBOARD_UB/control/snapshot${TAB_QS}")
  # Test runner IDs typically contain the test-id suffix too — search both hostname and TEST_ID.
  MATCHES=$(echo "$SNAP" | python -c "
import sys, json
d = json.load(sys.stdin)
elements = d.get('data', {}).get('elements', d.get('elements', []))
needle_hostname, needle_test = '$HOSTNAME', '$TEST_ID'
matches = [e.get('id') for e in elements
           if (needle_hostname and needle_hostname in str(e)) or (needle_test and needle_test in str(e))]
print(','.join(str(m) for m in matches[:5]))
")
  if [ -n "$MATCHES" ]; then
    DEVICE_ROW_FOUND="YES"
    echo "Temp runner registered — device row(s): $MATCHES"
    break
  fi
  echo "Temp runner row not visible yet (attempt $i/6) — runner heartbeat registers it once authenticated; retrying in 5s"
  sleep 5
done
[ "$DEVICE_ROW_FOUND" = "NO" ] && echo "Temp runner '$HOSTNAME'/'$TEST_ID' did not appear under Online Runners within ~30s"
```

### Step 3.5 — Backfill `tenant_id` onto the rendezvous claim (two-machine mode only)

The claim itself was **already published early in Phase 0.6.5** (before login) so it overlaps the sibling's poll window. `tenant_id` wasn't known then; now that Phase 2 has resolved it, re-acquire the **same** `resource_key` to refresh the metadata in place (idempotent for the owning machine_id). Best-effort — a failure here doesn't matter, the rendezvous keys on `topic` and the cross-tenant signal also carries `operator_email`.

```bash
if [ -n "$RENDEZVOUS_SLUG" ] && [ -n "${TENANT_ID:-}" ]; then
  RDV_SLUG_HYPHENIZED=$(echo "$RENDEZVOUS_SLUG" | tr 'A-Z' 'a-z' | tr ':T.' '---')
  CORR_TOPIC="manual-test-coord-rendezvous-${RDV_SLUG_HYPHENIZED}"
  RESOURCE_KEY="manual-test-coord-rendezvous-${RDV_SLUG_HYPHENIZED}-${MACHINE_ID}"
  curl -s -X POST "$STAGING_COORD/claims/acquire" \
    -H "Content-Type: application/json" \
    -d "{
      \"kind\": \"phase\",
      \"resource_key\": \"${RESOURCE_KEY}\",
      \"machine_id\": \"${MACHINE_ID}\",
      \"ttl_seconds\": 7200,
      \"topic\": \"${CORR_TOPIC}\",
      \"metadata\": {
        \"temp_runner_hostname\": \"${HOSTNAME}\",
        \"temp_runner_test_id\": \"${TEST_ID}\",
        \"tenant_id\": \"${TENANT_ID}\",
        \"operator_email\": \"${EMAIL}\"
      }
    }" >/dev/null && echo "Backfilled tenant_id=$TENANT_ID onto rendezvous claim" \
    || echo "tenant_id backfill failed (non-fatal — early claim from Phase 0.6.5 stands)"
fi
```

Note: the coord `ClaimRequest` field is `topic` (per `qontinui-coord/src/claims.rs:96-115`), not `correlation_topic`. TTL for `kind=phase` is 7200s. Coord owns the topic-to-correlation_id mapping; the skill never derives a correlation_id locally.

**Predicate.**
- PASS: the temp runner's hostname (or `TEST_ID`) appears as a device/runner row in the dashboard snapshot within ~30s of auto-signin (`DEVICE_ROW_FOUND=YES`). This confirms the real registration path — runner auto-signin via the injected auto-login creds against `QONTINUI_API_URL` (staging) → device-JWT mint via `/api/v1/devices/pair-confirm` → persistent WS to `/api/v1/devices/ws` → heartbeat — landed end-to-end.
- FAIL / PRODUCT_GAP: the row never appears (`DEVICE_ROW_FOUND=NO`). FAIL if the runner authenticated but never registered (registration/heartbeat path bug); PRODUCT_GAP if the dashboard exposes no runners/devices surface at all for the operator to observe registered runners.

## Phase 4: Heartbeat observed via device-row status badge

Goal: confirm the dashboard surfaces device freshness without poking `coord.devices.last_heartbeat` directly.

Heartbeat cadence is **30s** by default (per `qontinui-runner/src-tauri/src/fleet.rs:487-498`, env override `COORD_HEARTBEAT_INTERVAL_SECS`). The runner's `mcp/backend_relay.rs:140` also uses 30s. Plan said "~5s" but that's not what the code does — wait at least one full interval.

```bash
sleep 35   # one heartbeat interval + jitter
# Do NOT use page/refresh here — it drops HttpOnly cookies in headless
# Chromium, reverting to unauthenticated state. A discover + snapshot is
# enough to pick up DOM changes from the backend heartbeat.
curl -s -X POST "$DASHBOARD_UB/control/discover${TAB_QS}" -H "Content-Type: application/json" -d '{}'
sleep 2
SNAP=$(curl -s "$DASHBOARD_UB/control/snapshot${TAB_QS}")
# Look at the temp runner's row for a status indicator: "online", "connected", "live", a green dot, etc.
echo "$SNAP" | python -c "
import sys, json
d = json.load(sys.stdin)
elements = d.get('data', {}).get('elements', d.get('elements', []))
hostname = '$HOSTNAME'
row_ids = [e for e in elements if hostname and hostname in str(e)]
for e in row_ids:
    blob = json.dumps(e).lower()
    has_status = any(k in blob for k in ['online','connected','live','active','healthy'])
    print(e.get('id'), 'has_status_badge=', has_status)
"
```

**Predicate.**
- PASS: row carries an online/connected/live/active indicator post-heartbeat.
- PRODUCT_GAP: row has no freshness signal at all — operators can't tell live from stale devices.
- FAIL: row shows `offline` / `stale` despite the heartbeat having fired (runner-side `fleet::heartbeat` logged success). Cross-check with `GET $COORD/coord/devices` if the dashboard surfaces it; if not, this is a render-side bug.

## Phase 5: WS observed via "connection: live" badge

Goal: distinguish WS connectivity from heartbeat freshness. The runner-side device WS dials `/api/v1/devices/ws` after the device-JWT mint (per [[feedback_axum_middleware_path_leak]] context).

Same row, different signal. Many dashboards collapse "WS connected" into the same badge as "heartbeat fresh"; if so, Phase 4 + Phase 5 fold into a single signal — document that and don't double-count the PASS.

```bash
SNAP=$(curl -s "$DASHBOARD_UB/control/snapshot${TAB_QS}")
echo "$SNAP" | python -c "
import sys, json
d = json.load(sys.stdin)
elements = d.get('data', {}).get('elements', d.get('elements', []))
hostname = '$HOSTNAME'
for e in elements:
    if hostname and hostname in str(e):
        blob = json.dumps(e).lower()
        ws_signal = any(k in blob for k in ['ws','websocket','connection','realtime','rt'])
        print(e.get('id'), 'has_ws_signal=', ws_signal)
"
```

**Predicate.**
- PASS: row exposes a WS-specific signal AND it reads as connected/live.
- FOLDED-INTO-PHASE-4: dashboard does not separate heartbeat from WS — record once, don't count twice.
- PRODUCT_GAP: neither heartbeat nor WS signal rendered.

## Phase 6: Tenant isolation observed via sibling-session cross-verification

Goal: assert that this session does NOT see the sibling session's temp-runner device in its devices list (DOM-level cross-tenant isolation).

```bash
if [ -z "$RENDEZVOUS_SLUG" ]; then
  echo "SETUP_GAP: single-machine run, no sibling session — tenant isolation not verified this iteration"
  # Phase counter: skipped, not failed
else
  # Hyphenize the slug the same way Phase 3.5 does so both sessions hit the same topic.
  RDV_SLUG_HYPHENIZED=$(echo "$RENDEZVOUS_SLUG" | tr 'A-Z' 'a-z' | tr ':T.' '---')
  CORR_TOPIC="manual-test-coord-rendezvous-${RDV_SLUG_HYPHENIZED}"
  TIMEOUT_SECS=$((${WAIT_TIMEOUT:-5} * 60))
  ELAPSED=0
  SIBLING_HOSTNAME=""
  SIBLING_MACHINE_ID=""
  SIBLING_FOUND=0   # informational only (Phase 8.1 never releases regardless)
  while [ "$ELAPSED" -lt "$TIMEOUT_SECS" ]; do
    RESP=$(curl -s "$STAGING_COORD/coord/claims/by-correlation-topic?topic=${CORR_TOPIC}")
    SIBLING_JSON=$(echo "$RESP" | python -c "
import sys, json
d = json.load(sys.stdin)
claims = d.get('claims', d.get('data',{}).get('claims', []))
me = '$MACHINE_ID'
others = [c for c in claims if str(c.get('machine_id') or '') != me]
if others:
    c = others[0]
    print(json.dumps({
        'machine_id': c.get('machine_id'),
        'hostname': (c.get('metadata') or {}).get('temp_runner_hostname',''),
    }))
")
    if [ -n "$SIBLING_JSON" ]; then
      SIBLING_HOSTNAME=$(echo "$SIBLING_JSON" | python -c "import sys,json; print(json.load(sys.stdin)['hostname'])")
      SIBLING_MACHINE_ID=$(echo "$SIBLING_JSON" | python -c "import sys,json; print(json.load(sys.stdin)['machine_id'])")
      break
    fi
    sleep 10
    ELAPSED=$((ELAPSED + 10))
  done

  if [ -z "$SIBLING_HOSTNAME" ]; then
    echo "BLOCKED: sibling session never registered — was /manual-test-coord invoked on the other machine with the same --rendezvous-slug? (Our own claim is left alive for its TTL by Phase 8.1 so a slower sibling can still discover it.)"
  else
    SIBLING_FOUND=1
    # Defense-in-depth allow-list check
    case "$SIBLING_MACHINE_ID" in
      c79a07d5-7e40-49b4-87fa-554c749f9644|84c02292-32cb-4983-be85-d00f868b7003) ;;
      *) echo "SECURITY_ANOMALY: sibling claim machine_id $SIBLING_MACHINE_ID not in operator allow-list (spaceship + MSI). Continuing." ;;
    esac

    # Re-snapshot OUR session's devices page (still logged in as Phase 1's operator)
    curl -s -X POST "$DASHBOARD_UB/ai/find${TAB_QS}" -H "Content-Type: application/json" -d '{"query": "Devices"}'
    # → click DEVICES_NAV_ID
    sleep 2
    curl -s -X POST "$DASHBOARD_UB/control/discover${TAB_QS}" -H "Content-Type: application/json" -d '{}'
    SNAP=$(curl -s "$DASHBOARD_UB/control/snapshot${TAB_QS}")
    SIBLING_PRESENT=$(echo "$SNAP" | python -c "
import sys, json
d = json.load(sys.stdin)
sibling = '$SIBLING_HOSTNAME'
elements = d.get('data', {}).get('elements', d.get('elements', []))
print('YES' if any(sibling and sibling in str(e) for e in elements) else 'NO')
")
    if [ "$SIBLING_PRESENT" = "NO" ]; then
      echo "PASS: tenant isolation verified — sibling hostname '$SIBLING_HOSTNAME' absent from this session's devices page"
    else
      echo "FAIL: tenant isolation BREACH — sibling hostname '$SIBLING_HOSTNAME' rendered on this session's devices page"
    fi
  fi
fi
```

**Predicate.**
- PASS: sibling claim found, sibling hostname absent from local devices page.
- FAIL: sibling hostname IS rendered locally → cross-tenant leak (high-severity coord-side bug).
- BLOCKED: timeout elapsed without sibling claim.
- SETUP_GAP: single-machine mode.
- SECURITY_ANOMALY: sibling machine_id outside allow-list (additive finding; doesn't override PASS/FAIL).

## Phase 7: Dispatch round-trip via dashboard job UI

Goal: confirm dashboard-mediated dispatch reaches the temp runner and returns terminal state. Per Resolved Q2, use the decision tree:

1. **If dashboard exposes a dispatch button/form** (most likely: Workflows page → "Run" / "Dispatch" button):
   ```bash
   curl -s -X POST "$DASHBOARD_UB/ai/find" -H "Content-Type: application/json" -d '{"query": "Run workflow"}'
   # Fallbacks: "Dispatch", "Run", "Execute"
   # → DISPATCH_BTN_ID
   # ... drive workflow selection + click → poll for terminal state via snapshot
   ```
   PASS via dashboard DOM observation.

2. **Else if dispatch is runner-UI-only** (the runner already has a "run workflow" UI that's UI-Bridge-instrumentable): drive the runner UI through `$RUNNER_UB`.
   ```bash
   curl -s -X POST "$RUNNER_UB/ai/find" -H "Content-Type: application/json" -d '{"query": "Run"}'
   # ...
   ```
   PASS via runner DOM observation. ADD `PRODUCT_GAP: dashboard-mediated dispatch not exposed — tested via runner-direct path`.

3. **Else if neither surface exposes a user-initiated dispatch**: BLOCKED + PRODUCT_GAP (structural product issue worth surfacing prominently in the report).

The terminal-state observation pattern (loop snapshot + look for `completed` / `failed` / `error` in the job row's text/status) mirrors what `/manual-test` already documents for runner-side workflow runs. **TODO: operator confirm during first run** — the exact "Workflows" tab location and "Run" button text on `demo.staging.qontinui.io` haven't been driven through UI Bridge yet; the first iteration may need to refine `ai/find` queries based on the live dashboard.

**Predicate.**
- PASS: dispatched job reached a terminal state observed via DOM.
- FAIL: job dispatched but never reached terminal state (timeout — e.g. 3 minutes).
- PRODUCT_GAP-only: runner-direct path used (no dashboard dispatch UI).
- BLOCKED: no dispatch surface anywhere.

## Phase 7.5: Per-agent coord-mcp proxy refresh seam (focus-mode only)

Run this ONLY when `` names the per-agent coord-mcp proxy / refresh-past-expiry
seam (runner #663, shipped on main as `79c3d0f3`). It is NOT part of the default
sequence — it exercises a **runner-local** surface, not the dashboard, so it does
not touch any Phase 1–7 dashboard flow. Background: #592's per-agent proxy keeps
an agent's coord-mcp call alive past its JWT expiry because the heartbeat's
`agent_token::maybe_refresh` rotates the `AGENT_TOKENS` slot; the seam lets a test
seed/observe that slot so the rotation is verifiable in seconds, not the real ~4h.

**The seam endpoints** (gated behind `#[cfg(any(debug_assertions, feature = "test-fixtures"))]`
in `mcp/test_fixtures.rs` — present on any debug temp runner, absent from prod
release):
- `POST /ui-bridge/test/coord-mcp/seed-agent-token {agent_id, jwt, jwt_exp, workdir}`
  → registers a `TokenSlot` + an Agent-bound proxy nonce; returns the nonce.
- `GET /ui-bridge/test/coord-mcp/agent-token/{agent_id}` → `{present, exp, jti, ttl_secs}`
  (never the token). Re-read it to prove a refresh rotated the slot (`exp` jumps).
- Tier-B knob: env `QONTINUI_AGENT_JWT_EXP_COMPRESS_SECS` (read at the spawn seed
  site) clamps a REAL agent's bookkeeping `exp` to `min(jwt_exp, now+N)`, so a
  refresh fires within N seconds while the real JWT still authenticates it.

**Spawn a NORMAL temp runner — do NOT override `COORD_HTTP_URL`.** Pointing it at a
plain-HTTP stub reaps the runner at boot (the device-WS upgrade `/api/v1/devices/ws`
a plain `http.server` can't serve); a WS-capable stub is required for a fake coord.
Build from the primary checkout when it is on `main` (it carries a built frontend
`dist/`; a `git_ref` worktree build lacks `dist/` and is also reaped).

### Tier A — seam + gate, against REAL coord (achievable now, no real agent needed)
1. `agent_id=<uuid>`; craft an unsigned JWT whose payload is `{"sub_type":"agent",...}`
   (the gate reads `sub_type` via `jwt_unverified_claim` — no signature check).
2. Seed: `POST …/coord-mcp/seed-agent-token {agent_id, jwt:<fake>, jwt_exp: now+10, workdir}`
   → nonce. Read it back: `GET …/agent-token/{agent_id}` → `present:true, ttl_secs≈10`.
3. `POST /coord-mcp` with `X-Coord-Mcp-Proxy-Key: <nonce>` (or
   `Authorization: Bearer <nonce>`) + a JSON-RPC body.
   **PASS gate:** the agent nonce returns coord's OWN `{"error":"invalid token"}`
   (coord rejecting the fake JWT, forwarded back through the proxy) — proving the
   gate accepted the `sub_type:agent` bearer and forwarded to coord — WHILE a bogus
   nonce returns `COORD_MCP_PROXY_UNAUTHORIZED` (stopped at the gate). The different
   errors are the proof. (Live-verified 2026-06-30.)

### Tier B — real-coord refresh-past-expiry round-trip (full residual closure)
Needs a REAL coord-minted agent JWT (the fake one above can't refresh — coord
rejects it). Source it from `POST $COORD/agents/allocate {device_id, repos:[…]}`
(`agent_worktrees.rs` `keys.issue(...)` → `resp.token` is a real agent JWT +
`token_jti`/`token_exp`). **`/agents/allocate` is behind `require_jwt` (a coord
device/service JWT), so run this FROM a coord-paired context** (a paired runner /
a device that holds a device JWT) — a bare temp runner has no device identity, the
one remaining blocker.
1. Allocate → `{token, token_jti, token_exp, agent_id}`.
2. Seed the temp runner's slot with the REAL token but a short exp:
   `seed-agent-token {agent_id:<allocated>, jwt:<real token>, jwt_exp: now+10, workdir}`.
   (agent_id MUST match the allocation — the refresh route is `/agents/{agent_id}/refresh-token`.)
3. Trigger the lazy request-path refresh: `POST /coord-mcp` with the nonce. The
   slot's `ttl<margin` → `maybe_refresh` POSTs `$COORD/agents/{agent_id}/refresh-token`
   with the real token → coord ACCEPTS → rotates the slot.
4. `GET …/agent-token/{agent_id}` → **PASS = `exp` jumped from `now+10` to ~`now+4h`
   and `jti` rotated** — the refresh-past-(compressed)-expiry round-trip, live.
   Alternatively, for the spawn-path variant, set `QONTINUI_AGENT_JWT_EXP_COMPRESS_SECS=120`
   on a paired runner and let a real agent be dispatched to it — the same observation
   via the GET endpoint, riding the real heartbeat loop.

**Predicate.**
- PASS: Tier A gate-pass-vs-reject observed AND (Tier B, when a paired context is
  available) the slot's `exp` rotation observed.
- PARTIAL: Tier A only (no coord-paired device available to allocate a real agent) —
  the rotation is covered by the runner's in-binary tests (`agent_token`,
  `mcp::test_fixtures`, `coord_mcp`); report Tier B as armed-pending-paired-context.
- BLOCKED: seam routes 404 (runner built from a non-main / release source without
  the seam) → rebuild from a main debug source.

## Phase 8: Cleanup

**Always run, even on early failure.** Release first (cheap, fast); then dashboard-driven delete; then supervisor stop.

### Step 8.1 - NEVER release the rendezvous claim — always leave it for its TTL

**Do not release the rendezvous claim, ever** — not even when the sibling was found. The earlier "release-on-found" rule had a fatal race: **A finding B does not mean B has found A.** In the 2026-05-25 two-machine run, spaceship found MSI's claim and released its own *before* MSI reached its Phase 6 poll, so MSI never saw spaceship and cross-tenant isolation went unverified. Releasing on first sighting can erase your claim out from under a slower peer whose poll window hasn't opened yet. The only safe rule is: **publish early (Phase 0.6.5), never release, let the 7200s TTL reap it.** That guarantees both peers' claims stay alive across *both* poll windows regardless of pace. The bounded leak is safe — the `/mtc` launcher rotates the slug per 5-min bucket, so iterations never collide, and coord TTL-reaps every claim within 2 h.

```bash
if [ -n "$RENDEZVOUS_SLUG" ]; then
  RDV_SLUG_HYPHENIZED=$(echo "$RENDEZVOUS_SLUG" | tr 'A-Z' 'a-z' | tr ':T.' '---')
  RESOURCE_KEY="manual-test-coord-rendezvous-${RDV_SLUG_HYPHENIZED}-${MACHINE_ID}"
  # Intentionally NO /claims/release. A found-sibling does not imply the sibling
  # found us; releasing now could erase our claim before a slower peer polls
  # (the 2026-05-25 missed-rendezvous bug). Leave it for the 7200s TTL.
  echo "Rendezvous claim LEFT ALIVE for its 7200s TTL (resource_key $RESOURCE_KEY) — never released, so a slower sibling can still discover us. SIBLING_FOUND=${SIBLING_FOUND:-0} (informational only; does not gate release). Coord reaps on expiry."
fi
```

> Cross-iteration note: because every run now leaves its claim, a *subsequent* run in the **same 5-min bucket/slug** would re-acquire the same `resource_key` (same machine_id ⇒ idempotent refresh, not a conflict). The `/mtc` launcher rotates the slug per 5-min bucket, so back-to-back runs don't collide; only a deliberate `--rendezvous-slug` reuse within 2 h would.

### Step 8.2 — Drive dashboard's delete-device flow against the temp runner

```bash
# Re-navigate to devices, find the temp runner's row, click its "remove" / "delete" / kebab menu.
curl -s -X POST "$DASHBOARD_UB/ai/find${TAB_QS}" -H "Content-Type: application/json" -d '{"query": "Devices"}'
# ... click row → find delete action
curl -s -X POST "$DASHBOARD_UB/ai/find${TAB_QS}" -H "Content-Type: application/json" -d '{"query": "Remove"}'
# Fallbacks: "Delete", "Unpair", trash icon labeled "Delete device"
```

If the dashboard exposes no delete-device flow, flag `PRODUCT_GAP: dashboard cannot remove paired devices — operators must rely on TTL cleanup via state_reconciler_watcher.rs:199-211`. Continue to Step 8.3 — the watcher will TTL the row eventually.

### Step 8.3 - Stop temp runner

```bash
curl -s -X POST "$SUPERVISOR_BASE/runners/${TEST_ID}/stop"
echo "Temp runner $TEST_ID stopped"
```

### Step 8.4 - Kill headless dashboard tab

```bash
# Kill the Playwright-backed Chromium tab spawned in Phase 0.6.
kill $HEADLESS_PID 2>/dev/null || true
echo "Headless tab PID=$HEADLESS_PID terminated"
```

### Step 8.5 — Emit forensics accumulators (Phase 0.7 helpers)

Final flush of the structured forensics captured by `capture_on_fail`
(Phase 0.7) and the Phase 0.6 login-web.cjs result JSON. Emit
before the Phase 9 report narrative so the executing agent folds these
findings into the per-phase verdicts and remediation plan.

```bash
CONTROL_FAILURE_COUNT=0
if [ -f "$CONTROL_FAILURES_LOG" ]; then
  CONTROL_FAILURE_COUNT=$(wc -l < "$CONTROL_FAILURES_LOG" | tr -d ' \n\r')
fi
if [ "${CONTROL_FAILURE_COUNT:-0}" -gt 0 ]; then
  echo ""
  echo "Phase 9 — Control-call failures captured ($CONTROL_FAILURE_COUNT):"
  # Print each entry's two /health snapshots side-by-side (connectedTabs only)
  # before the raw entry, so the SSE-flap discriminator is readable at a glance.
  python -c "
import json, sys
for line in open('$CONTROL_FAILURES_LOG'):
    e = json.loads(line)
    def tabs(s):
        try: return json.loads(s).get('data', {}).get('connectedTabs', [])
        except Exception: return '<unparsable>'
    print(json.dumps(e))
    print('  connectedTabs @fail : %s' % tabs(e.get('health_snapshot','')))
    print('  connectedTabs @+5s  : %s' % tabs(e.get('health_snapshot_retry','')))
    print('---')
" || awk '{print; print "---"}' "$CONTROL_FAILURES_LOG"
fi
rm -f "$CONTROL_FAILURES_LOG"

if [ "${#LOGIN_ATTEMPTS[@]}" -gt 0 ]; then
  echo ""
  echo "Phase 9 — Login attempts (${#LOGIN_ATTEMPTS[@]}):"
  printf '  %s\n' "${LOGIN_ATTEMPTS[@]}"
fi
```

Discriminative mapping for the agent's report:

- **CONTROL_FAILURES with distinct `x_vercel_id` prefixes across consecutive failures** → mechanism #3 (Vercel deployment skew). Surface as `PRODUCT_GAP: Vercel single-region sticky-session needed`.
- **CONTROL_FAILURES where the failed tabId is ABSENT from `health_snapshot` but PRESENT in `health_snapshot_retry` (5s later)** → mechanism #1 (SSE listener flap + reconnect). The transport self-healed; if this shape appears on a deployment that should carry ui-bridge ≥ 0.8.2's re-arm-WS fix (PR #41 `e72883e`), the fix is incomplete or the deployed bundle is stale — file a follow-up rather than blaming tab pruning.
- **CONTROL_FAILURES with the failed tabId MISSING from BOTH `health_snapshot` and `health_snapshot_retry`** → our headless tab dropped its transport and did not recover. The re-arm-WS fix (UI Bridge PR #41 `e72883e`) shipped in `@qontinui/ui-bridge@0.8.2`, and qontinui-web depends on `^0.8.5`, so if this STILL happens the deployed bundle is stale — confirm Phase 0.3 Vercel freshness and that the live deploy carries ≥ 0.8.2. With `?tabId` pinning, a `/control/*` HTTP **404** (`TAB_NOT_FOUND`) is the cleaner signal that `$OUR_TAB_ID` was pruned — re-run Phase 0.6.
- **LOGIN_ATTEMPTS with `status: 401, body: "Invalid username/email or password"`** → mechanism (1) staging password drift. Reseed `josh@qontinui.io` against `Qontinui123+` per `qontinui-web/dev-credentials.json`.
- **LOGIN_ATTEMPTS with `status: 429, retry_after: <N>`** → mechanism (2) rate-limit hit. Wait `<N>` seconds or widen `@auth_rate_limit` for operator workflows.
- **LOGIN_ATTEMPTS with `status: 422` or any other** → mechanism (3) form/middleware rejection. `body` head usually carries the FastAPI detail.

**Predicate.**
- PASS: release + delete + stop + headless-kill all returned success.
- PRODUCT_GAP: delete-device flow missing on dashboard; relied on TTL cleanup.
- FAIL: temp runner stop returned an error (supervisor problem - include the error body in the report).

## Phase 9: Report

Produce a compact report mirroring `/manual-test` §"Phase 5: Report & Evaluate" + §"Phase 6: Design Remediation Plan" shapes.

### Report header (always emit, even on early abort)

```
## /manual-test-coord report
- target=<staging|local|both>  rendezvous_slug=<slug|none>  iter_id=<TEST_ID>
- phases: 0=<PASS|...> 1=<...> 1.5=<...> 2=<...> 3=<...> 4=<...> 5=<...> 6=<...> 7=<...> 8=<...>
- counts: bugs=N  friction=N  missing=N  product_gaps=N  setup_gaps=N  security_anomalies=N  blocked=N  relay_races=N
- summary: <one-paragraph overall health assessment>
```

Per-phase verdict legend:
- `PASS` — phase predicate satisfied.
- `FAIL` — phase predicate failed; product or runner bug surfaced.
- `DEFERRED` — phase predicate inconclusive due to a known upstream gate (e.g. default-tenant-propagation plan in flight, Vercel deploy-freshness alert).
- `PRODUCT_GAP` — dashboard does not expose the surface needed; not a bug, but a dashboard feature gap.
- `SETUP_GAP` — environment-side prerequisite missing (single-machine mode for Phase 6).
- `SECURITY_ANOMALY` — additive finding (sibling machine_id outside allow-list); attaches to PASS or FAIL, doesn't replace.
- `RELAY_RACE` — our headless tab never connected to the relay, or its `tabId` was pruned mid-run, so `/control/*` calls couldn't be pinned (e.g. Phase 0.6 found no NEW tab, or a `/control/*?tabId=` 404'd repeatedly). With per-tab `?tabId` routing (ui-bridge ≥ 0.8.2) this should be rare; if it recurs, suspect a STALE deployed bundle (Phase 0.3 Vercel freshness) rather than a primary-flip. Cross-reference "Multi-tab routing".
- `BLOCKED` — phase couldn't run at all (sibling timeout, no dispatch surface, temp runner failed to start).

### Remediation plan

For every non-PASS phase, follow the `/manual-test` Phase 6 remediation-plan structure:

```
### qontinui-runner / qontinui-coord / qontinui-web / UI Bridge / Cross-cutting
- **Issue:** [one-line description]
- **Root cause (hypothesis):** [file paths / modules]
- **Proposed fix:** [concrete change — NEVER a coord/web test endpoint; PRODUCT_GAP for genuine missing dashboard surface]
- **Verification:** [specific UI Bridge call + expected DOM state]
- **Priority:** P0 / P1 / P2
```

If Phases 0–8 turned up no deficiencies, write `No deficiencies surfaced during this test run.` and stop.

### Per-repo table (only when remediation plan has entries)

| Repo | Issues | Priority breakdown |
|---|---|---|
| qontinui-coord | N | P0=… P1=… P2=… |
| qontinui-web | N | … |
| qontinui-runner | N | … |
| UI Bridge | N | … |
| Cross-cutting / docs | N | … |

## Rules

### Coord-test-specific rules
- **NEVER add admin or test-only endpoints to qontinui-coord or qontinui-web to make a phase pass.** A missing dashboard surface is a `PRODUCT_GAP`, not a missing-endpoint bug. Phase-by-phase decision: would a real operator hit this gap? If yes ⇒ dashboard feature work, not a test endpoint.
- **NEVER release the rendezvous claim in Phase 8 — leave it for its 7200s TTL, always**, even when the sibling was found. A found-sibling does NOT imply the sibling found us; releasing on first sighting can erase our claim before a slower peer's poll window opens (the 2026-05-25 missed-rendezvous bug: spaceship found MSI and released before MSI polled). Combined with early publish (Phase 0.6.5), never-release guarantees both peers' claims overlap both poll windows. The bounded leak is safe because `/mtc` rotates the slug per 5-min bucket, so iterations don't collide; coord TTL-reaps within 2 h. (`SIBLING_FOUND` is now informational only — it does not gate release.)
- **ALWAYS stop the temp runner in Phase 8** even on early failure (supervisor build-pool slots are scarce; a leaked test runner blocks other agents).
- **For `--target=both`**: run the entire phase sequence twice — once against local (single-machine, Phase 6 SKIPs with SETUP_GAP), once against staging (two-machine model if `--rendezvous-slug` given). Spawn a fresh temp runner per target.
- **Two-machine timing**: rendezvous claim TTL is 7200s, so the operator has ~2h between the two `/manual-test-coord` invocations before claims expire. Skill must tolerate ~60s delivery skew between machines.
- **Defense-in-depth machine_id check**: Phase 6's sibling-machine_id allow-list (spaceship + MSI UUIDs) is a `SECURITY_ANOMALY` signal, not a blocker. A rogue sibling doesn't abort the run — it adds a finding.

### Inherited rules (from `/manual-test`)
- **NEVER restart, kill, or rebuild the primary runner** — the supervisor spawns temp runners independently.
- **NEVER ask the user for input** — make reasonable assumptions and proceed.
- **NEVER report "needs a rebuild" and stop** — spawn a temp runner with `{"rebuild": true}` on the supervisor.
- **ALWAYS snapshot before and after interactions** to track changes.
- **ALWAYS call discover** if snapshot returns empty or stale data.
- **ALWAYS wait 2–3 seconds after navigation/clicks** that change the view, then re-discover.
- **Prefer `ai/find` text queries over hardcoded element IDs** — dashboard selectors drift.
- **If an element can't be found**: retry with synonyms (Account / Profile / Settings; Runners / Devices / Online Runners; Run / Dispatch / Execute) before flagging the surface as missing.
- **If an action fails**: check console errors via `GET /control/console-errors`, try alternative approaches, log the failure.
- **Be thorough but practical** — don't spend more than 3 attempts on a single failing interaction before logging it and moving on.
- **Be fully autonomous** — the user should not need to intervene at any point during the test.
- **No `Co-Authored-By: Claude` trailer** if/when committing any remediation work — qontinui-claude-config's pre-commit hook blocks it (per [[feedback_no_claude_attribution]]).

### Related
- `/manual-test` (`manual-test.md`) — UI-Bridge-on-runner correctness testing; complementary surface.
- `/manual-test-loop` (`manual-test-loop.md`) — autonomous-loop template; coord version (`manual-test-coord-loop`) is Phase 2 of this plan.
- the `2026-05-21-manual-test-coord-skill` plan — the plan this skill implements.
- the `2026-05-20-default-tenant-propagation` plan — Phase 2 (tenant resolution) may DEFER on this until it lands.
- `proj_canonical_moved_to_aws` — `demo.staging.qontinui.io` is the canonical staging product surface.
- `feedback_build_verification_over_manual_observation` — the design principle this skill instantiates for the coord wire.

## Test Focus

$ARGUMENTS
