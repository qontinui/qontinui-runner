# WebSocket Integration for qontinui-runner

## Overview

This document describes the WebSocket integration that enables real-time communication between qontinui-runner and qontinui-web backend. The integration provides:

- **Live screenshot streaming** during automation execution
- **Real-time log streaming** for debugging and monitoring
- **Session tracking** with heartbeats
- **Bidirectional communication** for future enhancements
- **Automatic reconnection** on connection loss
- **Graceful degradation** when WebSocket is disabled

## Architecture

### Components

1. **websocket_client.py** - WebSocket client implementation
   - Manages connection lifecycle
   - Handles authentication via JWT
   - Sends screenshots, logs, and heartbeats
   - Implements reconnection logic

2. **websocket_config.py** - Configuration management
   - Loads settings from environment variables or dictionary
   - Validates configuration
   - Supports multiple configuration sources

3. **qontinui_executor.py** (Modified) - Integration point
   - Initializes WebSocket client
   - Starts/ends sessions with execution
   - Forwards logs to WebSocket
   - Provides command handlers for WebSocket operations

### Communication Flow

```
┌─────────────────┐         WebSocket          ┌─────────────────┐
│                 │◄─────────────────────────►│                 │
│ qontinui-runner │    ws://host/api/v1/...   │  qontinui-web   │
│                 │                             │                 │
└─────────────────┘                             └─────────────────┘
        │                                               │
        │ 1. Authenticate (HTTP)                        │
        │──────────────────────────────────────────────►│
        │◄────────────── JWT Token ─────────────────────│
        │                                               │
        │ 2. Connect (WebSocket)                        │
        │──────────────────────────────────────────────►│
        │◄──────────── Connected ────────────────────────│
        │                                               │
        │ 3. Start Session                              │
        │──────────────────────────────────────────────►│
        │◄────────── Session ID ─────────────────────────│
        │                                               │
        │ 4. Send Screenshots & Logs                    │
        │──────────────────────────────────────────────►│
        │──────────────────────────────────────────────►│
        │──────────────────────────────────────────────►│
        │                                               │
        │ 5. Heartbeat (every 30s)                      │
        │──────────────────────────────────────────────►│
        │◄────────────── OK ──────────────────────────────│
        │                                               │
        │ 6. End Session                                │
        │──────────────────────────────────────────────►│
        │◄────────────── OK ──────────────────────────────│
        │                                               │
        │ 7. Disconnect                                 │
        │──────────────────────────────────────────────►│
```

## Configuration

### Environment Variables

Configure WebSocket via environment variables:

```bash
# Enable WebSocket integration
export QONTINUI_WS_ENABLED=true

# WebSocket server URL
export QONTINUI_WS_URL=ws://localhost:8001

# Authentication (option 1: email/password)
export QONTINUI_WS_EMAIL=user@example.com
export QONTINUI_WS_PASSWORD=your-password

# Authentication (option 2: JWT token)
export QONTINUI_WS_TOKEN=eyJhbGciOiJIUzI1NiIs...

# Project ID (required)
export QONTINUI_WS_PROJECT_ID=550e8400-e29b-41d4-a716-446655440000

# Optional settings
export QONTINUI_WS_HEARTBEAT_INTERVAL=30  # seconds
export QONTINUI_WS_AUTO_RECONNECT=true
export QONTINUI_WS_MAX_RECONNECT=5
export QONTINUI_RUNNER_VERSION=0.1.0
```

### Programmatic Configuration

Configure via command interface:

```json
{
  "type": "command",
  "command": "ws_configure",
  "params": {
    "config": {
      "enabled": true,
      "api_url": "ws://localhost:8001",
      "email": "user@example.com",
      "password": "password",
      "project_id": "550e8400-e29b-41d4-a716-446655440000",
      "heartbeat_interval": 30,
      "auto_reconnect": true,
      "max_reconnect_attempts": 5
    }
  }
}
```

## Usage

### Automatic Mode (Recommended)

When WebSocket is configured via environment variables and `QONTINUI_WS_ENABLED=true`, the integration works automatically:

