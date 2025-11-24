# State Detection Service

The State Detection Service is a Python bridge that enables qontinui-runner to perform local state detection using the qontinui library.

## Overview

This service processes captured screenshots and input events to automatically detect application states, including:

- **StateImages**: Persistent visual elements that identify the state
- **StateRegions**: Functional areas (grids, panels, clickable regions)
- **StateLocations**: Specific click points that trigger transitions
- **State boundaries**: Spatial extent of modal dialogs or popup windows

## Architecture

```
qontinui-runner (TypeScript)
    ↓
    Captures screenshots + events
    ↓
state_detection_service.py (Python)
    ↓
    Uses qontinui.discovery.state_construction.StateBuilder
    ↓
    Returns JSON with detected states
```

## Installation

### Prerequisites

1. Python 3.12+ with qontinui library installed:
   ```bash
   cd /path/to/qontinui
   pip install -e .
   ```

2. Required dependencies (should be installed with qontinui):
   - opencv-python-headless
   - numpy
   - Pillow

### Verify Installation

```bash
python -c "from qontinui.discovery.state_construction.state_builder import StateBuilder; print('OK')"
```

## Usage

### Command Line Interface

```bash
python state_detection_service.py <screenshots_dir> <events.json> <output.json>
```

**Arguments:**

- `screenshots_dir`: Directory containing PNG screenshots (named with timestamps)
- `events.json`: JSON file with input events (clicks, keypresses)
- `output.json`: Output file for detected states

**Example:**

```bash
python state_detection_service.py \
    ./captures/session1 \
    ./captures/session1/events.json \
    ./output/states.json
```

### Input Format

#### Screenshots

Screenshots should be PNG files named with timestamps:
```
captures/session1/
    screenshot_1234567890123.png
    screenshot_1234567890456.png
    screenshot_1234567890789.png
```

Timestamp format: milliseconds since epoch (e.g., `1234567890123`)

#### Events JSON

```json
{
  "events": [
    {
      "timestamp": 1234567890.123,
      "event_type": "click",
      "x": 100,
      "y": 200,
      "button": "left"
    },
    {
      "timestamp": 1234567890.456,
      "event_type": "key",
      "key": "Enter"
    }
  ]
}
```

**Event Fields:**

- `timestamp` (required): Event time in seconds (float)
- `event_type` (required): Type of event ("click", "key", "move")
- `x`, `y` (optional): Coordinates for click/move events
- `button` (optional): Mouse button ("left", "right", "middle")
- `key` (optional): Key name for keyboard events
- `metadata` (optional): Additional event data

### Output Format

```json
{
  "version": "1.0",
  "service": "LocalStateDetectionService",
  "screenshots_dir": "./captures/session1",
  "num_states": 2,
  "num_transitions": 5,
  "states": [
    {
      "name": "main_menu",
      "description": "Auto-generated state from 10 screenshots",
      "state_images": [
        {
          "name": "title_bar_logo",
          "similarity": 0.92,
          "bbox": [10, 10, 100, 40],
          "context": "logo"
        }
      ],
      "state_regions": [
        {
          "name": "menu_panel",
          "type": "panel",
          "bbox": [50, 100, 300, 400]
        }
      ],
      "state_locations": [
        {
          "name": "click_to_settings",
          "x": 150,
          "y": 250,
          "target_state": "settings",
          "confidence": 0.87
        }
      ],
      "boundary": null,
      "metadata": {
        "num_images": 1,
        "num_regions": 1,
        "num_locations": 1
      }
    }
  ]
}
```

## Integration with qontinui-runner

### TypeScript Integration

```typescript
import { exec } from 'child_process';
import { promisify } from 'util';

const execAsync = promisify(exec);

async function detectStates(
  screenshotsDir: string,
  eventsFile: string,
  outputFile: string
): Promise<any> {
  const pythonScript = path.join(
    __dirname,
    '../python-bridge/services/state_detection_service.py'
  );

  const command = `python ${pythonScript} ${screenshotsDir} ${eventsFile} ${outputFile}`;

  try {
    const { stdout, stderr } = await execAsync(command);

    console.log('State detection output:', stdout);
    if (stderr) {
      console.error('State detection errors:', stderr);
    }

    // Read the output file
    const statesData = JSON.parse(fs.readFileSync(outputFile, 'utf-8'));
    return statesData;

  } catch (error) {
    console.error('State detection failed:', error);
    throw error;
  }
}
```

## API Reference

### LocalStateDetectionService

Main service class for state detection.

#### `__init__(verbose: bool = True)`

Create a new state detection service.

**Parameters:**
- `verbose`: Enable progress logging to stderr

#### `process_capture_session(screenshots_dir, events_file, output_file) -> Dict`

Process a capture session to detect states.

