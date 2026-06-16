"""Find existing log files and directories in a project.

Used by ``find_log_files`` and ``suggest_log_sources`` MCP tools to discover
log output on disk and generate ready-to-use log source configurations that
are compatible with the runner's ``GlobalLogSource`` format.
"""

from __future__ import annotations

import asyncio
import logging
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from uuid import uuid4

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

#: Directories to skip during traversal.
SKIP_DIRS: frozenset[str] = frozenset(
    {
        "node_modules",
        ".git",
        "__pycache__",
        ".venv",
        "venv",
        "target",
        "dist",
        "build",
        ".next",
        ".cache",
    }
)

#: Glob-style patterns for log files (matched case-insensitively).
LOG_FILE_PATTERNS: frozenset[str] = frozenset(
    {
        "*.log",
        "*.log.*",
        "*.jsonl",
        "*.err.log",
    }
)

#: Well-known log directory names.
LOG_DIR_NAMES: frozenset[str] = frozenset(
    {
        "logs",
        "log",
        ".logs",
        ".dev-logs",
    }
)

#: Common specific log filenames to look for explicitly.
COMMON_LOG_FILES: frozenset[str] = frozenset(
    {
        "debug.log",
        "error.log",
        "app.log",
        "access.log",
        "npm-debug.log",
        "yarn-error.log",
        "lerna-debug.log",
    }
)

#: Valid runner log-source categories.
VALID_CATEGORIES: frozenset[str] = frozenset(
    {
        "frontend",
        "backend",
        "api",
        "mobile",
        "database",
        "build",
        "testing",
        "runner",
        "general",
    }
)

#: Default category when the framework's category cannot be mapped.
DEFAULT_CATEGORY: str = "general"

#: Maximum directory depth to descend.
MAX_DEPTH: int = 4

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _format_guess(path: Path) -> str:
    """Guess the log format based on file extension.

    Returns ``"json"``, ``"jsonl"``, or ``"plaintext"``.
    """
    suffix = path.suffix.lower()
    if suffix == ".json":
        return "json"
    if suffix == ".jsonl":
        return "jsonl"
    return "plaintext"


def _is_log_file(filename: str) -> bool:
    """Return ``True`` if *filename* looks like a log file."""
    lower = filename.lower()

    # Exact match on common log files.
    if lower in COMMON_LOG_FILES:
        return True

    # Extension-based checks.
    if lower.endswith(".log"):
        return True
    if lower.endswith(".jsonl"):
        return True
    if lower.endswith(".err.log"):
        return True

    # Rotated logs like app.log.1, app.log.2, etc.
    parts = lower.rsplit(".", 1)
    if len(parts) == 2 and parts[1].isdigit():
        base = parts[0]
        if base.endswith(".log"):
            return True

    return False


def _modified_iso(path: Path) -> str | None:
    """Return the ISO-formatted last-modified time of *path*, or ``None``."""
    try:
        mtime = path.stat().st_mtime
        return datetime.fromtimestamp(mtime, tz=timezone.utc).isoformat()
    except OSError:
        return None


def _file_size(path: Path) -> int:
    """Return the file size in bytes, or ``0`` on error."""
    try:
        return path.stat().st_size
    except OSError:
        return 0


def _map_framework_category(framework_category: str | None) -> str:
    """Map a framework detection category to a valid runner log-source category."""
    if framework_category is None:
        return DEFAULT_CATEGORY

    normalized = framework_category.lower().strip()

    # Direct match.
    if normalized in VALID_CATEGORIES:
        return normalized

    # Common mappings.
    mapping: dict[str, str] = {
        "fullstack": "backend",
        "system": "backend",
        "web-frontend": "frontend",
        "web-backend": "backend",
        "web": "frontend",
        "server": "backend",
        "rest-api": "api",
        "graphql": "api",
        "ios": "mobile",
        "android": "mobile",
        "react-native": "mobile",
        "flutter": "mobile",
        "db": "database",
        "sql": "database",
        "ci": "build",
        "ci-cd": "build",
        "bundler": "build",
        "test": "testing",
        "e2e": "testing",
        "unit-test": "testing",
        "automation": "runner",
        "desktop": "runner",
    }

    return mapping.get(normalized, DEFAULT_CATEGORY)


# ---------------------------------------------------------------------------
# find_log_files
# ---------------------------------------------------------------------------


