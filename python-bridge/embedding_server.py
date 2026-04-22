"""Standalone embedding service serving MiniLM-L6-v2 on port 8001.

This is a lightweight FastAPI app that exposes the same endpoints as the
web backend's /api/embeddings router, but runs as an independent process
managed by the runner's Process Manager.

Endpoints:
    POST /api/embeddings/compute-text  — single 384-dim text embedding
    POST /api/embeddings/compute-batch — batch text embeddings
    GET  /api/embeddings/status        — health / readiness probe
"""

import os
import sys
import threading

# ---------------------------------------------------------------------------
# Ensure the qontinui package is importable.  When launched from the runner
# the CWD is python-bridge/ but the qontinui package lives one level up in
# the workspace.  We add the parent qontinui/src to sys.path if needed.
# ---------------------------------------------------------------------------
_SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
_WORKSPACE_ROOT = os.path.dirname(os.path.dirname(_SCRIPT_DIR))  # qontinui-root
_QONTINUI_SRC = os.path.join(_WORKSPACE_ROOT, "qontinui", "src")
if _QONTINUI_SRC not in sys.path and os.path.isdir(_QONTINUI_SRC):
    sys.path.insert(0, _QONTINUI_SRC)

import uvicorn  # noqa: E402 — must load after sys.path fixup above
from fastapi import FastAPI  # noqa: E402
from pydantic import BaseModel  # noqa: E402

# ---------------------------------------------------------------------------
# Lazy-loaded, thread-safe embedding provider singleton
# ---------------------------------------------------------------------------
_provider = None
_provider_lock = threading.Lock()


def _get_provider():
    global _provider
    if _provider is not None:
        return _provider
    with _provider_lock:
        if _provider is not None:
            return _provider
        from qontinui.embeddings import (
            EmbeddingConfig,
            EmbeddingProviderType,
            get_embedding_provider,
        )

        config = EmbeddingConfig(provider=EmbeddingProviderType.SENTENCE_TRANSFORMERS)
        _provider = get_embedding_provider(config)
        print(
            f"[embedding-server] Loaded model={config.model_name} dim={_provider.dimension}",
            flush=True,
        )
    return _provider


# ---------------------------------------------------------------------------
# Schemas
# ---------------------------------------------------------------------------


class TextEmbeddingRequest(BaseModel):
    text: str
    model: str = "minilm"


class BatchEmbeddingRequest(BaseModel):
    texts: list[str]
    model: str = "minilm"


class TextEmbeddingResponse(BaseModel):
    success: bool
    embedding: list[float]
    embedding_dim: int
    error: str | None = None


class BatchEmbeddingResponse(BaseModel):
    success: bool
    embeddings: list[list[float]]
    embedding_dim: int


# ---------------------------------------------------------------------------
# App
# ---------------------------------------------------------------------------

app = FastAPI(title="Qontinui Embedding Service", version="1.0.0")


@app.post("/api/embeddings/compute-text", response_model=TextEmbeddingResponse)
def compute_text_embedding(request: TextEmbeddingRequest):
    """Compute a single 384-dim text embedding."""
    try:
        provider = _get_provider()
        vec = provider.embed(request.text)
        return TextEmbeddingResponse(
            success=True,
            embedding=vec.tolist(),
            embedding_dim=len(vec),
        )
    except Exception as e:
        return TextEmbeddingResponse(
            success=False,
            embedding=[],
            embedding_dim=0,
            error=str(e),
        )


@app.post("/api/embeddings/compute-batch", response_model=BatchEmbeddingResponse)
def compute_batch_embedding(request: BatchEmbeddingRequest):
    """Compute 384-dim embeddings for multiple texts."""
    try:
        provider = _get_provider()
        matrix = provider.embed_batch(request.texts)
        return BatchEmbeddingResponse(
            success=True,
            embeddings=[row.tolist() for row in matrix],
            embedding_dim=int(matrix.shape[1]) if len(matrix) > 0 else 384,
        )
    except Exception:
        return BatchEmbeddingResponse(
            success=False,
            embeddings=[],
            embedding_dim=0,
        )


@app.get("/api/embeddings/status")
def embedding_status():
    """Health/status probe for the embedding service."""
    try:
        provider = _get_provider()
        return {
            "available": True,
            "model": "all-MiniLM-L6-v2",
            "dimension": provider.dimension,
        }
    except Exception as e:
        return {"available": False, "error": str(e)}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    port = int(os.environ.get("EMBEDDING_PORT", "8001"))
    host = os.environ.get("EMBEDDING_HOST", "127.0.0.1")
    print(f"[embedding-server] Starting on {host}:{port}", flush=True)
    uvicorn.run(app, host=host, port=port, log_level="info")
