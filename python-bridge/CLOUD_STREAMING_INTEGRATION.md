# Cloud Streaming Integration

This document describes how the qontinui-runner streams automation results to qontinui-web in real-time.

## Architecture

```
qontinui-runner (Python)
    ↓ WebSocket
qontinui-web backend (FastAPI, port 8000)
    ↓ Database
PostgreSQL
```

## Components

### 1. WebSocket Client (`websocket_client.py`)

The `RunnerWebSocketClient` class manages the WebSocket connection to qontinui-web:

- **Connection**: Establishes WebSocket connection with authentication
- **Session Management**: Starts/ends automation sessions
- **Event Streaming**: Sends logs, screenshots, and test events
- **Heartbeat**: Keeps connection alive
- **Auto-reconnect**: Handles connection failures

### 2. WebSocket Handler (`websocket_handler.py`)

The `WebSocketHandler` class provides a higher-level interface:

- **Configuration**: Manages connection settings
- **Event Loop**: Runs WebSocket operations in background thread
- **Non-blocking**: Sends events without blocking automation
- **Command Handling**: Receives commands from web app

### 3. Main Executor (`qontinui_executor.py`)

The `QontinuiExecutor` integrates WebSocket streaming:

- **Event Forwarding**: Automatically forwards events to WebSocket
- **Session Lifecycle**: Starts session on workflow start, ends on completion
- **Helper Methods**: Provides convenient methods for emitting test events

## Configuration

### Environment Variables

```bash
# Enable WebSocket streaming
export QONTINUI_WS_ENABLED=true

# WebSocket server URL (main backend, NOT qontinui-api)
export QONTINUI_WS_URL=ws://localhost:8000

# Authentication
export QONTINUI_WS_TOKEN=<jwt_token>
# OR
export QONTINUI_WS_EMAIL=user@example.com
export QONTINUI_WS_PASSWORD=password

# Project association
export QONTINUI_WS_PROJECT_ID=<project_uuid>

# Optional: Custom runner name
export QONTINUI_RUNNER_NAME="My Laptop"
```

### Runtime Configuration (from Rust)

```rust
use crate::commands::websocket::WebSocketConfig;

let config = WebSocketConfig {
    enabled: true,
    url: "ws://localhost:8000".to_string(),
    token: jwt_token.clone(),
    project_id: Some(project_id.clone()),
    runner_name: Some("My Laptop".to_string()),
};

configure_websocket(config, state)?;
connect_websocket(state)?;
```

## Event Types

### Standard Automation Events

These are automatically forwarded to WebSocket when emitted:

- `action_started` - Action execution begins
- `action_completed` - Action execution ends
- `action_execution` - Action executed
- `image_recognition` - Pattern matching result
- `match_found` - Image found on screen

### Test Execution Events

Use these methods for streaming test execution to web:

```python
# Step started
executor.emit_step_started(
    step_name="Login to application",
    step_type="action",
    metadata={"action_id": "click_login_button"}
)

# Step completed
executor.emit_step_completed(
    step_name="Login to application",
    success=True,
    step_type="action",
    metadata={"duration_ms": 1234}
)

# Screenshot captured
executor.emit_screenshot_captured(
    screenshot_base64=base64_image_data,
    step_name="After login",
    metadata={"state": "logged_in"}
)

# Error detected
executor.emit_error_detected(
    error_message="Login button not found",
    error_type="element_not_found",
    screenshot_base64=base64_image_data,
    metadata={"expected_state": "login_page"}
)
```

## Usage Example

### In Workflow Execution

```python
from qontinui_executor import QontinuiExecutor

executor = QontinuiExecutor()

# Load configuration
executor.load_configuration("config.json")

# Configure WebSocket
executor.websocket_handler.configure(
    enabled=True,
    api_url="ws://localhost:8000",
    token=jwt_token,
    project_id=project_uuid,
    runner_name="My Laptop"
)

# Connect
executor.websocket_handler.connect()

# Start workflow (automatically starts WebSocket session)
executor.start_execution(workflow_id="my_workflow")

# During execution, emit test events
executor.emit_step_started("Login", step_type="workflow")
try:
    # ... execute login workflow ...
    executor.emit_step_completed("Login", success=True)
except Exception as e:
    executor.emit_error_detected(
        error_message=str(e),
        error_type="execution_error"
    )
    executor.emit_step_completed("Login", success=False, error=str(e))

# Stop workflow (automatically ends WebSocket session)
executor.stop_execution()

# Disconnect
executor.websocket_handler.disconnect()
```

### In Action Execution

```python
def execute_custom_action(executor, action_config):
    """Execute a custom action with WebSocket streaming."""

    action_name = action_config.get("name", "Custom Action")

    # Emit step started
    executor.emit_step_started(
        step_name=action_name,
        step_type="action",
        metadata={"config": action_config}
    )

    try:
        # Execute action
        result = do_something()

        # Capture screenshot
        screenshot = capture_screen()
        executor.emit_screenshot_captured(
            screenshot_base64=screenshot,
            step_name=action_name,
            metadata={"result": result}
        )

        # Emit completion
        executor.emit_step_completed(
            step_name=action_name,
            success=True,
            metadata={"result": result}
        )

    except Exception as e:
        # Capture error screenshot
        screenshot = capture_screen()

        # Emit error
        executor.emit_error_detected(
            error_message=str(e),
            error_type="action_failure",
            screenshot_base64=screenshot,
            metadata={"action": action_name}
        )

        # Emit completion with error
        executor.emit_step_completed(
            step_name=action_name,
            success=False,
            error=str(e)
        )

        raise
```

