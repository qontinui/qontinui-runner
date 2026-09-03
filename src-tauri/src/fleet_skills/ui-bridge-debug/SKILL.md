---
name: ui-bridge-debug
description: Debug and verify UI state in qontinui applications using the UI Bridge SDK. Use when inspecting, testing, or verifying frontend UI in the runner, qontinui-web, or qontinui-mobile. Never assert on these apps through Playwright — it is allowed only as the headless browser the UI Bridge is driven through.
user-invocable: false
---

# UI Bridge Debugging

When debugging or verifying UI in qontinui applications, **always use the UI Bridge SDK**. **Never assert on the runner, qontinui-web, or qontinui-mobile through Playwright** — it is allowed only as the headless browser HOST the Bridge is driven through (see below).

## Why Not Playwright?

- **Runner (Tauri):** Playwright cannot render the Tauri app. It loads the Vite dev server URL (localhost:1420) but requires Tauri IPC to function — Playwright just shows "Starting API server..." forever.
- **qontinui-web:** The UI Bridge SDK is integrated and provides element state, computed styles, component data, and programmatic interaction — all richer than Playwright screenshots.
- **qontinui-mobile:** Same principle — use the UI Bridge when the SDK is integrated.

**What is banned is Playwright as an *inspection API***: locators, bespoke DOM
assertions, an ad-hoc `page.screenshot()` stood up as evidence — anything that
makes it a second source of UI truth. If the Bridge cannot answer your question,
**fix the Bridge**; do not route around it with a locator. Playwright as the
browser **host** the Bridge is driven through is fine and already shipped —
that is exactly what the `ui-bridge-inject` and `ui-bridge-login-web` recipes
below do, and the observations still come from `/control/*`. Same reason you do
not hand-roll a driver: the wrapper already ships it. Canonical rule, with the
checkable examples:
`qontinui-claude-config/knowledge-base/qontinui-specific/ui-bridge.md` →
"The UI Bridge Is the Only Frontend-Inspection Tool".

## Endpoints

| Application | UI Bridge Base URL |
|-------------|-------------------|
| **Runner** (Tauri webview) | `http://127.0.0.1:9876/ui-bridge/control/*` |
| **qontinui-web** (Next.js) | `http://localhost:3001/api/ui-bridge/control/*` |

Note (2026-05-13, Phase 2 of the UI Bridge vision-pipeline plan): the legacy
`/ui-bridge/control/screenshot`, `/ui-bridge/control/annotated-screenshot`,
`/ui-bridge/control/element-screenshot`, `/ui-bridge/control/capture-element-images`,
`/ui-bridge/control/get-element-images`, `/ui-bridge/control/diagnose-stuck-screen`,
`/ui-bridge/sdk/screenshot`, and `/ui-bridge/ai/media/*` routes have been
deleted. All screenshot/visual capture now flows through `/ui-bridge/vision/*`
(see "Vision capture" below).

## Core Workflow

### 1. Discover elements (always call first)

```bash
curl -s -X POST $BASE/control/discover -H "Content-Type: application/json" -d '{"interactive_only": false}'
```

### 2. Get a full snapshot

```bash
curl -s $BASE/control/snapshot
```

Returns all elements with positions (`rect`), text content, visibility, computed styles (color, backgroundColor, colorScheme, fontSize, fontWeight, lineHeight, overflow, textOverflow, whiteSpace, position, zIndex, cursor, padding, margin, borderColor, borderWidth, borderRadius, plus display/visibility/opacity/pointerEvents), and enabled state.

### 3. Interact with elements

```bash
# Click
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "click"}'

# Type
curl -s -X POST $BASE/control/element/<id>/action -H "Content-Type: application/json" -d '{"action": "type", "params": {"text": "value"}}'
```

### 4. Verify layout without screenshots

Use element rects from the snapshot to check positioning and detect overlaps:

```bash
curl -s $BASE/control/snapshot | python -c "
import sys, json
data = json.load(sys.stdin)
for el in data.get('data', {}).get('elements', []):
    rect = el.get('state', {}).get('rect', {})
    tc = el.get('state', {}).get('textContent', '')[:60]
    eid = el.get('id', '')
    print(f'{eid:40s} x={rect.get(\"x\",0):>6.0f} y={rect.get(\"y\",0):>6.0f} w={rect.get(\"width\",0):>6.0f}  {tc}')
"
```

