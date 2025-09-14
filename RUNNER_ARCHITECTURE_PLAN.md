# Qontinui Runner Architecture Plan

## Overview
The Qontinui Runner is a Tauri-based desktop application that executes automation configurations created in the web builder. It leverages the existing Qontinui Python library through a bridge architecture.

## Architecture Components

### 1. Core Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Tauri Frontend (UI)                   │
│  - Configuration Manager                                 │
│  - Execution Monitor                                     │
│  - Log Viewer                                           │
│  - State Visualizer                                     │
└─────────────────────────────────────────────────────────┘
                            │
                    IPC Communication
                            │
┌─────────────────────────────────────────────────────────┐
│                   Tauri Backend (Rust)                   │
│  - Configuration Loader                                  │
│  - Python Process Manager                                │
│  - Event Handler                                        │
│  - File System Manager                                  │
└─────────────────────────────────────────────────────────┘
                            │
                    Process Bridge
                            │
┌─────────────────────────────────────────────────────────┐
│              Python Qontinui Executor                    │
│  - JSONRunner (existing)                                │
│  - StateExecutor (existing)                             │
│  - ActionExecutor (existing)                            │
│  - Image Recognition (existing)                         │
└─────────────────────────────────────────────────────────┘
```

### 2. Component Details

#### A. Tauri Frontend (React/TypeScript)
- **Configuration Manager**
  - Load JSON from file or URL
  - Connect to web API to fetch configurations
  - Display configuration details
  - Validate before execution

- **Execution Monitor**
  - Real-time execution status
  - Current state display
  - Action progress indicators
  - Success/failure tracking

- **Log Viewer**
  - Real-time log streaming
  - Log filtering and search
  - Export logs
  - Screenshot capture on errors

- **State Visualizer**
  - Visual state machine diagram
  - Current state highlighting
  - Transition animations
  - Execution path history

#### B. Tauri Backend (Rust)
- **Configuration Loader**
  - Load JSON from local files
  - Fetch from web API with authentication
  - Cache configurations
  - Validate JSON schema

- **Python Process Manager**
  - Spawn Python subprocess for Qontinui
  - Manage process lifecycle
  - Handle stdin/stdout communication
  - Error handling and restart logic

- **Event Handler**
  - Process events from Python executor
  - Emit events to frontend
  - Handle user commands (start/stop/pause)
  - State change notifications

- **File System Manager**
  - Handle image storage
  - Manage configuration files
  - Screenshot capture
  - Log file management

#### C. Python Bridge Layer
- **Communication Protocol**
  - JSON-RPC over stdin/stdout
  - Event streaming for real-time updates
  - Binary data handling for images
  - Error propagation

- **Executor Wrapper**
  - Wrap existing JSONRunner
  - Add event emission hooks
  - Progress reporting
  - Screenshot capture on failures

### 3. Data Flow

```
1. User loads configuration (file/API)
   └─> Rust validates and caches
       └─> UI displays configuration

2. User starts execution
   └─> Rust spawns Python process
       └─> Python loads configuration
           └─> Begins state machine execution

3. During execution
   └─> Python emits events (state change, action, etc.)
       └─> Rust processes and forwards to UI
           └─> UI updates in real-time

4. On completion/error
   └─> Python sends final status
       └─> Rust captures logs/screenshots
           └─> UI shows results
