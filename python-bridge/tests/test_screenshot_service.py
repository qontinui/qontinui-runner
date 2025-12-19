"""Comprehensive unit tests for ScreenshotService.

This module tests the ScreenshotService:
- Service initialization and directory creation
- Screenshot storage with sequential numbering
- Metadata sidecar file creation
- Debug visual storage
- Screenshot retrieval by reference
- Cleanup with retention policy
- Disabled mode (all methods return None)
- Missing file handling
"""

import json

from services.screenshot_service import ScreenshotService


class TestScreenshotServiceInitialization:
    """Test ScreenshotService initialization."""

    def test_init_creates_directories(self, temp_storage_dir):
        """Test that initialization creates necessary directories."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        assert service.screenshots_dir.exists()
        assert service.debug_visuals_dir.exists()
        assert service.screenshots_dir == temp_storage_dir / "screenshots"
        assert service.debug_visuals_dir == temp_storage_dir / "debug_visuals"

    def test_init_disabled_does_not_create_directories(self, temp_storage_dir):
        """Test that disabled service doesn't create directories."""
        # Remove the temp_storage_dir to test it doesn't get created
        storage_dir = temp_storage_dir / "disabled_test"

        service = ScreenshotService(storage_dir, enabled=False)

        assert not service.screenshots_dir.exists()
        assert not service.debug_visuals_dir.exists()

    def test_init_with_existing_directories(self, temp_storage_dir):
        """Test initialization when directories already exist."""
        # Pre-create directories
        (temp_storage_dir / "screenshots").mkdir(parents=True)
        (temp_storage_dir / "debug_visuals").mkdir(parents=True)

        # Should not raise error
        service = ScreenshotService(temp_storage_dir, enabled=True)

        assert service.screenshots_dir.exists()
        assert service.debug_visuals_dir.exists()


class TestStoreScreenshot:
    """Test store_screenshot method."""

    def test_store_screenshot_basic(self, temp_storage_dir, sample_png_bytes):
        """Test basic screenshot storage."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        reference = service.store_screenshot(
            screenshot_data=sample_png_bytes,
            action_id="action-001",
            active_states=["Login", "MainMenu"],
            metadata={"test": "data"},
        )

        assert reference == "screenshots/screenshot-0001.png"

        # Verify file exists
        screenshot_path = temp_storage_dir / "screenshots" / "screenshot-0001.png"
        assert screenshot_path.exists()
        assert screenshot_path.read_bytes() == sample_png_bytes

    def test_store_screenshot_creates_metadata(self, temp_storage_dir, sample_png_bytes):
        """Test that screenshot storage creates metadata sidecar."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        service.store_screenshot(
            screenshot_data=sample_png_bytes,
            action_id="action-001",
            active_states=["Login"],
            metadata={"action_type": "CLICK", "success": True},
        )

        # Verify metadata file exists
        metadata_path = temp_storage_dir / "screenshots" / "screenshot-0001.json"
        assert metadata_path.exists()

        # Verify metadata content
        metadata = json.loads(metadata_path.read_text())
        assert metadata["screenshot_number"] == 1
        assert metadata["action_id"] == "action-001"
        assert metadata["active_states"] == ["Login"]
        assert metadata["action_type"] == "CLICK"
        assert metadata["success"] is True
        assert "timestamp" in metadata
        assert metadata["filename"] == "screenshot-0001.png"

    def test_store_screenshot_sequential_numbering(self, temp_storage_dir, sample_png_bytes):
        """Test that screenshots are numbered sequentially."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        ref1 = service.store_screenshot(sample_png_bytes, "action-001", [])
        ref2 = service.store_screenshot(sample_png_bytes, "action-002", [])
        ref3 = service.store_screenshot(sample_png_bytes, "action-003", [])

        assert ref1 == "screenshots/screenshot-0001.png"
        assert ref2 == "screenshots/screenshot-0002.png"
        assert ref3 == "screenshots/screenshot-0003.png"

    def test_store_screenshot_disabled_returns_none(self, temp_storage_dir, sample_png_bytes):
        """Test that disabled service returns None."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        reference = service.store_screenshot(
            screenshot_data=sample_png_bytes, action_id="action-001", active_states=[]
        )

        assert reference is None

    def test_store_screenshot_with_empty_metadata(self, temp_storage_dir, sample_png_bytes):
        """Test storing screenshot with no additional metadata."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        reference = service.store_screenshot(
            screenshot_data=sample_png_bytes,
            action_id="action-001",
            active_states=[],
            metadata=None,
        )

        assert reference == "screenshots/screenshot-0001.png"

        # Verify metadata still has required fields
        metadata_path = temp_storage_dir / "screenshots" / "screenshot-0001.json"
        metadata = json.loads(metadata_path.read_text())
        assert "screenshot_number" in metadata
        assert "action_id" in metadata
        assert "timestamp" in metadata

    def test_store_screenshot_with_empty_active_states(self, temp_storage_dir, sample_png_bytes):
        """Test storing screenshot with no active states."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        service.store_screenshot(
            screenshot_data=sample_png_bytes, action_id="action-001", active_states=[]
        )

        metadata_path = temp_storage_dir / "screenshots" / "screenshot-0001.json"
        metadata = json.loads(metadata_path.read_text())
        assert metadata["active_states"] == []