## WebSocket Message Format

### Log Message

```json
{
  "type": "log",
  "session_id": "session_uuid",
  "level": "info",
  "message": "Step started: Login",
  "log_data": {
    "event_type": "step_started",
    "step_name": "Login",
    "step_type": "workflow",
    "timestamp": 1234567890.123
  },
  "sequence_number": 42,
  "timestamp": "2024-01-15T10:30:00.000Z"
}
```

### Screenshot Message

```json
{
  "type": "screenshot",
  "session_id": "session_uuid",
  "screenshot_data": "base64_encoded_image_data",
  "name": "step_Login_1234567890",
  "width": 1920,
  "height": 1080,
  "content_type": "image/png",
  "automation_metadata": {
    "event_type": "screenshot_captured",
    "step_name": "Login",
    "timestamp": 1234567890.123
  },
  "timestamp": "2024-01-15T10:30:00.000Z"
}
```

## Backend Integration

The qontinui-web backend receives these WebSocket messages and:

1. **Stores logs** in `automation_session_logs` table
2. **Stores screenshots** in S3/MinIO via `automation_session_screenshots` table
3. **Broadcasts events** to connected frontend clients
4. **Updates session status** in `automation_sessions` table

See `qontinui-web/backend/routers/automation/websocket.py` for the server-side implementation.

## Frontend Integration

The qontinui-web frontend can:

1. **View live logs** in real-time
2. **See screenshots** as they're captured
3. **Monitor execution progress** with step tracking
4. **Detect errors** immediately with error events
5. **Send commands** to the runner (pause, stop, etc.)

## Debugging

### Check WebSocket Connection

```python
# Python side
executor.websocket_handler.is_connected  # Should be True
executor.websocket_handler.websocket_handler.ws_client.get_stats()
```

### Enable Debug Logging

```python
import logging
logging.basicConfig(level=logging.DEBUG)
```

### Check Logs

- **Python logs**: `.dev-logs/runner-backend.log`
- **WebSocket debug**: `.dev-logs/python-ws-debug.log`
- **Backend logs**: `.dev-logs/web-backend.log`

### Common Issues

**Issue**: Connection fails with 401 Unauthorized
- **Cause**: Invalid or expired JWT token
- **Fix**: Refresh token or re-authenticate

**Issue**: Session not created
- **Cause**: Project ID not provided or invalid
- **Fix**: Ensure project_id is a valid UUID

**Issue**: Screenshots not appearing
- **Cause**: Base64 encoding issue or size limit
- **Fix**: Check image size (max 10MB), verify base64 encoding

**Issue**: Events not streaming
- **Cause**: WebSocket session not started
- **Fix**: Call `websocket_handler.start_session()` before emitting events

## Best Practices

1. **Start session early**: Call `start_session()` before executing workflows
2. **End session always**: Use try/finally to ensure `end_session()` is called
3. **Batch screenshots**: Don't send screenshot for every action (too much data)
4. **Use metadata**: Include useful context in event metadata
5. **Handle errors gracefully**: Always emit error events for debugging
6. **Test locally first**: Verify WebSocket connection with localhost before deploying
7. **Monitor session status**: Check heartbeat and connection status periodically

## Advanced Features

### Test Execution Tracking

For test execution with transitions:

```python
# Send transition started
websocket_handler.send_transition_started(
    sequence_number=1,
    from_state="login_page",
    to_state="dashboard",
    transition_name="Login Flow"
)

# Send transition completed
websocket_handler.send_transition_completed(
    sequence_number=1,
    from_state="login_page",
    to_state="dashboard",
    transition_name="Login Flow",
    status="success",
    duration_ms=5000,
    screenshot_id=screenshot_id
)

# Send deficiency report
websocket_handler.send_deficiency(
    title="Login button misaligned",
    description="The login button is 5px too far to the right",
    severity="low",
    deficiency_type="ui_issue",
    transition_sequence_number=1,
    state="login_page",
    screenshot_ids=[screenshot_id],
    reproduction_steps=[
        "Navigate to login page",
        "Observe button position"
    ]
)
```

### Custom Runner Name

Set a custom name for your runner to identify it in the web UI:

```python
websocket_handler.configure(
    enabled=True,
    api_url="ws://localhost:8000",
    token=jwt_token,
    project_id=project_uuid,
    runner_name="Josh's MacBook Pro"  # Custom name
)
```

## Future Enhancements

Planned features for cloud streaming:

1. **Bidirectional control**: Pause/resume workflows from web UI
2. **Live video streaming**: Stream screen recording in real-time
3. **Remote debugging**: Inspect state and variables from web UI
4. **Distributed execution**: Run workflows across multiple runners
5. **Analytics**: Aggregate metrics across all test runs
