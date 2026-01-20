"""
Screenshot Step Output Formatter.

Formats screen capture data with detected elements for AI test generation.
"""

from typing import Any

from .base import AnalysisFormatter, AssertableField


class ScreenshotFormatter(AnalysisFormatter):
    """Formatter for screenshot step outputs."""

    @property
    def step_type(self) -> str:
        return "screenshot"

    @property
    def display_name(self) -> str:
        return "Screenshot"

    @property
    def description(self) -> str:
        return "Screen capture with optional element detection"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "Screenshot Capture")
        dimensions = output.get("dimensions", {})
        monitor_index = output.get("monitor_index")
        analysis_source = output.get("analysis_source")
        detected_elements = output.get("detected_elements", [])
        success = output.get("success", True)
        error = output.get("error")

        lines.append(f"### Screenshot: {step_name}")
        lines.append(
            f"- **Dimensions:** {dimensions.get('width', 0)}x{dimensions.get('height', 0)}"
        )

        if monitor_index is not None:
            lines.append(f"- **Monitor:** {monitor_index}")

        if analysis_source:
            lines.append(f"- **Analysis Source:** {analysis_source}")

        status_emoji = "✅" if success else "❌"
        lines.append(f"- **Status:** {status_emoji} {'Captured' if success else 'Failed'}")

        if error:
            lines.append(f"- **Error:** {error}")

        # Detected elements summary
        if detected_elements:
            lines.append("")
            lines.append(f"**Detected Elements:** {len(detected_elements)}")
            lines.append("")
            lines.append("| # | Label | Type | Confidence | Bounds |")
            lines.append("|---|-------|------|------------|--------|")

            for i, el in enumerate(detected_elements[:15]):
                confidence = el.get("confidence", 0) * 100
                bbox = el.get("bounding_box", {})
                bounds = f"({bbox.get('x', 0)}, {bbox.get('y', 0)}) {bbox.get('width', 0)}x{bbox.get('height', 0)}"
                lines.append(
                    f"| {i + 1} | {el.get('label', '')} | {el.get('element_type', '')} | {confidence:.0f}% | {bounds} |"
                )

            if len(detected_elements) > 15:
                lines.append(f"*... and {len(detected_elements) - 15} more elements*")

        # Note about screenshot availability
        if output.get("screenshot_base64"):
            lines.append("")
            lines.append("**Screenshot:** Available (base64 encoded)")
        if output.get("annotated_screenshot_base64"):
            lines.append("**Annotated Screenshot:** Available (with element overlays)")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Success status
        fields.append(
            AssertableField(
                path="success",
                name="Capture Success",
                description="Whether the screenshot was captured successfully",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert screenshot captured successfully"],
            )
        )

        # Dimensions
        dimensions = output.get("dimensions", {})
        fields.append(
            AssertableField(
                path="dimensions.width",
                name="Screenshot Width",
                description="Width of the captured screenshot in pixels",
                field_type="number",
                example_value=dimensions.get("width", 0),
                suggested_assertions=["Assert width matches expected resolution"],
            )
        )

        fields.append(
            AssertableField(
                path="dimensions.height",
                name="Screenshot Height",
                description="Height of the captured screenshot in pixels",
                field_type="number",
                example_value=dimensions.get("height", 0),
                suggested_assertions=["Assert height matches expected resolution"],
            )
        )

        # Element count
        detected_elements = output.get("detected_elements", [])
        if detected_elements:
            fields.append(
                AssertableField(
                    path="detected_elements.length",
                    name="Element Count",
                    description="Number of elements detected in the screenshot",
                    field_type="number",
                    example_value=len(detected_elements),
                    suggested_assertions=[
                        "Assert expected number of elements detected",
                        "Assert at least N elements found",
                    ],
                )
            )

            # Individual elements (limited)
            for i, el in enumerate(detected_elements[:5]):
                fields.append(
                    AssertableField(
                        path=f"detected_elements[{i}].label",
                        name=f"Element {i + 1} Label",
                        description=f"Label of detected element: {el.get('label', '')}",
                        field_type="string",
                        example_value=el.get("label"),
                        suggested_assertions=[
                            f"Assert element \"{el.get('label', '')}\" is present"
                        ],
                    )
                )

                fields.append(
                    AssertableField(
                        path=f"detected_elements[{i}].confidence",
                        name=f"Element {i + 1} Confidence",
                        description=f"Detection confidence for {el.get('label', '')}",
                        field_type="number",
                        example_value=el.get("confidence"),
                        suggested_assertions=["Assert confidence above threshold"],
                    )
                )

        return fields
