"""Test Results Service for submitting workflow execution data to qontinui-web backend.

This service handles HTTP submission of test run data to the backend's testing API:
- POST /api/v1/testing/runs - Create a test run
- POST /api/v1/testing/runs/{run_id}/transitions - Report transitions
- PUT /api/v1/testing/runs/{run_id}/complete - Complete run

The service is designed to be called during and after workflow execution
to report test results to the QA Testing dashboard in qontinui-web.
"""

import logging
import os
import platform
import socket
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any
from uuid import UUID

import httpx
from qontinui_schemas.common import utc_now

logger = logging.getLogger(__name__)


@dataclass
class TransitionResult:
    """Result of a single transition execution."""

    sequence_number: int
    from_state: str
    to_state: str
    transition_name: str
    status: str  # "success", "failed", "timeout", "skipped"
    started_at: datetime
    completed_at: datetime
    duration_ms: int
    error_message: str | None = None
    error_type: str | None = None
    screenshot_id: UUID | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to API payload format."""
        return {
            "sequence_number": self.sequence_number,
            "from_state": self.from_state,
            "to_state": self.to_state,
            "transition_name": self.transition_name,
            "status": self.status,
            "started_at": self.started_at.isoformat(),
            "completed_at": self.completed_at.isoformat(),
            "duration_ms": self.duration_ms,
            "error_message": self.error_message,
            "error_type": self.error_type,
            "screenshot_id": str(self.screenshot_id) if self.screenshot_id else None,
            "metadata": self.metadata,
        }


@dataclass
class ActionData:
    """Data from an action execution for historical indexing."""

    action_id: str
    action_type: str
    success: bool
    pattern_id: str | None = None
    pattern_name: str | None = None
    active_states: list[str] = field(default_factory=list)
    match_count: int | None = None
    best_match_score: float | None = None
    match_x: int | None = None
    match_y: int | None = None
    match_width: int | None = None
    match_height: int | None = None
    duration_ms: int | None = None
    result_data: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to API payload format."""
        return {
            "action_id": self.action_id,
            "pattern_id": self.pattern_id,
            "pattern_name": self.pattern_name,
            "action_type": self.action_type,
            "active_states": self.active_states,
            "success": self.success,
            "match_count": self.match_count,
            "best_match_score": self.best_match_score,
            "match_x": self.match_x,
            "match_y": self.match_y,
            "match_width": self.match_width,
            "match_height": self.match_height,
            "duration_ms": self.duration_ms,
            "result_data": self.result_data,
        }


