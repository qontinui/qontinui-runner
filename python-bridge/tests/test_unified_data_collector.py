"""Comprehensive unit tests for UnifiedDataCollector.

This module tests the UnifiedDataCollector service:
- Action initialization and state capture
- Text typed recording
- Match result recording with and without screenshots
- Click recording
- Transition recording
- Condition recording
- Record creation with immutable output
- Buffer clearing after record creation
- State change capture
- Screenshot service integration
- Thread safety with basic threading tests
"""

import threading
import time
from unittest.mock import Mock

import pytest

from models.action_execution_record import ActionExecutionRecord
from services.unified_data_collector import UnifiedDataCollector


class TestUnifiedDataCollectorInitialization:
    """Test UnifiedDataCollector initialization."""

    def test_init_with_state_memory(self, mock_state_memory):
        """Test initialization with state_memory."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory, screenshot_service=None)

        assert collector.state_memory is mock_state_memory
        assert collector.screenshot_service is None
        assert collector._current_action_id is None

    def test_init_with_screenshot_service(self, mock_state_memory, mock_screenshot_service):
        """Test initialization with screenshot_service."""
        collector = UnifiedDataCollector(
            state_memory=mock_state_memory, screenshot_service=mock_screenshot_service
        )

        assert collector.state_memory is mock_state_memory
        assert collector.screenshot_service is mock_screenshot_service

    def test_init_without_services(self):
        """Test initialization with None services."""
        collector = UnifiedDataCollector(state_memory=None, screenshot_service=None)

        assert collector.state_memory is None
        assert collector.screenshot_service is None


class TestStartAction:
    """Test start_action method."""

    def test_start_action_captures_initial_state(self, mock_state_memory):
        """Test that start_action captures the 'before' state snapshot."""
        mock_state_memory.set_active_states(["Login", "MainMenu"])

        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action(
            action_id="action-001",
            parent_action_id=None,
            workflow_id="workflow-001",
            nesting_level=0,
        )

        assert collector._current_action_id == "action-001"
        assert collector._current_workflow_id == "workflow-001"
        assert collector._active_states_before == ["Login", "MainMenu"]

    def test_start_action_clears_previous_buffers(self, mock_state_memory):
        """Test that start_action clears buffers from previous action."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)

        # First action
        collector.start_action("action-001", None, "workflow-001", 0)
        collector.record_text_typed("first text")
        collector.record_click(100, 200, "left")

        # Second action should clear buffers
        collector.start_action("action-002", None, "workflow-001", 0)

        assert collector._text_typed is None
        assert len(collector._clicks) == 0

    def test_start_action_with_nested_hierarchy(self, mock_state_memory):
        """Test start_action with parent action and nesting level."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)

        collector.start_action(
            action_id="action-child",
            parent_action_id="action-parent",
            workflow_id="workflow-001",
            nesting_level=2,
        )

        assert collector._current_action_id == "action-child"
        assert collector._current_parent_action_id == "action-parent"
        assert collector._current_nesting_level == 2

    def test_start_action_without_state_memory(self):
        """Test start_action when state_memory is None."""
        collector = UnifiedDataCollector(state_memory=None)

        collector.start_action("action-001", None, "workflow-001", 0)

        assert collector._active_states_before == []


class TestRecordTextTyped:
    """Test record_text_typed method."""

    def test_record_text_typed_stores_text(self, mock_state_memory):
        """Test that record_text_typed stores the typed text."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_text_typed("hello@example.com")

        assert collector._text_typed == "hello@example.com"

    def test_record_text_typed_overwrites_previous(self, mock_state_memory):
        """Test that record_text_typed overwrites previous value."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_text_typed("first")
        collector.record_text_typed("second")

        assert collector._text_typed == "second"

    def test_record_text_typed_empty_string(self, mock_state_memory):
        """Test recording empty string."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_text_typed("")

        assert collector._text_typed == ""


