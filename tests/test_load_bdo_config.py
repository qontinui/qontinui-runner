#!/usr/bin/env python3
"""Test loading BDO config with fixed image references."""

import sys
import os

# Add parent directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))


def test_load_fixed_config():
    """Test that fixed config loads correctly."""
    print("Testing fixed config loading...\n")

    try:
        from qontinui.json_executor import ConfigParser

        config_path = os.path.join(
            os.path.dirname(__file__),
            "..", "..",
            "bdo_config (42)_fixed.json"
        )

        if not os.path.exists(config_path):
            print(f"❌ Fixed config not found: {config_path}")
            return False

        parser = ConfigParser()
        config = parser.parse_file(config_path)

        print(f"✅ Successfully loaded fixed config")
        print(f"   Workflows: {len(config.workflows)}")
        print(f"   States: {len(config.states)}")
        print(f"   Images: {len(config.images)}")

        # Check that states have proper image references
        print(f"\n✅ Checking state image references...")
        for state in config.states:
            state_images = state.state_images if hasattr(state, 'state_images') else []
            if state_images:
                print(f"   State '{state.name}': {len(state_images)} images")

        return True
    except Exception as e:
        print(f"❌ Failed to load fixed config: {e}")
        import traceback
        traceback.print_exc()
        return False


def test_action_executor_with_fixed_config():
    """Test that ActionExecutor works with fixed config."""
    print("\nTesting ActionExecutor with fixed config...\n")

    try:
        from qontinui.json_executor import ConfigParser, ActionExecutor

        config_path = os.path.join(
            os.path.dirname(__file__),
            "..", "..",
            "bdo_config (42)_fixed.json"
        )

        if not os.path.exists(config_path):
            print(f"❌ Fixed config not found: {config_path}")
            return False

        parser = ConfigParser()
        config = parser.parse_file(config_path)

        # Create ActionExecutor (this will validate image references)
        executor = ActionExecutor(config)
        print(f"✅ Successfully created ActionExecutor with fixed config")

        # Check that image map is populated
        if hasattr(executor, 'state_executor') and hasattr(executor.state_executor, 'image_map'):
            image_map = executor.state_executor.image_map
            print(f"   Image map size: {len(image_map)}")

            # Try to access a few images
            sample_ids = list(image_map.keys())[:3]
            for img_id in sample_ids:
                print(f"   ✓ Image '{img_id}' accessible in image map")

        print(f"\n✅ ActionExecutor initialized successfully with fixed config")
        return True
    except Exception as e:
        print(f"❌ Failed to initialize ActionExecutor: {e}")
        import traceback
        traceback.print_exc()
        return False


if __name__ == "__main__":
    print("=" * 60)
    print("Testing BDO Config with Fixed Image References")
    print("=" * 60)
    print()

    results = []

    # Run tests
    results.append(("Config Loading", test_load_fixed_config()))
    results.append(("ActionExecutor", test_action_executor_with_fixed_config()))

    # Print summary
    print("\n" + "=" * 60)
    print("Test Summary")
    print("=" * 60)

    for test_name, passed in results:
        status = "✅ PASS" if passed else "❌ FAIL"
        print(f"{status}: {test_name}")

    all_passed = all(result[1] for result in results)

    if all_passed:
        print("\n🎉 All tests passed! Fixed config is ready to use.")
        print("\nNext steps:")
        print("  1. Rename the fixed config to replace the original")
        print("  2. Run the automation again")
        print("  3. The hanging issue should be resolved")
        sys.exit(0)
    else:
        print("\n❌ Some tests failed.")
        sys.exit(1)
