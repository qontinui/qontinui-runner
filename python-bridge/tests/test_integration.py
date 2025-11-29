"""Integration tests for component interactions.

This module verifies that UnifiedDataCollector, ScreenshotService, StateMemory,
and TreeViewLayer work together correctly in realistic scenarios.

Test Coverage:
- Full action recording flow with all components
- Sequential action recording with state evolution
- State transition recording and verification
- Screenshot integration and retrieval
- Tree view generation from recorded actions
- Match result recording with screenshots
"""

import json
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

import pytest

from models.action_execution_record import ActionExecutionRecord
from services.screenshot_service import ScreenshotService
from services.tree_view_layer import TreeViewLayer
from services.unified_data_collector import UnifiedDataCollector


# ============================================================================
# Helper Functions
# ============================================================================


def create_fake_png_data(size: int = 1000) -> bytes:
    """Create fake PNG image data for testing.

    Args:
        size: Size in bytes of the fake data (must be >= 8)

    Returns:
        Fake PNG image data as bytes
    """
    # Standard PNG header (8 bytes)
    png_header = b"\x89PNG\r\n\x1a\n"
    # Fill rest with dummy data
    return png_header + b"X" * (size - len(png_header))


def verify_screenshot_exists(storage_dir: Path, screenshot_ref: str) -> bool:
    """Verify that a screenshot and its metadata exist.

    Args:
        storage_dir: Root storage directory
        screenshot_ref: Screenshot reference path

    Returns:
        True if both screenshot and metadata exist
    """
    screenshot_path = storage_dir / screenshot_ref
    metadata_path = screenshot_path.with_suffix(".json")

    return screenshot_path.exists() and metadata_path.exists()


# ============================================================================
# Fixtures
# ============================================================================
# Note: mock_state_memory is provided by conftest.py


@pytest.fixture
def temp_storage():
    """Create temporary storage directory for tests.

    Yields:
        Path to temporary storage directory (automatically cleaned up)
    """
    with tempfile.TemporaryDirectory() as tmpdir:
        yield Path(tmpdir)


@pytest.fixture
def screenshot_service(temp_storage):
    """Create real ScreenshotService instance.

    Args:
        temp_storage: Temporary storage directory fixture

    Returns:
        Enabled ScreenshotService instance
    """
    return ScreenshotService(temp_storage, enabled=True)


@pytest.fixture
def data_collector(mock_state_memory, screenshot_service):
    """Create UnifiedDataCollector with real dependencies.

    Args:
        mock_state_memory: Mock state memory fixture from conftest.py
        screenshot_service: Real screenshot service fixture

    Returns:
        UnifiedDataCollector instance
    """
    return UnifiedDataCollector(mock_state_memory, screenshot_service)


# ============================================================================
# Test 1: Full Action Recording Flow
# ============================================================================


def test_full_action_recording_flow(
    data_collector, mock_state_memory, screenshot_service, temp_storage
):
    """Test complete action recording flow with all components.

    Scenario:
    1. Start action with initial state
    2. Record text typed
    3. Record click event
    4. Create record
    5. Verify all data is captured correctly
    6. Verify screenshot was stored
    7. Verify state was captured correctly
    """
    # Setup: Set initial state
    mock_state_memory.set_active_states(["Login", "FormVisible"])

    # Step 1: Start action
    start_time = time.time()
    data_collector.start_action(
        action_id="action-001", parent_action_id=None, workflow_id="login-workflow", nesting_level=0
    )

    # Step 2: Record text typed
    data_collector.record_text_typed("user@example.com")

    # Step 3: Record click event
    data_collector.record_click(x=250, y=450, button="left", target_type="submit_button")

    # Simulate state change
    mock_state_memory.add_state("Dashboard")
    mock_state_memory.remove_state("FormVisible")

    # Step 4: Create record
    end_time = time.time()
    record = data_collector.create_record(
        action_type="TYPE_AND_CLICK",
        config={"target": "email_field", "button": "submit"},
        start_time=start_time,
        end_time=end_time,
        success=True,
        metadata={"description": "Login form submission"},
    )

    # Verify record identity
    assert record.action_id == "action-001"
    assert record.action_type == "TYPE_AND_CLICK"
    assert record.workflow_id == "login-workflow"
    assert record.nesting_level == 0
    assert record.parent_action_id is None

    # Verify timing
    assert record.start_time == start_time
    assert record.end_time == end_time
    assert record.duration_ms is not None
    assert record.duration_ms > 0

    # Verify state capture
    assert "Login" in record.active_states_before
    assert "FormVisible" in record.active_states_before
    assert "Login" in record.active_states_after
    assert "Dashboard" in record.active_states_after
    assert "FormVisible" not in record.active_states_after

    # Verify state change detection
    assert record.state_changed is True
    assert "Dashboard" in record.states_activated
    assert "FormVisible" in record.states_deactivated

    # Verify runtime data
    assert record.typed_text == "user@example.com"
    assert record.clicked_location == (250, 450)
    assert record.click_button == "left"
    assert record.click_target_type == "submit_button"

    # Verify outcome
    assert record.success is True
    assert record.error_message is None
    assert record.retry_count == 0

    # Verify metadata
    assert record.metadata["description"] == "Login form submission"

    # Verify config
    assert record.config["target"] == "email_field"
    assert record.config["button"] == "submit"