class TestStoreDebugVisual:
    """Test store_debug_visual method."""

    def test_store_debug_visual_basic(self, temp_storage_dir, sample_png_bytes):
        """Test basic debug visual storage."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        match_info = {
            "confidence": 0.95,
            "threshold": 0.8,
            "location": {"x": 100, "y": 200},
            "template_name": "button.png",
            "found": True,
        }

        reference = service.store_debug_visual(
            debug_image_data=sample_png_bytes, action_id="find-button-001", match_info=match_info
        )

        assert reference == "debug_visuals/debug-0001-action-find-button-001.png"

        # Verify file exists
        debug_path = temp_storage_dir / "debug_visuals" / "debug-0001-action-find-button-001.png"
        assert debug_path.exists()
        assert debug_path.read_bytes() == sample_png_bytes

    def test_store_debug_visual_creates_metadata(self, temp_storage_dir, sample_png_bytes):
        """Test that debug visual storage creates metadata."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        match_info = {"confidence": 0.95, "found": True, "location": {"x": 100, "y": 200}}

        service.store_debug_visual(
            debug_image_data=sample_png_bytes, action_id="action-001", match_info=match_info
        )

        # Verify metadata file exists
        metadata_path = temp_storage_dir / "debug_visuals" / "debug-0001-action-action-001.json"
        assert metadata_path.exists()

        # Verify metadata content
        metadata = json.loads(metadata_path.read_text())
        assert metadata["debug_number"] == 1
        assert metadata["action_id"] == "action-001"
        assert metadata["match_info"]["confidence"] == 0.95
        assert metadata["match_info"]["found"] is True
        assert "timestamp" in metadata

    def test_store_debug_visual_sanitizes_action_id(self, temp_storage_dir, sample_png_bytes):
        """Test that action_id is sanitized for filename safety."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Action ID with special characters
        reference = service.store_debug_visual(
            debug_image_data=sample_png_bytes,
            action_id="action/with:special*chars?",
            match_info={"found": True},
        )

        # Special characters should be replaced with hyphens
        assert "action-with-special-chars" in reference
        assert "/" not in reference
        assert ":" not in reference
        assert "*" not in reference
        assert "?" not in reference

    def test_store_debug_visual_sequential_numbering(self, temp_storage_dir, sample_png_bytes):
        """Test sequential numbering for debug visuals."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        ref1 = service.store_debug_visual(sample_png_bytes, "action-001", {})
        ref2 = service.store_debug_visual(sample_png_bytes, "action-002", {})
        ref3 = service.store_debug_visual(sample_png_bytes, "action-003", {})

        assert "debug-0001-" in ref1
        assert "debug-0002-" in ref2
        assert "debug-0003-" in ref3

    def test_store_debug_visual_disabled_returns_none(self, temp_storage_dir, sample_png_bytes):
        """Test that disabled service returns None."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        reference = service.store_debug_visual(
            debug_image_data=sample_png_bytes, action_id="action-001", match_info={}
        )

        assert reference is None


class TestGetScreenshot:
    """Test get_screenshot method."""

    def test_get_screenshot_by_relative_path(self, temp_storage_dir, sample_png_bytes):
        """Test retrieving screenshot by relative path."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Store a screenshot
        reference = service.store_screenshot(sample_png_bytes, "action-001", [])

        # Retrieve it
        retrieved_data = service.get_screenshot(reference)

        assert retrieved_data == sample_png_bytes

    def test_get_screenshot_by_absolute_path(self, temp_storage_dir, sample_png_bytes):
        """Test retrieving screenshot by absolute path."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Store a screenshot
        service.store_screenshot(sample_png_bytes, "action-001", [])

        # Retrieve by absolute path
        absolute_path = temp_storage_dir / "screenshots" / "screenshot-0001.png"
        retrieved_data = service.get_screenshot(str(absolute_path))

        assert retrieved_data == sample_png_bytes

    def test_get_screenshot_missing_file_returns_none(self, temp_storage_dir):
        """Test that getting non-existent screenshot returns None."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        retrieved_data = service.get_screenshot("screenshots/nonexistent.png")

        assert retrieved_data is None

    def test_get_screenshot_disabled_returns_none(self, temp_storage_dir):
        """Test that disabled service returns None."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        retrieved_data = service.get_screenshot("screenshots/screenshot-0001.png")

        assert retrieved_data is None


class TestCleanupOldScreenshots:
    """Test cleanup_old_screenshots method."""

    def test_cleanup_keeps_recent_screenshots(self, temp_storage_dir, sample_png_bytes):
        """Test that cleanup keeps the most recent screenshots."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create 10 screenshots
        for i in range(10):
            service.store_screenshot(sample_png_bytes, f"action-{i:03d}", [])

        # Keep only the last 5
        deleted = service.cleanup_old_screenshots(keep_last_n=5)

        assert deleted == 5

        # Verify only screenshots 6-10 remain
        screenshots = sorted(service.screenshots_dir.glob("screenshot-*.png"))
        assert len(screenshots) == 5
        assert screenshots[0].name == "screenshot-0006.png"
        assert screenshots[4].name == "screenshot-0010.png"

    def test_cleanup_deletes_metadata_files(self, temp_storage_dir, sample_png_bytes):
        """Test that cleanup also deletes metadata files."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create 5 screenshots
        for i in range(5):
            service.store_screenshot(sample_png_bytes, f"action-{i:03d}", [])

        # Keep only the last 2
        service.cleanup_old_screenshots(keep_last_n=2)

        # Verify metadata files were also deleted
        metadata_files = sorted(service.screenshots_dir.glob("screenshot-*.json"))
        assert len(metadata_files) == 2
        assert metadata_files[0].name == "screenshot-0004.json"
        assert metadata_files[1].name == "screenshot-0005.json"

    def test_cleanup_with_no_screenshots(self, temp_storage_dir):
        """Test cleanup when there are no screenshots."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        deleted = service.cleanup_old_screenshots(keep_last_n=5)

        assert deleted == 0

    def test_cleanup_with_fewer_than_limit(self, temp_storage_dir, sample_png_bytes):
        """Test cleanup when there are fewer screenshots than limit."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create 3 screenshots
        for i in range(3):
            service.store_screenshot(sample_png_bytes, f"action-{i:03d}", [])

        # Try to keep 10
        deleted = service.cleanup_old_screenshots(keep_last_n=10)

        assert deleted == 0

        # All screenshots should remain
        screenshots = sorted(service.screenshots_dir.glob("screenshot-*.png"))
        assert len(screenshots) == 3

    def test_cleanup_disabled_returns_zero(self, temp_storage_dir):
        """Test that disabled service returns 0."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        deleted = service.cleanup_old_screenshots(keep_last_n=5)

        assert deleted == 0

    def test_cleanup_debug_visuals(self, temp_storage_dir, sample_png_bytes):
        """Test that cleanup also cleans debug visuals."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create 5 debug visuals
        for i in range(5):
            service.store_debug_visual(sample_png_bytes, f"action-{i:03d}", {})

        # Keep only the last 2
        service.cleanup_old_screenshots(keep_last_n=2)

        # Should have deleted 3 debug visuals (plus any screenshots)
        debug_visuals = sorted(service.debug_visuals_dir.glob("debug-*.png"))
        assert len(debug_visuals) == 2

    def test_cleanup_zero_keeps_none(self, temp_storage_dir, sample_png_bytes):
        """Test that keep_last_n=0 deletes all screenshots."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create 5 screenshots
        for i in range(5):
            service.store_screenshot(sample_png_bytes, f"action-{i:03d}", [])

        # Keep none
        deleted = service.cleanup_old_screenshots(keep_last_n=0)

        assert deleted == 5

        # Verify all are deleted
        screenshots = list(service.screenshots_dir.glob("screenshot-*.png"))
        assert len(screenshots) == 0