@dataclass
class DeficiencyReport:
    """Report of a deficiency found during test execution."""

    title: str
    description: str
    severity: str  # "critical", "high", "medium", "low", "informational"
    deficiency_type: str  # "functional_bug", "ui_issue", "performance", etc.
    state: str | None = None
    transition_sequence_number: int | None = None
    screenshot_ids: list[UUID] = field(default_factory=list)
    reproduction_steps: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to API payload format."""
        return {
            "title": self.title,
            "description": self.description,
            "severity": self.severity,
            "deficiency_type": self.deficiency_type,
            "state": self.state,
            "transition_sequence_number": self.transition_sequence_number,
            "screenshot_ids": [str(sid) for sid in self.screenshot_ids],
            "reproduction_steps": self.reproduction_steps,
            "metadata": self.metadata,
        }


class TestResultsService:
    """Service for submitting test results to the qontinui-web backend.

    Usage:
        service = TestResultsService(
            api_url="http://localhost:8000",
            access_token="jwt-token",
            project_id="project-uuid"
        )

        # Start a test run
        run_id = await service.start_test_run(
            run_name="Nightly Regression",
            workflow_name="Login Flow",
            workflow_config={...}
        )

        # Report transitions as they happen
        await service.report_transition(run_id, TransitionResult(...))

        # Complete the test run
        await service.complete_test_run(run_id, status="completed", metrics={...})
    """

    def __init__(
        self,
        api_url: str,
        access_token: str,
        project_id: str,
        runner_version: str = "0.1.0",
    ):
        """Initialize the test results service.

        Args:
            api_url: Base URL for the qontinui-web backend API (e.g., http://localhost:8000)
            access_token: JWT access token for authentication
            project_id: UUID of the project to submit results for
            runner_version: Version of the qontinui-runner
        """
        self.api_url = api_url.rstrip("/")
        self.access_token = access_token
        self.project_id = project_id
        self.runner_version = runner_version

        # HTTP client with auth header
        self.client = httpx.AsyncClient(
            base_url=self.api_url,
            headers={
                "Authorization": f"Bearer {access_token}",
                "Content-Type": "application/json",
            },
            timeout=30.0,
        )

        # Collect runner metadata
        self.runner_metadata = self._get_runner_metadata()

        # Track pending items for batch submission
        self._pending_transitions: list[TransitionResult] = []
        self._pending_deficiencies: list[DeficiencyReport] = []
        self._pending_actions: list[ActionData] = []

    def _get_runner_metadata(self) -> dict[str, Any]:
        """Collect metadata about the runner environment."""
        try:
            screen_resolution = self._get_screen_resolution()
        except Exception:
            screen_resolution = "unknown"

        return {
            "runner_version": self.runner_version,
            "os": f"{platform.system()} {platform.release()}",
            "hostname": socket.gethostname(),
            "screen_resolution": screen_resolution,
            "python_version": platform.python_version(),
        }

    def _get_screen_resolution(self) -> str:
        """Get the current screen resolution."""
        try:
            from screeninfo import get_monitors

            monitors = get_monitors()
            if monitors:
                return f"{monitors[0].width}x{monitors[0].height}"
        except ImportError:
            pass

        # Fallback for Windows
        if platform.system() == "Windows":
            try:
                import ctypes

                user32 = ctypes.windll.user32
                return f"{user32.GetSystemMetrics(0)}x{user32.GetSystemMetrics(1)}"
            except Exception:
                pass

        return "unknown"

    async def start_test_run(
        self,
        run_name: str,
        workflow_name: str,
        workflow_config: dict[str, Any],
        description: str | None = None,
    ) -> str:
        """Create a new test run in the backend.

        Args:
            run_name: Name for the test run (e.g., "Nightly Regression - 2024-12-24")
            workflow_name: Name of the workflow being tested
            workflow_config: Snapshot of the workflow configuration
            description: Optional description of the test run

        Returns:
            The run_id (UUID) of the created test run

        Raises:
            httpx.HTTPError: If the API request fails
        """
        # Count states and transitions from config
        states = workflow_config.get("states", [])
        transitions = []
        for state in states:
            transitions.extend(state.get("transitions", []))

        workflow_metadata = {
            "workflow_name": workflow_name,
            "total_states": len(states),
            "total_transitions": len(transitions),
        }

        payload = {
            "project_id": self.project_id,
            "run_name": run_name,
            "description": description,
            "runner_metadata": self.runner_metadata,
            "workflow_metadata": workflow_metadata,
            "configuration_snapshot": workflow_config,
        }

        logger.info(f"Creating test run: {run_name}")
        response = await self.client.post("/api/v1/testing/runs", json=payload)
        response.raise_for_status()

        data = response.json()
        run_id = data["run_id"]
        logger.info(f"Created test run: {run_id}")
        return run_id

    async def report_transition(
        self, run_id: str, transition: TransitionResult, batch: bool = True
    ) -> None:
        """Report a transition execution result.

        Args:
            run_id: The test run ID
            transition: The transition result to report
            batch: If True, queue for batch submission. If False, submit immediately.
        """
        if batch:
            self._pending_transitions.append(transition)
            # Auto-submit when batch reaches 50 (API limit)
            if len(self._pending_transitions) >= 50:
                await self.flush_transitions(run_id)
        else:
            await self._submit_transitions(run_id, [transition])

    async def flush_transitions(self, run_id: str) -> None:
        """Submit all pending transitions to the backend."""
        if not self._pending_transitions:
            return

        transitions = self._pending_transitions.copy()
        self._pending_transitions.clear()
        await self._submit_transitions(run_id, transitions)

    async def _submit_transitions(self, run_id: str, transitions: list[TransitionResult]) -> None:
        """Submit transitions to the backend API."""
        if not transitions:
            return

        payload = {"transitions": [t.to_dict() for t in transitions]}

        logger.info(f"Submitting {len(transitions)} transitions for run {run_id}")
        response = await self.client.post(
            f"/api/v1/testing/runs/{run_id}/transitions", json=payload
        )
        response.raise_for_status()
        logger.debug(f"Transitions submitted: {response.json()}")

    async def report_deficiency(
        self, run_id: str, deficiency: DeficiencyReport, batch: bool = True
    ) -> None:
        """Report a deficiency found during testing.

        Args:
            run_id: The test run ID
            deficiency: The deficiency to report
            batch: If True, queue for batch submission. If False, submit immediately.
        """
        if batch:
            self._pending_deficiencies.append(deficiency)
            # Auto-submit when batch reaches 20 (API limit)
            if len(self._pending_deficiencies) >= 20:
                await self.flush_deficiencies(run_id)
        else:
            await self._submit_deficiencies(run_id, [deficiency])

    async def flush_deficiencies(self, run_id: str) -> None:
        """Submit all pending deficiencies to the backend."""
        if not self._pending_deficiencies:
            return

        deficiencies = self._pending_deficiencies.copy()
        self._pending_deficiencies.clear()
        await self._submit_deficiencies(run_id, deficiencies)

    async def _submit_deficiencies(self, run_id: str, deficiencies: list[DeficiencyReport]) -> None:
        """Submit deficiencies to the backend API."""
        if not deficiencies:
            return

        payload = {"deficiencies": [d.to_dict() for d in deficiencies]}

        logger.info(f"Submitting {len(deficiencies)} deficiencies for run {run_id}")
        response = await self.client.post(
            f"/api/v1/testing/runs/{run_id}/deficiencies", json=payload
        )
        response.raise_for_status()
        logger.debug(f"Deficiencies submitted: {response.json()}")

    async def complete_test_run(
        self,
        run_id: str,
        status: str,
        final_metrics: dict[str, Any],
        summary: str | None = None,
    ) -> None:
        """Mark a test run as complete.

        Args:
            run_id: The test run ID
            status: Final status ("completed", "failed", "timeout", "aborted", "crashed")
            final_metrics: Final test metrics (transitions executed, coverage, etc.)
            summary: Optional summary text
        """
        # Flush any pending data first
        await self.flush_transitions(run_id)
        await self.flush_deficiencies(run_id)
        await self.flush_actions(run_id)

        payload = {
            "status": status,
            "ended_at": utc_now().isoformat(),
            "final_metrics": final_metrics,
            "summary": summary,
        }

        logger.info(f"Completing test run {run_id} with status: {status}")
        response = await self.client.put(f"/api/v1/testing/runs/{run_id}/complete", json=payload)
        response.raise_for_status()
        logger.info(f"Test run completed: {response.json()}")

    # ========================================================================
    # Action Data Submission (for Config Testing / Historical Data)
    # ========================================================================

    async def report_action(self, run_id: str, action: ActionData, batch: bool = True) -> None:
        """Report an action execution for historical indexing.

        This data is used by Config Testing to provide historical results
        for integration testing (mock mode).

        Args:
            run_id: The test run ID (used for grouping/context)
            action: The action execution data
            batch: If True, queue for batch submission. If False, submit immediately.
        """
        if batch:
            self._pending_actions.append(action)
            # Auto-submit when batch reaches 50
            if len(self._pending_actions) >= 50:
                await self.flush_actions(run_id)
        else:
            await self._submit_actions(run_id, [action])

    async def flush_actions(self, run_id: str) -> None:
        """Submit all pending action data to the backend."""
        if not self._pending_actions:
            return

        actions = self._pending_actions.copy()
        self._pending_actions.clear()
        await self._submit_actions(run_id, actions)

    async def _submit_actions(self, run_id: str, actions: list[ActionData]) -> None:
        """Submit action data to the backend for historical indexing."""
        if not actions:
            return

        payload = {
            "run_id": run_id,
            "project_id": self.project_id,
            "workflow_id": None,  # Can be set from context if needed
            "actions": [a.to_dict() for a in actions],
        }

        logger.info(f"Submitting {len(actions)} actions for historical indexing")
        response = await self.client.post("/api/v1/testing/historical/actions", json=payload)
        response.raise_for_status()
        logger.debug(f"Actions indexed: {response.json()}")

    async def close(self) -> None:
        """Close the HTTP client."""
        await self.client.aclose()

    async def __aenter__(self) -> "TestResultsService":
        """Async context manager entry."""
        return self

    async def __aexit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        """Async context manager exit."""
        await self.close()


# Convenience function for creating service from environment
def create_test_results_service(
    access_token: str,
    project_id: str,
    api_url: str | None = None,
) -> TestResultsService:
    """Create a TestResultsService from environment or explicit config.

    Args:
        access_token: JWT access token
        project_id: Project UUID
        api_url: Backend API URL (defaults to QONTINUI_API_URL env or localhost:8000)

    Returns:
        Configured TestResultsService instance
    """
    if api_url is None:
        api_url = os.environ.get("QONTINUI_API_URL", "http://localhost:8000")

    return TestResultsService(
        api_url=api_url,
        access_token=access_token,
        project_id=project_id,
    )