def _is_under_source_dir(path: Path, root: Path) -> bool:
    """Return ``True`` if *path* is inside a ``src/`` directory relative to *root*."""
    try:
        rel = path.relative_to(root)
    except ValueError:
        return False
    return any(part.lower() == "src" for part in rel.parts)


def _scan_log_files_sync(root: str) -> list[dict[str, Any]]:
    """Walk the directory tree synchronously and collect log files/dirs.

    Returns a list of dicts describing each discovered log artefact.
    """
    root_path = Path(root).resolve()
    if not root_path.is_dir():
        logger.warning("find_log_files: %s is not a directory", root_path)
        return []

    root_depth = len(root_path.parts)
    results: list[dict[str, Any]] = []
    seen_paths: set[str] = set()
    # Track discovered log directories so we can skip individual files inside them.
    log_dir_paths: set[str] = set()

    for dirpath, dirnames, filenames in os.walk(root_path, topdown=True):
        current = Path(dirpath)
        depth = len(current.parts) - root_depth

        # Prune beyond max depth.
        if depth >= MAX_DEPTH:
            dirnames.clear()
            continue

        # Prune skipped directories (in-place so os.walk respects it).
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)

        # Check if this directory itself is a log directory.
        if current.name.lower() in LOG_DIR_NAMES and current != root_path:
            # Skip .dev-logs in per-project scans — handled at workspace level.
            if current.name.lower() == ".dev-logs":
                dirnames.clear()
                continue

            # Skip log dirs under src/ (e.g., src/components/logs is source code).
            if _is_under_source_dir(current, root_path):
                continue

            abs_str = str(current)
            if abs_str not in seen_paths:
                seen_paths.add(abs_str)
                log_dir_paths.add(abs_str)
                results.append(
                    {
                        "path": abs_str,
                        "name": current.name,
                        "type": "directory",
                        "size_bytes": 0,
                        "modified": _modified_iso(current),
                        "format_guess": "plaintext",
                    }
                )

        # Skip individual files if their parent directory is already a log source.
        if str(current) in log_dir_paths:
            continue

        # Check files in the current directory.
        for filename in filenames:
            if _is_log_file(filename):
                file_path = current / filename
                abs_str = str(file_path)
                if abs_str not in seen_paths:
                    seen_paths.add(abs_str)
                    results.append(
                        {
                            "path": abs_str,
                            "name": filename,
                            "type": "file",
                            "size_bytes": _file_size(file_path),
                            "modified": _modified_iso(file_path),
                            "format_guess": _format_guess(file_path),
                        }
                    )

    return results


async def find_log_files(project_path: str) -> list[dict[str, Any]]:
    """Scan a project directory for log files and log directories.

    Looks for files matching common log-file patterns (``*.log``, ``*.jsonl``,
    ``*.err.log``, etc.) and directories named ``logs``, ``log``, ``.logs``, or
    ``.dev-logs``.  Directories such as ``node_modules``, ``.git``, and other
    build artefact folders are skipped.

    Args:
        project_path: Root directory to scan.

    Returns:
        A list of dicts, each describing a discovered log file or directory
        with keys ``path``, ``name``, ``type``, ``size_bytes``, ``modified``,
        and ``format_guess``.
    """
    return await asyncio.to_thread(_scan_log_files_sync, project_path)


# ---------------------------------------------------------------------------
# suggest_log_sources
# ---------------------------------------------------------------------------


def _build_source_dict(
    *,
    name: str,
    description: str,
    category: str,
    source_type: str,
    path: str,
    format_guess: str,
    keywords: list[str] | None = None,
    parser: str | None = None,
    error_patterns: list[str] | None = None,
    warning_patterns: list[str] | None = None,
    pattern: str | None = None,
) -> dict[str, Any]:
    """Build a single log source config dict compatible with ``GlobalLogSource``."""
    return {
        "id": str(uuid4()),
        "name": name,
        "description": description,
        "category": category,
        "type": source_type,
        "path": path,
        "pattern": pattern if source_type == "directory" else None,
        "tail_lines": 100,
        "enabled": True,
        "color": None,
        "keywords": keywords or [],
        "format": format_guess,
        "parser": parser or "generic",
        "timestamp_pattern": None,
        "timezone": "local",
        "error_patterns": error_patterns or [],
        "warning_patterns": warning_patterns or [],
        "ignore_patterns": [],
        "poll_interval_ms": 5000,
    }


