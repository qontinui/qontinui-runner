"""Workflow Context Module.

This module provides a workflow-level context store that persists across actions
during workflow execution. It enables:
- User-defined context values via CAPTURE_CONTEXT action
- Auto-populated execution context (errors, screenshots, etc.)
- Template variable resolution for TRIGGER_AI_ANALYSIS prompts

Following the Single Responsibility Principle, this module:
- Stores and retrieves context values
- Does NOT execute actions or handle templates (see template_resolver.py)
"""

from __future__ import annotations

import json
import threading
from typing import Any


class WorkflowContext:
    """Workflow-level context store that persists across actions.

    This class maintains two separate namespaces:
    - context: User-defined values set via CAPTURE_CONTEXT action
    - execution: Auto-populated values from the execution system

    Thread Safety:
        All methods are protected by a reentrant lock (RLock) to ensure
        thread-safe access during concurrent execution.

    Example:
        >>> ctx = WorkflowContext()
        >>> ctx.set("errorMessage", "TypeError: Cannot read property 'x' of undefined")
        >>> ctx.update_execution_context({"lastError": "Action CLICK failed"})
        >>> ctx.get("errorMessage")
        "TypeError: Cannot read property 'x' of undefined"
        >>> ctx.get_execution("lastError")
        "Action CLICK failed"
    """

    def __init__(self) -> None:
        """Initialize an empty workflow context."""
        self._lock = threading.RLock()
        self._context: dict[str, Any] = {}
        self._execution: dict[str, Any] = {
            "lastError": None,
            "failedActions": [],
            "consoleErrors": [],
            "lastScreenshot": None,
            "workflowName": None,
            "currentUrl": None,
        }

    def set(self, key: str, value: Any) -> None:
        """Set a user-defined context value.

        Args:
            key: The context key (used as {{context.key}} in templates)
            value: The value to store

        Example:
            >>> ctx.set("nextJsError", "Module not found: '@/components/Button'")
        """
        with self._lock:
            self._context[key] = value

    def get(self, key: str, default: Any = None) -> Any:
        """Get a user-defined context value.

        Args:
            key: The context key to retrieve
            default: Value to return if key not found

        Returns:
            The stored value, or default if not found

        Example:
            >>> ctx.get("nextJsError", "No error captured")
        """
        with self._lock:
            return self._context.get(key, default)

    def get_execution(self, key: str, default: Any = None) -> Any:
        """Get an auto-populated execution context value.

        Args:
            key: The execution context key to retrieve
            default: Value to return if key not found

        Returns:
            The stored value, or default if not found

        Example:
            >>> ctx.get_execution("lastError")
        """
        with self._lock:
            return self._execution.get(key, default)

    def update_execution_context(self, data: dict[str, Any]) -> None:
        """Update auto-populated execution context values.

        This method merges the provided data into the execution context,
        allowing partial updates without overwriting unrelated values.

        Args:
            data: Dictionary of execution context values to update

        Example:
            >>> ctx.update_execution_context({
            ...     "lastError": "FIND action failed: image not found",
            ...     "lastScreenshot": "/tmp/screenshot-001.png"
            ... })
        """
        with self._lock:
            self._execution.update(data)

    def add_failed_action(self, action_id: str, action_type: str, error: str) -> None:
        """Record a failed action in the execution context.

        Args:
            action_id: The ID of the failed action
            action_type: The type of action (e.g., "CLICK", "FIND")
            error: The error message

        Example:
            >>> ctx.add_failed_action("click-123", "CLICK", "Element not found")
        """
        with self._lock:
            self._execution["failedActions"].append(
                {
                    "id": action_id,
                    "type": action_type,
                    "error": error,
                }
            )
            self._execution["lastError"] = error

    def add_console_error(self, error: str) -> None:
        """Record a console error in the execution context.

        Args:
            error: The console error message

        Example:
            >>> ctx.add_console_error("Uncaught TypeError: x is not a function")
        """
        with self._lock:
            self._execution["consoleErrors"].append(error)

    def get_all_context(self) -> dict[str, Any]:
        """Get all context values (both user-defined and execution).

        Returns:
            Dictionary with 'context' and 'execution' keys

        Example:
            >>> ctx.get_all_context()
            {'context': {'errorMessage': '...'}, 'execution': {'lastError': '...'}}
        """
        with self._lock:
            return {
                "context": self._context.copy(),
                "execution": self._execution.copy(),
            }

    def get_context_dict(self) -> dict[str, Any]:
        """Get user-defined context values as a dictionary.

        Returns:
            Copy of the user-defined context dictionary
        """
        with self._lock:
            return self._context.copy()

    def get_execution_dict(self) -> dict[str, Any]:
        """Get execution context values as a dictionary.

        Returns:
            Copy of the execution context dictionary
        """
        with self._lock:
            return self._execution.copy()

    def clear(self) -> None:
        """Clear all context values (user-defined and execution).

        This should be called at the start of each workflow execution
        to ensure a clean state.

        Example:
            >>> ctx.clear()
            >>> ctx.get_all_context()
            {'context': {}, 'execution': {'lastError': None, ...}}
        """
        with self._lock:
            self._context.clear()
            self._execution = {
                "lastError": None,
                "failedActions": [],
                "consoleErrors": [],
                "lastScreenshot": None,
                "workflowName": None,
                "currentUrl": None,
            }

    def to_json(self) -> str:
        """Serialize context to JSON string.

        Returns:
            JSON string representation of all context values
        """
        with self._lock:
            return json.dumps(self.get_all_context(), indent=2, default=str)

    def __repr__(self) -> str:
        """Return string representation for debugging."""
        with self._lock:
            return (
                f"WorkflowContext("
                f"context_keys={list(self._context.keys())}, "
                f"execution_keys={list(self._execution.keys())})"
            )
