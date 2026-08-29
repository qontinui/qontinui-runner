## UI Bridge Control - Core API Reference

UI Bridge enables AI-driven control of applications through a unified HTTP API. This context covers the core API that works with ANY UI Bridge-enabled application.

### Connection Setup

```python
from ui_bridge import UIBridgeClient

# Default connection (localhost:9876)
client = UIBridgeClient()

# Custom connection
client = UIBridgeClient(
    base_url="http://localhost:9876",
    timeout=30.0,
    api_path="/ui-bridge"
)

# Common connection targets:
# - Local runner:        http://localhost:9876
# - Android emulator:    http://10.0.2.2:8087
# - iOS simulator:       http://localhost:8087
# - Physical device:     http://<device-ip>:8087
# - Web app (dev):       http://localhost:3001 (check app's configured port)
```

### Element Discovery

```python
# Get all registered elements
elements = client.get_elements()
for elem in elements:
    print(f"{elem.id}: {elem.type} - {elem.label}")
    print(f"  Actions: {elem.actions}")
    print(f"  State: visible={elem.state.visible}, enabled={elem.state.enabled}")

# Get specific element
elem = client.get_element("submit-button")

# Find elements by criteria
found = client.find(
    types=["button", "input"],      # Filter by element type
    interactive_only=True,           # Only interactive elements
    include_hidden=False,            # Exclude hidden elements
    limit=20                         # Max results
)

# Get full UI snapshot
snapshot = client.get_snapshot()
```

### Element Actions

All actions support wait options: `wait_visible=True`, `wait_enabled=True`, `timeout=10.0`

```python
# Click actions
client.click("button-id")
client.double_click("button-id")
client.right_click("button-id")

# Input actions
client.type("input-id", "hello world")
client.type("input-id", "replace", clear=True)  # Clear first
client.clear("input-id")

# Focus actions
client.focus("input-id")
client.blur("input-id")

# Selection actions (dropdowns, checkboxes)
client.select("dropdown-id", "option-value")
client.select("dropdown-id", "Option Label", by_label=True)
client.check("checkbox-id")
client.uncheck("checkbox-id")
client.toggle("checkbox-id")

# Scroll actions
client.scroll("container-id", direction="down", amount=100)
client.scroll("container-id", to_element="target-element-id")

# Hover
client.hover("element-id")
```

### Component Actions

Components expose high-level operations that orchestrate multiple element interactions:

```python
# List all components
components = client.get_components()
for comp in components:
    print(f"{comp.id}: {comp.name}")
    for action in comp.actions:
        print(f"  - {action.id}: {action.label}")

# Execute component action
result = client.execute_component_action(
    component_id="login-form",
    action_id="submit",
    params={"email": "user@example.com", "password": "secret"}
)

# Fluent syntax
result = client.component("login-form").action("submit", {...})
```

### Element State

```python
# Get current state
state = client.get_element_state("element-id")
print(f"Visible: {state.visible}")
print(f"Enabled: {state.enabled}")
print(f"Focused: {state.focused}")
print(f"Value: {state.value}")
print(f"Text: {state.text}")
print(f"Bounds: {state.rect}")  # {x, y, width, height}

# Wait for state
client.click("element-id", wait_visible=True, wait_enabled=True, timeout=10.0)
```

### Health & Connection

```python
# Check if server is reachable
if client.is_connected():
    print("Connected!")

# Get server health
health = client.health()
print(f"Status: {health.status}")
```

### Error Handling

```python
from ui_bridge import (
    UIBridgeError,
    ElementNotFoundError,
    ActionFailedError,
)

try:
    client.click("non-existent")
except ElementNotFoundError as e:
    print(f"Element not found: {e}")
except ActionFailedError as e:
    print(f"Action failed: {e}")
except UIBridgeError as e:
    print(f"Bridge error: {e}, code: {e.code}")
```

### HTTP API Endpoints (Raw)

