"""Test that runtime metadata flows correctly from library events to tree events.

This test verifies that:
1. TYPE actions include typed_text in runtime metadata
2. RUN_WORKFLOW actions include workflow_name in runtime metadata
3. Tree events emitted to Tauri include complete runtime data
4. The data structure matches what the frontend expects
"""

import json
import sys
import time
from io import StringIO
from unittest.mock import MagicMock, Mock, patch

import pytest

from execution_tree import ExecutionNode, ExecutionTree
from models.action_execution_record import ActionExecutionRecord
from services.unified_data_collector import UnifiedDataCollector


class TestRuntimeMetadataFlow:
    """Test suite for runtime metadata flow from Python to Tauri."""

    def test_type_action_runtime_metadata(self):
        """Verify TYPE action includes typed_text in runtime metadata."""
        # Setup
        collector = UnifiedDataCollector(state_memory=None, screenshot_service=None)

        # Simulate TYPE action execution
        collector.start_action(
            action_id="type-001",
            parent_action_id=None,
            workflow_id="test-workflow",
            nesting_level=0,
        )

        # Library emits TEXT_TYPED event -> EventTranslator calls record_text_typed
        typed_text = "hello@example.com"
        collector.record_text_typed(typed_text)

        # Create record
        start_time = time.time()
        end_time = start_time + 1.0

        record = collector.create_record(
            action_type="TYPE",
            config={
                "textSource": {"stateId": "Main", "stringIds": ["string-123"]},
                "typing_delay": 50,
            },
            start_time=start_time,
            end_time=end_time,
            success=True,
        )

        # Verify record has typed_text
        assert record.typed_text == typed_text, "Record should contain typed text"

        # Convert to tree event format (what gets emitted to Tauri)
        tree_event_data = record.to_tree_event_data()

        # Verify tree event has runtime metadata with typed_text
        assert "metadata" in tree_event_data
        assert "runtime" in tree_event_data["metadata"]
        assert "typed_text" in tree_event_data["metadata"]["runtime"]
        assert tree_event_data["metadata"]["runtime"]["typed_text"] == typed_text

        # Verify config is also present
        assert "config" in tree_event_data["metadata"]
        assert tree_event_data["metadata"]["config"]["textSource"]["stateId"] == "Main"

        print(f"✓ TYPE action runtime metadata correct:")
        print(f"  - typed_text: {typed_text}")
        print(f"  - config present: {tree_event_data['metadata']['config']}")

    def test_run_workflow_runtime_metadata(self):
        """Verify RUN_WORKFLOW action includes workflow_name in runtime metadata."""
        # Setup
        collector = UnifiedDataCollector(state_memory=None, screenshot_service=None)

        # Simulate RUN_WORKFLOW action execution
        collector.start_action(
            action_id="run-workflow-001",
            parent_action_id=None,
            workflow_id="parent-workflow",
            nesting_level=0,
        )

        # Record workflow execution (called by qontinui_executor.py)
        workflow_name = "Login Flow"
        workflow_status = "success"
        collector.record_workflow_execution(
            workflow_name=workflow_name, workflow_status=workflow_status
        )

        # Create record
        start_time = time.time()
        end_time = start_time + 2.0

        record = collector.create_record(
            action_type="RUN_WORKFLOW",
            config={"workflowId": "workflow-456"},
            start_time=start_time,
            end_time=end_time,
            success=True,
        )

        # Verify record has workflow metadata
        assert record.workflow_name == workflow_name, "Record should contain workflow name"
        assert record.workflow_status == workflow_status, "Record should contain workflow status"

        # Convert to tree event format
        tree_event_data = record.to_tree_event_data()

        # Verify tree event has runtime metadata with workflow info
        assert "metadata" in tree_event_data
        assert "runtime" in tree_event_data["metadata"]
        assert "workflow_name" in tree_event_data["metadata"]["runtime"]
        assert tree_event_data["metadata"]["runtime"]["workflow_name"] == workflow_name
        assert tree_event_data["metadata"]["runtime"]["workflow_status"] == workflow_status

        print(f"✓ RUN_WORKFLOW runtime metadata correct:")
        print(f"  - workflow_name: {workflow_name}")
        print(f"  - workflow_status: {workflow_status}")

    def test_tree_event_json_serialization(self):
        """Verify tree events can be serialized to JSON for Tauri."""
        # Setup
        collector = UnifiedDataCollector(state_memory=None, screenshot_service=None)

        # TYPE action
        collector.start_action("type-001", None, "test-wf", 0)
        collector.record_text_typed("test123")

        start = time.time()
        record = collector.create_record(
            action_type="TYPE",
            config={"textSource": {"stateId": "Main", "stringIds": ["str-1"]}},
            start_time=start,
            end_time=start + 0.5,
            success=True,
        )

        # Convert to tree event data
        event_data = record.to_tree_event_data()

        # Create ExecutionNode (simulating qontinui_executor)
        node = ExecutionNode(
            id="type-001",
            node_type="action",
            name="TYPE",
            timestamp=start,
            end_timestamp=start + 0.5,
            metadata={"config": record.config, "execution_record": event_data},
            status="success",
        )

        # Simulate tree event emission (what qontinui_executor._emit_tree_event does)
        tree_event = {
            "type": "tree_event",
            "event_type": "action_completed",
            "node": node.to_dict(include_children=False),
        }

        # Serialize to JSON (what gets sent to Tauri)
        json_output = json.dumps(tree_event, indent=2)

        # Parse back to verify it's valid JSON
        parsed = json.loads(json_output)

        # Verify structure
        assert parsed["type"] == "tree_event"
        assert parsed["event_type"] == "action_completed"
        assert parsed["node"]["name"] == "TYPE"
        assert (
            parsed["node"]["metadata"]["execution_record"]["metadata"]["runtime"]["typed_text"]
            == "test123"
        )

        print("✓ Tree event JSON serialization successful")
        print(f"  Sample output: {json_output[:200]}...")

    def test_complete_type_action_flow(self):
        """Test complete flow: library event -> collector -> record -> tree event -> JSON."""

        # 1. Setup collector
        collector = UnifiedDataCollector(state_memory=None, screenshot_service=None)

        # 2. Start action (qontinui_executor calls this)
        action_id = "type-complete-001"
        collector.start_action(action_id, None, "login-workflow", 0)

        # 3. Library emits TEXT_TYPED event -> EventTranslator -> collector
        typed_text = "l"  # The single character from the user's example
        collector.record_text_typed(typed_text)

        # 4. Create record (qontinui_executor calls this after action completes)
        config = {
            "clear_before": False,
            "press_enter": False,
            "textSource": {
                "stateId": "Main",
                "stringIds": ["string-1759325036257"],
                "useAll": False,
            },
            "typing_delay": 50,
        }

        start_time = time.time()
        end_time = start_time + 0.1

        record = collector.create_record(
            action_type="TYPE",
            config=config,
            start_time=start_time,
            end_time=end_time,
            success=True,
        )

        # 5. Convert to tree event data
        tree_data = record.to_tree_event_data()

        # 6. Create execution node and tree event
        node = ExecutionNode(
            id=action_id,
            node_type="action",
            name="TYPE",
            timestamp=start_time,
            end_timestamp=end_time,
            metadata={"config": config, "execution_record": tree_data},
            status="success",
        )

        # 7. Build final tree event (what gets printed to stdout for Tauri)
        tree_event = {
            "type": "tree_event",
            "event_type": "action_completed",
            "node": node.to_dict(include_children=False),
        }

        # 8. Verify the JSON that Tauri receives
        json_str = json.dumps(tree_event)
        parsed = json.loads(json_str)

        # Critical assertions - what frontend expects
        runtime = parsed["node"]["metadata"]["execution_record"]["metadata"]["runtime"]
        config_received = parsed["node"]["metadata"]["config"]

        print("\n" + "=" * 60)
        print("COMPLETE TYPE ACTION FLOW TEST")
        print("=" * 60)

        # Verify typed text is in runtime
        assert "typed_text" in runtime, "Runtime must include typed_text"
        assert (
            runtime["typed_text"] == "l"
        ), f"Expected typed_text='l', got '{runtime.get('typed_text')}'"
        print(f"✓ Typed text in runtime: '{runtime['typed_text']}'")

        # Verify config is complete
        assert config_received == config, "Config should be complete"
        print(f"✓ Config complete:")
        print(f"  - textSource.stateId: {config_received['textSource']['stateId']}")
        print(f"  - textSource.stringIds: {config_received['textSource']['stringIds']}")

        # Verify frontend can access the data
        print(f"\n✓ Frontend access paths:")
        print(
            f"  - node.metadata.config.textSource.stateId = '{config_received['textSource']['stateId']}'"
        )
        print(
            f"  - node.metadata.execution_record.metadata.runtime.typed_text = '{runtime['typed_text']}'"
        )
        print(f"  - OR node.metadata.runtime.typed_text = (if nested)")

        # Check if runtime is directly in metadata (it should be in execution_record.metadata)
        if "runtime" in parsed["node"]["metadata"]:
            print(
                f"  - Direct: node.metadata.runtime.typed_text = '{parsed['node']['metadata']['runtime'].get('typed_text')}'"
            )

        print("=" * 60 + "\n")

        return tree_event

    def test_missing_typed_text_detection(self):
        """Test that we can detect when typed_text is missing."""

        # Create a TYPE action WITHOUT calling record_text_typed
        collector = UnifiedDataCollector(state_memory=None, screenshot_service=None)
        collector.start_action("type-missing-001", None, "test-wf", 0)

        # DON'T call record_text_typed - simulating the bug

        record = collector.create_record(
            action_type="TYPE",
            config={"textSource": {"stateId": "Main"}},
            start_time=time.time(),
            end_time=time.time() + 0.1,
            success=True,
        )

        tree_data = record.to_tree_event_data()
        runtime = tree_data["metadata"]["runtime"]

        # This should fail if typed_text is missing
        if "typed_text" not in runtime:
            print("✗ BUG DETECTED: typed_text missing from runtime metadata!")
            print(f"  Runtime keys: {list(runtime.keys())}")
            print("  This means EventTranslator.on_text_typed() was not called")
            print("  OR record_text_typed() was not called")
            assert False, "typed_text is missing - library not emitting TEXT_TYPED events"
        else:
            print(f"✓ typed_text present (value: {runtime.get('typed_text')})")


if __name__ == "__main__":
    """Run tests manually for debugging."""
    test = TestRuntimeMetadataFlow()

    print("\n" + "=" * 60)
    print("Running Runtime Metadata Flow Tests")
    print("=" * 60 + "\n")

    try:
        test.test_type_action_runtime_metadata()
        print()

        test.test_run_workflow_runtime_metadata()
        print()

        test.test_tree_event_json_serialization()
        print()

        tree_event = test.test_complete_type_action_flow()

        # Print the actual JSON that would be sent to Tauri
        print("\nFULL JSON OUTPUT (what Tauri receives):")
        print("-" * 60)
        print(json.dumps(tree_event, indent=2))
        print("-" * 60)

        test.test_missing_typed_text_detection()

        print("\n" + "=" * 60)
        print("ALL TESTS PASSED ✓")
        print("=" * 60)

    except AssertionError as e:
        print(f"\n✗ TEST FAILED: {e}")
        import traceback

        traceback.print_exc()
        sys.exit(1)
