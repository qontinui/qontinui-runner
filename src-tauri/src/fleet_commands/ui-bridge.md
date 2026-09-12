# UI Bridge — Inspect & Interact with Frontend UI

Use the UI Bridge SDK to inspect page state, interact with elements, and verify UI behavior. This is a direct inspection tool — use `/ufix` instead if you need to fix a bug.

## Target Applications

Three applications have the UI Bridge SDK installed:

| Application | Base URL | Description |
|-------------|----------|-------------|
| **Web frontend** (Next.js) | `https://qontinui.io/api/ui-bridge` | qontinui-web frontend, direct HTTP |
| **Runner UI** (Tauri) | `http://localhost:9876/ui-bridge` | Runner's React frontend, proxied via Tauri IPC |
| **Mobile app** (React Native) | `http://localhost:8087/ui-bridge` | qontinui-mobile, via `ui-bridge-native` SDK (port 8087) |

All expose the same endpoints — the only difference is the base URL and access method.

**Determine target from user input.** Default to **web frontend** if ambiguous.

```bash
# Set BASE based on target:
# Web frontend:
BASE="https://qontinui.io/api/ui-bridge"

# Runner UI:
BASE="http://localhost:9876/ui-bridge"

# Mobile app (requires adb port forwarding for emulator):
#   adb forward tcp:8087 tcp:8087
BASE="http://localhost:8087/ui-bridge"
```

### Mobile App Notes

The mobile app uses `ui-bridge-native` (`UIBridgeNativeProvider`) which runs an HTTP server on port 8087 inside the device/emulator.

- **Emulator**: Run `adb forward tcp:8087 tcp:8087` to forward the port to localhost
- **Physical device**: Use the device's IP address instead of localhost
- **Fallback**: If the native HTTP server is unavailable, use screenshot-based verification instead:
  ```bash
  python <workspace-root>/qontinui-claude-config/scripts/mobile-feedback.py capture
  # Then read: <workspace-root>/.dev-logs/mobile/screenshots/latest.png
  ```

## SDK vs Control Endpoints

UI Bridge exposes two endpoint families. Choose the right one based on what you're inspecting:

| Aspect | Control (`/ui-bridge/control/*`) | SDK (`/ui-bridge/sdk/*`) |
|--------|----------------------------------|--------------------------|
| **Inspects** | The runner's own Tauri/React UI | An external app connected via the UI Bridge SDK |
| **Use when** | Debugging runner UI bugs, verifying runner state | Debugging web/mobile frontend bugs in the connected app |
| **Availability** | Always available when the runner is running | Only available when an SDK app is connected |
| **Connection** | Built-in, no setup needed | Requires `POST /ui-bridge/sdk/connect` with the app URL |
| **Check status** | N/A | `GET /ui-bridge/sdk/status` |

**Decision rule:** If you are fixing a bug in the runner UI itself, use Control endpoints. If you are fixing a bug in qontinui-web, qontinui-mobile, or any other SDK-integrated app, use SDK endpoints.

## Response Formats

All UI Bridge endpoints return responses wrapped in an `APIResponse` envelope:

```json
{
  "success": true,
  "data": {},
  "timestamp": 1710000000000,
  "_meta": {}
}
```

### The _meta Field

The `_meta` object provides context about data freshness and relay behavior:

| Field | Type | Meaning |
|-------|------|---------|
| `stale` | `boolean` | `true` if the returned data is from cache and may not reflect current UI state |
| `staleSinceMs` | `number` | Milliseconds since the last successful live refresh (present when stale is true and a timeout/error occurred) |
| `fallback` | `boolean` | `true` if the response is a fallback due to relay failure |
| `reason` | `string` | Human-readable explanation of why a fallback was returned |

### Example Responses

**Success (live data):**
```json
{"success": true, "data": {"elements": []}, "timestamp": 1710000000000, "_meta": {"stale": false}}
```

**Stale (no SSE listeners, cached data returned):**
```json
{"success": true, "data": {"elements": []}, "timestamp": 1710000000000, "_meta": {"stale": true}}
```

