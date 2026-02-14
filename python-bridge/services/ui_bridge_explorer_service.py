"""
UI Bridge Explorer Service for qontinui-runner.

This service wraps the qontinui library's UIBridgeExplorer
to provide UI Bridge exploration through the runner with progress streaming.
"""

import asyncio
import hashlib
import logging
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)

# Try to import the UI Bridge explorer from qontinui library
try:
    from qontinui.discovery.target_connection import create_connection
    from qontinui.discovery.ui_bridge_explorer import (
        ExplorationConfig,
        ExplorationResult,
        UIBridgeExplorer,
    )

    HAS_UI_BRIDGE_EXPLORER = True
except ImportError as e:
    logger.warning(f"UI Bridge explorer not available: {e}")
    HAS_UI_BRIDGE_EXPLORER = False
    UIBridgeExplorer = None  # type: ignore[assignment, misc]
    ExplorationResult = None  # type: ignore[assignment, misc]
    ExplorationConfig = None  # type: ignore[assignment, misc]
    create_connection = None  # type: ignore[assignment, misc]


@dataclass
class CurrentElementInfo:
    """Information about the element currently being explored."""

    id: str
    label: str
    action: str


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
    # Enhanced progress tracking
    phase: str = "connecting"  # connecting, discovering, exploring, analyzing, complete, stopped
    current_depth: int = 0
    max_depth: int = 2
    states_found: int = 0
    errors_count: int = 0
    start_time: float = field(default_factory=time.time)
    current_element_info: CurrentElementInfo | None = None


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

    def _parse_phase_from_message(self, message: str) -> tuple[str, int]:
        """Parse the exploration phase and depth from a progress message.

        Returns:
            Tuple of (phase, depth)
        """
        message_lower = message.lower()

        if (
            "connecting" in message_lower
            or "initializing" in message_lower
            or "starting capture session" in message_lower
        ):
            return "connecting", 0
        elif "capturing initial state" in message_lower or (
            "found" in message_lower and "starting exploration" in message_lower
        ):
            return "discovering", 0
        elif "exploring depth" in message_lower:
            # Extract depth from message like "Exploring depth 2: 15 elements to check"
            import re

            match = re.search(r"depth\s*(\d+)", message_lower)
            if match:
                return "exploring", int(match.group(1))
            return "exploring", 0
        elif "exploring:" in message_lower:
            return "exploring", 0
        elif "exporting capture session" in message_lower:
            return "analyzing", 0
        elif "complete" in message_lower or "finished" in message_lower:
            return "complete", 0
        elif "stopping" in message_lower:
            return "stopped", 0

        return "exploring", 0

    def _parse_current_element_info(
        self, message: str, element_id: str | None
    ) -> CurrentElementInfo | None:
        """Parse current element info from progress message.

        Returns:
            CurrentElementInfo or None if not exploring an element
        """
        if not element_id:
            return None

        # Try to extract element name and action from message
        # Message format is usually "Exploring: <element_name>..."
        label = element_id
        action = "click"

        if message.startswith("Exploring:"):
            # Extract label from "Exploring: Button Label..."
            label_part = message[len("Exploring:") :].strip()
            if label_part.endswith("..."):
                label_part = label_part[:-3]
            if label_part:
                label = label_part

        return CurrentElementInfo(id=element_id, label=label, action=action)

    def _emit_detailed_progress(self, job: UIBridgeExplorationJob) -> None:
        """Emit a detailed progress event with all tracking information."""
        max_elements = job.config.get("max_total_elements", 100)
        elapsed_ms = int((time.time() - job.start_time) * 1000)

        # Estimate remaining time based on current progress
        estimated_remaining_ms = -1
        if job.elements_explored > 0 and max_elements > 0:
            progress_ratio = job.elements_explored / max_elements
            if progress_ratio > 0 and progress_ratio < 1:
                estimated_total_ms = elapsed_ms / progress_ratio
                estimated_remaining_ms = int(estimated_total_ms - elapsed_ms)

        # Calculate remaining elements
        elements_remaining = max(0, max_elements - job.elements_explored)

        self._emit_event(
            "ui_bridge_exploration_progress",
            {
                "job_id": job.job_id,
                "phase": job.phase,
                "currentDepth": job.current_depth,
                "maxDepth": job.max_depth,
                "elementsDiscovered": job.elements_discovered,
                "elementsExplored": job.elements_explored,
                "elementsRemaining": elements_remaining,
                "currentElement": (
                    {
                        "id": job.current_element_info.id,
                        "label": job.current_element_info.label,
                        "action": job.current_element_info.action,
                    }
                    if job.current_element_info
                    else None
                ),
                "statesFound": job.states_found,
                "errorsCount": job.errors_count,
                "elapsedTimeMs": elapsed_ms,
                "estimatedRemainingMs": estimated_remaining_ms,
                "message": job.progress_message,
                # Legacy fields for backward compatibility
                "percent": job.progress_percent,
                "current_element": job.current_element,
            },
        )

    async def _run_exploration(self, job: UIBridgeExplorationJob) -> None:
        """Run the UI Bridge exploration."""
        job.status = "running"
        job.progress_message = "Initializing..."
        job.progress_percent = 0
        job.phase = "connecting"
        job.start_time = time.time()

        self._emit_event(
            "ui_bridge_exploration_started",
            {
                "job_id": job.job_id,
                "connection_url": job.connection_url,
                "target_type": job.target_type,
            },
        )

        # Emit initial progress
        self._emit_detailed_progress(job)

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

            job.max_depth = exploration_config.max_depth

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
                    job.phase = "stopped"
                    job.progress_message = "Stopping..."
                    self._emit_detailed_progress(job)
                    return False

                job.progress_message = message
                job.elements_discovered = elements_discovered
                job.elements_explored = elements_explored
                job.current_element = current_element

                # Parse phase and depth from message
                phase, depth = self._parse_phase_from_message(message)
                job.phase = phase
                job.current_depth = depth

                # Parse current element info
                job.current_element_info = self._parse_current_element_info(
                    message, current_element
                )

                # Calculate progress percentage
                max_elements = exploration_config.max_total_elements
                if max_elements > 0:
                    job.progress_percent = min(int((elements_explored / max_elements) * 100), 99)

                # Emit detailed progress
                self._emit_detailed_progress(job)
                return True

            explorer = UIBridgeExplorer(
                config=exploration_config,
                on_progress=on_progress,
            )

            # Connect to target
            job.phase = "connecting"
            job.progress_message = "Connecting..."
            self._emit_detailed_progress(job)
            await explorer.connect()

            # Run exploration
            try:
                result = await explorer.explore()
            finally:
                await explorer.disconnect()

            # Store result
            job.result = result
            job.status = "completed"
            job.phase = "complete"
            job.progress_message = "Complete"
            job.progress_percent = 100
            job.elements_discovered = result.elements_discovered
            job.elements_explored = result.elements_explored
            job.errors_count = len(result.errors)
            job.states_found = (
                len(result.state_discovery_result.states) if result.state_discovery_result else 0
            )

            # Emit final progress
            self._emit_detailed_progress(job)

            self._emit_event(
                "ui_bridge_exploration_completed",
                {
                    "job_id": job.job_id,
                    "elements_discovered": result.elements_discovered,
                    "elements_explored": result.elements_explored,
                    "render_logs_count": len(result.render_logs),
                    "states_found": job.states_found,
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
            job.errors_count += 1

            # Emit final progress with error
            self._emit_detailed_progress(job)

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

        def compute_snapshot_hash(snapshot: Any) -> str | None:
            """Compute a short hash from snapshot element IDs for state comparison."""
            if not snapshot:
                return None
            try:
                element_ids = sorted(e.id for e in snapshot.elements)
                return hashlib.md5("|".join(element_ids).encode()).hexdigest()[:8]
            except Exception:
                return None

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
                    # Enhanced fields for transition discovery
                    "parent_step_id": s.parent_step_id,
                    "action_result": (
                        {
                            "response_time_ms": s.action_result.response_time_ms,
                            "new_elements": s.action_result.new_elements,
                            "removed_elements": s.action_result.removed_elements,
                        }
                        if s.action_result
                        else None
                    ),
                    "snapshot_before_hash": compute_snapshot_hash(s.snapshot_before),
                    "snapshot_after_hash": compute_snapshot_hash(s.snapshot_after),
                    "elements_before": (
                        [e.id for e in s.snapshot_before.elements] if s.snapshot_before else []
                    ),
                    "elements_after": (
                        [e.id for e in s.snapshot_after.elements] if s.snapshot_after else []
                    ),
                }
                for s in result.steps
            ],
            "render_logs": [
                {
                    "id": log.get("id", ""),
                    "type": log.get("type", "dom_snapshot"),
                    "timestamp": log.get("timestamp", ""),
                    "url": log.get("snapshot", {}).get("url", ""),
                    "elements_count": log.get("elements_count", 0),
                    "snapshot": log.get("snapshot", {}),
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
