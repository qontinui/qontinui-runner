"""
Model Manager for Lazy Loading ML Models.

This module handles:
- Downloading models on first use (SAM, CLIP, EasyOCR language packs)
- Caching models in the user data directory
- Progress reporting for downloads
- Verification of downloaded models

Models are NOT bundled with the installer to keep the initial download small.
They are downloaded on demand when first needed.

Model Registry:
- SAM3 (Segment Anything Model 3): ~2.5GB
- CLIP (OpenAI's vision-language model): ~1.5GB
- EasyOCR language packs: ~100MB per language

Usage:
    from services.model_manager import ModelManager

    manager = ModelManager()

    # Check if a model is available
    if not manager.is_available("sam3"):
        # Download with progress callback
        manager.download("sam3", progress_callback=lambda p: print(f"{p}%"))

    # Get the model path
    path = manager.get_model_path("sam3")
"""

import hashlib
import json
import logging
import os
import shutil
import tempfile
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Callable

import httpx

logger = logging.getLogger(__name__)


class ModelType(str, Enum):
    """Supported model types."""

    SAM3 = "sam3"
    SAM3_LARGE = "sam3_large"
    CLIP = "clip"
    EASYOCR = "easyocr"


@dataclass
class ModelInfo:
    """Information about a downloadable model."""

    name: str
    model_type: ModelType
    url: str
    filename: str
    size_bytes: int
    sha256: str | None = None
    description: str = ""


# Model registry - URLs and checksums for all downloadable models
# Note: These URLs point to the official model repositories
MODEL_REGISTRY: dict[str, ModelInfo] = {
    "sam3": ModelInfo(
        name="SAM3 Base",
        model_type=ModelType.SAM3,
        url="https://dl.fbaipublicfiles.com/segment_anything/sam_vit_b_01ec64.pth",
        filename="sam_vit_b_01ec64.pth",
        size_bytes=375_000_000,  # ~375MB
        sha256="01ec64d29a2fca3f0661936605ae66be39e3cf60ad41cbef9d3dbc0c0caefc29",
        description="Segment Anything Model 3 - Base variant (ViT-B)",
    ),
    "sam3_large": ModelInfo(
        name="SAM3 Large",
        model_type=ModelType.SAM3_LARGE,
        url="https://dl.fbaipublicfiles.com/segment_anything/sam_vit_l_0b3195.pth",
        filename="sam_vit_l_0b3195.pth",
        size_bytes=1_250_000_000,  # ~1.25GB
        sha256="0b3195507c641ddb6910d2bb5adee89c0d7c9d6ad35f3d1c6a81ee1e4e2b5cb3",
        description="Segment Anything Model 3 - Large variant (ViT-L)",
    ),
    # CLIP model (used for semantic understanding)
    "clip_vit_b32": ModelInfo(
        name="CLIP ViT-B/32",
        model_type=ModelType.CLIP,
        url="https://openaipublic.azureedge.net/clip/models/40d365715913c9da98579312b702a82c18be219cc2a73407c4526f58eba950af/ViT-B-32.pt",
        filename="ViT-B-32.pt",
        size_bytes=354_000_000,  # ~354MB
        sha256="40d365715913c9da98579312b702a82c18be219cc2a73407c4526f58eba950af",
        description="CLIP Vision Transformer Base/32",
    ),
    # EasyOCR models - just the core detection model
    # Language packs are handled separately by EasyOCR itself
    "easyocr_detection": ModelInfo(
        name="EasyOCR Detection",
        model_type=ModelType.EASYOCR,
        url="https://github.com/JaidedAI/EasyOCR/releases/download/v1.3/craft_mlt_25k.pth",
        filename="craft_mlt_25k.pth",
        size_bytes=100_000_000,  # ~100MB
        sha256=None,  # EasyOCR handles verification
        description="EasyOCR text detection model",
    ),
}


class DownloadError(Exception):
    """Error during model download."""

    pass


class VerificationError(Exception):
    """Model verification failed."""

    pass


