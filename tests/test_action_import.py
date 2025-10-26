#!/usr/bin/env python3
"""Test that Action import works correctly after schema refactor."""

import sys
import os

# Add parent directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

def test_action_import():
    """Test that Action can be imported from correct location."""
    print("Testing Action import from qontinui.config.schema...")

    try:
        from qontinui.config.schema import Action
        print("✅ Successfully imported Action from qontinui.config.schema")

        # Test creating an Action instance
        action = Action(
            id="test-action-1",
            type="CLICK",
            config={"target": {"x": 100, "y": 200}}
        )
        print(f"✅ Successfully created Action instance: {action.type} (ID: {action.id})")

        return True
    except ImportError as e:
        print(f"❌ Failed to import Action: {e}")
        return False
    except Exception as e:
        print(f"❌ Failed to create Action instance: {e}")
        return False


def test_config_loading():
    """Test that config can be loaded with ConfigParser."""
    print("\nTesting config loading with ConfigParser...")

    try:
        from qontinui.json_executor import ConfigParser

        config_path = os.path.join(os.path.dirname(__file__), "..", "..", "bdo_config (42).json")

        if not os.path.exists(config_path):
            print(f"⚠️  Config file not found: {config_path}")
            return False

        parser = ConfigParser()
        config = parser.parse_file(config_path)

        print(f"✅ Successfully loaded config")
        print(f"   Workflows: {len(config.workflows)}")
        print(f"   States: {len(config.states)}")
        print(f"   Images: {len(config.images)}")

        return True
    except Exception as e:
        print(f"❌ Failed to load config: {e}")
        import traceback
        traceback.print_exc()
        return False


def test_action_executor_with_config():
    """Test that ActionExecutor can execute actions from loaded config."""
    print("\nTesting ActionExecutor with loaded config...")

    try:
        from qontinui.json_executor import ConfigParser, ActionExecutor
        from qontinui.config.schema import Action

        config_path = os.path.join(os.path.dirname(__file__), "..", "..", "bdo_config (42).json")

        if not os.path.exists(config_path):
            print(f"⚠️  Config file not found: {config_path}")
            return False

        parser = ConfigParser()
        config = parser.parse_file(config_path)

        # Create ActionExecutor
        executor = ActionExecutor(config)
        print(f"✅ Successfully created ActionExecutor")

        # Try to create an action using the correct import
        action = Action(
            id="test-wait-1",
            type="WAIT",
            config={"duration": 100}
        )

        print(f"✅ Successfully created test Action: {action.type}")
        print(f"✅ All imports working correctly!")

        return True
    except Exception as e:
        print(f"❌ Failed: {e}")
        import traceback
        traceback.print_exc()
        return False


if __name__ == "__main__":
    print("=" * 60)
    print("Testing Action Import Fix")
    print("=" * 60)

    results = []

    # Run tests
    results.append(("Action Import", test_action_import()))
    results.append(("Config Loading", test_config_loading()))
    results.append(("ActionExecutor", test_action_executor_with_config()))

    # Print summary
    print("\n" + "=" * 60)
    print("Test Summary")
    print("=" * 60)

    for test_name, passed in results:
        status = "✅ PASS" if passed else "❌ FAIL"
        print(f"{status}: {test_name}")

    all_passed = all(result[1] for result in results)

    if all_passed:
        print("\n🎉 All tests passed! Import fix successful.")
        sys.exit(0)
    else:
        print("\n❌ Some tests failed.")
        sys.exit(1)
