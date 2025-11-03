"""Test UnifiedDataCollector path return functionality.

This test verifies that UnifiedDataCollector.record_match_result() returns
absolute paths to saved screenshots and debug visuals, which can then be
included in frontend events.
"""

import sys
from pathlib import Path
from unittest.mock import Mock

import pytest

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from services.unified_data_collector import UnifiedDataCollector


@pytest.fixture
def mock_state_memory():
    """Mock state memory service."""
    state_memory = Mock()
    state_memory.get_active_states.return_value = ["TestState"]
    return state_memory


@pytest.fixture
def mock_screenshot_service():
    """Mock screenshot service that returns paths."""
    service = Mock()
    service.storage_dir = Path("/mock/storage")
    service.store_screenshot.return_value = "screenshots/screenshot-0001.png"
    service.store_debug_visual.return_value = "debug/debug-0001.png"
    return service


@pytest.fixture
def sample_png_bytes():
    """Return a minimal valid PNG image as bytes."""
    # 1x1 transparent PNG
    return (
        b'\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01'
        b'\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\nIDATx\x9cc\x00\x01'
        b'\x00\x00\x05\x00\x01\r\n-\xb4\x00\x00\x00\x00IEND\xaeB`\x82'
    )


class TestUnifiedDataCollectorPathReturn:
    """Test that UnifiedDataCollector returns paths correctly."""

    def test_record_match_result_returns_paths_dict(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test that record_match_result returns dict with both paths."""
        collector = UnifiedDataCollector(
            state_memory=mock_state_memory,
            screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {
            "image_id": "button.png",
            "confidence": 0.95,
            "found": True
        }

        # Call record_match_result with screenshot and debug visual
        result = collector.record_match_result(
            match_summary=match_summary,
            screenshot_data=sample_png_bytes,
            debug_visual_data=sample_png_bytes
        )

        # Verify result is a dict
        assert isinstance(result, dict)
        assert "screenshot_path" in result
        assert "debug_visual_path" in result

        # Verify paths are absolute
        screenshot_path = result["screenshot_path"]
        debug_visual_path = result["debug_visual_path"]

        assert screenshot_path == "/mock/storage/screenshots/screenshot-0001.png"
        assert debug_visual_path == "/mock/storage/debug/debug-0001.png"

        # Verify paths are strings
        assert isinstance(screenshot_path, str)
        assert isinstance(debug_visual_path, str)

    def test_record_match_result_returns_none_when_no_data(
        self, mock_state_memory, mock_screenshot_service
    ):
        """Test that paths are None when no screenshot data provided."""
        collector = UnifiedDataCollector(
            state_memory=mock_state_memory,
            screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "found": True}

        # Call without screenshot data
        result = collector.record_match_result(
            match_summary=match_summary,
            screenshot_data=None,
            debug_visual_data=None
        )

        # Verify result has None values
        assert result["screenshot_path"] is None
        assert result["debug_visual_path"] is None

    def test_record_match_result_returns_partial_paths(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test that only provided images get paths."""
        collector = UnifiedDataCollector(
            state_memory=mock_state_memory,
            screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "found": True}

        # Call with only screenshot data (no debug visual)
        result = collector.record_match_result(
            match_summary=match_summary,
            screenshot_data=sample_png_bytes,
            debug_visual_data=None
        )

        # Verify only screenshot path is set
        assert result["screenshot_path"] == "/mock/storage/screenshots/screenshot-0001.png"
        assert result["debug_visual_path"] is None

    def test_record_match_result_without_screenshot_service(
        self, mock_state_memory, sample_png_bytes
    ):
        """Test that None values returned when no screenshot service."""
        collector = UnifiedDataCollector(
            state_memory=mock_state_memory,
            screenshot_service=None  # No screenshot service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "found": True}

        # Call with screenshot data but no service
        result = collector.record_match_result(
            match_summary=match_summary,
            screenshot_data=sample_png_bytes,
            debug_visual_data=sample_png_bytes
        )

        # Verify paths are None when service unavailable
        assert result["screenshot_path"] is None
        assert result["debug_visual_path"] is None

    def test_record_match_result_handles_service_returning_none(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test handling when screenshot service returns None."""
        # Make service return None (storage failure)
        mock_screenshot_service.store_screenshot.return_value = None
        mock_screenshot_service.store_debug_visual.return_value = None

        collector = UnifiedDataCollector(
            state_memory=mock_state_memory,
            screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        match_summary = {"image_id": "button.png", "found": True}

        result = collector.record_match_result(
            match_summary=match_summary,
            screenshot_data=sample_png_bytes,
            debug_visual_data=sample_png_bytes
        )

        # Verify paths are None when service fails
        assert result["screenshot_path"] is None
        assert result["debug_visual_path"] is None

    def test_multiple_matches_return_different_paths(
        self, mock_state_memory, mock_screenshot_service, sample_png_bytes
    ):
        """Test that multiple calls return different paths."""
        # Setup service to return incrementing filenames
        mock_screenshot_service.store_screenshot.side_effect = [
            "screenshots/screenshot-0001.png",
            "screenshots/screenshot-0002.png",
        ]
        mock_screenshot_service.store_debug_visual.side_effect = [
            "debug/debug-0001.png",
            "debug/debug-0002.png",
        ]

        collector = UnifiedDataCollector(
            state_memory=mock_state_memory,
            screenshot_service=mock_screenshot_service
        )
        collector.start_action("action-001", None, "workflow-001", 0)

        # First match
        result1 = collector.record_match_result(
            match_summary={"image_id": "first.png", "found": True},
            screenshot_data=sample_png_bytes,
            debug_visual_data=sample_png_bytes
        )

        # Second match
        result2 = collector.record_match_result(
            match_summary={"image_id": "second.png", "found": True},
            screenshot_data=sample_png_bytes,
            debug_visual_data=sample_png_bytes
        )

        # Verify paths are different
        assert result1["screenshot_path"] == "/mock/storage/screenshots/screenshot-0001.png"
        assert result2["screenshot_path"] == "/mock/storage/screenshots/screenshot-0002.png"
        assert result1["debug_visual_path"] == "/mock/storage/debug/debug-0001.png"
        assert result2["debug_visual_path"] == "/mock/storage/debug/debug-0002.png"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
