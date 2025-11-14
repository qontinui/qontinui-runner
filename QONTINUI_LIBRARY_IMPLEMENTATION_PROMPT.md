# Implementation Prompt for qontinui Library

## Context

The qontinui-runner now has WebSocket integration to stream real-time automation data to qontinui-web backend for monitoring and analysis. The runner forwards all events it receives from the qontinui library, including screenshots, keyboard input, mouse input, and action execution events.

## Current State

The qontinui library already emits the following events that the runner captures:

1. **MATCH_ATTEMPTED** - Image recognition events with screenshots
2. **TEXT_TYPED** - Keyboard typing events
3. **MOUSE_CLICKED** - Mouse click events
4. **ACTION_COMPLETED** - Action execution completion events

These events are captured by the runner's EventTranslator and forwarded to the WebSocket backend with timestamps.

## Requirements

Ensure the qontinui library properly emits all automation events during execution, including:

### 1. Screenshot Capture with Base64 Encoding

**Current Implementation:**
- Screenshots are sent as base64-encoded PNG images in event data
- Included in `MATCH_ATTEMPTED` events with key `screenshot_base64`
- Optional debug visuals with key `debug_visual_base64` or `visual_debug_image`

**Required:**
- ✅ **Already implemented** - Verify screenshots are base64-encoded PNG
- ✅ **Already implemented** - Include in MATCH_ATTEMPTED events
- Ensure screenshots are captured for ALL image recognition attempts (not just successful matches)
- Include both regular screenshot and debug visual (with match location overlay)

**Example Event Data:**
```python
{
    "type": "MATCH_ATTEMPTED",
    "data": {
        "image_id": "login_button",
        "screenshot_base64": "iVBORw0KGgoAAAANS...",  # Full screenshot as base64
        "debug_visual_base64": "iVBORw0KGgoAAAANS...",  # Annotated screenshot
        "found": True,
        "confidence": 0.95,
        "threshold": 0.8,
        "location": {"x": 850, "y": 450, "width": 100, "height": 40},
        "template_size": {"width": 100, "height": 40},
        "screenshot_dimensions": {"width": 1920, "height": 1080},
        "method": "template_matching",
        "timestamp": 1700000000.123  # Unix timestamp with milliseconds
    }
}
```

### 2. Keyboard Input Capture

**Current Implementation:**
- TEXT_TYPED events are emitted when text is typed
- Captured by runner's EventTranslator

**Required:**
- ✅ **Already implemented** - Emit TEXT_TYPED events for all typing actions
- Include the full text that was typed
- Include timing information (start time, duration if available)
- Include target element information (if available)

**Example Event Data:**
```python
{
    "type": "TEXT_TYPED",
    "data": {
        "text": "username@example.com",  # The text that was typed
        "length": 20,
        "action_id": "type_username_001",
        "success": True,
        "target": "input_field",  # Optional: what element received the text
        "timestamp": 1700000000.456,
        "duration_ms": 250  # Optional: how long it took to type
    }
}
```

### 3. Mouse Input Capture

**Current Implementation:**
- MOUSE_CLICKED events are emitted for mouse actions
- Currently used by runner's CaptureToolService for training data

**Required:**
- Emit MOUSE_CLICKED events for ALL mouse actions during automation execution
- Include click coordinates (x, y)
- Include button type (left, right, middle)
- Include target information (what was clicked)
- Include drag information for drag operations

**Example Event Data:**
```python
# Click event
{
    "type": "MOUSE_CLICKED",
    "data": {
        "x": 850,
        "y": 450,
        "button": "left",  # or "right", "middle"
        "click_type": "single",  # or "double"
        "target_type": "image_match",  # or "coordinates", "element"
        "target_id": "login_button",  # Optional: ID of what was clicked
        "timestamp": 1700000000.789
    }
}

# Drag event (if applicable)
{
    "type": "MOUSE_DRAGGED",
    "data": {
        "start_x": 100,
        "start_y": 200,
        "end_x": 500,
        "end_y": 300,
        "duration_ms": 500,
        "timestamp": 1700000001.123
    }
}
```

### 4. Action Execution Events

**Current Implementation:**
- ACTION_COMPLETED events are emitted after actions complete

**Required:**
- ✅ **Already implemented** - Emit ACTION_COMPLETED for all actions
- Include action type, success status, error messages
- Include execution duration
- Include state context (current states)

**Example Event Data:**
```python
{
    "type": "ACTION_COMPLETED",
    "data": {
        "action_type": "CLICK",
        "action_id": "click_login_001",
        "success": True,
        "error_message": None,  # or error string if failed
        "duration_ms": 1250,
        "state_name": "LoginScreen",
        "timestamp": 1700000002.456,
        "metadata": {
            "target": "login_button",
            "retry_count": 0
        }
    }
}
```

### 5. Timestamp Requirements