# ============================================================================
# Test 2: Multiple Actions Sequential
# ============================================================================


def test_multiple_actions_sequential(data_collector, mock_state_memory):
    """Test recording multiple actions sequentially.

    Scenario:
    1. Record action 1 (root level)
    2. Record action 2 (child of action 1)
    3. Record action 3 (child of action 2)
    4. Verify parent-child relationships
    5. Verify state evolution across actions
    6. Verify buffer is cleared between actions
    """
    # Initial state
    mock_state_memory.set_active_states(["Initial"])

    # Action 1: Root level action
    start_time_1 = time.time()
    data_collector.start_action(
        action_id="action-001", parent_action_id=None, workflow_id="test-workflow", nesting_level=0
    )
    data_collector.record_text_typed("action 1 text")
    mock_state_memory.add_state("StateA")
    end_time_1 = time.time()

    record_1 = data_collector.create_record(
        action_type="ACTION_1", start_time=start_time_1, end_time=end_time_1, success=True
    )

    # Verify action 1
    assert record_1.action_id == "action-001"
    assert record_1.parent_action_id is None
    assert record_1.nesting_level == 0
    assert record_1.typed_text == "action 1 text"
    assert "Initial" in record_1.active_states_before
    assert "StateA" in record_1.active_states_after
    assert "StateA" in record_1.states_activated

    # Action 2: Child of action 1
    start_time_2 = time.time()
    data_collector.start_action(
        action_id="action-002",
        parent_action_id="action-001",
        workflow_id="test-workflow",
        nesting_level=1,
    )
    data_collector.record_text_typed("action 2 text")
    mock_state_memory.add_state("StateB")
    end_time_2 = time.time()

    record_2 = data_collector.create_record(
        action_type="ACTION_2", start_time=start_time_2, end_time=end_time_2, success=True
    )

    # Verify action 2
    assert record_2.action_id == "action-002"
    assert record_2.parent_action_id == "action-001"
    assert record_2.nesting_level == 1
    assert record_2.typed_text == "action 2 text"
    assert "StateA" in record_2.active_states_before
    assert "StateB" in record_2.active_states_after
    assert "StateB" in record_2.states_activated

    # Verify buffer was cleared (no text from action 1)
    assert record_2.typed_text != "action 1 text"

    # Action 3: Child of action 2
    start_time_3 = time.time()
    data_collector.start_action(
        action_id="action-003",
        parent_action_id="action-002",
        workflow_id="test-workflow",
        nesting_level=2,
    )
    # No text typed in action 3
    mock_state_memory.remove_state("StateA")
    end_time_3 = time.time()

    record_3 = data_collector.create_record(
        action_type="ACTION_3", start_time=start_time_3, end_time=end_time_3, success=True
    )

    # Verify action 3
    assert record_3.action_id == "action-003"
    assert record_3.parent_action_id == "action-002"
    assert record_3.nesting_level == 2
    assert record_3.typed_text is None  # Buffer was cleared, no text
    assert "StateA" in record_3.active_states_before
    assert "StateA" not in record_3.active_states_after
    assert "StateA" in record_3.states_deactivated

    # Verify state evolution
    assert len(record_1.active_states_before) == 1  # Just "Initial"
    assert len(record_1.active_states_after) == 2  # "Initial" + "StateA"
    assert len(record_2.active_states_after) == 3  # "Initial" + "StateA" + "StateB"
    assert len(record_3.active_states_after) == 2  # "Initial" + "StateB" (StateA removed)


