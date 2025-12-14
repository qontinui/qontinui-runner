"""Template Resolver Module.

This module provides template variable resolution for TRIGGER_AI_ANALYSIS prompts.
It resolves {{context.key}} and {{execution.key}} patterns in template strings.

Following the Single Responsibility Principle, this module:
- Resolves template variables from WorkflowContext
- Does NOT store context or execute actions
"""

from __future__ import annotations

import json
import re
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from workflow_context import WorkflowContext


# Pattern to match {{namespace.key}} or {{namespace.key.nested}}
TEMPLATE_PATTERN = re.compile(r"\{\{(context|execution)\.([a-zA-Z0-9_.]+)\}\}")


def resolve_template(template: str, context: WorkflowContext) -> str:
    """Resolve template variables in a string.

    Supported patterns:
    - {{context.key}} - User-defined context values (set via CAPTURE_CONTEXT)
    - {{execution.lastError}} - Last error message from failed action
    - {{execution.failedActions}} - JSON array of failed actions
    - {{execution.consoleErrors}} - Captured console error output
    - {{execution.lastScreenshot}} - Path to most recent screenshot
    - {{execution.workflowName}} - Current workflow name
    - {{execution.currentUrl}} - Current page URL

    Args:
        template: String containing {{variable}} patterns
        context: WorkflowContext instance with values to substitute

    Returns:
        String with all template variables resolved

    Example:
        >>> ctx = WorkflowContext()
        >>> ctx.set("errorMsg", "Cannot read 'x' of undefined")
        >>> ctx.update_execution_context({"lastScreenshot": "/tmp/shot.png"})
        >>> resolve_template(
        ...     "Error: {{context.errorMsg}}\\nScreenshot: {{execution.lastScreenshot}}",
        ...     ctx
        ... )
        "Error: Cannot read 'x' of undefined\\nScreenshot: /tmp/shot.png"
    """
    if not template:
        return template

    def replace_match(match: re.Match) -> str:
        namespace = match.group(1)  # "context" or "execution"
        key = match.group(2)  # e.g., "errorMessage" or "nested.key"

        # Handle nested keys (e.g., "config.timeout")
        keys = key.split(".")

        if namespace == "context":
            value = context.get(keys[0])
        else:  # execution
            value = context.get_execution(keys[0])

        # Navigate nested keys
        for nested_key in keys[1:]:
            if isinstance(value, dict):
                value = value.get(nested_key)
            else:
                value = None
                break

        # Format the value for insertion
        return format_value(value)

    return TEMPLATE_PATTERN.sub(replace_match, template)


def format_value(value: Any) -> str:
    """Format a value for template insertion.

    Args:
        value: The value to format

    Returns:
        String representation suitable for template insertion

    - None -> "(not set)"
    - str -> as-is
    - list -> JSON array or newline-separated if strings
    - dict -> JSON object
    - other -> str()
    """
    if value is None:
        return "(not set)"

    if isinstance(value, str):
        return value

    if isinstance(value, list):
        # For list of strings, join with newlines for readability
        if all(isinstance(item, str) for item in value):
            return "\n".join(value) if value else "(empty)"
        # For complex lists (e.g., failed actions), use JSON
        return json.dumps(value, indent=2, default=str) if value else "(empty)"

    if isinstance(value, dict):
        return json.dumps(value, indent=2, default=str)

    return str(value)


def get_available_variables(context: WorkflowContext) -> dict[str, list[str]]:
    """Get list of available template variables.

    This is useful for debugging or auto-completion.

    Args:
        context: WorkflowContext instance

    Returns:
        Dictionary with 'context' and 'execution' lists of available keys

    Example:
        >>> ctx = WorkflowContext()
        >>> ctx.set("errorMsg", "test")
        >>> get_available_variables(ctx)
        {'context': ['errorMsg'], 'execution': ['lastError', 'failedActions', ...]}
    """
    return {
        "context": list(context.get_context_dict().keys()),
        "execution": list(context.get_execution_dict().keys()),
    }


def validate_template(template: str) -> list[str]:
    """Validate a template string and return list of variable references.

    Args:
        template: String containing {{variable}} patterns

    Returns:
        List of variable references found (e.g., ["context.errorMsg", "execution.lastError"])

    Example:
        >>> validate_template("Error: {{context.msg}}\\n{{execution.lastError}}")
        ['context.msg', 'execution.lastError']
    """
    matches = TEMPLATE_PATTERN.findall(template)
    return [f"{namespace}.{key}" for namespace, key in matches]