async def suggest_log_sources(project_path: str) -> dict[str, Any]:
    """Combine framework detection and log file discovery.

    Detects the project framework via
    :func:`qontinui_setup_mcp.discovery.frameworks.detect_framework`, then
    scans for existing log files with :func:`find_log_files`.  The results are
    merged into a list of log-source config dicts that are directly compatible
    with the runner's ``GlobalLogSource`` format.

    Args:
        project_path: Root directory of the project to analyse.

    Returns:
        A dict with keys:

        - ``framework`` -- the framework detection result dict.
        - ``found_log_files`` -- raw list from :func:`find_log_files`.
        - ``suggested_sources`` -- list of ``GlobalLogSource``-compatible
          config dicts ready to be pushed to the runner.
        - ``needs_logging_setup`` -- ``True`` if the framework does not log to
          files by default **and** no log files were found on disk.
    """
    from qontinui_setup_mcp.discovery.frameworks import detect_framework

    # Run detection and scanning concurrently.
    framework_result, found_log_files = await asyncio.gather(
        detect_framework(project_path),
        find_log_files(project_path),
    )

    # Extract framework metadata with safe defaults.
    framework_name: str = framework_result.get("framework", "Unknown")
    framework_key: str = framework_result.get("key", "unknown")
    framework_category: str | None = framework_result.get("category")
    framework_logs_to_files: bool = framework_result.get(
        "logs_to_file_by_default", True
    )

    # Look up the full definition to get patterns, keywords, etc.
    from qontinui_setup_mcp.discovery.frameworks import get_framework_definition

    fw_def = get_framework_definition(framework_key)
    framework_keywords: list[str] = list(fw_def.keywords) if fw_def else []
    framework_parser: str = fw_def.default_parser if fw_def else "generic"
    framework_error_patterns: list[str] = list(fw_def.error_patterns) if fw_def else []
    framework_warning_patterns: list[str] = (
        list(fw_def.warning_patterns) if fw_def else []
    )
    framework_default_log_locations: list[str] = (
        list(fw_def.default_log_locations) if fw_def else []
    )

    project_name = Path(project_path).resolve().name
    mapped_category = _map_framework_category(framework_category)

    suggested_sources: list[dict[str, Any]] = []
    seen_paths: set[str] = set()

    # 1. Sources from discovered log files / directories.
    for entry in found_log_files:
        entry_path: str = entry["path"]
        if entry_path in seen_paths:
            continue
        seen_paths.add(entry_path)

        entry_name: str = entry["name"]
        source_type: str = entry["type"]
        format_guess: str = entry["format_guess"]

        suggested_sources.append(
            _build_source_dict(
                name=f"{framework_name} - {entry_name}",
                description=f"Auto-discovered log source for {project_name}",
                category=mapped_category,
                source_type=source_type,
                path=entry_path,
                format_guess=format_guess,
                keywords=framework_keywords,
                parser=framework_parser,
                error_patterns=framework_error_patterns,
                warning_patterns=framework_warning_patterns,
                pattern="*.log" if source_type == "directory" else None,
            )
        )

    # 2. Add framework default log locations that exist on disk but were not
    #    already discovered (e.g., they may reside outside the scanned depth).
    for rel_path in framework_default_log_locations:
        log_path = Path(project_path).resolve() / rel_path
        abs_str = str(log_path)

        if abs_str in seen_paths:
            continue
        if not log_path.exists():
            continue

        seen_paths.add(abs_str)

        is_dir = log_path.is_dir()
        entry_type = "directory" if is_dir else "file"
        entry_format = "plaintext" if is_dir else _format_guess(log_path)

        suggested_sources.append(
            _build_source_dict(
                name=f"{framework_name} - {log_path.name}",
                description=f"Default log location for {framework_name}",
                category=mapped_category,
                source_type=entry_type,
                path=abs_str,
                format_guess=entry_format,
                keywords=framework_keywords,
                parser=framework_parser,
                error_patterns=framework_error_patterns,
                warning_patterns=framework_warning_patterns,
                pattern="*.log" if is_dir else None,
            )
        )

    needs_logging_setup = not framework_logs_to_files and len(found_log_files) == 0

    return {
        "framework": framework_result,
        "found_log_files": found_log_files,
        "suggested_sources": suggested_sources,
        "needs_logging_setup": needs_logging_setup,
    }


