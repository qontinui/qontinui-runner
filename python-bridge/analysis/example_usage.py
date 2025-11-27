"""Example usage of the StateImage extraction service.

This script demonstrates how to use the StateImageExtractor to extract
identifying images from detected states.
"""

import json
import logging
from pathlib import Path
from typing import List

import cv2
import numpy as np

from models import DetectedState, Frame, InputEvent, StateImage
from analysis import ImageExtractionConfig, StateImageExtractor, save_state_image


# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='[%(asctime)s] %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


def load_frames_from_directory(screenshots_dir: Path) -> List[Frame]:
    """Load frames from a directory of screenshots.

    Args:
        screenshots_dir: Directory containing PNG screenshots

    Returns:
        List of Frame objects
    """
    logger.info("Loading frames from %s", screenshots_dir)

    screenshot_files = sorted(screenshots_dir.glob("*.png"))
    frames = []

    for i, file_path in enumerate(screenshot_files):
        try:
            # Load image
            image = cv2.imread(str(file_path))
            if image is None:
                logger.warning("Failed to load %s", file_path)
                continue

            # Extract timestamp from filename (format: screenshot_1234567890123.png)
            try:
                timestamp_str = file_path.stem.split("_")[-1]
                timestamp = float(timestamp_str) / 1000.0
            except (ValueError, IndexError):
                # Fallback to file modification time
                timestamp = file_path.stat().st_mtime

            # Create Frame object
            frame = Frame(
                image=image,
                timestamp=timestamp,
                frame_index=i,
                file_path=str(file_path),
                metadata={"filename": file_path.name},
            )
            frames.append(frame)

        except Exception as e:
            logger.error("Error loading %s: %s", file_path, e)

    logger.info("Loaded %d frames", len(frames))
    return frames


def load_events_from_json(events_file: Path) -> List[InputEvent]:
    """Load input events from JSON file.

    Expected format:
    {
        "events": [
            {
                "timestamp": 1234567890.123,
                "event_type": "click",
                "x": 100,
                "y": 200,
                "button": "left"
            },
            ...
        ]
    }

    Args:
        events_file: Path to JSON file with events

    Returns:
        List of InputEvent objects
    """
    logger.info("Loading events from %s", events_file)

    with open(events_file, "r") as f:
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

    logger.info("Loaded %d events", len(events))
    return events


def create_sample_state(frames: List[Frame]) -> DetectedState:
    """Create a sample DetectedState for testing.

    Args:
        frames: Available frames

    Returns:
        DetectedState object
    """
    if not frames:
        raise ValueError("No frames available")

    # Create a simple state spanning all frames
    state = DetectedState(
        name="SampleState",
        description="A sample state for testing image extraction",
        state_images=[],
        start_frame_index=0,
        end_frame_index=len(frames) - 1,
        frame_indices=list(range(len(frames))),
        boundary=None,
        click_locations=[(100, 100), (200, 200)],  # Sample click locations
        transitions_to=[],
        metadata={"source": "example"},
    )

    return state


def example_basic_extraction():
    """Example: Basic image extraction from a state."""
    logger.info("=" * 80)
    logger.info("EXAMPLE: Basic Image Extraction")
    logger.info("=" * 80)

    # Create synthetic frames for demonstration
    frames = []
    for i in range(5):
        # Create a simple colored frame
        image = np.zeros((600, 800, 3), dtype=np.uint8)
        # Draw some UI elements
        cv2.rectangle(image, (50, 50), (150, 150), (255, 0, 0), -1)
        cv2.circle(image, (100, 100), 30, (0, 255, 0), -1)
        cv2.putText(image, "Frame {}".format(i), (200, 100),
                   cv2.FONT_HERSHEY_SIMPLEX, 1, (255, 255, 255), 2)

        frame = Frame(
            image=image,
            timestamp=float(i),
            frame_index=i,
            file_path=f"frame_{i}.png",
        )
        frames.append(frame)

    # Create a sample state
    state = create_sample_state(frames)

    # Create extractor with default config
    config = ImageExtractionConfig(
        min_size=(20, 20),
        max_size=(500, 500),
        extract_at_click_locations=True,
        click_region_padding=30,
    )
    extractor = StateImageExtractor(config)

    # Create sample events
    events = [
        InputEvent(timestamp=1.0, event_type="click", x=100, y=100, button="left"),
        InputEvent(timestamp=3.0, event_type="click", x=200, y=200, button="left"),
    ]

    # Extract images
    extracted_images = extractor.extract_from_state(state, frames, events)

    logger.info("Extracted %d images", len(extracted_images))
    for img in extracted_images:
        logger.info(
            "  - %s: bbox=%s, position=%s, type=%s, method=%s",
            img.name, img.bbox, img.position, img.position_type, img.extraction_method
        )

    return extracted_images


