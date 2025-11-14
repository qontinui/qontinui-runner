# Implementation Prompt for qontinui-web Backend

## Context

The qontinui-runner now streams real-time automation data via WebSocket, including:
- Screenshots (base64-encoded PNG images)
- Keyboard input events (text typed)
- Mouse input events (clicks, drags)
- Image recognition events (match attempts with confidence)
- Action execution events (completion status, timing)

All events include timestamps in ISO 8601 UTC format. The web backend needs to receive, store, and associate this data for monitoring and analysis.

## Current WebSocket Implementation

**Endpoint:** `ws://localhost:8001/api/v1/automation/ws/runner?token={JWT_TOKEN}`

**Already Implemented (from AUTOMATION_RUNNER_INTEGRATION.md):**
- WebSocket connection handler
- Session management (start/end)
- Heartbeat mechanism
- Basic message routing

**Runner sends these message types:**
1. `session_start` - Start of automation session
2. `session_end` - End of automation session (with status)
3. `screenshot` - Screenshot upload with metadata
4. `log` - Structured log entry
5. `heartbeat` - Keep-alive ping

## Requirements

### 1. Receive and Store Automation Events

The runner sends all automation events as **structured log messages** via the `log` message type.

**Message Format:**
```json
{
  "type": "log",
  "session_id": "uuid-of-session",
  "level": "info",
  "message": "Action completed: CLICK",
  "log_data": {
    "event_type": "action_completed",
    "action_type": "CLICK",
    "action_id": "click_001",
    "success": true,
    "duration_ms": 1250,
    "state_name": "LoginScreen",
    // ... additional event-specific data
  },
  "sequence_number": 42,
  "timestamp": "2024-11-14T12:00:05.123Z"
}
```

**Implementation Tasks:**

#### Task 1.1: Store Log Entries in Database
- Create `AutomationLog` model/table to store log entries
- Schema should include:
  - `id` (UUID primary key)
  - `session_id` (foreign key to AutomationSession)
  - `sequence_number` (integer for ordering)
  - `level` (string: debug, info, warning, error, critical)
  - `message` (text)
  - `log_data` (JSON field for structured data)
  - `timestamp` (datetime)
  - `created_at` (datetime)

**Example SQL Schema:**
```sql
CREATE TABLE automation_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES automation_sessions(id) ON DELETE CASCADE,
    sequence_number INTEGER NOT NULL,
    level VARCHAR(20) NOT NULL,
    message TEXT NOT NULL,
    log_data JSONB,
    timestamp TIMESTAMP WITH TIME ZONE NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    INDEX idx_session_sequence (session_id, sequence_number),
    INDEX idx_timestamp (timestamp),
    INDEX idx_event_type ((log_data->>'event_type'))
);
```

#### Task 1.2: WebSocket Message Handler
Update the WebSocket handler to save log messages:

```python
async def handle_log_message(websocket, message: dict):
    """Handle log message from runner."""
    session_id = message.get("session_id")

    # Validate session exists and is active
    session = await get_active_session(session_id)
    if not session:
        await send_error(websocket, "Invalid or inactive session")
        return

    # Create log entry
    log_entry = AutomationLog(
        session_id=session_id,
        sequence_number=message.get("sequence_number"),
        level=message.get("level"),
        message=message.get("message"),
        log_data=message.get("log_data", {}),
        timestamp=parse_iso_timestamp(message.get("timestamp"))
    )

    await db.save(log_entry)

    # Send acknowledgment (optional)
    await send_response(websocket, {"success": True})
```

### 2. Receive and Store Screenshots

Screenshots are sent via the `screenshot` message type with base64-encoded image data.

**Message Format:**
```json
{
  "type": "screenshot",
  "session_id": "uuid-of-session",
  "screenshot_data": "iVBORw0KGgoAAAANS...",
  "name": "match_login_button_1700000000",
  "width": 1920,
  "height": 1080,
  "content_type": "image/png",
  "automation_metadata": {
    "event_type": "image_recognition",
    "image_id": "login_button",
    "state_name": "LoginScreen",
    "found": true,
    "confidence": 0.95,
    "threshold": 0.8,
    "match_location": {"x": 850, "y": 450},
    "action_type": "find_image"
  },
  "timestamp": "2024-11-14T12:00:05.456Z"
}
```

**Implementation Tasks:**

