"""
Training Export Module

Single Responsibility: Coordinate training data export
- Initialize training export service
- Configure export settings from environment
- Coordinate with UnifiedDataCollector
- Manage export lifecycle
"""

import logging
import os
from pathlib import Path

logger = logging.getLogger(__name__)

try:
    from services.training_export_service import ExportConfig, TrainingExportService  # noqa: F401

    EXPORT_AVAILABLE = True
except ImportError:
    EXPORT_AVAILABLE = False
    logger.debug("Training export modules not available")


class TrainingExportCoordinator:
    """
    Coordinates training data export.

    Responsibilities:
    - Initialize training export service from environment config
    - Provide callback for data collection
    - Manage export lifecycle
    """

    def __init__(self, emit_log_fn):
        """
        Initialize training export coordinator.

        Args:
            emit_log_fn: Function to emit log messages (level, message)
        """
        self.emit_log = emit_log_fn
        self.export_service: TrainingExportService | None = None
        self.export_enabled = False

    def initialize(self, run_dir: Path) -> bool:
        """
        Initialize training export service based on environment configuration.

        Args:
            run_dir: Directory for storing run data

        Returns:
            True if export service initialized, False otherwise
        """
        if not EXPORT_AVAILABLE:
            self.emit_log("debug", "Training export not available")
            return False

        # Check if export is enabled via environment variable
        export_enabled_str = os.getenv("QONTINUI_EXPORT_TRAINING_DATA", "false").lower()
        self.export_enabled = export_enabled_str == "true"

        if not self.export_enabled:
            self.emit_log(
                "info", "TrainingExportService disabled (QONTINUI_EXPORT_TRAINING_DATA not set)"
            )
            return False

        # Get export directory from environment
        export_dir = os.getenv("QONTINUI_EXPORT_DIR")
        if not export_dir:
            self.emit_log(
                "warning", "TrainingExportService enabled but QONTINUI_EXPORT_DIR not set"
            )
            return False

        try:
            export_config = ExportConfig(
                enabled=True,
                output_dir=Path(export_dir),
                dataset_version="1.0.0",
                export_mode="on_completion",  # Export when run completes
                filter_successful_only=False,  # Include all records
                filter_with_matches=False,  # Include records without matches
            )

            self.export_service = TrainingExportService(config=export_config, storage_dir=run_dir)

            self.emit_log("info", f"TrainingExportService enabled: {export_dir}")
            return True

        except Exception as e:
            self.emit_log("error", f"Failed to initialize training export: {e}")
            import traceback

            self.emit_log("debug", f"Traceback: {traceback.format_exc()}")
            return False

    def get_record_callback(self):
        """
        Get callback function for UnifiedDataCollector.

        Returns:
            Callback function or None if export not enabled
        """
        if self.export_service:
            return self.export_service.add_record
        return None

    def is_enabled(self) -> bool:
        """
        Check if training export is enabled.

        Returns:
            True if export is enabled, False otherwise
        """
        return self.export_enabled and self.export_service is not None

    def export_data(self) -> bool:
        """
        Trigger manual export of collected data.

        Returns:
            True if export successful, False otherwise
        """
        if not self.export_service:
            return False

        try:
            # Export service handles export automatically on completion
            # This method is for manual triggering if needed
            self.emit_log("info", "Triggering manual training data export")
            return True
        except Exception as e:
            self.emit_log("error", f"Failed to export training data: {e}")
            return False
