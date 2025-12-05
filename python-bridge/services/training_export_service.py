"""Training Export Service for automatic dataset generation.

This service manages the collection of ActionExecutionRecord objects during
runner execution and automatically exports them to training datasets for
qontinui-train. It integrates with UnifiedDataCollector to capture execution data.
"""

import logging
from dataclasses import dataclass
from pathlib import Path

from exporters.training_data_exporter import ExportStatistics, TrainingDataExporter
from models.action_execution_record import ActionExecutionRecord

logger = logging.getLogger(__name__)


@dataclass
class ExportConfig:
    """Configuration for training data export."""

    enabled: bool = False
    output_dir: Path | None = None
    dataset_version: str = "1.0.0"
    export_mode: str = "on_completion"  # "on_completion", "incremental", "manual"
    batch_size: int = 50  # For incremental mode
    filter_successful_only: bool = False
    filter_with_matches: bool = False


class TrainingExportService:
    """Service for collecting and exporting training data during runner execution.

    This service acts as a middleware layer between UnifiedDataCollector and
    TrainingDataExporter. It:
    - Buffers ActionExecutionRecord objects during execution
    - Automatically exports based on configured mode
    - Provides manual export capabilities
    - Tracks export statistics

    Usage:
        >>> export_config = ExportConfig(
        ...     enabled=True,
        ...     output_dir=Path("datasets/session_001"),
        ...     export_mode="incremental",
        ...     batch_size=50
        ... )
        >>> service = TrainingExportService(
        ...     config=export_config,
        ...     storage_dir=Path("runs/session_001")
        ... )
        >>> # Records are automatically exported as they're added
        >>> service.add_record(record)
        >>> # Or manually trigger export
        >>> stats = service.export_now()
    """

    def __init__(self, config: ExportConfig, storage_dir: Path):
        """Initialize the TrainingExportService.

        Args:
            config: Export configuration.
            storage_dir: Root directory where screenshots are stored (run_dir).
        """
        self.config = config
        self.storage_dir = storage_dir
        self.records_buffer: list[ActionExecutionRecord] = []
        self.total_records_added = 0
        self.total_records_exported = 0

        # Initialize exporter if enabled
        self.exporter: TrainingDataExporter | None = None
        if config.enabled and config.output_dir:
            self.exporter = TrainingDataExporter(
                output_dir=config.output_dir, dataset_version=config.dataset_version
            )
            logger.info(f"TrainingExportService initialized: {config.output_dir}")
        else:
            logger.info("TrainingExportService disabled")

    def add_record(self, record: ActionExecutionRecord) -> None:
        """Add an ActionExecutionRecord to the buffer.

        Depending on the export mode, this may trigger an automatic export.

        Args:
            record: The execution record to add.
        """
        if not self.config.enabled or not self.exporter:
            return

        self.records_buffer.append(record)
        self.total_records_added += 1

        # Handle incremental export
        if self.config.export_mode == "incremental":
            if len(self.records_buffer) >= self.config.batch_size:
                self._export_buffered_records()
                logger.info(f"Incremental export triggered: {len(self.records_buffer)} records")

    def _export_buffered_records(self) -> ExportStatistics | None:
        """Export buffered records and clear the buffer.

        Returns:
            ExportStatistics if export succeeded, None otherwise.
        """
        if not self.exporter or not self.records_buffer:
            return None

        # Apply filters
        records_to_export = self._filter_records(self.records_buffer)

        if not records_to_export:
            logger.warning("No records to export after filtering")
            self.records_buffer.clear()
            return None

        # Export
        try:
            stats = self.exporter.export_records(
                records=records_to_export, storage_dir=self.storage_dir
            )
            self.total_records_exported += stats.records_with_screenshots
            logger.info(
                f"Exported {stats.records_with_screenshots} records "
                f"({stats.unique_images} unique images, {stats.total_annotations} annotations)"
            )

            # Clear buffer after successful export
            self.records_buffer.clear()

            return stats
        except Exception as e:
            logger.error(f"Failed to export records: {e}", exc_info=True)
            return None

    def _filter_records(self, records: list[ActionExecutionRecord]) -> list[ActionExecutionRecord]:
        """Apply configured filters to records.

        Args:
            records: List of records to filter.

        Returns:
            Filtered list of records.
        """
        filtered = records

        # Filter: successful only
        if self.config.filter_successful_only:
            filtered = [r for r in filtered if r.success]

        # Filter: only records with match results
        if self.config.filter_with_matches:
            filtered = [r for r in filtered if r.match_summary is not None]

        return filtered

    def export_on_completion(self) -> ExportStatistics | None:
        """Export all buffered records on completion.

        This should be called when the automation run completes.

        Returns:
            ExportStatistics if export succeeded, None otherwise.
        """
        if not self.config.enabled:
            return None

        logger.info(f"Exporting on completion: {len(self.records_buffer)} records buffered")
        return self._export_buffered_records()

    def export_now(self) -> ExportStatistics | None:
        """Manually trigger export of buffered records.

        Returns:
            ExportStatistics if export succeeded, None otherwise.
        """
        logger.info(f"Manual export triggered: {len(self.records_buffer)} records")
        return self._export_buffered_records()

    def export_to_coco(self, output_file: Path) -> bool:
        """Export the dataset to COCO format.

        Args:
            output_file: Path to save the COCO JSON file.

        Returns:
            True if export succeeded, False otherwise.
        """
        if not self.exporter:
            logger.error("Exporter not initialized")
            return False

        try:
            self.exporter.export_to_coco(output_file)
            logger.info(f"COCO format exported to: {output_file}")
            return True
        except Exception as e:
            logger.error(f"Failed to export COCO format: {e}", exc_info=True)
            return False

    def get_summary(self) -> dict:
        """Get a summary of the export service state.

        Returns:
            Dictionary with service statistics.
        """
        summary = {
            "enabled": self.config.enabled,
            "total_records_added": self.total_records_added,
            "total_records_exported": self.total_records_exported,
            "records_in_buffer": len(self.records_buffer),
            "export_mode": self.config.export_mode,
        }

        # Add dataset summary if exporter is available
        if self.exporter:
            dataset_summary = self.exporter.get_summary()
            summary["dataset"] = dataset_summary

        return summary