# ============================================================================
# Test 3: State Transition Recording
# ============================================================================


def test_state_transition_recording(data_collector, mock_state_memory):
    """Test recording GO_TO_STATE action with transition data.

    Scenario:
    1. Start with initial states
    2. Record GO_TO_STATE action
    3. Record transition data
    4. Create record
    5. Verify transition_data in record
    6. Verify states_activated shows new states
    """
    # Setup: Initial states
    mock_state_memory.set_active_states(["Login", "FormVisible"])

    # Start action
    start_time = time.time()
    data_collector.start_action(
        action_id="transition-001",
        parent_action_id=None,
        workflow_id="navigation-workflow",
        nesting_level=0,
    )

    # Record transition
    transition_info = {
        "from_state": "Login",
        "to_state": "Dashboard",
        "transition_type": "navigation",
        "success": True,
        "method": "click_logo",
    }
    data_collector.record_transition(transition_info)

    # Simulate state transition
    mock_state_memory.remove_state("Login")
    mock_state_memory.remove_state("FormVisible")
    mock_state_memory.add_state("Dashboard")
    mock_state_memory.add_state("MenuBar")

    # Create record
    end_time = time.time()
    record = data_collector.create_record(
        action_type="GO_TO_STATE",
        config={"target_state": "Dashboard"},
        start_time=start_time,
        end_time=end_time,
        success=True,
    )

    # Verify transition data
    assert record.transition_data is not None
    assert record.transition_data["from_state"] == "Login"
    assert record.transition_data["to_state"] == "Dashboard"
    assert record.transition_data["transition_type"] == "navigation"
    assert record.transition_data["success"] is True
    assert record.transition_data["method"] == "click_logo"

    # Verify states
    assert "Login" in record.active_states_before
    assert "FormVisible" in record.active_states_before
    assert "Dashboard" in record.active_states_after
    assert "MenuBar" in record.active_states_after
    assert "Login" not in record.active_states_after
    assert "FormVisible" not in record.active_states_after

    # Verify state changes
    assert record.state_changed is True
    assert "Dashboard" in record.states_activated
    assert "MenuBar" in record.states_activated
    assert "Login" in record.states_deactivated
    assert "FormVisible" in record.states_deactivated


# ============================================================================
# Test 4: Screenshot Integration
# ============================================================================


def test_screenshot_integration(temp_storage, mock_state_memory):
    """Test screenshot integration with real ScreenshotService.

    Scenario:
    1. Create collector with real ScreenshotService
    2. Record action with screenshot
    3. Verify screenshot_reference is set
    4. Verify screenshot can be retrieved via ScreenshotService
    5. Verify metadata sidecar exists
    """
    # Setup: Create real screenshot service
    screenshot_service = ScreenshotService(temp_storage, enabled=True)
    collector = UnifiedDataCollector(mock_state_memory, screenshot_service)

    # Set initial state
    mock_state_memory.set_active_states(["Login"])

    # Start action
    start_time = time.time()
    collector.start_action(
        action_id="screenshot-action-001",
        parent_action_id=None,
        workflow_id="test-workflow",
        nesting_level=0,
    )

    # Record match with screenshot
    fake_screenshot = create_fake_png_data(2000)
    match_summary = {
        "image_id": "login_button.png",
        "confidence": 0.95,
        "threshold": 0.8,
        "found": True,
        "location": {"x": 100, "y": 200},
    }

    collector.record_match_result(match_summary=match_summary, screenshot_data=fake_screenshot)

    # Create record
    end_time = time.time()
    record = collector.create_record(
        action_type="FIND",
        config={"image": "login_button.png"},
        start_time=start_time,
        end_time=end_time,
        success=True,
    )

    # Verify screenshot reference is set
    assert record.screenshot_reference is not None
    assert "screenshots/screenshot-" in record.screenshot_reference
    assert record.screenshot_reference.endswith(".png")

    # Verify screenshot can be retrieved
    retrieved_screenshot = screenshot_service.get_screenshot(record.screenshot_reference)
    assert retrieved_screenshot is not None
    assert retrieved_screenshot == fake_screenshot

    # Verify metadata sidecar exists
    assert verify_screenshot_exists(temp_storage, record.screenshot_reference)

    # Read and verify metadata
    metadata_path = temp_storage / record.screenshot_reference.replace(".png", ".json")
    metadata = json.loads(metadata_path.read_text())
    assert metadata["action_id"] == "screenshot-action-001"
    assert metadata["active_states"] == ["Login"]
    assert metadata["event_type"] == "match_attempt"
    assert metadata["image_id"] == "login_button.png"
    assert metadata["found"] is True


