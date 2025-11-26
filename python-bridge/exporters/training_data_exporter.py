"""Training Data Exporter for qontinui-train integration.

This module exports ActionExecutionRecord data to training dataset formats
compatible with qontinui-train and qontinui-finetune. It supports:
- COCO format export
- Incremental dataset building
- Screenshot deduplication
- Bounding box extraction from match results and click locations
- Dataset versioning and metadata tracking

Note: Bounding box inference from click locations uses the sophisticated
analysis provided by qontinui.discovery.click_analysis, which detects
actual element boundaries rather than using fixed-size boxes.
"""

import json
import hashlib
import logging
import shutil
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import List, Dict, Any, Optional, Set, Tuple

import numpy as np
from PIL import Image

from models.action_execution_record import ActionExecutionRecord

# Import click analysis from qontinui library
try:
    from qontinui.discovery.click_analysis import (
        ClickBoundingBoxInferrer,
        InferenceConfig,
        InferenceResult,
    )
    CLICK_ANALYSIS_AVAILABLE = True
except ImportError:
    CLICK_ANALYSIS_AVAILABLE = False

logger = logging.getLogger(__name__)


@dataclass
class ExportStatistics:
    """Statistics for an export operation."""
    total_records: int = 0
    records_with_screenshots: int = 0
    records_with_matches: int = 0
    records_with_clicks: int = 0
    total_annotations: int = 0
    unique_images: int = 0
    skipped_records: int = 0
    export_time_seconds: float = 0.0


