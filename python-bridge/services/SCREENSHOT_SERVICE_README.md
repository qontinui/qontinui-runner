# ScreenshotService

A clean, filesystem-safe service for managing screenshot lifecycle in the qontinui-runner Python bridge.

## Overview

The `ScreenshotService` class follows the Single Responsibility Principle (SRP) by focusing exclusively on screenshot storage, retrieval, and cleanup operations. It does not handle action execution, event emission, or state management - those concerns are delegated to their respective services.

## Features

- **Sequential Numbering**: Screenshots use predictable naming (screenshot-0001.png, screenshot-0002.png, etc.)
- **Metadata Sidecar Files**: Each screenshot has a corresponding JSON file with contextual information
- **Debug Visuals**: Support for annotated debug images with match information
- **Retention Policy**: Configurable cleanup to prevent unbounded storage growth
- **Disable-Friendly**: Can be disabled without errors (all methods return None)
- **Filesystem-Safe**: Proper path handling and filename sanitization

## Directory Structure

```
storage_dir/
├── screenshots/
│   ├── screenshot-0001.png
│   ├── screenshot-0001.json  (metadata)
│   ├── screenshot-0002.png
│   ├── screenshot-0002.json
│   └── ...
└── debug_visuals/
    ├── debug-0001-action-123.png
    ├── debug-0001-action-123.json  (match info)
    └── ...
```

## Usage Examples

### Basic Usage

```python
from pathlib import Path
from services.screenshot_service import ScreenshotService

# Initialize service
storage_dir = Path("/path/to/screenshots")
service = ScreenshotService(storage_dir, enabled=True)

# Store a screenshot
with open("screen.png", "rb") as f:
    screenshot_data = f.read()

reference = service.store_screenshot(
    screenshot_data=screenshot_data,
    action_id="click-login-button",
    active_states=["Login", "MainMenu"],
    metadata={
        "action_type": "CLICK",
        "success": True,
        "timestamp": "2025-11-01T12:34:56"
    }
)
# Returns: "screenshots/screenshot-0001.png"
```

### Store Debug Visual

```python
# Store an annotated debug image
with open("debug_annotated.png", "rb") as f:
    debug_data = f.read()

match_info = {
    "confidence": 0.95,
    "threshold": 0.8,
    "location": {"x": 100, "y": 200},
    "template_name": "company_logo.png",
    "found": True
}

debug_ref = service.store_debug_visual(
    debug_image_data=debug_data,
    action_id="find-logo-456",
    match_info=match_info
)
# Returns: "debug_visuals/debug-0001-action-find-logo-456.png"
```

### Retrieve Screenshot

```python
# Get screenshot by reference
image_data = service.get_screenshot("screenshots/screenshot-0001.png")

if image_data:
    with open("retrieved.png", "wb") as f:
        f.write(image_data)
```

### Cleanup Old Screenshots

```python
# Keep only the 100 most recent screenshots
deleted_count = service.cleanup_old_screenshots(keep_last_n=100)
print(f"Cleaned up {deleted_count} old screenshots")
```

### Disabled Service

```python
# Create a disabled service for testing or when screenshots aren't needed
service = ScreenshotService(storage_dir, enabled=False)

# All methods return None without performing I/O
ref = service.store_screenshot(data, "action-id", ["State"])  # Returns None
```

## Metadata Format

### Screenshot Metadata (`screenshot-XXXX.json`)

```json
{
  "screenshot_number": 1,
  "action_id": "click-login-button",
  "active_states": ["Login", "MainMenu"],
  "timestamp": "2025-11-01T12:34:56.789123",
  "filename": "screenshot-0001.png",
  "action_type": "CLICK",
  "success": true
}
```

### Debug Visual Metadata (`debug-XXXX-action-XXX.json`)

```json
{
  "debug_number": 1,
  "action_id": "find-logo-456",
  "timestamp": "2025-11-01T12:35:01.234567",
  "filename": "debug-0001-action-find-logo-456.png",
  "match_info": {
    "confidence": 0.95,
    "threshold": 0.8,
    "location": {"x": 100, "y": 200},
    "template_name": "company_logo.png",
    "found": true
  }
}
```

## Integration with QontinuiExecutor

The service is designed to integrate with the existing `QontinuiExecutor` class:

```python
# In QontinuiExecutor.__init__
from pathlib import Path
from services.screenshot_service import ScreenshotService

# Initialize screenshot service based on configuration
screenshot_dir = self.config.get("settings", {}).get("screenshotDirectory")
screenshot_enabled = self.config.get("settings", {}).get("screenshotsEnabled", True)

if screenshot_dir:
    self.screenshot_service = ScreenshotService(
        storage_dir=Path(screenshot_dir),
        enabled=screenshot_enabled
    )
else:
    self.screenshot_service = None

# Use in action execution
def _execute_action(self, action_data):
    # ... action execution code ...

    if self.screenshot_service and should_take_screenshot:
        screenshot_data = capture_screenshot()  # Your existing screenshot capture
        ref = self.screenshot_service.store_screenshot(
            screenshot_data=screenshot_data,
            action_id=action_data["id"],
            active_states=self._get_active_states(),
            metadata={
                "action_type": action_data["type"],
                "success": success
            }
        )

        # Emit event with screenshot reference
        self._emit_event(EventType.SCREENSHOT_TAKEN, {"reference": ref})
```

## Testing

Run the test suite:

```bash
cd python-bridge
python test_screenshot_service.py
```

All tests should pass, verifying:
- Service initialization
- Screenshot storage
- Debug visual storage
- Screenshot retrieval
- Cleanup operations
- Disabled service behavior
- Filename sanitization

## Design Principles

1. **Single Responsibility**: Only handles screenshot lifecycle management
2. **Filesystem-Safe**: Proper path handling with pathlib.Path
3. **Sequential**: Predictable numbering for easy debugging
4. **Metadata-Rich**: Context stored alongside images
5. **Type-Safe**: Full type hints throughout
6. **Well-Documented**: Comprehensive docstrings
7. **Testable**: Easy to test in isolation
8. **Disable-Friendly**: Gracefully handles disabled state

## Future Enhancements

Potential improvements (not implemented):
- Compression support for storage savings
- Async file I/O for better performance
- Image format conversion (JPEG, WebP)
- Thumbnail generation
- Database integration for metadata querying
- Cloud storage backends (S3, Azure Blob)