# ============================================================================
# Test 5: Tree View from Recorded Actions
# ============================================================================


def test_tree_view_from_recorded_actions(data_collector, mock_state_memory):
    """Test creating tree views from recorded actions.

    Scenario:
    1. Record multiple actions with different hierarchies
    2. Create execution_tree_view from records
    3. Verify tree structure is correct
    4. Test other views as well
    """
    # Setup initial state
    mock_state_memory.set_active_states([])

    records = []

    # Record action 1 (root)
    start_time = time.time()
    data_collector.start_action("action-001", None, "workflow-1", 0)
    mock_state_memory.add_state("StateA")
    record_1 = data_collector.create_record(
        action_type="ACTION_1", start_time=start_time, end_time=start_time + 0.1, success=True
    )
    records.append(record_1)

    # Record action 2 (child of action 1)
    start_time = time.time()
    data_collector.start_action("action-002", "action-001", "workflow-1", 1)
    mock_state_memory.add_state("StateB")
    record_2 = data_collector.create_record(
        action_type="ACTION_2", start_time=start_time, end_time=start_time + 0.1, success=True
    )
    records.append(record_2)

    # Record action 3 (child of action 2)
    start_time = time.time()
    data_collector.start_action("action-003", "action-002", "workflow-1", 2)
    record_3 = data_collector.create_record(
        action_type="ACTION_3", start_time=start_time, end_time=start_time + 0.1, success=True
    )
    records.append(record_3)

    # Record action 4 (root, different workflow)
    start_time = time.time()
    data_collector.start_action("action-004", None, "workflow-2", 0)
    mock_state_memory.remove_state("StateA")
    record_4 = data_collector.create_record(
        action_type="ACTION_4", start_time=start_time, end_time=start_time + 0.1, success=True
    )
    records.append(record_4)

    # Test execution tree view
    execution_tree = TreeViewLayer.execution_tree_view(records)
    assert len(execution_tree) > 0

    # Verify hierarchy
    workflow_1_node = next(n for n in execution_tree if n.id == "workflow-1")
    assert workflow_1_node is not None
    assert len(workflow_1_node.children) >= 1

    # Find action-001 in workflow-1
    action_1_node = next(n for n in workflow_1_node.children if n.id == "action-001")
    assert action_1_node is not None
    assert len(action_1_node.children) >= 1

    # Find action-002 as child of action-001
    action_2_node = next(n for n in action_1_node.children if n.id == "action-002")
    assert action_2_node is not None
    assert len(action_2_node.children) >= 1

    # Find action-003 as child of action-002
    action_3_node = next(n for n in action_2_node.children if n.id == "action-003")
    assert action_3_node is not None

    # Test action-only tree view
    action_tree = TreeViewLayer.action_only_tree_view(records)
    assert len(action_tree) == 4
    assert all(node.type == "action" for node in action_tree)

    # Test state transition tree view
    state_transition_tree = TreeViewLayer.state_transition_tree_view(records)
    assert len(state_transition_tree) > 0

    # Should have "States Entered" and "States Exited" groups
    group_labels = [node.label for node in state_transition_tree]
    assert any("Entered" in label for label in group_labels)
    assert any("Exited" in label for label in group_labels)

    # Test timeline tree view
    timeline_tree = TreeViewLayer.timeline_tree_view(records)
    assert len(timeline_tree) > 0
    assert all("[" in node.label for node in timeline_tree)  # Has timestamp

    # Test state grouped tree view
    state_grouped_tree = TreeViewLayer.state_grouped_tree_view(records)
    assert len(state_grouped_tree) > 0
    assert all(node.type == "state_group" for node in state_grouped_tree)

    # Test serialization
    for tree in [
        execution_tree,
        action_tree,
        state_transition_tree,
        timeline_tree,
        state_grouped_tree,
    ]:
        tree_dict = [node.to_dict() for node in tree]
        assert isinstance(tree_dict, list)
        if tree_dict:
            assert "id" in tree_dict[0]
            assert "label" in tree_dict[0]
            assert "children" in tree_dict[0]


