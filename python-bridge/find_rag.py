#!/usr/bin/env python3
"""Find RAG elements in a screenshot using SAM3 segmentation and vector matching.

This script performs the core RAG find operation:
1. Takes a screenshot (file path or base64) + project_id
2. Segments the screenshot using SAM3 (or grid fallback)
3. Vectorizes each segment with CLIP embeddings
4. Matches segments against ALL indexed StateImages in the vector database
5. Returns segments with their matches sorted by similarity

Usage:
    python find_rag.py --project-id <id> --screenshot /path/to/screenshot.png
    python find_rag.py --project-id <id> --screenshot-base64 <base64_data>
    python find_rag.py --project-id <id> --capture-screen --monitor 0
"""

import argparse
import asyncio
import base64
import io
import json
import sys
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image
from qontinui.rag import (
    QdrantLocalDB,
    RAGIndex,
    SegmentVectorizer,
)


def get_rag_dir(project_id: str) -> Path:
    """Get the RAG directory for a project.

    Args:
        project_id: Project ID

    Returns:
        Path to RAG directory (~/.qontinui/rag/{project_id})
    """
    home = Path.home()
    rag_dir = home / ".qontinui" / "rag" / project_id
    return rag_dir


def log_error(message: str, **kwargs: Any) -> None:
    """Output error as JSON line for runner to parse.

    Args:
        message: Error message
        **kwargs: Additional fields to include in JSON
    """
    error_data = {"error": message, **kwargs}
    print(json.dumps(error_data), file=sys.stderr, flush=True)


def log_progress(message: str, **kwargs: Any) -> None:
    """Output progress as JSON line for runner to parse.

    Args:
        message: Progress message
        **kwargs: Additional fields to include in JSON
    """
    progress_data = {"progress": message, **kwargs}
    print(json.dumps(progress_data), file=sys.stderr, flush=True)


def load_screenshot(
    screenshot_path: str | None = None,
    screenshot_base64: str | None = None,
    capture_screen: bool = False,
    monitor: int = 0,
) -> Image.Image:
    """Load screenshot from file, base64, or capture.

    Args:
        screenshot_path: Path to screenshot file
        screenshot_base64: Base64 encoded screenshot
        capture_screen: Whether to capture the screen
        monitor: Monitor index for capture

    Returns:
        PIL Image

    Raises:
        ValueError: If no valid source provided
    """
    if screenshot_path:
        return Image.open(screenshot_path).convert("RGB")

    if screenshot_base64:
        # Remove data URL prefix if present
        if "base64," in screenshot_base64:
            screenshot_base64 = screenshot_base64.split("base64,")[1]
        image_data = base64.b64decode(screenshot_base64)
        return Image.open(io.BytesIO(image_data)).convert("RGB")

    if capture_screen:
        try:
            from qontinui.hal import ScreenCapture  # type: ignore[attr-defined]

            capture = ScreenCapture()
            monitors = capture.list_monitors()
            if monitor >= len(monitors):
                raise ValueError(f"Monitor {monitor} not found. Available: {len(monitors)}")
            np_image = capture.capture_monitor(monitors[monitor])
            return Image.fromarray(np_image)
        except ImportError:
            raise ValueError(
                "Screen capture not available. Install qontinui with HAL support."
            ) from None

    raise ValueError("No screenshot source provided")