# ---------------------------------------------------------------------------
# suggest_workspace_sources — workspace-level log discovery
# ---------------------------------------------------------------------------

#: Patterns for categorising dev-log files by filename.
_DEV_LOG_CLASSIFICATION: list[tuple[str, str, str, str]] = [
    # (filename_contains, category, parser, display_name)
    ("backend.log", "backend", "python", "Backend"),
    ("backend.err.log", "backend", "python", "Backend Errors"),
    ("frontend.log", "frontend", "javascript", "Frontend"),
    ("frontend.err.log", "frontend", "javascript", "Frontend Errors"),
    ("runner-tauri.log", "runner", "rust", "Runner"),
    ("runner-actions.jsonl", "runner", "generic", "Runner Actions"),
    ("runner-general.jsonl", "runner", "generic", "Runner General"),
    ("runner-image-recognition.jsonl", "runner", "generic", "Image Recognition"),
    ("runner-playwright.jsonl", "testing", "generic", "Playwright"),
    ("ai-output.jsonl", "runner", "generic", "AI Output"),
    ("supervisor.log", "runner", "generic", "Supervisor"),
    ("supervisor.err.log", "runner", "generic", "Supervisor Errors"),
]

#: Log files to skip (temp copies, rotated, empty, huge debug dumps).
_DEV_LOG_SKIP_PATTERNS: frozenset[str] = frozenset(
    {
        "-copy",
        "-copy2",
        "-temp",
        "-temp2",
        "python-ws-debug",
        "backend-velocity",
        "logcat.log",
        "metro.log",
        "runner-build.log",
        "browser-events",
    }
)

#: Color assignments for important source categories.
_CATEGORY_COLORS: dict[str, str] = {
    "backend": "#22c55e",
    "frontend": "#3b82f6",
    "runner": "#f97316",
    "testing": "#a855f7",
}


def _classify_dev_log(filename: str) -> tuple[str, str, str, str] | None:
    """Return ``(display_name, category, parser, format)`` for a dev-log file.

    Returns ``None`` if the file should be skipped.
    """
    lower = filename.lower()

    # Skip temp/debug/copy files.
    for skip in _DEV_LOG_SKIP_PATTERNS:
        if skip in lower:
            return None

    # Match against known patterns.
    for pattern_file, category, parser, display_name in _DEV_LOG_CLASSIFICATION:
        if lower == pattern_file:
            fmt = "jsonl" if lower.endswith(".jsonl") else "plaintext"
            return (display_name, category, parser, fmt)

    return None


def _scan_workspace_sources_sync(workspace_path: str) -> dict[str, Any]:
    """Scan a workspace root for dev-log files and return classified sources."""
    root = Path(workspace_path).resolve()
    if not root.is_dir():
        return {"sources": [], "dev_logs_dir": None}

    # Look for .dev-logs directory.
    dev_logs_dir = root / ".dev-logs"
    if not dev_logs_dir.is_dir():
        return {"sources": [], "dev_logs_dir": None}

    sources: list[dict[str, Any]] = []

    for entry in sorted(dev_logs_dir.iterdir()):
        if not entry.is_file():
            continue
        if entry.stat().st_size == 0:
            continue

        classification = _classify_dev_log(entry.name)
        if classification is None:
            continue

        display_name, category, parser, fmt = classification

        source = _build_source_dict(
            name=display_name,
            description=f"Dev log from .dev-logs/{entry.name}",
            category=category,
            source_type="file",
            path=str(entry),
            format_guess=fmt,
            parser=parser,
        )
        source["color"] = _CATEGORY_COLORS.get(category)
        sources.append(source)

    return {
        "sources": sources,
        "dev_logs_dir": str(dev_logs_dir),
    }


async def suggest_workspace_sources(workspace_path: str) -> dict[str, Any]:
    """Discover dev-log sources at the workspace root level.

    Scans for a ``.dev-logs`` directory in *workspace_path* and classifies
    individual log files into categorised source configs (backend, frontend,
    runner, etc.).

    Args:
        workspace_path: Root directory of the workspace (parent of projects).

    Returns:
        A dict with keys ``sources`` (list of ``GlobalLogSource``-compatible
        dicts) and ``dev_logs_dir`` (path to the ``.dev-logs`` directory, or
        ``None`` if not found).
    """
    return await asyncio.to_thread(_scan_workspace_sources_sync, workspace_path)
