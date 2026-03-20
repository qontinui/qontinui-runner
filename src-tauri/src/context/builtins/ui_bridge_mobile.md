## UI Bridge Control - qontinui-mobile

This context documents the UI Bridge elements available in the qontinui-mobile React Native app.

### Connection

```python
from ui_bridge import UIBridgeClient

# Android emulator (special IP for host access)
client = UIBridgeClient("http://10.0.2.2:8087")

# iOS simulator
client = UIBridgeClient("http://localhost:8087")

# Physical device (use device's IP on your network)
client = UIBridgeClient("http://192.168.1.100:8087")
```

**Note:** The server only runs in development mode (`__DEV__`).

### Available Elements

#### Run Cards

| Element ID         | Type      | Description                 |
| ------------------ | --------- | --------------------------- |
| `run-card-{runId}` | pressable | Individual run history card |

**Actions:** `click` (navigates to run details)

**State:** `label` contains workflow name and status

#### Finding Cards

| Element ID                 | Type      | Description             |
| -------------------------- | --------- | ----------------------- |
| `finding-card-{findingId}` | pressable | Individual finding card |

**Actions:** `click` (opens finding details)

**State:** `label` contains finding title and severity

#### Execution Controls

| Element ID             | Type   | Description             |
| ---------------------- | ------ | ----------------------- |
| `execution-stop-btn`   | button | Stop running workflow   |
| `execution-resume-btn` | button | Resume paused workflow  |
| `execution-force-btn`  | button | Force continue workflow |

**Component:** `execution-controls`

**Component Actions:**

- `stop` - Stop the current workflow
- `resume` - Resume a paused workflow
- `force-continue` - Force continue past a waiting point

```python
# Using component action (recommended)
client.execute_component_action("execution-controls", "stop")

# Using element directly
client.click("execution-stop-btn")
```

#### Quick Actions

| Element ID                 | Type   | Description           |
| -------------------------- | ------ | --------------------- |
| `quick-action-rerun-btn`   | button | Re-run last workflow  |
| `quick-action-reload-btn`  | button | Reload configuration  |
| `quick-action-monitor-btn` | button | Change target monitor |

**Component:** `quick-actions`

**Component Actions:**

- `rerun-last` - Re-run the last executed workflow
- `reload-config` - Reload the last loaded configuration

```python
# Re-run the last workflow
client.execute_component_action("quick-actions", "rerun-last")
```

#### Stats Cards (Dashboard)

| Element ID Pattern   | Type | Description             |
| -------------------- | ---- | ----------------------- |
| `stats-card-{label}` | view | Statistics display card |

**Examples:** `stats-card-total-runs`, `stats-card-success-rate`, `stats-card-active-workflows`

**State:** `label` contains the stat name and value

### Common Automation Patterns

**Check if a workflow is running:**

```python
elements = client.get_elements()
stop_btn = next((e for e in elements if e.id == "execution-stop-btn"), None)
is_running = stop_btn is not None and stop_btn.state.visible
```

**Stop a running workflow:**

```python
client.execute_component_action("execution-controls", "stop")
```

**Navigate to a specific run:**

```python
# Find the run card by workflow name
elements = client.get_elements()
for elem in elements:
    if elem.id.startswith("run-card-") and "MyWorkflow" in elem.label:
        client.click(elem.id)
        break
```

**Re-run the last workflow:**

```python
client.execute_component_action("quick-actions", "rerun-last")
```

**Wait for workflow completion:**

```python
import time

def wait_for_completion(client, timeout=300):
    start = time.time()
    while time.time() - start < timeout:
        elements = client.get_elements()
        resume_btn = next((e for e in elements if e.id == "execution-resume-btn"), None)
        stop_btn = next((e for e in elements if e.id == "execution-stop-btn"), None)

        # If resume is visible but stop isn't, workflow is paused/complete
        if resume_btn and resume_btn.state.visible and (not stop_btn or not stop_btn.state.visible):
            return "paused"
        # If neither is visible, no workflow is active
        if (not resume_btn or not resume_btn.state.visible) and (not stop_btn or not stop_btn.state.visible):
            return "completed"

        time.sleep(2)
    return "timeout"
```

### App Structure

```
qontinui-mobile/
├── app/
│   ├── _layout.tsx          # UIBridgeNativeProvider here
│   ├── (tabs)/               # Tab navigation
│   └── run/[id].tsx          # Run details screen
└── src/components/
    ├── runs/RunCard.tsx      # run-card-{id} elements
    ├── findings/FindingCard.tsx  # finding-card-{id} elements
    ├── workflow/
    │   ├── ExecutionControls.tsx  # execution-* elements
    │   └── QuickActions.tsx       # quick-action-* elements
    └── dashboard/StatsCard.tsx    # stats-card-* elements
```