**Stale with timeout:**
```json
{"success": true, "data": {"elements": []}, "timestamp": 1710000000000, "_meta": {"stale": true, "staleSinceMs": 15000}}
```

**Fallback (relay error):**
```json
{"success": true, "data": {"elements": []}, "timestamp": 1710000000000, "_meta": {"fallback": true, "reason": "Relay connection lost"}}
```

**Error:**
```json
{"success": false, "error": "Element not found", "timestamp": 1710000000000}
```

## When the SDK Is Not Connected

If Control endpoints return `"Frontend did not become ready"` errors, the SDK hasn't loaded in the webview. This typically means the webview itself has a problem (connection refused, blank screen, crash).

**The `/control/snapshot` endpoint automatically falls back to a native window capture** when the SDK is not connected. Check for `"source": "native_capture"` in the response — it will include a `screenshot` field (base64 PNG) showing what the webview is actually displaying, plus a `reason` field explaining why the SDK path failed.

You can also use these endpoints directly — they work without the SDK:

```bash
# Native window capture — works even when SDK/React is completely dead (Runner only)
curl -s "http://localhost:9876/ui-bridge/control/annotated-screenshot?runner=true"
# Returns: {"success":true,"data":{"screenshot":"<base64 PNG>","width":...,"height":...}}

# Health endpoint includes diagnosticScreenshot when ready:false for >30s
curl -s http://localhost:9876/ui-bridge/health
# Check data.diagnosticScreenshot.screenshot if present
```

Decode the base64 PNG to visually inspect the error. This is essential for diagnosing webview-level issues (ERR_CONNECTION_REFUSED, blank pages) that the SDK can never report.

## Quick primitive selector

Before reaching for `sleep N && re-discover` or `page/evaluate + __TAURI_INTERNALS__`, check whether one of these existing primitives fits. Full details in the `ui_bridge_core.md` builtin context under "Choosing a wait / nav / batch primitive".

- **Wait for an element** — `POST /control/wait-for-element-state` (registered id) or `POST /ai/wait-for-element-condition` (structured selector, no id yet).
- **Wait for a route** — `POST /control/wait-for-route` with a glob pattern (`**` supported).
- **Switch tabs** — `POST /control/activate-tab/{id}` (fire-and-forget), `POST /control/page/set-tab` (verification signal), or `stateMachine.navigateTo("page-<slug>")` via `page/evaluate` (state-machine side effects). Avoid `element/<id>/action` on sidebar buttons.
- **Batch a sequence** — `POST /control/batch-execute` for mixed action/wait/snapshot, `POST /control/batch` / `/control/batch-actions` for pure action batches.
- **Call a Tauri command** — `GET /ui-bridge/commands` to list, `POST /ui-bridge/invoke/{cmd}` to call. No `page/evaluate` boilerplate.
- **Recent console errors** — `GET /control/console-errors?since=<epoch-ms-or-ISO>&group=true` (both numeric and ISO-8601 `since` are accepted).
- **Did that click do anything?** — Read `expectChange` in the click/doubleClick response instead of re-snapshotting.

## API Quick Reference

### Inspection

```bash
# Full UI snapshot — elements, state, values (ALWAYS start here)
# Auto-falls back to native window screenshot if SDK is not connected
curl -s $BASE/control/snapshot

# Force element discovery (call if snapshot returns stale/empty data)
curl -s -X POST $BASE/control/discover -H "Content-Type: application/json" -d '{"interactive_only": false}'

# List all registered elements
curl -s $BASE/control/elements

# Get specific element details
curl -s $BASE/control/element/<element-id>
```

### Interaction

All 17 SDK standard actions are supported. Common examples:

```bash
# Click / Double-click / Right-click
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "click"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "doubleClick"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "rightClick"}'

# Type text (with optional clear-first and keystroke delay)
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "type", "params": {"text": "value", "clear": true}}'

# Clear / Focus / Blur / Hover
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "clear"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "focus"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "blur"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "hover"}'

# Select dropdown option
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "select", "params": {"value": "option1"}}'

# Scroll
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "scroll", "params": {"direction": "down", "amount": 300}}'

# Check / Uncheck / Toggle
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "check"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "uncheck"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "toggle"}'

# Drag element to target
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "drag", "params": {"target": {"elementId": "target-id"}, "steps": 20}}'

# Submit / Reset form
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "submit"}'
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "reset"}'

# Send keyboard events (works with xterm.js terminals, canvas elements, etc.)
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "sendKeys", "params": {"keys": [{"key": "a"}, {"key": "Enter"}, {"key": "v", "modifiers": {"ctrl": true}}]}}'
```