### 5. Navigate (web frontend)

```bash
curl -s -X POST $BASE/control/page/navigate -H "Content-Type: application/json" -d '{"url": "/specs"}'
```

### 6. Vision capture (when you genuinely need a pixel image)

Element rects are the right tool for almost all debugging — they're cheap,
parseable, and don't depend on a renderer. When you must have an actual
image (e.g. visual-regression evidence, sharing UI state with a human, or
feeding a vision-language model), use the `/ui-bridge/vision/*` routes:

```bash
# Capture the runner window with the default Claude-vision contract
# (JPEG, <5 MiB ceiling, claude_vision_v1 named OutputContract)
curl -s -X POST $BASE/vision/capture \
  -H "Content-Type: application/json" \
  -d '{"contract": "claude"}'

# Capture and crop to a single discovered element
curl -s -X POST $BASE/vision/capture \
  -H "Content-Type: application/json" \
  -d '{"element": "<id-from-discover>", "contract": "claude"}'

# Annotate a captured frame with overlay rectangles + labels
curl -s -X POST $BASE/vision/annotate \
  -H "Content-Type: application/json" \
  -d '{"annotations":[{"rect":{"x":10,"y":10,"width":100,"height":40},"label":"target"}]}'
```

Both endpoints return:

```jsonc
{
  "success": true,
  "data": {
    "path":     "tmp_vision_cache/<sha256>.jpeg",  // file on disk under runner CWD
    "sha256":   "<64 hex chars>",
    "width":    1920,
    "height":   1080,
    "bytes":    214567,
    "format":   "jpeg",            // "jpeg" | "webp" | "png"
    "contract": "claude_vision_v1" // "claude_vision_v1" | "png_strict" | "webp_lossy"
  }
}
```

If you need the bytes in your shell (e.g. to base64-encode or save to a
known path), follow up with the cache stream endpoint:

```bash
# Stream the raw bytes as image/jpeg|webp|png
curl -s -o screenshot.jpg "$BASE/vision/cache/<sha256>"
```

Phase 2 only captures the runner's own window — cross-runner / cross-window
capture is not supported here. `/vision/extract` (OCR) and `/vision/describe`
(VLM caption) arrive in Phase 4.

## Injected mode for pre-auth pages

The endpoints above assume the target page already embeds the UI Bridge SDK. A **bare pre-auth page** — sign-in / register / forgot-password on prod or staging — ships **zero UI Bridge code**, so it has no `/ui-bridge` of its own and none of the calls above reach it. Use **injected mode** when you need to debug or verify such a page.

**When to use it:** debugging a page that ships no UI Bridge code (a bare login/register/forgot-password form), where you still want snapshot/discover/element-action against the live DOM.

**How it works:** the `ui-bridge-inject` CLI (in `@qontinui/ui-bridge-wrapper`) launches Chromium, navigates to the bare page, injects the UI Bridge engine bundle into it, and registers that tab as a relay tab against a **local temp runner's** `/ui-bridge` relay. `<workspace-root>` is the directory that contains the repo checkouts (the parent of this repo's checkout). The CLI is a **build artifact**: it exists only if `<workspace-root>/ui-bridge` is checked out AND its packages have been built (`npm run build` at the ui-bridge root); if either is missing, report injected mode as unavailable. You then drive the page through that temp runner's `/control/*` API — the same surface as everything above. Nothing is injected into the prod artifact; no prod-relay auth is needed.

```bash
# Spawn a temp runner first (supervisor :9875) — its /ui-bridge base is the relay.
RELAY_BASE="http://127.0.0.1:${TEST_PORT}/ui-bridge"   # TEMP RUNNER base — NOT the page origin

node <workspace-root>/ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs \
  --url "<bare-page-url>" \
  --relay "$RELAY_BASE" \
  --ready-timeout 30000 &
# Prints one stdout JSON line {"tabId":..,"uiBridgeRegistered":..,"url":..} then stays alive
# until SIGTERM. Set BASE="$RELAY_BASE", capture tabId (or poll the runner's /tabs), and use
# the normal control/discover + control/snapshot + element/<id>/action calls (pin ?tabId=<id>
# when multiple tabs are connected). SIGTERM the CLI on teardown — it does not exit on its own.
#
# Relay-free one-shot alternative: replace --relay with one or more --exec '<action> <json>'
# flags; the CLI runs each via the injected runtime, prints {"action","result"} lines, exits.
```