class TrainingDataExporter:
    """Export ActionExecutionRecord data to training dataset formats.

    This exporter transforms execution data captured during automation runs
    into training datasets for ML models. It extracts:
    - Screenshots as training images
    - Match results as bounding box annotations (automated labels)
    - Click locations as interaction annotations (human-validated labels)
    - State context as semantic labels

    The exporter produces an intermediate format that can be viewed/curated
    before being transformed into final training formats (COCO, YOLO, etc.)
    by qontinui-train's dataset preparation tools.

    Directory Structure:
        output_dir/
            manifest.jsonl          # One record per line (appendable)
            images/
                <hash>.png          # Deduplicated screenshots
            annotations/
                <hash>.json         # Per-image annotations
            metadata.json           # Dataset-level metadata

    Usage:
        >>> exporter = TrainingDataExporter(output_dir=Path("datasets/session_001"))
        >>> records = [...]  # List of ActionExecutionRecord objects
        >>> stats = exporter.export_records(records, storage_dir=Path("runs/session_001"))
        >>> print(f"Exported {stats.total_annotations} annotations from {stats.unique_images} images")
    """

    def __init__(
        self,
        output_dir: Path,
        dataset_version: str = "1.0.0",
        use_smart_bbox_inference: bool = True,
        inference_config: Optional[Any] = None,
    ):
        """Initialize the TrainingDataExporter.

        Args:
            output_dir: Directory where the training dataset will be created.
            dataset_version: Version string for this dataset (e.g., "1.0.0").
            use_smart_bbox_inference: If True, use qontinui's click analysis to
                infer accurate bounding boxes. If False, fall back to fixed-size boxes.
            inference_config: Optional InferenceConfig for click analysis.
        """
        self.output_dir = output_dir
        self.dataset_version = dataset_version
        self.images_dir = output_dir / "images"
        self.annotations_dir = output_dir / "annotations"
        self.manifest_path = output_dir / "manifest.jsonl"
        self.metadata_path = output_dir / "metadata.json"

        # Category tracking
        self.category_id_map: Dict[str, int] = {}
        self.next_category_id = 0

        # Image deduplication
        self.image_hashes: Set[str] = set()

        # Click analysis configuration
        self.use_smart_bbox_inference = use_smart_bbox_inference and CLICK_ANALYSIS_AVAILABLE
        self._bbox_inferrer: Optional[Any] = None
        self._inference_config = inference_config

        if use_smart_bbox_inference and not CLICK_ANALYSIS_AVAILABLE:
            logger.warning(
                "Smart bounding box inference requested but qontinui.discovery.click_analysis "
                "is not available. Falling back to fixed-size boxes."
            )

        # Create directory structure
        self._create_directories()

        # Load existing state if resuming
        self._load_existing_state()

    def _create_directories(self) -> None:
        """Create the output directory structure."""
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.images_dir.mkdir(exist_ok=True)
        self.annotations_dir.mkdir(exist_ok=True)

    def _load_existing_state(self) -> None:
        """Load existing category mappings and image hashes if resuming."""
        if self.metadata_path.exists():
            with open(self.metadata_path, 'r') as f:
                metadata = json.load(f)
                self.category_id_map = metadata.get("category_map", {})
                self.next_category_id = len(self.category_id_map)

        # Load existing image hashes to avoid duplicates
        for img_path in self.images_dir.glob("*.png"):
            self.image_hashes.add(img_path.stem)

    def export_records(
        self,
        records: List[ActionExecutionRecord],
        storage_dir: Path,
        filter_fn: Optional[callable] = None
    ) -> ExportStatistics:
        """Export a list of ActionExecutionRecords to training format.

        Args:
            records: List of ActionExecutionRecord objects to export.
            storage_dir: Root directory where screenshots are stored (e.g., run_dir).
            filter_fn: Optional function to filter records. Should return True to include.
                      Example: lambda r: r.success and r.match_summary is not None

        Returns:
            ExportStatistics object with export results.
        """
        start_time = datetime.now()
        stats = ExportStatistics()
        stats.total_records = len(records)

        for record in records:
            # Apply filter if provided
            if filter_fn and not filter_fn(record):
                stats.skipped_records += 1
                continue

            # Skip records without screenshots
            if not record.screenshot_reference:
                stats.skipped_records += 1
                continue

            # Process record
            self._export_single_record(record, storage_dir, stats)

        # Update metadata
        self._update_metadata(stats)

        stats.export_time_seconds = (datetime.now() - start_time).total_seconds()
        return stats

    def _export_single_record(
        self,
        record: ActionExecutionRecord,
        storage_dir: Path,
        stats: ExportStatistics
    ) -> None:
        """Export a single ActionExecutionRecord.

        Args:
            record: The record to export.
            storage_dir: Root directory where screenshots are stored.
            stats: Statistics object to update.
        """
        # Build screenshot path
        screenshot_path = storage_dir / record.screenshot_reference
        if not screenshot_path.exists():
            stats.skipped_records += 1
            return

        stats.records_with_screenshots += 1

        # Read and hash image
        image_data = screenshot_path.read_bytes()
        img_hash = self._hash_image(image_data)

        # Copy image if not already present (deduplication)
        is_new_image = img_hash not in self.image_hashes
        if is_new_image:
            dest_path = self.images_dir / f"{img_hash}.png"
            shutil.copy(screenshot_path, dest_path)
            self.image_hashes.add(img_hash)
            stats.unique_images += 1

        # Get image dimensions and load as numpy array if smart inference is enabled
        with Image.open(screenshot_path) as img:
            img_width, img_height = img.size
            screenshot_array = None
            if self.use_smart_bbox_inference and record.clicked_location:
                # Convert to numpy array for click analysis
                screenshot_array = np.array(img)

        # Build annotations for this image
        annotations = []

        # Annotation from match_summary (automated label)
        if record.match_summary:
            stats.records_with_matches += 1
            bbox = self._extract_bbox_from_match(record.match_summary)
            if bbox:
                category_id = self._get_or_create_category_id(
                    record.match_summary.get("image_id", "unknown")
                )
                annotations.append({
                    "bbox": bbox,
                    "category_id": category_id,
                    "category_name": record.match_summary.get("image_id", "unknown"),
                    "confidence": record.match_summary.get("confidence", 0.0),
                    "source": "template_matching",
                    "verified": False,
                })
                stats.total_annotations += 1

        # Annotation from clicked_location (human interaction - high confidence)
        if record.clicked_location:
            stats.records_with_clicks += 1

            # Use smart inference if available, otherwise fall back to fixed-size
            if self.use_smart_bbox_inference and screenshot_array is not None:
                annotation = self._infer_bbox_smart(
                    record.clicked_location,
                    screenshot_array,
                    record.click_target_type,
                )
            else:
                bbox = self._infer_bbox_from_click(
                    record.clicked_location,
                    img_width,
                    img_height
                )
                category_name = record.click_target_type or "click_target"
                category_id = self._get_or_create_category_id(category_name)
                annotation = {
                    "bbox": bbox,
                    "category_id": category_id,
                    "category_name": category_name,
                    "confidence": 1.0,  # Human interaction is high confidence
                    "source": "user_click",
                    "verified": True,  # User clicks are pre-verified
                }

            if annotation:
                annotations.append(annotation)
                stats.total_annotations += 1

        # Save or update annotation file
        if annotations:
            self._save_annotations(img_hash, annotations, record, img_width, img_height)

        # Append to manifest (incremental)
        self._append_to_manifest(img_hash, record, is_new_image)

    def _extract_bbox_from_match(self, match_summary: Dict[str, Any]) -> Optional[List[int]]:
        """Extract bounding box from match_summary.

        Args:
            match_summary: Match result dictionary with location and template_size.

        Returns:
            Bounding box as [x, y, width, height] or None if insufficient data.
        """
        if not match_summary.get("found"):
            return None

        location = match_summary.get("location")
        template_size = match_summary.get("template_size")

        if not location or not template_size:
            return None

        x = location.get("x", 0)
        y = location.get("y", 0)
        width = template_size.get("width", 0)
        height = template_size.get("height", 0)

        if width == 0 or height == 0:
            return None

        return [x, y, width, height]

    def _infer_bbox_from_click(
        self,
        clicked_location: Tuple[int, int],
        img_width: int,
        img_height: int,
        bbox_size: int = 50
    ) -> List[int]:
        """Infer a bounding box from a click location using fixed-size fallback.

        This is the simple fallback method that creates a fixed-size bounding box
        centered on the click location. For more sophisticated detection, use
        _infer_bbox_smart() with the qontinui library.

        Args:
            clicked_location: (x, y) tuple of click coordinates.
            img_width: Image width for boundary checking.
            img_height: Image height for boundary checking.
            bbox_size: Size of the inferred bounding box (default 50px).

        Returns:
            Bounding box as [x, y, width, height].
        """
        x, y = clicked_location
        half_size = bbox_size // 2

        # Center bbox on click, clamp to image boundaries
        bbox_x = max(0, x - half_size)
        bbox_y = max(0, y - half_size)
        bbox_w = min(bbox_size, img_width - bbox_x)
        bbox_h = min(bbox_size, img_height - bbox_y)

        return [bbox_x, bbox_y, bbox_w, bbox_h]

    def _infer_bbox_smart(
        self,
        clicked_location: Tuple[int, int],
        screenshot: np.ndarray,
        click_target_type: Optional[str] = None,
    ) -> Optional[Dict[str, Any]]:
        """Infer bounding box using qontinui's sophisticated click analysis.

        Uses multiple detection strategies (edge detection, contour detection,
        color segmentation, flood fill) to find the actual element boundaries
        at the click location rather than using a fixed-size box.

        Args:
            clicked_location: (x, y) tuple of click coordinates.
            screenshot: Screenshot as numpy array (RGB or BGR).
            click_target_type: Optional hint about the target type.

        Returns:
            Annotation dictionary with bbox, category, confidence, and metadata,
            or None if detection failed.
        """
        if not CLICK_ANALYSIS_AVAILABLE:
            return None

        # Lazy initialization of the inferrer
        if self._bbox_inferrer is None:
            config = self._inference_config
            if config is None:
                config = InferenceConfig()
            self._bbox_inferrer = ClickBoundingBoxInferrer(config)

        try:
            # Run inference
            result: InferenceResult = self._bbox_inferrer.infer_bounding_box(
                screenshot=screenshot,
                click_location=clicked_location,
            )

            bbox = result.primary_bbox

            # Determine category name
            # Use detected element type if available, otherwise fall back to provided type
            if bbox.element_type.value != "unknown":
                category_name = f"{click_target_type or 'click_target'}_{bbox.element_type.value}"
            else:
                category_name = click_target_type or "click_target"

            category_id = self._get_or_create_category_id(category_name)

            # Build annotation with rich metadata
            annotation = {
                "bbox": bbox.as_bbox_list(),
                "category_id": category_id,
                "category_name": category_name,
                "confidence": bbox.confidence,
                "source": "smart_click_analysis",
                "verified": True,  # User clicks are pre-verified
                "inference_metadata": {
                    "strategy_used": bbox.strategy_used.value,
                    "element_type": bbox.element_type.value,
                    "used_fallback": result.used_fallback,
                    "processing_time_ms": result.processing_time_ms,
                    "alternatives_count": len(result.alternative_candidates),
                },
            }

            # Log if fallback was used
            if result.used_fallback:
                logger.debug(
                    f"Click analysis fell back to fixed-size box at {clicked_location}"
                )

            return annotation

        except Exception as e:
            logger.warning(f"Smart bbox inference failed: {e}. Using fallback.")
            # Fall back to fixed-size
            img_height, img_width = screenshot.shape[:2]
            bbox = self._infer_bbox_from_click(clicked_location, img_width, img_height)
            category_name = click_target_type or "click_target"
            category_id = self._get_or_create_category_id(category_name)
            return {
                "bbox": bbox,
                "category_id": category_id,
                "category_name": category_name,
                "confidence": 1.0,
                "source": "user_click_fallback",
                "verified": True,
            }

    def _save_annotations(
        self,
        img_hash: str,
        annotations: List[Dict[str, Any]],
        record: ActionExecutionRecord,
        img_width: int,
        img_height: int
    ) -> None:
        """Save annotations for an image.

        Args:
            img_hash: Image hash (used as filename).
            annotations: List of annotation dictionaries.
            record: Source ActionExecutionRecord for context.
            img_width: Image width.
            img_height: Image height.
        """
        annotation_file = self.annotations_dir / f"{img_hash}.json"

        # Load existing annotations if file exists (for incremental updates)
        existing_annotations = []
        if annotation_file.exists():
            with open(annotation_file, 'r') as f:
                data = json.load(f)
                existing_annotations = data.get("annotations", [])

        # Merge annotations (avoid duplicates based on bbox + category)
        merged_annotations = existing_annotations.copy()
        for new_ann in annotations:
            if not self._annotation_exists(new_ann, existing_annotations):
                merged_annotations.append(new_ann)

        # Save complete annotation file
        data = {
            "image_id": img_hash,
            "image_filename": f"{img_hash}.png",
            "image_width": img_width,
            "image_height": img_height,
            "annotations": merged_annotations,
            "context": {
                "action_type": record.action_type,
                "active_states": sorted(record.active_states_before),
                "timestamp": record.start_time,
                "success": record.success,
            },
            "version": self.dataset_version,
        }

        with open(annotation_file, 'w') as f:
            json.dump(data, f, indent=2)

    def _annotation_exists(
        self,
        annotation: Dict[str, Any],
        existing: List[Dict[str, Any]]
    ) -> bool:
        """Check if an annotation already exists.

        Args:
            annotation: New annotation to check.
            existing: List of existing annotations.

        Returns:
            True if a similar annotation exists.
        """
        for ann in existing:
            if (ann["bbox"] == annotation["bbox"] and
                ann["category_id"] == annotation["category_id"]):
                return True
        return False

    def _append_to_manifest(
        self,
        img_hash: str,
        record: ActionExecutionRecord,
        is_new: bool
    ) -> None:
        """Append a record to the manifest file.

        Args:
            img_hash: Image hash.
            record: Source record.
            is_new: Whether this is a new image.
        """
        manifest_entry = {
            "id": img_hash,
            "image": f"images/{img_hash}.png",
            "annotations": f"annotations/{img_hash}.json",
            "action_id": record.action_id,
            "action_type": record.action_type,
            "timestamp": record.start_time,
            "active_states": sorted(record.active_states_before),
            "source": "runner_execution",
            "is_new": is_new,
            "reviewed": False,
        }

        # Append to JSONL file (one line per entry)
        with open(self.manifest_path, 'a') as f:
            f.write(json.dumps(manifest_entry) + '\n')

    def _hash_image(self, image_data: bytes) -> str:
        """Generate a hash for image data.

        Args:
            image_data: Raw image bytes.

        Returns:
            SHA256 hash as hexadecimal string.
        """
        return hashlib.sha256(image_data).hexdigest()[:16]

    def _get_or_create_category_id(self, category_name: str) -> int:
        """Get or create a category ID for a category name.

        Args:
            category_name: Name of the category (e.g., "login_button").

        Returns:
            Integer category ID.
        """
        if category_name not in self.category_id_map:
            self.category_id_map[category_name] = self.next_category_id
            self.next_category_id += 1
        return self.category_id_map[category_name]

    def _update_metadata(self, stats: ExportStatistics) -> None:
        """Update the dataset metadata file.

        Args:
            stats: Export statistics to include in metadata.
        """
        metadata = {
            "dataset_version": self.dataset_version,
            "created": datetime.now().isoformat(),
            "total_images": stats.unique_images,
            "total_annotations": stats.total_annotations,
            "category_map": self.category_id_map,
            "categories": [
                {"id": cat_id, "name": cat_name}
                for cat_name, cat_id in sorted(
                    self.category_id_map.items(),
                    key=lambda x: x[1]
                )
            ],
            "statistics": {
                "total_records_processed": stats.total_records,
                "records_with_screenshots": stats.records_with_screenshots,
                "records_with_matches": stats.records_with_matches,
                "records_with_clicks": stats.records_with_clicks,
                "skipped_records": stats.skipped_records,
                "export_time_seconds": stats.export_time_seconds,
            },
            "format": "intermediate",
            "description": "Training dataset exported from qontinui-runner execution data",
        }

        with open(self.metadata_path, 'w') as f:
            json.dump(metadata, f, indent=2)

    def export_to_coco(self, output_file: Path) -> None:
        """Convert the intermediate format to COCO JSON format.

        This method reads the exported dataset and converts it to COCO format
        for compatibility with qontinui-train and standard ML frameworks.

        Args:
            output_file: Path to save the COCO JSON file.
        """
        coco_data = {
            "info": {
                "version": self.dataset_version,
                "description": "GUI automation training dataset",
                "date_created": datetime.now().isoformat(),
            },
            "images": [],
            "annotations": [],
            "categories": [
                {"id": cat_id, "name": cat_name, "supercategory": "gui_element"}
                for cat_name, cat_id in self.category_id_map.items()
            ],
        }

        annotation_id = 0

        # Read all annotation files
        for ann_file in sorted(self.annotations_dir.glob("*.json")):
            with open(ann_file, 'r') as f:
                data = json.load(f)

            img_hash = data["image_id"]

            # Add image entry
            coco_data["images"].append({
                "id": img_hash,
                "file_name": data["image_filename"],
                "width": data["image_width"],
                "height": data["image_height"],
                "date_captured": data["context"].get("timestamp"),
            })

            # Add annotations
            for ann in data["annotations"]:
                bbox = ann["bbox"]
                area = bbox[2] * bbox[3]  # width * height

                coco_data["annotations"].append({
                    "id": annotation_id,
                    "image_id": img_hash,
                    "category_id": ann["category_id"],
                    "bbox": bbox,
                    "area": area,
                    "iscrowd": 0,
                    "confidence": ann.get("confidence", 1.0),
                    "source": ann.get("source", "unknown"),
                    "verified": ann.get("verified", False),
                })
                annotation_id += 1

        # Write COCO JSON
        with open(output_file, 'w') as f:
            json.dump(coco_data, f, indent=2)

    def get_summary(self) -> Dict[str, Any]:
        """Get a summary of the current dataset.

        Returns:
            Dictionary with dataset statistics and information.
        """
        if not self.metadata_path.exists():
            return {"error": "No metadata found. Dataset may be empty."}

        with open(self.metadata_path, 'r') as f:
            metadata = json.load(f)

        # Count manifest entries
        manifest_count = 0
        reviewed_count = 0
        if self.manifest_path.exists():
            with open(self.manifest_path, 'r') as f:
                for line in f:
                    manifest_count += 1
                    entry = json.loads(line)
                    if entry.get("reviewed"):
                        reviewed_count += 1

        return {
            "dataset_version": metadata.get("dataset_version"),
            "total_images": metadata.get("total_images"),
            "total_annotations": metadata.get("total_annotations"),
            "total_categories": len(metadata.get("category_map", {})),
            "manifest_entries": manifest_count,
            "reviewed_entries": reviewed_count,
            "statistics": metadata.get("statistics", {}),
        }