class TestFilenameSanitization:
    """Test _sanitize_filename method."""

    def test_sanitize_removes_special_characters(self, temp_storage_dir):
        """Test that special characters are removed."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        sanitized = service._sanitize_filename("action/with:special*chars?")

        assert "/" not in sanitized
        assert ":" not in sanitized
        assert "*" not in sanitized
        assert "?" not in sanitized

    def test_sanitize_replaces_spaces(self, temp_storage_dir):
        """Test that spaces are replaced with hyphens."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        sanitized = service._sanitize_filename("action with spaces")

        assert " " not in sanitized
        assert "-" in sanitized

    def test_sanitize_removes_consecutive_hyphens(self, temp_storage_dir):
        """Test that consecutive hyphens are collapsed."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        sanitized = service._sanitize_filename("action---multiple---hyphens")

        assert "---" not in sanitized
        assert "--" not in sanitized

    def test_sanitize_removes_leading_trailing_hyphens(self, temp_storage_dir):
        """Test that leading/trailing hyphens are removed."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        sanitized = service._sanitize_filename("-action-name-")

        assert not sanitized.startswith("-")
        assert not sanitized.endswith("-")

    def test_sanitize_limits_length(self, temp_storage_dir):
        """Test that long filenames are truncated."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        long_name = "a" * 200
        sanitized = service._sanitize_filename(long_name)

        assert len(sanitized) <= 100


class TestNextNumberCalculation:
    """Test _get_next_screenshot_number and _get_next_debug_visual_number."""

    def test_next_screenshot_number_starts_at_one(self, temp_storage_dir):
        """Test that first screenshot number is 1."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        next_num = service._get_next_screenshot_number()

        assert next_num == 1

    def test_next_screenshot_number_increments(self, temp_storage_dir, sample_png_bytes):
        """Test that screenshot numbers increment."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create some screenshots
        service.store_screenshot(sample_png_bytes, "action-001", [])
        service.store_screenshot(sample_png_bytes, "action-002", [])

        next_num = service._get_next_screenshot_number()

        assert next_num == 3

    def test_next_debug_visual_number_starts_at_one(self, temp_storage_dir):
        """Test that first debug visual number is 1."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        next_num = service._get_next_debug_visual_number()

        assert next_num == 1

    def test_next_debug_visual_number_increments(self, temp_storage_dir, sample_png_bytes):
        """Test that debug visual numbers increment."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create some debug visuals
        service.store_debug_visual(sample_png_bytes, "action-001", {})
        service.store_debug_visual(sample_png_bytes, "action-002", {})

        next_num = service._get_next_debug_visual_number()

        assert next_num == 3

    def test_next_number_handles_gaps(self, temp_storage_dir, sample_png_bytes):
        """Test that next number handles gaps in sequence."""
        service = ScreenshotService(temp_storage_dir, enabled=True)

        # Create screenshots 1, 2, 3
        service.store_screenshot(sample_png_bytes, "action-001", [])
        service.store_screenshot(sample_png_bytes, "action-002", [])
        service.store_screenshot(sample_png_bytes, "action-003", [])

        # Manually delete #2
        (service.screenshots_dir / "screenshot-0002.png").unlink()

        # Next number should still be 4 (max + 1)
        next_num = service._get_next_screenshot_number()

        assert next_num == 4


class TestDisabledMode:
    """Test that all methods handle disabled mode correctly."""

    def test_disabled_store_screenshot_returns_none(self, temp_storage_dir, sample_png_bytes):
        """Test store_screenshot returns None when disabled."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        result = service.store_screenshot(sample_png_bytes, "action-001", [])

        assert result is None

    def test_disabled_store_debug_visual_returns_none(self, temp_storage_dir, sample_png_bytes):
        """Test store_debug_visual returns None when disabled."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        result = service.store_debug_visual(sample_png_bytes, "action-001", {})

        assert result is None

    def test_disabled_get_screenshot_returns_none(self, temp_storage_dir):
        """Test get_screenshot returns None when disabled."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        result = service.get_screenshot("screenshots/screenshot-0001.png")

        assert result is None

    def test_disabled_cleanup_returns_zero(self, temp_storage_dir):
        """Test cleanup_old_screenshots returns 0 when disabled."""
        service = ScreenshotService(temp_storage_dir, enabled=False)

        result = service.cleanup_old_screenshots(keep_last_n=5)

        assert result == 0

    def test_disabled_no_filesystem_operations(self, temp_storage_dir, sample_png_bytes):
        """Test that disabled service performs no filesystem operations."""
        storage_dir = temp_storage_dir / "disabled_test"
        service = ScreenshotService(storage_dir, enabled=False)

        # Try various operations
        service.store_screenshot(sample_png_bytes, "action-001", [])
        service.store_debug_visual(sample_png_bytes, "action-001", {})
        service.get_screenshot("test.png")
        service.cleanup_old_screenshots(keep_last_n=5)

        # Directory should not have been created
        assert not storage_dir.exists()
