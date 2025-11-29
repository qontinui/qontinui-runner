"""Comprehensive unit tests for ActionExecutionRecord.

This module tests the immutable ActionExecutionRecord dataclass:
- Record creation with minimal and maximal fields
- Immutability (FrozenInstanceError on modification)
- Computed properties (duration_seconds, state_changed, states_activated, states_deactivated)
- Conversion to tree event format
- Runtime data formatting for different action types
- State change detection with various scenarios
"""

import time
from dataclasses import FrozenInstanceError

import pytest

from models.action_execution_record import ActionExecutionRecord


class TestActionExecutionRecordCreation:
    """Test creating ActionExecutionRecord with various configurations."""

    def test_create_with_minimal_fields(self):
        """Test creating record with only required fields."""
        record = ActionExecutionRecord(action_id="test-001", action_type="CLICK")

        assert record.action_id == "test-001"
        assert record.action_type == "CLICK"
        assert record.config == {}
        assert record.success is True
        assert record.error_message is None
        assert record.retry_count == 0
        assert record.active_states_before == frozenset()
        assert record.active_states_after == frozenset()

    def test_create_with_all_fields(self):
        """Test creating record with all fields populated."""
        start = time.time()
        end = start + 0.5

        record = ActionExecutionRecord(
            action_id="test-002",
            action_type="FIND",
            config={"image": "button.png", "threshold": 0.8},
            start_time=start,
            end_time=end,
            duration_ms=500.0,
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login", "Dashboard"]),
            success=True,
            error_message=None,
            retry_count=2,
            typed_text="hello",
            match_summary={"confidence": 0.95, "found": True},
            clicked_location=(100, 200),
            click_button="left",
            click_target_type="button",
            transition_data={"from": "Login", "to": "Dashboard"},
            condition_result=True,
            branch_taken="then",
            parent_action_id="parent-001",
            workflow_id="workflow-001",
            nesting_level=1,
            screenshot_reference="screenshots/test.png",
            visual_debug_reference="debug/test.png",
            metadata={"custom": "data"},
        )

        assert record.action_id == "test-002"
        assert record.action_type == "FIND"
        assert record.config["image"] == "button.png"
        assert record.start_time == start
        assert record.end_time == end
        assert record.duration_ms == 500.0
        assert "Login" in record.active_states_before
        assert "Dashboard" in record.active_states_after
        assert record.typed_text == "hello"
        assert record.match_summary["confidence"] == 0.95
        assert record.clicked_location == (100, 200)
        assert record.click_button == "left"
        assert record.parent_action_id == "parent-001"
        assert record.workflow_id == "workflow-001"
        assert record.nesting_level == 1
        assert record.metadata["custom"] == "data"


class TestActionExecutionRecordImmutability:
    """Test that ActionExecutionRecord is truly immutable."""

    def test_cannot_modify_action_id(self):
        """Test that action_id cannot be modified after creation."""
        record = ActionExecutionRecord(action_id="test-001", action_type="CLICK")

        with pytest.raises(FrozenInstanceError):
            record.action_id = "new-id"

    def test_cannot_modify_action_type(self):
        """Test that action_type cannot be modified after creation."""
        record = ActionExecutionRecord(action_id="test-001", action_type="CLICK")

        with pytest.raises(FrozenInstanceError):
            record.action_type = "TYPE"

    def test_cannot_modify_success(self):
        """Test that success flag cannot be modified after creation."""
        record = ActionExecutionRecord(action_id="test-001", action_type="CLICK", success=True)

        with pytest.raises(FrozenInstanceError):
            record.success = False

    def test_cannot_modify_active_states(self):
        """Test that active states cannot be modified after creation."""
        record = ActionExecutionRecord(
            action_id="test-001", action_type="CLICK", active_states_before=frozenset(["Login"])
        )

        with pytest.raises(FrozenInstanceError):
            record.active_states_before = frozenset(["Dashboard"])

    def test_config_dict_is_separate_instance(self):
        """Test that modifying source dict doesn't affect record."""
        config = {"x": 100, "y": 200}
        record = ActionExecutionRecord(action_id="test-001", action_type="CLICK", config=config)

        # Modify source dict
        config["x"] = 500

        # Record should be unaffected (defensive copy)
        assert record.config["x"] == 100


