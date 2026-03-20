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

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/ui-bridge/control/elements` | List all elements |
| GET | `/ui-bridge/control/element/:id` | Get element details |
| GET | `/ui-bridge/control/element/:id/state` | Get element state |
| POST | `/ui-bridge/control/element/:id/action` | Execute action |
| GET | `/ui-bridge/control/components` | List all components |
| POST | `/ui-bridge/control/component/:id/action/:actionId` | Execute component action |
| POST | `/ui-bridge/control/find` | Find elements |
| GET | `/ui-bridge/control/snapshot` | Get UI snapshot |
| GET | `/health` | Health check |

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