### JavaScript Evaluation

```bash
# Execute JavaScript in the webview (Runner only)
curl -s -X POST $BASE/control/page/evaluate -H "Content-Type: application/json" -d '{"expression": "document.title"}'
```

This is a powerful escape hatch for interacting with elements that the standard action system can't reach (e.g., canvas-based terminals, WebGL content). The expression is evaluated with `eval()` and the result is returned. Promises are automatically awaited.

### Terminal HTTP API (Runner only)

The Runner exposes a terminal management API at `http://localhost:9876` (separate from the UI Bridge endpoints).

```bash
# List terminal sessions
curl -s http://localhost:9876/terminals

# Create terminal
curl -s -X POST http://localhost:9876/terminals -H "Content-Type: application/json" -d '{"title": "My Terminal"}'

# Write to terminal (data is base64-encoded)
curl -s -X POST http://localhost:9876/terminals/{id}/write -H "Content-Type: application/json" -d '{"data": "base64..."}'

# Read terminal buffer
curl -s http://localhost:9876/terminals/{id}/buffer

# Resize terminal
curl -s -X POST http://localhost:9876/terminals/{id}/resize -H "Content-Type: application/json" -d '{"cols": 120, "rows": 40}'

# Close terminal
curl -s -X DELETE http://localhost:9876/terminals/{id}
```

### Navigation (web frontend only)

```bash
# Navigate to a route
curl -s -X POST $BASE/control/page/navigate -H "Content-Type: application/json" -d '{"url": "/build/workflows"}'

# Refresh page
curl -s -X POST $BASE/control/page/refresh

# Simulate clicking the window's X button (runner only; calls Tauri's native
# WebviewWindow::close(), which fires a proper CloseRequested event — the
# same path an OS X-click takes). `window.close()` via /control/page/evaluate
# is a no-op for top-level webviews, so drop to this for close-handler tests.
curl -s -X POST $BASE/control/page/close-request
```

### Console Errors

```bash
# Get recent console errors (default limit: 50)
curl -s $BASE/control/console-errors

# Get errors since a timestamp
curl -s "$BASE/control/console-errors?since=1707900000000"

# Get errors with custom limit
curl -s "$BASE/control/console-errors?limit=10"
```

Note: Console errors and HMR compilation errors are both included. HMR errors (TypeScript, module resolution, syntax errors from Next.js dev mode) are captured by intercepting EventSource connections to webpack/turbopack HMR endpoints.

### Browser Events Log

In development, the SDK captures browser events beyond just console errors — network failures, navigation, long tasks, resource errors, React crashes, WebSocket disconnections, and HMR compilation errors/warnings. All events are persisted to `.dev-logs/browser-events.jsonl` (JSONL, cleared on page load). Read this file for full browser-side debugging context.

### Components & Specs

```bash
# List registered components
curl -s $BASE/control/components

# Get component details
curl -s $BASE/control/component/<id>

# Execute component action
curl -s -X POST $BASE/control/component/<id>/action/<action_id> -H "Content-Type: application/json"

# List loaded specs
curl -s $BASE/control/specs
```

## Design Audit

The `runDesignAudit` command performs automated visual and accessibility checks on discovered UI elements. It uses WCAG 2.1 luminance-based contrast ratio calculations to detect readability issues.

**Endpoint:** `POST $BASE/ai/design-audit`

```bash
# Run a runDesignAudit design audit on the current page
curl -s -X POST $BASE/ai/design-audit -H "Content-Type: application/json"
```

**What runDesignAudit checks:**
- **Contrast ratios** � WCAG 2.1 luminance-based calculation: `(L1 + 0.05) / (L2 + 0.05)`
- **Select option visibility** � detects dark-background selects missing `color-scheme: dark`
- **Touch target sizes** � buttons/links smaller than 44x44px
- **Font sizes** � text smaller than 12px

