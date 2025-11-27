# StateImage Extraction Service - Integration Guide

This guide explains how to integrate the StateImage Extraction Service into the qontinui-runner workflow.

## Overview

The StateImage Extraction Service bridges the gap between captured screen data and the qontinui state detection system. It extracts identifying visual elements that help recognize and distinguish application states.

## Integration Points

### 1. State Detection Service Integration

The image extractor works downstream from the State Detection Service:

```python
# In state_detection_service.py

from analysis import StateImageExtractor, ImageExtractionConfig

class LocalStateDetectionService:
    def __init__(self):
        # ... existing initialization ...

        # Add image extractor
        self.image_extractor = StateImageExtractor(
            ImageExtractionConfig(
                min_size=(30, 30),
                max_size=(400, 400),
                extract_at_click_locations=True,
                click_region_padding=40,
            )
        )

    def _build_states(self, screenshots, transitions):
        """Build states and extract images."""
        states = []

        # Build state using StateBuilder
        state = self.StateBuilder.build_state_from_screenshots(...)

        # Convert to DetectedState
        detected_state = DetectedState(
            name=state.name,
            description=state.description,
            state_images=[],  # Will be populated below
            start_frame_index=0,
            end_frame_index=len(screenshots) - 1,
            frame_indices=list(range(len(screenshots))),
        )

        # Create Frame objects
        frames = [
            Frame(
                image=screenshot,
                timestamp=i * 1.0,
                frame_index=i,
            )
            for i, screenshot in enumerate(screenshots)
        ]

        # Extract state images
        events = self._convert_transitions_to_events(transitions)
        extracted_images = self.image_extractor.extract_from_state(
            state=detected_state,
            frames=frames,
            events=events,
        )

        # Add to state
        detected_state.state_images = extracted_images
        states.append(detected_state)

        return states
```

### 2. Capture Manager Integration

The capture manager should provide Frame objects:

```python
# In capture_manager.py

from models import Frame

class CaptureManager:
    def get_frames(self, session_id: str) -> List[Frame]:
        """Get frames for a capture session."""
        screenshots_dir = self.get_session_dir(session_id)
        frames = []

        for i, file_path in enumerate(sorted(screenshots_dir.glob("*.png"))):
            image = cv2.imread(str(file_path))
            timestamp = self._extract_timestamp(file_path)

            frame = Frame(
                image=image,
                timestamp=timestamp,
                frame_index=i,
                file_path=str(file_path),
                metadata={"session_id": session_id},
            )
            frames.append(frame)

        return frames
```

### 3. Event Manager Integration

The event manager should provide InputEvent objects:

```python
# In event_manager.py

from models import InputEvent

class EventManager:
    def get_events(self, session_id: str) -> List[InputEvent]:
        """Get input events for a capture session."""
        events_file = self.get_events_file(session_id)

        with open(events_file) as f:
            data = json.load(f)

        events = []
        for event_data in data.get("events", []):
            event = InputEvent(
                timestamp=event_data["timestamp"],
                event_type=event_data.get("event_type", "click"),
                x=event_data.get("x"),
                y=event_data.get("y"),
                button=event_data.get("button"),
                key=event_data.get("key"),
                metadata=event_data.get("metadata"),
            )
            events.append(event)

        return events
```

### 4. Training Export Integration

Export extracted images for training:

```python
# In training_export.py

from analysis import save_state_image
from pathlib import Path

def export_training_data(states: List[DetectedState], output_dir: Path):
    """Export state images for training."""
    for state in states:
        state_dir = output_dir / state.name

        # Export state images
        for state_image in state.state_images:
            save_state_image(state_image, state_dir)

        # Export metadata
        metadata = {
            "state": state.to_dict(),
            "images": [img.to_dict() for img in state.state_images],
        }

        metadata_file = state_dir / "metadata.json"
        with open(metadata_file, "w") as f:
            json.dump(metadata, f, indent=2)
```

## Complete Integration Example

Here's a complete example showing the full pipeline:

```python
#!/usr/bin/env python3
"""Complete pipeline example for state detection and image extraction."""

import json
from pathlib import Path
from typing import List

import cv2

from models import DetectedState, Frame, InputEvent
from analysis import StateImageExtractor, ImageExtractionConfig, save_state_image


def process_capture_session(
    screenshots_dir: Path,
    events_file: Path,
    output_dir: Path,
):
    """Process a complete capture session.

    Args:
        screenshots_dir: Directory with captured screenshots
        events_file: JSON file with input events
        output_dir: Where to save results
    """
    print("=" * 80)
    print("QONTINUI STATE DETECTION AND IMAGE EXTRACTION PIPELINE")
    print("=" * 80)

    # Step 1: Load frames
    print("\n[1/5] Loading frames...")
    frames = load_frames(screenshots_dir)
    print(f"Loaded {len(frames)} frames")

    # Step 2: Load events
    print("\n[2/5] Loading events...")
    events = load_events(events_file)
    print(f"Loaded {len(events)} events")

    # Step 3: Detect states (simplified - use actual StateBuilder in production)
    print("\n[3/5] Detecting states...")
    states = detect_states(frames, events)
    print(f"Detected {len(states)} states")

    # Step 4: Extract state images
    print("\n[4/5] Extracting state images...")
    config = ImageExtractionConfig(
        min_size=(30, 30),
        max_size=(400, 400),
        extract_at_click_locations=True,
        click_region_padding=40,
        edge_detection="canny",
        position_tolerance_px=10,
    )
    extractor = StateImageExtractor(config)

    for state in states:
        print(f"\nProcessing state: {state.name}")
        state_images = extractor.extract_from_state(state, frames, events)
        state.state_images = state_images
        print(f"  Extracted {len(state_images)} images")

        # Categorize
        fixed = sum(1 for img in state_images if img.position_type == "fixed")
        dynamic = sum(1 for img in state_images if img.position_type == "dynamic")
        print(f"  Fixed: {fixed}, Dynamic: {dynamic}")

    # Step 5: Save results
    print("\n[5/5] Saving results...")
    save_results(states, output_dir)
    print(f"Results saved to {output_dir}")

    print("\n" + "=" * 80)
    print("PIPELINE COMPLETE")
    print("=" * 80)


def load_frames(screenshots_dir: Path) -> List[Frame]:
    """Load frames from directory."""
    frames = []
    for i, file_path in enumerate(sorted(screenshots_dir.glob("*.png"))):
        image = cv2.imread(str(file_path))
        if image is None:
            continue

        # Extract timestamp from filename
        try:
            timestamp_str = file_path.stem.split("_")[-1]
            timestamp = float(timestamp_str) / 1000.0
        except:
            timestamp = float(i)

        frame = Frame(
            image=image,
            timestamp=timestamp,
            frame_index=i,
            file_path=str(file_path),
        )
        frames.append(frame)

    return frames


def load_events(events_file: Path) -> List[InputEvent]:
    """Load events from JSON."""
    with open(events_file) as f:
        data = json.load(f)

    events = []
    for event_data in data.get("events", []):
        event = InputEvent(
            timestamp=event_data["timestamp"],
            event_type=event_data.get("event_type", "click"),
            x=event_data.get("x"),
            y=event_data.get("y"),
            button=event_data.get("button"),
            key=event_data.get("key"),
            metadata=event_data.get("metadata"),
        )
        events.append(event)

    return events


def detect_states(frames: List[Frame], events: List[InputEvent]) -> List[DetectedState]:
    """Detect states from frames and events.

    This is a simplified version. In production, use the actual StateBuilder
    from qontinui library.
    """
    # For demo, create a single state spanning all frames
    state = DetectedState(
        name="MainState",
        description="Primary application state",
        state_images=[],
        start_frame_index=0,
        end_frame_index=len(frames) - 1,
        frame_indices=list(range(len(frames))),
        click_locations=[
            (event.x, event.y)
            for event in events
            if event.event_type == "click" and event.x and event.y
        ],
    )

    return [state]


def save_results(states: List[DetectedState], output_dir: Path):
    """Save detected states and extracted images."""
    output_dir.mkdir(parents=True, exist_ok=True)

    # Save each state
    for state in states:
        state_dir = output_dir / state.name
        state_dir.mkdir(exist_ok=True)

        # Save images
        images_dir = state_dir / "images"
        for state_image in state.state_images:
            save_state_image(state_image, images_dir)

        # Save metadata
        metadata = {
            "state": state.to_dict(),
            "num_images": len(state.state_images),
            "image_methods": {},
        }

        # Count by method
        for img in state.state_images:
            method = img.extraction_method
            metadata["image_methods"][method] = metadata["image_methods"].get(method, 0) + 1

        metadata_file = state_dir / "metadata.json"
        with open(metadata_file, "w") as f:
            json.dump(metadata, f, indent=2)

    # Save summary
    summary = {
        "num_states": len(states),
        "states": [
            {
                "name": state.name,
                "num_images": len(state.state_images),
                "num_frames": len(state.frame_indices),
            }
            for state in states
        ],
    }

    summary_file = output_dir / "summary.json"
    with open(summary_file, "w") as f:
        json.dump(summary, f, indent=2)


if __name__ == "__main__":
    import sys

    if len(sys.argv) != 4:
        print("Usage: python pipeline_example.py <screenshots_dir> <events.json> <output_dir>")
        sys.exit(1)

    screenshots_dir = Path(sys.argv[1])
    events_file = Path(sys.argv[2])
    output_dir = Path(sys.argv[3])

    process_capture_session(screenshots_dir, events_file, output_dir)
```

