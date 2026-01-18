"""
Playwright State Collector Service for qontinui-runner.

This service wraps the qontinui library's SafePlaywrightStateCollector
to provide Playwright-based web extraction through the runner.
"""

import asyncio
import base64
import io
import logging
import uuid
from dataclasses import dataclass, field
from typing import Any

import numpy as np
from PIL import Image

logger = logging.getLogger(__name__)

# Try to import the Playwright collector from qontinui library
try:
    from qontinui.extraction.web.playwright_collector import (
        SafePlaywrightStateCollector,
        CollectionResult,
    )
    from qontinui.extraction.web.safety import SafetyConfig, ActionRisk

    HAS_PLAYWRIGHT_COLLECTOR = True
except ImportError as e:
    logger.warning(f"Playwright collector not available: {e}")
    HAS_PLAYWRIGHT_COLLECTOR = False
    SafePlaywrightStateCollector = None
    CollectionResult = None
    SafetyConfig = None
    ActionRisk = None


@dataclass
class PlaywrightJob:
    """Represents a running or completed Playwright collection job."""

    job_id: str
    status: str  # "pending", "running", "completed", "failed"
    url: str
    config: dict[str, Any]
    result: Any | None = None
    error: str | None = None
    progress_message: str = ""
    progress_percent: int = 0