If not using the Python client:

| Method | Endpoint                                            | Description              |
| ------ | --------------------------------------------------- | ------------------------ |
| GET    | `/ui-bridge/control/elements`                       | List all elements        |
| GET    | `/ui-bridge/control/element/:id`                    | Get element details      |
| GET    | `/ui-bridge/control/element/:id/state`              | Get element state        |
| POST   | `/ui-bridge/control/element/:id/action`             | Execute action           |
| GET    | `/ui-bridge/control/components`                     | List all components      |
| POST   | `/ui-bridge/control/component/:id/action/:actionId` | Execute component action |
| POST   | `/ui-bridge/control/find`                           | Find elements            |
| POST   | `/ui-bridge/ai/find`                                | NL element lookup        |
| GET    | `/ui-bridge/control/snapshot`                       | Get UI snapshot          |
| GET    | `/health`                                           | Health check             |

`/ui-bridge/ai/find` request body: `{ query: string, minConfidence?: number, includeHidden?: bool, context?: object, confidenceThreshold?: number }`.

- `includeHidden` (bool, optional, **default true**): match elements regardless of visibility — preserves historical front-end behaviour (the registry contains hidden elements like collapsed-sidebar children). Pass `false` to opt into the visibility filter.

### Choosing a wait / nav / batch primitive

Prefer these primitives over `sleep N && re-discover`; they all already exist. Each pairs a bounded wait or a single round-trip with a verification signal, which is usually what the naive sleep is trying to approximate.

**Waits** — use instead of fixed sleeps:

- `POST /ui-bridge/control/wait-for-element-state` — wait until a known element id becomes `visible` / `enabled` / `disabled` / `ariaDisabled` / `focused`. 100 ms polling. Body: `{ id, state, timeout_ms }`. Use when you already have a registered element id. `enabled` is the derived fold `!(disabled || ariaDisabled)`. Note the camelCase `ariaDisabled` — `aria_disabled` is rejected with `error_detail.code = "INVALID_STATE"`. **Which of the two disabled signals you can actually wait on depends on the bridge build the app is running:** on a build that emits them separately, `disabled` and `ariaDisabled` distinguish the native attribute from the ARIA one; on a build that emits only the `enabled` fold, `disabled` still works (derived from `enabled`) but `ariaDisabled` is unanswerable — the wait returns `{found:false, reason:"state-not-observable", requested_state:"ariaDisabled"}` rather than silently burning the timeout. Treat that `reason` as "ask a different question", not as "the element is not ARIA-disabled"; `disabled` is the portable choice when you just need "not interactable".
- `POST /ui-bridge/control/wait-for-route` — wait until the current route matches a glob (`**` supported). Body: `{ pattern, timeout_ms }`. Use after a navigation trigger when the route is the observable signal.
- `POST /ui-bridge/ai/wait-for-element-condition` — structured-selector wait: `{ condition: 'present'|'visible'|'clickable'|'text-matches', target: { id|title|aria_label|text|type }, timeout_ms }`. Use when you don't have a registered id yet. Returns 408 on timeout.

**Tab navigation** — pick the simplest that works:

- `POST /ui-bridge/control/activate-tab/{tab_id}` — fire-and-forget Tauri event; fastest path when you just need the tab switched and don't need a signal back.
- `POST /ui-bridge/control/page/set-tab` — body `{ tab }`. Dispatches a `CustomEvent("ui-bridge-set-tab", …)` and reads back `[data-page-id]` so you get a verification signal in the response. Works even when the SDK is unresponsive.
- `window.__UI_BRIDGE__.stateMachine.navigateTo("page-<slug>")` via `POST /ui-bridge/control/page/evaluate` — runs the compiled sidebar transition. Use when you need the state-machine side effects (active-state updates, etc.).
- Avoid clicking sidebar buttons via `POST /ui-bridge/control/element/<id>/action` for navigation — the click handler fires but may not route through the state-machine transition. Use one of the three above instead.