**All events MUST include:**
- `timestamp` field with Unix timestamp (seconds since epoch)
- Millisecond precision (e.g., 1700000000.123)
- UTC timezone

**Implementation:**
```python
import time

event_data = {
    "type": "EVENT_TYPE",
    "data": {
        # ... event-specific data ...
        "timestamp": time.time()  # Unix timestamp with milliseconds
    }
}
```

## Implementation Tasks

### Task 1: Verify Screenshot Capture
- [ ] Ensure base64 encoding is working correctly
- [ ] Verify screenshots are included in MATCH_ATTEMPTED events
- [ ] Test with different image sizes and formats
- [ ] Confirm debug visuals include match location overlays

### Task 2: Verify Keyboard Capture
- [ ] Ensure TEXT_TYPED events are emitted for all typing actions
- [ ] Include full text and metadata
- [ ] Test with special characters and multi-line text

### Task 3: Ensure Mouse Event Emission
- [ ] Emit MOUSE_CLICKED for all click actions (left, right, double)
- [ ] Include accurate coordinates and button information
- [ ] Emit MOUSE_DRAGGED for drag operations (if supported)
- [ ] Test with different screen resolutions

### Task 4: Add Timestamps to All Events
- [ ] Review all event emission points
- [ ] Add `timestamp: time.time()` to all event data
- [ ] Verify millisecond precision
- [ ] Ensure consistent UTC timezone

### Task 5: Testing
- [ ] Create test automation that performs all action types
- [ ] Verify all events are emitted with correct data
- [ ] Check that runner receives and forwards all events
- [ ] Validate with WebSocket backend

## Event Flow Architecture

```
qontinui Library (Action Execution)
    ↓
[Capture Screenshots - base64 encode]
[Capture Keyboard Input - text and timing]
[Capture Mouse Input - coordinates and button]
    ↓
Emit Events via qontinui.reporting
    ↓
qontinui-runner EventTranslator (callbacks registered)
    ↓
[Decode and process event data]
    ↓
Forward to WebSocket Backend (with timestamps)
    ↓
qontinui-web (store and associate data)
```

## Data Format Checklist

For each event type, ensure:

**MATCH_ATTEMPTED:**
- [x] image_id (string)
- [x] screenshot_base64 (base64 PNG)
- [ ] debug_visual_base64 (base64 PNG with overlay)
- [x] found (boolean)
- [x] confidence (float 0.0-1.0)
- [x] threshold (float 0.0-1.0)
- [x] location (dict with x, y, width, height)
- [x] template_size (dict with width, height)
- [x] screenshot_dimensions (dict with width, height)
- [ ] timestamp (float Unix epoch)

**TEXT_TYPED:**
- [x] text (string)
- [ ] length (int)
- [ ] action_id (string)
- [ ] success (boolean)
- [ ] timestamp (float Unix epoch)
- [ ] duration_ms (optional int)

**MOUSE_CLICKED:**
- [ ] x (int)
- [ ] y (int)
- [ ] button (string: "left", "right", "middle")
- [ ] click_type (string: "single", "double")
- [ ] target_type (string)
- [ ] target_id (optional string)
- [ ] timestamp (float Unix epoch)

**ACTION_COMPLETED:**
- [x] action_type (string)
- [ ] action_id (string)
- [ ] success (boolean)
- [ ] error_message (optional string)
- [ ] duration_ms (int)
- [ ] state_name (string)
- [ ] timestamp (float Unix epoch)

## Example Integration Test

Create a test automation workflow:

```python
# test_websocket_integration.py
from qontinui import Find, Image, Location
import time

def test_all_events():
    """Test that all event types are emitted correctly."""

    # 1. Image recognition (should emit MATCH_ATTEMPTED with screenshot)
    login_button = Image("login_button.png")
    result = Find(login_button).execute()

    # 2. Mouse click (should emit MOUSE_CLICKED)
    if result:
        result.click()

    # 3. Keyboard input (should emit TEXT_TYPED)
    username_field = Find(Image("username_field.png")).execute()
    if username_field:
        username_field.click()
        username_field.type("test@example.com")

    # Verify events were emitted with correct data
    # (Check runner logs and WebSocket backend)
```

## Notes

- All existing event emission code should continue to work
- The runner's EventTranslator handles event forwarding automatically
- No changes needed to event registration or callback system
- Focus on ensuring data completeness and timestamp accuracy

## Questions?

If you need clarification on:
- Event data format requirements
- Timestamp format
- Screenshot encoding
- Runner integration

See `/home/user/qontinui-runner/python-bridge/WEBSOCKET_INTEGRATION.md` for full documentation.

## Success Criteria

✅ All automation events are emitted with complete data
✅ Screenshots are base64-encoded and included in events
✅ Keyboard and mouse inputs are captured accurately
✅ All events include Unix timestamp with millisecond precision
✅ Events are received and forwarded by qontinui-runner
✅ WebSocket backend receives and can parse all events
