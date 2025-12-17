"""Unified Data Collector Service.

This module implements the UnifiedDataCollector class, which follows the Single Responsibility
Principle (SRP) by focusing solely on collecting and buffering execution data during action
execution, then creating immutable ActionExecutionRecord objects.

The service handles:
- Thread-safe data collection during action execution
- Buffering runtime data (text typed, matches, clicks, transitions, conditions)
- Capturing state snapshots before/after execution
- Integrating with ScreenshotService for screenshot storage
- Creating immutable ActionExecutionRecord objects on completion
- Clearing buffer after record creation

Design Principles:
- Single Responsibility: Only handles data collection and record creation
- Thread-Safe: Uses threading.RLock for concurrent access protection
- Immutable Output: Creates frozen ActionExecutionRecord dataclasses
- Clear Lifecycle: start_action() -> record_*() -> create_record() -> clear buffer
"""

from __future__ import annotations

import threading
from collections.abc import Callable
from typing import Any

from models.action_execution_record import ActionExecutionRecord


class UnifiedDataCollector:
    """Thread-safe service for collecting action execution data.

    This service follows the Single Responsibility Principle by exclusively handling
    the collection and aggregation of execution data. It does not handle:
    - Action execution (delegated to ActionExecutor)
    - Event emission (delegated to EventEmitter)
    - Screenshot capture (delegated to ScreenshotService, though it stores references)

    Responsibilities:
    1. Buffer runtime data during action execution (thread-safe)
    2. Capture state snapshots before/after from state_memory
    3. Store screenshot references from ScreenshotService
    4. Create immutable ActionExecutionRecord on completion
    5. Clear buffer after record creation

    Lifecycle:
        1. start_action(action_id, ...) - Initialize buffer for new action
        2. record_text_typed(text) - Record typed text
        3. record_match_result(...) - Record image recognition results
        4. record_click(...) - Record click events
        5. record_transition(...) - Record state transitions
        6. record_condition(...) - Record condition evaluations
        7. create_record(...) - Create immutable record and clear buffer
        8. (Repeat for next action)

    Thread Safety:
        All methods are protected by a reentrant lock (RLock) to ensure thread-safe
        access to the internal buffer. This protects against race conditions during
        data collection for a single action.

        IMPORTANT: The collector maintains a single buffer and is designed for
        sequential action execution. Only one action should be active at a time.
        For concurrent action execution, create separate collector instances.

    Example:
        >>> from pathlib import Path
        >>> collector = UnifiedDataCollector(state_memory, screenshot_service)
        >>> collector.start_action("action-1", None, "workflow-1", 0)
        >>> collector.record_text_typed("hello@example.com")
        >>> collector.record_click(100, 200, "left", "button")
        >>> record = collector.create_record(
        ...     action_type="CLICK",
        ...     metadata={"success": True}
        ... )
        >>> print(record.text_typed)
        hello@example.com
    """

    def __init__(
        self,
        state_memory: Any,
        screenshot_service: Any | None = None,
        record_created_callback: Callable[[Any], None] | None = None,
    ) -> None:
        """Initialize the UnifiedDataCollector.

        Args:
            state_memory: State memory service for capturing active state snapshots.
                         Must provide get_active_state_names() method that returns
                         a list of active state names.
            screenshot_service: Optional ScreenshotService for storing screenshots.
                              If None, screenshot functionality is disabled.
            record_created_callback: Optional callback function called when a record
                                   is created. Signature: callback(record: ActionExecutionRecord)

        Example:
            >>> collector = UnifiedDataCollector(state_memory, screenshot_service)
            >>> # Collector is ready to buffer execution data
        """
        self.state_memory = state_memory
        self.screenshot_service = screenshot_service
        self.record_created_callback = record_created_callback
        self._lock = threading.RLock()

        # Runtime buffer (cleared after each action)
        self._current_action_id: str | None = None
        self._current_parent_action_id: str | None = None
        self._current_workflow_id: str | None = None
        self._current_nesting_level: int = 0

        # Captured data
        self._active_states_before: list[str] = []
        self._text_typed: str | None = None
        self._match_results: list[dict[str, Any]] = []
        self._clicks: list[dict[str, Any]] = []
        self._transitions: list[dict[str, Any]] = []
        self._conditions: list[dict[str, Any]] = []
        self._screenshot_paths: list[str] = []
        self._workflow_name: str | None = None
        self._workflow_status: str | None = None

    def start_action(
        self, action_id: str, parent_action_id: str | None, workflow_id: str, nesting_level: int
    ) -> None:
        """Start collecting data for a new action.

        This method initializes the buffer for a new action and captures the
        "before" state snapshot from state_memory.

        Thread-safe: Protected by internal lock.

        Args:
            action_id: Unique identifier for the action being executed.
            parent_action_id: ID of parent action (None for top-level actions).
            workflow_id: ID of the workflow containing this action.
            nesting_level: Depth in the execution hierarchy (0 for top-level).

        Example:
            >>> collector.start_action(
            ...     action_id="click-submit-123",
            ...     parent_action_id="form-fill-456",
            ...     workflow_id="login-workflow",
            ...     nesting_level=1
            ... )
            >>> # Buffer is now ready to collect data for this action
        """
        with self._lock:
            # Store action identifiers
            self._current_action_id = action_id
            self._current_parent_action_id = parent_action_id
            self._current_workflow_id = workflow_id
            self._current_nesting_level = nesting_level

            # Capture "before" state snapshot
            if self.state_memory:
                self._active_states_before = self.state_memory.get_active_state_names()
                print(
                    f"[DEBUG] start_action: active_states_before = {self._active_states_before}",
                    flush=True,
                )
            else:
                self._active_states_before = []
                print("[DEBUG] start_action: no state_memory, using empty list", flush=True)

            # Clear runtime buffers
            self._text_typed = None
            self._match_results = []
            self._clicks = []
            self._transitions = []
            self._conditions = []
            self._screenshot_paths = []
            self._workflow_name = None
            self._workflow_status = None

    def record_text_typed(self, text: str) -> None:
        """Record text that was typed during action execution.

        Thread-safe: Protected by internal lock.

        Args:
            text: Text that was typed (e.g., "hello@example.com").

        Example:
            >>> collector.record_text_typed("password123")
            >>> # Text is buffered and will be included in the final record
        """
        with self._lock:
            self._text_typed = text

    def record_match_result(
        self,
        match_summary: dict[str, Any],
        screenshot_data: bytes | None = None,
        debug_visual_data: bytes | None = None,
    ) -> dict[str, str | None]:
        """Record an image recognition match result.

        This method buffers match information and optionally stores screenshots
        through the ScreenshotService if provided.

        Thread-safe: Protected by internal lock.

        Args:
            match_summary: Dictionary containing match information:
                - image_id: Template image identifier
                - confidence: Match confidence (0.0-1.0)
                - threshold: Threshold used for matching
                - found: Whether match succeeded
                - location: Optional match coordinates {"x": int, "y": int}
            screenshot_data: Optional screenshot image data (PNG bytes).
                           Stored via ScreenshotService if available.
            debug_visual_data: Optional debug visualization image data (PNG bytes).
                              Stored via ScreenshotService if available.

        Returns:
            Dictionary containing:
                - screenshot_path: Absolute path to saved screenshot (or None)
                - debug_visual_path: Absolute path to saved debug visual (or None)

        Example:
            >>> result = collector.record_match_result(
            ...     match_summary={
            ...         "image_id": "login_button.png",
            ...         "confidence": 0.95,
            ...         "threshold": 0.8,
            ...         "found": True,
            ...         "location": {"x": 100, "y": 200}
            ...     },
            ...     screenshot_data=png_bytes,
            ...     debug_visual_data=annotated_png_bytes
            ... )
            >>> print(result["screenshot_path"])
            /path/to/storage/screenshots/screenshot-0001.png
        """
        with self._lock:
            # Store screenshot if provided
            screenshot_ref = None
            screenshot_abs_path = None
            if screenshot_data and self.screenshot_service:
                screenshot_ref = self.screenshot_service.store_screenshot(
                    screenshot_data=screenshot_data,
                    action_id=self._current_action_id or "unknown",
                    active_states=self._active_states_before,
                    metadata={
                        "event_type": "match_attempt",
                        "image_id": match_summary.get("image_id", "unknown"),
                        "found": match_summary.get("found", False),
                    },
                )
                if screenshot_ref:
                    self._screenshot_paths.append(screenshot_ref)
                    # Convert relative path to absolute path for frontend
                    screenshot_abs_path = str(self.screenshot_service.storage_dir / screenshot_ref)

            # Store debug visual if provided
            debug_visual_ref = None
            debug_visual_abs_path = None
            if debug_visual_data and self.screenshot_service:
                debug_visual_ref = self.screenshot_service.store_debug_visual(
                    debug_image_data=debug_visual_data,
                    action_id=self._current_action_id or "unknown",
                    match_info=match_summary,
                )
                if debug_visual_ref:
                    # Convert relative path to absolute path for frontend
                    debug_visual_abs_path = str(
                        self.screenshot_service.storage_dir / debug_visual_ref
                    )
                # Debug visuals are stored separately and not included in screenshot_paths

            # Build match result entry
            match_entry = {
                **match_summary,
                "screenshot_ref": screenshot_ref,
                "debug_visual_ref": debug_visual_ref,
            }

            self._match_results.append(match_entry)

            # Return absolute paths for immediate use (e.g., by EventTranslator)
            return {
                "screenshot_path": screenshot_abs_path,
                "debug_visual_path": debug_visual_abs_path,
            }

    def record_click(self, x: int, y: int, button: str, target_type: str = "unknown") -> None:
        """Record a click event.

        Thread-safe: Protected by internal lock.

        Args:
            x: X coordinate of the click.
            y: Y coordinate of the click.
            button: Button clicked ("left", "right", "middle").
            target_type: Type of target clicked (e.g., "button", "link", "element").

        Example:
            >>> collector.record_click(
            ...     x=250,
            ...     y=450,
            ...     button="left",
            ...     target_type="submit_button"
            ... )
            >>> # Click event is buffered
        """
        with self._lock:
            click_entry = {
                "x": x,
                "y": y,
                "button": button,
                "target_type": target_type,
            }
            self._clicks.append(click_entry)

    def record_transition(self, transition_data: dict[str, Any]) -> None:
        """Record a state transition.

        Thread-safe: Protected by internal lock.

        Args:
            transition_data: Dictionary containing transition information:
                - from_state: Source state name
                - to_state: Target state name
                - transition_type: Type of transition
                - success: Whether transition succeeded

        Example:
            >>> collector.record_transition({
            ...     "from_state": "Login",
            ...     "to_state": "Dashboard",
            ...     "transition_type": "navigation",
            ...     "success": True
            ... })
            >>> # Transition is buffered
        """
        with self._lock:
            self._transitions.append(transition_data)

    def record_condition(self, passed: bool, branch: str) -> None:
        """Record a condition evaluation result.

        Thread-safe: Protected by internal lock.

        Args:
            passed: Whether the condition evaluated to True.
            branch: Branch identifier or description.

        Example:
            >>> collector.record_condition(
            ...     passed=True,
            ...     branch="login_successful"
            ... )
            >>> # Condition result is buffered
        """
        with self._lock:
            condition_entry = {
                "passed": passed,
                "branch": branch,
            }
            self._conditions.append(condition_entry)

    def record_workflow_execution(self, workflow_name: str, workflow_status: str) -> None:
        """Record workflow execution information for RUN_WORKFLOW actions.

        Thread-safe: Protected by internal lock.

        Args:
            workflow_name: Name of the workflow that was executed.
            workflow_status: Status of workflow execution ("success" or "failed").

        Example:
            >>> collector.record_workflow_execution(
            ...     workflow_name="Login Flow",
            ...     workflow_status="success"
            ... )
            >>> # Workflow info is buffered and will be included in runtime metadata
        """
        with self._lock:
            self._workflow_name = workflow_name
            self._workflow_status = workflow_status

    def create_record(
        self,
        action_type: str = "",
        config: dict[str, Any] | None = None,
        start_time: float | None = None,
        end_time: float | None = None,
        success: bool = True,
        error_message: str | None = None,
        retry_count: int = 0,
        metadata: dict[str, Any] | None = None,
    ) -> ActionExecutionRecord:
        """Create an immutable ActionExecutionRecord from buffered data.

        This method:
        1. Captures the "after" state snapshot
        2. Extracts runtime data from buffers
        3. Creates a frozen ActionExecutionRecord with all collected data
        4. Clears the runtime buffer for the next action

        Thread-safe: Protected by internal lock.

        Args:
            action_type: Type of action executed (e.g., "CLICK", "TYPE", "FIND").
            config: Action configuration dictionary (coordinates, text, etc.).
            start_time: Unix timestamp when action started.
            end_time: Unix timestamp when action completed (None if still running).
            success: Whether the action completed successfully.
            error_message: Error message if action failed (None if successful).
            retry_count: Number of retry attempts made (0 if first attempt succeeded).
            metadata: Optional additional metadata to include in the record.

        Returns:
            Immutable ActionExecutionRecord containing all collected data.

        Example:
            >>> import time
            >>> start = time.time()
            >>> collector.start_action("action-1", None, "workflow-1", 0)
            >>> collector.record_text_typed("hello@example.com")
            >>> collector.record_click(100, 200, "left", "button")
            >>> end = time.time()
            >>> record = collector.create_record(
            ...     action_type="CLICK",
            ...     config={"x": 100, "y": 200},
            ...     start_time=start,
            ...     end_time=end,
            ...     success=True
            ... )
            >>> print(record.action_id)
            action-1
            >>> print(record.typed_text)
            hello@example.com
            >>> # Buffer is now cleared and ready for next action

        Note:
            After calling this method, the buffer is cleared. You must call
            start_action() again before recording more data.
        """
        with self._lock:
            # Capture "after" state snapshot
            if self.state_memory:
                active_states_after = self.state_memory.get_active_state_names()
                print(
                    f"[DEBUG] create_record: active_states_after = {active_states_after}",
                    flush=True,
                )
            else:
                active_states_after = []
                print("[DEBUG] create_record: no state_memory, using empty list", flush=True)

            # Convert state lists to frozen sets
            state_before_set = frozenset(self._active_states_before)
            state_after_set = frozenset(active_states_after)

            # Calculate duration in milliseconds if both times provided
            duration_ms = None
            if end_time is not None and start_time is not None:
                duration_ms = (end_time - start_time) * 1000.0

            # Extract runtime data from buffers
            # For clicks, use the first one (or None)
            clicked_location = None
            click_button = None
            click_target_type = None
            if self._clicks:
                first_click = self._clicks[0]
                clicked_location = (first_click["x"], first_click["y"])
                click_button = first_click.get("button")
                click_target_type = first_click.get("target_type")

            # For matches, create lightweight summary
            match_summary = None
            screenshot_ref = None
            debug_visual_ref = None
            if self._match_results:
                first_match = self._match_results[0]
                match_summary = {
                    "image_id": first_match.get("image_id"),
                    "confidence": first_match.get("confidence"),
                    "threshold": first_match.get("threshold"),
                    "found": first_match.get("found"),
                    "location": first_match.get("location"),
                }
                screenshot_ref = first_match.get("screenshot_ref")
                debug_visual_ref = first_match.get("debug_visual_ref")

            # For transitions, use the first one
            transition_data = None
            if self._transitions:
                transition_data = self._transitions[0]

            # For conditions, use the first one
            condition_result = None
            branch_taken = None
            if self._conditions:
                first_condition = self._conditions[0]
                condition_result = first_condition.get("passed")
                branch_taken = first_condition.get("branch")

            # Create record with new structure
            record = ActionExecutionRecord(
                action_id=self._current_action_id or "",
                action_type=action_type,
                config=config or {},
                start_time=start_time or 0.0,
                end_time=end_time,
                duration_ms=duration_ms,
                active_states_before=state_before_set,
                active_states_after=state_after_set,
                success=success,
                error_message=error_message,
                retry_count=retry_count,
                typed_text=self._text_typed,
                match_summary=match_summary,
                clicked_location=clicked_location,
                click_button=click_button,
                click_target_type=click_target_type,
                transition_data=transition_data,
                condition_result=condition_result,
                branch_taken=branch_taken,
                workflow_name=self._workflow_name,
                workflow_status=self._workflow_status,
                parent_action_id=self._current_parent_action_id,
                workflow_id=self._current_workflow_id,
                nesting_level=self._current_nesting_level,
                screenshot_reference=screenshot_ref,
                visual_debug_reference=debug_visual_ref,
                metadata=metadata or {},
            )

            # Clear runtime buffer
            self._clear_runtime_buffer()

            # Notify callback if provided
            if self.record_created_callback:
                try:
                    self.record_created_callback(record)
                except Exception as e:
                    # Log but don't raise - callbacks shouldn't break execution
                    import logging

                    logging.getLogger(__name__).error(
                        f"Error in record_created_callback: {e}", exc_info=True
                    )

            return record

    def _clear_runtime_buffer(self) -> None:
        """Clear the runtime buffer after record creation.

        This is an internal method called by create_record() to reset the buffer
        state for the next action.

        Thread-safe: Must be called within a lock context.

        Note:
            This method assumes the caller holds self._lock.
        """
        # Clear action identifiers
        self._current_action_id = None
        self._current_parent_action_id = None
        self._current_workflow_id = None
        self._current_nesting_level = 0

        # Clear captured data
        self._active_states_before = []
        self._text_typed = None
        self._match_results = []
        self._clicks = []
        self._transitions = []
        self._conditions = []
        self._screenshot_paths = []
        self._workflow_name = None
        self._workflow_status = None
