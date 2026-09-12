# UI Bridge Comprehensive Test

Run a structured, category-by-category test of all UI Bridge functionality across the web frontend and runner. Produces a checklist report with discoverability and effectiveness ratings.

## Target Applications

| Application | Base URL |
|-------------|----------|
| **Web frontend** | `https://qontinui.io/api/ui-bridge` |
| **Runner UI** | `http://localhost:9876/ui-bridge` |

### Injected target (bare pre-auth page) — optional

A bare pre-auth page (sign-in / register / forgot-password) ships **zero UI Bridge code**, so it has no `/api/ui-bridge` of its own. To run the suite against one, use the **injected transport**: the `ui-bridge-inject` CLI (in `@qontinui/ui-bridge-wrapper`) launches a Chromium tab, injects the engine bundle, and registers it as a relay tab against a **local temp runner's** `/ui-bridge` base — then every `/control/*` call in this suite targets that relay. `<workspace-root>` is the directory that contains the repo checkouts (the parent of this repo's checkout). The CLI is a **build artifact**: it exists only if `<workspace-root>/ui-bridge` is checked out AND its packages have been built (`npm run build` at the ui-bridge root); if either is missing, skip the injected target.

Launch (Variant B / relay mode — the temp runner is spawned exactly as `/manual-test` Phase 0 does):

```bash
# RELAY_BASE is the TEMP RUNNER's UI Bridge base — NOT the bare page's origin.
RELAY_BASE="http://127.0.0.1:${TEST_PORT}/ui-bridge"
node <workspace-root>/ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs \
  --url "<pre-auth-page-url>" \
  --relay "$RELAY_BASE" \
  --ready-timeout 30000 &
# Prints one stdout JSON line {"tabId":..,"uiBridgeRegistered":..,"url":..} then stays
# alive until SIGTERM. Capture tabId (or poll the runner's /tabs); set BASE="$RELAY_BASE"
# and (if multiple tabs) append ?tabId=<id> to /control/* calls. SIGTERM the CLI on teardown.
```

**SPA hydration is handled by the launcher.** The injected runtime waits for the DOM to **settle** (content painted + quiet, or a hard cap) before `ready()` returns, so on a client-rendered SPA (e.g. prod `qontinui.io/login`, a Next.js page) the first `/control/snapshot` or `/control/discover` right after the CLI's ready line already sees the pre-auth controls (email/username + password inputs, the "Sign In" submit) — no manual poll needed. (Tune via `--settle-quiet`/`--settle-timeout`; `--no-settle` reverts to the old ready-only gate, which would need a poll.) If the target control mounts *lazily* after unrelated chrome paints (lazy-loaded login, SSR streaming), pass `--expect-selector '<css>'` so settle waits for that element specifically rather than firing on the chrome. If the controls still don't appear (`registration.totalRegistered: 0`, `elements: []`) or `ready()` throws `INJECTED_EXPECT_SELECTOR_UNMET` / `INJECTED_RUNTIME_NOT_SETTLED`, the inject failed, the page hydrates slower than the cap (raise `--settle-timeout`), or the selector is wrong — treat the run as **BLOCKED/UNVERIFIED**, not a pass, the same observe-the-goal rule the verification binding (Instructions) applies to authed DOM.

**Injected mode skips runner-only categories.** A bare page has no runner internals, so do NOT run — mark SKIP with the reason "injected bare page, N/A":

- **Cat 14 (JS Evaluation):** `page/evaluate` against a bare page works for DOM queries, but the runner-specific eval semantics aren't under test — run only the simple-expression sanity check, skip the rest.
- **Terminal HTTP API / `/terminals`, Cat 18 render-log, Cat 34 state-machine `/explore`, Cat 35 intents:** runner-only surfaces a bare page does not expose.
- Any category asserting runner navigation, components, or specs registration — a bare login page registers none.

**Auth is NOT auto-performed in injected mode.** Unlike a temp runner (which auto-logs-in), the injected tab lands on the bare page unauthenticated. The operator/skill fills credentials (from SSM `/qontinui/operator/*`) via the same `type`-into-field + `click`-submit pattern `/manual-test --transport=injected` uses. Any category whose PASS depends on the **authed** DOM is verified by observing that authed DOM **on the page** post-submit — never a 2xx/redirect/log signal (see the verification binding in Instructions). Against a prod `--url`, never complete a destructive register/signup, and gate behind explicit confirmation.

## Instructions

> **Verification is observation, not inference.** When a test asserts a user-visible outcome, confirm it by observing the rendered page through the UI Bridge (`discover`/`snapshot` the relevant page) — never by reading a backend/API/DB/registration/log signal that *implies* it. Those confirm plumbing, not the outcome, and routinely disagree with what the page shows. If the surface can't be reached, the result is UNVERIFIED/BLOCKED, not PASS.

