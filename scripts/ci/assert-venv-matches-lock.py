#!/usr/bin/env python3
"""Fail when the running interpreter's venv drifts from a Poetry lock.

`poetry sync` makes the venv equal the lock; this script is the independent
observation that it did, so a restored cache, a stray `pip install` or a
resolver quirk cannot silently put Pillow, yarl or any other locked package
back into an advisory range.

Checks, for the platform this runs on (PEP 508 markers evaluated against the
running interpreter):
  * every installed distribution that the lock lists must be at the locked
    version;
  * every locked package the platform needs (non-optional groups) must be
    installed;
  * no installed distribution may be absent from the lock, except the build
    tooling in BUILD_TOOLING (PyInstaller is pip-installed after the sync).

Usage (from the venv, e.g. via `poetry run`):
    assert-venv-matches-lock.py python-bridge/poetry.lock [--groups main,dev]

Exit 0 conforms, 1 drift, 2 usage or unreadable input.
"""
from __future__ import annotations

import argparse
import re
import sys
import tomllib
from importlib import metadata
from pathlib import Path

from packaging.markers import Marker

# Installed by the workflow after the sync, or by the interpreter itself.
BUILD_TOOLING = {
    "pip",
    "setuptools",
    "wheel",
    "pyinstaller",
    "pyinstaller-hooks-contrib",
    "altgraph",
    "macholib",
    "pefile",
    "pywin32-ctypes",
}


def norm(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def installed_distributions() -> dict[str, str]:
    out: dict[str, str] = {}
    for dist in metadata.distributions():
        name = dist.metadata["Name"]
        if name:
            out[norm(name)] = dist.version
    return out


def _applies(markers, groups: set[str], env: dict[str, str] | None) -> bool:
    """Does a lock entry's `markers` select it for any of `groups`?"""
    if not markers:
        return True
    if isinstance(markers, str):
        return Marker(markers).evaluate(env)
    return any(g in markers and Marker(markers[g]).evaluate(env) for g in groups)


def check(
    lock: dict,
    installed: dict[str, str],
    groups: set[str],
    env: dict[str, str] | None = None,
) -> list[str]:
    problems: list[str] = []
    locked: dict[str, str] = {}
    required: set[str] = set()
    for pkg in lock["package"]:
        name = norm(pkg["name"])
        locked[name] = pkg["version"]
        in_groups = set(pkg.get("groups", ["main"])) & groups
        if in_groups and _applies(pkg.get("markers"), in_groups, env):
            required.add(name)
    for name, want in sorted(locked.items()):
        have = installed.get(name)
        if have is not None and have != want:
            problems.append(f"{name}: installed {have}, lock {want}")
    for name in sorted(required - installed.keys()):
        problems.append(f"{name}: locked {locked[name]} but not installed")
    for name in sorted(installed.keys() - locked.keys() - BUILD_TOOLING):
        problems.append(f"{name}: installed {installed[name]} but not in the lock")
    return problems


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("lock", type=Path)
    ap.add_argument("--groups", default="main,dev")
    args = ap.parse_args(argv)
    try:
        lock = tomllib.loads(args.lock.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        print(f"cannot read {args.lock}: {exc}", file=sys.stderr)
        return 2
    problems = check(lock, installed_distributions(), set(args.groups.split(",")))
    if problems:
        print(f"venv does not match {args.lock}:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1
    print(f"venv matches {args.lock} ({len(lock['package'])} locked packages)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
