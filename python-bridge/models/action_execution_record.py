"""ActionExecutionRecord data model for unified action execution tracking.

This module provides the single source of truth for action execution data,
supporting multiple use cases: real-time logging, mock run recording, and
tree view generation.

Following the Single Responsibility Principle, this module:
- Defines the immutable ActionExecutionRecord data structure
- Provides computed properties for derived data
- Provides conversion methods for different consumers
- Does NOT collect data or manage execution state
"""

from dataclasses import dataclass, field
from typing import Optional, Dict, Any, FrozenSet
import time


@dataclass(frozen=True)
class ActionExecutionRecord:
    """Immutable record of a single action execution.

    This is the single source of truth for all action execution data.
    All fields are captured during execution and become read-only after
    construction. Derived data is provided via properties.

    Identity Fields:
        action_id: Unique identifier for this action execution
        action_type: Type of action (e.g., "CLICK", "TYPE", "GO_TO_STATE")

    Configuration:
        config: Action configuration dictionary (coordinates, text, etc.)

    Timing:
        start_time: Unix timestamp when action started
        end_time: Unix timestamp when action completed (None if still running)
        duration_ms: Total execution time in milliseconds (None if still running)

    State Context:
        active_states_before: Set of active state names before action execution
        active_states_after: Set of active state names after action execution

    Outcome:
        success: Whether the action completed successfully
        error_message: Error message if action failed (None if successful)
        retry_count: Number of retry attempts made (0 if first attempt succeeded)

    Runtime Data (Action-Specific):
        typed_text: Text that was typed (for TYPE actions)
        match_summary: Lightweight match result data (for image-based actions)
        clicked_location: Click coordinates as (x, y) tuple (for CLICK actions)
        click_button: Mouse button used ("left", "right", "middle")
        click_target_type: Target type ("coordinates", "image", "text")
        transition_data: State transition information (for GO_TO_STATE)
        condition_result: Condition evaluation result (for IF actions)
        branch_taken: Which branch was executed ("then", "else", None)

    Hierarchy:
        parent_action_id: ID of parent action/workflow (None for root)
        workflow_id: ID of containing workflow
        nesting_level: Depth in execution tree (0 for root)

    Visual References:
        screenshot_reference: Path to screenshot file (if captured)
        visual_debug_reference: Path to annotated debug image (if generated)

    Metadata:
        metadata: Additional arbitrary data for extensibility

    Properties (Computed):
        duration_seconds: Duration in seconds (from duration_ms)
        state_changed: Whether active states changed during action
        states_activated: States that were activated during action
        states_deactivated: States that were deactivated during action
    """

    # Identity
    action_id: str
    action_type: str

    # Configuration
    config: Dict[str, Any] = field(default_factory=dict)

    # Timing
    start_time: float = field(default_factory=time.time)
    end_time: Optional[float] = None
    duration_ms: Optional[float] = None

    # State context
    active_states_before: FrozenSet[str] = field(default_factory=frozenset)
    active_states_after: FrozenSet[str] = field(default_factory=frozenset)

    # Outcome
    success: bool = True
    error_message: Optional[str] = None
    retry_count: int = 0

    # Runtime data (action-specific)
    typed_text: Optional[str] = None
    match_summary: Optional[Dict[str, Any]] = None
    clicked_location: Optional[tuple[int, int]] = None
    click_button: Optional[str] = None
    click_target_type: Optional[str] = None
    transition_data: Optional[Dict[str, Any]] = None
    condition_result: Optional[bool] = None
    branch_taken: Optional[str] = None
    workflow_name: Optional[str] = None  # For RUN_WORKFLOW actions
    workflow_status: Optional[str] = None  # For RUN_WORKFLOW actions

    # Hierarchy
    parent_action_id: Optional[str] = None
    workflow_id: Optional[str] = None
    nesting_level: int = 0

    # Visual references
    screenshot_reference: Optional[str] = None
    visual_debug_reference: Optional[str] = None

    # Extensibility
    metadata: Dict[str, Any] = field(default_factory=dict)

    @property
    def duration_seconds(self) -> Optional[float]:
        """Calculate duration in seconds.

        Returns:
            Duration in seconds if action has completed, None otherwise
        """
        if self.duration_ms is None:
            return None
        return self.duration_ms / 1000.0

    @property
    def state_changed(self) -> bool:
        """Check if active states changed during action execution.

        Returns:
            True if the set of active states is different before vs after
        """
        return self.active_states_before != self.active_states_after

    @property
    def states_activated(self) -> FrozenSet[str]:
        """Get states that were activated during action execution.

        Returns:
            Set of state names that were not active before but are active after
        """
        return self.active_states_after - self.active_states_before

    @property
    def states_deactivated(self) -> FrozenSet[str]:
        """Get states that were deactivated during action execution.

        Returns:
            Set of state names that were active before but not active after
        """
        return self.active_states_before - self.active_states_after

    def to_tree_event_data(self) -> Dict[str, Any]:
        """Convert to tree event format for frontend consumption.

        This method transforms the record into the format expected by
        the frontend tree view components. It includes all necessary
        data for display, hierarchy, and user interaction.

        Returns:
            Dictionary containing:
                - id: Action ID
                - type: "action"
                - name: Action type (display name)
                - timestamp: Start time (Unix timestamp)
                - end_timestamp: End time (Unix timestamp or None)
                - duration: Duration in seconds (or None)
                - parent_id: Parent action/workflow ID
                - nesting_level: Depth in execution tree
                - workflow_name: Name of containing workflow
                - status: "success", "failed", or "running"
                - error: Error message (if failed)
                - metadata: Combined metadata including runtime data
                - state_context: State change information
        """
        return {
            "id": self.action_id,
            "type": "action",
            "name": self.action_type,
            "timestamp": self.start_time,
            "end_timestamp": self.end_time,
            "duration": self.duration_seconds,
            "parent_id": self.parent_action_id,
            "nesting_level": self.nesting_level,
            "workflow_name": self.workflow_id,
            "status": "success" if self.success else "failed" if self.end_time else "running",
            "error": self.error_message,
            "metadata": {
                **self.metadata,
                "config": self.config,
                "runtime": self._format_runtime_for_display(),
                "screenshot_reference": self.screenshot_reference,
                "visual_debug_reference": self.visual_debug_reference,
                "retry_count": self.retry_count,
            },
            "state_context": {
                "active_before": sorted(self.active_states_before),
                "active_after": sorted(self.active_states_after),
                "state_changed": self.state_changed,
                "states_activated": sorted(self.states_activated),
                "states_deactivated": sorted(self.states_deactivated),
            },
        }

    def _format_runtime_for_display(self) -> Dict[str, Any]:
        """Format runtime data for display in frontend.

        Collects all action-specific runtime data into a clean dictionary,
        omitting None values to keep the payload small.

        Returns:
            Dictionary containing only non-None runtime data fields
        """
        runtime: Dict[str, Any] = {}

        if self.typed_text is not None:
            runtime["typed_text"] = self.typed_text

        if self.match_summary is not None:
            runtime["match_summary"] = self.match_summary

        if self.clicked_location is not None:
            runtime["clicked_location"] = {
                "x": self.clicked_location[0],
                "y": self.clicked_location[1],
            }

        if self.click_button is not None:
            runtime["click_button"] = self.click_button

        if self.click_target_type is not None:
            runtime["click_target_type"] = self.click_target_type

        if self.transition_data is not None:
            runtime["transition_data"] = self.transition_data

        if self.condition_result is not None:
            runtime["condition_result"] = self.condition_result

        if self.branch_taken is not None:
            runtime["branch_taken"] = self.branch_taken

        if self.workflow_name is not None:
            runtime["workflow_name"] = self.workflow_name

        if self.workflow_status is not None:
            runtime["workflow_status"] = self.workflow_status

        return runtime
