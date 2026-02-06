# State Boundary Detection Service

This module provides advanced state boundary detection for video frame analysis using visual clustering and computer vision techniques.

## Overview

The State Boundary Detector identifies unique screen states from sequences of video frames by:

1. **Feature Extraction**: Extracts visual features using ORB, SIFT, or SURF detectors
2. **Perceptual Hashing**: Fast initial grouping using pHash for similar frames
3. **Similarity Analysis**: SSIM (Structural Similarity Index) for detailed comparison
4. **Clustering**: Groups frames into states using DBSCAN, hierarchical, or k-means
5. **Transition Detection**: Identifies state changes using optical flow analysis
6. **Event Correlation**: Correlates visual changes with input events

## Installation

Install the required dependencies:

```bash
pip install opencv-python scikit-image imagehash scikit-learn numpy
```

Or use the provided requirements file:

```bash
pip install -r analysis/requirements.txt
```

## Quick Start

```python
from analysis import StateBoundaryDetector, StateBoundaryConfig
from models import Frame

# Load your frames
frames = [...]  # List of Frame objects

# Create detector with default config
detector = StateBoundaryDetector()

# Detect states
states = detector.detect_states(frames)

# Print results
for state in states:
    print(f"{state.name}: {len(state.frame_indices)} frames")
```

## Configuration

Customize detection behavior with `StateBoundaryConfig`:

```python
config = StateBoundaryConfig(
    similarity_threshold=0.92,          # SSIM threshold (0.0-1.0)
    clustering_algorithm="dbscan",      # "dbscan", "hierarchical", "kmeans"
    min_state_duration_ms=500,          # Minimum state duration
    feature_extractor="orb",            # "orb", "sift", "surf"
    feature_count=500,                  # Number of features to extract
    optical_flow_threshold=0.3,         # Flow magnitude threshold
    phash_size=8,                       # Perceptual hash size
    phash_difference_threshold=10,      # Max hamming distance for pHash
    min_frames_per_state=3,             # Minimum frames per cluster
    dbscan_eps=0.5,                     # DBSCAN epsilon
    dbscan_min_samples=3,               # DBSCAN min samples
    hierarchical_distance_threshold=1.5,# Hierarchical clustering threshold
    kmeans_n_clusters=None,             # K-means clusters (None = auto)
)

detector = StateBoundaryDetector(config)
```

## Key Features

### 1. State Detection

Automatically groups visually similar frames into states:

```python
states = detector.detect_states(frames)

for state in states:
    print(f"State: {state.name}")
    print(f"  Frames: {state.frame_indices}")
    print(f"  Duration: {state.metadata['duration_ms']}ms")
    print(f"  Representative frame: {state.metadata['representative_frame_index']}")
```

### 2. Transition Detection

Identifies when the UI transitions between states:

```python
from models import InputEvent

# Load input events
events = [...]  # List of InputEvent objects

# Detect transitions
transitions = detector.identify_transitions(frames, events)

for transition in transitions:
    print(f"Transition at frame {transition.frame_index}")
    print(f"  Optical flow: {transition.optical_flow_magnitude}")
    print(f"  Visual change: {transition.visual_change_score}")
    if transition.trigger_event_index is not None:
        event = events[transition.trigger_event_index]
        print(f"  Triggered by: {event.event_type}")
```

### 3. Frame Similarity

Compare individual frames:

```python
similarity = detector.compute_similarity(frame1, frame2)
print(f"SSIM similarity: {similarity:.4f}")

phash = detector.compute_perceptual_hash(frame)
print(f"Perceptual hash: {phash}")
```

## Clustering Algorithms

### DBSCAN (Default)

Best for: Variable number of states, noise tolerance

```python
config = StateBoundaryConfig(
    clustering_algorithm="dbscan",
    dbscan_eps=0.5,              # Distance threshold
    dbscan_min_samples=3,        # Minimum cluster size
)
```

**Pros:**

- Automatically determines number of states
- Handles noise and outliers
- No need to specify cluster count

**Cons:**

- Sensitive to parameter tuning
- May create too many small clusters

### Hierarchical Clustering

Best for: Hierarchical state relationships

```python
config = StateBoundaryConfig(
    clustering_algorithm="hierarchical",
    hierarchical_distance_threshold=1.5,  # Linkage threshold
)
```

**Pros:**

- Creates natural hierarchy of states
- Deterministic results
- Good for nested UI states

**Cons:**

- Slower than other methods
- Requires distance threshold tuning

### K-Means

Best for: Known number of states

```python
config = StateBoundaryConfig(
    clustering_algorithm="kmeans",
    kmeans_n_clusters=5,  # Number of expected states
)
```

**Pros:**

- Fast and scalable
- Consistent cluster sizes
- Works well with auto-detection