1. **On startup**: Configuration is loaded from environment
2. **On execution start**: WebSocket session starts automatically
3. **During execution**: Logs are streamed in real-time
4. **On execution end**: Session ends automatically with status

No additional commands needed!

### Manual Mode

For more control, use the command interface:

#### 1. Configure WebSocket

```json
{
  "type": "command",
  "command": "ws_configure",
  "params": {
    "config": {
      "enabled": true,
      "api_url": "ws://localhost:8001",
      "token": "your-jwt-token",
      "project_id": "project-uuid"
    }
  }
}
```

#### 2. Connect to Server

```json
{
  "type": "command",
  "command": "ws_connect",
  "params": {}
}
```

Response:
```json
{
  "success": true
}
```

#### 3. Start Session

```json
{
  "type": "command",
  "command": "ws_start_session",
  "params": {
    "config_snapshot": {
      "workflow_id": "login_workflow",
      "version": "1.0.0"
    }
  }
}
```

Response:
```json
{
  "success": true
}
```

#### 4. Send Screenshot (Programmatic)

```python
# From within executor code
executor._ws_send_screenshot(
    image=pil_image,  # PIL Image object
    name="login_screen_001",
    metadata={
        "state_name": "LoginScreen",
        "action_type": "click",
        "execution_time_ms": 1250
    }
)
```

#### 5. Send Log (Automatic)

Logs are automatically sent via WebSocket when you use `_emit_log`:

```python
executor._emit_log("info", "Button clicked successfully")
```

This will:
- Emit to stdout (for Tauri frontend)
- Send to WebSocket (if enabled)

#### 6. End Session

```json
{
  "type": "command",
  "command": "ws_end_session",
  "params": {
    "status": "completed",
    "error": null
  }
}
```

#### 7. Check Status

```json
{
  "type": "command",
  "command": "ws_status",
  "params": {}
}
```

Response:
```json
{
  "success": true,
  "enabled": true,
  "connected": true,
  "stats": {
    "is_connected": true,
    "session_id": "session-uuid",
    "screenshots_sent": 42,
    "logs_sent": 156,
    "heartbeats_sent": 10,
    "reconnect_attempts": 0
  },
  "config": {
    "enabled": true,
    "api_url": "ws://localhost:8001",
    "project_id": "project-uuid",
    "heartbeat_interval": 30
  }
}
```

#### 8. Disconnect

```json
{
  "type": "command",
  "command": "ws_disconnect",
  "params": {}
}
```

## Message Protocol

### Session Start

**Sent by runner when session starts:**

```json
{
  "type": "session_start",
  "project_id": "uuid",
  "runner_version": "0.1.0",
  "runner_os": "Linux 5.15.0",
  "runner_hostname": "runner-01",
  "configuration_snapshot": {
    "workflow_id": "login_test",
    "workflows": [...]
  },
  "timestamp": "2024-11-14T12:00:00Z"
}
```

**Response from server:**

```json
{
  "type": "response",
  "success": true,
  "message": "Session started successfully",
  "data": {
    "session_id": "session-uuid"
  },
  "timestamp": "2024-11-14T12:00:00Z"
}
```

### Screenshot Upload

**Sent by runner:**

```json
{
  "type": "screenshot",
  "session_id": "session-uuid",
  "screenshot_data": "BASE64_ENCODED_PNG",
  "name": "login_screen_001",
  "width": 1920,
  "height": 1080,
  "content_type": "image/png",
  "automation_metadata": {
    "state_name": "LoginScreen",
    "action_type": "click",
    "execution_time_ms": 1250
  },
  "timestamp": "2024-11-14T12:00:05Z"
}
```

**Response:**

```json
{
  "type": "response",
  "success": true,
  "message": "Screenshot uploaded successfully",
  "data": {
    "screenshot_id": "screenshot-uuid",
    "presigned_url": "https://s3.amazonaws.com/..."
  },
  "timestamp": "2024-11-14T12:00:05Z"
}
```

### Log Entry

**Sent by runner:**