def example_contour_detection():
    """Example: Contour-based image extraction."""
    logger.info("=" * 80)
    logger.info("EXAMPLE: Contour Detection")
    logger.info("=" * 80)

    # Create a frame with distinct UI elements
    image = np.ones((600, 800, 3), dtype=np.uint8) * 240  # Light gray background

    # Draw some UI elements
    cv2.rectangle(image, (50, 50), (150, 150), (100, 100, 255), 2)  # Red box
    cv2.rectangle(image, (200, 50), (300, 150), (255, 100, 100), -1)  # Filled blue
    cv2.circle(image, (400, 100), 50, (100, 255, 100), 2)  # Green circle
    cv2.putText(image, "Button", (500, 100), cv2.FONT_HERSHEY_SIMPLEX,
               1, (0, 0, 0), 2)

    frame = Frame(
        image=image,
        timestamp=0.0,
        frame_index=0,
        file_path="test_frame.png",
    )

    # Create extractor
    config = ImageExtractionConfig(
        edge_detection="canny",
        min_contour_area=100,
        max_contours=10,
    )
    extractor = StateImageExtractor(config)

    # Detect contours
    bboxes = extractor.detect_contours(frame.image)

    logger.info("Detected %d contours", len(bboxes))
    for i, bbox in enumerate(bboxes):
        logger.info("  Contour %d: x=%d, y=%d, w=%d, h=%d", i, *bbox)

    return bboxes


def example_position_analysis():
    """Example: Fixed vs. dynamic position detection."""
    logger.info("=" * 80)
    logger.info("EXAMPLE: Position Analysis")
    logger.info("=" * 80)

    # Create a sample image
    image = np.zeros((100, 100, 3), dtype=np.uint8)
    cv2.circle(image, (50, 50), 30, (255, 255, 255), -1)

    state_image = StateImage(
        name="test_image",
        image=image,
        bbox=(0, 0, 100, 100),
        position_type="unknown",
        position=(0, 0),
    )

    # Test with fixed positions (all very close)
    fixed_occurrences = [(100, 100), (101, 100), (100, 101), (101, 101)]

    # Test with dynamic positions (spread out)
    dynamic_occurrences = [(100, 100), (150, 200), (50, 50), (200, 300)]

    config = ImageExtractionConfig(position_tolerance_px=5)
    extractor = StateImageExtractor(config)

    # Analyze fixed positions
    is_fixed = extractor.determine_position_type(state_image, fixed_occurrences)
    logger.info("Fixed positions analysis: %s (expected: True)", is_fixed)

    # Analyze dynamic positions
    is_fixed = extractor.determine_position_type(state_image, dynamic_occurrences)
    logger.info("Dynamic positions analysis: %s (expected: False)", is_fixed)


def example_save_and_load():
    """Example: Save and load StateImages."""
    logger.info("=" * 80)
    logger.info("EXAMPLE: Save and Load")
    logger.info("=" * 80)

    # Create a sample image
    image = np.zeros((100, 100, 3), dtype=np.uint8)
    cv2.rectangle(image, (10, 10), (90, 90), (255, 255, 255), -1)

    state_image = StateImage(
        name="test_save_load",
        image=image,
        bbox=(10, 10, 80, 80),
        position_type="fixed",
        position=(10, 10),
        extraction_method="manual",
        metadata={"test": True},
    )

    # Save to temp directory
    output_dir = Path("/tmp/qontinui_test")
    file_path = save_state_image(state_image, output_dir)
    logger.info("Saved image to %s", file_path)

    # Load back
    from analysis import load_state_image
    loaded_image = load_state_image(file_path)
    logger.info("Loaded image: %s", loaded_image.name)
    logger.info("  - bbox: %s", loaded_image.bbox)
    logger.info("  - position: %s", loaded_image.position)
    logger.info("  - Image shape: %s", loaded_image.image.shape)


