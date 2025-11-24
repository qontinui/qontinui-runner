#!/usr/bin/env python3
"""
State Detection Bridge Script for qontinui-runner

This script serves as a bridge between the TypeScript StateDetectionService
and the Python qontinui library for state detection analysis.

Usage:
    python state_detection_bridge.py <session_path> <config_json>

Arguments:
    session_path: Path to directory containing screenshot images
    config_json: JSON string with analysis configuration

Output:
    Prints JSON to stdout with detection results
"""

import json
import os
import sys
import traceback
from pathlib import Path
from typing import Any

# Add qontinui library to path if needed
# sys.path.insert(0, str(Path(__file__).parent.parent.parent / "qontinui" / "src"))

try:
    import numpy as np
    from PIL import Image

    # Import qontinui detection modules
    # from qontinui.discovery.state_detection.detector import StateDetector
    # from qontinui.discovery.state_construction.state_builder import StateBuilder
    # from qontinui.discovery.models import StateImage, DiscoveredState

except ImportError as e:
    print(json.dumps({
        "success": False,
        "error": f"Failed to import required modules: {str(e)}"
    }), file=sys.stdout)
    sys.exit(1)


def load_screenshots(session_path: str) -> list[tuple[str, np.ndarray]]:
    """Load all screenshots from the session directory.

    Args:
        session_path: Path to directory containing screenshots

    Returns:
        List of tuples (filename, image_array)
    """
    screenshots = []
    session_dir = Path(session_path)

    if not session_dir.exists():
        raise FileNotFoundError(f"Session path does not exist: {session_path}")

    # Supported image formats
    image_extensions = {'.png', '.jpg', '.jpeg', '.gif', '.bmp'}

    # Load all images
    for img_path in sorted(session_dir.glob('*')):
        if img_path.suffix.lower() in image_extensions:
            try:
                img = Image.open(img_path)
                img_array = np.array(img)
                screenshots.append((img_path.name, img_array))
            except Exception as e:
                print(f"Warning: Failed to load {img_path.name}: {str(e)}", file=sys.stderr)
                continue

    return screenshots


def parse_config(config_json: str) -> dict[str, Any]:
    """Parse and validate configuration JSON.

    Args:
        config_json: JSON string with configuration

    Returns:
        Configuration dictionary
    """
    try:
        config = json.loads(config_json)
        return config
    except json.JSONDecodeError as e:
        raise ValueError(f"Invalid configuration JSON: {str(e)}")


def analyze_session(screenshots: list[tuple[str, np.ndarray]], config: dict[str, Any]) -> dict[str, Any]:
    """Run state detection analysis on screenshots.

    Args:
        screenshots: List of (filename, image_array) tuples
        config: Analysis configuration

    Returns:
        Analysis results dictionary
    """
    # TODO: Replace with actual qontinui library calls
    # This is a placeholder implementation

    # Example: Initialize detector with config
    # detector = StateDetector(
    #     min_region_size=tuple(config.get("min_region_size", [20, 20])),
    #     max_region_size=tuple(config.get("max_region_size", [500, 500])),
    #     stability_threshold=config.get("stability_threshold", 0.98),
    #     # ... other config options
    # )

    # Example: Run detection
    # state_images = detector.detect_elements(screenshots)
    # states = StateBuilder().build_states(state_images)

    # Placeholder results
    state_images = [
        {
            "id": "img_001",
            "name": "Example Button",
            "x": 100,
            "y": 200,
            "x2": 200,
            "y2": 250,
            "width": 100,
            "height": 50,
            "pixel_hash": "abc123def456",
            "frequency": 0.95,
            "screenshots": [name for name, _ in screenshots[:10]],
            "tags": ["button"],
            "dark_pixel_percentage": 0.3,
            "light_pixel_percentage": 0.7,
            "mask_density": 1.0,
            "has_mask": False
        }
    ]

    states = [
        {
            "id": "state_001",
            "name": "Main Menu",
            "state_image_ids": ["img_001"],
            "screenshots": [name for name, _ in screenshots[:5]],
            "confidence": 0.92,
            "metadata": {
                "element_count": 1,
                "transition_count": 0
            }
        }
    ]

    return {
        "success": True,
        "state_images": state_images,
        "states": states,
        "total_screenshots": len(screenshots),
        "metadata": {
            "processing_mode": config.get("processing_mode", "full"),
            "config": config
        }
    }


def main():
    """Main entry point for the bridge script."""
    try:
        # Check command-line arguments
        if len(sys.argv) != 3:
            raise ValueError(
                f"Expected 2 arguments, got {len(sys.argv) - 1}. "
                "Usage: python state_detection_bridge.py <session_path> <config_json>"
            )

        session_path = sys.argv[1]
        config_json = sys.argv[2]

        # Parse configuration
        config = parse_config(config_json)

        # Log configuration (to stderr so it doesn't interfere with JSON output)
        print(f"[Bridge] Starting analysis for session: {session_path}", file=sys.stderr)
        print(f"[Bridge] Configuration: {json.dumps(config, indent=2)}", file=sys.stderr)

        # Load screenshots
        print(f"[Bridge] Loading screenshots...", file=sys.stderr)
        screenshots = load_screenshots(session_path)
        print(f"[Bridge] Loaded {len(screenshots)} screenshots", file=sys.stderr)

        if not screenshots:
            raise ValueError(f"No screenshots found in {session_path}")

        # Run analysis
        print(f"[Bridge] Running state detection analysis...", file=sys.stderr)
        results = analyze_session(screenshots, config)

        # Output results as JSON to stdout
        print(json.dumps(results, indent=2), file=sys.stdout)

        print(f"[Bridge] Analysis complete!", file=sys.stderr)
        print(f"[Bridge] Found {len(results['state_images'])} elements", file=sys.stderr)
        print(f"[Bridge] Detected {len(results['states'])} states", file=sys.stderr)

        sys.exit(0)

    except Exception as e:
        # Print error as JSON to stdout
        error_result = {
            "success": False,
            "error": str(e),
            "traceback": traceback.format_exc()
        }
        print(json.dumps(error_result, indent=2), file=sys.stdout)

        # Also log to stderr for debugging
        print(f"[Bridge] ERROR: {str(e)}", file=sys.stderr)
        print(traceback.format_exc(), file=sys.stderr)

        sys.exit(1)


if __name__ == "__main__":
    main()