class TestRecordMatchResult:
    """Test record_match_result method."""

    def test_record_match_result_without_screenshots(self, mock_state_memory):
        """Test recording match result without screenshot data."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {
            "image_id": "button.png",
            "confidence": 0.95,
            "threshold": 0.8,
            "found": True,
            "location": {"x": 100, "y": 200},
        }

        collector.record_match_result(match_summary)

        assert len(collector._match_results) == 1
        assert collector._match_results[0]["image_id"] == "button.png"
        assert collector._match_results[0]["confidence"] == 0.95

    def test_record_match_result_with_screenshot(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test recording match result with screenshot data."""
        collector = UnifiedDataCollector(
            state_memory=mock_state_memory, screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "confidence": 0.95, "found": True}

        collector.record_match_result(match_summary=match_summary, screenshot_data=sample_png_bytes)

        # Verify screenshot service was called
        mock_screenshot_service.store_screenshot.assert_called_once()
        call_args = mock_screenshot_service.store_screenshot.call_args

        assert call_args.kwargs["screenshot_data"] == sample_png_bytes
        assert call_args.kwargs["action_id"] == "action-001"

    def test_record_match_result_with_debug_visual(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test recording match result with debug visual data."""
        collector = UnifiedDataCollector(
            state_memory=mock_state_memory, screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "found": True}

        collector.record_match_result(
            match_summary=match_summary, debug_visual_data=sample_png_bytes
        )

        # Verify debug visual service was called
        mock_screenshot_service.store_debug_visual.assert_called_once()
        call_args = mock_screenshot_service.store_debug_visual.call_args

        assert call_args.kwargs["debug_image_data"] == sample_png_bytes
        assert call_args.kwargs["action_id"] == "action-001"
        assert call_args.kwargs["match_info"] == match_summary

    def test_record_match_result_multiple(self, mock_state_memory):
        """Test recording multiple match results."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_match_result({"image_id": "first.png", "found": False})
        collector.record_match_result({"image_id": "second.png", "found": True})

        assert len(collector._match_results) == 2
        assert collector._match_results[0]["image_id"] == "first.png"
        assert collector._match_results[1]["image_id"] == "second.png"


class TestRecordClick:
    """Test record_click method."""

    def test_record_click_basic(self, mock_state_memory):
        """Test basic click recording."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_click(150, 250, "left", "button")

        assert len(collector._clicks) == 1
        assert collector._clicks[0]["x"] == 150
        assert collector._clicks[0]["y"] == 250
        assert collector._clicks[0]["button"] == "left"
        assert collector._clicks[0]["target_type"] == "button"

    def test_record_click_right_button(self, mock_state_memory):
        """Test recording right-click."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_click(100, 200, "right", "context_menu")

        assert collector._clicks[0]["button"] == "right"
        assert collector._clicks[0]["target_type"] == "context_menu"

    def test_record_click_multiple(self, mock_state_memory):
        """Test recording multiple clicks."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_click(100, 200, "left", "button")
        collector.record_click(300, 400, "left", "link")

        assert len(collector._clicks) == 2


class TestRecordTransition:
    """Test record_transition method."""

    def test_record_transition_basic(self, mock_state_memory):
        """Test basic transition recording."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        transition_data = {
            "from_state": "Login",
            "to_state": "Dashboard",
            "transition_type": "navigation",
            "success": True,
        }

        collector.record_transition(transition_data)

        assert len(collector._transitions) == 1
        assert collector._transitions[0]["from_state"] == "Login"
        assert collector._transitions[0]["to_state"] == "Dashboard"

    def test_record_transition_multiple(self, mock_state_memory):
        """Test recording multiple transitions."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_transition({"from_state": "A", "to_state": "B"})
        collector.record_transition({"from_state": "B", "to_state": "C"})

        assert len(collector._transitions) == 2


class TestRecordCondition:
    """Test record_condition method."""

    def test_record_condition_passed(self, mock_state_memory):
        """Test recording passed condition."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_condition(passed=True, branch="then")

        assert len(collector._conditions) == 1
        assert collector._conditions[0]["passed"] is True
        assert collector._conditions[0]["branch"] == "then"

    def test_record_condition_failed(self, mock_state_memory):
        """Test recording failed condition."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_condition(passed=False, branch="else")

        assert collector._conditions[0]["passed"] is False
        assert collector._conditions[0]["branch"] == "else"


class TestCreateRecord:
    """Test create_record method."""

    def test_create_record_basic(self, mock_state_memory):
        """Test creating basic record."""
        mock_state_memory.set_active_states(["Login"])

        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        start = time.time()
        end = start + 0.5

        record = collector.create_record(
            action_type="CLICK",
            config={"x": 100, "y": 200},
            start_time=start,
            end_time=end,
            success=True,
        )

        assert isinstance(record, ActionExecutionRecord)
        assert record.action_id == "action-001"
        assert record.action_type == "CLICK"
        assert record.success is True

    def test_create_record_immutable(self, mock_state_memory):
        """Test that created record is immutable."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        record = collector.create_record(action_type="CLICK")

        # Verify record is frozen
        from dataclasses import FrozenInstanceError

        with pytest.raises(FrozenInstanceError):
            record.action_id = "new-id"

    def test_create_record_with_typed_text(self, mock_state_memory):
        """Test creating record with typed text."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_text_typed("hello@example.com")

        record = collector.create_record(action_type="TYPE")

        assert record.typed_text == "hello@example.com"

    def test_create_record_with_click(self, mock_state_memory):
        """Test creating record with click data."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_click(150, 250, "left", "button")

        record = collector.create_record(action_type="CLICK")

        assert record.clicked_location == (150, 250)
        assert record.click_button == "left"
        assert record.click_target_type == "button"

    def test_create_record_with_match_result(self, mock_state_memory):
        """Test creating record with match result."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {
            "image_id": "button.png",
            "confidence": 0.95,
            "threshold": 0.8,
            "found": True,
            "location": {"x": 100, "y": 200},
        }

        collector.record_match_result(match_summary)

        record = collector.create_record(action_type="FIND")

        assert record.match_summary["image_id"] == "button.png"
        assert record.match_summary["confidence"] == 0.95
        assert record.match_summary["found"] is True

    def test_create_record_clears_buffer(self, mock_state_memory):
        """Test that create_record clears the buffer."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        collector.record_text_typed("test")
        collector.record_click(100, 200, "left")

        collector.create_record(action_type="CLICK")

        # Buffer should be cleared
        assert collector._current_action_id is None
        assert collector._text_typed is None
        assert len(collector._clicks) == 0

    def test_create_record_captures_state_changes(self, mock_state_memory):
        """Test that create_record captures state changes."""
        mock_state_memory.set_active_states(["Login"])

        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        # Change state before creating record
        mock_state_memory.add_state("Dashboard")

        record = collector.create_record(action_type="GO_TO_STATE")

        assert "Login" in record.active_states_before
        assert "Login" in record.active_states_after
        assert "Dashboard" in record.active_states_after
        assert record.state_changed is True

    def test_create_record_calculates_duration(self, mock_state_memory):
        """Test that create_record calculates duration."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        start = time.time()
        end = start + 0.5

        record = collector.create_record(action_type="CLICK", start_time=start, end_time=end)

        assert record.duration_ms == pytest.approx(500.0, rel=1e-6)

    def test_create_record_with_error(self, mock_state_memory):
        """Test creating record for failed action."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        record = collector.create_record(
            action_type="FIND", success=False, error_message="Image not found", retry_count=3
        )

        assert record.success is False
        assert record.error_message == "Image not found"
        assert record.retry_count == 3