class TestComputedProperties:
    """Test computed properties of ActionExecutionRecord."""

    def test_duration_seconds_with_duration_ms(self):
        """Test duration_seconds calculation from duration_ms."""
        record = ActionExecutionRecord(
            action_id="test-001", action_type="CLICK", duration_ms=1500.0
        )

        assert record.duration_seconds == 1.5

    def test_duration_seconds_without_duration_ms(self):
        """Test duration_seconds returns None when duration_ms is None."""
        record = ActionExecutionRecord(action_id="test-001", action_type="CLICK", duration_ms=None)

        assert record.duration_seconds is None

    def test_duration_seconds_zero(self):
        """Test duration_seconds with zero duration."""
        record = ActionExecutionRecord(action_id="test-001", action_type="CLICK", duration_ms=0.0)

        assert record.duration_seconds == 0.0

    def test_state_changed_true(self):
        """Test state_changed is True when states differ."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Dashboard"]),
        )

        assert record.state_changed is True

    def test_state_changed_false_same_states(self):
        """Test state_changed is False when states are identical."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="CLICK",
            active_states_before=frozenset(["Login", "MainMenu"]),
            active_states_after=frozenset(["Login", "MainMenu"]),
        )

        assert record.state_changed is False

    def test_state_changed_false_both_empty(self):
        """Test state_changed is False when both state sets are empty."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="CLICK",
            active_states_before=frozenset(),
            active_states_after=frozenset(),
        )

        assert record.state_changed is False

    def test_states_activated_single(self):
        """Test states_activated with single new state."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login", "Dashboard"]),
        )

        assert record.states_activated == frozenset(["Dashboard"])

    def test_states_activated_multiple(self):
        """Test states_activated with multiple new states."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login", "Dashboard", "Settings"]),
        )

        assert record.states_activated == frozenset(["Dashboard", "Settings"])

    def test_states_activated_none(self):
        """Test states_activated returns empty set when no states activated."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="CLICK",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login"]),
        )

        assert record.states_activated == frozenset()

    def test_states_deactivated_single(self):
        """Test states_deactivated with single removed state."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login", "Dashboard"]),
            active_states_after=frozenset(["Dashboard"]),
        )

        assert record.states_deactivated == frozenset(["Login"])

    def test_states_deactivated_multiple(self):
        """Test states_deactivated with multiple removed states."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login", "Dashboard", "Settings"]),
            active_states_after=frozenset(["Dashboard"]),
        )

        assert record.states_deactivated == frozenset(["Login", "Settings"])

    def test_states_deactivated_none(self):
        """Test states_deactivated returns empty set when no states deactivated."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="CLICK",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login"]),
        )

        assert record.states_deactivated == frozenset()

    def test_simultaneous_activation_and_deactivation(self):
        """Test state changes when some states activate and others deactivate."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login", "Form"]),
            active_states_after=frozenset(["Dashboard", "Settings"]),
        )

        assert record.state_changed is True
        assert record.states_activated == frozenset(["Dashboard", "Settings"])
        assert record.states_deactivated == frozenset(["Login", "Form"])


class TestToTreeEventData:
    """Test conversion to tree event format."""

    def test_to_tree_event_data_success(self):
        """Test conversion for successful action."""
        start = time.time()
        end = start + 0.5

        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="CLICK",
            config={"x": 100, "y": 200},
            start_time=start,
            end_time=end,
            duration_ms=500.0,
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login", "Dashboard"]),
            success=True,
            parent_action_id="parent-001",
            workflow_id="workflow-001",
            nesting_level=1,
            screenshot_reference="screenshots/test.png",
            metadata={"custom": "data"},
        )

        event_data = record.to_tree_event_data()

        assert event_data["id"] == "test-001"
        assert event_data["type"] == "action"
        assert event_data["name"] == "CLICK"
        assert event_data["timestamp"] == start
        assert event_data["end_timestamp"] == end
        assert event_data["duration"] == 0.5
        assert event_data["parent_id"] == "parent-001"
        assert event_data["nesting_level"] == 1
        assert event_data["workflow_name"] == "workflow-001"
        assert event_data["status"] == "success"
        assert event_data["error"] is None
        assert event_data["metadata"]["custom"] == "data"
        assert event_data["metadata"]["config"]["x"] == 100
        assert event_data["metadata"]["screenshot_reference"] == "screenshots/test.png"

    def test_to_tree_event_data_failed(self):
        """Test conversion for failed action."""
        start = time.time()
        end = start + 1.0

        record = ActionExecutionRecord(
            action_id="test-002",
            action_type="FIND",
            start_time=start,
            end_time=end,
            duration_ms=1000.0,
            success=False,
            error_message="Image not found",
            retry_count=3,
        )

        event_data = record.to_tree_event_data()

        assert event_data["status"] == "failed"
        assert event_data["error"] == "Image not found"
        assert event_data["metadata"]["retry_count"] == 3

    def test_to_tree_event_data_running(self):
        """Test conversion for running (incomplete) action."""
        start = time.time()

        record = ActionExecutionRecord(
            action_id="test-003",
            action_type="WAIT",
            start_time=start,
            end_time=None,
            duration_ms=None,
            success=True,
        )

        event_data = record.to_tree_event_data()

        assert event_data["status"] == "running"
        assert event_data["end_timestamp"] is None
        assert event_data["duration"] is None

    def test_to_tree_event_data_state_context(self):
        """Test state_context in tree event data."""
        record = ActionExecutionRecord(
            action_id="test-004",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login", "Form"]),
            active_states_after=frozenset(["Dashboard", "Settings"]),
        )

        event_data = record.to_tree_event_data()
        state_context = event_data["state_context"]

        assert sorted(state_context["active_before"]) == ["Form", "Login"]
        assert sorted(state_context["active_after"]) == ["Dashboard", "Settings"]
        assert state_context["state_changed"] is True
        assert sorted(state_context["states_activated"]) == ["Dashboard", "Settings"]
        assert sorted(state_context["states_deactivated"]) == ["Form", "Login"]