#### Task 2.1: Decode and Store Screenshot
```python
import base64
from io import BytesIO

async def handle_screenshot_message(websocket, message: dict):
    """Handle screenshot upload from runner."""
    session_id = message.get("session_id")

    # Validate session
    session = await get_active_session(session_id)
    if not session:
        await send_error(websocket, "Invalid or inactive session")
        return

    # Decode base64 image data
    screenshot_data_base64 = message.get("screenshot_data")
    try:
        screenshot_bytes = base64.b64decode(screenshot_data_base64)
    except Exception as e:
        await send_error(websocket, f"Invalid base64 data: {e}")
        return

    # Upload to storage (S3, local filesystem, etc.)
    storage_path = await upload_to_storage(
        data=screenshot_bytes,
        filename=f"{session_id}/{message.get('name')}.png",
        content_type=message.get("content_type", "image/png")
    )

    # Create screenshot record
    screenshot = AutomationScreenshot(
        id=generate_uuid(),
        session_id=session_id,
        name=message.get("name"),
        storage_path=storage_path,
        width=message.get("width"),
        height=message.get("height"),
        content_type=message.get("content_type"),
        automation_metadata=message.get("automation_metadata", {}),
        timestamp=parse_iso_timestamp(message.get("timestamp")),
        presigned_url=generate_presigned_url(storage_path)
    )

    await db.save(screenshot)

    # Send response with screenshot ID
    await send_response(websocket, {
        "success": True,
        "message": "Screenshot uploaded successfully",
        "data": {
            "screenshot_id": str(screenshot.id),
            "presigned_url": screenshot.presigned_url
        }
    })
```

### 3. Associate Input Logs with Screenshots

The web backend is responsible for associating keyboard/mouse input events with their corresponding screenshots based on timestamps and event context.

**Association Logic:**

Screenshots are captured during image recognition (MATCH_ATTEMPTED events). Input events (TEXT_TYPED, MOUSE_CLICKED) that occur shortly after a screenshot should be associated with that screenshot.

**Implementation:**

#### Task 3.1: Create Input Event Tracking
Extract input events from log entries and create associations:

```python
async def process_log_for_input_events(log_entry: AutomationLog):
    """Process log entry and create input-screenshot associations."""
    event_type = log_entry.log_data.get("event_type")

    # Check if this is an input event
    if event_type not in ["text_typed", "mouse_clicked", "mouse_dragged"]:
        return

    # Find nearest screenshot (within time window)
    screenshot = await find_nearest_screenshot(
        session_id=log_entry.session_id,
        timestamp=log_entry.timestamp,
        time_window_seconds=5  # Look for screenshots within 5 seconds
    )

    if screenshot:
        # Create association
        association = ScreenshotInputAssociation(
            screenshot_id=screenshot.id,
            log_id=log_entry.id,
            input_type=event_type,
            input_data=log_entry.log_data,
            timestamp_diff_ms=calculate_diff_ms(screenshot.timestamp, log_entry.timestamp)
        )
        await db.save(association)
```

#### Task 3.2: Query API for Associated Data
Create API endpoint to retrieve screenshots with associated inputs:

```python
@router.get("/api/v1/automation/screenshots/{screenshot_id}/inputs")
async def get_screenshot_inputs(screenshot_id: str):
    """Get all input events associated with a screenshot."""
    screenshot = await db.get(AutomationScreenshot, screenshot_id)

    # Get associated input logs
    associations = await db.query(
        ScreenshotInputAssociation
    ).filter(
        screenshot_id=screenshot_id
    ).join(
        AutomationLog
    ).all()

    return {
        "screenshot": screenshot,
        "inputs": [
            {
                "type": assoc.input_type,
                "data": assoc.input_data,
                "timestamp": assoc.log.timestamp,
                "time_offset_ms": assoc.timestamp_diff_ms
            }
            for assoc in associations
        ]
    }
```

### 4. Query and Analysis APIs

Provide APIs to query and analyze automation sessions.

#### Task 4.1: Session Timeline API
```python
@router.get("/api/v1/automation/sessions/{session_id}/timeline")
async def get_session_timeline(session_id: str):
    """Get chronological timeline of session events."""
    session = await db.get(AutomationSession, session_id)

    # Get all logs ordered by sequence
    logs = await db.query(AutomationLog).filter(
        session_id=session_id
    ).order_by("sequence_number").all()

    # Get all screenshots ordered by timestamp
    screenshots = await db.query(AutomationScreenshot).filter(
        session_id=session_id
    ).order_by("timestamp").all()

    # Merge into timeline
    timeline = merge_timeline(logs, screenshots)

    return {
        "session": session,
        "timeline": timeline
    }
```

