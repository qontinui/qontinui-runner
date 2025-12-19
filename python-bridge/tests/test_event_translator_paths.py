"""Test EventTranslator screenshot path integration.

This test verifies that when EventTranslator receives MATCH_ATTEMPTED events
with screenshot data, it:
1. Records to UnifiedDataCollector
2. Receives back absolute file paths
3. Includes paths in image_recognition events emitted to frontend
"""

import base64
import sys
from pathlib import Path
from unittest.mock import Mock

import pytest

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from event_translator import EventTranslator


@pytest.fixture
def mock_collector():
    """Mock UnifiedDataCollector that returns paths."""
    collector = Mock()
    collector.record_match_result.return_value = {
        "screenshot_path": "/absolute/path/screenshots/test-0001.png",
        "debug_visual_path": "/absolute/path/debug/test-0001.png",
    }
    return collector


@pytest.fixture
def mock_emitter():
    """Mock event emitter to capture emitted events."""
    return Mock()


@pytest.fixture
def sample_png_base64():
    """Return a minimal valid PNG image as base64."""
    # 1x1 transparent PNG
    png_bytes = (
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01"
        b"\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\nIDATx\x9cc\x00\x01"
        b"\x00\x00\x05\x00\x01\r\n-\xb4\x00\x00\x00\x00IEND\xaeB`\x82"
    )
    return base64.b64encode(png_bytes).decode("utf-8")


class TestEventTranslatorPathIntegration:
    """Test EventTranslator screenshot path integration."""

    def test_match_attempted_with_screenshot_includes_paths(
        self, mock_collector, mock_emitter, sample_png_base64
    ):
        """Test that screenshot paths are included in emitted events."""
        translator = EventTranslator(
            emitter=mock_emitter,
            collector=mock_collector,
            state_lookup=None,
            hierarchy_lookup=None,
            image_data_lookup=None,
        )

        # Simulate MATCH_ATTEMPTED event from library
        library_event = {
            "image_id": "button.png",
            "template_dimensions": {"width": 100, "height": 50},
            "screenshot_dimensions": {"width": 1920, "height": 1080},
            "similarity_threshold": 0.8,
            "best_match_confidence": 0.95,
            "best_match_location": {"x": 500, "y": 300},
            "threshold_passed": True,
            "screenshot_base64": sample_png_base64,
            "debug_visual_base64": sample_png_base64,
        }

        # Trigger event handler
        translator.on_match_attempted(library_event)

        # Verify collector was called with decoded data
        mock_collector.record_match_result.assert_called_once()
        call_args = mock_collector.record_match_result.call_args

        assert call_args.kwargs["match_summary"]["image_id"] == "button.png"
        assert call_args.kwargs["match_summary"]["found"] is True
        assert call_args.kwargs["screenshot_data"] is not None
        assert call_args.kwargs["debug_visual_data"] is not None

        # Verify emitter was called with paths
        mock_emitter.assert_called_once()
        event_name, event_data = mock_emitter.call_args[0]

        assert event_name == "image_recognition"
        assert event_data["image_path"] == "button.png"
        assert event_data["found"] is True
        assert event_data["confidence"] == 0.95
        assert event_data["screenshot_path"] == "/absolute/path/screenshots/test-0001.png"
        assert event_data["template_path"] == "/absolute/path/debug/test-0001.png"

    def test_match_attempted_without_collector(self, mock_emitter, sample_png_base64):
        """Test that events work even without collector (no paths)."""
        translator = EventTranslator(
            emitter=mock_emitter,
            collector=None,  # No collector
            state_lookup=None,
            hierarchy_lookup=None,
            image_data_lookup=None,
        )

        library_event = {
            "image_id": "icon.png",
            "template_dimensions": {"width": 32, "height": 32},
            "screenshot_dimensions": {"width": 1920, "height": 1080},
            "similarity_threshold": 0.8,
            "best_match_confidence": 0.75,
            "best_match_location": {"x": 200, "y": 150},
            "threshold_passed": False,
            "screenshot_base64": sample_png_base64,
        }

        translator.on_match_attempted(library_event)

        # Verify emitter was called without paths
        mock_emitter.assert_called_once()
        event_name, event_data = mock_emitter.call_args[0]

        assert event_name == "image_recognition"
        assert event_data["image_path"] == "icon.png"
        assert event_data["found"] is False
        assert "screenshot_path" not in event_data
        assert "template_path" not in event_data

    def test_match_attempted_collector_error_handling(
        self, mock_collector, mock_emitter, sample_png_base64
    ):
        """Test that collector errors don't crash event emission."""
        translator = EventTranslator(
            emitter=mock_emitter,
            collector=mock_collector,
            state_lookup=None,
            hierarchy_lookup=None,
            image_data_lookup=None,
        )

        # Make collector raise an error
        mock_collector.record_match_result.side_effect = Exception("Storage full")

        library_event = {
            "image_id": "button.png",
            "template_dimensions": {"width": 100, "height": 50},
            "screenshot_dimensions": {"width": 1920, "height": 1080},
            "similarity_threshold": 0.8,
            "best_match_confidence": 0.95,
            "best_match_location": {"x": 500, "y": 300},
            "threshold_passed": True,
            "screenshot_base64": sample_png_base64,
        }

        # Should not crash
        translator.on_match_attempted(library_event)

        # Verify emitter was still called (without paths)
        mock_emitter.assert_called_once()
        event_name, event_data = mock_emitter.call_args[0]

        assert event_name == "image_recognition"
        assert event_data["found"] is True
        assert "screenshot_path" not in event_data

    def test_match_attempted_with_image_data_lookup(
        self, mock_collector, mock_emitter, sample_png_base64
    ):
        """Test that image_data_lookup is still called for template base64."""

        def image_data_lookup(image_id):
            if image_id == "button.png":
                return sample_png_base64
            return None

        translator = EventTranslator(
            emitter=mock_emitter,
            collector=mock_collector,
            state_lookup=None,
            hierarchy_lookup=None,
            image_data_lookup=image_data_lookup,
        )

        library_event = {
            "image_id": "button.png",
            "template_dimensions": {"width": 100, "height": 50},
            "screenshot_dimensions": {"width": 1920, "height": 1080},
            "similarity_threshold": 0.8,
            "best_match_confidence": 0.95,
            "best_match_location": {"x": 500, "y": 300},
            "threshold_passed": True,
            "screenshot_base64": sample_png_base64,
        }

        translator.on_match_attempted(library_event)

        # Verify both image_data and paths are present
        event_name, event_data = mock_emitter.call_args[0]

        assert event_data["image_data"] == sample_png_base64  # From lookup
        assert event_data["screenshot_path"] == "/absolute/path/screenshots/test-0001.png"
        assert event_data["template_path"] == "/absolute/path/debug/test-0001.png"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
