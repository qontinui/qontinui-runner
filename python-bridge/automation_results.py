"""Automation Results Management for qontinui-runner.

This module handles saving automation execution results for QA feedback loops.
Results are saved to a standardized location for analysis by Claude Code.
"""

import json
import logging
import shutil
from pathlib import Path
from typing import Any

from qontinui_schemas.common import utc_now

logger = logging.getLogger(__name__)

# Result storage locations
AUTOMATION_RESULTS_DIR = Path.home() / ".automation-results"
DEV_LOGS_DIR = Path("C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs")
MAX_HISTORY_RUNS = 10


def save_automation_results(
    execution_id: str,
    config_path: str,
    workflow_name: str,
    success: bool,
    duration_ms: int,
    error: str | None,
    events: list[dict[str, Any]],
    monitor: int | str | None = None,
) -> Path:
    """Save automation results to filesystem for QA feedback loop.

    This saves execution results to a standardized location that can be
    analyzed by Claude Code's /analyze-automation and /qa commands.

    Args:
        execution_id: Unique execution identifier
        config_path: Path to configuration file
        workflow_name: Name of executed workflow
        success: Whether execution succeeded
        duration_ms: Execution duration in milliseconds
        error: Error message if failed, None if successful
        events: List of execution events
        monitor: Monitor identifier where execution ran

    Returns:
        Path to the saved execution.json file
    """
    latest_dir = AUTOMATION_RESULTS_DIR / "latest"
    history_dir = AUTOMATION_RESULTS_DIR / "history"
    latest_logs_dir = latest_dir / "logs"
    latest_screenshots_dir = latest_dir / "screenshots"

    # Create directory structure
    for d in [latest_dir, history_dir, latest_logs_dir, latest_screenshots_dir]:
        d.mkdir(parents=True, exist_ok=True)

    # Archive previous latest to history (if exists)
    existing_execution_file = latest_dir / "execution.json"
    if existing_execution_file.exists():
        try:
            with open(existing_execution_file) as f:
                prev_data = json.load(f)
                prev_id = prev_data.get("execution_id", "unknown")
                prev_timestamp = (
                    prev_data.get("timestamp", "unknown").replace(":", "-").replace(".", "-")
                )

            history_entry_name = f"{prev_timestamp}_{prev_id[:8]}"
            history_entry_dir = history_dir / history_entry_name

            if not history_entry_dir.exists():
                shutil.copytree(latest_dir, history_entry_dir)
                logger.info(f"Archived previous run to history: {history_entry_name}")

            # Clean up old history entries
            entries = sorted(
                [d for d in history_dir.iterdir() if d.is_dir()],
                key=lambda x: x.stat().st_mtime,
                reverse=True,
            )
            for old_entry in entries[MAX_HISTORY_RUNS:]:
                try:
                    shutil.rmtree(old_entry)
                except Exception as e:
                    logger.warning(f"Failed to remove old history entry: {e}")
        except Exception as e:
            logger.warning(f"Failed to archive previous results: {e}")

    # Clear latest directory
    for item in latest_dir.iterdir():
        if item.is_file():
            item.unlink()
        elif item.is_dir():
            shutil.rmtree(item)

    # Recreate subdirectories
    latest_logs_dir.mkdir(exist_ok=True)
    latest_screenshots_dir.mkdir(exist_ok=True)

    # Capture log snapshots from .dev-logs
    if DEV_LOGS_DIR.exists():
        log_files = [
            "backend.log",
            "frontend.log",
            "qontinui-api.log",
            "runner.log",
            "runner-tauri.log",
        ]
        for log_file in log_files:
            src_log = DEV_LOGS_DIR / log_file
            if src_log.exists():
                try:
                    with open(src_log, errors="ignore") as f:
                        lines = f.readlines()
                        last_lines = lines[-500:] if len(lines) > 500 else lines

                    dst_log = latest_logs_dir / log_file
                    with open(dst_log, "w") as f:
                        f.writelines(last_lines)
                except Exception as e:
                    logger.warning(f"Failed to capture log {log_file}: {e}")

        # Copy AI output log (complete file, not truncated)
        ai_output_log = DEV_LOGS_DIR / "ai-output.jsonl"
        if ai_output_log.exists():
            try:
                shutil.copy2(ai_output_log, latest_logs_dir / "ai-output.jsonl")
                logger.info("Copied AI output log to automation results")
            except Exception as e:
                logger.warning(f"Failed to copy AI output log: {e}")

    # Build execution results JSON
    timestamp = utc_now().isoformat()

    execution_result: dict[str, Any] = {
        "execution_id": execution_id,
        "config_path": config_path,
        "workflow_name": workflow_name,
        "monitor": monitor,
        "success": success,
        "duration_ms": duration_ms,
        "timestamp": timestamp,
        "error": error,
        "summary": {
            "total_events": len(events),
            "test_results_count": 0,
            "console_errors_count": 0,
            "network_failures_count": 0,
        },
        "test_results": [],
        "console_errors": [],
        "network_failures": [],
        "screenshots": [],
        "log_snapshots": {
            "backend": (
                str(latest_logs_dir / "backend.log")
                if (latest_logs_dir / "backend.log").exists()
                else None
            ),
            "frontend": (
                str(latest_logs_dir / "frontend.log")
                if (latest_logs_dir / "frontend.log").exists()
                else None
            ),
            "api": (
                str(latest_logs_dir / "qontinui-api.log")
                if (latest_logs_dir / "qontinui-api.log").exists()
                else None
            ),
            "runner": (
                str(latest_logs_dir / "runner.log")
                if (latest_logs_dir / "runner.log").exists()
                else None
            ),
            "runner_tauri": (
                str(latest_logs_dir / "runner-tauri.log")
                if (latest_logs_dir / "runner-tauri.log").exists()
                else None
            ),
            "ai_output": (
                str(latest_logs_dir / "ai-output.jsonl")
                if (latest_logs_dir / "ai-output.jsonl").exists()
                else None
            ),
        },
    }

    # Write execution.json
    execution_file = latest_dir / "execution.json"
    with open(execution_file, "w") as f:
        json.dump(execution_result, f, indent=2)

    logger.info(f"Saved automation results to {execution_file}")
    return execution_file


def get_latest_results() -> dict[str, Any] | None:
    """Get the latest automation results.

    Returns:
        Latest execution results as dictionary, or None if no results exist
    """
    latest_dir = AUTOMATION_RESULTS_DIR / "latest"
    execution_file = latest_dir / "execution.json"

    if not execution_file.exists():
        return None

    try:
        with open(execution_file) as f:
            return json.load(f)  # type: ignore[no-any-return]
    except Exception as e:
        logger.error(f"Failed to load latest results: {e}")
        return None


def get_history_results(limit: int = 10) -> list[dict[str, Any]]:
    """Get historical automation results.

    Args:
        limit: Maximum number of historical results to return

    Returns:
        List of historical execution results
    """
    history_dir = AUTOMATION_RESULTS_DIR / "history"

    if not history_dir.exists():
        return []

    results = []
    entries = sorted(
        [d for d in history_dir.iterdir() if d.is_dir()],
        key=lambda x: x.stat().st_mtime,
        reverse=True,
    )

    for entry_dir in entries[:limit]:
        execution_file = entry_dir / "execution.json"
        if execution_file.exists():
            try:
                with open(execution_file) as f:
                    results.append(json.load(f))
            except Exception as e:
                logger.warning(f"Failed to load history result from {entry_dir}: {e}")

    return results