class PlaywrightCollectorService:
    """
    Service for Playwright-based state collection.

    This service manages Playwright extraction jobs, tracking their status
    and storing results for retrieval.
    """

    def __init__(self, event_manager=None):
        """
        Initialize the Playwright collector service.

        Args:
            event_manager: EventManager for emitting events to Rust bridge.
        """
        self.event_manager = event_manager
        self._jobs: dict[str, PlaywrightJob] = {}
        self._current_job_id: str | None = None
        self._current_task: asyncio.Task | None = None
        self._async_loop: asyncio.AbstractEventLoop | None = None

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

    def _numpy_to_base64(self, image: np.ndarray) -> str:
        """Convert numpy array image to base64 string."""
        if image is None:
            return ""
        try:
            pil_image = Image.fromarray(image)
            buffer = io.BytesIO()
            pil_image.save(buffer, format="PNG")
            return base64.b64encode(buffer.getvalue()).decode("utf-8")
        except Exception as e:
            logger.warning(f"Failed to convert image to base64: {e}")
            return ""

    def start_collection(self, config: dict[str, Any]) -> dict[str, Any]:
        """
        Start a Playwright state collection job.

        Args:
            config: Collection configuration with:
                - url: Target URL (required)
                - max_depth: Maximum navigation depth (default: 2)
                - max_elements_per_page: Max elements per page (default: 50)
                - max_risk_level: "safe", "caution", or "dry_run" (default: "safe")
                - dry_run: Whether to skip clicking (default: False)
                - verify_extractions: Whether to verify with pattern matching (default: True)
                - verification_threshold: Similarity threshold (default: 0.85)
                - additional_blocked_keywords: Extra keywords to block
                - additional_safe_keywords: Extra keywords to allow
                - blocked_selectors: CSS selectors to skip

        Returns:
            Dict with success status and job_id.
        """
        if not HAS_PLAYWRIGHT_COLLECTOR:
            return {
                "success": False,
                "error": "Playwright collector not available. Install qontinui library.",
            }

        if self._current_job_id is not None:
            job = self._jobs.get(self._current_job_id)
            if job and job.status == "running":
                return {
                    "success": False,
                    "error": f"Collection already in progress: {self._current_job_id}",
                }

        url = config.get("url")
        if not url:
            return {"success": False, "error": "URL is required"}

        # Create job
        job_id = str(uuid.uuid4())
        job = PlaywrightJob(
            job_id=job_id,
            status="pending",
            url=url,
            config=config,
        )
        self._jobs[job_id] = job
        self._current_job_id = job_id

        logger.info(f"Starting Playwright collection job {job_id} for {url}")

        # Start collection in background
        loop = self._get_or_create_async_loop()

        def run_collection():
            try:
                asyncio.set_event_loop(loop)
                loop.run_until_complete(self._run_collection(job))
            except Exception as e:
                logger.error(f"Collection task error: {e}", exc_info=True)
                job.status = "failed"
                job.error = str(e)

        import threading

        thread = threading.Thread(target=run_collection, daemon=True)
        thread.start()

        return {
            "success": True,
            "job_id": job_id,
        }

    async def _run_collection(self, job: PlaywrightJob) -> None:
        """Run the Playwright collection."""
        job.status = "running"
        job.progress_message = "Initializing..."
        job.progress_percent = 0

        self._emit_event(
            "playwright_collection_started",
            {"job_id": job.job_id, "url": job.url},
        )

        try:
            config = job.config

            # Build safety config
            safety_kwargs = {}

            # Handle risk level
            risk_level = config.get("max_risk_level", "safe")
            if risk_level == "dry_run":
                safety_kwargs["dry_run"] = True
                safety_kwargs["max_risk"] = ActionRisk.SAFE
            elif risk_level == "caution":
                safety_kwargs["max_risk"] = ActionRisk.CAUTION
            else:
                safety_kwargs["max_risk"] = ActionRisk.SAFE

            # Handle dry_run flag (explicit override)
            if config.get("dry_run", False):
                safety_kwargs["dry_run"] = True

            # Handle blocked keywords
            if config.get("additional_blocked_keywords"):
                safety_kwargs["additional_dangerous_keywords"] = config[
                    "additional_blocked_keywords"
                ]

            # Handle safe keywords
            if config.get("additional_safe_keywords"):
                safety_kwargs["additional_safe_keywords"] = config[
                    "additional_safe_keywords"
                ]

            # Handle blocked selectors
            if config.get("blocked_selectors"):
                safety_kwargs["blocked_selectors"] = config["blocked_selectors"]

            safety_config = SafetyConfig(**safety_kwargs)

            # Create collector
            collector = SafePlaywrightStateCollector(
                safety_config=safety_config,
                verify_extractions=config.get("verify_extractions", True),
                verification_threshold=config.get("verification_threshold", 0.85),
            )

            # Progress callback
            def on_progress(message: str, percent: int):
                job.progress_message = message
                job.progress_percent = percent
                self._emit_event(
                    "playwright_collection_progress",
                    {
                        "job_id": job.job_id,
                        "message": message,
                        "percent": percent,
                    },
                )

            # Run collection
            result = await collector.collect(
                url=job.url,
                max_depth=config.get("max_depth", 2),
                max_elements_per_page=config.get("max_elements_per_page", 50),
                on_progress=on_progress,
            )

            # Store result
            job.result = result
            job.status = "completed"
            job.progress_message = "Complete"
            job.progress_percent = 100

            self._emit_event(
                "playwright_collection_completed",
                {
                    "job_id": job.job_id,
                    "metrics": result.metrics,
                    "clickables_count": len(result.clickables),
                    "pages_visited": len(result.pages_visited),
                },
            )

            logger.info(
                f"Playwright collection {job.job_id} completed: "
                f"{len(result.clickables)} clickables, {len(result.pages_visited)} pages"
            )

        except Exception as e:
            logger.error(f"Playwright collection failed: {e}", exc_info=True)
            job.status = "failed"
            job.error = str(e)
            job.progress_message = f"Failed: {e}"

            self._emit_event(
                "playwright_collection_failed",
                {
                    "job_id": job.job_id,
                    "error": str(e),
                },
            )

    def get_job_status(self, job_id: str | None = None) -> dict[str, Any]:
        """
        Get the status of a collection job.

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
            "url": job.url,
            "progress_message": job.progress_message,
            "progress_percent": job.progress_percent,
            "error": job.error,
            "has_results": job.result is not None,
        }

    def get_results(self, job_id: str | None = None) -> dict[str, Any]:
        """
        Get the results of a completed collection job.

        Args:
            job_id: Job ID to get results for (uses current if not specified)

        Returns:
            Dict with collection results.
        """
        jid = job_id or self._current_job_id
        if not jid or jid not in self._jobs:
            return {"success": False, "error": "Job not found"}

        job = self._jobs[jid]
        if job.status != "completed":
            return {
                "success": False,
                "error": f"Job not completed (status: {job.status})",
            }

        if job.result is None:
            return {"success": False, "error": "No results available"}

        # Serialize results
        result: CollectionResult = job.result
        clickables = []

        for c in result.clickables:
            clickable_data = {
                "element_id": c.element_id,
                "selector": c.selector,
                "tag_name": c.tag_name,
                "text": c.text,
                "aria_label": c.aria_label,
                "bounding_box": c.bounding_box,
                "risk_level": c.risk_level,
                "risk_reason": c.risk_reason,
                "was_clicked": c.was_clicked,
                "verification_confidence": c.verification_confidence,
                "verified": c.verified,
                "error": c.error,
            }

            # Include screenshots as base64
            if c.screenshot is not None:
                clickable_data["screenshot"] = self._numpy_to_base64(c.screenshot)

            if c.page_screenshot_before is not None:
                clickable_data["page_screenshot_before"] = self._numpy_to_base64(
                    c.page_screenshot_before
                )

            if c.page_screenshot_after is not None:
                clickable_data["page_screenshot_after"] = self._numpy_to_base64(
                    c.page_screenshot_after
                )

            clickables.append(clickable_data)

        return {
            "success": True,
            "job_id": job.job_id,
            "url": job.url,
            "clickables": clickables,
            "skipped_dangerous": result.skipped_dangerous,
            "metrics": result.metrics,
            "pages_visited": result.pages_visited,
            "errors": result.errors,
        }

    def stop_collection(self) -> dict[str, Any]:
        """
        Stop the current collection job.

        Returns:
            Dict with success status.
        """
        if self._current_job_id is None:
            return {"success": False, "error": "No collection in progress"}

        job = self._jobs.get(self._current_job_id)
        if job and job.status == "running":
            job.status = "failed"
            job.error = "Cancelled by user"
            job.progress_message = "Cancelled"

            self._emit_event(
                "playwright_collection_cancelled",
                {"job_id": job.job_id},
            )

            logger.info(f"Playwright collection {job.job_id} cancelled")

        return {"success": True}

    def clear_job(self, job_id: str) -> dict[str, Any]:
        """
        Clear a completed job from memory.

        Args:
            job_id: Job ID to clear

        Returns:
            Dict with success status.
        """
        if job_id in self._jobs:
            del self._jobs[job_id]
            if self._current_job_id == job_id:
                self._current_job_id = None
            return {"success": True}
        return {"success": False, "error": "Job not found"}