> **Reading rendered text:** `snapshot` omits non-interactive spans (e.g. the Terminal StatusStrip's count pills) — read those via `POST /ui-bridge/control/page/read-value {"selector":"[data-page-element=…]"}` (DOM ground truth). Avoid OCR (`vision/extract`) for assertions: its cache only invalidates on control actions, so UI-only re-renders serve stale text unless you pass `{"force":true}`. For Terminal session-bucket states specifically, seed them deterministically with the debug seam (`POST /ui-bridge/test/seed-terminal-scenario`, teardown `/ui-bridge/test/clear-injected` — runner #420; contract in `qontinui-runner/src-tauri/src/mcp/test_fixtures.rs` module docs).

Run every test category below against **both** the web frontend and the runner. For each category, produce a checklist result using the format described in the Reporting section.

**You MUST complete Phase 0 (App Health & Recovery) before running any test categories.** The goal is to ensure both apps are in a testable state. Testing an unresponsive app doesn't test the UI Bridge — it just produces a wall of failures.

For each category, set `BASE` to the appropriate URL and run the tests. Capture the raw JSON output where needed for verification, but summarize results — don't dump raw JSON in the report.

### Test Navigation Strategy

**CRITICAL: Navigate to the right page BEFORE testing each category.** Different categories need different UI elements (inputs, toggles, scrollable lists, specs). Testing whatever page happens to be showing will produce false negatives — the SDK works fine but the page has no relevant elements.

Each category specifies a **Page Setup** step. Always execute the navigation FIRST, wait 2 seconds for the page to settle, then take a snapshot to confirm the expected elements are present before running the category's tests.

**Key pages for testing:**

| App | Page | Route | Rich in |
|-----|------|-------|---------|
| Web | Workflows (Execute) | `/` or `/workflows` | Buttons, tabs, search input, workflow cards |
| Web | Active Dashboard | `/runs/active` | Tabs, status badges, metric values |
| Web | Workflow Builder | `/automation-builder/workflows` | Forms, inputs, toggles (Advanced Options) |
| Runner | Execute | `/` | Buttons, tabs, inputs, textareas, selects |
| Runner | Workflows (Build) | `/workflows` | Workflow list, search, scrollable content |
| Runner | Active | `/active` | Monitoring elements, tabs |
| Web | State Machine | `/automation-builder/states` | Graph nodes, edges, state panels |
| Web | Settings | `/settings` | Forms, inputs, toggles, selects |
| Runner | Settings | `/settings` | Configuration forms, toggles |

**For categories needing form controls (Cat 3, 4, 5, 7, 23):**
- **Web:** Navigate to a workflow builder page or settings page that has text inputs, checkboxes, toggles
- **Runner:** The Execute page (`/`) has inputs, textareas, and selects. The Workflows page has a search input and workflow cards.

**For scrolling (Cat 6):** Navigate to a page with a long scrollable list (e.g., Workflows page with multiple workflow cards).

**For specs (Cat 12):** Specs are registered and available — call `GET /control/specs` and expect non-empty results.

**For state machine (Cat 34):** Navigate to `/automation-builder/states` on the web frontend. The runner's state machine page may differ — check available routes.

**For media-rich pages (Cat 36):** Navigate to any page with images, icons, or media elements (workflow cards often have icons).

**For cross-app comparison (Cat 38):** Both web and runner must be running. Tests compare snapshots between the two apps.

---

## Phase 0: App Health & Recovery

**Purpose:** Ensure both apps are running, responsive, and free of blocking errors before testing. If an app has errors, use the UI Bridge itself to diagnose and fix them — this process counts as part of the test evaluation.

### Step 1: Check availability

```bash
curl -s -o /dev/null -w "%{http_code}" https://qontinui.io/api/ui-bridge/control/snapshot
curl -s -o /dev/null -w "%{http_code}" http://localhost:9876/ui-bridge/control/snapshot
```

If either returns non-200, the app is completely down. Note it, skip that app entirely, and recommend the user start it.

### Step 2: Check responsiveness (not just availability)

An app can return 200 on `/control/snapshot` but still be non-functional (e.g., empty elements, no browser connection, SSE relay broken). For each app that returned 200:

1. **Take a snapshot** — `GET $BASE/control/snapshot` — check if `elements` array is non-empty
2. **Check health** — `GET $BASE/health` (web) or equivalent — check for `responsive: true`, connected tabs/clients
3. **Check console errors** — `GET $BASE/control/console-errors` — look for JavaScript errors that could be blocking the app

If the snapshot returns **0 elements** or the health check shows **no connected browser/responsive: false**, the app is unresponsive. Proceed to Step 3.

If the snapshot returns elements and the app is responsive, skip to Step 4.

### Step 3: Diagnose and fix (use UI Bridge as the diagnostic tool)

This is where the UI Bridge tests itself — use its own endpoints to find and fix the problem. **Any UI Bridge endpoints used during diagnosis count toward the test evaluation** (e.g., if console-errors correctly identifies a JS error, that's a positive data point for Category 11).

**Diagnosis checklist:**

1. **Console errors** — `GET $BASE/control/console-errors` — look for TypeError, import errors, missing module errors
2. **Dev logs** — Read `.dev-logs/frontend.err.log`, `.dev-logs/frontend.log`, and the newest `qontinui-runner.log.*` (the runner's daily-rolled tracing sink) for startup errors. Glob for the sink in **both** `.dev-logs/` and `<LOCALAPPDATA>/qontinui-runner/dev-logs/` — the runner usually writes to the latter; `GET http://localhost:9876/log-sources/runner-log-sink` returns the exact dir
3. **Health endpoint** — `GET $BASE/health` — check SSE/WebSocket connection status, tab count, responsive flag
4. **AI snapshot** — `GET $BASE/ai/snapshot` (if available) — may provide semantic page state even when elements are empty

**Common issues and fixes:**

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| 0 elements, `responsive: false` | Browser SSE relay not connected or JS error blocking React render | Check console-errors, fix the JS error, restart frontend |
| 0 elements, page on `/login` | Auth wall blocking access to instrumented pages | This is expected for unauthenticated sessions — note it but don't treat as a bug |
| Snapshot has elements but all actions timeout | SSE relay connects but command handler not responding | Check if CommandRelayListener component is mounted, check for JS errors |
| TypeError in console-errors | Missing import, bad module resolution | Read the error, find the source file, fix the import, restart the service |

**Fix workflow:**

1. Identify the root error from console-errors or dev logs
2. Read the relevant source file to understand the issue
3. Make the code fix
4. Restart the affected service: `.\dev-start.ps1 -Frontend` (or `-Backend`, `-All`, or supervisor restart for runner)
5. Wait 10-15 seconds for the service to fully start
6. Re-check: take a new snapshot, verify elements are now present
7. If still broken, iterate (max 3 attempts per app)

**If the app cannot be fixed after 3 attempts**, document what was tried and tested, note which UI Bridge endpoints were exercised during diagnosis, and proceed with testing the other app. The diagnostic process itself provides valuable test data.

### Step 4: Record Phase 0 results

For each app, record:
- Initial state (available, responsive, element count)
- Any errors found and how they were discovered (which UI Bridge endpoints helped)
- Any fixes applied
- Final state after recovery
- Whether the app is ready for functional testing

**Include Phase 0 as a category in the final report** — it tests the UI Bridge's self-diagnostic capabilities (health, console-errors, AI snapshot).

---

## Test Categories

### Category 1: Element Discovery & Snapshot

**What it tests:** The ability to discover all interactive and content elements on a page and capture a full UI snapshot.

**Page Setup:**
- **Web:** Navigate to `/` (Execute page) — has buttons, tabs, search input, workflow cards
- **Runner:** Stay on default page or navigate to `/` — has buttons, inputs, selects, textareas

**How to test:**

1. **Snapshot capture** — `GET $BASE/control/snapshot` — verify it returns elements with id, type, label, state, actions
2. **Discover interactive** — `POST $BASE/control/discover` with `{"interactive_only": true}` — verify only interactive elements returned
3. **Discover all** — `POST $BASE/control/discover` with `{"interactive_only": false}` — verify content elements also included. Count MUST be greater than interactive-only count.
4. **Element listing** — `GET $BASE/control/elements` — verify returns same elements as snapshot
5. **Element detail** — `GET $BASE/control/element/<id>` for a specific element from the snapshot — verify full detail returned
6. **Element state** — `GET $BASE/control/element/<id>/state` — verify state properties (visible, enabled, focused, value, rect, computedStyles, inViewport)

---

### Category 2: Click Actions

**What it tests:** click, doubleClick, rightClick, middleClick actions on buttons, links, tabs, and other clickable elements.

**How to test:**

1. **Button click** — Find a button element, execute `{"action": "click"}`, re-snapshot to verify state change
2. **Tab click** — Find a tab element, click it, verify the active tab changed in the re-snapshot
3. **Link click** — Find a navigation link, click it, verify page/view changed
4. **Double click** — Find an element that responds to double-click, execute `{"action": "doubleClick"}`, verify response

**Specific scenarios:**
- **Web:** Navigate to a page with tabs (e.g., UI Bridge States page), click each tab, verify tab content changes
- **Runner:** Click between Application/State Explorer tabs on the UI Bridge Integration page, verify panel switches

---

### Category 3: Text Input

**What it tests:** type, clear, sendKeys actions on input fields, textareas, and search boxes.

**Page Setup — IMPORTANT:** Navigate to a page with real `<input>` and `<textarea>` elements. Do NOT test on a page that only has comboboxes (Radix Select) — those are buttons internally.
- **Web:** Navigate to `/automation-builder/workflows` (Workflow Builder) — has form inputs. Or use the search/filter input on the Execute page.
- **Runner:** Navigate to `/` (Execute page) — has `input` and `textarea` elements (search, description fields)

After navigating, take a snapshot and find elements with `type: "input"` or `type: "textarea"`. If none are found, navigate to another page.

**How to test:**

1. **Type into input** — Find an input element, execute `{"action": "type", "params": {"text": "test value"}}`, then GET the element's state to verify `value` field contains "test value"
2. **Clear input** — Execute `{"action": "clear"}` on the same input, GET state, verify value is empty
3. **Type with clear** — Execute `{"action": "type", "params": {"text": "new value", "clear": true}}`, verify old value replaced
4. **Send keys** — Execute `{"action": "sendKeys", "params": {"keys": [{"key": "a"}, {"key": "b"}]}}`, verify input received keystrokes
5. **Verify React controlled input** — After typing, GET `/control/element/<id>/state` and confirm `value` field shows the typed text (tests _valueTracker fix for React controlled inputs)

---

### Category 4: Selection Controls

**What it tests:** select, check, uncheck, toggle actions on dropdowns, checkboxes, radio buttons, and switches.

**Page Setup — IMPORTANT:** Navigate to a page with selection controls (toggles, checkboxes, selects, switches).
- **Web:** Navigate to the Workflow Builder page and look for the "Advanced Options" toggle/section at the bottom. Alternatively, navigate to any settings page with toggle switches.
- **Runner:** Navigate to `/` (Execute page) — has select dropdowns. Or navigate to a settings/configuration page with toggles.

After navigating, take a snapshot and search for elements with `type: "select"`, `type: "checkbox"`, or elements with `role: "switch"` or `aria-checked` in their state. If the page has an "Advanced Options" collapsible section, click it to expand and expose more controls.

**How to test:**

1. **Select dropdown** — Find a select element, GET its state to see `availableOptions`, execute `{"action": "select", "params": {"value": "<option>"}}`, verify selectedOptions changed
2. **Check checkbox** — Find a checkbox or switch element, execute `{"action": "check"}`, verify checked state is true
3. **Uncheck checkbox** — Execute `{"action": "uncheck"}`, verify checked state is false
4. **Toggle** — Find a switch/toggle element, execute `{"action": "toggle"}`, verify state flipped (checked goes from false→true or true→false)

**Specific scenarios:**
- **Web:** Find the Advanced Options toggle on the Workflow Builder, toggle it, verify aria-checked or expanded state changes
- **Runner:** Find a select dropdown, select a value from availableOptions, verify the value changed

---

### Category 5: Focus Management

**What it tests:** focus, blur, hover actions and their effect on element state.

**Page Setup:** Use the same page as Category 3 (a page with `<input>` elements). If you already navigated there for Cat 3, stay on that page.

**How to test:**

1. **Focus** — Find an input element, execute `{"action": "focus"}`, then `GET /control/element/<id>/state`, verify `focused: true`
2. **Blur** — Execute `{"action": "blur"}`, GET state, verify `focused: false`
3. **Hover** — Execute `{"action": "hover"}` on a button, verify response success

**Specific scenarios:**
- **Web:** Focus an input, verify `focused: true` in state endpoint (this field was recently added — confirm it appears)
- **Runner:** Focus an input, verify focused state in element detail

---

### Category 6: Scrolling

**What it tests:** scroll and scrollIntoView actions on scrollable containers and elements.

**Page Setup:** Navigate to a page with scrollable content.
- **Web:** Navigate to `/` (Execute page) or any page with a list of items that extends below the viewport
- **Runner:** Navigate to `/workflows` (Workflows list) or `/` if it has enough content to scroll

**How to test:**

1. **Scroll down** — Find any element, execute `{"action": "scroll", "params": {"direction": "down", "amount": 300}}`. Verify the response contains `scrollInfo` with `before` and `after` scroll positions. If the page is scrollable, `changed` should be `true`.
2. **Scroll up** — Execute `{"action": "scroll", "params": {"direction": "up", "amount": 300}}`, verify scrollTop decreased
3. **ScrollIntoView** — Find an element, execute `{"action": "scrollIntoView"}`, verify element is now in viewport

**Note:** The `scroll` action finds the nearest scrollable ancestor of the target element and scrolls it. If no scrollable ancestor exists, it falls back to `document.body`. If the entire page fits in the viewport, `changed` will be `false` — this is correct behavior, not a failure.

---

### Category 7: Form Operations

**What it tests:** submit, reset actions on form elements, plus overall form state reading.

**Page Setup:** Use the same page as Category 3 (page with form inputs).

**How to test:**

1. **Read form state** — Snapshot the page, verify form fields have values, validation state, and available actions
2. **Type + verify** — Type into an input, GET state, verify value persists (tests React controlled input fix)
3. **Clear + verify** — Clear the input, GET state, verify empty
4. **Submit** — If a submit button or form exists, execute `{"action": "submit"}`, verify effect
5. **Reset** — If a form exists, execute `{"action": "reset"}`, verify fields return to defaults

---

### Category 8: Navigation

**What it tests:** Page navigation via the control API (navigate, refresh, back, forward).

**How to test:**

1. **Navigate** — `POST $BASE/control/page/navigate` with `{"url": "/path"}`, re-snapshot to verify new page
2. **Refresh** — `POST $BASE/control/page/refresh`, verify page reloaded (snapshot still valid after)
3. **Back** — `POST $BASE/control/page/back`, verify previous page restored
4. **Forward** — `POST $BASE/control/page/forward`, verify forward navigation works

**Specific scenarios:**
- **Web:** Navigate to `/automation-builder/ui-bridge-states`, then to `/runs/active`, then back, then forward
- **Runner:** Navigate between pages using the control API (if supported — runner may not support all navigation)

---

### Category 9: Component Registration & Actions

**What it tests:** High-level component registration, listing, state retrieval, and action execution.

**How to test:**

1. **List components** — `GET $BASE/control/components` — verify components are registered with IDs, names, descriptions
2. **Component detail** — `GET $BASE/control/component/<id>` — verify actions list, element associations
3. **Component state** — `GET $BASE/control/component/<id>/state` — verify state properties returned
4. **Component action** — `POST $BASE/control/component/<id>/action/<actionId>` — verify action executes and state changes

**Specific scenarios:**
- **Web:** List components on the UI Bridge States page, execute a component action
- **Runner:** List components on the UI Bridge Integration page, check component state

---

### Category 10: Element Finding & Search

**What it tests:** The find endpoint for locating elements by criteria (text, role, type, accessibility).

**How to test:**

1. **Find by text** — `POST $BASE/control/find` with `{"text": "some button label"}` — verify matching element returned
2. **Find by role** — `POST $BASE/control/find` with `{"role": "button"}` — verify all buttons returned
3. **Find by type** — `POST $BASE/control/find` with `{"element_type": "input"}` — verify all inputs returned
4. **Find with multiple criteria** — Combine text + role, verify narrowed results

**Specific scenarios:**
- **Web:** Find all buttons on the current page, find a specific tab by text
- **Runner:** Find all input elements, find the discovery panel by text

---

### Category 11: Console Errors & Browser Events

**What it tests:** Capturing console errors, HMR errors, and browser event logs.

**How to test:**

1. **Console errors** — `GET $BASE/control/console-errors` — verify returns array of error entries (may be empty if no errors)
2. **Console errors with limit** — `GET $BASE/control/console-errors?limit=5` — verify limit respected
3. **Console errors since timestamp** — `GET $BASE/control/console-errors?since=<timestamp>` — verify filtering works
4. **Browser events log** — Check `.dev-logs/browser-events.jsonl` for captured events

**Specific scenarios:**
- **Web:** Trigger a known error scenario if possible, then check console-errors endpoint
- **Runner:** Check for any existing console errors after page load

---

### Category 12: Specs System

**What it tests:** Loading, listing, and evaluating UI Bridge specs (`.spec.uibridge.json` files).

**Important:** Specs ARE registered in both apps. If `GET /control/specs` returns empty, something is wrong — investigate.

**How to test:**

1. **List specs** — `GET $BASE/control/specs` — verify specs are returned (should be non-empty on both apps)
2. **Spec detail** — Verify individual specs contain groups, assertions, metadata
3. **Spec assertions** — Check that assertion types (exists, visible, hasText, etc.) are properly defined
4. **Spec count** — Count the number of registered specs and report it

**Specific scenarios:**
- **Web:** Check that specs are loaded, review assertion groups
- **Runner:** Verify specs are listed with proper structure (count, groups, assertions)

---

### Category 13: Drag & Drop

**What it tests:** The drag action for moving elements between positions or containers.

**How to test:**

1. **Drag element** — Find a draggable element, execute `{"action": "drag", "params": {"target": {"elementId": "target-id"}, "steps": 20}}`, verify element moved
2. **Verify position** — Re-snapshot and check the dragged element's rect has changed

**Specific scenarios:**
- **Web:** On the UI Bridge States graph editor, drag a state node to a new position
- **Runner:** If a drag-and-drop interface exists (e.g., state machine graph), drag a node

---

### Category 14: JavaScript Evaluation (Runner Only)

**What it tests:** The page/evaluate endpoint for executing arbitrary JavaScript in the webview.

**How to test:**

1. **Simple expression** — `POST $BASE/control/page/evaluate` with `{"expression": "document.title"}` — verify returns page title
2. **DOM query** — `{"expression": "document.querySelectorAll('button').length"}` — verify returns button count
3. **Async expression** — `{"expression": "new Promise(r => setTimeout(() => r('done'), 100))"}` — verify async result returned

**Specific scenarios:**
- **Runner only:** Execute `document.title`, verify it matches expected page title

---

### Category 15: Idle Detection

**What it tests:** Whether the UI Bridge correctly detects when the page has settled (network idle, DOM settled, no loading indicators, no animations).

**How to test:**

1. **Trigger navigation** — Navigate to a new page
2. **Check snapshot timing** — Take a snapshot immediately and after a delay; the post-delay snapshot should show settled state
3. **Loading indicators** — If any loading spinners exist during navigation, verify they clear before the idle state

**Specific scenarios:**
- **Web:** Navigate to a data-heavy page, observe if snapshot waits for data load
- **Runner:** Switch tabs, verify idle detection allows elements to settle before snapshot

---

### Category 16: Connection Health

**What it tests:** Whether the UI Bridge endpoints respond correctly and report health status.

**How to test:**

1. **Snapshot response time** — Measure time for `GET $BASE/control/snapshot`, should be under 2 seconds
2. **Error handling** — Request a nonexistent element `GET $BASE/control/element/nonexistent-id`, verify clean error response
3. **Invalid action** — `POST $BASE/control/element/<id>/action` with `{"action": "invalidAction"}`, verify error message

**Specific scenarios:**
- **Web:** Test all major endpoints respond, check response format consistency
- **Runner:** Same endpoint checks, compare response format with web

---

### Category 17: Workflow System

**What it tests:** Listing and executing registered workflows via the control API.

**How to test:**

1. **List workflows** — `GET $BASE/control/workflows` — check if any workflows are registered
2. **Workflow detail** — If workflows exist, get details and verify step definitions
3. **Execute workflow** — If a safe test workflow exists, execute it and check status

**Specific scenarios:**
- **Web:** Check for registered workflows on the automation builder pages
- **Runner:** Check for registered workflows in the state machine builder

---

### Category 18: Render Log

**What it tests:** DOM change observation, snapshot logging, and render log retrieval.

**How to test:**

1. **Get render log (base path)** — `GET $BASE/render-log` — verify it returns entries or a valid empty response
2. **Get render log (control alias)** — `GET $BASE/control/render-log` — verify this ALSO works (both paths should be valid on both apps)
3. **Entry content** — After some interactions (clicks, type, navigate), check render log for new entries recording those interactions
4. **Entry structure** — Verify entries have `type`, `timestamp`, and relevant metadata (e.g., `action`, `elementId` for interaction entries)

**Specific scenarios:**
- **Web:** Both `/render-log` and `/control/render-log` should return 200 with entries
- **Runner:** Both paths should return entries including interaction history from the test session

---

### Category 19: AI Search & NL Actions

**What it tests:** Natural language element search (`/ai/find`), AI-powered search (`/ai/search`), and AI action execution (`/ai/execute`).

**Page Setup:**
- **Web:** Navigate to `/` (Execute page) — has varied interactive elements for search and action targets
- **Runner:** Navigate to `/` — has buttons, inputs, selects for NL targeting

**How to test:**

1. **AI find by description** — `POST $BASE/ai/find` with `{"query": "search input"}` — verify it returns a matching element with confidence score
2. **AI find with spatial relation** — `POST $BASE/ai/find` with `{"query": "button near the search box"}` — verify spatial resolution works
3. **AI find with container context** — `POST $BASE/ai/find` with `{"query": "first button in the main area"}` — verify container scoping and ordinal
4. **AI search** — `POST $BASE/ai/search` with `{"query": "all buttons"}` — verify returns multiple results with relevance scores
5. **AI execute** — `POST $BASE/ai/execute` with `{"instruction": "click the first tab"}` — verify action is executed and response includes result
6. **Disambiguation** — `POST $BASE/ai/find` with an ambiguous query (e.g., `{"query": "button"}`) — verify disambiguation suggestions are returned when multiple matches

---

### Category 20: Semantic Snapshot & Page Summary

**What it tests:** AI-readable semantic page snapshots and page summaries, compared with control snapshots.

**Page Setup:**
- **Web:** Navigate to `/` — data-rich page for semantic analysis
- **Runner:** Navigate to `/` — same rationale

**How to test:**

1. **AI snapshot** — `GET $BASE/ai/snapshot` — verify returns semantic representation of the page (elements grouped by region, roles, and purpose)
2. **AI summary** — `GET $BASE/ai/summary` — verify returns a natural language summary of the page state
3. **Compare with control snapshot** — `GET $BASE/control/snapshot` and `GET $BASE/ai/snapshot` — verify the AI snapshot covers the same elements but in a more structured/semantic format
4. **Snapshot completeness** — Verify AI snapshot includes: page context (URL, title), element groupings, modal state, toast state, navigation context

---

### Category 21: Capabilities

**What it tests:** The self-description endpoint that reports which features the UI Bridge instance supports.

**How to test:**

1. **Get capabilities** — `GET $BASE/capabilities` (or `GET $BASE/control/capabilities` if that's the path) — verify it returns a list of supported features
2. **Feature flags** — Check that the response includes flags for: AI search, forms, idle detection, network monitoring, screenshots, specs, change tracking, etc.
3. **Compare apps** — Compare capabilities between web and runner — document which features are available in each

**Note:** This is a discovery/introspection endpoint. If neither path returns a valid response, try `GET $BASE/health` which may include capability information.

---

### Category 22: AI Assertions

**What it tests:** AI-powered assertions that verify UI state using natural language predicates.

**Page Setup:**
- **Web:** Navigate to `/` — needs visible elements to assert against
- **Runner:** Navigate to `/` — same rationale

**How to test:**

1. **Single assertion (exists)** — `POST $BASE/ai/assert` with `{"assertion": "a search input exists on the page"}` — verify pass/fail result
2. **Single assertion (visible)** — `POST $BASE/ai/assert` with `{"assertion": "the main navigation is visible"}` — verify result
3. **Single assertion (hasText)** — `POST $BASE/ai/assert` with `{"assertion": "the page title contains 'Qontinui'"}` — verify text matching
4. **Single assertion (negative)** — `POST $BASE/ai/assert` with `{"assertion": "there are no error messages on the page"}` — verify negative assertion
5. **Batch assertions** — `POST $BASE/ai/assert-batch` with `{"assertions": ["a button exists", "the page has loaded", "no modal is blocking the view"]}` — verify all assertions are evaluated and individual results returned
6. **Failing assertion** — `POST $BASE/ai/assert` with `{"assertion": "a dinosaur element is visible"}` — verify it correctly reports failure

**Assertion types to cover (sample from the 21 types):** exists, visible, hidden, enabled, disabled, hasText, hasValue, checked, unchecked, hasClass, hasAttribute, inViewport, hasChildren, isEmpty, isFocused, hasRole, matchesSelector, hasStyle, isInteractive, hasLabel, isValid.

---

### Category 23: Form Discovery

**What it tests:** Form-centric page view with field values, validation state, dirty tracking, and constraints.

**Page Setup — IMPORTANT:** Navigate to a page with real form elements.
- **Web:** Navigate to `/automation-builder/workflows` (Workflow Builder) — has form inputs, toggles, validation
- **Runner:** Navigate to `/` (Execute page) — has inputs, textareas, selects

**How to test:**

1. **Get forms** — `GET $BASE/control/forms` — verify returns form-centric view with fields, values, validation state
2. **Form field details** — Check that form fields include: `validationState` (valid/invalid/pending), `constraints` (min, max, pattern), `required` flag
3. **Dirty tracking** — Type into a field (via Cat 3's type action), then `GET $BASE/control/forms` — verify the modified field shows dirty state
4. **Snapshot forms** — Take a forms snapshot, interact with a field, take another snapshot — verify diff shows the change
5. **Validation state** — If a required field exists, clear it and check that `validationState` changes to `invalid`

---

### Category 24: Network Monitoring

**What it tests:** Network request tracking — history, in-flight requests, and wait-for-request.

**How to test:**

1. **Request history** — `GET $BASE/control/network-requests` — verify returns array of recent requests with URL, method, status, timing
2. **In-flight requests** — `GET $BASE/control/network-requests/in-flight` — verify returns currently pending requests (may be empty if page is idle)
3. **Request detail** — If request history has entries, `GET $BASE/control/network-request/<id>` for a specific request — verify full request/response details
4. **Wait for request** — Trigger a navigation, then `POST $BASE/control/network-requests/wait` with `{"url": "/api", "timeout": 5000}` — verify it waits and returns the matching request

**Note:** In-flight may return empty if the page is fully loaded. This is correct behavior, not a failure. The important test is that the endpoint responds with the right structure.

---

### Category 25: Change Tracking & Diffs

**What it tests:** Before/after semantic diffing, change categorization, conditional waits, and scoped diffs.

**Page Setup:**
- **Web:** Navigate to `/` — needs interactive elements to trigger changes
- **Runner:** Navigate to `/` — same rationale

**How to test:**

1. **Execute with diff** — `POST $BASE/ai/execute-with-diff` with `{"instruction": "click the first tab", "categorize": true}` — verify response includes before/after diff and change category
2. **Element action with diff** — `POST $BASE/ai/execute-with-diff` with `{"elementAction": {"elementId": "<id>", "action": "click"}, "categorize": true}` — verify structured diff
3. **Change categories** — Verify the diff response includes a `category` field with one of: `navigation`, `feedback`, `data-update`, `ui-state`, `loading`, `no-op`
4. **Categorize last diff** — `GET $BASE/ai/categorize-last-diff` — verify it returns the category of the most recent diff
5. **Scoped diff** — `POST $BASE/ai/scoped-diff` with `{"scope": "main"}` — verify diff is scoped to the specified container
6. **Summarize diff** — `POST $BASE/ai/summarize-diff` with `{"budget": 500}` — verify text summary within budget

---

### Category 26: Bookmarks

**What it tests:** Named snapshot bookmarks for comparing UI state across time.

**Page Setup:** Use whatever page is currently loaded — bookmarks capture state at any point.

**How to test:**

1. **Save bookmark** — `POST $BASE/ai/bookmarks` with `{"name": "test-start"}` — verify bookmark saved successfully
2. **List bookmarks** — `GET $BASE/ai/bookmarks` — verify `test-start` appears in the list
3. **Get bookmark detail** — `GET $BASE/ai/bookmark/test-start` — verify it contains the captured snapshot data
4. **Diff from bookmark** — Perform an interaction (click a tab, type in a field), then `GET $BASE/ai/bookmark/test-start/diff` — verify it shows changes since the bookmark
5. **Delete bookmark** — `DELETE $BASE/ai/bookmark/test-start` — verify deletion succeeds
6. **Verify deleted** — `GET $BASE/ai/bookmarks` — verify `test-start` no longer in the list

---

### Category 27: Error Sessions

**What it tests:** Per-automation error session tracking with baseline comparison for regression detection.

**How to test:**

1. **Start session** — `POST $BASE/control/error-sessions/start` — verify returns `sessionId`
2. **List sessions** — `GET $BASE/control/error-sessions` — verify active session appears
3. **End session** — `POST $BASE/control/error-sessions/end` — verify returns `ErrorSessionSummary` (or null if no errors during session)
4. **Capture baseline** — `POST $BASE/control/error-baselines/capture` with `{"label": "test-baseline"}` — verify returns baseline with `fingerprintCount`
5. **Compare baseline** — `POST $BASE/control/error-baselines/compare` — verify returns comparison with `newErrors`, `fixedErrors`, `knownErrors`, `isRegression`, `delta`
6. **Error report** — `GET $BASE/control/error-report` — verify composite report includes: health score, recent errors, active session info, error snapshots

---

### Category 28: Design Inspection

**What it tests:** Element computed styles, design snapshots, and design audits.

**Page Setup:**
- **Web:** Navigate to `/` — needs styled interactive elements
- **Runner:** Navigate to `/` — same rationale

**How to test:**

1. **Element styles** — Find an element from the snapshot, `GET $BASE/control/design/element/<id>/styles` — verify returns computed styles (color, font, padding, etc.)
2. **Design snapshot** — `POST $BASE/control/design/snapshot` — verify returns page-level design summary (colors, typography, spacing patterns)
3. **Design audit** — `POST $BASE/control/design/audit` — verify returns accessibility and design consistency findings
4. **Responsive check** — `POST $BASE/control/design/responsive` — verify returns viewport-aware design information

---

### Category 29: Undo/Redo

**What it tests:** Undo/redo state detection and execution.

**Page Setup:**
- **Web:** Navigate to a page with an editor or form where undo makes sense (e.g., Workflow Builder)
- **Runner:** Navigate to `/` — may have undo-capable inputs

**How to test:**

1. **Get undo state** — `GET $BASE/control/undo-state` — verify returns `canUndo`, `canRedo`, `undoDescription`, `source`, `summary`
2. **Execute undo** — If `canUndo` is true: `POST $BASE/control/undo` — verify undo executes and state changes
3. **Execute redo** — If `canRedo` is true: `POST $BASE/control/redo` — verify redo executes

**Note:** Many pages may not have undo-capable contexts. The undo state returning `canUndo: false, canRedo: false` is a **valid response** — it means the detection works but there's nothing to undo. Only mark as FAIL if the endpoint itself errors or returns malformed data. To get a `canUndo: true` state, try typing into a contentEditable field or a rich text editor first.

---

### Category 30: Annotations

**What it tests:** CRUD operations on UI annotations, coverage analysis, and export/import.

**How to test:**

1. **Create annotation** — `POST $BASE/control/annotations` with `{"elementId": "<id>", "text": "Test annotation", "type": "note"}` — verify annotation created with an ID
2. **List annotations** — `GET $BASE/control/annotations` — verify the test annotation appears
3. **Get annotation** — `GET $BASE/control/annotation/<id>` — verify full annotation detail returned
4. **Update annotation** — `PUT $BASE/control/annotation/<id>` with `{"text": "Updated annotation"}` — verify update succeeds
5. **Coverage** — `GET $BASE/control/annotations/coverage` — verify returns coverage metrics (how many elements are annotated)
6. **Export** — `GET $BASE/control/annotations/export` — verify returns exportable format
7. **Delete annotation** — `DELETE $BASE/control/annotation/<id>` — verify deletion succeeds

**Note:** Annotations may not be implemented on all apps. If the endpoint returns 404, mark as SKIP with a note that the feature is not available on that app.

---

### Category 31: Advanced Idle Detection

**What it tests:** Composite idle signals — network, DOM, loading indicators, form mutation — individually and combined.

**How to test:**

1. **Composite idle status** — `GET $BASE/control/idle-status` — verify returns all signal states with weights and composite score
2. **Individual signals** — For each of `network`, `dom`, `loading-indicators`, `form-mutation`: `GET $BASE/control/idle-status/<signal>` — verify individual signal status
3. **Wait for idle** — `POST $BASE/control/wait-for-idle` with `{"timeout": 5000, "minStableMs": 300}` — verify it resolves when app is idle
4. **Wait for single signal** — `POST $BASE/control/wait-for-idle/network` with `{"timeout": 5000}` — verify network-only wait works
5. **Wait with exclusion** — `POST $BASE/control/wait-for-idle` with `{"timeout": 5000, "exclude": ["dom"]}` — verify DOM signal excluded from idle calculation
6. **Wait for targets** — `POST $BASE/control/wait-for-targets` with `{"targets": ["network"], "timeout": 5000}` — verify targeted wait

---

### Category 32: Clipboard

**What it tests:** System clipboard read/write operations.

**How to test:**

1. **Write to clipboard** — `POST $BASE/control/clipboard` with `{"text": "UI Bridge test clipboard content"}` — verify write succeeds
2. **Read from clipboard** — `GET $BASE/control/clipboard` — verify returns the text that was just written
3. **HTML clipboard** — `POST $BASE/control/clipboard` with `{"text": "fallback", "html": "<b>Bold test</b>"}` — verify HTML content can be written (if supported)

**Note:** Browser-based clipboard access may be blocked by permissions policies. The runner uses the Rust `arboard` crate for direct OS clipboard access, which is more reliable. If the web frontend returns a permission error, this is expected browser behavior — mark as PARTIAL with a note, not FAIL. The runner should work without permission issues.

---

### Category 33: Element History

**What it tests:** Per-element action history, global action history, and interaction metrics.

**Prerequisites:** Run some interactions first (clicks, types from earlier categories) to populate action history.

**How to test:**

1. **Global action history** — `GET $BASE/control/action-history` (or `GET $BASE/control/history`) — verify returns a chronological list of actions performed during the session
2. **Per-element history** — Find an element that was interacted with earlier, `GET $BASE/control/element/<id>/history` — verify returns actions performed on that specific element
3. **Interaction metrics** — `GET $BASE/control/metrics` (or `GET $BASE/control/interaction-metrics`) — verify returns aggregate metrics (total actions, actions by type, most interacted elements)

**Note:** History endpoints may use different path patterns. Try both `/control/action-history` and `/control/history`. If neither returns data, check the render log (Cat 18) which also records interactions.

---

### Category 34: State Machine

**What it tests:** State discovery, state listing, and state machine integration via the UI Bridge.

**Page Setup — IMPORTANT:**
- **Web:** Navigate to `/automation-builder/states` — the state machine page with graph nodes and edges
- **Runner:** Navigate to the state machine page if available (check routes)

**How to test:**

1. **Discover states** — `POST $BASE/control/discover-states` — verify returns discovered states based on element co-occurrence (may need render log data as input)
2. **State explorer** — `POST $BASE/explore` (runner only) — verify state exploration can be started
3. **Explore status** — `GET $BASE/explore/status` (runner only) — verify exploration status endpoint responds
4. **Snapshot on state page** — Take a snapshot on the state machine page — verify graph elements (nodes, edges, panels) are discovered

**Note:** State discovery requires render log data from multiple page loads to build co-occurrence patterns. If `/control/discover-states` needs a request body with render logs, provide entries from `GET $BASE/render-log`. The state machine page may be web-only — mark runner as SKIP if the page doesn't exist.

---

### Category 35: Intents

**What it tests:** Intent registration and execution via the UI Bridge.

**Prerequisites:** Register a test intent via the API before testing.

**How to test:**

1. **List intents** — `GET $BASE/control/intents` — verify returns list of registered intents (may be empty initially)
2. **Register intent** — `POST $BASE/control/intents` with `{"name": "test-intent", "description": "A test intent", "handler": "noop"}` — verify intent registered
3. **Get intent** — `GET $BASE/control/intent/test-intent` — verify intent details returned
4. **Execute intent** — `POST $BASE/control/intent/test-intent/execute` — verify intent executes (even if handler is noop)
5. **Clean up** — `DELETE $BASE/control/intent/test-intent` — verify deletion

**Note:** Intents may not be implemented on all apps. If the endpoint returns 404, this feature may be exposed under a different path or may not be available yet. Mark as SKIP with a note. Try alternative paths like `/control/workflows` (Cat 17) which may serve a similar purpose.

---

### Category 36: Media Discovery

**What it tests:** Finding images, icons, media elements, and running accessibility/performance audits on media.

**Page Setup:**
- **Web:** Navigate to `/` — workflow cards often have icons and images
- **Runner:** Navigate to `/` — has UI icons

**How to test:**

1. **Find images** — `POST $BASE/control/find` with `{"element_type": "image"}` or `POST $BASE/control/discover` filtered to images — verify image elements returned
2. **Find by role** — `POST $BASE/control/find` with `{"role": "img"}` — verify elements with img role returned
3. **Media accessibility** — For each discovered image element, `GET $BASE/control/element/<id>/state` — check for `alt` text, `aria-label`, or missing accessibility attributes
4. **Icon discovery** — `POST $BASE/control/find` with `{"element_type": "icon"}` or search for elements with icon-related classes (lucide, svg) — verify icon elements found

**Note:** This category tests the SDK's ability to discover and inspect media elements, not a dedicated media endpoint. Results depend on what media is present on the page. If the page has no images, navigate to a different page or mark specific tests as SKIP.

---

### Category 37: Page Data Extraction

**What it tests:** Structured data extraction from the page — data maps, regions, and content extraction.

**Page Setup:**
- **Web:** Navigate to `/` — has workflow cards with structured data (names, statuses, dates)
- **Runner:** Navigate to `/` — has task data

**How to test:**

1. **AI snapshot with regions** — `GET $BASE/ai/snapshot` — verify the response groups elements into semantic regions (header, main, sidebar, footer)
2. **Find data elements** — `POST $BASE/control/find` with `{"role": "status"}` or `{"element_type": "badge"}` — verify data-bearing elements found
3. **Extract structured content** — For elements with text content, `GET $BASE/control/element/<id>/state` — verify `textContent`, `value`, and semantic properties are extractable
4. **Page context** — Verify snapshot includes `page` context with `url`, `pathname`, `title`, `semanticName`, `section`
5. **Visual description** — If screenshot tools are available, check for `visual-description` endpoint that returns text-based layout analysis with detected regions

**Note:** "Data extraction" here means using the SDK's existing snapshot/find/state endpoints to pull structured data from the page. There may not be a dedicated `/data-map` endpoint — the test validates that existing endpoints provide enough information for structured extraction.

---

### Category 38: Cross-App Comparison

**What it tests:** Comparing UI state between the web frontend and runner apps to verify consistency.

**Prerequisites:** Both web (port 3001) and runner (port 9876) must be running and responsive.

**How to test:**

1. **Snapshot both apps** — `GET https://qontinui.io/api/ui-bridge/control/snapshot` and `GET http://localhost:9876/ui-bridge/control/snapshot` — capture both
2. **Compare element types** — Count elements by type in each app — note which element types appear in both vs only in one
3. **Compare capabilities** — Compare health/capabilities responses from both apps — document feature parity
4. **Compare AI snapshots** — `GET $BASE/ai/snapshot` on both apps — compare semantic structure and grouping
5. **Compare idle status** — `GET $BASE/control/idle-status` on both apps — verify both report idle state consistently

**Note:** The web and runner are different applications, so they will have different elements and pages. The comparison tests that the UI Bridge API surface is consistent across both, not that the apps look the same. Focus on: response format consistency, same endpoint paths working on both, and feature availability.

---

### Category 39: Performance & Timeline

**What it tests:** Performance entries, event timeline, health reports, and network chain correlation.

**How to test:**

1. **Timeline** — `GET $BASE/control/timeline` — verify returns chronological entries of actions and events with timestamps
2. **Timeline with filters** — `GET $BASE/control/timeline?limit=10&minSeverity=warning` — verify filtering works
3. **Health report** — `GET $BASE/control/health` — verify returns `status` (healthy/degraded/broken), `score` (0-100), `summary`, `breakdown`, `errorRate`, `topIssue`
4. **Health with window** — `GET $BASE/control/health?windowMs=60000` — verify time-windowed health works
5. **Network chains** — `GET $BASE/control/network-chains` — verify returns correlated request/error chains
6. **Network chains filtered** — `GET $BASE/control/network-chains?failuresOnly=true` — verify filtering to failures
7. **Browser events** — `GET $BASE/control/browser-events?deduplicate=true` — verify deduplicated event log with fingerprinting
8. **Error snapshots** — `GET $BASE/control/error-snapshots` — verify returns auto-captured app state on crash/error events

---

## Reporting Format

For each category, produce a report in this format:

```
### Category N: [Name] — [App: Web/Runner]

**Status:** PASS / PARTIAL / FAIL / SKIP

#### Checklist
- [x] Test 1 description — passed
- [ ] Test 2 description — failed: [reason]
- [~] Test 3 description — partial: [details]
- [-] Test 4 description — skipped: [reason]

#### Discoverability (1-5)
**Rating: N/5**
How easy was it to figure out how to access and use this functionality?
- 5 = Obvious, self-documenting, works on first try
- 4 = Easy to find, minor ambiguity
- 3 = Required some exploration or documentation
- 2 = Difficult to find or understand without docs
- 1 = Very difficult, undocumented, or confusing

**Notes:** [Specific observations about discoverability]

#### Effectiveness (1-5)
**Rating: N/5**
How well did the functionality work once accessed?
- 5 = Perfect, fast, reliable, complete data
- 4 = Works well, minor issues
- 3 = Functional but with notable limitations
- 2 = Partially working, significant issues
- 1 = Broken or unusable

**Notes:** [Specific observations about effectiveness]
```

## Final Summary

After all categories, produce a summary table:

```
## UI Bridge Test Summary — [Date]

| # | Category | Web Status | Web Disc. | Web Eff. | Runner Status | Runner Disc. | Runner Eff. |
|---|----------|------------|-----------|----------|---------------|--------------|-------------|
| 0 | Health & Recovery | ... | ... | ... | ... | ... | ... |
| 1 | Element Discovery | ... | ... | ... | ... | ... | ... |
| 2 | Click Actions | ... | ... | ... | ... | ... | ... |
| 3 | Text Input | ... | ... | ... | ... | ... | ... |
| 4 | Selection Controls | ... | ... | ... | ... | ... | ... |
| 5 | Focus Management | ... | ... | ... | ... | ... | ... |
| 6 | Scrolling | ... | ... | ... | ... | ... | ... |
| 7 | Form Operations | ... | ... | ... | ... | ... | ... |
| 8 | Navigation | ... | ... | ... | ... | ... | ... |
| 9 | Component Registration | ... | ... | ... | ... | ... | ... |
| 10 | Element Finding | ... | ... | ... | ... | ... | ... |
| 11 | Console Errors | ... | ... | ... | ... | ... | ... |
| 12 | Specs System | ... | ... | ... | ... | ... | ... |
| 13 | Drag & Drop | ... | ... | ... | ... | ... | ... |
| 14 | JS Evaluation | ... | ... | ... | ... | ... | ... |
| 15 | Idle Detection | ... | ... | ... | ... | ... | ... |
| 16 | Connection Health | ... | ... | ... | ... | ... | ... |
| 17 | Workflow System | ... | ... | ... | ... | ... | ... |
| 18 | Render Log | ... | ... | ... | ... | ... | ... |
| 19 | AI Search & NL Actions | ... | ... | ... | ... | ... | ... |
| 20 | Semantic Snapshot | ... | ... | ... | ... | ... | ... |
| 21 | Capabilities | ... | ... | ... | ... | ... | ... |
| 22 | AI Assertions | ... | ... | ... | ... | ... | ... |
| 23 | Form Discovery | ... | ... | ... | ... | ... | ... |
| 24 | Network Monitoring | ... | ... | ... | ... | ... | ... |
| 25 | Change Tracking | ... | ... | ... | ... | ... | ... |
| 26 | Bookmarks | ... | ... | ... | ... | ... | ... |
| 27 | Error Sessions | ... | ... | ... | ... | ... | ... |
| 28 | Design Inspection | ... | ... | ... | ... | ... | ... |
| 29 | Undo/Redo | ... | ... | ... | ... | ... | ... |
| 30 | Annotations | ... | ... | ... | ... | ... | ... |
| 31 | Advanced Idle | ... | ... | ... | ... | ... | ... |
| 32 | Clipboard | ... | ... | ... | ... | ... | ... |
| 33 | Element History | ... | ... | ... | ... | ... | ... |
| 34 | State Machine | ... | ... | ... | ... | ... | ... |
| 35 | Intents | ... | ... | ... | ... | ... | ... |
| 36 | Media Discovery | ... | ... | ... | ... | ... | ... |
| 37 | Page Data Extraction | ... | ... | ... | ... | ... | ... |
| 38 | Cross-App Comparison | ... | ... | ... | ... | ... | ... |
| 39 | Performance & Timeline | ... | ... | ... | ... | ... | ... |

**Overall Web Score:** X/5 discoverability, Y/5 effectiveness
**Overall Runner Score:** X/5 discoverability, Y/5 effectiveness

### Phase 0 Summary
- **Web:** [initial state] → [final state]. Fixes applied: [list or "none needed"]
- **Runner:** [initial state] → [final state]. Fixes applied: [list or "none needed"]
- **Endpoints exercised during recovery:** [list which UI Bridge endpoints were used diagnostically]

### Top Issues
1. [Most critical issue found]
2. [Second most critical]
3. ...

### Recommendations
1. [Actionable improvement suggestion]
2. ...
```

Save the full report to `$PWD/.dev-logs/ui-bridge-test-report-$(date +%Y%m%d-%H%M%S).md`.

## Rules

- **Navigate first, then test** — each category specifies a Page Setup step. ALWAYS navigate to the right page before testing. Don't test text input on a page that has no inputs. Don't test selection controls on a page that has no checkboxes. If the first page you navigate to doesn't have the required elements, try another page before marking the category as FAIL.
- **Verify elements exist before testing actions** — after navigating, take a snapshot and confirm the page has the element types you need (inputs, selects, switches, etc.). If not, navigate to a different page. Use `POST /control/find` to search for elements by type.
- **Phase 0 is mandatory** — always run health & recovery before functional tests. Never test an unresponsive app — fix it first or document why it can't be fixed
- **Diagnostic work counts as testing** — any UI Bridge endpoints used during Phase 0 recovery (console-errors, health, AI snapshot, navigate) provide data points for the relevant test categories
- **Test both apps** — run every category against web AND runner unless the category is app-specific (e.g., JS evaluation is runner-only) or the app could not be made responsive in Phase 0
- **Snapshot before and after** — always take a snapshot before interactions and re-snapshot after to verify changes
- **Don't dump raw JSON** — summarize results in the checklist format
- **Be honest about ratings** — if something is hard to find or doesn't work well, rate it accordingly
- **Note app-specific differences** — if behavior differs between web and runner, call it out
- **Continue on failure** — if one category fails, still test all remaining categories
- **Capture errors** — if an endpoint returns an error, include the error message in the report
- **Fix before failing** — if an app has a JS error or connectivity issue that blocks testing, attempt to fix it (up to 3 tries) before marking categories as FAIL. Use the UI Bridge's own diagnostic endpoints to find the root cause
- **Don't blame the SDK for missing elements** — if a page has no inputs, that's not a text input bug. Navigate to a page that has inputs. Only mark a test as FAIL if the SDK endpoint itself returns an error or incorrect data when the required elements are present.

## Arguments

$ARGUMENTS