**Parameters:**
- `screenshots_dir` (Path): Directory containing PNG screenshots
- `events_file` (Path): JSON file with input events
- `output_file` (Path): Where to write the output JSON

**Returns:**
- Dictionary with detected states and metadata

**Raises:**
- `FileNotFoundError`: If screenshots_dir or events_file doesn't exist
- `ValueError`: If data is invalid or corrupted

### Internal Methods

#### `_load_screenshots(screenshots_dir) -> Tuple[List[np.ndarray], List[Path]]`

Load all PNG screenshots from a directory.

#### `_load_events(events_file) -> List[InputEvent]`

Load input events from JSON file.

#### `_group_into_transitions(screenshots, screenshot_files, events) -> List`

Group screenshots and events into state transitions.

#### `_build_states(screenshots, transitions) -> List[Any]`

Build State objects using qontinui StateBuilder.

#### `_serialize_states(states, transitions, screenshots_dir) -> Dict`

Serialize State objects to JSON-compatible format.

## Configuration

### StateBuilder Parameters

The service uses these default parameters for the qontinui StateBuilder:

```python
builder = StateBuilder(
    consistency_threshold=0.85,  # Minimum consistency for StateImages (0-1)
    min_image_area=100,          # Minimum pixel area for detected images
    min_region_area=500,         # Minimum pixel area for detected regions
)
```

You can modify these in the `_build_states()` method.

## Error Handling

The service includes comprehensive error handling:

1. **Missing Files**: Clear error messages if screenshots or events files don't exist
2. **Invalid Data**: Validation of JSON structure and image formats
3. **Import Errors**: Automatic path setup for qontinui library
4. **Processing Errors**: Graceful handling with detailed error messages

## Progress Reporting

When `verbose=True` (default), the service logs progress to stderr:

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
```

## Advanced Usage

### Programmatic Usage

```python
from pathlib import Path
from state_detection_service import LocalStateDetectionService

# Create service
service = LocalStateDetectionService(verbose=True)

# Process session
result = service.process_capture_session(
    screenshots_dir=Path('./captures/session1'),
    events_file=Path('./captures/session1/events.json'),
    output_file=Path('./output/states.json')
)

# Access results
print(f"Detected {result['num_states']} states")
for state in result['states']:
    print(f"State: {state['name']}")
    print(f"  Images: {len(state['state_images'])}")
    print(f"  Regions: {len(state['state_regions'])}")
    print(f"  Locations: {len(state['state_locations'])}")
```

### Custom StateBuilder Configuration

```python
from qontinui.discovery.state_construction.state_builder import StateBuilder

# Create custom builder
builder = StateBuilder(
    consistency_threshold=0.9,   # Higher threshold for more precise matching
    min_image_area=200,          # Larger minimum image size
    min_region_area=1000,        # Larger minimum region size
)

# Build state
state = builder.build_state_from_screenshots(
    screenshot_sequence=screenshots,
    transitions_from_state=transitions,
)
```

## Troubleshooting

### Import Error: Cannot find qontinui

**Problem:** `ImportError: No module named 'qontinui'`

**Solution:**
1. Install qontinui: `pip install -e /path/to/qontinui`
2. Or add to PYTHONPATH: `export PYTHONPATH=/path/to/qontinui/src:$PYTHONPATH`

### No screenshots found

**Problem:** `ValueError: No PNG files found in directory`

**Solution:**
- Verify screenshot files are PNG format
- Check directory path is correct
- Ensure files have `.png` extension (case-sensitive)

### Timestamp parsing errors

**Problem:** Screenshots not matched to events properly

**Solution:**
- Ensure screenshot filenames include timestamps: `screenshot_1234567890123.png`
- Verify timestamp format is milliseconds since epoch
- Check that event timestamps are in seconds (float)

### Memory issues with large captures

**Problem:** Out of memory errors with many screenshots

**Solution:**
- Process captures in smaller batches
- Reduce image resolution before processing
- Use fewer screenshots per state (representative samples)

## Performance

**Typical Performance:**
- Loading: ~100 screenshots/second
- State building: ~5-10 seconds per state
- Serialization: <1 second

**Scaling:**
- 10-20 screenshots per state: Optimal
- 50+ screenshots per state: Slower but more accurate
- 100+ screenshots: Consider sampling

## Future Enhancements

Planned improvements:

1. **Multi-state detection**: Automatically cluster screenshots into multiple states
2. **State transitions**: Build full transition graph between states
3. **Incremental processing**: Update existing state models with new captures
4. **Real-time detection**: Process screenshots as they're captured
5. **ML-based clustering**: Use visual similarity for better state grouping

## License

MIT License - See main qontinui LICENSE file

## Support

For issues or questions:
- GitHub Issues: https://github.com/qontinui/qontinui
- Documentation: https://qontinui.github.io