# ============================================================================
# Test 6: Match Result with Screenshot
# ============================================================================


def test_match_result_with_screenshot(temp_storage, mock_state_memory):
    """Test recording match result with screenshot and debug visual.

    Scenario:
    1. Create match_summary dict
    2. Include screenshot bytes (fake PNG data)
    3. Include debug visual bytes
    4. Record via collector
    5. Verify match_summary in record
    6. Verify screenshot stored and referenced
    7. Verify debug visual stored and referenced
    """
    # Setup
    screenshot_service = ScreenshotService(temp_storage, enabled=True)
    collector = UnifiedDataCollector(mock_state_memory, screenshot_service)
    mock_state_memory.set_active_states(["SearchPage"])

    # Start action
    start_time = time.time()
    collector.start_action(
        action_id="match-action-001",
        parent_action_id=None,
        workflow_id="search-workflow",
        nesting_level=0,
    )

    # Create fake screenshot and debug visual
    fake_screenshot = create_fake_png_data(3000)
    fake_debug_visual = create_fake_png_data(3500)

    # Record match result with both screenshot and debug visual
    match_summary = {
        "image_id": "search_icon.png",
        "confidence": 0.89,
        "threshold": 0.75,
        "found": True,
        "location": {"x": 450, "y": 120},
    }

    collector.record_match_result(
        match_summary=match_summary,
        screenshot_data=fake_screenshot,
        debug_visual_data=fake_debug_visual,
    )

    # Create record
    end_time = time.time()
    record = collector.create_record(
        action_type="FIND",
        config={"image": "search_icon.png", "threshold": 0.75},
        start_time=start_time,
        end_time=end_time,
        success=True,
    )

    # Verify match_summary in record
    assert record.match_summary is not None
    assert record.match_summary["image_id"] == "search_icon.png"
    assert record.match_summary["confidence"] == 0.89
    assert record.match_summary["threshold"] == 0.75
    assert record.match_summary["found"] is True
    assert record.match_summary["location"]["x"] == 450
    assert record.match_summary["location"]["y"] == 120

    # Verify screenshot stored and referenced
    assert record.screenshot_reference is not None
    assert "screenshots/screenshot-" in record.screenshot_reference

    retrieved_screenshot = screenshot_service.get_screenshot(record.screenshot_reference)
    assert retrieved_screenshot == fake_screenshot

    # Verify screenshot metadata
    screenshot_metadata_path = temp_storage / record.screenshot_reference.replace(".png", ".json")
    screenshot_metadata = json.loads(screenshot_metadata_path.read_text())
    assert screenshot_metadata["action_id"] == "match-action-001"
    assert screenshot_metadata["active_states"] == ["SearchPage"]
    assert screenshot_metadata["event_type"] == "match_attempt"
    assert screenshot_metadata["image_id"] == "search_icon.png"
    assert screenshot_metadata["found"] is True

    # Verify debug visual stored and referenced
    assert record.visual_debug_reference is not None
    assert "debug_visuals/debug-" in record.visual_debug_reference

    # Verify debug visual can be retrieved
    debug_path = temp_storage / record.visual_debug_reference
    assert debug_path.exists()
    retrieved_debug = debug_path.read_bytes()
    assert retrieved_debug == fake_debug_visual

    # Verify debug visual metadata
    debug_metadata_path = debug_path.with_suffix(".json")
    assert debug_metadata_path.exists()
    debug_metadata = json.loads(debug_metadata_path.read_text())
    assert debug_metadata["action_id"] == "match-action-001"
    assert debug_metadata["match_info"]["confidence"] == 0.89
    assert debug_metadata["match_info"]["found"] is True


