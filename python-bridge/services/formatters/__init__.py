"""
Step Output Formatters for AI Test Generation.

This module provides a registry-based system for formatting different step output types
into AI-friendly summaries. Each formatter knows how to:

- Summarize step output data for AI prompts
- Extract assertable fields for verification hints
- Format the output consistently across types

Usage:
    from services.formatters import formatter_registry

    # Get a formatter for a step type
    formatter = formatter_registry.get('api_request')

    # Format output for AI
    summary = formatter.summarize_for_ai(output_data)

    # Get assertable fields
    fields = formatter.get_assertable_fields(output_data)
"""

from .base import AnalysisFormatter, AssertableField, FormatterRegistry
from .api_request import ApiRequestFormatter
from .gui_action import GuiActionFormatter
from .shell_command import ShellCommandFormatter
from .mcp_call import McpCallFormatter
from .screenshot import ScreenshotFormatter
from .workflow_ref import WorkflowRefFormatter
from .playwright_script import PlaywrightScriptFormatter
from .state_navigation import StateNavigationFormatter
from .check import CheckFormatter

# Create and populate the global registry
formatter_registry = FormatterRegistry()
formatter_registry.register(ApiRequestFormatter())
formatter_registry.register(GuiActionFormatter())
formatter_registry.register(ShellCommandFormatter())
formatter_registry.register(McpCallFormatter())
formatter_registry.register(ScreenshotFormatter())
formatter_registry.register(WorkflowRefFormatter())
formatter_registry.register(PlaywrightScriptFormatter())
formatter_registry.register(StateNavigationFormatter())
formatter_registry.register(CheckFormatter())

__all__ = [
    "AnalysisFormatter",
    "AssertableField",
    "FormatterRegistry",
    "formatter_registry",
    "ApiRequestFormatter",
    "GuiActionFormatter",
    "ShellCommandFormatter",
    "McpCallFormatter",
    "ScreenshotFormatter",
    "WorkflowRefFormatter",
    "PlaywrightScriptFormatter",
    "StateNavigationFormatter",
    "CheckFormatter",
]
