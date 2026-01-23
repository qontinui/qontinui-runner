"""
UI Bridge Explorer Service for qontinui-runner.

This service wraps the qontinui library's UIBridgeExplorer
to provide UI Bridge exploration through the runner with progress streaming.
"""

import asyncio
import logging
import threading
import uuid
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)

# Try to import the UI Bridge explorer from qontinui library
try:
    from qontinui.discovery.ui_bridge_explorer import (
        UIBridgeExplorer,
        ExplorationResult,
        ExplorationConfig,
    )
    from qontinui.discovery.target_connection import create_connection

    HAS_UI_BRIDGE_EXPLORER = True
except ImportError as e:
    logger.warning(f"UI Bridge explorer not available: {e}")
    HAS_UI_BRIDGE_EXPLORER = False
    UIBridgeExplorer = None  # type: ignore[assignment, misc]
    ExplorationResult = None  # type: ignore[assignment, misc]
    ExplorationConfig = None  # type: ignore[assignment, misc]
    create_connection = None  # type: ignore[assignment, misc]


@dataclass
class UIBridgeExplorationJob:
    """Represents a running or completed UI Bridge exploration job."""

    job_id: str
    status: str  # "pending", "running", "completed", "failed"
    connection_url: str
    target_type: str
    config: dict[str, Any]
    result: Any | None = None
    error: str | None = None
    progress_message: str = ""
    progress_percent: int = 0
    elements_discovered: int = 0
    elements_explored: int = 0
    current_element: str | None = None


