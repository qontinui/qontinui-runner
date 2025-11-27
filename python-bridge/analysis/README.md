# StateImage Extraction Service

The StateImage Extraction Service is a comprehensive tool for extracting identifying images from detected states in the qontinui-runner python-bridge. It analyzes captured frames and input events to identify visual elements that can be used to recognize and distinguish different application states.

## Overview

StateImages are persistent visual elements (like buttons, icons, labels) that help identify application states. This service:

- Extracts images within state boundaries
- Focuses on regions around click locations from input events
- Uses edge detection and contour analysis to find UI elements
- Determines if positions are fixed (same across frames) or dynamic
- Supports multiple extraction methods

## Architecture

### Core Components

1. **ImageExtractionConfig**: Configuration dataclass for extraction parameters
2. **StateImageExtractor**: Main extraction service
3. **StateImage**: Data model representing an extracted image
4. **Frame**: Data model representing a captured screenshot
5. **DetectedState**: Data model representing an application state

### Extraction Methods

1. **Click Location Extraction**: Extracts regions around user click locations
2. **Contour Detection**: Uses edge detection to find UI elements automatically
3. **Best Crop**: Refines extraction boundaries using edge analysis

### Position Classification

Images are classified as:
- **Fixed**: Appears at the same location across frames (e.g., menu bar buttons)
- **Dynamic**: Appears at varying locations (e.g., popup dialogs)

## Installation

The service requires:
```bash
pip install numpy opencv-python
```

It also depends on the qontinui-runner models:
```python
from models import DetectedState, Frame, InputEvent, StateImage
```

## Usage

### Basic Usage

```python
from analysis import ImageExtractionConfig, StateImageExtractor
from models import DetectedState, Frame, InputEvent

# Create configuration
config = ImageExtractionConfig(
    min_size=(20, 20),
    max_size=(500, 500),
    extract_at_click_locations=True,
    click_region_padding=30,
    edge_detection="canny",
    position_tolerance_px=5,
)

# Create extractor
extractor = StateImageExtractor(config)

# Extract images from a state
extracted_images = extractor.extract_from_state(
    state=my_state,
    frames=all_frames,
    events=input_events,
)

# Process results
for image in extracted_images:
    print(f"Extracted: {image.name}")
    print(f"  Position: {image.position_type} at {image.position}")
    print(f"  Method: {image.extraction_method}")
    print(f"  BBox: {image.bbox}")
```

### Extract at Specific Location

```python
# Extract image around a specific point
state_image = extractor.extract_at_location(
    frame=frame.image,
    x=100,
    y=200,
    padding=30,
    frame_index=0,
)

if state_image:
    print(f"Extracted region: {state_image.bbox}")
```

### Contour Detection

```python
# Detect UI elements using contours
bboxes = extractor.detect_contours(frame.image)

for x, y, w, h in bboxes:
    print(f"Found contour at ({x}, {y}) size {w}x{h}")
```

### Position Analysis

```python
# Determine if position is fixed or dynamic
occurrences = [(100, 100), (101, 100), (100, 101)]  # Very close positions
is_fixed = extractor.determine_position_type(state_image, occurrences)
print(f"Position type: {'fixed' if is_fixed else 'dynamic'}")
```

### Save and Load Images

```python
from analysis import save_state_image, load_state_image
from pathlib import Path

# Save image to disk
output_dir = Path("./output/images")
file_path = save_state_image(state_image, output_dir)

# Load image back
loaded_image = load_state_image(file_path)
```

## Configuration Options

### ImageExtractionConfig

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `min_size` | Tuple[int, int] | (20, 20) | Minimum image size (width, height) |
| `max_size` | Tuple[int, int] | (500, 500) | Maximum image size (width, height) |
| `edge_detection` | str | "canny" | Edge detection method: "canny", "sobel", "laplacian" |
| `contour_approximation` | float | 0.02 | Contour approximation epsilon |
| `extract_at_click_locations` | bool | True | Whether to extract at click locations |
| `click_region_padding` | int | 20 | Padding around click locations (pixels) |
| `position_tolerance_px` | int | 5 | Tolerance for fixed position classification |
| `canny_threshold1` | int | 50 | Canny edge detection lower threshold |
| `canny_threshold2` | int | 150 | Canny edge detection upper threshold |
| `similarity_threshold` | float | 0.85 | Template matching threshold |
| `min_contour_area` | int | 100 | Minimum contour area to consider |
| `max_contours` | int | 50 | Maximum number of contours to extract |

