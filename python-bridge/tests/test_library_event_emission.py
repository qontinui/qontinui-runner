"""Test that qontinui library emits MATCH_ATTEMPTED events during FIND actions.

This test verifies that the FindImageOrchestrator correctly emits MATCH_ATTEMPTED
events when performing image matching operations.
"""

import sys
from unittest.mock import Mock

import numpy as np
import pytest

# Mock qontinui imports before importing
sys.modules["qontinui.reporting"] = Mock()
sys.modules["qontinui.reporting.EventType"] = Mock()
sys.modules["qontinui.reporting.emit_event"] = Mock()


def test_library_emits_match_attempted_event():
    """Test that FindImageOrchestrator emits MATCH_ATTEMPTED event."""
    # Import after mocking
    from qontinui.actions.basic.find.implementations.find_image.find_image_orchestrator import (
        FindImageOrchestrator,
    )
    from qontinui.reporting import EventType, emit_event

    # Create orchestrator
    orchestrator = FindImageOrchestrator()

    # Create mock pattern
    mock_pattern = Mock()
    mock_pattern.name = "test_button.png"

    # Create mock template and screenshot (1x1 images for testing)
    template = np.zeros((50, 100, 3), dtype=np.uint8)
    screenshot = np.zeros((1080, 1920, 3), dtype=np.uint8)

    # Create mock options
    mock_options = Mock()
    mock_options.similarity = 0.8
    mock_options.match_method = Mock()
    mock_options.match_method.name = "CCOEFF_NORMED"
    mock_options.search_type = Mock()
    mock_options.search_type.name = "BEST"
    mock_options.scale_invariant = False
    mock_options.use_grayscale = False
    mock_options.use_edges = False

    # Create mock matches (empty - no match found)
    mock_matches = []

    # Call the event emission method directly
    orchestrator._emit_match_attempted_event(
        pattern=mock_pattern,
        template=template,
        screenshot=screenshot,
        options=mock_options,
        matches=mock_matches,
    )

    # Verify emit_event was called
    emit_event.assert_called_once()

    # Verify the call arguments
    call_args = emit_event.call_args
    assert call_args[0][0] == EventType.MATCH_ATTEMPTED

    event_data = call_args[1]["data"]
    assert event_data["image_id"] == "test_button.png"
    assert event_data["template_dimensions"] == {"width": 100, "height": 50}
    assert event_data["screenshot_dimensions"] == {"width": 1920, "height": 1080}
    assert event_data["similarity_threshold"] == 0.8
    assert event_data["best_match_confidence"] == 0.0  # No match
    assert event_data["threshold_passed"] is False
    assert event_data["match_method"] == "CCOEFF_NORMED"
    assert event_data["find_all_mode"] is False


def test_library_emits_event_with_match_found():
    """Test event emission when match is found."""
    from qontinui.actions.basic.find.implementations.find_image.find_image_orchestrator import (
        FindImageOrchestrator,
    )
    from qontinui.reporting import emit_event

    orchestrator = FindImageOrchestrator()

    # Create mock pattern
    mock_pattern = Mock()
    mock_pattern.name = "found_icon.png"

    # Create mock template and screenshot
    template = np.zeros((32, 32, 3), dtype=np.uint8)
    screenshot = np.zeros((1080, 1920, 3), dtype=np.uint8)

    # Create mock options
    mock_options = Mock()
    mock_options.similarity = 0.8
    mock_options.match_method = Mock()
    mock_options.match_method.name = "CCOEFF_NORMED"
    mock_options.search_type = Mock()
    mock_options.search_type.name = "BEST"

    # Create mock match (match found!)
    mock_match = Mock()
    mock_match.score = 0.95
    mock_match.target = Mock()
    mock_match.target.region = Mock()
    mock_match.target.region.x = 500
    mock_match.target.region.y = 300

    mock_matches = [mock_match]

    # Clear previous calls
    emit_event.reset_mock()

    # Call the event emission method
    orchestrator._emit_match_attempted_event(
        pattern=mock_pattern,
        template=template,
        screenshot=screenshot,
        options=mock_options,
        matches=mock_matches,
    )

    # Verify emit_event was called
    emit_event.assert_called_once()

    # Verify the call arguments
    call_args = emit_event.call_args
    event_data = call_args[1]["data"]

    assert event_data["image_id"] == "found_icon.png"
    assert event_data["best_match_confidence"] == 0.95
    assert event_data["best_match_location"] == {"x": 500, "y": 300}
    assert event_data["threshold_passed"] is True  # 0.95 > 0.8


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