class UIBridgeExplorerService:
    """
    Service for UI Bridge exploration with progress streaming.

    This service manages UI Bridge exploration jobs, tracking their status
    and storing results for retrieval.
    """

    def __init__(self, event_manager: Any = None):
        """
        Initialize the UI Bridge explorer service.

        Args:
            event_manager: EventManager for emitting events to Rust bridge.
        """
        self.event_manager = event_manager
        self._jobs: dict[str, UIBridgeExplorationJob] = {}
        self._current_job_id: str | None = None
        self._async_loop: asyncio.AbstractEventLoop | None = None
        self._stop_requested: bool = False

    def _get_or_create_async_loop(self) -> asyncio.AbstractEventLoop:
        """Get or create an async event loop."""
        if self._async_loop is None or self._async_loop.is_closed():
            try:
                self._async_loop = asyncio.get_event_loop()
            except RuntimeError:
                self._async_loop = asyncio.new_event_loop()
                asyncio.set_event_loop(self._async_loop)
        return self._async_loop

    def _emit_event(self, event_type: str, data: dict[str, Any]) -> None:
        """Emit an event to the Rust bridge."""
        if self.event_manager:
            self.event_manager.emit_event_wrapper(event_type, data)

    def start_exploration(self, config: dict[str, Any]) -> dict[str, Any]:
        """
        Start a UI Bridge exploration job.

        Args:
            config: Exploration configuration with:
                - connection_url: Target URL (required)
                - target_type: "web", "desktop", or "mobile" (default: "web")
                - max_depth: Maximum navigation depth (default: 2)
                - max_elements_per_page: Max elements per page (default: 20)
                - max_total_elements: Max total elements (default: 100)
                - action_delay_ms: Delay between actions (default: 500)
                - blocked_keywords: Keywords to block
                - safe_keywords: Keywords to allow
                - blocked_selectors: CSS selectors to skip
                - capture_screenshots: Whether to capture screenshots (default: False)
                - run_state_discovery: Run co-occurrence analysis (default: True)

        Returns:
            Dict with success status and job_id.
        """
        if not HAS_UI_BRIDGE_EXPLORER:
            return {
                "success": False,
                "error": "UI Bridge explorer not available. Install qontinui library.",
            }

        if self._current_job_id is not None:
            job = self._jobs.get(self._current_job_id)
            if job and job.status == "running":
                return {
                    "success": False,
                    "error": f"Exploration already in progress: {self._current_job_id}",
                }

        connection_url = config.get("connection_url")
        if not connection_url:
            return {"success": False, "error": "connection_url is required"}

        target_type = config.get("target_type", "web")

        # Create job
        job_id = str(uuid.uuid4())
        job = UIBridgeExplorationJob(
            job_id=job_id,
            status="pending",
            connection_url=connection_url,
            target_type=target_type,
            config=config,
        )
        self._jobs[job_id] = job
        self._current_job_id = job_id
        self._stop_requested = False

        logger.info(
            f"Starting UI Bridge exploration job {job_id} for {connection_url} (type: {target_type})"
        )

        # Start exploration in background thread
        loop = self._get_or_create_async_loop()

        def run_exploration() -> None:
            try:
                asyncio.set_event_loop(loop)
                loop.run_until_complete(self._run_exploration(job))
            except Exception as e:
                logger.error(f"Exploration task error: {e}", exc_info=True)
                job.status = "failed"
                job.error = str(e)

        thread = threading.Thread(target=run_exploration, daemon=True)
        thread.start()

        return {
            "success": True,
            "job_id": job_id,
        }

    async def _run_exploration(self, job: UIBridgeExplorationJob) -> None:
        """Run the UI Bridge exploration."""
        job.status = "running"
        job.progress_message = "Initializing..."
        job.progress_percent = 0

        self._emit_event(
            "ui_bridge_exploration_started",
            {
                "job_id": job.job_id,
                "connection_url": job.connection_url,
                "target_type": job.target_type,
            },
        )

        try:
            config = job.config

            # Build exploration config
            from qontinui.discovery.target_connection import ExplorationConfig

            exploration_config = ExplorationConfig(
                target_type=job.target_type,  # type: ignore[arg-type]
                connection_url=job.connection_url,
                max_depth=config.get("max_depth", 2),
                max_elements_per_page=config.get("max_elements_per_page", 20),
                max_total_elements=config.get("max_total_elements", 100),
                action_delay_ms=config.get("action_delay_ms", 500),
                blocked_keywords=config.get("blocked_keywords", []),
                safe_keywords=config.get("safe_keywords", []),
                blocked_selectors=config.get("blocked_selectors", []),
                capture_screenshots=config.get("capture_screenshots", False),
                record_render_logs=True,
            )

            # Create explorer with progress callback
            from qontinui.discovery.ui_bridge_explorer import UIBridgeExplorer

            def on_progress(
                message: str,
                elements_discovered: int,
                elements_explored: int,
                current_element: str | None = None,
            ) -> bool:
                """Progress callback - returns False to stop exploration."""
                if self._stop_requested:
                    return False

                job.progress_message = message
                job.elements_discovered = elements_discovered
                job.elements_explored = elements_explored
                job.current_element = current_element

                # Calculate progress percentage
                max_elements = exploration_config.max_total_elements
                if max_elements > 0:
                    job.progress_percent = min(int((elements_explored / max_elements) * 100), 99)

                self._emit_event(
                    "ui_bridge_exploration_progress",
                    {
                        "job_id": job.job_id,
                        "message": message,
                        "percent": job.progress_percent,
                        "elements_discovered": elements_discovered,
                        "elements_explored": elements_explored,
                        "current_element": current_element,
                    },
                )
                return True

            explorer = UIBridgeExplorer(
                config=exploration_config,
                on_progress=on_progress,
            )

            # Run exploration
            job.progress_message = "Connecting..."
            result = await explorer.explore()

            # Store result
            job.result = result
            job.status = "completed"
            job.progress_message = "Complete"
            job.progress_percent = 100
            job.elements_discovered = result.elements_discovered
            job.elements_explored = result.elements_explored

            self._emit_event(
                "ui_bridge_exploration_completed",
                {
                    "job_id": job.job_id,
                    "elements_discovered": result.elements_discovered,
                    "elements_explored": result.elements_explored,
                    "render_logs_count": len(result.render_logs),
                    "states_found": (
                        len(result.state_discovery_result.states)
                        if result.state_discovery_result
                        else 0
                    ),
                },
            )

            logger.info(
                f"UI Bridge exploration {job.job_id} completed: "
                f"{result.elements_explored} elements explored"
            )

        except Exception as e:
            logger.error(f"UI Bridge exploration failed: {e}", exc_info=True)
            job.status = "failed"
            job.error = str(e)
            job.progress_message = f"Failed: {e}"

            self._emit_event(
                "ui_bridge_exploration_failed",
                {
                    "job_id": job.job_id,
                    "error": str(e),
                },
            )

    def get_job_status(self, job_id: str | None = None) -> dict[str, Any]:
        """
        Get the status of an exploration job.

        Args:
            job_id: Job ID to check (uses current if not specified)

        Returns:
            Dict with job status information.
        """
        jid = job_id or self._current_job_id
        if not jid or jid not in self._jobs:
            return {
                "success": True,
                "status": "idle",
                "job_id": None,
            }

        job = self._jobs[jid]
        return {
            "success": True,
            "job_id": job.job_id,
            "status": job.status,
            "connection_url": job.connection_url,
            "target_type": job.target_type,
            "progress_message": job.progress_message,
            "progress_percent": job.progress_percent,
            "elements_discovered": job.elements_discovered,
            "elements_explored": job.elements_explored,
            "current_element": job.current_element,
            "error": job.error,
            "has_results": job.result is not None,
        }

    def get_results(self, job_id: str | None = None) -> dict[str, Any]:
        """
        Get the results of a completed exploration job.

        Args:
            job_id: Job ID to get results for (uses current if not specified)

        Returns:
            Dict with exploration results.
        """
        jid = job_id or self._current_job_id
        if not jid or jid not in self._jobs:
            return {"success": False, "error": "No exploration job found"}

        job = self._jobs[jid]

        if job.status == "running":
            return {"success": False, "error": "Exploration still in progress"}

        if job.status == "failed":
            return {"success": False, "error": job.error or "Exploration failed"}

        if job.result is None:
            return {"success": False, "error": "No results available"}

        result = job.result

        # Convert result to dict
        response_data: dict[str, Any] = {
            "exploration_id": result.exploration_id,
            "elements_discovered": result.elements_discovered,
            "elements_explored": result.elements_explored,
            "steps": [
                {
                    "step_id": s.step_id,
                    "timestamp": s.timestamp.isoformat(),
                    "element_id": s.element_id,
                    "action": s.action,
                    "success": s.action_result.success if s.action_result else None,
                    "state_changed": (s.action_result.state_changed if s.action_result else None),
                    "depth": s.depth,
                }
                for s in result.steps
            ],
            "render_logs": [
                {
                    "id": log.id,
                    "timestamp": log.timestamp.isoformat(),
                    "url": log.url,
                    "elements_count": len(log.elements),
                }
                for log in result.render_logs
            ],
            "render_log_count": len(result.render_logs),
            "errors": result.errors,
            "start_time": (result.start_time.isoformat() if result.start_time else None),
            "end_time": result.end_time.isoformat() if result.end_time else None,
        }

        # Include state discovery results if available
        if result.state_discovery_result:
            sdr = result.state_discovery_result
            response_data["state_discovery"] = {
                "states": [
                    {
                        "id": s.id,
                        "name": s.name,
                        "state_image_ids": s.state_image_ids,
                        "screenshot_ids": s.screenshot_ids,
                        "confidence": s.confidence,
                    }
                    for s in sdr.states
                ],
                "elements": [
                    {
                        "id": e.id,
                        "name": e.name,
                        "type": e.type,
                        "render_ids": e.render_ids,
                        "tag_name": e.tag_name,
                        "text_content": e.text_content,
                        "component_name": e.component_name,
                    }
                    for e in sdr.elements
                ],
                "element_to_renders": sdr.element_to_renders,
                "render_count": sdr.render_count,
                "unique_element_count": sdr.unique_element_count,
            }

        return {"success": True, "data": response_data}

    def stop_exploration(self) -> dict[str, Any]:
        """
        Stop the current exploration job.

        Returns:
            Dict with success status.
        """
        if not self._current_job_id:
            return {"success": False, "error": "No exploration in progress"}

        job = self._jobs.get(self._current_job_id)
        if not job:
            return {"success": False, "error": "Job not found"}

        if job.status != "running":
            return {"success": False, "error": f"Job not running (status: {job.status})"}

        self._stop_requested = True
        job.progress_message = "Stopping..."

        self._emit_event(
            "ui_bridge_exploration_stopping",
            {"job_id": job.job_id},
        )

        return {"success": True, "message": "Stop requested"}
