"""
API Request Step Output Formatter.

Formats HTTP request/response data for AI test generation.
"""

import json
from typing import Any

from .base import AnalysisFormatter, AssertableField


class ApiRequestFormatter(AnalysisFormatter):
    """Formatter for API request step outputs."""

    @property
    def step_type(self) -> str:
        return "api_request"

    @property
    def display_name(self) -> str:
        return "API Request"

    @property
    def description(self) -> str:
        return "HTTP request/response data with status, headers, and body"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "API Request")
        method = output.get("method", "GET")
        url = output.get("url", "")
        status_code = output.get("status_code")
        duration = output.get("duration_ms")
        error = output.get("error")
        response = output.get("response")
        extractions = output.get("extractions")

        lines.append(f"### API Request: {step_name}")
        lines.append(f"- **Method:** {method}")
        lines.append(f"- **URL:** {url}")

        if status_code is not None:
            status_emoji = "✅" if status_code < 400 else "❌"
            lines.append(f"- **Status:** {status_emoji} {status_code}")

        if duration is not None:
            lines.append(f"- **Duration:** {duration}ms")

        if error:
            lines.append(f"- **Error:** {error}")

        # Response summary
        if response is not None:
            lines.append("")
            lines.append("**Response Data:**")
            lines.append("```json")
            try:
                response_str = json.dumps(response, indent=2)
                # Truncate very long responses
                if len(response_str) > 8000:
                    response_str = response_str[:8000] + "\n... (truncated)"
                lines.append(response_str)
            except (TypeError, ValueError):
                lines.append(str(response)[:4000])
            lines.append("```")

        # Extractions summary
        if extractions and len(extractions) > 0:
            lines.append("")
            lines.append("**Extracted Variables:**")
            for key, value in extractions.items():
                value_str = json.dumps(value) if isinstance(value, (dict, list)) else str(value)
                lines.append(f"- `{key}`: {value_str[:100]}")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Status code
        status_code = output.get("status_code")
        if status_code is not None:
            fields.append(
                AssertableField(
                    path="status_code",
                    name="HTTP Status Code",
                    description="The HTTP response status code",
                    field_type="number",
                    example_value=status_code,
                    suggested_assertions=[
                        "Assert status code equals expected value",
                        "Assert status code is in 2xx range",
                    ],
                )
            )

        # Response body
        response = output.get("response")
        if response is not None:
            fields.append(
                AssertableField(
                    path="response",
                    name="Response Body",
                    description="The full response body",
                    field_type="object" if isinstance(response, dict) else "any",
                    suggested_assertions=[
                        "Assert response is not null/empty",
                        "Assert response contains expected structure",
                    ],
                )
            )

            # Extract nested fields from response
            if isinstance(response, dict):
                self._extract_nested_fields(response, "response", fields)

        # Duration
        duration = output.get("duration_ms")
        if duration is not None:
            fields.append(
                AssertableField(
                    path="duration_ms",
                    name="Response Duration",
                    description="Time taken for the request in milliseconds",
                    field_type="number",
                    example_value=duration,
                    suggested_assertions=[
                        "Assert response time is under threshold",
                        "Record as performance metric",
                    ],
                )
            )

        # Extractions
        extractions = output.get("extractions")
        if extractions:
            for key, value in extractions.items():
                fields.append(
                    AssertableField(
                        path=f"extractions.{key}",
                        name=f"Extracted: {key}",
                        description=f"Variable extracted from response: {key}",
                        field_type=self._get_type(value),
                        example_value=value,
                        suggested_assertions=[
                            f"Assert {key} equals expected value",
                            f"Assert {key} is not null",
                        ],
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
        if depth > 3:
            return

        for key, value in obj.items():
            path = f"{base_path}.{key}"

            # Skip very long arrays
            if isinstance(value, list) and len(value) > 100:
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
                    description=f"Response field: {key}",
                    field_type=self._get_type(value),
                    example_value=self._truncate_value(value),
                    suggested_assertions=[
                        f"Assert {key} equals expected value",
                        f"Assert {key} is not null",
                    ],
                )
            )

            # Recurse into nested objects
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
        if isinstance(value, str) and len(value) > 100:
            return value[:100] + "..."
        if isinstance(value, list) and len(value) > 5:
            return [*value[:5], f"... ({len(value) - 5} more)"]
        return value