```

### 4. Communication Protocol

#### Commands (UI → Python)
```json
{
  "type": "command",
  "id": "unique-id",
  "command": "start|stop|pause|resume",
  "params": {
    "config_path": "/path/to/config.json",
    "mode": "state_machine|sequential"
  }
}
```

#### Events (Python → UI)
```json
{
  "type": "event",
  "event": "state_changed|action_started|action_completed|error",
  "data": {
    "state_id": "state-1",
    "action_id": "action-1",
    "status": "success|failure",
    "message": "Details...",
    "screenshot": "base64..."
  }
}
```

### 5. Implementation Phases

#### Phase 1: Basic Runner (MVP)
- Load local JSON configuration
- Execute using existing JSONRunner
- Display execution logs
- Basic start/stop controls

#### Phase 2: Enhanced Monitoring
- Real-time state visualization
- Action progress tracking
- Screenshot capture
- Error reporting with context

#### Phase 3: Web Integration
- Connect to web API
- Authentication flow
- Download configurations
- Upload execution results

#### Phase 4: Advanced Features
- Pause/resume execution
- Step-through debugging
- Breakpoints
- Variable inspection
- Performance metrics

### 6. Technology Stack

#### Rust Backend
```toml
[dependencies]
tauri = "2.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1", features = ["full"] }
anyhow = "1.0"
base64 = "0.21"
reqwest = { version = "0.11", features = ["json"] }
```

#### Python Requirements
```txt
qontinui  # Existing library
python-json-rpc  # For communication
pillow  # Image handling
numpy  # Already in qontinui
opencv-python  # Already in qontinui
```

#### Frontend
```json
{
  "dependencies": {
    "react": "^18.2.0",
    "@tauri-apps/api": "^2.0.0",
    "react-flow-renderer": "^10.3.0",
    "monaco-editor": "^0.41.0",
    "antd": "^5.0.0"
  }
}
```

### 7. File Structure

```
qontinui-runner/
├── src-tauri/
│   ├── src/
│   │   ├── main.rs
│   │   ├── config/
│   │   │   ├── mod.rs
│   │   │   ├── loader.rs
│   │   │   └── validator.rs
│   │   ├── executor/
│   │   │   ├── mod.rs
│   │   │   ├── python_bridge.rs
│   │   │   └── event_handler.rs
│   │   ├── api/
│   │   │   ├── mod.rs
│   │   │   └── client.rs
│   │   └── commands.rs
│   └── Cargo.toml
├── src/
│   ├── App.tsx
│   ├── components/
│   │   ├── ConfigManager.tsx
│   │   ├── ExecutionMonitor.tsx
│   │   ├── LogViewer.tsx
│   │   └── StateVisualizer.tsx
│   ├── services/
│   │   ├── executor.ts
│   │   └── api.ts
│   └── types/
│       └── config.ts
├── python-bridge/
│   ├── __init__.py
│   ├── executor_wrapper.py
│   ├── event_emitter.py
│   └── requirements.txt
└── package.json
```

### 8. Key Design Decisions

1. **Python Process Bridge**: Rather than reimplementing Qontinui in Rust, we use the existing Python library through a subprocess bridge. This ensures compatibility and leverages existing code.

2. **Event-Driven Architecture**: All communication is event-based, allowing for real-time UI updates and responsive user experience.

3. **Configuration Caching**: Configurations are cached locally for offline use and faster loading.

4. **Modular Design**: Each component is independent, making it easy to extend and maintain.

5. **Progressive Enhancement**: Start with basic functionality and add features incrementally.

### 9. Security Considerations

- **Sandboxed Execution**: Python process runs in restricted environment
- **Input Validation**: All JSON configurations validated before execution
- **API Authentication**: Secure token storage for web API access
- **Screenshot Privacy**: Option to disable screenshot capture
- **Log Sanitization**: Remove sensitive data from logs

### 10. Performance Optimization

- **Lazy Loading**: Load images only when needed
- **Streaming Logs**: Use streaming for real-time log display
- **Image Caching**: Cache processed images for faster recognition
- **Parallel Processing**: Execute independent actions in parallel
- **Resource Management**: Limit memory usage and clean up resources

### 11. Error Handling Strategy

- **Graceful Degradation**: Continue execution when possible
- **Detailed Error Context**: Capture state, action, and screenshot on error
- **Retry Logic**: Configurable retry for failed actions
- **Recovery Options**: Allow user to skip, retry, or abort
- **Error Reporting**: Send error reports to web API for debugging

### 12. Testing Strategy

- **Unit Tests**: Test individual components
- **Integration Tests**: Test Python-Rust communication
- **E2E Tests**: Test complete execution flows
- **Performance Tests**: Measure execution speed and resource usage
- **Compatibility Tests**: Test with various JSON configurations

## Next Steps

1. **Set up Tauri project structure**
2. **Implement Python bridge wrapper**
3. **Create basic Rust command handlers**
4. **Build minimal UI for configuration loading**
5. **Test basic execution flow**
6. **Add real-time event streaming**
7. **Implement execution monitoring UI**
8. **Add error handling and recovery**
9. **Integrate with web API**
10. **Polish UI and add advanced features**

## Success Metrics

- Execute JSON configurations from web builder
- Real-time execution monitoring
- < 100ms latency for event updates
- 95% execution success rate
- Handle configurations up to 1000 states/actions
- Support offline execution
- Cross-platform compatibility (Windows, macOS, Linux)