```json
{
  "type": "log",
  "session_id": "session-uuid",
  "level": "info",
  "message": "Successfully clicked login button",
  "log_data": {
    "action_type": "click",
    "state_name": "LoginScreen"
  },
  "sequence_number": 1,
  "timestamp": "2024-11-14T12:00:05Z"
}
```

Log levels: `debug`, `info`, `warning`, `error`, `critical`

### Heartbeat

**Sent every 30 seconds:**

```json
{
  "type": "heartbeat",
  "session_id": "session-uuid",
  "timestamp": "2024-11-14T12:00:30Z"
}
```

**Response:**

```json
{
  "type": "response",
  "success": true,
  "message": "Heartbeat received",
  "timestamp": "2024-11-14T12:00:30Z"
}
```

### Session End

**Sent by runner:**

```json
{
  "type": "session_end",
  "session_id": "session-uuid",
  "status": "completed",
  "error_message": null,
  "timestamp": "2024-11-14T12:05:00Z"
}
```

Status values: `completed`, `failed`

### Error Response

**Sent by server on errors:**

```json
{
  "type": "error",
  "error": "Failed to upload screenshot",
  "details": {
    "error": "Invalid image format"
  },
  "timestamp": "2024-11-14T12:00:05Z"
}
```

## Implementation Details

### Async Event Loop

WebSocket operations run in a separate background thread with its own asyncio event loop:

- **Main thread**: Runs stdin/stdout loop for Tauri communication
- **WebSocket thread**: Runs asyncio event loop for WebSocket operations
- **Communication**: Uses `asyncio.run_coroutine_threadsafe()` to schedule WebSocket operations

### Thread Safety

- All WebSocket operations are thread-safe
- Logs are sent non-blocking (fire-and-forget)
- Screenshots and sessions use futures with timeouts

### Error Handling

1. **Connection errors**: Logged but don't stop execution
2. **Authentication failures**: Logged and connection attempt fails
3. **Send errors**: Logged but automation continues
4. **Reconnection**: Automatic with exponential backoff (up to 5 attempts)

### Graceful Degradation

If WebSocket is disabled or fails:
- Automation continues normally
- Logs still go to stdout for Tauri frontend
- No functionality is lost
- Warnings are logged about WebSocket issues

## Testing

### Manual Testing with wscat

Install wscat:
```bash
npm install -g wscat
```

Connect to server:
```bash
wscat -c "ws://localhost:8001/api/v1/automation/ws/runner?token=YOUR_JWT_TOKEN"
```

Send test message:
```json
{"type": "heartbeat", "session_id": "test", "timestamp": "2024-11-14T12:00:00Z"}
```

### Integration Testing

1. **Start qontinui-web backend**:
   ```bash
   cd qontinui-web
   uvicorn app.main:app --host 0.0.0.0 --port 8001
   ```

2. **Configure environment**:
   ```bash
   export QONTINUI_WS_ENABLED=true
   export QONTINUI_WS_URL=ws://localhost:8001
   export QONTINUI_WS_EMAIL=test@example.com
   export QONTINUI_WS_PASSWORD=testpass
   export QONTINUI_WS_PROJECT_ID=your-project-uuid
   ```

3. **Run qontinui-runner**:
   ```bash
   cd qontinui-runner
   npm run tauri dev
   ```

4. **Load config and start execution**

5. **Verify in qontinui-web**:
   - Check sessions: `GET /api/v1/automation/sessions`
   - Check screenshots: `GET /api/v1/automation/screenshots`
   - Check logs: `GET /api/v1/automation/sessions/{session_id}/logs`

## Troubleshooting

### WebSocket not connecting

**Check configuration**:
```json
{
  "command": "ws_status"
}
```

**Common issues**:
- Wrong API URL (check protocol: `ws://` not `http://`)
- Invalid token or credentials
- qontinui-web not running
- Firewall blocking connection
- Missing project_id

### Authentication failing

**Test auth endpoint**:
```bash
curl -X POST http://localhost:8001/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email":"user@example.com","password":"password"}'
```

### Logs not appearing

**Check**:
- WebSocket connection status
- Session started successfully
- Log level configuration
- qontinui-web log handler