**Three severity levels for contrast checks:**
| Severity | Contrast Ratio | Meaning |
|----------|---------------|---------|
| `error` | < 1.15:1 | Text is nearly invisible � foreground and background colors are almost identical |
| `warning` | < 3.0:1 | Fails WCAG AA for large text (18px+) � low contrast |
| `info` | < 4.5:1 | Fails WCAG AA for normal text (<18px) � insufficient contrast |

**Response format:**
Each finding includes an `element` ID, `issue` description, `severity`, and a `fix` field with actionable instructions for resolving the problem.

```json
{
  "issues": [
    {
      "element": "btn-submit",
      "issue": "Low contrast: 2.45:1 (rgb(150,150,150) on rgb(200,200,200))",
      "severity": "warning",
      "fix": "Increase contrast to at least 4.5:1 for WCAG AA compliance"
    }
  ],
  "checkedElements": 50,
  "timestamp": 1710000000000
}
```

## Element Data Model

Each element in the snapshot contains:

| Field | Content |
|-------|---------|
| `id` | Element identifier (used in `/element/<id>/action`) |
| `element_type` | button, input, link, select, checkbox, etc. |
| `label` | Human-readable label |
| `actions` | Available actions: click, type, focus, clear, hover |
| `state.visible` | Whether element is visible |
| `state.enabled` | Whether element is enabled/interactive |
| `state.value` | Current input value |
| `state.text_content` | Displayed text |
| `state.checked` | Checkbox/radio state |
| `state.selected_options` | Selected dropdown options |
| `state.rect` | Position and size (x, y, width, height) |

## Modes

Interpret `$ARGUMENTS` to determine what to do:

### Snapshot Mode (default)
If the user provides no arguments, asks "what's on the page", or wants to see UI state:
1. Take a snapshot
2. Summarize the page: what page/view is active, key elements visible, notable state (form values, disabled buttons, error messages, loading states)
3. Present a structured summary — don't dump raw JSON

### Injected Mode (`--injected <url>`)
If the user passes `--injected <url>` (or asks to snapshot/interact a **bare pre-auth page** — sign-in / register / forgot-password — that ships no UI Bridge code), drive it via the `ui-bridge-inject` CLI in `@qontinui/ui-bridge-wrapper`. The CLI launches Chromium, injects the engine bundle into the bare page, and exposes it for snapshot/interaction. `<workspace-root>` is the directory that contains the repo checkouts (the parent of this repo's checkout). The CLI is a **build artifact**: it exists only if `<workspace-root>/ui-bridge` is checked out AND its packages have been built (`npm run build` at the ui-bridge root); if either is missing, report injected mode as unavailable instead of improvising.

Pick a variant:

- **Variant A — quick one-shot snapshot (relay-free).** Best for "just show me what's on this bare page." Run each action with `--exec` (repeatable); the CLI runs them via the injected runtime and prints `{"action","result"}` JSON lines, then exits. No temp runner needed.

  ```bash
  node <workspace-root>/ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs \
    --url "<bare-page-url>" \
    --exec 'snapshot {}'
  # Prints {"action":"snapshot","result":{...elements...}} then exits.
  ```

- **Variant B — live drive (relay mode, default).** For multi-step interaction. Spawn a temp runner (supervisor `:9875`, as `/manual-test` Phase 0 does), then point `--relay` at the **temp runner's** `/ui-bridge` base — **NOT** the page origin (the injected bundle's `startRelayClient` POSTs there to register the tab):

  ```bash
  RELAY_BASE="http://127.0.0.1:${TEST_PORT}/ui-bridge"   # temp runner base, NOT the page origin
  node <workspace-root>/ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs \
    --url "<bare-page-url>" --relay "$RELAY_BASE" --ready-timeout 30000 &
  # Prints one stdout JSON line {"tabId":..,"uiBridgeRegistered":..,"url":..} then stays
  # alive until SIGTERM. Set BASE="$RELAY_BASE", capture tabId (or poll the runner's /tabs),
  # and drive with the normal /control/* snapshot/interact calls above (pin ?tabId=<id> if
  # multiple tabs are connected). SIGTERM the CLI when done — it does not exit on its own.
  ```

