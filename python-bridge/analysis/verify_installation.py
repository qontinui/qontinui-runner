#!/usr/bin/env python3
"""Verify StateImage Extraction Service installation and dependencies.

Run this script to check that all required components are properly installed.
"""

import sys
from pathlib import Path


def check_python_version():
    """Check Python version."""
    print("Checking Python version...")
    version = sys.version_info
    if version.major >= 3 and version.minor >= 7:
        print(f"  ✓ Python {version.major}.{version.minor}.{version.micro} (OK)")
        return True
    else:
        print(f"  ✗ Python {version.major}.{version.minor}.{version.micro} (Need 3.7+)")
        return False


def check_dependencies():
    """Check required dependencies."""
    print("\nChecking dependencies...")

    dependencies = {
        "numpy": "numpy",
        "cv2": "opencv-python",
        "dataclasses": "dataclasses (built-in)",
    }

    all_ok = True
    for module, package in dependencies.items():
        try:
            if module == "dataclasses":
                # Built-in for Python 3.7+
                import dataclasses
                print(f"  ✓ {package}")
            elif module == "numpy":
                import numpy as np
                print(f"  ✓ {package} (version {np.__version__})")
            elif module == "cv2":
                import cv2
                print(f"  ✓ {package} (version {cv2.__version__})")
            else:
                __import__(module)
                print(f"  ✓ {package}")
        except ImportError:
            print(f"  ✗ {package} (MISSING)")
            all_ok = False

    return all_ok


def check_models():
    """Check models module."""
    print("\nChecking models module...")

    try:
        from models import DetectedState, Frame, InputEvent, StateImage
        print("  ✓ models.DetectedState")
        print("  ✓ models.Frame")
        print("  ✓ models.InputEvent")
        print("  ✓ models.StateImage")
        return True
    except ImportError as e:
        print(f"  ✗ models module (ERROR: {e})")
        return False


def check_analysis_module():
    """Check analysis module."""
    print("\nChecking analysis module...")

    try:
        from analysis import (
            ImageExtractionConfig,
            StateImageExtractor,
            save_state_image,
            load_state_image,
        )
        print("  ✓ analysis.ImageExtractionConfig")
        print("  ✓ analysis.StateImageExtractor")
        print("  ✓ analysis.save_state_image")
        print("  ✓ analysis.load_state_image")
        return True
    except ImportError as e:
        print(f"  ✗ analysis module (ERROR: {e})")
        return False


def check_files():
    """Check required files exist."""
    print("\nChecking required files...")

    base_dir = Path(__file__).parent.parent
    required_files = [
        "models/__init__.py",
        "models/state_models.py",
        "analysis/__init__.py",
        "analysis/image_extractor.py",
        "analysis/example_usage.py",
        "analysis/README.md",
        "analysis/INTEGRATION_GUIDE.md",
    ]

    all_ok = True
    for file_path in required_files:
        full_path = base_dir / file_path
        if full_path.exists():
            print(f"  ✓ {file_path}")
        else:
            print(f"  ✗ {file_path} (MISSING)")
            all_ok = False

    return all_ok


def run_basic_test():
    """Run a basic functionality test."""
    print("\nRunning basic functionality test...")

    try:
        import numpy as np
        from models import DetectedState, Frame, InputEvent
        from analysis import ImageExtractionConfig, StateImageExtractor

        # Create test frame
        image = np.ones((600, 800, 3), dtype=np.uint8) * 240
        frame = Frame(
            image=image,
            timestamp=0.0,
            frame_index=0,
            file_path="test.png",
        )

        # Create test state
        state = DetectedState(
            name="TestState",
            description="Test state",
            state_images=[],
            start_frame_index=0,
            end_frame_index=0,
            frame_indices=[0],
            click_locations=[(100, 100)],
        )

        # Create test event
        event = InputEvent(
            timestamp=0.0,
            event_type="click",
            x=100,
            y=100,
            button="left",
        )

        # Create extractor and extract
        config = ImageExtractionConfig(
            min_size=(20, 20),
            extract_at_click_locations=True,
            click_region_padding=30,
        )
        extractor = StateImageExtractor(config)

        extracted = extractor.extract_from_state(state, [frame], [event])

        print(f"  ✓ Basic extraction test passed (extracted {len(extracted)} images)")
        return True

    except Exception as e:
        print(f"  ✗ Basic test failed: {e}")
        import traceback
        traceback.print_exc()
        return False


def main():
    """Run all verification checks."""
    print("=" * 80)
    print("StateImage Extraction Service - Installation Verification")
    print("=" * 80)

    results = {
        "Python version": check_python_version(),
        "Dependencies": check_dependencies(),
        "Models module": check_models(),
        "Analysis module": check_analysis_module(),
        "Required files": check_files(),
        "Basic functionality": run_basic_test(),
    }

    print("\n" + "=" * 80)
    print("VERIFICATION SUMMARY")
    print("=" * 80)

    all_passed = True
    for check, passed in results.items():
        status = "✓ PASS" if passed else "✗ FAIL"
        print(f"{check:.<50} {status}")
        if not passed:
            all_passed = False

    print("=" * 80)

    if all_passed:
        print("\n✓ All checks passed! Installation is complete and working.")
        print("\nNext steps:")
        print("  1. Read the README.md for detailed documentation")
        print("  2. Run example_usage.py to see usage examples")
        print("  3. Read INTEGRATION_GUIDE.md for integration instructions")
        print("  4. Run tests with: pytest analysis/test_image_extractor.py")
        return 0
    else:
        print("\n✗ Some checks failed. Please fix the issues above.")
        print("\nCommon solutions:")
        print("  - Install missing dependencies: pip install numpy opencv-python")
        print("  - Check Python path includes python-bridge directory")
        print("  - Verify all files are present and not corrupted")
        return 1


if __name__ == "__main__":
    sys.exit(main())
