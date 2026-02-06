#!/usr/bin/env python3
"""Search RAG elements using semantic vector similarity.

This script performs semantic search on indexed GUI elements using Qdrant.
It generates embeddings for the query text and retrieves the most similar elements.

Usage:
    python search_rag.py --project-id <id> --query "login button" --limit 10
    python search_rag.py --project-id <id> --query "search" --limit 5 --min-score 0.7
"""

import argparse
import asyncio
import json
import sys
from pathlib import Path
from typing import Any

from qontinui.rag import (
    QdrantLocalDB,
    RAGIndex,
    TextEmbedder,
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


async def search_elements(
    project_id: str,
    query: str,
    limit: int = 10,
    min_score: float = 0.0,
) -> list[dict[str, Any]]:
    """Search GUI elements using semantic similarity.

    Args:
        project_id: Project ID to search in
        query: Search query text
        limit: Maximum number of results to return
        min_score: Minimum similarity score (0.0-1.0)

    Returns:
        List of search results with element data and similarity scores

    Raises:
        FileNotFoundError: If embeddings database not found
        RuntimeError: If search fails
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

        # Initialize text embedder (same model as used in generate_embeddings.py)
        text_embedder = TextEmbedder(model_name="all-MiniLM-L6-v2")

        # Generate query embedding
        query_embedding = text_embedder.encode(query)

        # Initialize vector database and RAG index
        db = QdrantLocalDB(db_path)
        rag_index = RAGIndex(db)

        # Perform semantic search using text embedding
        search_results = await rag_index.search_by_text(
            query_embedding=query_embedding,
            filters=None,  # No filters for now
            limit=limit,
        )

        # Convert results to JSON-serializable format
        results = []
        for result in search_results:
            # Filter by minimum score
            if result.score < min_score:
                continue

            element = result.element

            # Build result dictionary
            result_dict = {
                "element_id": element.id,
                "name": element.name,  # type: ignore[attr-defined]
                "score": float(result.score),
                "element_type": element.element_type.value if element.element_type else None,
                "text_description": element.text_description or "",
                "source_screenshot_id": element.source_screenshot_id or "",
                "state_id": element.state_id or "",
                "bounding_box": (
                    {
                        "x": element.bounding_box.x,
                        "y": element.bounding_box.y,
                        "width": element.bounding_box.width,
                        "height": element.bounding_box.height,
                    }
                    if element.bounding_box
                    else None
                ),
                "metadata": element.metadata or {},  # type: ignore[attr-defined]
            }
            results.append(result_dict)

        # Close database
        db.close()

        return results

    except FileNotFoundError as e:
        log_error(f"File not found: {e}")
        raise
    except Exception as e:
        log_error(f"Search failed: {e}")
        raise RuntimeError(f"Search failed: {e}") from e


def main() -> None:
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Search RAG elements using semantic similarity",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    python search_rag.py --project-id abc123 --query "login button"
    python search_rag.py --project-id my-project --query "search" --limit 5
    python search_rag.py --project-id my-project --query "submit" --min-score 0.7
        """,
    )
    parser.add_argument(
        "--project-id",
        type=str,
        required=True,
        help="Project ID (embeddings at ~/.qontinui/rag/{project_id}/embeddings/)",
    )
    parser.add_argument(
        "--query",
        type=str,
        required=True,
        help="Search query text",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=10,
        help="Maximum number of results (default: 10)",
    )
    parser.add_argument(
        "--min-score",
        type=float,
        default=0.0,
        help="Minimum similarity score 0.0-1.0 (default: 0.0)",
    )

    args = parser.parse_args()

    # Validate arguments
    if args.limit < 1:
        log_error("Limit must be at least 1")
        sys.exit(1)

    if not 0.0 <= args.min_score <= 1.0:
        log_error("Min score must be between 0.0 and 1.0")
        sys.exit(1)

    # Run async search
    try:
        results = asyncio.run(
            search_elements(
                project_id=args.project_id,
                query=args.query,
                limit=args.limit,
                min_score=args.min_score,
            )
        )

        # Output results as JSON
        output = {
            "success": True,
            "query": args.query,
            "result_count": len(results),
            "results": results,
        }
        print(json.dumps(output, indent=2), flush=True)

    except FileNotFoundError as e:
        output = {
            "success": False,
            "error": str(e),
            "query": args.query,
            "result_count": 0,
            "results": [],
        }
        print(json.dumps(output, indent=2), flush=True)
        sys.exit(1)

    except Exception as e:
        output = {
            "success": False,
            "error": f"Search failed: {e}",
            "query": args.query,
            "result_count": 0,
            "results": [],
        }
        print(json.dumps(output, indent=2), flush=True)
        sys.exit(1)


if __name__ == "__main__":
    main()
