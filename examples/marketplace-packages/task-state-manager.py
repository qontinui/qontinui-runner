"""
Task State Manager - Community Package for Multi-Session Task Continuation

This package provides helper functions for managing task state across multiple
Claude sessions. Use these functions to track progress on large tasks that may
require multiple sessions to complete.

IMPORTANT: This package returns data structures. The qontinui runtime handles
file I/O operations (reading/writing task-state.json).

Usage in automation workflows:
1. Call create_task_state() at the start of a large task
2. Call update_progress() as you complete items
3. Call mark_needs_continuation() before session ends if incomplete
4. Call mark_completed() when the task is done

The CONTINUE_TASK action reads task-state.json and uses continue_prompt to
resume work in a new Claude session.

Author: Qontinui
License: MIT
Version: 1.0.0
Category: utilities
Tags: task-management, continuation, multi-session, ai-automation
"""

import json
import uuid
from dataclasses import asdict, dataclass, field
from datetime import datetime
from typing import Any


@dataclass
class TaskProgress:
    """Progress tracking for a task."""

    completed: int = 0
    total: int = 0
    percentage: float = 0.0

    def update(self, completed: int, total: int | None = None) -> None:
        """Update progress counts."""
        self.completed = completed
        if total is not None:
            self.total = total
        if self.total > 0:
            self.percentage = round((self.completed / self.total) * 100, 2)


@dataclass
class TaskState:
    """
    Task state for multi-session continuation.

    This dataclass represents the complete state of a continuable task.
    It's serialized to task-state.json for persistence between sessions.
    """

    task_id: str
    status: str  # "in_progress", "needs_continuation", "completed"
    description: str
    progress: TaskProgress
    continue_prompt: str
    files_modified: list[str] = field(default_factory=list)
    errors_encountered: list[str] = field(default_factory=list)
    started_at: str = ""
    updated_at: str = ""
    iteration: int = 1

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "task_id": self.task_id,
            "status": self.status,
            "description": self.description,
            "progress": {
                "completed": self.progress.completed,
                "total": self.progress.total,
                "percentage": self.progress.percentage,
            },
            "continue_prompt": self.continue_prompt,
            "files_modified": self.files_modified,
            "errors_encountered": self.errors_encountered,
            "started_at": self.started_at,
            "updated_at": self.updated_at,
            "iteration": self.iteration,
        }

    def to_json(self, indent: int = 2) -> str:
        """Serialize to JSON string."""
        return json.dumps(self.to_dict(), indent=indent)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "TaskState":
        """Create TaskState from dictionary."""
        progress_data = data.get("progress", {})
        progress = TaskProgress(
            completed=progress_data.get("completed", 0),
            total=progress_data.get("total", 0),
            percentage=progress_data.get("percentage", 0.0),
        )
        return cls(
            task_id=data.get("task_id", str(uuid.uuid4())),
            status=data.get("status", "in_progress"),
            description=data.get("description", ""),
            progress=progress,
            continue_prompt=data.get("continue_prompt", ""),
            files_modified=data.get("files_modified", []),
            errors_encountered=data.get("errors_encountered", []),
            started_at=data.get("started_at", ""),
            updated_at=data.get("updated_at", ""),
            iteration=data.get("iteration", 1),
        )

    @classmethod
    def from_json(cls, json_str: str) -> "TaskState":
        """Create TaskState from JSON string."""
        return cls.from_dict(json.loads(json_str))


def create_task_state(
    description: str,
    total_items: int,
    initial_prompt: str | None = None,
) -> TaskState:
    """
    Create a new task state for a large task.

    Args:
        description: Human-readable description of the task
        total_items: Estimated total number of items to process
        initial_prompt: Optional initial continuation prompt

    Returns:
        TaskState object ready to be serialized

    Example:
        state = create_task_state(
            description="Fix 5000 linting errors in the qontinui codebase",
            total_items=5000,
            initial_prompt="Run 'ruff check .' and fix errors file by file"
        )
        # Write state.to_json() to task-state.json
    """
    now = datetime.utcnow().isoformat() + "Z"
    return TaskState(
        task_id=str(uuid.uuid4()),
        status="in_progress",
        description=description,
        progress=TaskProgress(completed=0, total=total_items, percentage=0.0),
        continue_prompt=initial_prompt or f"Continue: {description}",
        files_modified=[],
        errors_encountered=[],
        started_at=now,
        updated_at=now,
        iteration=1,
    )


