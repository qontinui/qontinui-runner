"""Example usage of the State Boundary Detector.

This script demonstrates how to use the StateBoundaryDetector to analyze
video frames and detect unique states and transitions.
"""

import logging
import sys
from pathlib import Path
from typing import List

import cv2
import numpy as np

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from analysis.state_boundary_detector import StateBoundaryConfig, StateBoundaryDetector
from models.state_models import Frame, InputEvent

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
)
logger = logging.getLogger(__name__)


def load_frames_from_directory(screenshots_dir: Path) -> List[Frame]:
    """Load frames from a directory of screenshots.

    Args:
        screenshots_dir: Directory containing screenshot files

    Returns:
        List of Frame objects sorted by timestamp
    """
    logger.info(f"Loading frames from {screenshots_dir}")

    # Find all image files
    image_files = sorted(screenshots_dir.glob("*.png")) + sorted(screenshots_dir.glob("*.jpg"))

    frames = []
    for i, image_path in enumerate(image_files):
        try:
            # Load image with OpenCV
            image = cv2.imread(str(image_path))
            if image is None:
                logger.warning(f"Failed to load {image_path}")
                continue

            # Extract timestamp from filename (assuming format: screenshot_1234567890.png)
            # Or use file modification time as fallback
            try:
                timestamp_str = image_path.stem.split("_")[-1]
                timestamp = float(timestamp_str) / 1000.0  # Convert ms to seconds
            except (ValueError, IndexError):
                timestamp = image_path.stat().st_mtime

            # Create Frame object
            frame = Frame(
                image=image,
                timestamp=timestamp,
                frame_index=i,
                file_path=str(image_path),
            )
            frames.append(frame)

        except Exception as e:
            logger.error(f"Error loading {image_path}: {e}")

    logger.info(f"Loaded {len(frames)} frames")
    return frames


def load_input_events(events_file: Path) -> List[InputEvent]:
    """Load input events from a JSON file.

    Args:
        events_file: Path to JSON file containing input events

    Returns:
        List of InputEvent objects
    """
    import json

    logger.info(f"Loading events from {events_file}")

    if not events_file.exists():
        logger.warning(f"Events file not found: {events_file}")
        return []

    try:
        with open(events_file, "r") as f:
            data = json.load(f)

        events = []
        for event_data in data.get("events", []):
            event = InputEvent(
                timestamp=event_data.get("timestamp", 0.0),
                event_type=event_data.get("event_type", "click"),
                x=event_data.get("x"),
                y=event_data.get("y"),
                button=event_data.get("button"),
                key=event_data.get("key"),
                metadata=event_data.get("metadata"),
            )
            events.append(event)

        logger.info(f"Loaded {len(events)} events")
        return events

    except Exception as e:
        logger.error(f"Error loading events: {e}")
        return []


def example_basic_detection(frames: List[Frame]):
    """Example 1: Basic state detection with default configuration.

    Args:
        frames: List of frames to analyze
    """
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 1: Basic State Detection")
    logger.info("=" * 80)

    # Create detector with default config
    detector = StateBoundaryDetector()

    # Detect states
    states = detector.detect_states(frames)

    # Print results
    logger.info(f"\nDetected {len(states)} unique states:")
    for state in states:
        logger.info(f"  - {state.name}:")
        logger.info(f"      Frames: {len(state.frame_indices)}")
        logger.info(f"      Duration: {state.metadata.get('duration_ms', 0)}ms")
        logger.info(f"      First occurrence: {state.metadata.get('first_occurrence_ms', 0)}ms")
        logger.info(
            f"      Representative frame: {state.metadata.get('representative_frame_index', -1)}"
        )


def example_custom_config(frames: List[Frame]):
    """Example 2: State detection with custom configuration.

    Args:
        frames: List of frames to analyze
    """
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 2: Custom Configuration")
    logger.info("=" * 80)

    # Create custom configuration
    config = StateBoundaryConfig(
        similarity_threshold=0.95,  # Higher threshold = stricter similarity
        clustering_algorithm="hierarchical",  # Use hierarchical clustering
        min_state_duration_ms=1000,  # States must last at least 1 second
        feature_extractor="orb",
        feature_count=1000,  # More features for better accuracy
        optical_flow_threshold=0.5,
    )

    # Create detector with custom config
    detector = StateBoundaryDetector(config)

    # Detect states
    states = detector.detect_states(frames)

    # Print results
    logger.info(f"\nDetected {len(states)} unique states with custom config:")
    for state in states:
        logger.info(f"  - {state.name}: {len(state.frame_indices)} frames")


