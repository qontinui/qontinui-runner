"""
Check Step Output Formatter.

Formats validation/check step data with issues for AI test generation.
"""

from typing import Any

from .base import AnalysisFormatter, AssertableField


class CheckFormatter(AnalysisFormatter):
    """Formatter for check/validation step outputs."""

    @property
    def step_type(self) -> str:
        return "check"

    @property
    def display_name(self) -> str:
        return "Check/Validation"

    @property
    def description(self) -> str:
        return "Validation check with pass/fail results and issues"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "Check")
        check_type = output.get("check_type", "custom")
        checks_passed = output.get("checks_passed", 0)
        checks_failed = output.get("checks_failed", 0)
        issues = output.get("issues", [])
        duration_ms = output.get("duration_ms")
        error = output.get("error")

        lines.append(f"### Check: {step_name}")
        lines.append(f"- **Check Type:** {check_type}")

        total = checks_passed + checks_failed
        pass_rate = f"{(checks_passed / total * 100):.0f}" if total > 0 else "N/A"
        lines.append(f"- **Results:** {checks_passed}/{total} passed ({pass_rate}%)")

        status_emoji = "✅" if checks_failed == 0 else "❌"
        status_text = "All Passed" if checks_failed == 0 else f"{checks_failed} Failed"
        lines.append(f"- **Status:** {status_emoji} {status_text}")

        if duration_ms is not None:
            lines.append(f"- **Duration:** {duration_ms}ms")

        if error:
            lines.append(f"- **Error:** {error}")

        # Issues
        if issues:
            lines.append("")
            lines.append(f"**Issues Found:** {len(issues)}")
            lines.append("")

            # Group by severity
            errors = [i for i in issues if i.get("severity") == "error"]
            warnings = [i for i in issues if i.get("severity") == "warning"]
            infos = [i for i in issues if i.get("severity") == "info"]

            if errors:
                lines.append(f"**Errors ({len(errors)}):**")
                for issue in errors[:5]:
                    location = f" [{issue.get('location', '')}]" if issue.get("location") else ""
                    lines.append(f"- ❌ {issue.get('message', '')}{location}")
                    if issue.get("suggestion"):
                        lines.append(f"  *Suggestion: {issue.get('suggestion')}*")
                if len(errors) > 5:
                    lines.append(f"  ... and {len(errors) - 5} more errors")

            if warnings:
                lines.append("")
                lines.append(f"**Warnings ({len(warnings)}):**")
                for issue in warnings[:3]:
                    location = f" [{issue.get('location', '')}]" if issue.get("location") else ""
                    lines.append(f"- ⚠️ {issue.get('message', '')}{location}")
                if len(warnings) > 3:
                    lines.append(f"  ... and {len(warnings) - 3} more warnings")

            if infos and not errors and not warnings:
                lines.append("")
                lines.append(f"**Info ({len(infos)}):**")
                for issue in infos[:3]:
                    lines.append(f"- ℹ️ {issue.get('message', '')}")

        # Screenshots for visual regression
        if output.get("reference_screenshot") or output.get("current_screenshot"):
            lines.append("")
            lines.append("**Screenshots:**")
            if output.get("reference_screenshot"):
                lines.append("- Reference screenshot available")
            if output.get("current_screenshot"):
                lines.append("- Current screenshot available")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []
        issues = output.get("issues", [])

        # Overall success
        fields.append(
            AssertableField(
                path="success",
                name="Check Success",
                description="Whether all checks passed",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert all checks passed"],
            )
        )

        # Checks passed
        fields.append(
            AssertableField(
                path="checks_passed",
                name="Checks Passed",
                description="Number of checks that passed",
                field_type="number",
                example_value=output.get("checks_passed", 0),
                suggested_assertions=["Assert expected number of checks passed"],
            )
        )

        # Checks failed
        fields.append(
            AssertableField(
                path="checks_failed",
                name="Checks Failed",
                description="Number of checks that failed",
                field_type="number",
                example_value=output.get("checks_failed", 0),
                suggested_assertions=[
                    "Assert no checks failed",
                    "Assert failed count below threshold",
                ],
            )
        )

        # Issue count
        fields.append(
            AssertableField(
                path="issues.length",
                name="Issue Count",
                description="Total number of issues found",
                field_type="number",
                example_value=len(issues),
                suggested_assertions=[
                    "Assert no issues found",
                    "Assert issue count below threshold",
                ],
            )
        )

        # Error count
        error_count = sum(1 for i in issues if i.get("severity") == "error")
        fields.append(
            AssertableField(
                path="issues.error_count",
                name="Error Count",
                description="Number of error-severity issues",
                field_type="number",
                example_value=error_count,
                suggested_assertions=["Assert no errors found"],
            )
        )

        # Warning count
        warning_count = sum(1 for i in issues if i.get("severity") == "warning")
        fields.append(
            AssertableField(
                path="issues.warning_count",
                name="Warning Count",
                description="Number of warning-severity issues",
                field_type="number",
                example_value=warning_count,
                suggested_assertions=["Assert warning count below threshold"],
            )
        )

        # Individual issues (first few)
        for i, issue in enumerate(issues[:3]):
            message = issue.get("message", "")
            fields.append(
                AssertableField(
                    path=f"issues[{i}].message",
                    name=f"Issue {i + 1}",
                    description=f"Issue: {message[:50]}...",
                    field_type="string",
                    example_value=message,
                    suggested_assertions=["Check if issue is expected/known"],
                )
            )

        # Check type
        fields.append(
            AssertableField(
                path="check_type",
                name="Check Type",
                description="Type of validation performed",
                field_type="string",
                example_value=output.get("check_type", "custom"),
                suggested_assertions=["Verify correct check type was run"],
            )
        )

        # Duration
        duration_ms = output.get("duration_ms")
        if duration_ms is not None:
            fields.append(
                AssertableField(
                    path="duration_ms",
                    name="Check Duration",
                    description="Time taken to run checks in milliseconds",
                    field_type="number",
                    example_value=duration_ms,
                    suggested_assertions=[
                        "Assert check completes within time limit",
                        "Record as performance metric",
                    ],
                )
            )

        return fields
