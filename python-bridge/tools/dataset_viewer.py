#!/usr/bin/env python3
"""Dataset Viewer and Validation Tool.

A command-line tool for viewing, validating, and curating training datasets
exported from qontinui-runner.

Usage:
    python dataset_viewer.py <dataset_dir> [--mode MODE]

Modes:
    summary   - Show dataset statistics (default)
    browse    - Browse images with annotations
    validate  - Mark images as good/bad
    export    - Export filtered dataset

Examples:
    # View summary
    python dataset_viewer.py ~/datasets/session_001

    # Browse images
    python dataset_viewer.py ~/datasets/session_001 --mode browse

    # Validate annotations
    python dataset_viewer.py ~/datasets/session_001 --mode validate
"""

import argparse
import json
import sys
from pathlib import Path
from typing import List, Dict, Any, Optional
import logging

# Try to import PIL for image viewing
try:
    from PIL import Image, ImageDraw, ImageFont

    PIL_AVAILABLE = True
except ImportError:
    PIL_AVAILABLE = False
    print("Warning: PIL not available. Image viewing disabled.")
    print("Install with: pip install Pillow")

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class DatasetViewer:
    """View and validate training datasets."""

    def __init__(self, dataset_dir: Path):
        """Initialize the viewer.

        Args:
            dataset_dir: Path to the dataset directory.
        """
        self.dataset_dir = dataset_dir
        self.manifest_path = dataset_dir / "manifest.jsonl"
        self.metadata_path = dataset_dir / "metadata.json"
        self.images_dir = dataset_dir / "images"
        self.annotations_dir = dataset_dir / "annotations"

        # Validation results
        self.reviews_path = dataset_dir / "reviews.jsonl"
        self.reviews: Dict[str, Dict[str, Any]] = {}

        self._load_reviews()

    def _load_reviews(self) -> None:
        """Load existing review data."""
        if not self.reviews_path.exists():
            return

        with open(self.reviews_path, "r") as f:
            for line in f:
                review = json.loads(line)
                self.reviews[review["image_id"]] = review

    def _save_review(self, image_id: str, status: str, notes: str = "") -> None:
        """Save a review for an image.

        Args:
            image_id: Image ID.
            status: Review status (good, bad, skip).
            notes: Optional review notes.
        """
        review = {
            "image_id": image_id,
            "status": status,
            "notes": notes,
        }
        self.reviews[image_id] = review

        # Append to reviews file
        with open(self.reviews_path, "a") as f:
            f.write(json.dumps(review) + "\n")

    def show_summary(self) -> None:
        """Show dataset summary statistics."""
        if not self.metadata_path.exists():
            print("Error: No metadata.json found")
            return

        with open(self.metadata_path, "r") as f:
            metadata = json.load(f)

        # Count manifest entries
        manifest_count = 0
        reviewed_count = 0
        if self.manifest_path.exists():
            with open(self.manifest_path, "r") as f:
                for line in f:
                    manifest_count += 1
                    entry = json.loads(line)
                    if entry["id"] in self.reviews:
                        reviewed_count += 1

        # Display summary
        print("\n" + "=" * 60)
        print(f"Dataset: {self.dataset_dir.name}")
        print("=" * 60)
        print(f"Version: {metadata.get('dataset_version')}")
        print(f"Created: {metadata.get('created')}")
        print()
        print(f"Total Images: {metadata.get('total_images', 0)}")
        print(f"Total Annotations: {metadata.get('total_annotations', 0)}")
        print(f"Total Categories: {len(metadata.get('categories', []))}")
        print(f"Manifest Entries: {manifest_count}")
        print(f"Reviewed: {reviewed_count} / {manifest_count}")
        print()

        # Category breakdown
        print("Categories:")
        for cat in metadata.get("categories", []):
            print(f"  [{cat['id']}] {cat['name']}")
        print()

        # Statistics
        stats = metadata.get("statistics", {})
        if stats:
            print("Processing Statistics:")
            print(f"  Total Records: {stats.get('total_records_processed', 0)}")
            print(f"  With Screenshots: {stats.get('records_with_screenshots', 0)}")
            print(f"  With Matches: {stats.get('records_with_matches', 0)}")
            print(f"  With Clicks: {stats.get('records_with_clicks', 0)}")
            print(f"  Skipped: {stats.get('skipped_records', 0)}")
            print(f"  Export Time: {stats.get('export_time_seconds', 0):.2f}s")
        print("=" * 60)
        print()

    def browse_images(self, limit: int = 10) -> None:
        """Browse images with annotations.

        Args:
            limit: Maximum number of images to show.
        """
        if not self.manifest_path.exists():
            print("Error: No manifest.jsonl found")
            return

        print(f"\nBrowsing dataset: {self.dataset_dir.name}")
        print("=" * 60)

        count = 0
        with open(self.manifest_path, "r") as f:
            for line in f:
                if count >= limit:
                    break

                entry = json.loads(line)
                image_id = entry["id"]
                image_path = self.dataset_dir / entry["image"]
                ann_path = self.dataset_dir / entry["annotations"]

                # Load annotations
                if not ann_path.exists():
                    continue

                with open(ann_path, "r") as af:
                    ann_data = json.load(af)

                # Display info
                print(f"\nImage #{count + 1}: {entry['image']}")
                print(f"  Action: {entry['action_type']}")
                print(f"  States: {', '.join(entry['active_states'])}")
                print(f"  Timestamp: {entry['timestamp']}")
                print(f"  Annotations: {len(ann_data['annotations'])}")

                for i, ann in enumerate(ann_data["annotations"]):
                    bbox = ann["bbox"]
                    print(
                        f"    [{i+1}] {ann['category_name']} @ [{bbox[0]}, {bbox[1]}, {bbox[2]}, {bbox[3]}]"
                    )
                    print(
                        f"        Confidence: {ann['confidence']:.2f}, Source: {ann['source']}, Verified: {ann['verified']}"
                    )

                # Review status
                if image_id in self.reviews:
                    review = self.reviews[image_id]
                    print(f"  Review: {review['status'].upper()}")
                    if review["notes"]:
                        print(f"  Notes: {review['notes']}")

                count += 1

        print(f"\nShowing {count} of {self._count_manifest_entries()} entries")
        print("=" * 60)

    def validate_interactive(self) -> None:
        """Interactive validation mode."""
        if not PIL_AVAILABLE:
            print("Error: PIL required for interactive validation")
            print("Install with: pip install Pillow")
            return

        if not self.manifest_path.exists():
            print("Error: No manifest.jsonl found")
            return

        print("\nInteractive Validation Mode")
        print("=" * 60)
        print("Commands:")
        print("  g - Mark as GOOD")
        print("  b - Mark as BAD")
        print("  s - SKIP (review later)")
        print("  q - QUIT")
        print("=" * 60)
        print()

        entries = self._load_manifest()
        for i, entry in enumerate(entries):
            image_id = entry["id"]

            # Skip already reviewed
            if image_id in self.reviews:
                continue

            image_path = self.dataset_dir / entry["image"]
            ann_path = self.dataset_dir / entry["annotations"]

            # Load annotations
            with open(ann_path, "r") as f:
                ann_data = json.load(f)

            # Show image info
            print(f"\n[{i+1}/{len(entries)}] {entry['image']}")
            print(f"Action: {entry['action_type']}, States: {', '.join(entry['active_states'])}")
            print(f"Annotations: {len(ann_data['annotations'])}")

            # Draw image with bounding boxes
            try:
                img = Image.open(image_path)
                draw = ImageDraw.Draw(img)

                for ann in ann_data["annotations"]:
                    bbox = ann["bbox"]
                    x, y, w, h = bbox
                    color = "green" if ann["verified"] else "yellow"
                    draw.rectangle([x, y, x + w, y + h], outline=color, width=3)

                    # Draw label
                    label = f"{ann['category_name']} ({ann['confidence']:.2f})"
                    draw.text((x, y - 15), label, fill=color)

                # Show image
                img.show()
            except Exception as e:
                print(f"Error displaying image: {e}")

            # Get user input
            while True:
                cmd = input("Review (g/b/s/q): ").strip().lower()
                if cmd in ["g", "b", "s", "q"]:
                    break
                print("Invalid command")

            if cmd == "q":
                print("Exiting validation mode")
                break
            elif cmd == "g":
                self._save_review(image_id, "good")
                print("✓ Marked as GOOD")
            elif cmd == "b":
                notes = input("Notes (optional): ").strip()
                self._save_review(image_id, "bad", notes)
                print("✗ Marked as BAD")
            elif cmd == "s":
                print("○ Skipped")

        print("\nValidation session complete")
        self._show_review_summary()

    def _load_manifest(self) -> List[Dict[str, Any]]:
        """Load all manifest entries.

        Returns:
            List of manifest entries.
        """
        entries = []
        with open(self.manifest_path, "r") as f:
            for line in f:
                entries.append(json.loads(line))
        return entries

    def _count_manifest_entries(self) -> int:
        """Count manifest entries.

        Returns:
            Number of entries.
        """
        count = 0
        with open(self.manifest_path, "r") as f:
            for _ in f:
                count += 1
        return count

    def _show_review_summary(self) -> None:
        """Show review statistics."""
        good_count = sum(1 for r in self.reviews.values() if r["status"] == "good")
        bad_count = sum(1 for r in self.reviews.values() if r["status"] == "bad")
        skip_count = sum(1 for r in self.reviews.values() if r["status"] == "skip")

        total = len(self.reviews)
        print("\nReview Summary:")
        print(f"  Total Reviewed: {total}")
        print(f"  Good: {good_count} ({good_count/total*100:.1f}%)")
        print(f"  Bad: {bad_count} ({bad_count/total*100:.1f}%)")
        print(f"  Skipped: {skip_count} ({skip_count/total*100:.1f}%)")

    def export_filtered(self, output_dir: Path, status_filter: str = "good") -> None:
        """Export a filtered dataset.

        Args:
            output_dir: Output directory for filtered dataset.
            status_filter: Filter by review status (good, bad, skip).
        """
        if not self.reviews:
            print("Error: No reviews found. Run validation first.")
            return

        output_dir.mkdir(parents=True, exist_ok=True)
        output_images_dir = output_dir / "images"
        output_annotations_dir = output_dir / "annotations"
        output_images_dir.mkdir(exist_ok=True)
        output_annotations_dir.mkdir(exist_ok=True)

        # Filter manifest entries
        filtered_manifest = output_dir / "manifest.jsonl"
        filtered_count = 0

        entries = self._load_manifest()
        with open(filtered_manifest, "w") as out_f:
            for entry in entries:
                image_id = entry["id"]
                if image_id not in self.reviews:
                    continue

                review = self.reviews[image_id]
                if review["status"] != status_filter:
                    continue

                # Copy image and annotation
                src_image = self.dataset_dir / entry["image"]
                src_ann = self.dataset_dir / entry["annotations"]
                dst_image = output_images_dir / src_image.name
                dst_ann = output_annotations_dir / src_ann.name

                import shutil

                shutil.copy(src_image, dst_image)
                shutil.copy(src_ann, dst_ann)

                # Write to filtered manifest
                out_f.write(json.dumps(entry) + "\n")
                filtered_count += 1

        print(f"\nExported {filtered_count} images to: {output_dir}")
        print(f"Filter: {status_filter}")


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="View and validate training datasets",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("dataset_dir", type=Path, help="Path to dataset directory")
    parser.add_argument(
        "--mode",
        choices=["summary", "browse", "validate", "export"],
        default="summary",
        help="Viewing mode (default: summary)",
    )
    parser.add_argument("--limit", type=int, default=10, help="Limit for browse mode (default: 10)")
    parser.add_argument("--output", type=Path, help="Output directory for export mode")
    parser.add_argument(
        "--filter",
        choices=["good", "bad", "skip"],
        default="good",
        help="Filter for export mode (default: good)",
    )

    args = parser.parse_args()

    # Validate dataset directory
    if not args.dataset_dir.exists():
        print(f"Error: Dataset directory not found: {args.dataset_dir}")
        sys.exit(1)

    viewer = DatasetViewer(args.dataset_dir)

    # Execute requested mode
    if args.mode == "summary":
        viewer.show_summary()
    elif args.mode == "browse":
        viewer.browse_images(limit=args.limit)
    elif args.mode == "validate":
        viewer.validate_interactive()
    elif args.mode == "export":
        if not args.output:
            print("Error: --output required for export mode")
            sys.exit(1)
        viewer.export_filtered(args.output, args.filter)


if __name__ == "__main__":
    main()
