"""
State Navigation Step Output Formatter.

Formats state navigation data with path tracking for AI test generation.
"""

from typing import Any

from .base import AnalysisFormatter, AssertableField


class StateNavigationFormatter(AnalysisFormatter):
    """Formatter for state navigation step outputs."""

    @property
    def step_type(self) -> str:
        return "state_navigation"

    @property
    def display_name(self) -> str:
        return "State Navigation"

    @property
    def description(self) -> str:
        return "Automated navigation to target state with path tracking"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "State Navigation")
        target_state = output.get("target_state", "")
        final_state = output.get("final_state")
        reached = output.get("reached", False)
        path_taken = output.get("path_taken", [])
        duration_ms = output.get("duration_ms")
        error = output.get("error")

        lines.append(f"### State Navigation: {step_name}")
        lines.append(f"- **Target State:** {target_state}")

        if final_state:
            lines.append(f"- **Final State:** {final_state}")

        reached_emoji = "✅" if reached else "❌"
        lines.append(f"- **Reached Target:** {reached_emoji} {'Yes' if reached else 'No'}")

        if duration_ms is not None:
            lines.append(f"- **Duration:** {duration_ms}ms")

        if error:
            lines.append(f"- **Error:** {error}")

        # Path taken
        if path_taken:
            lines.append("")
            lines.append(f"**Path Taken:** {len(path_taken)} transitions")
            lines.append("")

            for i, step in enumerate(path_taken):
                action = f" ({step.get('action_taken', '')})" if step.get("action_taken") else ""
                lines.append(
                    f'{i + 1}. {step.get("from_state", "")} → {step.get("to_state", "")} via "{step.get("transition_name", "")}"{action}'
                )
        elif reached:
            lines.append("")
            lines.append("**Note:** Already at target state (no navigation needed)")

        # Screenshot availability
        if output.get("final_screenshot"):
            lines.append("")
            lines.append("**Final Screenshot:** Available")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Reached target
        fields.append(
            AssertableField(
                path="reached",
                name="Target Reached",
                description="Whether the target state was reached",
                field_type="boolean",
                example_value=output.get("reached", False),
                suggested_assertions=[
                    "Assert target state was reached",
                    "Assert navigation succeeded",
                ],
            )
        )

        # Target state
        fields.append(
            AssertableField(
                path="target_state",
                name="Target State",
                description="The state we were navigating to",
                field_type="string",
                example_value=output.get("target_state", ""),
                suggested_assertions=["Verify target state is correct"],
            )
        )

        # Final state
        final_state = output.get("final_state")
        if final_state:
            fields.append(
                AssertableField(
                    path="final_state",
                    name="Final State",
                    description="The state reached after navigation",
                    field_type="string",
                    example_value=final_state,
                    suggested_assertions=[
                        "Assert final state equals target",
                        "Assert ended in expected state",
                    ],
                )
            )

        # Path length
        path_taken = output.get("path_taken", [])
        if path_taken:
            fields.append(
                AssertableField(
                    path="path_taken.length",
                    name="Path Length",
                    description="Number of transitions taken",
                    field_type="number",
                    example_value=len(path_taken),
                    suggested_assertions=[
                        "Assert path length is optimal",
                        "Assert path length under threshold",
                    ],
                )
            )

            # Individual path steps
            for i, step in enumerate(path_taken[:5]):
                fields.append(
                    AssertableField(
                        path=f"path_taken[{i}].transition_name",
                        name=f"Transition {i + 1}",
                        description=f"Transition from {step.get('from_state', '')} to {step.get('to_state', '')}",
                        field_type="string",
                        example_value=step.get("transition_name", ""),
                        suggested_assertions=[
                            f'Assert transition "{step.get("transition_name", "")}" was used'
                        ],
                    )
                )

        # Duration
        duration_ms = output.get("duration_ms")
        if duration_ms is not None:
            fields.append(
                AssertableField(
                    path="duration_ms",
                    name="Navigation Duration",
                    description="Time taken to navigate in milliseconds",
                    field_type="number",
                    example_value=duration_ms,
                    suggested_assertions=[
                        "Assert navigation completes within time limit",
                        "Record as performance metric",
                    ],
                )
            )

        # Success
        fields.append(
            AssertableField(
                path="success",
                name="Navigation Success",
                description="Whether navigation completed without errors",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert navigation succeeded"],
            )
        )

        return fields
