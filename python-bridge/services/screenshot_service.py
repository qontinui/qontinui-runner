"""Screenshot Service Module.

This module implements the ScreenshotService class, which follows the Single Responsibility
Principle (SRP) by focusing solely on screenshot lifecycle management. The service handles:
- Storing screenshots with metadata
- Storing debug visuals
- Retrieving screenshots by reference
- Cleaning up old screenshots

The service is designed to be:
- Filesystem-safe: Proper path handling and sanitization
- Disabled-friendly: Gracefully handles disabled state
- Metadata-rich: Stores contextual information alongside images
- Sequential: Uses predictable numbering for screenshots
"""

import json
import re
from datetime import datetime
from pathlib import Path
from typing import Any


class ScreenshotService:
    """Service for managing screenshot lifecycle.

    This service follows the Single Responsibility Principle by exclusively handling
    screenshot-related operations. It does not handle:
    - Action execution (delegated to ActionExecutor)
    - Event emission (delegated to EventEmitter)
    - State management (delegated to StateManager)

    Responsibilities:
    1. Store screenshots with sequential numbering
    2. Store metadata sidecar files
    3. Store debug visualization images
    4. Retrieve screenshots by reference
    5. Clean up old screenshots based on retention policy

    Directory Structure:
        storage_dir/
            screenshots/
                screenshot-0001.png
                screenshot-0001.json  (metadata)
                screenshot-0002.png
                screenshot-0002.json
                ...
            debug_visuals/
                debug-0001-action-123.png
                debug-0001-action-123.json  (match info)
                ...

    The service can be disabled via the `enabled` parameter, in which case all
    operations return None without performing any I/O.
    """

    def __init__(self, storage_dir: Path, enabled: bool = True) -> None:
        """Initialize the ScreenshotService.

        Creates the necessary directory structure and prepares the service for use.

        Args:
            storage_dir: Root directory for storing screenshots and debug visuals.
                        Must be a pathlib.Path object.
            enabled: Whether the service is enabled. If False, all operations
                    return None without performing any filesystem operations.

        Example:
            >>> from pathlib import Path
            >>> service = ScreenshotService(Path("/tmp/screenshots"), enabled=True)
            >>> # Service is ready to store screenshots
        """
        self.storage_dir = storage_dir
        self.enabled = enabled
        self.screenshots_dir = storage_dir / "screenshots"
        self.debug_visuals_dir = storage_dir / "debug_visuals"

        # Create directory structure if enabled
        if self.enabled:
            self.screenshots_dir.mkdir(parents=True, exist_ok=True)
            self.debug_visuals_dir.mkdir(parents=True, exist_ok=True)

    def store_screenshot(
        self,
        screenshot_data: bytes,
        action_id: str,
        active_states: list[str],
        metadata: dict[str, Any] | None = None,
    ) -> str | None:
        """Store a screenshot with associated metadata.

        This method:
        1. Determines the next sequential screenshot number
        2. Saves the screenshot image to disk
        3. Creates a metadata sidecar JSON file
        4. Returns a relative path reference

        Args:
            screenshot_data: Raw PNG image data as bytes.
            action_id: Identifier for the action that triggered this screenshot.
            active_states: List of active state names when screenshot was taken.
            metadata: Optional additional metadata to store. Common fields:
                     - timestamp: When the screenshot was taken
                     - action_type: Type of action executed
                     - success: Whether the action succeeded

        Returns:
            Relative path to the screenshot (e.g., "screenshots/screenshot-0001.png")
            or None if the service is disabled.

        Example:
            >>> screenshot_ref = service.store_screenshot(
            ...     png_data,
            ...     action_id="click-button-123",
            ...     active_states=["Login", "MainMenu"],
            ...     metadata={"action_type": "CLICK", "success": True}
            ... )
            >>> print(screenshot_ref)
            screenshots/screenshot-0001.png
        """
        if not self.enabled:
            return None

        # Find next available screenshot number
        next_num = self._get_next_screenshot_number()

        # Create filenames
        screenshot_filename = f"screenshot-{next_num:04d}.png"
        metadata_filename = f"screenshot-{next_num:04d}.json"

        screenshot_path = self.screenshots_dir / screenshot_filename
        metadata_path = self.screenshots_dir / metadata_filename

        # Write screenshot data
        screenshot_path.write_bytes(screenshot_data)

        # Build metadata
        full_metadata = {
            "screenshot_number": next_num,
            "action_id": action_id,
            "active_states": active_states,
            "timestamp": datetime.utcnow().isoformat(),
            "filename": screenshot_filename,
        }

        # Merge additional metadata if provided
        if metadata:
            full_metadata.update(metadata)

        # Write metadata
        metadata_path.write_text(
            json.dumps(full_metadata, indent=2, ensure_ascii=False), encoding="utf-8"
        )

        # Return relative path
        return f"screenshots/{screenshot_filename}"

    def store_debug_visual(
        self, debug_image_data: bytes, action_id: str, match_info: dict[str, Any]
    ) -> str | None:
        """Store a debug visualization image with match information.

        Debug visuals are typically annotated screenshots showing where template
        matching occurred, confidence levels, and other diagnostic information.

        This method:
        1. Determines the next sequential debug visual number
        2. Saves the debug image to disk
        3. Creates a metadata JSON file with match information
        4. Returns a relative path reference

        Args:
            debug_image_data: Raw PNG image data as bytes (typically annotated).
            action_id: Identifier for the action being debugged.
            match_info: Dictionary containing match/recognition information:
                       - confidence: Match confidence score (0.0-1.0)
                       - threshold: Threshold used for matching
                       - location: Coordinates where match was found
                       - template_name: Name of the template image
                       - found: Whether a match was found

        Returns:
            Relative path to the debug visual (e.g., "debug_visuals/debug-0001-action-123.png")
            or None if the service is disabled.

        Example:
            >>> debug_ref = service.store_debug_visual(
            ...     annotated_png_data,
            ...     action_id="find-logo-456",
            ...     match_info={
            ...         "confidence": 0.95,
            ...         "threshold": 0.8,
            ...         "location": {"x": 100, "y": 200},
            ...         "template_name": "company_logo.png",
            ...         "found": True
            ...     }
            ... )
            >>> print(debug_ref)
            debug_visuals/debug-0001-action-find-logo-456.png
        """
        if not self.enabled:
            return None

        # Find next available debug visual number
        next_num = self._get_next_debug_visual_number()

        # Sanitize action_id for filename (remove special characters)
        safe_action_id = self._sanitize_filename(action_id)

        # Create filenames
        debug_filename = f"debug-{next_num:04d}-action-{safe_action_id}.png"
        metadata_filename = f"debug-{next_num:04d}-action-{safe_action_id}.json"

        debug_path = self.debug_visuals_dir / debug_filename
        metadata_path = self.debug_visuals_dir / metadata_filename

        # Write debug image data
        debug_path.write_bytes(debug_image_data)

        # Build metadata
        full_metadata = {
            "debug_number": next_num,
            "action_id": action_id,
            "timestamp": datetime.utcnow().isoformat(),
            "filename": debug_filename,
            "match_info": match_info,
        }

        # Write metadata
        metadata_path.write_text(
            json.dumps(full_metadata, indent=2, ensure_ascii=False), encoding="utf-8"
        )

        # Return relative path
        return f"debug_visuals/{debug_filename}"

    def get_screenshot(self, reference: str) -> bytes | None:
        """Retrieve a screenshot by its reference path.

        Args:
            reference: Relative path to the screenshot (e.g., "screenshots/screenshot-0001.png")
                      or absolute path. The method handles both formats.

        Returns:
            Raw PNG image data as bytes, or None if the screenshot doesn't exist
            or the service is disabled.

        Example:
            >>> image_data = service.get_screenshot("screenshots/screenshot-0001.png")
            >>> if image_data:
            ...     with open("output.png", "wb") as f:
            ...         f.write(image_data)
        """
        if not self.enabled:
            return None

        # Handle both relative and absolute paths
        ref_path = Path(reference)

        if ref_path.is_absolute():
            screenshot_path = ref_path
        else:
            screenshot_path = self.storage_dir / reference

        # Check if file exists
        if not screenshot_path.exists():
            return None

        # Read and return image data
        return screenshot_path.read_bytes()

    def cleanup_old_screenshots(self, keep_last_n: int = 100) -> int:
        """Clean up old screenshots, keeping only the most recent ones.

        This method implements a retention policy to prevent unbounded storage growth.
        It removes the oldest screenshots while preserving the most recent ones.

        Both the screenshot image and its metadata sidecar file are removed together.

        Args:
            keep_last_n: Number of most recent screenshots to retain. Default is 100.
                        Set to 0 to delete all screenshots (use with caution).

        Returns:
            Number of screenshot pairs (image + metadata) deleted.
            Returns 0 if the service is disabled.

        Example:
            >>> # Keep only the 50 most recent screenshots
            >>> deleted_count = service.cleanup_old_screenshots(keep_last_n=50)
            >>> print(f"Cleaned up {deleted_count} old screenshots")
            Cleaned up 23 old screenshots
        """
        if not self.enabled:
            return 0

        deleted_count = 0

        # Clean up regular screenshots
        deleted_count += self._cleanup_directory(
            self.screenshots_dir, pattern="screenshot-*.png", keep_last_n=keep_last_n
        )

        # Clean up debug visuals
        deleted_count += self._cleanup_directory(
            self.debug_visuals_dir, pattern="debug-*.png", keep_last_n=keep_last_n
        )

        return deleted_count

    def _get_next_screenshot_number(self) -> int:
        """Determine the next sequential screenshot number.

        Returns:
            Next available screenshot number (1-based).
        """
        existing_screenshots = sorted(self.screenshots_dir.glob("screenshot-*.png"))

        if not existing_screenshots:
            return 1

        # Extract numbers from existing screenshots
        numbers = []
        for screenshot in existing_screenshots:
            match = re.match(r"screenshot-(\d+)\.png", screenshot.name)
            if match:
                numbers.append(int(match.group(1)))

        if not numbers:
            return 1

        return max(numbers) + 1

    def _get_next_debug_visual_number(self) -> int:
        """Determine the next sequential debug visual number.

        Returns:
            Next available debug visual number (1-based).
        """
        existing_debug_visuals = sorted(self.debug_visuals_dir.glob("debug-*.png"))

        if not existing_debug_visuals:
            return 1

        # Extract numbers from existing debug visuals
        numbers = []
        for debug_visual in existing_debug_visuals:
            match = re.match(r"debug-(\d+)-", debug_visual.name)
            if match:
                numbers.append(int(match.group(1)))

        if not numbers:
            return 1

        return max(numbers) + 1

    def _sanitize_filename(self, filename: str) -> str:
        """Sanitize a string for safe use in filenames.

        Removes or replaces characters that are unsafe in filenames across
        different operating systems.

        Args:
            filename: String to sanitize.

        Returns:
            Sanitized string safe for use in filenames.
        """
        # Replace spaces and special characters with hyphens
        safe_name = re.sub(r"[^\w\-_]", "-", filename)

        # Remove consecutive hyphens
        safe_name = re.sub(r"-+", "-", safe_name)

        # Remove leading/trailing hyphens
        safe_name = safe_name.strip("-")

        # Limit length to reasonable size
        return safe_name[:100]

    def _cleanup_directory(self, directory: Path, pattern: str, keep_last_n: int) -> int:
        """Clean up files in a directory based on retention policy.

        Args:
            directory: Directory to clean up.
            pattern: Glob pattern for files to clean (e.g., "screenshot-*.png").
            keep_last_n: Number of most recent files to keep.

        Returns:
            Number of file pairs (image + metadata) deleted.
        """
        # Find all files matching pattern
        files = sorted(directory.glob(pattern))

        if len(files) <= keep_last_n:
            return 0

        # Determine which files to delete
        files_to_delete = files[: len(files) - keep_last_n]

        deleted_count = 0
        for file_path in files_to_delete:
            # Delete the image file
            file_path.unlink(missing_ok=True)

            # Delete the corresponding metadata file
            metadata_path = file_path.with_suffix(".json")
            metadata_path.unlink(missing_ok=True)

            deleted_count += 1

        return deleted_count