## Data Models

### StateImage

Represents an extracted identifying image:

```python
@dataclass
class StateImage:
    name: str                           # Unique identifier
    image: np.ndarray                   # Image data (BGR format)
    bbox: Tuple[int, int, int, int]     # (x, y, width, height)
    position_type: str                  # "fixed" or "dynamic"
    position: Tuple[int, int]           # (x, y) top-left corner
    similarity_threshold: float         # Match threshold (0.0-1.0)
    context_image: Optional[np.ndarray] # Larger context around image
    source_frame_index: Optional[int]   # Source frame index
    extraction_method: str              # Extraction method used
    metadata: Dict[str, Any]            # Additional metadata
```

### Frame

Represents a captured screenshot:

```python
@dataclass
class Frame:
    image: np.ndarray          # OpenCV image (BGR format)
    timestamp: float           # Capture timestamp
    frame_index: int           # Sequential index
    file_path: Optional[str]   # Source file path
    metadata: Dict[str, Any]   # Additional metadata
```

### DetectedState

Represents an application state:

```python
@dataclass
class DetectedState:
    name: str                                    # State name
    description: str                             # State description
    state_images: List[StateImage]               # Extracted images
    start_frame_index: int                       # First frame
    end_frame_index: int                         # Last frame
    frame_indices: List[int]                     # All frame indices
    boundary: Optional[Tuple[int, int, int, int]] # State boundary
    click_locations: List[Tuple[int, int]]       # Click coordinates
    transitions_to: List[str]                    # Target states
    metadata: Dict[str, Any]                     # Additional metadata
```

## Extraction Pipeline

The extraction process follows these steps:

1. **Get State Frames**: Identify frames belonging to the state
2. **Identify Click Locations**: Extract click events within the state timeframe
3. **Extract at Clicks**: Extract regions around each click location
4. **Contour Analysis**: Detect additional UI elements using edge detection
5. **Position Classification**: Analyze occurrences to determine position type
6. **Result Compilation**: Return all extracted StateImages

### Click Location Extraction

When `extract_at_click_locations=True`:
- For each click location (x, y)
- Extract region: [x-padding, y-padding] to [x+padding, y+padding]
- Validate size constraints
- Extract larger context for reference
- Create StateImage with metadata

### Contour Detection

1. Convert frame to grayscale
2. Apply edge detection (Canny, Sobel, or Laplacian)
3. Find contours using OpenCV
4. Filter by area and size constraints
5. Convert to bounding boxes
6. Exclude regions near click locations (already extracted)
7. Refine boundaries using best_crop

### Position Classification

For each extracted image:
1. Search for occurrences across all state frames
2. Use template matching with similarity threshold
3. Collect all matching positions (x, y)
4. Calculate position variance
5. Classify as "fixed" if max distance < tolerance, else "dynamic"

## Examples

See `example_usage.py` for comprehensive examples:

```bash
# Run synthetic examples
python3 analysis/example_usage.py

# Run with real captured data
python3 analysis/example_usage.py /path/to/screenshots /path/to/events.json
```

### Example Output

```
[INFO] Extracting images from state: LoginScreen
[DEBUG] Found 12 frames for state LoginScreen
[DEBUG] Found 3 click locations in state LoginScreen
[DEBUG] Extracted image at click location (150, 200) from frame 0
[DEBUG] Extracted 8 images from contour analysis
[DEBUG] Image LoginScreen_click_150_200 classified as fixed position (found in 10 frames)
[INFO] Extracted 11 total images from state LoginScreen
```

## Error Handling