# ============================================================================
# Test 7: Error Handling and Edge Cases
# ============================================================================


def test_failed_action_recording(data_collector, mock_state_memory):
    """Test recording a failed action with error message."""
    mock_state_memory.set_active_states(["Initial"])

    start_time = time.time()
    data_collector.start_action("error-action-001", None, "test-workflow", 0)

    # Create record for failed action
    end_time = time.time()
    record = data_collector.create_record(
        action_type="CLICK",
        config={"x": 100, "y": 200},
        start_time=start_time,
        end_time=end_time,
        success=False,
        error_message="Element not found at coordinates",
        retry_count=3,
    )

    # Verify failure data
    assert record.success is False
    assert record.error_message == "Element not found at coordinates"
    assert record.retry_count == 3


def test_action_without_state_change(data_collector, mock_state_memory):
    """Test recording action that doesn't change state."""
    mock_state_memory.set_active_states(["StateA", "StateB"])

    start_time = time.time()
    data_collector.start_action("no-change-001", None, "test-workflow", 0)

    # Don't change state
    end_time = time.time()
    record = data_collector.create_record(
        action_type="WAIT",
        config={"duration_ms": 1000},
        start_time=start_time,
        end_time=end_time,
        success=True,
    )

    # Verify no state change
    assert record.state_changed is False
    assert len(record.states_activated) == 0
    assert len(record.states_deactivated) == 0
    assert record.active_states_before == record.active_states_after


def test_collector_without_screenshot_service(mock_state_memory):
    """Test collector works correctly without screenshot service."""
    # Create collector without screenshot service
    collector = UnifiedDataCollector(mock_state_memory, screenshot_service=None)

    mock_state_memory.set_active_states([])

    start_time = time.time()
    collector.start_action("no-screenshot-001", None, "test-workflow", 0)

    # Try to record match with screenshot (should not error)
    collector.record_match_result(
        match_summary={"image_id": "test.png", "found": True},
        screenshot_data=create_fake_png_data(),
    )

    end_time = time.time()
    record = collector.create_record(
        action_type="FIND", start_time=start_time, end_time=end_time, success=True
    )

    # Verify match_summary exists but no screenshot reference
    assert record.match_summary is not None
    assert record.screenshot_reference is None


def test_empty_tree_views():
    """Test tree view generation with empty record list."""
    empty_records = []

    # All views should handle empty records gracefully
    execution_tree = TreeViewLayer.execution_tree_view(empty_records)
    assert execution_tree == []

    action_tree = TreeViewLayer.action_only_tree_view(empty_records)
    assert action_tree == []

    state_tree = TreeViewLayer.state_transition_tree_view(empty_records)
    assert state_tree == []

    timeline_tree = TreeViewLayer.timeline_tree_view(empty_records)
    assert timeline_tree == []

    grouped_tree = TreeViewLayer.state_grouped_tree_view(empty_records)
    assert grouped_tree == []


# ============================================================================
# Test 8: Complex Workflow Integration
# ============================================================================


