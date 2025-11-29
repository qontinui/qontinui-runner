"""Pytest configuration and shared fixtures for unified data architecture tests.

This module provides common fixtures and utilities used across all test modules:
- Mock StateMemory for state tracking simulation
- Sample action execution records
- Temporary directories for file operations
- Common test data builders
"""

import time
from pathlib import Path
from typing import Any, Dict, List, Optional
from unittest.mock import Mock

import pytest

from models.action_execution_record import ActionExecutionRecord


class MockStateMemory:
    """Mock StateMemory for testing.

    Simulates state tracking by maintaining a list of active state names
    that can be updated programmatically during tests.
    """

    def __init__(self, initial_states: Optional[List[str]] = None):
        """Initialize mock state memory.

        Args:
            initial_states: Initial list of active state names. Defaults to empty list.
        """
        self._active_states: List[str] = initial_states if initial_states else []

    def get_active_state_names(self) -> List[str]:
        """Get current active state names.

        Returns:
            List of active state names
        """
        return self._active_states.copy()

    def set_active_states(self, states: List[str]) -> None:
        """Set active states (for test control).

        Args:
            states: New list of active state names
        """
        self._active_states = states.copy()

    def add_state(self, state: str) -> None:
        """Add a state to active states.

        Args:
            state: State name to add
        """
        if state not in self._active_states:
            self._active_states.append(state)

    def remove_state(self, state: str) -> None:
        """Remove a state from active states.

        Args:
            state: State name to remove
        """
        if state in self._active_states:
            self._active_states.remove(state)


@pytest.fixture
def mock_state_memory():
    """Provide a MockStateMemory instance.

    Returns:
        MockStateMemory with empty initial state
    """
    return MockStateMemory()


@pytest.fixture
def mock_state_memory_with_states():
    """Provide a MockStateMemory instance with predefined states.

    Returns:
        MockStateMemory with ["Login", "MainMenu"] initial states
    """
    return MockStateMemory(["Login", "MainMenu"])


@pytest.fixture
def temp_storage_dir(tmp_path):
    """Provide a temporary directory for file operations.

    Args:
        tmp_path: pytest's built-in temporary directory fixture

    Returns:
        Path to temporary storage directory
    """
    storage = tmp_path / "storage"
    storage.mkdir()
    return storage


@pytest.fixture
def sample_action_record():
    """Provide a sample ActionExecutionRecord.

    Returns:
        ActionExecutionRecord with typical field values
    """
    return ActionExecutionRecord(
        action_id="test-action-001",
        action_type="CLICK",
        config={"x": 100, "y": 200},
        start_time=time.time(),
        end_time=time.time() + 0.5,
        duration_ms=500.0,
        active_states_before=frozenset(["Login"]),
        active_states_after=frozenset(["Login", "MainMenu"]),
        success=True,
        error_message=None,
        retry_count=0,
        clicked_location=(100, 200),
        click_button="left",
        click_target_type="button",
        parent_action_id=None,
        workflow_id="test-workflow",
        nesting_level=0,
        screenshot_reference="screenshots/screenshot-0001.png",
        metadata={"test": "data"},
    )


@pytest.fixture
def sample_failed_action_record():
    """Provide a sample failed ActionExecutionRecord.

    Returns:
        ActionExecutionRecord representing a failed action
    """
    return ActionExecutionRecord(
        action_id="test-action-002",
        action_type="FIND",
        config={"image": "login_button.png"},
        start_time=time.time(),
        end_time=time.time() + 1.0,
        duration_ms=1000.0,
        active_states_before=frozenset(["Login"]),
        active_states_after=frozenset(["Login"]),
        success=False,
        error_message="Image not found",
        retry_count=3,
        match_summary={
            "image_id": "login_button.png",
            "confidence": 0.45,
            "threshold": 0.8,
            "found": False,
            "location": None,
        },
        parent_action_id=None,
        workflow_id="test-workflow",
        nesting_level=0,
        metadata={},
    )


@pytest.fixture
def sample_running_action_record():
    """Provide a sample running (incomplete) ActionExecutionRecord.

    Returns:
        ActionExecutionRecord with no end_time (still running)
    """
    return ActionExecutionRecord(
        action_id="test-action-003",
        action_type="WAIT",
        config={"duration_ms": 5000},
        start_time=time.time(),
        end_time=None,
        duration_ms=None,
        active_states_before=frozenset(["Dashboard"]),
        active_states_after=frozenset(["Dashboard"]),
        success=True,
        parent_action_id=None,
        workflow_id="test-workflow",
        nesting_level=0,
        metadata={},
    )


def create_action_record(**kwargs: Any) -> ActionExecutionRecord:
    """Create a customized ActionExecutionRecord with default values.

    This helper function provides sensible defaults for all required fields
    and allows selective override via kwargs.

    Args:
        **kwargs: Field values to override defaults

    Returns:
        ActionExecutionRecord with specified or default values

    Example:
        >>> record = create_action_record(
        ...     action_id="custom-001",
        ...     action_type="TYPE",
        ...     typed_text="hello@example.com"
        ... )
    """
    defaults = {
        "action_id": f"action-{int(time.time() * 1000)}",
        "action_type": "CLICK",
        "config": {},
        "start_time": time.time(),
        "end_time": time.time() + 0.1,
        "duration_ms": 100.0,
        "active_states_before": frozenset(),
        "active_states_after": frozenset(),
        "success": True,
        "error_message": None,
        "retry_count": 0,
        "typed_text": None,
        "match_summary": None,
        "clicked_location": None,
        "click_button": None,
        "click_target_type": None,
        "transition_data": None,
        "condition_result": None,
        "branch_taken": None,
        "parent_action_id": None,
        "workflow_id": "test-workflow",
        "nesting_level": 0,
        "screenshot_reference": None,
        "visual_debug_reference": None,
        "metadata": {},
    }

    # Merge kwargs with defaults
    merged = {**defaults, **kwargs}

    return ActionExecutionRecord(**merged)


@pytest.fixture
def action_record_builder():
    """Provide the create_action_record builder function.

    Returns:
        Function that creates customized ActionExecutionRecord objects
    """
    return create_action_record


@pytest.fixture
def mock_screenshot_service():
    """Provide a mock ScreenshotService.

    Returns:
        Mock object with screenshot service methods
    """
    mock = Mock()
    mock.store_screenshot.return_value = "screenshots/screenshot-0001.png"
    mock.store_debug_visual.return_value = "debug_visuals/debug-0001-action-test.png"
    mock.get_screenshot.return_value = b"PNG_DATA"
    mock.cleanup_old_screenshots.return_value = 0
    return mock


@pytest.fixture
def sample_png_bytes():
    """Provide sample PNG file bytes for testing.

    Returns:
        Minimal valid PNG file as bytes
    """
    # Minimal valid PNG file (1x1 pixel, transparent)
    return (
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01"
        b"\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\nIDATx\x9cc\x00\x01"
        b"\x00\x00\x05\x00\x01\r\n-\xb4\x00\x00\x00\x00IEND\xaeB`\x82"
    )