class TestFormatRuntimeForDisplay:
    """Test _format_runtime_for_display for different action types."""

    def test_format_runtime_typed_text(self):
        """Test runtime formatting for TYPE action with typed text."""
        record = ActionExecutionRecord(
            action_id="test-001", action_type="TYPE", typed_text="hello@example.com"
        )

        runtime = record._format_runtime_for_display()

        assert runtime["typed_text"] == "hello@example.com"
        assert len(runtime) == 1  # Only typed_text should be present

    def test_format_runtime_match_summary(self):
        """Test runtime formatting for FIND action with match summary."""
        record = ActionExecutionRecord(
            action_id="test-002",
            action_type="FIND",
            match_summary={
                "image_id": "button.png",
                "confidence": 0.95,
                "threshold": 0.8,
                "found": True,
                "location": {"x": 100, "y": 200},
            },
        )

        runtime = record._format_runtime_for_display()

        assert runtime["match_summary"]["image_id"] == "button.png"
        assert runtime["match_summary"]["confidence"] == 0.95
        assert runtime["match_summary"]["found"] is True

    def test_format_runtime_clicked_location(self):
        """Test runtime formatting for CLICK action with location."""
        record = ActionExecutionRecord(
            action_id="test-003",
            action_type="CLICK",
            clicked_location=(150, 250),
            click_button="left",
            click_target_type="button",
        )

        runtime = record._format_runtime_for_display()

        assert runtime["clicked_location"]["x"] == 150
        assert runtime["clicked_location"]["y"] == 250
        assert runtime["click_button"] == "left"
        assert runtime["click_target_type"] == "button"

    def test_format_runtime_transition_data(self):
        """Test runtime formatting for GO_TO_STATE action with transition data."""
        record = ActionExecutionRecord(
            action_id="test-004",
            action_type="GO_TO_STATE",
            transition_data={
                "from_state": "Login",
                "to_state": "Dashboard",
                "transition_type": "navigation",
            },
        )

        runtime = record._format_runtime_for_display()

        assert runtime["transition_data"]["from_state"] == "Login"
        assert runtime["transition_data"]["to_state"] == "Dashboard"

    def test_format_runtime_condition_result(self):
        """Test runtime formatting for IF action with condition result."""
        record = ActionExecutionRecord(
            action_id="test-005", action_type="IF", condition_result=True, branch_taken="then"
        )

        runtime = record._format_runtime_for_display()

        assert runtime["condition_result"] is True
        assert runtime["branch_taken"] == "then"

    def test_format_runtime_empty_when_no_data(self):
        """Test runtime formatting returns empty dict when no runtime data."""
        record = ActionExecutionRecord(action_id="test-006", action_type="WAIT")

        runtime = record._format_runtime_for_display()

        assert runtime == {}

    def test_format_runtime_combined_data(self):
        """Test runtime formatting with multiple types of runtime data."""
        record = ActionExecutionRecord(
            action_id="test-007",
            action_type="COMPLEX",
            typed_text="test",
            clicked_location=(100, 200),
            click_button="left",
            match_summary={"found": True},
        )

        runtime = record._format_runtime_for_display()

        assert "typed_text" in runtime
        assert "clicked_location" in runtime
        assert "click_button" in runtime
        assert "match_summary" in runtime


class TestStateChangeDetection:
    """Test state change detection with various scenarios."""

    def test_state_change_from_empty_to_populated(self):
        """Test state change when going from no states to some states."""
        record = ActionExecutionRecord(
            action_id="test-001",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(),
            active_states_after=frozenset(["Login"]),
        )

        assert record.state_changed is True
        assert record.states_activated == frozenset(["Login"])
        assert record.states_deactivated == frozenset()

    def test_state_change_from_populated_to_empty(self):
        """Test state change when going from some states to no states."""
        record = ActionExecutionRecord(
            action_id="test-002",
            action_type="RESET",
            active_states_before=frozenset(["Login", "Dashboard"]),
            active_states_after=frozenset(),
        )

        assert record.state_changed is True
        assert record.states_activated == frozenset()
        assert record.states_deactivated == frozenset(["Login", "Dashboard"])

    def test_no_state_change_with_complex_states(self):
        """Test no state change with multiple identical states."""
        states = frozenset(["Login", "Dashboard", "Settings", "Profile"])

        record = ActionExecutionRecord(
            action_id="test-003",
            action_type="CLICK",
            active_states_before=states,
            active_states_after=states,
        )

        assert record.state_changed is False
        assert record.states_activated == frozenset()
        assert record.states_deactivated == frozenset()

    def test_partial_state_change(self):
        """Test partial state change (some states remain, some change)."""
        record = ActionExecutionRecord(
            action_id="test-004",
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login", "MainMenu"]),
            active_states_after=frozenset(["MainMenu", "Dashboard"]),
        )

        assert record.state_changed is True
        assert record.states_activated == frozenset(["Dashboard"])
        assert record.states_deactivated == frozenset(["Login"])
        # MainMenu should be in both before and after
        assert "MainMenu" in record.active_states_before
        assert "MainMenu" in record.active_states_after
