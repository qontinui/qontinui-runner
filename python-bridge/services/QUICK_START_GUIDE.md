# State Detection Service - Quick Start Guide

Get started with the State Detection Service in 5 minutes.

## Prerequisites

✅ Python 3.12+ installed
✅ qontinui library installed: `pip install -e /path/to/qontinui`
✅ OpenCV and NumPy (installed with qontinui)

## Basic Usage

### 1. Prepare Your Data

Your capture session should have:

```
captures/session1/
├── screenshot_1234567890000.png
├── screenshot_1234567890100.png
├── screenshot_1234567890200.png
└── events.json
```

**Events JSON format:**

```json
{
  "events": [
    {
      "timestamp": 1234567890.0,
      "event_type": "click",
      "x": 100,
      "y": 200,
      "button": "left"
    }
  ]
}
```

### 2. Run the Service

**Command line:**

```bash
python3 state_detection_service.py \
    ./captures/session1 \
    ./captures/session1/events.json \
    ./output/states.json
```

> **Note:** Use `python3` (not `python`) to ensure Python 3.x is used.

**Python API:**

```python
from state_detection_service import LocalStateDetectionService
from pathlib import Path

service = LocalStateDetectionService()
result = service.process_capture_session(
    Path('./captures/session1'),
    Path('./captures/session1/events.json'),
    Path('./output/states.json')
)

print(f"Detected {result['num_states']} states")
```

**TypeScript (qontinui-runner):**

```typescript
import { StateDetectionServiceClient } from "./typescript_integration_example";

const client = new StateDetectionServiceClient();
const result = await client.detectStates(
  "./captures/session1",
  "./captures/session1/events.json",
  "./output/states.json",
);

console.log(`Detected ${result.num_states} states`);
```

### 3. Use the Results

The output JSON contains detected states:

```json
{
  "num_states": 2,
  "states": [
    {
      "name": "main_menu",
      "state_images": [...],
      "state_regions": [...],
      "state_locations": [...]
    }
  ]
}
```

## Test the Service

Run the test script to verify everything works:

```bash
python3 test_state_detection.py
```

This creates sample data and tests the full pipeline.

## Common Issues

### Import Error

```
ImportError: No module named 'qontinui'
```

**Fix:** Install qontinui: `pip install -e /path/to/qontinui`

### No Screenshots Found

```
ValueError: No PNG files found
```

**Fix:** Ensure screenshots are PNG format and in the correct directory

### Timestamp Parsing

If screenshots aren't matched correctly, verify:

- Filenames: `screenshot_1234567890000.png` (timestamp in milliseconds)
- Events: `"timestamp": 1234567890.0` (timestamp in seconds)

## Next Steps

- Read the full [STATE_DETECTION_README.md](STATE_DETECTION_README.md)
- Check [typescript_integration_example.ts](typescript_integration_example.ts) for TypeScript integration
- Customize StateBuilder parameters for your use case

## File Structure

```
qontinui-runner/python-bridge/services/
├── state_detection_service.py           # Main service (566 lines)
├── test_state_detection.py             # Test script (185 lines)
├── STATE_DETECTION_README.md           # Full documentation (412 lines)
├── QUICK_START_GUIDE.md               # This file
└── typescript_integration_example.ts   # TypeScript integration
```

## Support

- Issues: https://github.com/qontinui/qontinui/issues
- Docs: https://qontinui.github.io

## Example Output

```
[StateDetectionService] Processing capture session: ./captures/session1
[StateDetectionService] Loading screenshots...
[StateDetectionService] Loaded 15 screenshots
[StateDetectionService] Loading input events...
[StateDetectionService] Loaded 8 events
[StateDetectionService] Grouping screenshots into transitions...
[StateDetectionService] Identified 7 transitions
[StateDetectionService] Building states with qontinui StateBuilder...
[StateDetectionService] Built 2 states
[StateDetectionService] Serializing states to JSON...
[StateDetectionService] Wrote results to ./output/states.json

================================================================================
STATE DETECTION COMPLETE
================================================================================
Detected States: 2
Transitions: 7
Output: ./output/states.json
================================================================================
```