async def find_elements(
    project_id: str,
    screenshot: Image.Image,
    min_similarity: float = 0.5,
    max_segments: int = 100,
    enable_ocr: bool = False,
) -> dict[str, Any]:
    """Find all matching elements in a screenshot.

    Args:
        project_id: Project ID to search in
        screenshot: Screenshot image
        min_similarity: Minimum similarity threshold
        max_segments: Maximum segments to process
        enable_ocr: Whether to enable OCR text extraction

    Returns:
        Dictionary with segments and their matches

    Raises:
        FileNotFoundError: If embeddings database not found
        RuntimeError: If find operation fails
    """
    try:
        # Get project directory
        rag_dir = get_rag_dir(project_id)
        embeddings_dir = rag_dir / "embeddings"
        db_path = embeddings_dir / "vector.qvdb"

        # Validate database exists
        if not db_path.exists():
            raise FileNotFoundError(
                f"Embeddings database not found: {db_path}. Please generate embeddings first."
            )

        log_progress("Initializing segment vectorizer...")

        # Initialize segment vectorizer
        vectorizer = SegmentVectorizer(enable_ocr=enable_ocr)

        log_progress("Segmenting screenshot...")

        # Segment and vectorize the screenshot
        segment_vectors = vectorizer.vectorize_screenshot(
            screenshot,
            max_segments=max_segments,
            min_confidence=0.3,
        )

        log_progress(f"Found {len(segment_vectors)} segments")

        # Initialize vector database and RAG index
        db = QdrantLocalDB(db_path)
        rag_index = RAGIndex(db)

        log_progress("Loading indexed elements...")

        # Get all indexed elements
        # We'll search for each segment against all indexed elements
        all_elements = await rag_index.list_all_elements()

        log_progress(f"Matching against {len(all_elements)} indexed elements...")

        # Build results
        segments_data: list[dict[str, Any]] = []

        for i, segment in enumerate(segment_vectors):
            # Get segment info
            x, y, w, h = segment.bbox
            segment_info: dict[str, Any] = {
                "index": i,
                "bbox": {"x": x, "y": y, "width": w, "height": h},
                "center": {"x": segment.center[0], "y": segment.center[1]},
                "area": segment.area,
                "confidence": segment.confidence,
                "text_description": segment.text_description or "",
                "ocr_text": segment.ocr_text,
                "matches": [],
            }

            # Encode segment mask as base64 PNG for visualization
            # Convert mask to image
            mask_img = Image.fromarray((segment.mask * 255).astype(np.uint8))
            mask_buffer = io.BytesIO()
            mask_img.save(mask_buffer, format="PNG")
            segment_info["mask_base64"] = base64.b64encode(mask_buffer.getvalue()).decode()

            # Extract segment image for visualization
            segment_img = screenshot.crop((x, y, x + w, y + h))
            img_buffer = io.BytesIO()
            segment_img.save(img_buffer, format="PNG")
            segment_info["image_base64"] = base64.b64encode(img_buffer.getvalue()).decode()

            # Match against all indexed elements
            for element in all_elements:
                # Skip elements without image embeddings
                if not element.image_embedding:
                    continue

                # Calculate visual similarity
                visual_sim = vectorizer._cosine_similarity(
                    element.image_embedding,
                    segment.image_embedding,
                )

                # Apply threshold
                if visual_sim < min_similarity:
                    continue

                # Calculate text similarity if available
                text_sim: float | None = None
                if element.text_embedding and segment.text_embedding:
                    text_sim = vectorizer._cosine_similarity(
                        element.text_embedding,
                        segment.text_embedding,
                    )

                # Calculate combined score
                combined_score = visual_sim * 0.7
                if text_sim is not None:
                    combined_score += text_sim * 0.3
                else:
                    combined_score += visual_sim * 0.3

                match_info: dict[str, Any] = {
                    "element_id": element.id,
                    "element_name": element.name,  # type: ignore[attr-defined]
                    "visual_similarity": float(visual_sim),
                    "text_similarity": float(text_sim) if text_sim else None,
                    "combined_score": float(combined_score),
                    "element_type": element.element_type.value if element.element_type else None,
                    "text_description": element.text_description or "",
                    "state_id": element.state_id,
                }

                segment_info["matches"].append(match_info)

            # Sort matches by combined score
            segment_info["matches"].sort(
                key=lambda m: m["combined_score"],
                reverse=True,
            )

            # Limit to top 5 matches per segment
            segment_info["matches"] = segment_info["matches"][:5]

            segments_data.append(segment_info)

        # Close database
        db.close()

        # Sort segments by best match score (segments with better matches first)
        segments_data.sort(
            key=lambda s: s["matches"][0]["combined_score"] if s["matches"] else 0,
            reverse=True,
        )

        return {
            "success": True,
            "project_id": project_id,
            "screenshot_size": {"width": screenshot.width, "height": screenshot.height},
            "total_segments": len(segment_vectors),
            "total_elements": len(all_elements),
            "min_similarity": min_similarity,
            "segments": segments_data,
        }

    except FileNotFoundError as e:
        log_error(f"File not found: {e}")
        raise
    except Exception as e:
        log_error(f"Find operation failed: {e}")
        raise RuntimeError(f"Find operation failed: {e}") from e


def main() -> None:
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Find RAG elements in a screenshot using SAM3 segmentation",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    python find_rag.py --project-id abc123 --screenshot /path/to/screenshot.png
    python find_rag.py --project-id my-project --screenshot-base64 <base64_data>
    python find_rag.py --project-id my-project --capture-screen --monitor 0
        """,
    )
    parser.add_argument(
        "--project-id",
        type=str,
        required=True,
        help="Project ID (embeddings at ~/.qontinui/rag/{project_id}/embeddings/)",
    )
    parser.add_argument(
        "--screenshot",
        type=str,
        help="Path to screenshot file",
    )
    parser.add_argument(
        "--screenshot-base64",
        type=str,
        help="Base64 encoded screenshot",
    )
    parser.add_argument(
        "--capture-screen",
        action="store_true",
        help="Capture the screen instead of using a file",
    )
    parser.add_argument(
        "--monitor",
        type=int,
        default=0,
        help="Monitor index for screen capture (default: 0)",
    )
    parser.add_argument(
        "--min-similarity",
        type=float,
        default=0.5,
        help="Minimum similarity threshold 0.0-1.0 (default: 0.5)",
    )
    parser.add_argument(
        "--max-segments",
        type=int,
        default=100,
        help="Maximum number of segments to process (default: 100)",
    )
    parser.add_argument(
        "--enable-ocr",
        action="store_true",
        help="Enable OCR text extraction",
    )

    args = parser.parse_args()

    # Validate arguments
    if not args.screenshot and not args.screenshot_base64 and not args.capture_screen:
        log_error(
            "No screenshot source provided. Use --screenshot, --screenshot-base64, or --capture-screen"
        )
        sys.exit(1)

    if not 0.0 <= args.min_similarity <= 1.0:
        log_error("Min similarity must be between 0.0 and 1.0")
        sys.exit(1)

    try:
        # Load screenshot
        log_progress("Loading screenshot...")
        screenshot = load_screenshot(
            screenshot_path=args.screenshot,
            screenshot_base64=args.screenshot_base64,
            capture_screen=args.capture_screen,
            monitor=args.monitor,
        )

        # Run async find operation
        results = asyncio.run(
            find_elements(
                project_id=args.project_id,
                screenshot=screenshot,
                min_similarity=args.min_similarity,
                max_segments=args.max_segments,
                enable_ocr=args.enable_ocr,
            )
        )

        # Output results as JSON
        print(json.dumps(results, indent=2), flush=True)

    except FileNotFoundError as e:
        output = {
            "success": False,
            "error": str(e),
            "project_id": args.project_id,
            "segments": [],
        }
        print(json.dumps(output, indent=2), flush=True)
        sys.exit(1)

    except Exception as e:
        output = {
            "success": False,
            "error": f"Find operation failed: {e}",
            "project_id": args.project_id,
            "segments": [],
        }
        print(json.dumps(output, indent=2), flush=True)
        sys.exit(1)


if __name__ == "__main__":
    main()