The service includes comprehensive error handling:

- **Invalid Frames**: Skips frames that can't be loaded
- **Empty Regions**: Returns None for invalid extractions
- **Contour Errors**: Continues processing remaining contours
- **Template Matching**: Handles dimension mismatches gracefully

All errors are logged with appropriate severity levels.

## Logging

Configure logging level:

```python
import logging
logging.getLogger('analysis.image_extractor').setLevel(logging.DEBUG)
```

Log levels:
- **DEBUG**: Detailed extraction progress
- **INFO**: High-level operations
- **WARNING**: Non-critical issues
- **ERROR**: Extraction failures

## Integration with qontinui-runner

The service integrates with the broader qontinui-runner architecture:

1. **State Detection Service**: Provides DetectedState objects
2. **Capture Manager**: Provides Frame objects and input events
3. **Training Export**: Uses extracted images for model training
4. **State Recognition**: Uses StateImages for runtime state detection

## Performance Considerations

### Memory Usage

- Frames are loaded as needed, not all at once
- Context images are optional (can be omitted to save memory)
- Images are stored as numpy arrays (efficient)

### Processing Time

- Contour detection: O(n) per frame where n = pixel count
- Template matching: O(m*n) where m = template size, n = frame size
- Position analysis: O(k*f) where k = images, f = frames

### Optimization Tips

1. Limit `max_contours` to reduce processing time
2. Use smaller `click_region_padding` to extract fewer images
3. Set appropriate `min_contour_area` to filter noise
4. Use `frame_indices` instead of frame ranges for sparse states

## Advanced Usage

### Custom Edge Detection

```python
# Use custom edge detection parameters
config = ImageExtractionConfig(
    edge_detection="canny",
    canny_threshold1=30,   # Lower = more edges
    canny_threshold2=100,  # Adjust ratio for quality
)
```

### Multi-Scale Extraction

```python
# Extract at multiple scales
for padding in [20, 40, 60]:
    config.click_region_padding = padding
    images = extractor.extract_from_state(state, frames, events)
    # Process images at this scale
```

### Filtering Results

```python
# Filter by extraction method
click_images = [img for img in extracted_images
                if img.extraction_method == "click_location"]

# Filter by position type
fixed_images = [img for img in extracted_images
                if img.position_type == "fixed"]

# Filter by size
large_images = [img for img in extracted_images
                if img.bbox[2] * img.bbox[3] > 10000]  # Area > 10k pixels
```

## Testing

Run the test suite:

```bash
# Unit tests
pytest tests/test_image_extractor.py

# Integration tests
pytest tests/test_image_extraction_integration.py

# Run examples as smoke tests
python3 analysis/example_usage.py
```

## Troubleshooting

### No images extracted

- Check that frames have valid image data
- Verify click locations are within frame boundaries
- Lower `min_contour_area` to detect smaller elements
- Increase `click_region_padding` for larger regions

### Too many images extracted

- Increase `min_contour_area` to filter noise
- Reduce `max_contours` to limit results
- Disable contour detection with custom config
- Use more restrictive size constraints

### Position classification incorrect

- Adjust `position_tolerance_px` (higher = more lenient)
- Check that frames span sufficient time range
- Verify template matching threshold is appropriate
- Review occurrence data in metadata

### Memory issues

- Process states in batches
- Disable context image extraction
- Use lower resolution frames
- Limit number of frames per state

## Future Enhancements

Potential improvements:

1. Multi-scale template matching for zoom invariance
2. Color histogram analysis for better matching
3. Neural network-based element detection
4. Semantic grouping of related elements
5. Incremental extraction for large datasets
6. GPU acceleration for template matching
7. Automatic threshold tuning
8. Visual diff analysis between states

## Contributing

When contributing to this module:

1. Follow existing code style and patterns
2. Add comprehensive error handling
3. Include logging at appropriate levels
4. Update documentation for new features
5. Add examples for new functionality
6. Write unit tests for new methods

## License

Part of the qontinui-runner project. See main project LICENSE file.