**Critical:** `--relay` is the **temp runner's** `/ui-bridge` base, **NOT** the page origin. The injected bundle's `startRelayClient` POSTs to `--relay` to register; pointing it at the page origin means nothing registers and nothing drives.

**SPA hydration is handled by the launcher.** The injected runtime waits for the DOM to **settle** (content painted + quiet, or a hard cap) before `ready()` returns, so on a client-rendered SPA (e.g. prod `qontinui.io/login`, a Next.js page) the first `control/snapshot` or `control/discover` right after the CLI's ready line already sees the pre-auth controls (e.g. the email/password inputs and a "Sign In" button) — no manual poll needed. (Tune via `--settle-quiet`/`--settle-timeout`; `--no-settle` reverts to the old ready-only gate, which would need a poll.) If the target control mounts *lazily* after unrelated chrome paints (lazy-loaded login, SSR streaming, spinner-then-swap), pass `--expect-selector '<css>'` (e.g. `#login`, `input[type=password]`) so settle waits for that element specifically instead of firing on the chrome; if it never appears before the cap, `ready()` fails with `INJECTED_EXPECT_SELECTOR_UNMET`. If the controls still don't appear (`registration.totalRegistered: 0`, `elements: []`) or `ready()` throws `INJECTED_EXPECT_SELECTOR_UNMET`/`INJECTED_RUNTIME_NOT_SETTLED` (inject failed, slow hydration — raise `--settle-timeout`, or wrong selector), report **BLOCKED/UNVERIFIED**, not success, the same observe-the-goal rule the verify-on-page gate below applies to the authed DOM.

**Verify on the page, never by inference.** If you fill credentials and submit, the success criterion is the **authed DOM observed on the page** via `snapshot`/`find` (a known post-login landmark) — never a 2xx / redirect / log signal. Against a prod `--url` (`qontinui.io/login`), never complete a destructive register/signup, and confirm with the operator first.

### Authenticated, login-walled deployed pages (autonomous)

The inject CLI above is for **bare pre-auth** pages. To drive a **logged-in** deployed route (e.g. `https://qontinui.io/digital-twin`), don't stop at the relay's `401 {"code":"UNAUTHENTICATED","message":"…requires a valid session token"}` (prod runs `UI_BRIDGE_REQUIRE_AUTH=1` — that 401 means "no bearer", not "SDK absent"). Use the **`ui-bridge-login-web`** package bin (`@qontinui/ui-bridge-wrapper` ≥ 0.4.0 for the bin to *exist* — replaying what it captures needs a strictly higher floor on two packages, see the Version requirement below), which drives the full OAuth chain headless and lands you on the authed DOM. It runs from ANY directory (the old untracked `scripts/login-web.cjs` is gone):

