#!/usr/bin/env python3
"""Generate embeddings for RAG project.

This script is called by the Rust runner to generate embeddings for GUI elements.
It reads the RAG config, generates text and image embeddings, and indexes elements
in a Qdrant vector database.

Usage:
    python generate_embeddings.py --project-id <id>
"""

import argparse
import asyncio
import json
import sys
import uuid
from pathlib import Path
from typing import Any

from PIL import Image
from qontinui.rag import (
    BoundingBox,
    CLIPEmbedder,
    DINOv2Embedder,
    GUIElementChunk,
    QdrantLocalDB,
    RAGIndex,
    TextDescriptionGenerator,
    TextEmbedder,
)


def log_progress(status: str, **kwargs: Any) -> None:
    """Output progress as JSON line for runner to parse.

    Args:
        status: Status type (progress, complete, error)
        **kwargs: Additional fields to include in JSON
    """
    message = {"status": status, **kwargs}
    print(json.dumps(message), flush=True)


def id_to_uuid(element_id: str) -> str:
    """Convert an element ID to a UUID v5 (deterministic).

    Uses a namespace UUID and the element ID to generate a consistent UUID.

    Args:
        element_id: Element ID string

    Returns:
        UUID string in standard format
    """
    # Use a fixed namespace for all Qontinui RAG elements
    namespace = uuid.UUID("6ba7b810-9dad-11d1-80b4-00c04fd430c8")  # URL namespace
    return str(uuid.uuid5(namespace, element_id))


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


def crop_element_from_screenshot(screenshot_path: Path, bounding_box: BoundingBox) -> Image.Image:
    """Crop element region from screenshot using bounding box.

    Args:
        screenshot_path: Path to screenshot image
        bounding_box: Bounding box coordinates

    Returns:
        Cropped PIL Image of the element
    """
    screenshot = Image.open(screenshot_path)
    # Bounding box format: (x, y, width, height)
    # PIL crop format: (left, upper, right, lower)
    left = bounding_box.x
    upper = bounding_box.y
    right = bounding_box.x + bounding_box.width
    lower = bounding_box.y + bounding_box.height

    cropped = screenshot.crop((left, upper, right, lower))
    return cropped


def extract_elements_from_qontinui_config(config: dict[str, Any]) -> list[dict[str, Any]]:
    """Extract GUI elements from QontinuiConfig format.

    QontinuiConfig stores patterns within states/stateImages.
    This function extracts them into a flat list of elements for embedding.

    Args:
        config: QontinuiConfig dictionary

    Returns:
        List of element dictionaries with id, name, text_description, bounding_box, etc.
    """
    elements = []
    states = config.get("states", [])

    for state in states:
        state_id = state.get("id", "")
        state_name = state.get("name", "")

        # Extract from stateImages -> patterns
        for state_image in state.get("stateImages", []):
            state_image_id = state_image.get("id", "")
            state_image_name = state_image.get("name", "")

            for pattern in state_image.get("patterns", []):
                pattern_id = pattern.get("id", "")
                pattern_name = pattern.get("name", state_image_name)
                image_id = pattern.get("imageId", "")

                # Create element from pattern
                # Use "image" as element_type since patterns are image-based elements
                element = {
                    "id": pattern_id or f"{state_image_id}-pattern",
                    "name": pattern_name,
                    "element_type": "image",  # Valid ElementType enum value
                    "element_subtype": "stateImage",
                    "text_description": f"{pattern_name} in state {state_name}",
                    "state_id": state_id,
                    "state_name": state_name,
                    "source_screenshot_id": image_id,  # References images/{image_id}.png
                    "similarity_threshold": pattern.get("similarity", 0.8),
                    "is_defining_element": True,
                }

                # Add bounding box if available (pattern covers full image)
                # For patterns, we use the full image
                element["use_full_image"] = True

                elements.append(element)

    return elements