def example_transition_detection(frames: List[Frame], events: List[InputEvent]):
    """Example 3: Transition detection with event correlation.

    Args:
        frames: List of frames to analyze
        events: List of input events
    """
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 3: Transition Detection")
    logger.info("=" * 80)

    # Create detector
    detector = StateBoundaryDetector()

    # Identify transitions
    transitions = detector.identify_transitions(frames, events)

    # Print results
    logger.info(f"\nIdentified {len(transitions)} transitions:")
    for i, transition in enumerate(transitions[:10]):  # Show first 10
        logger.info(f"  Transition {i + 1}:")
        logger.info(f"      Frame: {transition.frame_index}")
        logger.info(f"      Timestamp: {transition.timestamp_ms}ms")
        logger.info(f"      Optical flow: {transition.optical_flow_magnitude:.4f}")
        logger.info(f"      Visual change: {transition.visual_change_score:.4f}")
        if transition.trigger_event_index is not None:
            event = events[transition.trigger_event_index]
            logger.info(f"      Triggered by: {event.event_type} at ({event.x}, {event.y})")


def example_frame_similarity(frames: List[Frame]):
    """Example 4: Computing similarity between specific frames.

    Args:
        frames: List of frames to analyze
    """
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 4: Frame Similarity Comparison")
    logger.info("=" * 80)

    detector = StateBoundaryDetector()

    if len(frames) < 3:
        logger.warning("Need at least 3 frames for this example")
        return

    # Compare first frame with several others
    base_frame = frames[0]
    logger.info(f"\nComparing frame 0 with other frames:")

    for i in [1, len(frames) // 4, len(frames) // 2, len(frames) - 1]:
        if i >= len(frames):
            continue

        similarity = detector.compute_similarity(base_frame, frames[i])
        phash = detector.compute_perceptual_hash(frames[i])

        logger.info(f"  Frame {i}:")
        logger.info(f"      SSIM similarity: {similarity:.4f}")
        logger.info(f"      Perceptual hash: {phash}")


def example_clustering_comparison(frames: List[Frame]):
    """Example 5: Compare different clustering algorithms.

    Args:
        frames: List of frames to analyze
    """
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 5: Clustering Algorithm Comparison")
    logger.info("=" * 80)

    algorithms = ["dbscan", "hierarchical", "kmeans"]

    for algo in algorithms:
        config = StateBoundaryConfig(
            clustering_algorithm=algo,
            min_state_duration_ms=500,
        )
        detector = StateBoundaryDetector(config)

        try:
            states = detector.detect_states(frames)
            logger.info(f"\n{algo.upper()}:")
            logger.info(f"  Detected {len(states)} states")
            logger.info(f"  Frame distribution: {[len(s.frame_indices) for s in states]}")
        except Exception as e:
            logger.error(f"  Error with {algo}: {e}")


def main():
    """Main entry point for examples."""
    # Example usage - update these paths to your actual data
    screenshots_dir = Path("./captures/session1")
    events_file = Path("./captures/session1/events.json")

    # Check if example data exists
    if not screenshots_dir.exists():
        logger.warning(f"Screenshots directory not found: {screenshots_dir}")
        logger.info("\nTo run these examples, you need to:")
        logger.info("1. Create a directory with screenshot files")
        logger.info("2. Optionally create an events.json file with input events")
        logger.info("3. Update the paths in this script")
        logger.info("\nExample directory structure:")
        logger.info("  ./captures/session1/")
        logger.info("    screenshot_1234567890.png")
        logger.info("    screenshot_1234567891.png")
        logger.info("    ...")
        logger.info("    events.json")
        return

    # Load frames
    frames = load_frames_from_directory(screenshots_dir)

    if not frames:
        logger.error("No frames loaded, cannot run examples")
        return

    # Load events (optional)
    events = load_input_events(events_file)

    # Run examples
    try:
        example_basic_detection(frames)
        example_custom_config(frames)

        if events:
            example_transition_detection(frames, events)

        example_frame_similarity(frames)
        example_clustering_comparison(frames)

    except Exception as e:
        logger.error(f"Error running examples: {e}")
        import traceback

        traceback.print_exc()

    logger.info("\n" + "=" * 80)
    logger.info("Examples completed!")
    logger.info("=" * 80)


if __name__ == "__main__":
    main()
