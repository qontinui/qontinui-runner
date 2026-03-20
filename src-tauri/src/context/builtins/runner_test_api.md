## Runner Test API Reference

This document provides accurate context for generating Python tests that interact with the qontinui-runner via HTTP APIs.

### Base Configuration

```python
BASE_URL = "http://localhost:9876"
```

### Navigation API

**DO NOT click sidebar elements for navigation.** Use the dedicated navigation API:

```python
def navigate_to_page(page: str, select_run: int = None) -> dict:
    """Navigate to a page using the navigation API."""
    payload = {"page": page}
    if select_run is not None:
        payload["select_run"] = select_run
    response = requests.post(f"{BASE_URL}/navigate", json=payload, timeout=10)
    response.raise_for_status()
    return response.json()
```

**Valid page names:** `run-recap`, `run-dashboard`, `run`, `active`, `history`, `library`, `logs`, `ai`, `settings`, `test-builder`, `unified-workflow-builder`

### UI Bridge API

**Get All Elements:**

```python
def get_elements_by_id() -> dict[str, dict]:
    """Get elements indexed by their ID."""
    response = requests.get(f"{BASE_URL}/ui-bridge/control/elements", timeout=10)
    response.raise_for_status()
    elements = response.json().get("data", {}).get("elements", [])
    return {elem["id"]: elem for elem in elements}
```

**IMPORTANT:** Elements are returned as an ARRAY, not a dictionary. Convert to dict by ID.

**Response structure for each element:**

```json
{
  "id": "recap-status-banner",
  "element_type": "div",
  "state": {
    "visible": true,
    "enabled": true,
    "text_content": "Complete"
  }
}
```

**Click an Element:**

```python
def click_element(element_id: str) -> dict:
    response = requests.post(
        f"{BASE_URL}/ui-bridge/control/element/{element_id}/action",
        json={"action": "click"},
        timeout=10
    )
    response.raise_for_status()
    return response.json()
```

### Helper Functions

```python
def get_element_text(elements: dict, element_id: str) -> str:
    """Get text content of an element."""
    elem = elements.get(element_id, {})
    return elem.get("state", {}).get("text_content", "") or ""

def element_visible(elements: dict, element_id: str) -> bool:
    """Check if element exists and is visible."""
    elem = elements.get(element_id)
    return elem.get("state", {}).get("visible", False) if elem else False

def find_run_by_name(elements: dict, name: str) -> Optional[str]:
    """Find a run selector item by name (case-insensitive partial match)."""
    for elem_id, elem_data in elements.items():
        if elem_id.startswith("run-selector-item-"):
            text = elem_data.get("state", {}).get("text_content", "") or ""
            if name.lower() in text.lower():
                return elem_id
    return None

def wait_for_element(element_id: str, timeout: float = 10.0) -> bool:
    start = time.time()
    while time.time() - start < timeout:
        try:
            elements = get_elements_by_id()
            if element_visible(elements, element_id):
                return True
        except Exception:
            pass
        time.sleep(0.5)
    return False
```

### Element ID Reference

**Run Selector:**
| Element ID | Description | Data Attributes |
|------------|-------------|-----------------|
| `run-selector` | Main container | `data-selected-run-id`, `data-is-open` |
| `run-selector-trigger` | Dropdown trigger button | - |
| `run-selector-dropdown` | Dropdown menu (when open) | `data-run-count` |
| `run-selector-list` | Scrollable run list | - |
| `run-selector-item-{id}` | Individual run item | `data-run-id`, `data-run-status`, `data-run-name` |
| `run-selector-current-run` | "Current Run" option | - |
| `run-selector-clear` | Clear selection button | - |

**Recap Page:**
| Element ID | Description |
|------------|-------------|
| `recap-tab` | Main recap container |
| `recap-status-banner` | Status banner section |
| `recap-status-label` | Status text (e.g., "Complete", "Failed") |
| `recap-ai-summary-section` | AI summary container |
| `recap-ai-summary-text` | Summary text content |
| `recap-goal-badge` | Goal achieved badge |
| `recap-failure-section` | Failure details (if failed) |
| `recap-failure-reason` | Failure reason text |
| `recap-staged-timeline` | Timeline container |
| `recap-stage-section-{stage}` | Stage section (setup, verification, agentic, completion) |

### Common Patterns

**Navigate and Select Run:**

```python
# 1. Navigate to recap page
navigate_to_page("run-recap")
time.sleep(1.0)

# 2. Open run selector
click_element("run-selector-trigger")
time.sleep(0.5)

# 3. Get elements and find the run
elements = get_elements_by_id()
run_item_id = find_run_by_name(elements, "My Workflow Name")

# 4. Click the run item
if run_item_id:
    click_element(run_item_id)
    time.sleep(1.0)
```

**Verify Page State:**

```python
elements = get_elements_by_id()
assert element_visible(elements, "recap-status-banner"), "Status banner not visible"
status_text = get_element_text(elements, "recap-status-label")
assert "complete" in status_text.lower(), f"Expected complete, got: {status_text}"
```

### Common Mistakes to Avoid

1. **DON'T** click sidebar items for navigation - use `POST /navigate`
2. **DON'T** assume elements are returned as a dict - they're an array, convert first
3. **DON'T** search for text in element IDs - use `state.text_content`
4. **DON'T** forget to wait after navigation/clicks - UI needs time to update
5. **DON'T** use hard-coded run IDs - find runs by name dynamically
6. **DON'T** assume failure section exists - only present for failed runs

### Test Script Template

```python
import requests
import time
import json
from typing import Any, Optional

BASE_URL = "http://localhost:9876"
_results = {"passed": [], "failed": [], "logs": []}

def log(message: str) -> None:
    _results["logs"].append(message)
    print(message)

def assertion(condition: bool, message: str) -> None:
    if condition:
        _results["passed"].append(message)
        log(f"[PASS] {message}")
    else:
        _results["failed"].append(message)
        log(f"[FAIL] {message}")

def test_main() -> None:
    log("=" * 60)
    log("TEST: <Test Name>")
    log("=" * 60)

    # TODO: Implement test steps using the APIs above

    # Summary
    total = len(_results["passed"]) + len(_results["failed"])
    log(f"\nPassed: {len(_results['passed'])}/{total}")
    result = {
        "passed": len(_results["failed"]) == 0,
        "summary": f"{len(_results['passed'])}/{total} assertions passed",
        "details": _results
    }
    print("\n" + json.dumps(result, indent=2))

if __name__ == "__main__":
    test_main()
```
