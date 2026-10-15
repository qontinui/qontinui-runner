"""Model management commands (``models_*``).

A mixin of ``QontinuiExecutor``. Its methods were moved verbatim from
``qontinui_executor.py`` by plan
2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain
and still read executor state through ``self``. ``COMMANDS`` maps each command
name to its handler method.
"""

import logging
from typing import Any, ClassVar

from ._host import ExecutorHost
from ._shared import get_model_manager

logger = logging.getLogger(__name__)


class ModelCommands(ExecutorHost):
    """Model management commands (``models_*``)."""

    COMMANDS: ClassVar[dict[str, str]] = {
        # Model management commands
        "models_list": "_handle_models_list",
        "models_download": "_handle_models_download",
        "models_delete": "_handle_models_delete",
        "models_status": "_handle_models_status",
        "models_disk_usage": "_handle_models_disk_usage",
    }

    # =========================================================================
    # Model Management Handlers
    # =========================================================================

    def _handle_models_list(self) -> dict[str, Any]:
        """List all available models with their download status.

        Returns:
            Dictionary with:
                - success: True
                - models: List of model info dictionaries with:
                    - id: Model identifier
                    - name: Human-readable name
                    - type: Model type (sam3, clip, easyocr)
                    - description: Model description
                    - size_bytes: Model size in bytes
                    - available: Whether model is downloaded
        """
        try:
            manager = get_model_manager()
            models = manager.list_models()
            return {"success": True, "models": models}
        except Exception as e:
            logger.exception(f"Failed to list models: {e}")
            return {"success": False, "error": str(e)}

    def _handle_models_download(self, params: dict[str, Any]) -> dict[str, Any]:
        """Download a model.

        Note: This is a synchronous download. For large models, progress
        events are emitted during download.

        Args:
            params:
                - model_id: Model identifier (e.g., "sam3", "clip_vit_b32")
                - force: Re-download even if already available (default: False)

        Returns:
            Dictionary with:
                - success: Whether download succeeded
                - path: Path to the downloaded model
                - error: Error message (if failed)
        """
        try:
            model_id = params.get("model_id", "")
            force = params.get("force", False)

            if not model_id:
                return {"success": False, "error": "model_id is required"}

            manager = get_model_manager()

            # Progress callback that emits events (use emit_event_wrapper for string event types)
            def on_progress(progress: int) -> None:
                self.event_manager.emit_event_wrapper(
                    "model_download_progress",
                    {
                        "model_id": model_id,
                        "progress": progress,
                    },
                )

            path = manager.download(model_id, progress_callback=on_progress, force=force)

            return {
                "success": True,
                "path": str(path),
                "model_id": model_id,
            }

        except Exception as e:
            logger.exception(f"Failed to download model {params.get('model_id')}: {e}")
            return {"success": False, "error": str(e)}

    def _handle_models_delete(self, params: dict[str, Any]) -> dict[str, Any]:
        """Delete a downloaded model.

        Args:
            params:
                - model_id: Model identifier

        Returns:
            Dictionary with:
                - success: Whether deletion succeeded
                - deleted: True if model was deleted
                - error: Error message (if failed)
        """
        try:
            model_id = params.get("model_id", "")

            if not model_id:
                return {"success": False, "error": "model_id is required"}

            manager = get_model_manager()
            deleted = manager.delete(model_id)

            return {
                "success": True,
                "deleted": deleted,
                "model_id": model_id,
            }

        except Exception as e:
            logger.exception(f"Failed to delete model {params.get('model_id')}: {e}")
            return {"success": False, "error": str(e)}

    def _handle_models_status(self, params: dict[str, Any]) -> dict[str, Any]:
        """Get status of a specific model.

        Args:
            params:
                - model_id: Model identifier

        Returns:
            Dictionary with:
                - success: True
                - model_id: Model identifier
                - available: Whether model is downloaded
                - path: Path to model (if available)
                - info: Model info (name, type, size, etc.)
        """
        try:
            model_id = params.get("model_id", "")

            if not model_id:
                return {"success": False, "error": "model_id is required"}

            manager = get_model_manager()
            available = manager.is_available(model_id)
            path = manager.get_model_path(model_id)
            info = manager.get_model_info(model_id)

            return {
                "success": True,
                "model_id": model_id,
                "available": available,
                "path": str(path) if path else None,
                "info": (
                    {
                        "name": info.name,
                        "type": info.model_type.value,
                        "description": info.description,
                        "size_bytes": info.size_bytes,
                    }
                    if info
                    else None
                ),
            }

        except Exception as e:
            logger.exception(f"Failed to get model status {params.get('model_id')}: {e}")
            return {"success": False, "error": str(e)}

    def _handle_models_disk_usage(self) -> dict[str, Any]:
        """Get disk usage for all downloaded models.

        Returns:
            Dictionary with:
                - success: True
                - total_bytes: Total disk usage in bytes
                - models: Dictionary of model_id -> size in bytes
                - models_dir: Path to models directory
        """
        try:
            manager = get_model_manager()
            usage = manager.get_disk_usage()

            return {
                "success": True,
                **usage,
            }

        except Exception as e:
            logger.exception(f"Failed to get disk usage: {e}")
            return {"success": False, "error": str(e)}
