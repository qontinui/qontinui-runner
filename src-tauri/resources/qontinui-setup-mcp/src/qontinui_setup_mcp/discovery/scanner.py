"""Scan a directory tree to discover software projects by manifest files."""

from __future__ import annotations

import asyncio
import logging
import os
from pathlib import Path
from typing import TypedDict

logger = logging.getLogger(__name__)

# Directories to skip during traversal.
SKIP_DIRS: frozenset[str] = frozenset(
    {
        "node_modules",
        ".git",
        "__pycache__",
        ".venv",
        "venv",
        ".env",
        "target",
        "dist",
        "build",
        ".next",
        ".cache",
        ".tox",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
    }
)

# Mapping of manifest filename → project type.
MANIFEST_MAP: dict[str, str] = {
    "package.json": "node",
    "pyproject.toml": "python",
    "Cargo.toml": "rust",
    "go.mod": "go",
    "pom.xml": "java-maven",
    "build.gradle": "java-gradle",
    "build.gradle.kts": "java-gradle",
    "Gemfile": "ruby",
    "pubspec.yaml": "flutter",
    "composer.json": "php",
}

# Glob-style extensions that need special handling.
DOTNET_EXTENSIONS: frozenset[str] = frozenset({".sln", ".csproj"})


class ProjectInfo(TypedDict):
    """Information about a discovered project."""

    path: str
    name: str
    type: str
    manifest: str


def _scan_sync(root: str, max_depth: int) -> list[ProjectInfo]:
    """Walk the directory tree synchronously and collect projects.

    A directory is considered a project only if it contains a ``.git``
    directory (i.e., it is a git repository root).  This prevents
    subdirectories with manifest files (like ``backend/`` inside a monorepo)
    from appearing as separate projects.

    The scan root itself is never reported — it is treated as a workspace
    container, not a project.
    """
    root_path = Path(root).resolve()
    if not root_path.is_dir():
        logger.warning("scan_workspace: %s is not a directory", root_path)
        return []

    root_depth = len(root_path.parts)
    projects: list[ProjectInfo] = []

    for dirpath, dirnames, filenames in os.walk(root_path, topdown=True):
        current = Path(dirpath)
        depth = len(current.parts) - root_depth

        # Check for .git BEFORE pruning it from dirnames.
        has_git = ".git" in dirnames

        # Prune skipped directories (in-place so os.walk respects it).
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)

        # Stop descending beyond max_depth (but still check this directory).
        if depth >= max_depth:
            dirnames.clear()

        # Only directories with .git are real project roots.
        # Skip the scan root (depth 0) — it's a workspace container.
        if not has_git or depth == 0:
            continue

        # Check for manifest files in this directory.
        filenames_set = set(filenames)
        found = False

        # Exact-name manifests.
        for manifest_name, project_type in MANIFEST_MAP.items():
            if manifest_name in filenames_set:
                projects.append(
                    ProjectInfo(
                        path=str(current),
                        name=current.name,
                        type=project_type,
                        manifest=manifest_name,
                    )
                )
                found = True
                break

        # .NET projects (*.sln, *.csproj).
        if not found:
            for filename in filenames:
                if Path(filename).suffix in DOTNET_EXTENSIONS:
                    projects.append(
                        ProjectInfo(
                            path=str(current),
                            name=current.name,
                            type="dotnet",
                            manifest=filename,
                        )
                    )
                    break

    return projects


async def scan_workspace(path: str, max_depth: int = 3) -> list[ProjectInfo]:
    """Scan a directory tree for software projects.

    Walks the filesystem up to *max_depth* levels below *path*, looking for
    directories that are git repositories (contain a ``.git`` directory) with
    well-known manifest files.

    Args:
        path: Root directory to scan.
        max_depth: Maximum directory depth to descend (default ``3``).

    Returns:
        A list of :class:`ProjectInfo` dicts, one per discovered project.
    """
    return await asyncio.to_thread(_scan_sync, path, max_depth)