def update_progress(
    state: TaskState,
    completed: int,
    total: int | None = None,
    files_modified: list[str] | None = None,
    continue_prompt: str | None = None,
) -> TaskState:
    """
    Update task progress.

    Args:
        state: Current task state
        completed: Number of items completed
        total: Optional updated total (if estimate changed)
        files_modified: Optional list of newly modified files to add
        continue_prompt: Optional updated continuation prompt

    Returns:
        Updated TaskState object

    Example:
        state = update_progress(
            state,
            completed=1247,
            files_modified=["src/vision/matcher.py"],
            continue_prompt="Continue from src/state_machine/detector.py"
        )
    """
    state.progress.update(completed, total)
    state.updated_at = datetime.utcnow().isoformat() + "Z"

    if files_modified:
        # Add new files, avoid duplicates
        for f in files_modified:
            if f not in state.files_modified:
                state.files_modified.append(f)

    if continue_prompt:
        state.continue_prompt = continue_prompt

    return state


def mark_needs_continuation(
    state: TaskState,
    continue_prompt: str,
    increment_iteration: bool = True,
) -> TaskState:
    """
    Mark task as needing continuation in a new session.

    Call this before your session ends if the task is incomplete.
    The CONTINUE_TASK action will read this state and prompt Claude
    with the continue_prompt.

    Args:
        state: Current task state
        continue_prompt: Detailed prompt for the next session
        increment_iteration: Whether to increment the iteration counter

    Returns:
        Updated TaskState with status="needs_continuation"

    Example:
        state = mark_needs_continuation(
            state,
            continue_prompt='''Continue fixing type errors.

PROGRESS: 1247/5000 errors fixed (24.9%)
LAST FILE: src/vision/pattern_matcher.py (partially done)
NEXT FILES: src/vision/template_matcher.py, src/state_machine/detector.py

Run: mypy src/ | head -100
Fix errors file by file, updating task state after each file.
'''
        )
    """
    state.status = "needs_continuation"
    state.continue_prompt = continue_prompt
    state.updated_at = datetime.utcnow().isoformat() + "Z"

    if increment_iteration:
        state.iteration += 1

    return state


def mark_completed(state: TaskState, summary: str | None = None) -> TaskState:
    """
    Mark task as completed.

    Call this when the task is fully done. The CONTINUE_TASK action
    will see status="completed" and stop the continuation loop.

    Args:
        state: Current task state
        summary: Optional completion summary for the continue_prompt

    Returns:
        Updated TaskState with status="completed"

    Example:
        state = mark_completed(
            state,
            summary="All 5000 linting errors have been fixed. Tests pass."
        )
    """
    state.status = "completed"
    state.progress.completed = state.progress.total
    state.progress.percentage = 100.0
    state.updated_at = datetime.utcnow().isoformat() + "Z"

    if summary:
        state.continue_prompt = f"COMPLETED: {summary}"

    return state


def add_error(state: TaskState, error: str) -> TaskState:
    """
    Record an error encountered during task execution.

    Args:
        state: Current task state
        error: Error message or description

    Returns:
        Updated TaskState with error added

    Example:
        state = add_error(state, "Failed to fix src/broken.py: syntax error")
    """
    state.errors_encountered.append(error)
    state.updated_at = datetime.utcnow().isoformat() + "Z"
    return state


def build_continuation_prompt(
    state: TaskState,
    last_file: str | None = None,
    next_files: list[str] | None = None,
    additional_context: str | None = None,
) -> str:
    """
    Build a detailed continuation prompt from task state.

    This helper generates a well-structured prompt that gives the
    next Claude session full context about the task.

    Args:
        state: Current task state
        last_file: The last file being worked on
        next_files: List of files to work on next
        additional_context: Any additional context or instructions

    Returns:
        Formatted continuation prompt string

    Example:
        prompt = build_continuation_prompt(
            state,
            last_file="src/vision/matcher.py",
            next_files=["src/state_machine/detector.py", "src/hal/capture.py"],
            additional_context="Focus on return type annotations"
        )
        state = mark_needs_continuation(state, continue_prompt=prompt)
    """
    lines = [
        f"Continue: {state.description}",
        "",
        f"PROGRESS: {state.progress.completed}/{state.progress.total} ({state.progress.percentage}%)",
    ]

    if last_file:
        lines.append(f"LAST FILE: {last_file}")

    if next_files:
        lines.append(f"NEXT FILES: {', '.join(next_files[:5])}")
        if len(next_files) > 5:
            lines.append(f"  ... and {len(next_files) - 5} more")

    if state.files_modified:
        lines.append(f"FILES MODIFIED THIS SESSION: {len(state.files_modified)}")

    if state.errors_encountered:
        lines.append(f"ERRORS ENCOUNTERED: {len(state.errors_encountered)}")
        for err in state.errors_encountered[-3:]:  # Last 3 errors
            lines.append(f"  - {err}")

    if additional_context:
        lines.append("")
        lines.append(additional_context)

    lines.append("")
    lines.append(f"Session: {state.iteration}")
    lines.append("Update task-state.json with progress after each file.")

    return "\n".join(lines)