def test_complex_workflow_integration(temp_storage, mock_state_memory):
    """Test a complex workflow with multiple actions, states, and screenshots.

    This test simulates a realistic workflow execution:
    - Login with state transitions
    - Search with image recognition
    - Navigate with screenshots
    - Error handling with retry
    """
    # Setup
    state_memory = mock_state_memory
    screenshot_service = ScreenshotService(temp_storage, enabled=True)
    collector = UnifiedDataCollector(state_memory, screenshot_service)

    records = []

    # Step 1: Navigate to login page
    state_memory.set_active_states(["HomePage"])
    start_time = time.time()
    collector.start_action("nav-login-001", None, "login-flow", 0)
    collector.record_click(100, 50, "left", "login_link")
    state_memory.add_state("LoginPage")
    state_memory.remove_state("HomePage")
    record = collector.create_record(
        action_type="CLICK",
        config={"target": "login_link"},
        start_time=start_time,
        end_time=time.time(),
        success=True,
    )
    records.append(record)

    # Step 2: Type username (with screenshot)
    start_time = time.time()
    collector.start_action("type-user-002", "nav-login-001", "login-flow", 1)
    collector.record_text_typed("testuser@example.com")
    collector.record_match_result(
        match_summary={"image_id": "username_field.png", "found": True, "confidence": 0.95},
        screenshot_data=create_fake_png_data(),
    )
    record = collector.create_record(
        action_type="TYPE",
        config={"text": "testuser@example.com"},
        start_time=start_time,
        end_time=time.time(),
        success=True,
    )
    records.append(record)

    # Step 3: Type password (with screenshot)
    start_time = time.time()
    collector.start_action("type-pass-003", "nav-login-001", "login-flow", 1)
    collector.record_text_typed("********")
    collector.record_match_result(
        match_summary={"image_id": "password_field.png", "found": True, "confidence": 0.92},
        screenshot_data=create_fake_png_data(),
    )
    record = collector.create_record(
        action_type="TYPE",
        config={"text": "********"},
        start_time=start_time,
        end_time=time.time(),
        success=True,
    )
    records.append(record)

    # Step 4: Click submit (with state transition)
    start_time = time.time()
    collector.start_action("submit-004", "nav-login-001", "login-flow", 1)
    collector.record_click(250, 400, "left", "submit_button")
    collector.record_transition(
        {
            "from_state": "LoginPage",
            "to_state": "Dashboard",
            "transition_type": "login_success",
            "success": True,
        }
    )
    state_memory.remove_state("LoginPage")
    state_memory.add_state("Dashboard")
    record = collector.create_record(
        action_type="CLICK",
        config={"target": "submit_button"},
        start_time=start_time,
        end_time=time.time(),
        success=True,
    )
    records.append(record)

    # Step 5: Search for item (with retry)
    start_time = time.time()
    collector.start_action("search-005", None, "search-flow", 0)
    collector.record_text_typed("test query")
    record = collector.create_record(
        action_type="TYPE",
        config={"text": "test query"},
        start_time=start_time,
        end_time=time.time(),
        success=False,
        error_message="Search field not found",
        retry_count=1,
    )
    records.append(record)

    # Step 6: Search retry (successful)
    start_time = time.time()
    collector.start_action("search-006", None, "search-flow", 0)
    collector.record_text_typed("test query")
    collector.record_match_result(
        match_summary={"image_id": "search_field.png", "found": True, "confidence": 0.88},
        screenshot_data=create_fake_png_data(),
    )
    state_memory.add_state("SearchResults")
    record = collector.create_record(
        action_type="TYPE",
        config={"text": "test query"},
        start_time=start_time,
        end_time=time.time(),
        success=True,
    )
    records.append(record)

    # Verify all records
    assert len(records) == 6

    # Verify parent-child relationships
    assert records[0].parent_action_id is None
    assert records[1].parent_action_id == "nav-login-001"
    assert records[2].parent_action_id == "nav-login-001"
    assert records[3].parent_action_id == "nav-login-001"
    assert records[4].parent_action_id is None
    assert records[5].parent_action_id is None

    # Verify state evolution
    assert "HomePage" in records[0].active_states_before
    assert "LoginPage" in records[0].active_states_after
    assert "Dashboard" in records[3].active_states_after
    assert "SearchResults" in records[5].active_states_after

    # Verify screenshots were stored
    screenshot_records = [r for r in records if r.screenshot_reference is not None]
    assert len(screenshot_records) == 3  # Steps 2, 3, and 6

    # Verify all screenshots can be retrieved
    for record in screenshot_records:
        screenshot = screenshot_service.get_screenshot(record.screenshot_reference)
        assert screenshot is not None

    # Generate tree views
    execution_tree = TreeViewLayer.execution_tree_view(records)
    assert len(execution_tree) > 0

    state_transition_tree = TreeViewLayer.state_transition_tree_view(records)
    assert len(state_transition_tree) > 0

    # Verify tree can be serialized
    tree_dict = [node.to_dict() for node in execution_tree]
    assert len(tree_dict) > 0

    # Verify state changes are reflected in tree
    # We need to recursively check all nodes, not just top-level ones
    def has_state_change_in_tree(nodes):
        """Recursively check if any node has state changes."""
        for node in nodes:
            if node.metadata.get("has_state_change", False):
                return True
            if node.children and has_state_change_in_tree(node.children):
                return True
        return False

    assert has_state_change_in_tree(execution_tree)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