### Screenshots not uploading

**Check**:
- Image format (should be PNG or JPEG)
- Base64 encoding
- Size limits (check qontinui-web settings)
- Session active

## Performance Considerations

### Network Impact

- **Screenshots**: Can be large (1-5 MB each) - consider compression
- **Logs**: Small overhead (< 1 KB each)
- **Heartbeats**: Minimal (< 100 bytes every 30s)

### Optimization Tips

1. **Reduce screenshot frequency**: Only capture on important actions
2. **Compress images**: Use JPEG with quality=85 instead of PNG
3. **Batch logs**: Group related logs (future enhancement)
4. **Adjust heartbeat**: Increase interval for long-running automations

### Monitoring

Check WebSocket stats:
```json
{
  "command": "ws_status"
}
```

Returns:
- Total screenshots sent
- Total logs sent
- Heartbeats sent
- Reconnection attempts

## Security

### Authentication

- JWT tokens are required for WebSocket connections
- Tokens obtained via HTTP authentication endpoint
- Tokens expire (check qontinui-web token lifetime)

### Best Practices

1. **Use HTTPS/WSS in production**: `wss://` for encrypted WebSocket
2. **Rotate tokens**: Don't hardcode tokens, use credentials
3. **Environment variables**: Store credentials in env vars, not code
4. **Secure transmission**: Screenshots contain sensitive data

## Future Enhancements

Potential improvements:

1. **Screenshot compression**: Automatic compression before upload
2. **Batch operations**: Send multiple logs in one message
3. **Selective streaming**: Configure which events to stream
4. **Two-way control**: Receive commands from qontinui-web
5. **Progress updates**: Real-time execution progress
6. **Live metrics**: CPU, memory, execution speed

## API Reference

### WebSocket Client Methods

**`RunnerWebSocketClient.__init__()`**
```python
RunnerWebSocketClient(
    api_url: str,              # WebSocket server URL
    token: str,                # JWT authentication token
    project_id: str,           # Project UUID
    runner_version: str = "0.1.0",
    auto_reconnect: bool = True,
    heartbeat_interval: int = 30,
    max_reconnect_attempts: int = 5,
    on_connected: Callable = None,
    on_disconnected: Callable = None,
    on_error: Callable = None
)
```

**`async connect() -> bool`**
- Establish WebSocket connection
- Returns: True if successful

**`async disconnect()`**
- Close WebSocket connection gracefully

**`async start_session(config_snapshot: dict) -> bool`**
- Start automation session
- Returns: True if successful

**`async end_session(status: str, error: str) -> bool`**
- End automation session
- Status: "completed" or "failed"
- Returns: True if successful

**`async send_screenshot(image, name: str, metadata: dict) -> str`**
- Send screenshot to server
- Image: PIL Image, bytes, or base64 string
- Returns: Screenshot ID if successful

**`async send_log(level: str, message: str, log_data: dict) -> bool`**
- Send log entry
- Level: debug, info, warning, error, critical
- Returns: True if successful

**`get_stats() -> dict`**
- Get client statistics
- Returns: Dictionary with connection stats

### Configuration Class

**`WebSocketConfig.from_env() -> WebSocketConfig`**
- Load configuration from environment variables

**`WebSocketConfig.from_dict(data: dict) -> WebSocketConfig`**
- Load configuration from dictionary

**`validate() -> (bool, str)`**
- Validate configuration
- Returns: (is_valid, error_message)

### Executor Commands

| Command | Description |
|---------|-------------|
| `ws_configure` | Set WebSocket configuration |
| `ws_connect` | Connect to WebSocket server |
| `ws_disconnect` | Disconnect from server |
| `ws_start_session` | Start automation session |
| `ws_end_session` | End automation session |
| `ws_status` | Get connection status and stats |

## Support

For issues or questions:
- GitHub Issues: https://github.com/your-org/qontinui-runner/issues
- Documentation: See qontinui-web AUTOMATION_RUNNER_INTEGRATION.md
- API Reference: GET /docs on qontinui-web server