#### Task 4.2: Image Recognition Analysis API
```python
@router.get("/api/v1/automation/sessions/{session_id}/image-recognition")
async def get_image_recognition_stats(session_id: str):
    """Get image recognition statistics for session."""

    # Query logs for image_recognition events
    recognition_events = await db.query(AutomationLog).filter(
        session_id=session_id,
        log_data__event_type="image_recognition"
    ).all()

    # Calculate statistics
    total_attempts = len(recognition_events)
    successful = sum(1 for e in recognition_events if e.log_data.get("found"))
    failed = total_attempts - successful
    avg_confidence = sum(e.log_data.get("confidence", 0) for e in recognition_events) / total_attempts if total_attempts > 0 else 0

    # Group by image_id
    by_image = {}
    for event in recognition_events:
        image_id = event.log_data.get("image_id")
        if image_id not in by_image:
            by_image[image_id] = {"attempts": 0, "successful": 0, "avg_confidence": 0}
        by_image[image_id]["attempts"] += 1
        if event.log_data.get("found"):
            by_image[image_id]["successful"] += 1
        by_image[image_id]["avg_confidence"] += event.log_data.get("confidence", 0)

    # Calculate averages
    for image_id in by_image:
        attempts = by_image[image_id]["attempts"]
        by_image[image_id]["avg_confidence"] /= attempts if attempts > 0 else 1

    return {
        "total_attempts": total_attempts,
        "successful": successful,
        "failed": failed,
        "success_rate": successful / total_attempts if total_attempts > 0 else 0,
        "avg_confidence": avg_confidence,
        "by_image": by_image
    }
```

### 5. Real-time Monitoring WebSocket

Create a monitoring WebSocket endpoint for live session viewing.

```python
@router.websocket("/api/v1/automation/sessions/{session_id}/monitor")
async def monitor_session(websocket: WebSocket, session_id: str):
    """WebSocket endpoint for monitoring active session."""
    await websocket.accept()

    # Subscribe to session events
    async with subscribe_to_session(session_id) as events:
        async for event in events:
            # Forward events to monitoring client
            await websocket.send_json({
                "type": event.type,
                "data": event.data,
                "timestamp": event.timestamp.isoformat()
            })
```

## Database Schema Summary

**AutomationSession** (already exists):
- id, project_id, runner_version, runner_os, runner_hostname, status, created_at, ended_at

**AutomationLog** (new):
- id, session_id, sequence_number, level, message, log_data (JSONB), timestamp, created_at
- Indexes: (session_id, sequence_number), timestamp, (log_data->>'event_type')

**AutomationScreenshot** (update existing):
- Add: automation_metadata (JSONB)
- Add: timestamp (datetime)
- Ensure: width, height, content_type, storage_path

**ScreenshotInputAssociation** (new):
- id, screenshot_id, log_id, input_type, input_data (JSONB), timestamp_diff_ms
- Indexes: screenshot_id, log_id

## Testing Checklist

- [ ] WebSocket receives and stores log messages
- [ ] Screenshots are decoded and uploaded to storage
- [ ] Screenshot records are created with metadata
- [ ] Input events are extracted from logs
- [ ] Input events are associated with nearest screenshots
- [ ] Timeline API returns chronological events
- [ ] Image recognition stats are calculated correctly
- [ ] Real-time monitoring WebSocket works
- [ ] Query performance is acceptable with large datasets

## Example Test Flow

1. **Runner starts session:**
   - Sends `session_start` → Backend creates session record

2. **Runner performs image recognition:**
   - Sends `screenshot` with image_recognition metadata → Backend stores image
   - Sends `log` with image_recognition event → Backend stores log

3. **Runner clicks on match:**
   - Sends `log` with mouse_clicked event → Backend stores log and associates with screenshot

4. **Runner types text:**
   - Sends `log` with text_typed event → Backend stores log and associates with screenshot

5. **Runner ends session:**
   - Sends `session_end` → Backend updates session status

6. **User queries timeline:**
   - GET `/api/v1/automation/sessions/{id}/timeline` → Returns all events in order

7. **User views screenshot with inputs:**
   - GET `/api/v1/automation/screenshots/{id}/inputs` → Returns screenshot and associated keyboard/mouse events

## Success Criteria

✅ All log messages are received and stored with correct data
✅ Screenshots are decoded, stored, and accessible
✅ Input events are correctly associated with screenshots based on timestamps
✅ Timeline API provides chronological view of session
✅ Image recognition statistics are accurate
✅ Real-time monitoring WebSocket streams live data
✅ Query APIs perform well with 1000+ events per session
✅ Frontend can visualize automation execution flow

## Notes

- Use database transactions for consistency
- Implement proper error handling for malformed messages
- Add rate limiting for WebSocket connections
- Consider pagination for large timeline queries
- Cache expensive aggregation queries
- Use background jobs for heavy association processing
- Implement WebSocket reconnection handling
- Add authentication/authorization for monitoring endpoints

## Questions?

See `/home/user/qontinui-runner/python-bridge/WEBSOCKET_INTEGRATION.md` for runner-side documentation and message protocol details.