**Cons:**

- Requires knowing/estimating state count
- Sensitive to initialization

## Feature Extractors

### ORB (Default)

Fast and efficient, good for real-time analysis:

```python
config = StateBoundaryConfig(
    feature_extractor="orb",
    feature_count=500,
)
```

### SIFT

More accurate, better for detailed matching:

```python
config = StateBoundaryConfig(
    feature_extractor="sift",
    feature_count=1000,
)
```

### SURF

Fast and robust (requires opencv-contrib):

```python
config = StateBoundaryConfig(
    feature_extractor="surf",
    feature_count=750,
)
```

## Advanced Usage

### Custom State Processing

```python
# Detect states
states = detector.detect_states(frames)

# Filter by duration
long_states = [s for s in states if s.metadata['duration_ms'] > 1000]

# Find most representative frames
for state in states:
    rep_idx = state.metadata['representative_frame_index']
    rep_frame = frames[rep_idx]
    # Save or process representative frame
```

### Transition Analysis

```python
# Get transitions
transitions = detector.identify_transitions(frames, events)

# Find high-impact transitions
significant = [t for t in transitions if t.visual_change_score > 0.5]

# Group by triggering event type
by_event = {}
for t in transitions:
    if t.trigger_event_index is not None:
        event = events[t.trigger_event_index]
        by_event.setdefault(event.event_type, []).append(t)
```

### Performance Tuning

For faster processing with many frames:

```python
config = StateBoundaryConfig(
    feature_extractor="orb",          # Fastest extractor
    feature_count=300,                # Fewer features
    phash_difference_threshold=15,    # More aggressive pHash filtering
    min_frames_per_state=5,           # Larger minimum clusters
)
```

For higher accuracy:

```python
config = StateBoundaryConfig(
    similarity_threshold=0.95,        # Stricter similarity
    feature_extractor="sift",         # More accurate features
    feature_count=1500,               # More features
    phash_difference_threshold=5,     # Stricter pHash
    min_state_duration_ms=1000,       # Longer states only
)
```

## Examples

See `example_state_boundary.py` for complete working examples:

```bash
# Run the examples
python analysis/example_state_boundary.py
```

## API Reference

### StateBoundaryDetector

#### Methods

**`__init__(config: Optional[StateBoundaryConfig] = None)`**

- Initialize detector with optional configuration

**`detect_states(frames: List[Frame]) -> List[DetectedState]`**

- Main state detection method
- Returns list of unique states

**`identify_transitions(frames: List[Frame], events: List[InputEvent]) -> List[TransitionPoint]`**

- Detect state transitions
- Correlate with input events

**`compute_similarity(frame1: Frame, frame2: Frame) -> float`**

- Compute SSIM between two frames
- Returns similarity score (0.0-1.0)

**`compute_perceptual_hash(frame: Frame) -> str`**

- Compute perceptual hash
- Returns hash string

### Data Models

**`StateBoundaryConfig`**: Configuration dataclass

**`FrameFeatures`**: Extracted features from a frame

**`TransitionPoint`**: Represents a state transition

**`DetectedState`** (from models.state_models): Detected state with metadata

## Integration

### With Video Capture Service

```python
from services import HistoricalCaptureService
from analysis import StateBoundaryDetector

# Capture video
capture_service = HistoricalCaptureService()
frames, events = capture_service.capture_session(duration=30)

# Detect states
detector = StateBoundaryDetector()
states = detector.detect_states(frames)
```

### With Training Data Export

```python
from analysis import StateBoundaryDetector
from exporters import TrainingDataExporter

# Detect states
detector = StateBoundaryDetector()
states = detector.detect_states(frames)

# Export for training
exporter = TrainingDataExporter()
exporter.export_states(states, frames, output_dir="training_data")
```

## Troubleshooting

### Issue: Too many small states

**Solution**: Increase `min_frames_per_state` and `min_state_duration_ms`

```python
config = StateBoundaryConfig(
    min_frames_per_state=5,
    min_state_duration_ms=1000,
)
```

### Issue: States not being detected

**Solution**: Lower `similarity_threshold` or adjust clustering parameters

```python
config = StateBoundaryConfig(
    similarity_threshold=0.85,
    dbscan_eps=0.7,
)
```

### Issue: Slow performance

**Solution**: Use ORB, reduce feature count, increase pHash threshold

```python
config = StateBoundaryConfig(
    feature_extractor="orb",
    feature_count=300,
    phash_difference_threshold=15,
)
```

### Issue: Missing transitions

**Solution**: Lower `optical_flow_threshold`

```python
config = StateBoundaryConfig(
    optical_flow_threshold=0.2,
)
```

## License

Part of the qontinui-runner project.
