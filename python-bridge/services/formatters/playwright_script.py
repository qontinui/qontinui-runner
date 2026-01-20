"""
Playwright Script Step Output Formatter.

Formats browser automation script execution data for AI test generation.
"""

import json
from typing import Any

from .base import AnalysisFormatter, AssertableField


class PlaywrightScriptFormatter(AnalysisFormatter):
    """Formatter for Playwright script step outputs."""

    @property
    def step_type(self) -> str:
        return "playwright_script"

    @property
    def display_name(self) -> str:
        return "Playwright Script"

    @property
    def description(self) -> str:
        return "Browser automation script with page state and network data"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "Playwright Script")
        final_url = output.get("final_url")
        page_title = output.get("page_title")
        console_logs = output.get("console_logs", [])
        network_requests = output.get("network_requests", [])
        extracted_data = output.get("extracted_data", {})
        success = output.get("success", True)
        duration_ms = output.get("duration_ms")
        error = output.get("error")

        lines.append(f"### Playwright Script: {step_name}")

        if final_url:
            lines.append(f"- **Final URL:** {final_url}")

        if page_title:
            lines.append(f"- **Page Title:** {page_title}")

        status_emoji = "✅" if success else "❌"
        lines.append(f"- **Status:** {status_emoji} {'Completed' if success else 'Failed'}")

        if duration_ms is not None:
            lines.append(f"- **Duration:** {duration_ms}ms")

        if error:
            lines.append(f"- **Error:** {error}")

        # Console logs
        if console_logs:
            lines.append("")
            lines.append(f"**Console Logs:** {len(console_logs)} entries")

            errors = [log for log in console_logs if log.get("type") == "error"]
            warnings = [log for log in console_logs if log.get("type") == "warn"]

            if errors:
                lines.append(f"- ❌ {len(errors)} errors")
            if warnings:
                lines.append(f"- ⚠️ {len(warnings)} warnings")

            # Show recent errors
            if errors:
                lines.append("")
                lines.append("**Console Errors:**")
                for err in errors[:3]:
                    lines.append(f"- {err.get('message', '')[:100]}")

        # Network requests
        if network_requests:
            lines.append("")
            lines.append(f"**Network Requests:** {len(network_requests)}")

            failed = [r for r in network_requests if r.get("status") and r.get("status") >= 400]
            if failed:
                lines.append(f"- ❌ {len(failed)} failed requests")
                for req in failed[:3]:
                    lines.append(
                        f"  - {req.get('method', 'GET')} {req.get('url', '')} ({req.get('status', '')})"
                    )

        # Extracted data
        if extracted_data:
            lines.append("")
            lines.append("**Extracted Data:**")
            lines.append("```json")
            try:
                data_str = json.dumps(extracted_data, indent=2)
                if len(data_str) > 2000:
                    data_str = data_str[:2000] + "\n... (truncated)"
                lines.append(data_str)
            except (TypeError, ValueError):
                lines.append(str(extracted_data)[:1000])
            lines.append("```")

        # Screenshot availability
        if output.get("final_screenshot"):
            lines.append("")
            lines.append("**Final Screenshot:** Available")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Success status
        fields.append(
            AssertableField(
                path="success",
                name="Script Success",
                description="Whether the Playwright script completed successfully",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert script executed successfully"],
            )
        )

        # Final URL
        final_url = output.get("final_url")
        if final_url:
            fields.append(
                AssertableField(
                    path="final_url",
                    name="Final URL",
                    description="The URL after script execution",
                    field_type="string",
                    example_value=final_url,
                    suggested_assertions=[
                        "Assert navigated to expected URL",
                        "Assert URL contains expected path",
                    ],
                )
            )

        # Page title
        page_title = output.get("page_title")
        if page_title:
            fields.append(
                AssertableField(
                    path="page_title",
                    name="Page Title",
                    description="The page title after script execution",
                    field_type="string",
                    example_value=page_title,
                    suggested_assertions=["Assert page title matches expected"],
                )
            )

        # Console errors
        console_logs = output.get("console_logs", [])
        if console_logs:
            error_count = sum(1 for log in console_logs if log.get("type") == "error")
            fields.append(
                AssertableField(
                    path="console_logs.error_count",
                    name="Console Error Count",
                    description="Number of console errors during execution",
                    field_type="number",
                    example_value=error_count,
                    suggested_assertions=[
                        "Assert no console errors",
                        "Assert error count below threshold",
                    ],
                )
            )

        # Network request count
        network_requests = output.get("network_requests", [])
        if network_requests:
            fields.append(
                AssertableField(
                    path="network_requests.length",
                    name="Network Request Count",
                    description="Total number of network requests made",
                    field_type="number",
                    example_value=len(network_requests),
                    suggested_assertions=["Assert expected number of API calls made"],
                )
            )

            failed_count = sum(
                1 for r in network_requests if r.get("status") and r.get("status") >= 400
            )
            fields.append(
                AssertableField(
                    path="network_requests.failed_count",
                    name="Failed Request Count",
                    description="Number of failed network requests (4xx/5xx)",
                    field_type="number",
                    example_value=failed_count,
                    suggested_assertions=["Assert no failed network requests"],
                )
            )

        # Extracted data fields
        extracted_data = output.get("extracted_data", {})
        for key, value in extracted_data.items():
            fields.append(
                AssertableField(
                    path=f"extracted_data.{key}",
                    name=f"Extracted: {key}",
                    description=f"Data extracted from page: {key}",
                    field_type=self._get_js_type(value),
                    example_value=value,
                    suggested_assertions=[
                        f"Assert {key} equals expected value",
                        f"Assert {key} is not null",
                    ],
                )
            )

        # Duration
        duration_ms = output.get("duration_ms")
        if duration_ms is not None:
            fields.append(
                AssertableField(
                    path="duration_ms",
                    name="Script Duration",
                    description="Script execution time in milliseconds",
                    field_type="number",
                    example_value=duration_ms,
                    suggested_assertions=[
                        "Assert script completes within time limit",
                        "Record as performance metric",
                    ],
                )
            )

        return fields

    def _get_js_type(self, value: Any) -> str:
        """Get JavaScript type for a value."""
        if value is None:
            return "null"
        if isinstance(value, list):
            return "array"
        if isinstance(value, bool):
            return "boolean"
        if isinstance(value, (int, float)):
            return "number"
        if isinstance(value, str):
            return "string"
        if isinstance(value, dict):
            return "object"
        return "any"
