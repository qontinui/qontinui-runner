"""
Base Formatter Classes and Registry.

Provides the abstract base class for step output formatters and the registry
for managing formatters by step type.
"""

from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import Any


@dataclass
class AssertableField:
    """A field from step output that can be used in assertions."""

    path: str
    name: str
    description: str
    field_type: str  # 'string', 'number', 'boolean', 'array', 'object', 'null', 'any'
    example_value: Any = None
    suggested_assertions: list[str] | None = None


class AnalysisFormatter(ABC):
    """
    Abstract base class for step output formatters.

    Each formatter handles a specific step output type and knows how to:
    - Summarize the output for AI prompts
    - Extract assertable fields for verification hints
    """

    @property
    @abstractmethod
    def step_type(self) -> str:
        """The step type this formatter handles (e.g., 'api_request')."""
        ...

    @property
    @abstractmethod
    def display_name(self) -> str:
        """Human-readable name for this step type."""
        ...

    @property
    @abstractmethod
    def description(self) -> str:
        """Short description of what this step type produces."""
        ...

    @abstractmethod
    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        """
        Generate a markdown summary of the step output for AI prompts.

        Args:
            output: The step output data dictionary

        Returns:
            Markdown-formatted summary string
        """
        ...

    @abstractmethod
    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        """
        Extract fields that can be used in assertions.

        Args:
            output: The step output data dictionary

        Returns:
            List of assertable fields with their metadata
        """
        ...


class FormatterRegistry:
    """
    Registry for step output formatters.

    Provides centralized access to formatters by step type.
    """

    def __init__(self) -> None:
        self._formatters: dict[str, AnalysisFormatter] = {}

    def register(self, formatter: AnalysisFormatter) -> None:
        """Register a formatter for its step type."""
        self._formatters[formatter.step_type] = formatter

    def get(self, step_type: str) -> AnalysisFormatter | None:
        """Get a formatter by step type."""
        return self._formatters.get(step_type)

    def has(self, step_type: str) -> bool:
        """Check if a formatter exists for a step type."""
        return step_type in self._formatters

    def get_all(self) -> list[AnalysisFormatter]:
        """Get all registered formatters."""
        return list(self._formatters.values())

    def get_registered_types(self) -> list[str]:
        """Get all registered step types."""
        return list(self._formatters.keys())

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        """
        Summarize a step output using the appropriate formatter.

        Falls back to generic summary if no formatter is registered.
        """
        step_type = output.get("step_type", "unknown")
        formatter = self.get(step_type)

        if formatter:
            return formatter.summarize_for_ai(output)

        # Generic fallback
        return self._generic_summary(output)

    def _generic_summary(self, output: dict[str, Any]) -> str:
        """Generate a generic summary for outputs without formatters."""
        lines = []
        step_name = output.get("step_name", "Step Output")
        step_type = output.get("step_type", "unknown")
        success = output.get("success", True)
        duration = output.get("duration_ms")
        error = output.get("error")

        lines.append(f"### {step_name}")
        lines.append(f"- **Type:** {step_type}")
        lines.append(f"- **Success:** {'Yes' if success else 'No'}")
        if duration is not None:
            lines.append(f"- **Duration:** {duration}ms")
        if error:
            lines.append(f"- **Error:** {error}")

        return "\n".join(lines)