class ModelManager:
    """
    Manages ML model downloads and caching.

    Models are stored in the user's app data directory:
    - Windows: %APPDATA%/com.qontinui.runner/models/
    - macOS: ~/Library/Application Support/com.qontinui.runner/models/
    - Linux: ~/.local/share/com.qontinui.runner/models/

    The directory can be overridden with the QONTINUI_MODELS_DIR environment variable.
    """

    def __init__(self, models_dir: str | Path | None = None):
        """
        Initialize the model manager.

        Args:
            models_dir: Custom directory for model storage. If None, uses
                       QONTINUI_MODELS_DIR env var or platform-specific default.
        """
        if models_dir:
            self.models_dir = Path(models_dir)
        elif os.environ.get("QONTINUI_MODELS_DIR"):
            self.models_dir = Path(os.environ["QONTINUI_MODELS_DIR"])
        else:
            self.models_dir = self._get_default_models_dir()

        self.models_dir.mkdir(parents=True, exist_ok=True)

        # Status file tracks downloaded models
        self._status_file = self.models_dir / "status.json"
        self._status = self._load_status()

        logger.info(f"Model manager initialized with models_dir: {self.models_dir}")

    def _get_default_models_dir(self) -> Path:
        """Get the platform-specific default models directory."""
        import sys

        if sys.platform == "win32":
            app_data = os.environ.get("APPDATA", Path.home() / "AppData" / "Roaming")
            return Path(app_data) / "com.qontinui.runner" / "models"
        elif sys.platform == "darwin":
            return Path.home() / "Library" / "Application Support" / "com.qontinui.runner" / "models"
        else:
            xdg_data = os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")
            return Path(xdg_data) / "com.qontinui.runner" / "models"

    def _load_status(self) -> dict:
        """Load the status file."""
        if self._status_file.exists():
            try:
                return json.loads(self._status_file.read_text())
            except Exception as e:
                logger.warning(f"Failed to load status file: {e}")
        return {"downloaded": {}, "version": 1}

    def _save_status(self) -> None:
        """Save the status file."""
        try:
            self._status_file.write_text(json.dumps(self._status, indent=2))
        except Exception as e:
            logger.warning(f"Failed to save status file: {e}")

    def is_available(self, model_id: str) -> bool:
        """
        Check if a model is available locally.

        Args:
            model_id: Model identifier (e.g., "sam3", "clip_vit_b32")

        Returns:
            True if the model is downloaded and verified
        """
        if model_id not in MODEL_REGISTRY:
            logger.warning(f"Unknown model: {model_id}")
            return False

        model_info = MODEL_REGISTRY[model_id]
        model_path = self.models_dir / model_info.filename

        if not model_path.exists():
            return False

        # Check if it's marked as downloaded in status
        if model_id in self._status.get("downloaded", {}):
            return True

        # File exists but not in status - verify it
        try:
            self._verify_model(model_id, model_path)
            self._status.setdefault("downloaded", {})[model_id] = {
                "path": str(model_path),
                "verified": True,
            }
            self._save_status()
            return True
        except VerificationError:
            return False

    def get_model_path(self, model_id: str) -> Path | None:
        """
        Get the path to a downloaded model.

        Args:
            model_id: Model identifier

        Returns:
            Path to the model file, or None if not available
        """
        if not self.is_available(model_id):
            return None

        model_info = MODEL_REGISTRY[model_id]
        return self.models_dir / model_info.filename

    def get_model_info(self, model_id: str) -> ModelInfo | None:
        """Get information about a model."""
        return MODEL_REGISTRY.get(model_id)

    def list_models(self) -> list[dict]:
        """
        List all available models with their status.

        Returns:
            List of model info dictionaries with availability status
        """
        result = []
        for model_id, info in MODEL_REGISTRY.items():
            result.append({
                "id": model_id,
                "name": info.name,
                "type": info.model_type.value,
                "description": info.description,
                "size_bytes": info.size_bytes,
                "available": self.is_available(model_id),
            })
        return result

    def download(
        self,
        model_id: str,
        progress_callback: Callable[[int], None] | None = None,
        force: bool = False,
    ) -> Path:
        """
        Download a model.

        Args:
            model_id: Model identifier
            progress_callback: Called with progress percentage (0-100)
            force: If True, re-download even if already available

        Returns:
            Path to the downloaded model

        Raises:
            DownloadError: If download fails
            VerificationError: If verification fails
        """
        if model_id not in MODEL_REGISTRY:
            raise DownloadError(f"Unknown model: {model_id}")

        if self.is_available(model_id) and not force:
            logger.info(f"Model {model_id} already available")
            return self.get_model_path(model_id)  # type: ignore

        model_info = MODEL_REGISTRY[model_id]
        model_path = self.models_dir / model_info.filename

        logger.info(f"Downloading model {model_id} from {model_info.url}")

        # Download to temp file first
        temp_path = None
        try:
            with tempfile.NamedTemporaryFile(delete=False, suffix=".tmp") as temp_file:
                temp_path = Path(temp_file.name)

                with httpx.stream("GET", model_info.url, follow_redirects=True, timeout=300) as response:
                    response.raise_for_status()

                    total_size = int(response.headers.get("content-length", model_info.size_bytes))
                    downloaded = 0

                    for chunk in response.iter_bytes(chunk_size=8192):
                        temp_file.write(chunk)
                        downloaded += len(chunk)

                        if progress_callback:
                            progress = min(100, int(downloaded * 100 / total_size))
                            progress_callback(progress)

            # Verify the download
            logger.info(f"Verifying downloaded model {model_id}")
            self._verify_model(model_id, temp_path)

            # Move to final location
            shutil.move(str(temp_path), str(model_path))
            temp_path = None

            # Update status
            self._status.setdefault("downloaded", {})[model_id] = {
                "path": str(model_path),
                "verified": True,
            }
            self._save_status()

            logger.info(f"Model {model_id} downloaded successfully to {model_path}")
            return model_path

        except httpx.HTTPError as e:
            raise DownloadError(f"HTTP error downloading {model_id}: {e}") from e
        except Exception as e:
            raise DownloadError(f"Error downloading {model_id}: {e}") from e
        finally:
            # Clean up temp file on error
            if temp_path and temp_path.exists():
                temp_path.unlink()

    def _verify_model(self, model_id: str, path: Path) -> None:
        """
        Verify a downloaded model.

        Args:
            model_id: Model identifier
            path: Path to the model file

        Raises:
            VerificationError: If verification fails
        """
        model_info = MODEL_REGISTRY[model_id]

        # Check file size
        actual_size = path.stat().st_size
        expected_size = model_info.size_bytes

        # Allow 10% variance in size (compression, metadata, etc.)
        if actual_size < expected_size * 0.9:
            raise VerificationError(
                f"Model {model_id} size mismatch: expected ~{expected_size}, got {actual_size}"
            )

        # Check SHA256 if available
        if model_info.sha256:
            sha256_hash = hashlib.sha256()
            with open(path, "rb") as f:
                for chunk in iter(lambda: f.read(8192), b""):
                    sha256_hash.update(chunk)

            actual_hash = sha256_hash.hexdigest()
            if actual_hash != model_info.sha256:
                raise VerificationError(
                    f"Model {model_id} hash mismatch: expected {model_info.sha256}, got {actual_hash}"
                )

    def delete(self, model_id: str) -> bool:
        """
        Delete a downloaded model.

        Args:
            model_id: Model identifier

        Returns:
            True if deleted, False if not found
        """
        if model_id not in MODEL_REGISTRY:
            return False

        model_info = MODEL_REGISTRY[model_id]
        model_path = self.models_dir / model_info.filename

        if model_path.exists():
            model_path.unlink()

        if model_id in self._status.get("downloaded", {}):
            del self._status["downloaded"][model_id]
            self._save_status()

        logger.info(f"Model {model_id} deleted")
        return True

    def get_disk_usage(self) -> dict:
        """
        Get disk usage information.

        Returns:
            Dictionary with usage statistics
        """
        total_size = 0
        model_sizes = {}

        for model_id, info in MODEL_REGISTRY.items():
            model_path = self.models_dir / info.filename
            if model_path.exists():
                size = model_path.stat().st_size
                model_sizes[model_id] = size
                total_size += size

        return {
            "total_bytes": total_size,
            "models": model_sizes,
            "models_dir": str(self.models_dir),
        }


# Global instance for convenience
_manager: ModelManager | None = None


def get_model_manager() -> ModelManager:
    """Get the global model manager instance."""
    global _manager
    if _manager is None:
        _manager = ModelManager()
    return _manager


def ensure_model(model_id: str, progress_callback: Callable[[int], None] | None = None) -> Path:
    """
    Ensure a model is available, downloading if necessary.

    This is a convenience function for common use cases.

    Args:
        model_id: Model identifier
        progress_callback: Optional progress callback

    Returns:
        Path to the model file
    """
    manager = get_model_manager()
    if not manager.is_available(model_id):
        return manager.download(model_id, progress_callback)
    return manager.get_model_path(model_id)  # type: ignore
