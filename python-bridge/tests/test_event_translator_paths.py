"""Test EventTranslator screenshot path integration.

This test verifies that when EventTranslator receives MATCH_ATTEMPTED events
with screenshot data, it:
1. Records to UnifiedDataCollector
2. Receives back absolute file paths
3. Includes paths in image_recognition events emitted to frontend
"""

import base64
import os
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any
from unittest.mock import Mock

import pytest

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

import event_translator
from event_manager import EventManager
from event_translator import EventTranslator

if TYPE_CHECKING:
    from qontinui.reporting import Event


def match_attempted(data: dict[str, Any]) -> "Event":
    """Wrap a MATCH_ATTEMPTED payload as qontinui's event registry delivers it.

    The import is deferred to call time, mirroring event_translator's own guarded
    import, so a box with no usable X display (pynput, imported by the qontinui
    package) still collects this module. On such a box these tests fail in
    ``EventTranslator.__init__`` unless an earlier module has already
    half-imported qontinui and left ``qontinui.reporting`` cached; in a full
    ``pytest tests`` run ``tests/services/test_trajectory_logger_wsm.py`` does
    exactly that and they pass. Run the suite under ``xvfb-run`` for a verdict
    that does not depend on collection order.
    """
    from qontinui.reporting import Event, EventType

    return Event(type=EventType.MATCH_ATTEMPTED, data=data)


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
        translator.on_match_attempted(match_attempted(library_event))

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

        translator.on_match_attempted(match_attempted(library_event))

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
        translator.on_match_attempted(match_attempted(library_event))

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

        translator.on_match_attempted(match_attempted(library_event))

        # Verify both image_data and paths are present
        event_name, event_data = mock_emitter.call_args[0]

        assert event_data["image_data"] == sample_png_base64  # From lookup
        assert event_data["screenshot_path"] == "/absolute/path/screenshots/test-0001.png"
        assert event_data["template_path"] == "/absolute/path/debug/test-0001.png"


@pytest.fixture
def isolated_temp_dir(tmp_path, monkeypatch):
    """Point every temp-dir source at an empty ``tmp_path`` subdirectory.

    Covers ``tempfile.gettempdir()`` (via ``tempfile.tempdir``) and the
    ``TEMP``/``TMP``/``TMPDIR`` environment names, which the deleted debug writes
    read (plan 2026-11-14-one-xlib-distribution-and-no-tmp-debug-logs-in-the-
    python-bridge, Phase 4).
    """
    temp = tmp_path / "os-temp"
    temp.mkdir()
    monkeypatch.setattr(tempfile, "tempdir", str(temp))
    for name in ("TEMP", "TMP", "TMPDIR"):
        monkeypatch.setenv(name, str(temp))
    return temp


class TestNoTempDirDebugLogs:
    """The event path writes nothing into the OS temp dir.

    These tests do not import qontinui: the event is a stand-in carrying only
    ``data`` (all ``on_match_attempted`` reads), and ``QONTINUI_AVAILABLE`` is
    forced on, so they need no X display and no collection-order luck.
    """

    def test_on_match_attempted_writes_nothing_to_temp_dir(
        self, isolated_temp_dir, monkeypatch, mock_emitter, sample_png_base64
    ):
        monkeypatch.setattr(event_translator, "QONTINUI_AVAILABLE", True)
        translator = EventTranslator(emitter=mock_emitter, collector=None)

        translator.on_match_attempted(
            SimpleNamespace(
                data={
                    "image_id": "button.png",
                    "similarity_threshold": 0.8,
                    "best_match_confidence": 0.95,
                    "best_match_location": {"x": 1, "y": 2},
                    "threshold_passed": True,
                    "screenshot_base64": sample_png_base64,
                }
            )
        )

        mock_emitter.assert_called_once()
        assert list(isolated_temp_dir.iterdir()) == []

    @staticmethod
    def _node() -> Mock:
        node = Mock()
        node.to_dict.return_value = {"id": "n1", "status": "running"}
        node.get_depth.return_value = 0
        node.node_type = "action"
        node.id = "n1"
        node.status = "running"
        node.get_path_from_root.return_value = ["n1"]
        return node

    def test_emit_tree_event_writes_nothing_to_temp_dir(self, isolated_temp_dir, capsys):
        EventManager().emit_tree_event("action_started", self._node())

        assert list(isolated_temp_dir.iterdir()) == []
        assert '"event_type": "action_started"' in capsys.readouterr().out

    @pytest.mark.skipif(
        hasattr(os, "geteuid") and os.geteuid() == 0, reason="root ignores directory modes"
    )
    def test_emit_tree_event_survives_read_only_temp_dir(self, isolated_temp_dir, capsys):
        isolated_temp_dir.chmod(0o555)
        try:
            EventManager().emit_tree_event("action_started", self._node())
        finally:
            isolated_temp_dir.chmod(0o755)

        assert '"event_type": "action_started"' in capsys.readouterr().out


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