def example_with_real_data(screenshots_dir: Path, events_file: Path):
    """Example: Extract images from real captured data.

    Args:
        screenshots_dir: Directory containing captured screenshots
        events_file: JSON file with input events
    """
    logger.info("=" * 80)
    logger.info("EXAMPLE: Real Data Processing")
    logger.info("=" * 80)

    # Load frames and events
    frames = load_frames_from_directory(screenshots_dir)
    events = load_events_from_json(events_file)

    if not frames:
        logger.error("No frames loaded")
        return

    # Create a state spanning all frames
    state = DetectedState(
        name="CapturedState",
        description="State from captured session",
        state_images=[],
        start_frame_index=0,
        end_frame_index=len(frames) - 1,
        frame_indices=list(range(len(frames))),
    )

    # Create extractor with custom config
    config = ImageExtractionConfig(
        min_size=(30, 30),
        max_size=(400, 400),
        extract_at_click_locations=True,
        click_region_padding=40,
        edge_detection="canny",
        canny_threshold1=50,
        canny_threshold2=150,
        position_tolerance_px=10,
    )
    extractor = StateImageExtractor(config)

    # Extract images
    extracted_images = extractor.extract_from_state(state, frames, events)

    logger.info("Extraction complete!")
    logger.info("  Total images extracted: %d", len(extracted_images))

    # Categorize by extraction method
    by_method = {}
    for img in extracted_images:
        method = img.extraction_method
        by_method[method] = by_method.get(method, 0) + 1

    logger.info("  By extraction method:")
    for method, count in by_method.items():
        logger.info("    - %s: %d", method, count)

    # Categorize by position type
    fixed_count = sum(1 for img in extracted_images if img.position_type == "fixed")
    dynamic_count = sum(1 for img in extracted_images if img.position_type == "dynamic")
    logger.info("  By position type:")
    logger.info("    - Fixed: %d", fixed_count)
    logger.info("    - Dynamic: %d", dynamic_count)

    # Save extracted images
    output_dir = screenshots_dir.parent / "extracted_images"
    output_dir.mkdir(parents=True, exist_ok=True)

    for img in extracted_images:
        save_state_image(img, output_dir)

    logger.info("Saved images to %s", output_dir)

    # Save metadata
    metadata = {
        "state": state.to_dict(),
        "images": [img.to_dict() for img in extracted_images],
        "config": {
            "min_size": config.min_size,
            "max_size": config.max_size,
            "edge_detection": config.edge_detection,
            "position_tolerance_px": config.position_tolerance_px,
        },
    }

    metadata_file = output_dir / "extraction_metadata.json"
    with open(metadata_file, "w") as f:
        json.dump(metadata, f, indent=2)

    logger.info("Saved metadata to %s", metadata_file)


def main():
    """Run all examples."""
    import sys

    logger.info("StateImage Extraction Service - Examples")
    logger.info("=" * 80)

    # Run synthetic examples
    try:
        example_basic_extraction()
    except Exception as e:
        logger.error("Error in basic extraction: %s", e)

    try:
        example_contour_detection()
    except Exception as e:
        logger.error("Error in contour detection: %s", e)

    try:
        example_position_analysis()
    except Exception as e:
        logger.error("Error in position analysis: %s", e)

    try:
        example_save_and_load()
    except Exception as e:
        logger.error("Error in save/load: %s", e)

    # Run with real data if provided
    if len(sys.argv) >= 3:
        screenshots_dir = Path(sys.argv[1])
        events_file = Path(sys.argv[2])

        if screenshots_dir.exists() and events_file.exists():
            try:
                example_with_real_data(screenshots_dir, events_file)
            except Exception as e:
                logger.error("Error processing real data: %s", e)
                import traceback
                traceback.print_exc()
        else:
            logger.warning("Invalid paths provided for real data")
    else:
        logger.info("")
        logger.info("To run with real captured data:")
        logger.info("  python example_usage.py <screenshots_dir> <events.json>")

    logger.info("=" * 80)
    logger.info("Examples complete!")


if __name__ == "__main__":
    main()
