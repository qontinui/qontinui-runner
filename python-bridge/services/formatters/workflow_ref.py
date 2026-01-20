"""
Workflow Reference Step Output Formatter.

Formats nested workflow execution data for AI test generation.
"""

import json
from typing import Any

from .base import AnalysisFormatter, AssertableField


class WorkflowRefFormatter(AnalysisFormatter):
    """Formatter for workflow reference step outputs."""

    @property
    def step_type(self) -> str:
        return "workflow_ref"

    @property
    def display_name(self) -> str:
        return "Workflow Reference"

    @property
    def description(self) -> str:
        return "Nested workflow execution with collected outputs"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "Workflow")
        workflow_id = output.get("workflow_id", "")
        workflow_name = output.get("workflow_name", "Unknown Workflow")
        final_state = output.get("final_state")
        nested_outputs = output.get("nested_outputs", [])
        output_variables = output.get("output_variables", {})
        success = output.get("success", True)
        duration_ms = output.get("duration_ms")
        error = output.get("error")

        lines.append(f"### Workflow Reference: {step_name}")
        lines.append(f"- **Workflow ID:** {workflow_id}")
        lines.append(f"- **Workflow Name:** {workflow_name}")

        if final_state:
            lines.append(f"- **Final State:** {final_state}")

        status_emoji = "✅" if success else "❌"
        lines.append(f"- **Status:** {status_emoji} {'Completed' if success else 'Failed'}")

        if duration_ms is not None:
            lines.append(f"- **Duration:** {duration_ms}ms")

        if error:
            lines.append(f"- **Error:** {error}")

        # Nested outputs summary
        if nested_outputs:
            lines.append("")
            lines.append(f"**Nested Steps:** {len(nested_outputs)}")

            # Count by type
            by_type: dict[str, int] = {}
            for nested in nested_outputs:
                step_type = nested.get("step_type", "unknown")
                by_type[step_type] = by_type.get(step_type, 0) + 1

            for step_type, count in by_type.items():
                lines.append(f"- {count} {step_type} steps")

            # Show first few nested outputs
            lines.append("")
            lines.append("**Step Sequence:**")
            for i, nested in enumerate(nested_outputs[:5]):
                status = "✓" if nested.get("success", True) else "✗"
                lines.append(
                    f"{i + 1}. [{status}] {nested.get('step_name', 'Step')} ({nested.get('step_type', 'unknown')})"
                )
            if len(nested_outputs) > 5:
                lines.append(f"   ... and {len(nested_outputs) - 5} more steps")

        # Output variables
        if output_variables:
            lines.append("")
            lines.append("**Output Variables:**")
            for key, value in output_variables.items():
                value_str = json.dumps(value) if isinstance(value, (dict, list)) else str(value)
                lines.append(f"- `{key}`: {value_str[:100]}")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Success status
        fields.append(
            AssertableField(
                path="success",
                name="Workflow Success",
                description="Whether the nested workflow completed successfully",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert workflow completed successfully"],
            )
        )

        # Final state
        final_state = output.get("final_state")
        if final_state:
            fields.append(
                AssertableField(
                    path="final_state",
                    name="Final State",
                    description="The state reached after workflow execution",
                    field_type="string",
                    example_value=final_state,
                    suggested_assertions=["Assert workflow ended in expected state"],
                )
            )

        # Nested output count
        nested_outputs = output.get("nested_outputs", [])
        if nested_outputs:
            fields.append(
                AssertableField(
                    path="nested_outputs.length",
                    name="Step Count",
                    description="Number of steps executed in the nested workflow",
                    field_type="number",
                    example_value=len(nested_outputs),
                    suggested_assertions=["Assert expected number of steps executed"],
                )
            )

            # Count successful nested steps
            success_count = sum(1 for o in nested_outputs if o.get("success", True))
            fields.append(
                AssertableField(
                    path="nested_outputs.success_count",
                    name="Successful Steps",
                    description="Number of steps that completed successfully",
                    field_type="number",
                    example_value=success_count,
                    suggested_assertions=["Assert all steps succeeded"],
                )
            )

        # Output variables
        output_variables = output.get("output_variables", {})
        for key, value in output_variables.items():
            fields.append(
                AssertableField(
                    path=f"output_variables.{key}",
                    name=f"Variable: {key}",
                    description=f"Output variable from workflow: {key}",
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
                    name="Workflow Duration",
                    description="Total execution time in milliseconds",
                    field_type="number",
                    example_value=duration_ms,
                    suggested_assertions=[
                        "Assert workflow completes within time limit",
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
