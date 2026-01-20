"""
MCP Call Step Output Formatter.

Formats MCP (Model Context Protocol) tool call data for AI test generation.
"""

import json
from typing import Any

from .base import AnalysisFormatter, AssertableField


class McpCallFormatter(AnalysisFormatter):
    """Formatter for MCP tool call step outputs."""

    @property
    def step_type(self) -> str:
        return "mcp_call"

    @property
    def display_name(self) -> str:
        return "MCP Tool Call"

    @property
    def description(self) -> str:
        return "MCP server tool invocation with arguments and result"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "MCP Call")
        server_id = output.get("server_id", "unknown")
        tool_name = output.get("tool_name", "unknown")
        arguments = output.get("arguments", {})
        result = output.get("result")
        success = output.get("success", True)
        duration = output.get("duration_ms")
        error = output.get("error")

        lines.append(f"### MCP Tool Call: {step_name}")
        lines.append(f"- **Server:** {server_id}")
        lines.append(f"- **Tool:** {tool_name}")

        # Arguments
        if arguments:
            lines.append("")
            lines.append("**Arguments:**")
            lines.append("```json")
            try:
                args_str = json.dumps(arguments, indent=2)
                if len(args_str) > 2000:
                    args_str = args_str[:2000] + "\n... (truncated)"
                lines.append(args_str)
            except (TypeError, ValueError):
                lines.append(str(arguments)[:1000])
            lines.append("```")

        status_emoji = "✅" if success else "❌"
        lines.append(f"- **Status:** {status_emoji} {'Success' if success else 'Error'}")

        if duration is not None:
            lines.append(f"- **Duration:** {duration}ms")

        # Result
        if result is not None:
            lines.append("")
            lines.append("**Result:**")
            lines.append("```json")
            try:
                result_str = json.dumps(result, indent=2)
                if len(result_str) > 6000:
                    result_str = result_str[:6000] + "\n... (truncated)"
                lines.append(result_str)
            except (TypeError, ValueError):
                lines.append(str(result)[:3000])
            lines.append("```")

        if error:
            lines.append("")
            lines.append(f"**Error:** {error}")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Success status
        fields.append(
            AssertableField(
                path="success",
                name="Call Success",
                description="Whether the MCP tool call completed successfully",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert tool call succeeded", "Assert no error returned"],
            )
        )

        # Is error flag
        fields.append(
            AssertableField(
                path="is_error",
                name="Is Error",
                description="Whether the tool returned an error response",
                field_type="boolean",
                example_value=output.get("is_error", False),
                suggested_assertions=["Assert is_error is false"],
            )
        )

        # Result
        result = output.get("result")
        if result is not None:
            fields.append(
                AssertableField(
                    path="result",
                    name="Tool Result",
                    description="The result returned by the MCP tool",
                    field_type="object" if isinstance(result, dict) else "any",
                    example_value=self._truncate_value(result),
                    suggested_assertions=[
                        "Assert result is not null",
                        "Assert result contains expected data",
                    ],
                )
            )

            # Extract nested fields from result
            if isinstance(result, dict):
                self._extract_nested_fields(result, "result", fields)

        # Duration
        duration = output.get("duration_ms")
        if duration is not None:
            fields.append(
                AssertableField(
                    path="duration_ms",
                    name="Call Duration",
                    description="Time taken for the MCP call in milliseconds",
                    field_type="number",
                    example_value=duration,
                    suggested_assertions=[
                        "Assert response time is under threshold",
                        "Record as performance metric",
                    ],
                )
            )

        # Arguments (for verification that correct args were passed)
        arguments = output.get("arguments", {})
        if arguments:
            for key, value in arguments.items():
                fields.append(
                    AssertableField(
                        path=f"arguments.{key}",
                        name=f"Argument: {key}",
                        description=f"Argument passed to the tool: {key}",
                        field_type=self._get_type(value),
                        example_value=value,
                        suggested_assertions=[f"Verify {key} was passed correctly"],
                    )
                )

        return fields

    def _extract_nested_fields(
        self,
        obj: dict[str, Any],
        base_path: str,
        fields: list[AssertableField],
        depth: int = 0,
    ) -> None:
        """Extract assertable fields from nested objects."""
        if depth > 2:
            return

        for key, value in obj.items():
            path = f"{base_path}.{key}"

            # Skip very long arrays
            if isinstance(value, list) and len(value) > 50:
                fields.append(
                    AssertableField(
                        path=path,
                        name=key,
                        description=f"Array with {len(value)} items",
                        field_type="array",
                        example_value=f"[{len(value)} items]",
                        suggested_assertions=[f"Assert {key} has expected length"],
                    )
                )
                continue

            fields.append(
                AssertableField(
                    path=path,
                    name=key,
                    description=f"Result field: {key}",
                    field_type=self._get_type(value),
                    example_value=self._truncate_value(value),
                    suggested_assertions=[
                        f"Assert {key} equals expected value",
                        f"Assert {key} is not null",
                    ],
                )
            )

            if isinstance(value, dict):
                self._extract_nested_fields(value, path, fields, depth + 1)

    def _get_type(self, value: Any) -> str:
        """Get the type string for a value."""
        if value is None:
            return "null"
        if isinstance(value, bool):
            return "boolean"
        if isinstance(value, int | float):
            return "number"
        if isinstance(value, str):
            return "string"
        if isinstance(value, list):
            return "array"
        if isinstance(value, dict):
            return "object"
        return "any"

    def _truncate_value(self, value: Any) -> Any:
        """Truncate a value for display."""
        if isinstance(value, str) and len(value) > 200:
            return value[:197] + "..."
        if isinstance(value, list) and len(value) > 5:
            return [*value[:5], f"... ({len(value) - 5} more)"]
        return value