Then snapshot/interact exactly as in Snapshot/Interact Mode. The injected runtime waits for the DOM to **settle** (content painted + quiet, or a hard cap) before `ready()` returns, so on a client-rendered SPA (e.g. prod `qontinui.io/login`, a Next.js page) the first `snapshot`/`find` right after the CLI's ready line already sees the pre-auth controls — no manual poll needed. (Tune via `--settle-quiet`/`--settle-timeout`; `--no-settle` reverts to the old ready-only gate, which would need a poll.) If the target control mounts *lazily* after unrelated chrome paints, pass `--expect-selector '<css>'` so settle waits for that element specifically. If the controls still don't appear (`registration.totalRegistered: 0`, `elements: []`) or `ready()` throws `INJECTED_EXPECT_SELECTOR_UNMET` / `INJECTED_RUNTIME_NOT_SETTLED` (inject failed, slow hydration — raise `--settle-timeout`, or wrong selector), report **BLOCKED/UNVERIFIED**, not success. If you fill credentials into the page, **verify any authed result by observing the authed DOM on the page** (`snapshot`/`find`) — never infer success from a 2xx/redirect. Against a prod `--url` (`qontinui.io/login`), never complete a destructive register/signup, and confirm with the operator first.

### Authenticated Web Pages — autonomous (login-walled deployed routes)

**Use this when** you must verify a **logged-in** deployed page (e.g. `https://qontinui.io/digital-twin`, any `(app)` route) — NOT a bare pre-auth page. Two walls block the obvious paths and the symptoms look like a dead end, so reach straight for the harness below:
- A direct `curl` to `/api/v1/...` returns `401 {"error":"UNAUTHORIZED"}` (the web fastapi-users gate).
- The relay (`GET /api/ui-bridge/sdk/status`, `…/control/snapshot`) returns `401 {"code":"UNAUTHENTICATED","message":"UI Bridge relay requires a valid session token"}` — prod runs with `UI_BRIDGE_REQUIRE_AUTH=1`. **A relay 401 is NOT "SDK not connected" — it's "no bearer."** Don't conclude verification is impossible; authenticate.

