"""Minimal test to isolate GO_TO_STATE freeze."""

import sys
sys.path.insert(0, '../qontinui/src')

print("=" * 80)
print("MINIMAL GO_TO_STATE TEST")
print("=" * 80)

print("\n1. Importing modules...")
from qontinui.json_executor.config_parser import ConfigParser
from qontinui.json_executor.action_executor import ActionExecutor
from qontinui.model.action.config_action import ConfigAction

print("2. Loading config...")
parser = ConfigParser()
config = parser.parse_file("../bdo_config (43).json")
print(f"   Loaded: {len(config.workflows)} workflows, {len(config.states)} states")

print("\n3. Creating ActionExecutor...")
action_executor = ActionExecutor(config, enable_gui=False)
print(f"   ActionExecutor created: {action_executor}")
print(f"   ActionExecutor type: {type(action_executor)}")

print("\n4. Creating GO_TO_STATE action...")
action = ConfigAction(
    id="test-minimal",
    type="GO_TO_STATE",
    config={"states": ["Processing"]}
)
print(f"   Action: {action}")

print("\n5. About to call execute_action...")
print("   If this hangs, the issue is in the library's execute_action method.")
print("   Watching for any output...\n")

try:
    result = action_executor.execute_action(action)
    print(f"\n6. *** SUCCESS - execute_action returned: {result} ***")
except KeyboardInterrupt:
    print("\n\n*** INTERRUPTED - User pressed Ctrl+C ***")
    print("This confirms the freeze is happening.")
except Exception as e:
    print(f"\n\n*** EXCEPTION: {type(e).__name__}: {e} ***")
    import traceback
    traceback.print_exc()

print("\n" + "=" * 80)
print("TEST COMPLETED")
print("=" * 80)