> **Version requirement — two floors, and the higher one is a safety floor.** For the `--email` / `--storage-state-out` flags (and inject's `--storage-state`) to *exist*: **`@qontinui/ui-bridge-wrapper ≥ 0.5.0` + `@qontinui/ui-bridge-headless ≥ 0.2.0`** (shipped via ui-bridge #117). The old published `0.4.2` bin **lacks those flags** — it prints its help and exits, so an unknown-flag invocation silently does nothing. `npx -y` pulls latest, so this is automatic once the release is live; pin `@0.5.0` to force it. If you're stuck on an older published version, fall back to the **direct auth-seed path**: mint an operator Cognito IdToken (web client `tb0epbojige1900ipu6q80j6b`, `USER_PASSWORD_AUTH`) and seed the 4-piece contract on the browser context — cookie `access_token=<IdToken>` + `qontinui_auth=1`, sessionStorage `auth_bearer_access_token`, localStorage `is_authenticated=true` + a future `token_expiry` — then `createTransport({kind:'injected'})` → `ctx.browserContext.addCookies`/`addInitScript` → `ctx.page.goto` → drive by id. (Proven working 2026-07-03 when the published `login-web` was flag-less; install the ui-bridge packages in an ISOLATED temp dir with `--legacy-peer-deps` to dodge the monorepo-root ERESOLVE.)
>
> **The SAFETY floor is higher, and lands on both packages:** to *replay* a captured artifact safely you need **`@qontinui/ui-bridge-wrapper ≥ 0.7.0` + `@qontinui/ui-bridge-headless ≥ 0.4.0`**. A storage state can carry `__uiBridge_tabId`, and restoring it re-registers the replayed tab under the **operator's live tab id**. The two are floored separately because they cover different halves: the wrapper strips the key at capture, headless scrubs it at restore — and the restore half is what covers an artifact captured *earlier*, or by someone else. The wrapper's peer range on headless is `>=0.3.0 <1`, so it does **not** force 0.4.0. Today a fresh `npx -y` resolves headless `latest` = 0.4.0 and is fine; a version pin, a lockfile or a warm npx cache is what can hold it at 0.3.0, which replays `--storage-state` without scrubbing. Below either floor, pass `--tab-id` or strip the key by hand. Derivation: `knowledge-base/qontinui-specific/ui-bridge.md` → "Tab-identity safety on storage-state replay".

```bash
export MSYS_NO_PATHCONV=1   # REQUIRED in Git Bash, else the leading-/ SSM name → ParameterNotFound
# `npx -p` pulls the wrapper PLUS browser peers — Playwright-as-host, the sanctioned
# arm of the rule; one-time `npx playwright install chromium`.
LOGIN_WEB="npx -y -p @qontinui/ui-bridge-wrapper -p @qontinui/ui-bridge -p @qontinui/ui-bridge-headless -p playwright ui-bridge-login-web"
EMAIL=$(aws ssm get-parameter --region eu-central-1 --name /qontinui/operator/email    --with-decryption --query Parameter.Value --output text)
export UIB_LOGIN_PASSWORD=$(aws ssm get-parameter --region eu-central-1 --name /qontinui/operator/password --with-decryption --query Parameter.Value --output text)
$LOGIN_WEB --url "https://qontinui.io/login?next=%2Fdigital-twin" --success /digital-twin --email "$EMAIL" --expect-text "Delivery"
```

**Pull creds from SSM (`eu-central-1`), NOT the `QONTINUI_TEST_*` env vars** — the env vars are frequently stale (the same `josh@qontinui.io` failed via a stale env password but logged in fine with `/qontinui/operator/password`). `MSYS_NO_PATHCONV=1` is mandatory for the `aws ssm` reads in Git Bash — but it ALSO un-converts the bin's own path args, so pass `--screenshot`/`--storage-state-out` as a **Windows path** (`'D:\…\out.png'`) or it silently writes to `D:\d\…` (drive-root resolution of the leading `/`; the JSON echoes your path so it looks fine). Prefer a `?next=<urlencoded-path>` `--url` so `--success` (pathname-only match) asserts the exact page — the default landing is `/admin/coord/fleet` (not `/dashboard`), so matching only `/dashboard` makes a *successful* login look like a timeout. A real bad-cred failure shows `"Incorrect username or password."` on the Cognito page (don't retry-loop → lockout). Dismiss the per-session UI Bridge **consent modal** with `--post-login-click "[data-testid='co-pilot-consent-allow']"`. For repeat multi-step driving, capture `--storage-state-out auth.json` once and replay via `ui-bridge-inject --storage-state auth.json` (no re-login — **mind the wrapper ≥ 0.7.0 + headless ≥ 0.4.0 replay floor above**); the relay also accepts a **coord device-JWT** as `Authorization: Bearer` for an already-registered authed tab. Full detail in the `/ui-bridge` command's "Authenticated Web Pages" section + `qontinui-dev-notes/plans/2026-06-05-ui-bridge-authed-web-drive-harness.md`.

### Multi-step driving: act by id, wait for registration, script in JS (don't reinvent these)

Once you're on an authed/injected page, drive controls **by their stable `useUIElement` id**. Do NOT hand-roll a Playwright driver, a polling loop, or a credential-seed helper — the wrapper already ships all three, and reinventing them is the single most common UI-Bridge mistake.

**Drive by id, not by visible text.** `find` matches an element's registered **label / accessible name** (case-insensitive substring), NOT its visible text — so `find {"text":"Show raw data"}` misses a button whose `useUIElement` label is "Toggle raw verdict". Pages instrument controls with stable ids (e.g. qontinui-web's `digital-twin-cell-schema`, `digital-twin-show-raw`); grep `useUIElement(` in the page's source to find them, then act on the id directly. `--exec` is **repeatable**, so a flat multi-step flow needs no relay:

```bash
# Each --exec runs against the in-page runtime in order, prints one JSON result line, then exits.
ui-bridge-inject --url https://qontinui.io/digital-twin --storage-state auth.json \
  --exec 'act {"id":"digital-twin-cell-schema","request":{"action":"click"}}' \
  --exec 'waitForElementRegistered {"predicate":{"id":"digital-twin-show-raw"},"timeoutMs":12000}' \
  --exec 'act {"id":"digital-twin-show-raw","request":{"action":"click"}}' \
  --exec 'getControlSnapshot {}'
```

**Wait for late-mounting controls with `waitForElementRegistered`, never a sleep loop.** A control that appears only after an async fetch (a drawer's verdict, a lazy panel) is racy. The action polls the registry for you: `waitForElementRegistered {"predicate":{...},"requirement":"registered","timeoutMs":...}` where `predicate` is `{id?, label?, testId?, selector?}` (`label` = case-insensitive substring; `selector` also falls back to `document.querySelector` for non-SDK elements). `requirement` may be `"registered"` (default), `"visible"`, or `"has-layout"`. `waitForElement` / `waitForElementByCondition` are the richer variants.

**For logic beyond a flat `--exec` chain (branching, loops, reading values back), use the injected transport directly** — it's the same engine the CLI wraps, so you still don't touch Playwright:

```js
import { createTransport } from '@qontinui/ui-bridge-wrapper';
const t = createTransport({ kind: 'injected', options: {
  targetUrl: 'https://qontinui.io/digital-twin',
  storageStatePath: 'auth.json',          // the same captured session the CLI replays
} });
await t.ready();                            // waits for the DOM to settle (no manual poll)
const ctx = await t.buildContext();
await ctx.act('digital-twin-cell-schema', { action: 'click' });
await ctx.execute('waitForElementRegistered', { predicate: { id: 'digital-twin-show-raw' }, timeoutMs: 12000 });
await ctx.act('digital-twin-show-raw', { action: 'click' });
const snap = await ctx.snapshot();
// ctx also exposes find() / assert() / whenSettled() and the raw page / browserContext handles.
```

> **The replay floor above governs this snippet too — it is the SAME restore path, not a lighter one.** `createTransport({ kind: 'injected' })` returns an `InjectedTransport extends HeadlessTransport` (`ui-bridge/packages/ui-bridge-wrapper/src/transports/injected.ts`), and that base class hands `storageStatePath` straight to `@qontinui/ui-bridge-headless`'s `launchHeadlessTab` (`transports/headless.ts`) — the very function `ui-bridge-inject --storage-state` reaches. So the `__uiBridge_tabId` scrub here is the installed **headless** package's, exactly as for the CLI: floor `@qontinui/ui-bridge-headless` ≥ **0.4.0**, plus `@qontinui/ui-bridge-wrapper` ≥ **0.7.0** for whatever captured `auth.json`. A driver written against the transport inherits that floor with **no `--help` surface to make it visible**, which is what makes it easier to get wrong than the CLI.
>
> The escape hatch is an **option, not a flag**: `--tab-id` is only the CLI's spelling of it (`inject-cli.ts` assigns the parsed value to `options.tabId`). When you cannot move the floor, pass `tabId: '<id>'` in the same `options` object above — `InjectedTransportOptions.tabId`, applied outside the relay-base branch, so it pins the identity on every document.

**Best of all, verify a route in CI without driving it.** If the route is in qontinui-web, a Spec-CI page spec (`frontend/specs/pages/<route>/state-machine.derived.json`) asserts the same `useUIElement` ids — and can route-stub coord data + walk click transitions — on every PR. That's cheaper and more durable than an ad-hoc driver. See `frontend/specs/pages/digital-twin/` for a route that stubs its coord data and drives a cell→drawer transition by id.

## Rules

- **NEVER** assert on the runner, qontinui-web, or qontinui-mobile through
  Playwright — no locators, no bespoke DOM checks, no ad-hoc screenshot offered
  as evidence; if the Bridge cannot answer it, fix the Bridge. Playwright as the
  browser **host** the Bridge is driven through (`ui-bridge-inject`,
  `ui-bridge-login-web`) is allowed — full rule in
  `qontinui-claude-config/knowledge-base/qontinui-specific/ui-bridge.md`
- **ALWAYS** call discover before reading elements
- **PREFER** element rects to verify layout — only reach for `/vision/capture` when an actual image is required
- **ALWAYS** set the correct BASE URL for the target application
- The snapshot includes untruncated `textContent` for reading full error messages