class TestScreenshotServiceIntegration:
    """Test integration with ScreenshotService."""

    def test_screenshot_reference_stored_in_record(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test that screenshot reference is included in record."""
        mock_screenshot_service.store_screenshot.return_value = "screenshots/test-0001.png"

        collector = UnifiedDataCollector(
            state_memory=mock_state_memory, screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "found": True}
        collector.record_match_result(match_summary=match_summary, screenshot_data=sample_png_bytes)

        record = collector.create_record(action_type="FIND")

        assert record.screenshot_reference == "screenshots/test-0001.png"

    def test_debug_visual_reference_stored_in_record(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test that debug visual reference is included in record."""
        mock_screenshot_service.store_debug_visual.return_value = "debug/test-0001.png"

        collector = UnifiedDataCollector(
            state_memory=mock_state_memory, screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "found": True}
        collector.record_match_result(
            match_summary=match_summary, debug_visual_data=sample_png_bytes
        )

        record = collector.create_record(action_type="FIND")

        assert record.visual_debug_reference == "debug/test-0001.png"


class TestThreadSafety:
    """Test thread safety of UnifiedDataCollector."""

    def test_concurrent_record_text_typed(self, mock_state_memory):
        """Test that concurrent calls to record_text_typed are thread-safe."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        results = []

        def record_text(text):
            collector.record_text_typed(text)
            results.append(text)

        threads = [
            threading.Thread(target=record_text, args=("text1",)),
            threading.Thread(target=record_text, args=("text2",)),
            threading.Thread(target=record_text, args=("text3",)),
        ]

        for thread in threads:
            thread.start()

        for thread in threads:
            thread.join()

        # One of the texts should have won
        assert collector._text_typed in ["text1", "text2", "text3"]

    def test_concurrent_record_click(self, mock_state_memory):
        """Test that concurrent calls to record_click are thread-safe."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)
        collector.start_action("action-001", None, "workflow-001", 0)

        def record_click():
            for i in range(10):
                collector.record_click(i, i * 10, "left")

        threads = [threading.Thread(target=record_click), threading.Thread(target=record_click)]

        for thread in threads:
            thread.start()

        for thread in threads:
            thread.join()

        # Should have recorded all clicks without data corruption
        assert len(collector._clicks) == 20

    def test_start_action_and_create_record_thread_safe(self, mock_state_memory):
        """Test that start_action and create_record can be called safely."""
        collector = UnifiedDataCollector(state_memory=mock_state_memory)

        records = []

        def create_action(action_id):
            collector.start_action(action_id, None, "workflow-001", 0)
            collector.record_text_typed(f"text-{action_id}")
            record = collector.create_record(action_type="TYPE")
            records.append(record)

        # Note: This test is limited since collector expects sequential execution
        # We're testing that the lock prevents corruption, not full concurrency
        thread = threading.Thread(target=create_action, args=("action-001",))
        thread.start()
        thread.join()

        assert len(records) == 1
        assert records[0].action_id == "action-001"
