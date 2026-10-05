#!/usr/bin/env python3
"""The compute half of coord's repo-declared CI repairs.

Run by `.github/workflows/coord-repair.yml` (plan
`2026-09-24-coord-deterministic-ci-repair-lane` §3 recipes 2/3, §5.1). Coord
dispatches that workflow for one PR head and one failing CI step; this script
finds the `[[repair]]` the repo declared for that step, runs it against the PR
head, and leaves `repair.patch` + `report.json` for the workflow to upload.

IT WRITES NOTHING ANYWHERE BUT ITS OUTPUT DIRECTORY AND THE `work/` CHECKOUT.
It never pushes, comments or labels: coord downloads the artifact, re-validates
the patch against the declaration (allowed_paths, `git apply --check` on
head_sha, `git diff -w` empty for a format repair) and is the only writer.

Two subcommands, because the toolchain setup the declared command needs has to
run BETWEEN them:

  select  validate the inputs, read the DEFAULT branch's `.qontinui/ci.toml`
          (never the PR's: a PR must not be able to declare its own repair),
          and record the single `[[repair]]` whose `step` and `kind` both match.
          No match is a failed job and no artifact — coord reads a missing
          artifact as a refusal.
  run     compute the PR's changed files (merge-base with the default branch),
          run the declared `command` argv in `work/`, write
          `git diff --binary` as `repair.patch`, run the declared `check` argv,
          and write `report.json`. A non-zero command or check is RECORDED, not
          raised: it is the verifier's answer, and coord needs to read it.

Inputs come only from the environment (the workflow passes `${{ inputs.* }}`
through `env:`, never into a shell line):

  COORD_REPAIR_PR, COORD_REPAIR_HEAD_SHA, COORD_REPAIR_STEP,
  COORD_REPAIR_KIND, COORD_REPAIR_CORRELATION_ID, COORD_REPAIR_DEFAULT_BRANCH

The declared command sees, in addition to the job environment:

  COORD_REPAIR_CHANGED_FILES  absolute path of a newline-separated list of the
                              PR's changed paths (repo-relative)
  COORD_REPAIR_KIND, COORD_REPAIR_STEP

Self-contained on purpose (stdlib only, python >= 3.11 for tomllib): each repo
carries its own copy so no repo's repair depends on another repo's tree.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import NoReturn

SCHEMA = "coord-repair/1"
KINDS = ("format", "regen")

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
PR_RE = re.compile(r"^[1-9][0-9]{0,9}$")
CORRELATION_RE = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
# A git ref name the default branch can plausibly have; it is only ever used
# as an argv element, but refuse anything that could read as an option.
BRANCH_RE = re.compile(r"^[A-Za-z0-9._/-]{1,255}$")
MAX_STEP_LEN = 256

# Bound on each of `command` and `check`. The job's own timeout-minutes is the
# backstop; this one fires first so the artifact still uploads with a recorded
# 124, instead of the job dying with no artifact at all.
DEFAULT_ARGV_TIMEOUT_SECS = 20 * 60
EXIT_TIMEOUT = 124
EXIT_NOT_FOUND = 127


def die(msg: str) -> NoReturn:
    print(f"::error title=coord-repair::{msg}", file=sys.stderr)
    sys.exit(1)


def env(name: str) -> str:
    value = os.environ.get(name)
    if value is None or value == "":
        die(f"{name} is not set")
    return value


def validated_inputs() -> dict:
    pr = env("COORD_REPAIR_PR")
    head_sha = env("COORD_REPAIR_HEAD_SHA")
    step = env("COORD_REPAIR_STEP")
    kind = env("COORD_REPAIR_KIND")
    correlation_id = env("COORD_REPAIR_CORRELATION_ID")
    default_branch = env("COORD_REPAIR_DEFAULT_BRANCH")
    if not PR_RE.match(pr):
        die("input pr must be a positive integer")
    if not SHA_RE.match(head_sha):
        die("input head_sha must be 40 lowercase hex characters")
    if kind not in KINDS:
        die(f"input kind must be one of {KINDS}")
    if not CORRELATION_RE.match(correlation_id):
        die("input correlation_id must match ^[A-Za-z0-9._-]{1,64}$")
    if not step.strip() or len(step) > MAX_STEP_LEN or any(ord(c) < 0x20 for c in step):
        die(
            f"input step must be non-empty, <= {MAX_STEP_LEN} chars, no control characters"
        )
    if not BRANCH_RE.match(default_branch) or default_branch.startswith("-"):
        die("default branch name is not a plain ref name")
    return {
        "pr": int(pr),
        "head_sha": head_sha,
        "step": step,
        "kind": kind,
        "correlation_id": correlation_id,
        "default_branch": default_branch,
    }


def argv_field(repair: dict, field: str, index: int) -> list[str]:
    value = repair.get(field)
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(t, str) for t in value)
        or not value[0].strip()
    ):
        die(f"repair[{index}].{field} must be a non-empty argv array of strings")
    return value


def cmd_select(args: argparse.Namespace) -> None:
    inputs = validated_inputs()
    manifest = Path(args.manifest)
    if not manifest.is_file():
        die(
            f"{manifest} not found on the default branch — this repo declares no repairs"
        )
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as e:
        die(f"{manifest} does not parse: {e}")
    repairs = data.get("repair", [])
    if not isinstance(repairs, list):
        die(f"{manifest}: `repair` must be an array of tables")
    matches = [
        (i, r)
        for i, r in enumerate(repairs)
        if isinstance(r, dict)
        and r.get("step") == inputs["step"]
        and r.get("kind") == inputs["kind"]
    ]
    if not matches:
        die(
            f"no [[repair]] in the default branch's {manifest} declares "
            f"step={inputs['step']!r} kind={inputs['kind']!r}"
        )
    if len(matches) > 1:
        die(
            f"{len(matches)} [[repair]] entries declare that step and kind; refusing to guess"
        )
    index, repair = matches[0]
    selected = {
        "index": index,
        "step": repair["step"],
        "kind": repair["kind"],
        "command": argv_field(repair, "command", index),
        "check": argv_field(repair, "check", index),
        "allowed_paths": repair.get("allowed_paths", []),
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(selected, indent=2) + "\n", encoding="utf-8")
    print(
        f"selected repair[{index}]: kind={selected['kind']} command={selected['command']}"
    )


def git(work: Path, *argv: str) -> bytes:
    proc = subprocess.run(
        ["git", "-C", str(work), *argv],
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        die(
            f"git {' '.join(argv)} exited {proc.returncode}: {proc.stderr.decode(errors='replace').strip()}"
        )
    return proc.stdout


def changed_files(work: Path, head_sha: str, default_branch: str) -> list[str]:
    base_ref = f"refs/remotes/origin/{default_branch}"
    merge_base = git(work, "merge-base", head_sha, base_ref).decode().strip()
    if not SHA_RE.match(merge_base):
        die(f"merge-base of {head_sha} and {base_ref} is not a commit: {merge_base!r}")
    raw = git(work, "diff", "--name-only", "-z", "--no-renames", merge_base, head_sha)
    names = [n.decode("utf-8", errors="surrogateescape") for n in raw.split(b"\0") if n]
    # A newline inside a path cannot be carried by a newline-separated list;
    # drop it rather than let it split into two bogus entries.
    return [n for n in names if "\n" not in n and "\r" not in n]


def run_argv(
    argv: list[str], work: Path, extra_env: dict, timeout: int, label: str
) -> int:
    print(f"::group::{label}: {argv}")
    try:
        proc = subprocess.run(
            argv,
            cwd=str(work),
            env={**os.environ, **extra_env},
            timeout=timeout,
            check=False,
        )
        code = proc.returncode
    except FileNotFoundError:
        print(f"{label}: {argv[0]!r} not found", file=sys.stderr)
        code = EXIT_NOT_FOUND
    except subprocess.TimeoutExpired:
        print(f"{label}: timed out after {timeout}s", file=sys.stderr)
        code = EXIT_TIMEOUT
    print("::endgroup::")
    print(f"{label} exit: {code}")
    return code


def cmd_run(args: argparse.Namespace) -> None:
    inputs = validated_inputs()
    selected = json.loads(Path(args.selected).read_text(encoding="utf-8"))
    if selected.get("step") != inputs["step"] or selected.get("kind") != inputs["kind"]:
        die("the selected repair does not match the inputs")
    work = Path(args.work).resolve()
    out_dir = Path(args.out_dir).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    for stale in ("repair.patch", "report.json", "changed-files.txt"):
        (out_dir / stale).unlink(missing_ok=True)

    head = git(work, "rev-parse", "HEAD").decode().strip()
    if head != inputs["head_sha"]:
        die(f"work/ is at {head}, not the requested head_sha {inputs['head_sha']}")
    if (
        subprocess.run(
            ["git", "-C", str(work), "diff", "--quiet", "HEAD"], check=False
        ).returncode
        != 0
    ):
        die(
            "work/ has tracked modifications before the repair ran; the patch would not be the repair's alone"
        )

    changed = changed_files(work, inputs["head_sha"], inputs["default_branch"])
    changed_path = out_dir / "changed-files.txt"
    changed_path.write_text(
        "".join(f"{n}\n" for n in changed), encoding="utf-8", errors="surrogateescape"
    )
    print(f"{len(changed)} changed file(s) vs origin/{inputs['default_branch']}")

    timeout = int(
        os.environ.get("COORD_REPAIR_ARGV_TIMEOUT_SECS", DEFAULT_ARGV_TIMEOUT_SECS)
    )
    extra_env = {
        "COORD_REPAIR_CHANGED_FILES": str(changed_path),
        "COORD_REPAIR_KIND": inputs["kind"],
        "COORD_REPAIR_STEP": inputs["step"],
    }
    command_exit = run_argv(selected["command"], work, extra_env, timeout, "command")

    patch = git(
        work,
        "-c",
        "core.quotePath=false",
        "diff",
        "--binary",
        "--no-color",
        "--no-ext-diff",
    )
    (out_dir / "repair.patch").write_bytes(patch)
    print(f"repair.patch: {len(patch)} bytes")

    check_exit = run_argv(selected["check"], work, extra_env, timeout, "check")

    report = {
        "schema": SCHEMA,
        "pr": inputs["pr"],
        "head_sha": inputs["head_sha"],
        "step": inputs["step"],
        "kind": inputs["kind"],
        "command_exit": command_exit,
        "check_exit": check_exit,
        "changed_files": changed,
    }
    (out_dir / "report.json").write_text(
        json.dumps(report, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
        errors="surrogateescape",
    )
    print(json.dumps({k: v for k, v in report.items() if k != "changed_files"}))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    p_select = sub.add_parser(
        "select", help="validate inputs and pick the declared [[repair]]"
    )
    p_select.add_argument(
        "--manifest", required=True, help="the DEFAULT branch's .qontinui/ci.toml"
    )
    p_select.add_argument(
        "--out", required=True, help="where to write the selected repair (JSON)"
    )
    p_select.set_defaults(func=cmd_select)
    p_run = sub.add_parser(
        "run", help="run the selected repair against work/ and write the artifact files"
    )
    p_run.add_argument("--selected", required=True)
    p_run.add_argument("--work", required=True, help="the PR-head checkout")
    p_run.add_argument("--out-dir", required=True)
    p_run.set_defaults(func=cmd_run)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
