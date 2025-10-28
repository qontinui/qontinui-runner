"""Test GO_TO_STATE with debug logging to find freeze point."""

import sys
import logging

# Set up detailed logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)

# Add parent directory to path
sys.path.insert(0, '../qontinui/src')

def test_go_to_state_with_debug():
    """Test GO_TO_STATE action with debug logging."""
    from qontinui.json_executor.config_parser import ConfigParser
    from qontinui.json_executor.action_executor import ActionExecutor

    print("\n=== Loading Config ===")
    parser = ConfigParser()
    config = parser.parse_file("../bdo_config (42).json")

    print(f"Loaded config: {len(config.workflows)} workflows, {len(config.states)} states")

    # Initialize ActionExecutor
    print("\n=== Initializing ActionExecutor ===")
    action_executor = ActionExecutor(config, enable_gui=False)

    print("\n=== Executing GO_TO_STATE ===")
    print("This should show debug logs from:")
    print("  - navigation_api.open_states()")
    print("  - pathfinding_navigator.navigate_to_states()")
    print("  - pathfinding_navigator.find_path_to_states()")
    print("  - enhanced_transition_executor.execute_transition()")
    print("  - enhanced_transition_executor._execute_single_incoming()")
    print("\nWatching for freeze...\n")

    try:
        # Create a GO_TO_STATE action
        from qontinui.model.action.config_action import ConfigAction

        action = ConfigAction(
            id="test-go-to-state",
            type="GO_TO_STATE",
            config={
                "states": ["Processing"]
            }
        )

        # Execute the action
        result = action_executor.execute_action(action)

        print(f"\n=== Result ===")
        print(f"Success: {result}")

    except KeyboardInterrupt:
        print("\n\n=== INTERRUPTED ===")
        print("Freeze detected! Last log line shows where it hung.")
    except Exception as e:
        print(f"\n=== ERROR ===")
        print(f"Exception: {e}")
        import traceback
        traceback.print_exc()

if __name__ == "__main__":
    test_go_to_state_with_debug()
