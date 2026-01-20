"""
Shell Command Step Output Formatter.

Formats command execution data for AI test generation.
"""

from typing import Any

from .base import AnalysisFormatter, AssertableField


class ShellCommandFormatter(AnalysisFormatter):
    """Formatter for shell command step outputs."""

    @property
    def step_type(self) -> str:
        return "shell_command"

    @property
    def display_name(self) -> str:
        return "Shell Command"

    @property
    def description(self) -> str:
        return "Command execution with stdout, stderr, and exit code"

    def summarize_for_ai(self, output: dict[str, Any]) -> str:
        lines: list[str] = []
        step_name = output.get("step_name", "Shell Command")
        command = output.get("command", "")
        working_directory = output.get("working_directory")
        exit_code = output.get("exit_code", 0)
        duration = output.get("duration_ms")
        stdout = output.get("stdout", "")
        stderr = output.get("stderr", "")
        extractions = output.get("extractions")

        lines.append(f"### Shell Command: {step_name}")
        lines.append("```bash")
        lines.append(command)
        lines.append("```")

        if working_directory:
            lines.append(f"- **Working Directory:** {working_directory}")

        status_emoji = "✅" if exit_code == 0 else "❌"
        lines.append(f"- **Exit Code:** {status_emoji} {exit_code}")

        if duration is not None:
            lines.append(f"- **Duration:** {duration}ms")

        # Stdout
        if stdout and stdout.strip():
            lines.append("")
            lines.append("**Standard Output:**")
            lines.append("```")
            # Truncate long output
            truncated_stdout = stdout[:4000] + "\n... (truncated)" if len(stdout) > 4000 else stdout
            lines.append(truncated_stdout)
            lines.append("```")

        # Stderr
        if stderr and stderr.strip():
            lines.append("")
            lines.append("**Standard Error:**")
            lines.append("```")
            truncated_stderr = stderr[:2000] + "\n... (truncated)" if len(stderr) > 2000 else stderr
            lines.append(truncated_stderr)
            lines.append("```")

        # Extractions
        if extractions and len(extractions) > 0:
            lines.append("")
            lines.append("**Extracted Variables:**")
            for key, value in extractions.items():
                value_str = str(value)[:100]
                lines.append(f"- `{key}`: {value_str}")

        return "\n".join(lines)

    def get_assertable_fields(self, output: dict[str, Any]) -> list[AssertableField]:
        fields: list[AssertableField] = []

        # Exit code
        fields.append(
            AssertableField(
                path="exit_code",
                name="Exit Code",
                description="The command exit code (0 typically means success)",
                field_type="number",
                example_value=output.get("exit_code", 0),
                suggested_assertions=[
                    "Assert exit code equals 0",
                    "Assert exit code is in expected range",
                ],
            )
        )

        # Stdout
        stdout = output.get("stdout")
        if stdout:
            fields.append(
                AssertableField(
                    path="stdout",
                    name="Standard Output",
                    description="The command standard output",
                    field_type="string",
                    example_value=stdout[:200] + "..." if len(stdout) > 200 else stdout,
                    suggested_assertions=[
                        "Assert stdout contains expected text",
                        "Assert stdout matches pattern",
                        "Parse stdout as JSON and validate",
                    ],
                )
            )

        # Stderr
        stderr = output.get("stderr")
        if stderr:
            fields.append(
                AssertableField(
                    path="stderr",
                    name="Standard Error",
                    description="The command standard error output",
                    field_type="string",
                    example_value=stderr[:200] + "..." if len(stderr) > 200 else stderr,
                    suggested_assertions=[
                        "Assert stderr is empty (no errors)",
                        "Assert stderr contains expected warning",
                    ],
                )
            )

        # Success
        fields.append(
            AssertableField(
                path="success",
                name="Command Success",
                description="Whether the command completed successfully",
                field_type="boolean",
                example_value=output.get("success", True),
                suggested_assertions=["Assert command succeeded"],
            )
        )

        # Duration
        duration = output.get("duration_ms")
        if duration is not None:
            fields.append(
                AssertableField(
                    path="duration_ms",
                    name="Execution Duration",
                    description="Command execution time in milliseconds",
                    field_type="number",
                    example_value=duration,
                    suggested_assertions=[
                        "Assert execution time is under threshold",
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
                        description=f"Variable extracted from output: {key}",
                        field_type=self._get_type(value),
                        example_value=value,
                        suggested_assertions=[
                            f"Assert {key} equals expected value",
                            f"Assert {key} is not null",
                        ],
                    )
                )

        return fields

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