def _handle_create(kwargs: dict[str, Any]) -> dict[str, Any]:
    """Handle create action."""
    state = create_task_state(
        description=kwargs.get("description", "Task"),
        total_items=kwargs.get("total_items", 0),
        initial_prompt=kwargs.get("initial_prompt"),
    )
    return {
        "success": True,
        "state_json": state.to_json(),
        "message": f"Created task: {state.task_id}",
    }


def _handle_update(state: TaskState, kwargs: dict[str, Any]) -> dict[str, Any]:
    """Handle update action."""
    state = update_progress(
        state,
        completed=kwargs.get("completed", state.progress.completed),
        total=kwargs.get("total"),
        files_modified=kwargs.get("files_modified"),
        continue_prompt=kwargs.get("continue_prompt"),
    )
    return {
        "success": True,
        "state_json": state.to_json(),
        "message": f"Progress: {state.progress.completed}/{state.progress.total}",
    }


def _handle_needs_continuation(state: TaskState, kwargs: dict[str, Any]) -> dict[str, Any]:
    """Handle needs_continuation action."""
    state = mark_needs_continuation(
        state,
        continue_prompt=kwargs.get("continue_prompt", state.continue_prompt),
        increment_iteration=kwargs.get("increment_iteration", True),
    )
    return {
        "success": True,
        "state_json": state.to_json(),
        "message": f"Marked for continuation (iteration {state.iteration})",
    }


def _handle_completed(state: TaskState, kwargs: dict[str, Any]) -> dict[str, Any]:
    """Handle completed action."""
    state = mark_completed(state, summary=kwargs.get("summary"))
    return {
        "success": True,
        "state_json": state.to_json(),
        "message": "Task completed",
    }


def _handle_add_error(state: TaskState, kwargs: dict[str, Any]) -> dict[str, Any]:
    """Handle add_error action."""
    state = add_error(state, error=kwargs.get("error", "Unknown error"))
    return {
        "success": True,
        "state_json": state.to_json(),
        "message": f"Error recorded ({len(state.errors_encountered)} total)",
    }


def _handle_build_prompt(state: TaskState, kwargs: dict[str, Any]) -> dict[str, Any]:
    """Handle build_prompt action."""
    prompt = build_continuation_prompt(
        state,
        last_file=kwargs.get("last_file"),
        next_files=kwargs.get("next_files"),
        additional_context=kwargs.get("additional_context"),
    )
    return {
        "success": True,
        "state_json": state.to_json(),
        "prompt": prompt,
        "message": "Prompt built",
    }


# Action handlers map
_ACTION_HANDLERS = {
    "update": _handle_update,
    "needs_continuation": _handle_needs_continuation,
    "completed": _handle_completed,
    "add_error": _handle_add_error,
    "build_prompt": _handle_build_prompt,
}


# Main entry point function (required by marketplace)
def task_state_manager(
    action: str,
    state_json: str | None = None,
    **kwargs: Any,
) -> dict[str, Any]:
    """
    Main entry point for task state management.

    This function provides a unified interface for all task state operations.
    It's designed to be called from qontinui automation workflows.

    Args:
        action: The action to perform:
            - "create": Create new task state
            - "update": Update progress
            - "needs_continuation": Mark for continuation
            - "completed": Mark as completed
            - "add_error": Add an error
            - "build_prompt": Build continuation prompt
        state_json: Current state as JSON (required for all except "create")
        **kwargs: Action-specific arguments

    Returns:
        Dict with:
            - "success": bool
            - "state_json": Updated state as JSON string
            - "message": Status message

    Example (from automation):
        result = task_state_manager(
            action="create",
            description="Fix all linting errors",
            total_items=5000
        )
        # result["state_json"] contains the new state
    """
    # Handle create separately (doesn't need existing state)
    if action == "create":
        return _handle_create(kwargs)

    # All other actions require existing state
    if not state_json:
        return {
            "success": False,
            "state_json": None,
            "message": "state_json is required for this action",
        }

    # Parse state and dispatch to handler
    state = TaskState.from_json(state_json)
    handler = _ACTION_HANDLERS.get(action)

    if handler is None:
        return {
            "success": False,
            "state_json": state_json,
            "message": f"Unknown action: {action}",
        }

    return handler(state, kwargs)