**Path A — Cognito login harness (the default, fully autonomous).** The **`ui-bridge-login-web`** package bin (`@qontinui/ui-bridge-wrapper` ≥ 0.4.0; ui-bridge PR #86) drives the entire OAuth redirect chain (app `/login` → Cognito hosted UI → `/auth/callback` → authed landing) in one headless Chromium tab via the injected transport, then confirms on the authed DOM. It is a **published bin** — it resolves the engine bundle from its own module tree, so it runs from ANY directory (no more "cd to the ui-bridge repo root"; the old untracked `scripts/login-web.cjs` was never committed and is gone). Origin plan: the `2026-06-05-ui-bridge-authed-web-drive-harness` plan.

```bash
export MSYS_NO_PATHCONV=1        # REQUIRED: Git Bash mangles leading-/ SSM names → ParameterNotFound
# One-time-per-machine: `npx playwright install chromium` (shared cache, persists).
# `npx -p` pulls the wrapper PLUS its browser peers — a bare `npx <pkg>` does NOT
# install peer deps, so the bin would fail to launch Chromium without the -p list.
LOGIN_WEB="npx -y -p @qontinui/ui-bridge-wrapper -p @qontinui/ui-bridge -p @qontinui/ui-bridge-headless -p playwright ui-bridge-login-web"
EMAIL=$(aws ssm get-parameter --region eu-central-1 --name /qontinui/operator/email    --with-decryption --query Parameter.Value --output text)
export UIB_LOGIN_PASSWORD=$(aws ssm get-parameter --region eu-central-1 --name /qontinui/operator/password --with-decryption --query Parameter.Value --output text)
$LOGIN_WEB \
  --url "https://qontinui.io/login?next=%2Fdigital-twin" --email "$EMAIL" --success /digital-twin \
  [--expect-text "Delivery"] [--screenshot out.png] [--keep-open] [--headed]
# Prints ONE JSON line {ok, finalUrl, landingPath, uiBridgeRoute, errorText, ...}. Exit 0 = login confirmed.
```

Prefer a `?next=<urlencoded-path>` `--url` so the authed landing is **deterministic** and `--success` can assert the exact page (the app honors `next` instead of situationally picking `/admin/coord/fleet` vs `/dashboard`). `--success` matches the landing **pathname only** (never the query/fragment). Useful flags built into the bin (no need to fork it): `--expect-text "<comma-list>"` (assert every token is in the authed body — a real CI gate), `--screenshot <path>` (+ `--scroll-to <text>`), `--post-login-click "<css>"` (e.g. dismiss the co-pilot consent modal `[data-testid='co-pilot-consent-allow']`), `--keep-open` (park the authed session to drive from another process).

- **Credentials — pull from SSM, NOT the OS env vars (the #1 gotcha).** The `QONTINUI_TEST_*` **environment** variables are frequently **stale** — a stale env password fails login while the email "looks right" (this burned an entire session: both env cred sets were rejected by prod Cognito; the SSM password for the *same* `josh@qontinui.io` worked first try). The **authoritative** prod-operator creds live in **SSM** (`eu-central-1`): `/qontinui/operator/email` (= `josh@qontinui.io`) + `/qontinui/operator/password` (see the fetch in the block above; the harness reads `UIB_LOGIN_PASSWORD` / `UIB_LOGIN_EMAIL` / `--email`/`--password`). **`MSYS_NO_PATHCONV=1` is mandatory** for the `aws ssm` reads in Git Bash — without it the leading-`/` parameter name is path-converted to a Windows path and you get a misleading `ParameterNotFound`. **But that same `MSYS_NO_PATHCONV=1` also stops Git Bash converting a `--screenshot`/`--storage-state-out`/`--url`-adjacent *path* arg** — so a Git-Bash-style `--screenshot /d/your/workspace/_scratch/out.png` reaches Node unconverted and Windows resolves the leading `/` relative to the current drive root, silently writing to `D:\d\your\workspace\...` (the JSON `screenshotPath` still echoes the path you gave, so it *looks* fine — proven 2026-06-17). Fix: pass a **Windows-style path** for file-output flags (e.g. `--screenshot 'D:\your\workspace\_scratch\out.png'`), or scope `MSYS_NO_PATHCONV=1` to only the `aws ssm` subcommands (e.g. `MSYS_NO_PATHCONV=1 aws ssm …` per-call) so the login bin's path args still convert normally. Other SSM identities: `/qontinui/spec-ci/auth-email` (`ci-bot@qontinui.io`, what Spec CI logs in with) and `/qontinui/sso-verify/*` (a dedicated SSO-verify pool, own `verify-client-id`). Prefer `operator/*` for a verdict that needs the operator's own tenant data (e.g. plan citations); `spec-ci/*` for a generic authed crawl. Don't use `QONTINUI_TEST_LOGIN_*` (a *different*, often-hotmail identity).
- **The "looks like a login failure" but isn't trap.** The post-login landing is **`/admin/coord/fleet`** (the operator's default), NOT `/dashboard` — so a `--success`/`waitForURL` that only matches `/dashboard` reads a *successful* login as a **false timeout**. Wait for "any authed URL that is not Cognito and not the app `/login`" (use `--success /admin/coord/fleet`, or match broadly). A *real* bad-cred failure is distinguishable: the Cognito page shows `"Incorrect username or password."` (scrape it) — an **operator-resource blocker** (ask for valid creds; do **not** retry-loop → lockout). Also expect a per-session UI Bridge **consent modal** on the first authed load — dismiss/accept it (button matching `/allow|approve|ok|continue/i`) before driving, since it can overlay the page. (Proven 2026-06-16: this exact flow live-verified the Digital Twin "Delivery" card end-to-end.)
- **Multi-step verification (navigate + click + type + assert) — prefer the reusable auth artifact, don't fork the bin.** For a single page the built-in flags above usually suffice (`--url ?next=`, `--expect-text`, `--screenshot`, `--post-login-click`). For arbitrary multi-step driving, capture an auth artifact ONCE and replay it through `ui-bridge-inject` (which has the full snapshot/find/click/`--exec` surface) WITHOUT re-driving the hosted-UI login each run:
  ```bash
  # 1) Drive login once, write a reusable artifact (cookies + localStorage + multi-origin sessionStorage,
  #    incl. qontinui-web's sessionStorage bearer auth_bearer_access_token):
  $LOGIN_WEB --url "https://qontinui.io/login?next=%2Fdigital-twin" --email "$EMAIL" \
    --success /digital-twin --storage-state-out auth.json
  # 2) Drive any login-walled page with it, many times:
  INJECT="npx -y -p @qontinui/ui-bridge-wrapper -p @qontinui/ui-bridge -p @qontinui/ui-bridge-headless -p playwright ui-bridge-inject"
  $INJECT --url https://qontinui.io/digital-twin --storage-state auth.json --exec 'getControlSnapshot {}'
  ```
  Only when neither flags nor the artifact fit should you write a small custom driver against the injected transport's `ctx.page` (Playwright) — model it on `src/login-web-cli.ts`'s `drive` step.

**Path B — device-JWT relay drive (server-side, when an authed tab is registered to the relay).** `_auth.ts` accepts a **coord device-JWT** as `Authorization: Bearer <jwt>` (verified via `/api/v1/devices/me`) in addition to a Cognito operator bearer — so the relay's `/control/*` endpoints can be driven **server-side**, against a tab already registered to that device's paired operator. Attach `-H "Authorization: Bearer <bearer>"` to the relay calls. For an **operator bearer**, the Path-A `--storage-state-out` artifact above already captures it (sessionStorage `auth_bearer_access_token`) — so the bootstrap is now non-interactive via the login bin. Standalone non-interactive minting of a fresh **device-JWT** from the runner's on-disk encrypted store (outside the runner process) is **not** built; the runner's own `device_jwt_refresher` mints them in-process. See the gap/assessment in the `2026-06-17-standalone-device-jwt-mint-assessment` plan.

**Always** verify by observing the authed DOM (`snapshot`/`find`/visible text), never by a 2xx/redirect.

### Verify Mode
If the user says "verify", "check", "assert", or describes expected state:
1. Take a snapshot
2. Compare actual state against expected
3. Report pass/fail for each expectation with specifics on mismatches

### Interact Mode
If the user asks to click, type, navigate, or perform actions:
1. Take a snapshot first to understand current state
2. Execute the requested interactions (discover → find element → act)
3. Wait briefly, re-snapshot to show the result
4. Report what changed

### Explore Mode
If the user says "explore" or wants a walkthrough of available UI:
1. Discover all elements
2. Group elements by type and region
3. Identify interactive elements and their current states
4. Suggest possible interactions

## Workflow Rules

- **ALWAYS snapshot first** before any interaction, to understand the current page
- **Call discover** if snapshot returns empty or stale data
- **Re-snapshot after interactions** to show the effect of actions
- **Summarize, don't dump** — present human-readable descriptions, not raw JSON. Include element IDs in parentheses so the user can reference them.
- **After navigation or clicks that change the view**, wait 2 seconds then re-discover and re-snapshot
- **If the app is not responding**, check if the service is running. Suggest `.\dev-start.ps1 -Frontend` or `-Runner` as appropriate.
- **Report errors clearly** — if an endpoint returns an error, explain what it means and suggest a fix

## Example Output Format

When reporting a snapshot:

```
## Current Page: Task Runs

**Active view:** Task run list showing 5 recent runs

### Key Elements
- **Navigation:** Dashboard (nav-dashboard), Task Runs (nav-task-runs, active), Workflows (nav-workflows)
- **Filters:** Status dropdown (filter-status, value: "all"), Date range (filter-date)
- **Task Run List:**
  - Run #42 — "Test login flow" — Completed (run-42-status: "completed")
  - Run #41 — "Check dashboard" — Failed (run-41-status: "failed")
- **Actions:** New Run button (btn-new-run, enabled), Export button (btn-export, disabled)

### Notable State
- Export button is disabled (no runs selected)
- Run #41 shows error indicator
```

## User Input

$ARGUMENTS
