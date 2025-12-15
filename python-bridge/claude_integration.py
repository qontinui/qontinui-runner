"""Claude Code integration module for Qontinui Runner.

This module provides functionality to trigger Claude Code analysis of automation
results. It's designed to be called from SHELL actions or after workflow completion.

The module handles platform-specific invocation:
- On Windows: Invokes Claude Code via WSL (where MCP configuration exists)
- On Linux/macOS: Invokes Claude Code directly

Usage from SHELL action:
    python -c "from claude_integration import trigger_analysis; trigger_analysis()"

Or import and use programmatically:
    from claude_integration import trigger_analysis
    result = trigger_analysis()
"""

import json
import logging
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# Default paths - these can be overridden via environment variables
DEFAULT_RESULTS_DIR = ".automation-results/latest"
DEFAULT_CLAUDE_COMMAND_PATH = "qontinui-claude-config/.claude/commands/analyze-automation.md"


def get_automation_results_dir() -> Path:
    """Get the path to the automation results directory.

    Returns:
        Path to the automation results directory

    The path is determined by:
    1. QONTINUI_RESULTS_DIR environment variable if set
    2. Default: parent_dir/.automation-results/latest
    """
    if env_path := os.environ.get("QONTINUI_RESULTS_DIR"):
        return Path(env_path)

    # Default: look for results in the parent directory of the runner
    runner_dir = Path(__file__).parent.parent  # qontinui-runner
    parent_dir = runner_dir.parent  # qontinui_parent_directory
    return parent_dir / DEFAULT_RESULTS_DIR


def get_execution_metadata() -> dict[str, Any] | None:
    """Read execution metadata from the latest automation results.

    Returns:
        Dictionary with execution metadata, or None if not found
    """
    results_dir = get_automation_results_dir()
    execution_json = results_dir / "execution.json"

    if not execution_json.exists():
        logger.warning(f"No execution.json found at {execution_json}")
        return None

    try:
        with open(execution_json) as f:
            return json.load(f)
    except Exception as e:
        logger.error(f"Failed to read execution.json: {e}")
        return None


def get_claude_command_content() -> str | None:
    """Get the content of the analyze-automation command.

    Returns:
        The command content as a string, or None if not found
    """
    # Look for the command file
    runner_dir = Path(__file__).parent.parent
    parent_dir = runner_dir.parent

    # Check environment variable first
    if env_path := os.environ.get("QONTINUI_ANALYZE_COMMAND"):
        command_path = Path(env_path)
    else:
        command_path = parent_dir / DEFAULT_CLAUDE_COMMAND_PATH

    if not command_path.exists():
        logger.warning(f"analyze-automation command not found at {command_path}")
        return None

    try:
        return command_path.read_text()
    except Exception as e:
        logger.error(f"Failed to read analyze-automation command: {e}")
        return None


def is_claude_available() -> bool:
    """Check if Claude Code CLI is available.

    Returns:
        True if Claude Code CLI is available
    """
    system = platform.system()

    if system == "Windows":
        # On Windows, check if we can invoke via WSL
        try:
            result = subprocess.run(
                ["wsl.exe", "which", "claude"],
                capture_output=True,
                text=True,
                timeout=10,
            )
            return result.returncode == 0
        except Exception:
            return False
    else:
        # On Unix, check directly
        return shutil.which("claude") is not None


