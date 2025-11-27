# StateImage Extraction Service - Quick Start Guide

Get started with the StateImage Extraction Service in 5 minutes.

## Installation

### 1. Install Dependencies

```bash
cd /mnt/c/qontinui/qontinui-runner/python-bridge

# Using pip
pip install numpy opencv-python

# Or using poetry (recommended)
poetry add numpy opencv-python
```

### 2. Verify Installation

```bash
python3 analysis/verify_installation.py
```

You should see all checks pass. If not, follow the error messages.

## Basic Usage

### Extract Images from a State

```python
from analysis import ImageExtractionConfig, StateImageExtractor
from models import DetectedState, Frame, InputEvent

# 1. Create configuration
config = ImageExtractionConfig(
    min_size=(30, 30),              # Minimum image size
    max_size=(400, 400),            # Maximum image size
    extract_at_click_locations=True, # Extract around clicks
    click_region_padding=40,        # Padding around clicks
)

# 2. Create extractor
extractor = StateImageExtractor(config)

# 3. Extract images
extracted_images = extractor.extract_from_state(
    state=my_state,      # DetectedState object
    frames=my_frames,    # List of Frame objects
    events=my_events,    # List of InputEvent objects
)

# 4. Use results
for img in extracted_images:
    print(f"Extracted: {img.name}")
    print(f"  Position: {img.position_type} at {img.position}")
    print(f"  Size: {img.bbox[2]}x{img.bbox[3]}")
    print(f"  Method: {img.extraction_method}")
```

## Run Examples

### Synthetic Examples

```bash
# Run all examples with synthetic data
python3 analysis/example_usage.py
```

This will demonstrate:
- Basic extraction
- Contour detection
- Position analysis
- Save/load operations

### Real Data Example

```bash
# Process captured data
python3 analysis/example_usage.py /path/to/screenshots /path/to/events.json
```

This will:
1. Load your captured screenshots
2. Load input events
3. Detect states
4. Extract images
5. Save results to `screenshots/../extracted_images/`

## Common Use Cases

### Use Case 1: Extract Images Around Clicks

```python
config = ImageExtractionConfig(
    extract_at_click_locations=True,
    click_region_padding=40,
)
```

### Use Case 2: Automatic Contour Detection

```python
config = ImageExtractionConfig(
    edge_detection="canny",
    min_contour_area=100,
    max_contours=20,
)
```

### Use Case 3: Find Fixed UI Elements

```python
# Extract images
images = extractor.extract_from_state(state, frames, events)

# Filter for fixed-position elements
fixed_images = [img for img in images if img.position_type == "fixed"]
```

## Configuration Presets

### Desktop App
```python
ImageExtractionConfig(
    min_size=(30, 30),
    max_size=(800, 600),
    click_region_padding=40,
    position_tolerance_px=2,
)
```

### Web App
```python
ImageExtractionConfig(
    min_size=(20, 20),
    max_size=(400, 400),
    click_region_padding=30,
    position_tolerance_px=10,
)
```

### Mobile App
```python
ImageExtractionConfig(
    min_size=(40, 40),
    max_size=(600, 800),
    click_region_padding=50,
    position_tolerance_px=15,
)
```

## Saving and Loading

### Save Extracted Images

```python
from analysis import save_state_image
from pathlib import Path

output_dir = Path("./output/images")

for state_image in extracted_images:
    file_path = save_state_image(state_image, output_dir)
    print(f"Saved to {file_path}")
```

### Load Images

```python
from analysis import load_state_image

image_path = Path("./output/images/test_image.png")
state_image = load_state_image(image_path)
```

## Testing

### Run Unit Tests

```bash
# Install pytest if needed
pip install pytest

# Run tests
pytest analysis/test_image_extractor.py -v
```

### Run Specific Test

```bash
pytest analysis/test_image_extractor.py::TestStateImageExtractor::test_extract_at_location -v
```

## Troubleshooting

### No images extracted

**Problem**: `extract_from_state()` returns empty list

**Solutions**:
1. Check that frames have valid image data
2. Verify click locations are within bounds
3. Lower `min_contour_area` to detect smaller elements
4. Increase `click_region_padding`

### Too many images extracted

**Problem**: Too many false positive extractions

**Solutions**:
1. Increase `min_contour_area`
2. Reduce `max_contours`
3. Use stricter `min_size` constraints
4. Increase edge detection thresholds

### Import errors

**Problem**: `ModuleNotFoundError: No module named 'models'`

**Solutions**:
1. Run from `python-bridge` directory
2. Add to Python path: `export PYTHONPATH=/path/to/python-bridge:$PYTHONPATH`
3. Use absolute imports in your code

### OpenCV errors

**Problem**: `cv2.error: OpenCV(4.x.x) ... error`

**Solutions**:
1. Verify image format: Should be BGR numpy array
2. Check image dimensions: `image.shape` should be (height, width, 3)
3. Ensure image data type: `image.dtype` should be `uint8`

## Next Steps

1. **Read the Full Documentation**
   - `README.md` - Complete reference
   - `INTEGRATION_GUIDE.md` - Integration instructions

2. **Explore Examples**
   - Run `example_usage.py` with different configurations
   - Modify examples for your use case

3. **Integrate into Your Workflow**
   - Connect to state detection service
   - Add to capture pipeline
   - Export for training

4. **Customize Configuration**
   - Tune parameters for your application type
   - Test different edge detection methods
   - Optimize for performance

## Getting Help

1. Run verification: `python3 analysis/verify_installation.py`
2. Check logs: Set `logging.DEBUG` for detailed output
3. Review documentation: `README.md` and `INTEGRATION_GUIDE.md`
4. Run examples: `python3 analysis/example_usage.py`

## Resources

- **Documentation**: `analysis/README.md`
- **Integration**: `analysis/INTEGRATION_GUIDE.md`
- **Examples**: `analysis/example_usage.py`
- **Tests**: `analysis/test_image_extractor.py`
- **Summary**: `analysis/IMPLEMENTATION_SUMMARY.md`

## Quick Reference

### Key Classes

- `ImageExtractionConfig`: Configuration
- `StateImageExtractor`: Main service
- `StateImage`: Extracted image data
- `DetectedState`: Application state
- `Frame`: Screenshot data
- `InputEvent`: User input

### Key Methods

- `extract_from_state()`: Main extraction
- `extract_at_location()`: Extract at point
- `detect_contours()`: Find UI elements
- `determine_position_type()`: Fixed vs dynamic
- `save_state_image()`: Save to disk
- `load_state_image()`: Load from disk

### Configuration Keys

- `min_size`: Minimum dimensions
- `max_size`: Maximum dimensions
- `click_region_padding`: Click area size
- `position_tolerance_px`: Fixed position threshold
- `edge_detection`: Detection method
- `similarity_threshold`: Match threshold

Now you're ready to use the StateImage Extraction Service!