async def generate_embeddings_for_project(project_id: str) -> None:
    """Generate embeddings for all elements in a RAG project.

    Supports both RAGConfig format (with 'elements' array) and
    QontinuiConfig format (with states/stateImages/patterns).

    Args:
        project_id: Project ID to generate embeddings for

    Raises:
        FileNotFoundError: If config or images not found
        RuntimeError: If embedding generation fails
    """
    try:
        # Get project directory
        rag_dir = get_rag_dir(project_id)
        config_path = rag_dir / "config.json"
        # Support both 'screenshots' (legacy) and 'images' (QontinuiConfig) directories
        images_dir = rag_dir / "images"
        screenshots_dir = rag_dir / "screenshots"
        embeddings_dir = rag_dir / "embeddings"
        db_path = embeddings_dir / "vector.qvdb"

        # Validate paths
        if not config_path.exists():
            raise FileNotFoundError(f"Config not found: {config_path}")

        # Use images dir if it exists, otherwise fall back to screenshots
        if images_dir.exists():
            source_images_dir = images_dir
        elif screenshots_dir.exists():
            source_images_dir = screenshots_dir
        else:
            raise FileNotFoundError(f"Neither images nor screenshots directory found in {rag_dir}")

        # Create embeddings directory
        embeddings_dir.mkdir(parents=True, exist_ok=True)

        log_progress("progress", percent=5, message="Loading config...")

        # Load config
        with open(config_path) as f:
            config = json.load(f)

        # Support both RAGConfig (elements) and QontinuiConfig (states/stateImages/patterns)
        elements_data = config.get("elements", [])
        if not elements_data:
            # Try to extract from QontinuiConfig format
            elements_data = extract_elements_from_qontinui_config(config)

        if not elements_data:
            log_progress("complete", elements_embedded=0, message="No elements to embed")
            return

        total_elements = len(elements_data)
        log_progress("progress", percent=10, message=f"Found {total_elements} elements")

        # Initialize embedders
        log_progress("progress", percent=15, message="Loading embedding models...")

        text_embedder = TextEmbedder(model_name="all-MiniLM-L6-v2")
        clip_embedder = CLIPEmbedder(model_name="openai/clip-vit-base-patch32")
        dinov2_embedder = DINOv2Embedder(model_name="dinov2_vitb14")
        text_generator = TextDescriptionGenerator()

        log_progress("progress", percent=25, message="Initializing vector database...")

        # Initialize vector database
        db = QdrantLocalDB(db_path)
        rag_index = RAGIndex(db)
        await rag_index.initialize()

        log_progress("progress", percent=30, message="Generating embeddings...")

        # Process each element
        processed_elements: list[GUIElementChunk] = []
        errors: list[str] = []

        for idx, element_data in enumerate(elements_data):
            try:
                # Calculate progress (30-90% range)
                progress = 30 + int((idx / total_elements) * 60)
                log_progress(
                    "progress",
                    percent=progress,
                    message=f"Processing element {idx + 1}/{total_elements}...",
                )

                # Create GUIElementChunk from config data
                element = GUIElementChunk.from_dict(element_data)

                # Generate text description if empty
                if not element.text_description:
                    element.text_description = text_generator.generate(element)

                # Generate text embedding
                if element.text_description:
                    element.text_embedding = text_embedder.encode(element.text_description)

                # Get image path
                image_id = element.source_screenshot_id
                if not image_id:
                    # Try to infer from element ID or use first image
                    images = list(source_images_dir.glob("*.png"))
                    if images:
                        image_path = images[0]
                    else:
                        raise FileNotFoundError("No images found")
                else:
                    image_path = source_images_dir / f"{image_id}.png"
                    if not image_path.exists():
                        # Try common extensions
                        for ext in [".jpg", ".jpeg", ".png"]:
                            alt_path = source_images_dir / f"{image_id}{ext}"
                            if alt_path.exists():
                                image_path = alt_path
                                break
                        else:
                            raise FileNotFoundError(f"Image not found: {image_path}")

                # Get element image - either use full pattern image or crop from screenshot
                use_full_image = element_data.get("use_full_image", False)
                if use_full_image or not element.bounding_box:
                    # Use the full image (for patterns)
                    element_image = Image.open(image_path)
                else:
                    # Crop from screenshot using bounding box
                    element_image = crop_element_from_screenshot(image_path, element.bounding_box)

                # Generate CLIP embedding
                clip_embedding = clip_embedder.encode_image(element_image)

                # Generate DINOv2 embedding (uses encode(), not encode_image())
                dinov2_embedding = dinov2_embedder.encode(element_image)

                # Store embeddings in element
                # Note: GUIElementChunk stores these in image_embedding field
                # For multi-vector search, we'll need to handle this in indexing
                element.image_embedding = clip_embedding

                # Create point with multiple vectors for Qdrant
                # We'll store both CLIP and DINOv2 embeddings
                # Convert element ID to UUID for Qdrant compatibility
                point = {
                    "id": id_to_uuid(element.id),
                    "vector": {
                        "text_embedding": element.text_embedding or [0.0] * 384,
                        "clip_embedding": clip_embedding,
                        "dinov2_embedding": dinov2_embedding,
                    },
                    "payload": {**element.to_dict(), "original_id": element.id},
                }

                # Index in RAG database
                await db.upsert(rag_index.COLLECTION_NAME, [point])

                processed_elements.append(element)

            except Exception as e:
                error_msg = f"Failed to process element {idx}: {e}"
                errors.append(error_msg)
                log_progress("progress", percent=progress, message=f"Warning: {error_msg}")

        # Save embeddings to JSON
        log_progress("progress", percent=90, message="Saving embeddings...")

        embeddings_json_path = embeddings_dir / "embeddings.json"
        embeddings_data = {
            "project_id": project_id,
            "total_elements": total_elements,
            "processed_elements": len(processed_elements),
            "failed_elements": len(errors),
            "elements": [elem.to_dict() for elem in processed_elements],
            "errors": errors,
        }

        with open(embeddings_json_path, "w") as f:
            json.dump(embeddings_data, f, indent=2)

        log_progress("progress", percent=95, message="Finalizing...")

        # Get final count from database
        element_count = await rag_index.get_element_count()

        # Close database
        db.close()

        log_progress("progress", percent=100, message="Complete")
        log_progress(
            "complete",
            elements_embedded=len(processed_elements),
            total=total_elements,
            failed=len(errors),
            db_element_count=element_count,
        )

    except FileNotFoundError as e:
        log_progress("error", message=f"File not found: {e}")
        sys.exit(1)
    except Exception as e:
        log_progress("error", message=f"Embedding generation failed: {e}")
        sys.exit(1)


def main() -> None:
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Generate embeddings for RAG project",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    python generate_embeddings.py --project-id abc123
    python generate_embeddings.py --project-id my-project
        """,
    )
    parser.add_argument(
        "--project-id",
        type=str,
        required=True,
        help="Project ID (config at ~/.qontinui/rag/{project_id}/config.json)",
    )

    args = parser.parse_args()

    # Run async main
    try:
        asyncio.run(generate_embeddings_for_project(args.project_id))
    except KeyboardInterrupt:
        log_progress("error", message="Interrupted by user")
        sys.exit(130)


if __name__ == "__main__":
    main()