def trigger_analysis(
    timeout_seconds: int = 600,
    working_directory: str | None = None,
) -> dict[str, Any]:
    """Trigger Claude Code to analyze automation results.

    This function invokes Claude Code with the analyze-automation prompt to
    review the latest automation results, identify issues, and potentially
    fix them.

    Args:
        timeout_seconds: Maximum time to wait for analysis (default: 600s / 10min)
        working_directory: Working directory for Claude Code (default: auto-detect)

    Returns:
        Dictionary with:
        - success: bool - Whether the analysis was triggered successfully
        - output: str - Output from Claude Code (if successful)
        - error: str - Error message (if failed)
        - execution_metadata: dict - Metadata from the automation run
    """
    result = {
        "success": False,
        "output": "",
        "error": "",
        "execution_metadata": None,
    }

    # Check for automation results
    metadata = get_execution_metadata()
    if not metadata:
        result["error"] = "No automation results found. Run an automation first."
        return result

    result["execution_metadata"] = metadata

    # Log what we found
    logger.info("Found automation results:")
    logger.info(f"  Config: {metadata.get('config_path', 'unknown')}")
    logger.info(f"  Workflow: {metadata.get('workflow_name', 'unknown')}")
    logger.info(f"  Success: {metadata.get('success', 'unknown')}")
    logger.info(f"  Duration: {metadata.get('duration_ms', 0)}ms")

    # Get the command content
    command_content = get_claude_command_content()
    if not command_content:
        result["error"] = "analyze-automation command file not found"
        return result

    # Determine working directory
    if working_directory:
        cwd = Path(working_directory)
    else:
        runner_dir = Path(__file__).parent.parent
        cwd = runner_dir.parent  # qontinui_parent_directory

    # Build the command based on platform
    system = platform.system()

    if system == "Windows":
        # On Windows, invoke via WSL where Claude Code has MCP configured
        # Convert Windows path to WSL path
        wsl_cwd = str(cwd).replace("\\", "/")
        if wsl_cwd[1] == ":":
            # Convert C:\... to /mnt/c/...
            drive = wsl_cwd[0].lower()
            wsl_cwd = f"/mnt/{drive}/{wsl_cwd[3:]}"

        cmd = [
            "wsl.exe",
            "bash",
            "-c",
            f'cd "{wsl_cwd}" && claude -p "{command_content[:1000]}..." --output-format text --permission-mode bypassPermissions 2>&1',
        ]

        # For very long prompts, we need to pass via a temp file
        # Create a temp script that reads the full prompt
        script_content = f"""#!/bin/bash
cd "{wsl_cwd}"
PROMPT=$(cat <<'ENDOFPROMPT'
{command_content}
ENDOFPROMPT
)
claude -p "$PROMPT" --output-format text --permission-mode bypassPermissions 2>&1
"""

        # Write script to temp file
        import tempfile

        with tempfile.NamedTemporaryFile(mode="w", suffix=".sh", delete=False) as f:
            f.write(script_content)
            script_path = f.name

        # Convert script path to WSL path
        wsl_script = script_path.replace("\\", "/")
        if wsl_script[1] == ":":
            drive = wsl_script[0].lower()
            wsl_script = f"/mnt/{drive}/{wsl_script[3:]}"

        cmd = ["wsl.exe", "bash", wsl_script]

    else:
        # On Unix, invoke directly
        cmd = [
            "claude",
            "-p",
            command_content,
            "--output-format",
            "text",
            "--permission-mode",
            "bypassPermissions",
        ]

    # Execute Claude Code
    logger.info("Starting Claude Code analysis...")

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
            cwd=str(cwd) if system != "Windows" else None,
        )

        # Clean up temp script on Windows
        if system == "Windows" and "script_path" in locals():
            try:
                os.unlink(script_path)
            except Exception:
                pass

        result["output"] = proc.stdout

        if proc.returncode == 0:
            result["success"] = True
            logger.info("Claude Code analysis completed successfully")
        else:
            result["error"] = proc.stderr or f"Claude Code exited with code {proc.returncode}"
            logger.error(f"Claude Code failed: {result['error']}")

    except subprocess.TimeoutExpired:
        result["error"] = f"Analysis timed out after {timeout_seconds} seconds"
        logger.error(result["error"])

    except Exception as e:
        result["error"] = f"Failed to invoke Claude Code: {e}"
        logger.error(result["error"])

    return result


def main():
    """Command-line entry point for triggering analysis."""
    import argparse

    parser = argparse.ArgumentParser(
        description="Trigger Claude Code analysis of automation results"
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=600,
        help="Timeout in seconds (default: 600)",
    )
    parser.add_argument(
        "--working-dir",
        type=str,
        help="Working directory for Claude Code",
    )
    parser.add_argument(
        "--check-only",
        action="store_true",
        help="Only check if analysis can be triggered, don't run it",
    )
    parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Enable verbose logging",
    )

    args = parser.parse_args()

    # Configure logging
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s - %(levelname)s - %(message)s",
    )

    if args.check_only:
        # Just check prerequisites
        print("Checking prerequisites...")

        metadata = get_execution_metadata()
        if metadata:
            print("✓ Found automation results:")
            print(f"  Config: {metadata.get('config_path', 'unknown')}")
            print(f"  Workflow: {metadata.get('workflow_name', 'unknown')}")
            print(f"  Success: {metadata.get('success', 'unknown')}")
        else:
            print("✗ No automation results found")
            sys.exit(1)

        if get_claude_command_content():
            print("✓ analyze-automation command found")
        else:
            print("✗ analyze-automation command not found")
            sys.exit(1)

        if is_claude_available():
            print("✓ Claude Code CLI available")
        else:
            print("✗ Claude Code CLI not available")
            sys.exit(1)

        print("\nAll prerequisites met. Ready to trigger analysis.")
        sys.exit(0)

    # Run the analysis
    result = trigger_analysis(
        timeout_seconds=args.timeout,
        working_directory=args.working_dir,
    )

    if result["success"]:
        print(result["output"])
        sys.exit(0)
    else:
        print(f"Error: {result['error']}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