**`page.pathname` vs `page.route.pattern`:** on the runner, `page.pathname` reflects the webview's HTML-history path and stays at the initial value across tab switches — tab navigation is React-state-only, not `history.pushState`. Prefer `page.route.pattern` and `page.route.id` for "what tab is active?" checks; both are wired to the active `MainTabId` via `useRouteAwareness`.

**Batch** — use when a manual-test sequence would otherwise need 3+ round-trips:

- `POST /ui-bridge/control/batch-execute` — mixed `action` / `wait` / `snapshot` steps, serial. Body `{ "actions": [...] }` (alias: `steps`), with `stop_on_error` (aliases: `stopOnError`, `stopOnFailure`) default `true`. A body carrying **neither** array key is a `400 steps_missing` — an empty batch is legal but has to be asked for (`{"actions": []}`), because silently succeeding on a key mismatch is what let this endpoint probe green while broken.
- `POST /ui-bridge/control/batch` and `POST /ui-bridge/control/batch-actions` — pure action batches with timing and pre/post snapshot diff. `/control/batch` spells the same array `steps`.

**Tauri invoke proxy** — drop the `page/evaluate + window.__TAURI_INTERNALS__.invoke(...)` boilerplate:

- `GET /ui-bridge/commands` — lists every command the runner will proxy, with `args_schema` / `response_schema`.
- `POST /ui-bridge/invoke/{command_name}` — body `{ args }`. Calls an allowlisted Tauri command directly. If the command you need isn't allowlisted, add it to `UI_BRIDGE_COMMANDS` in `src-tauri/src/ui_bridge_invoke.rs` (threat-review gate: no filesystem paths, credentials, or PTY handles).

**Console errors** — targeted retrieval, not "dump everything":

- `GET /ui-bridge/control/console-errors?since=<epoch-ms-or-ISO>&limit=&group=true&groupBy=&level=` — `since` accepts both epoch-ms numeric and ISO-8601 strings. Use `group=true` for fingerprint-rolled buckets on long runs. `level=` accepts a comma-separated allow-list (`error`, `warn`, `unhandledrejection`, `info`, `log`, `debug`, or `all`/`*`) to drop info-level entries the SDK captures via `console.warn`. Default unset returns everything (back-compat).

**Side-effect detection on clicks** — read before re-discovering:

- `click` / `doubleClick` actions on non-input elements automatically run an `expectChange` DOM-diff detector. The action response includes `expectChange: { effectChanged: boolean, remounted: boolean, preElementCount, postElementCount, preSignature, postSignature, preGeneration, postGeneration, settleMs }`. Read that instead of immediately calling `/control/snapshot` again; it'll usually tell you whether the click produced the side effect you expected.
- **`remounted: true` means the subtree was DESTROYED and REBUILT** even though it looks identical (same elements, same text — element IDs are deliberately preserved across remounts by the SDK registry). Any state living in that subtree is gone: a wizard's current step, a form draft, scroll position. Do NOT read `effectChanged: false` as "the click did nothing" without checking `remounted` — a same-shape remount used to be completely invisible here, which is how a wizard that reset itself on every click read as an inert button for an entire investigation.

### Common Patterns

**Wait for element to appear:**

```python
import time

def wait_for_element(client, element_id, timeout=10.0):
    start = time.time()
    while time.time() - start < timeout:
        try:
            elem = client.get_element(element_id)
            if elem and elem.state.visible:
                return elem
        except ElementNotFoundError:
            pass
        time.sleep(0.5)
    raise TimeoutError(f"Element {element_id} not found after {timeout}s")
```

**Interact with dynamic lists:**

```python
# Find all items matching a pattern
elements = client.get_elements()
list_items = [e for e in elements if e.id.startswith("list-item-")]

# Click first matching item
for item in list_items:
    if "target-text" in item.state.text:
        client.click(item.id)
        break
```
