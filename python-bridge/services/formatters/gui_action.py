"""
GUI Action Step Output Formatter.

Formats mouse/keyboard action data for AI test generation.
"""

from typing import Any

from .base import AnalysisFormatter, AssertableField


class GuiActionFormatter(AnalysisFormatter):
    """Formatter for GUI action step outputs."""

    @property
    def step_type(self) -> str:
        return "gui_action"

    @property
    def display_name(self) -> str:
        return "GUI Action"

    @property
    def description(self) -> str:
        return "Mouse/keyboard action with target location and screenshots"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "GUI Action")
        action_type = output.get("action_type", "click")
        target_label = output.get("target_label")
        target_location = output.get("target_location")
        target_bounds = output.get("target_bounds")
        confidence = output.get("confidence")
        typed_text = output.get("typed_text")
        key_pressed = output.get("key_pressed")
        scroll_delta = output.get("scroll_delta")
        monitor_index = output.get("monitor_index")
        success = output.get("success", True)
        error = output.get("error")
        screenshot_before = output.get("screenshot_before")
        screenshot_after = output.get("screenshot_after")

        lines.append(f"### GUI Action: {step_name}")
        lines.append(f"- **Action Type:** {action_type}")

        if target_label:
            lines.append(f"- **Target:** {target_label}")

        if target_location:
            lines.append(
                f"- **Location:** ({target_location.get('x', 0)}, {target_location.get('y', 0)})"
            )

        if target_bounds:
            lines.append(
                f"- **Bounds:** ({target_bounds.get('x', 0)}, {target_bounds.get('y', 0)}) "
                f"{target_bounds.get('width', 0)}x{target_bounds.get('height', 0)}"
            )

        if confidence is not None:
            confidence_percent = confidence * 100
            lines.append(f"- **Confidence:** {confidence_percent:.1f}%")

        if typed_text:
            lines.append(f'- **Typed Text:** "{typed_text}"')

        if key_pressed:
            lines.append(f"- **Key Pressed:** {key_pressed}")

        if scroll_delta:
            lines.append(
                f"- **Scroll Delta:** ({scroll_delta.get('x', 0)}, {scroll_delta.get('y', 0)})"
            )

        if monitor_index is not None:
            lines.append(f"- **Monitor:** {monitor_index}")

        status_emoji = "✅" if success else "❌"
        lines.append(f"- **Status:** {status_emoji} {'Success' if success else 'Failed'}")

        if error:
            lines.append(f"- **Error:** {error}")

        # Note about screenshots
        if screenshot_before or screenshot_after:
            lines.append("")
            lines.append("**Screenshots Available:**")
            if screenshot_before:
                lines.append("- Before action")
            if screenshot_after:
                lines.append("- After action")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Success status
        fields.append(
            AssertableField(
                path="success",
                name="Action Success",
                description="Whether the GUI action completed successfully",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert action succeeded", "Assert no error occurred"],
            )
        )

        # Confidence
        confidence = output.get("confidence")
        if confidence is not None:
            fields.append(
                AssertableField(
                    path="confidence",
                    name="Recognition Confidence",
                    description="Confidence score for target recognition (0-1)",
                    field_type="number",
                    example_value=confidence,
                    suggested_assertions=[
                        "Assert confidence is above threshold (e.g., > 0.8)",
                        "Record confidence as metric",
                    ],
                )
            )

        # Target location
        target_location = output.get("target_location")
        if target_location:
            fields.append(
                AssertableField(
                    path="target_location.x",
                    name="Target X Coordinate",
                    description="X coordinate of the action target",
                    field_type="number",
                    example_value=target_location.get("x"),
                    suggested_assertions=["Assert X is within expected range"],
                )
            )
            fields.append(
                AssertableField(
                    path="target_location.y",
                    name="Target Y Coordinate",
                    description="Y coordinate of the action target",
                    field_type="number",
                    example_value=target_location.get("y"),
                    suggested_assertions=["Assert Y is within expected range"],
                )
            )

        # Target bounds
        target_bounds = output.get("target_bounds")
        if target_bounds:
            fields.append(
                AssertableField(
                    path="target_bounds",
                    name="Target Bounding Box",
                    description="Bounding box of the detected target element",
                    field_type="object",
                    example_value=target_bounds,
                    suggested_assertions=[
                        "Assert bounds has positive dimensions",
                        "Assert bounds is within screen",
                    ],
                )
            )

        # Screenshots
        if output.get("screenshot_after"):
            fields.append(
                AssertableField(
                    path="screenshot_after",
                    name="Screenshot After Action",
                    description="Screenshot captured after the action",
                    field_type="string",
                    suggested_assertions=[
                        "Visual comparison with expected state",
                        "Check for expected elements in screenshot",
                    ],
                )
            )

        return fields