## Configuration Best Practices

### For Desktop Applications

```python
config = ImageExtractionConfig(
    min_size=(30, 30),           # Typical button size
    max_size=(800, 600),         # Larger windows
    click_region_padding=40,     # Generous padding
    position_tolerance_px=2,     # Strict for fixed UI
    edge_detection="canny",      # Good for sharp edges
    canny_threshold1=50,
    canny_threshold2=150,
)
```

### For Web Applications

```python
config = ImageExtractionConfig(
    min_size=(20, 20),           # Smaller web elements
    max_size=(400, 400),         # Smaller containers
    click_region_padding=30,     # Moderate padding
    position_tolerance_px=10,    # More lenient (scrolling)
    edge_detection="canny",
    canny_threshold1=30,         # Lower for gradients
    canny_threshold2=100,
)
```

### For Mobile Applications

```python
config = ImageExtractionConfig(
    min_size=(40, 40),           # Touch targets
    max_size=(600, 800),         # Full screen
    click_region_padding=50,     # Larger touch areas
    position_tolerance_px=15,    # Lenient for gestures
    edge_detection="sobel",      # Better for photos
)
```

## Error Handling

Always wrap extraction in try-except blocks:

```python
try:
    extracted_images = extractor.extract_from_state(state, frames, events)
except Exception as e:
    logger.error(f"Image extraction failed for state {state.name}: {e}")
    extracted_images = []
    # Continue with empty images or retry with different config
```

## Performance Optimization

### For Large Sessions (1000+ frames)

1. Process states in batches
2. Limit max_contours
3. Use sparse frame_indices instead of all frames
4. Disable context image extraction

```python
config = ImageExtractionConfig(
    max_contours=20,              # Limit contour detection
    extract_at_click_locations=True,  # Focus on clicks only
)

# Process in chunks
chunk_size = 100
for i in range(0, len(frames), chunk_size):
    chunk = frames[i:i+chunk_size]
    # Process chunk...
```

### Memory Management

```python
# Clear images after saving
for state_image in extracted_images:
    save_state_image(state_image, output_dir)
    # Clear large arrays
    state_image.image = None
    state_image.context_image = None
```

## Testing Integration

Add tests to verify integration:

```python
def test_full_pipeline():
    """Test complete integration pipeline."""
    # Setup test data
    test_dir = Path("test_data/session1")
    frames = load_frames(test_dir / "screenshots")
    events = load_events(test_dir / "events.json")

    # Detect states
    states = detect_states(frames, events)
    assert len(states) > 0

    # Extract images
    extractor = StateImageExtractor()
    for state in states:
        images = extractor.extract_from_state(state, frames, events)
        assert len(images) > 0

        # Verify all images have required fields
        for img in images:
            assert img.name
            assert img.image is not None
            assert img.position_type in ["fixed", "dynamic", "unknown"]
```

## Deployment Considerations

### Python Bridge Communication

When integrating with TypeScript/Tauri:

```python
# Return JSON-serializable results
def extract_images_api(state_json: str, frames_dir: str, events_json: str) -> str:
    """API endpoint for TypeScript integration."""
    try:
        # Parse inputs
        state_data = json.loads(state_json)
        state = DetectedState(**state_data)

        frames = load_frames(Path(frames_dir))

        with open(events_json) as f:
            events_data = json.load(f)
        events = [InputEvent(**e) for e in events_data]

        # Extract
        extractor = StateImageExtractor()
        images = extractor.extract_from_state(state, frames, events)

        # Serialize (without image data)
        result = {
            "success": True,
            "num_images": len(images),
            "images": [img.to_dict() for img in images],
        }

        return json.dumps(result)

    except Exception as e:
        return json.dumps({
            "success": False,
            "error": str(e),
        })
```

## Troubleshooting

### No images extracted

- Check frame loading: Verify images are valid
- Check click locations: Ensure within frame bounds
- Lower min_contour_area: May be filtering too aggressively
- Check logs: Enable DEBUG logging

### Too many false positives

- Increase min_contour_area
- Reduce max_contours
- Increase min_size constraints
- Use more restrictive edge detection thresholds

### Incorrect position classification

- Adjust position_tolerance_px
- Verify frames span sufficient time
- Check that template matching works (similarity_threshold)

## Next Steps

After integrating the image extractor:

1. Integrate with state persistence layer
2. Add runtime state detection using extracted images
3. Create training data export pipeline
4. Implement state transition validation
5. Add visual debugging tools

## Support

For issues or questions:

1. Check the logs (set log level to DEBUG)
2. Run example_usage.py to verify setup
3. Run test_image_extractor.py for unit tests
4. Review the main README.md for detailed documentation
