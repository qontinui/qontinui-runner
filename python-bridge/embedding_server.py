"""Standalone embedding service serving MiniLM-L6-v2 on port 8001.

This is a lightweight FastAPI app that exposes the same endpoints as the
web backend's /api/embeddings router, but runs as an independent process
managed by the runner's Process Manager.

Endpoints:
    POST /api/embeddings/compute-text  — single 384-dim text embedding
    POST /api/embeddings/compute-batch — batch text embeddings
    GET  /api/embeddings/status        — health / readiness probe

Memory footprint
----------------
This service MUST NOT create a CUDA context.  The python-bridge Poetry env
installs the CUDA build of torch (the `pytorch-cu128` source is pinned by the
`qontinui` path dep for qontinui-train's benefit), so `device="auto"` — the
`EmbeddingConfig` default — resolves to `cuda` whenever a GPU is present.
Measured on an RTX 3070 Laptop / Windows 11 box, that costs ~4.8 GB of
*private commit* for a ~90 MB model:

    import torch                    1343 MB
    + import sentence_transformers  2016 MB
    + model on cuda                 5542 MB   (CUDA context creation)
    + first inference               6980 MB   (cuBLAS/cuDNN kernel images)
    + model on cpu                  2179 MB   (steady state ~2311 MB)

On Windows/WDDM that commit is pagefile-backed, so the process can show a
40 MB working set while still holding ~7 GB of commit — it looks idle while
consuming the entire commit margin the box needs for other work.  The charge is
a one-time step at the first request (the provider is lazy), not a leak: both
devices hold flat memory across thousands of requests.

CUDA buys almost nothing here.  Driving this service over HTTP with an
identical 1200-single + 40-batch-of-32 load, back to back on the same box:

    device=auto -> cuda   7078 MB private commit   p50 27.1 ms   p95 45.6 ms
    device=cpu            2260 MB private commit   p50 33.6 ms   p95 82.8 ms

4.8 GB reclaimed for 6.5 ms of median latency.  The vectors are unchanged —
CPU vs CUDA embeddings of the same texts agree to a minimum cosine of
0.99999988 (max elementwise delta 2.1e-07), so `EMBEDDING_MODEL_TAG`
("minilm-l6-v2-256@sentence-transformers", see src-tauri/src/database/
embedding_client.rs) still names the same space and stored vectors stay
comparable.

The device therefore defaults to `cpu` and CUDA is opt-in via
`EMBEDDING_DEVICE`.
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
# Configuration
# ---------------------------------------------------------------------------

#: Devices `EmbeddingConfig.device` accepts.  "auto" is deliberately excluded:
#: on a CUDA-capable box it silently resolves to "cuda" and reintroduces the
#: multi-gigabyte commit charge documented above.  Ask for "cuda" explicitly.
VALID_DEVICES = frozenset({"cpu", "cuda", "mps"})

DEFAULT_DEVICE = "cpu"

#: Intra-op thread count for CPU inference.  Left unbounded, torch grabs one
#: thread per core (16 on the dev box) and every request pays fork/join barrier
#: costs on a machine that is already busy.  Measured single-text latency here:
#: 4 threads 12.2 ms, 8 threads 21.5 ms, 2 threads 35.7 ms, 1 thread 33.2 ms.
DEFAULT_TORCH_THREADS = 4

#: Entries in the in-memory embedding cache.  Bounded (LRU) and memory-only —
#: 4096 x 384 float32 is ~6 MB.  Disk persistence is deliberately off: the
#: library's default writes one .npy per distinct text into the temp dir with
#: no bound at all.
DEFAULT_CACHE_SIZE = 4096


def _env_int(name: str, default: int, minimum: int) -> int:
    """Read an int from the environment, falling back to `default` on garbage."""
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        value = int(raw)
    except ValueError:
        print(f"[embedding-server] {name}={raw!r} is not an integer; using {default}", flush=True)
        return default
    if value < minimum:
        print(f"[embedding-server] {name}={value} is below {minimum}; using {default}", flush=True)
        return default
    return value


def resolve_device(raw: str | None) -> str:
    """Resolve the configured device name, defaulting to CPU.

    Args:
        raw: Value of the ``EMBEDDING_DEVICE`` environment variable, if set.

    Returns:
        A device string accepted by ``EmbeddingConfig``. Unknown values — and
        ``"auto"``, which would resolve to CUDA behind our back — fall back to
        ``"cpu"`` with a warning rather than crashing the service.
    """
    if raw is None:
        return DEFAULT_DEVICE
    device = raw.strip().lower()
    if not device:
        return DEFAULT_DEVICE
    if device not in VALID_DEVICES:
        print(
            f"[embedding-server] EMBEDDING_DEVICE={raw!r} is not one of "
            f"{sorted(VALID_DEVICES)}; using {DEFAULT_DEVICE!r}",
            flush=True,
        )
        return DEFAULT_DEVICE
    return device


DEVICE = resolve_device(os.environ.get("EMBEDDING_DEVICE"))
TORCH_THREADS = _env_int("EMBEDDING_TORCH_THREADS", DEFAULT_TORCH_THREADS, minimum=1)
CACHE_SIZE = _env_int("EMBEDDING_CACHE_SIZE", DEFAULT_CACHE_SIZE, minimum=1)

# ---------------------------------------------------------------------------
# Lazy-loaded, thread-safe embedding provider singleton
# ---------------------------------------------------------------------------
_provider = None
_provider_lock = threading.Lock()

#: Serialises inference.  FastAPI runs these sync endpoints on a ~40-thread
#: pool; without this, every concurrent request starts its own multi-threaded
#: torch forward pass, oversubscribing the CPU and multiplying peak memory by
#: the number of in-flight requests.  One forward pass at a time — each already
#: parallel across TORCH_THREADS — is both faster and bounded.  It also makes
#: the shared LRU cache safe to mutate.
_inference_lock = threading.Lock()


def _get_provider():
    global _provider
    if _provider is not None:
        return _provider
    with _provider_lock:
        if _provider is not None:
            return _provider
        import torch
        from qontinui.embeddings import (
            CachedEmbeddingProvider,
            EmbeddingCache,
            EmbeddingConfig,
            EmbeddingProviderType,
            get_embedding_provider,
        )

        torch.set_num_threads(TORCH_THREADS)

        config = EmbeddingConfig(
            provider=EmbeddingProviderType.SENTENCE_TRANSFORMERS,
            device=DEVICE,
        )
        base = get_embedding_provider(config)
        _provider = CachedEmbeddingProvider(
            base,
            cache=EmbeddingCache(max_size=CACHE_SIZE, persist_to_disk=False),
        )
        print(
            f"[embedding-server] Loaded model={config.get_model_name()} "
            f"dim={_provider.dimension} device={DEVICE} "
            f"torch_threads={torch.get_num_threads()} cache_max={CACHE_SIZE}",
            flush=True,
        )
    return _provider


def _memory_mb() -> dict[str, float]:
    """Best-effort process memory snapshot for the status probe.

    psutil is not a declared dependency of python-bridge, so this degrades to
    an empty dict rather than failing the probe.
    """
    try:
        import psutil
    except ImportError:
        return {}
    info = psutil.Process().memory_info()
    return {
        "private_mb": round(getattr(info, "private", info.rss) / (1024 * 1024), 1),
        "rss_mb": round(info.rss / (1024 * 1024), 1),
    }


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
        with _inference_lock:
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
        with _inference_lock:
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
            "device": DEVICE,
            "torch_threads": TORCH_THREADS,
            "cache": provider.cache_stats(),
            "memory": _memory_mb(),
        }
    except Exception as e:
        return {"available": False, "error": str(e), "device": DEVICE}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    port = int(os.environ.get("EMBEDDING_PORT", "8001"))
    host = os.environ.get("EMBEDDING_HOST", "127.0.0.1")
    print(
        f"[embedding-server] Starting on {host}:{port} "
        f"(device={DEVICE}, torch_threads={TORCH_THREADS}, cache_max={CACHE_SIZE})",
        flush=True,
    )
    uvicorn.run(app, host=host, port=port, log_level="info")
