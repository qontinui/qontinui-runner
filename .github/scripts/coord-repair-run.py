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
  run     compute the PR's changed files (merge-base with origin/<base_ref>),
          run the declared `command` argv in `work/`, stage what it changed
          INSIDE `allowed_paths` (new files included) and write that as
          `repair.patch`, run the declared `check` argv, and write
          `report.json`. A non-zero command or check is RECORDED, not raised:
          it is the verifier's answer, and coord needs to read it.

WHICH CODE RUNS. `command` and `check` run with cwd `work/` (the PR head). Any
argv element naming a repo script under `.github/` must be spelled
`../trusted/.github/...` — the DEFAULT branch's copy, checked out beside work/
— and `select` refuses any other spelling, so a PR cannot rewrite the repair
script it is being repaired by. The scripts act on their cwd's git toplevel,
i.e. on `work/`.

`check_exit` IS FORGEABLE whenever `check` executes PR code — `cargo test`,
`poetry run …`, anything that compiles or imports the PR's tree: the PR
controls what that code does and therefore what it exits with. It is evidence,
not proof. Coord's own re-validation of the patch (allowed_paths, apply-check,
whitespace-only for a format repair) is the boundary, never this number.

Inputs come only from the environment (the workflow passes `${{ inputs.* }}`
through `env:`, never into a shell line):

  COORD_REPAIR_PR, COORD_REPAIR_HEAD_SHA, COORD_REPAIR_BASE_REF,
  COORD_REPAIR_STEP, COORD_REPAIR_KIND, COORD_REPAIR_CORRELATION_ID

plus, for `run`, the time budget:

  COORD_REPAIR_STARTED_AT     epoch seconds the job started (first step)
  COORD_REPAIR_BUDGET_SECS    seconds from then that command + check may use
                              in total; the rest of the job timeout is left
                              for the artifact upload
  COORD_REPAIR_ARGV_CAP_SECS  optional per-argv ceiling inside that budget

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
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path, PurePosixPath
from typing import NoReturn

SCHEMA = "coord-repair/1"
KINDS = ("format", "regen")

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
PR_RE = re.compile(r"^[1-9][0-9]{0,9}$")
CORRELATION_RE = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
BASE_REF_RE = re.compile(r"^[A-Za-z0-9._/-]{1,200}$")
MAX_STEP_LEN = 256

# The only spelling under which an argv element may name a repo script.
TRUSTED_PREFIX = "../trusted/"

# Per-argv cap, and the default total budget when the workflow sets none. The
# total is a DEADLINE shared by command + check: each gets min(remaining, cap),
# so a slow command cannot push the check (and the upload) past the job's own
# timeout-minutes, which would lose the artifact entirely.
ARGV_CAP_SECS = 20 * 60
DEFAULT_BUDGET_SECS = 22 * 60
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
    base_ref = env("COORD_REPAIR_BASE_REF")
    step = env("COORD_REPAIR_STEP")
    kind = env("COORD_REPAIR_KIND")
    correlation_id = env("COORD_REPAIR_CORRELATION_ID")
    if not PR_RE.match(pr):
        die("input pr must be a positive integer")
    if not SHA_RE.match(head_sha):
        die("input head_sha must be 40 lowercase hex characters")
    if (
        not BASE_REF_RE.match(base_ref)
        or ".." in base_ref
        or base_ref.startswith(("-", "/"))
        or base_ref.endswith("/")
    ):
        die(
            "input base_ref must be a plain branch name (^[A-Za-z0-9._/-]{1,200}$, no '..', no leading '-')"
        )
    if kind not in KINDS:
        die(f"input kind must be one of {KINDS}")
    if not CORRELATION_RE.match(correlation_id):
        die("input correlation_id must match ^[A-Za-z0-9._-]{1,64}$")
    if not step.strip() or len(step) > MAX_STEP_LEN or any(ord(c) < 0x20 for c in step):
        die(
            f"input step must be non-empty, <= {MAX_STEP_LEN} chars, no control characters"
        )
    return {
        "pr": int(pr),
        "head_sha": head_sha,
        "base_ref": base_ref,
        "step": step,
        "kind": kind,
        "correlation_id": correlation_id,
    }


def names_repo_script(token: str) -> bool:
    """Does this argv element name something under a `.github` directory?"""
    return any(part == ".github" for part in re.split(r"[/\\=]", token))


def check_trusted_spelling(token: str, label: str) -> None:
    """A `.github/` path must be `../trusted/.github/...`, nothing looser."""
    if not names_repo_script(token):
        return
    if not token.startswith(TRUSTED_PREFIX + ".github/"):
        die(
            f"{label} element {token!r} names a repo script but is not spelled "
            f"'{TRUSTED_PREFIX}.github/...': it would run the PR head's copy"
        )
    rest = PurePosixPath(token[len(TRUSTED_PREFIX) :])
    if any(part in ("..", "") for part in rest.parts) or "\\" in token:
        die(f"{label} element {token!r} must not leave trusted/")


def argv_field(repair: dict, field: str, index: int) -> list[str]:
    value = repair.get(field)
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(t, str) for t in value)
        or not value[0].strip()
    ):
        die(f"repair[{index}].{field} must be a non-empty argv array of strings")
    for token in value:
        check_trusted_spelling(token, f"repair[{index}].{field}")
    return value


def allowed_paths_field(repair: dict, index: int) -> list[str]:
    value = repair.get("allowed_paths")
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(g, str) and g.strip() for g in value)
    ):
        die(f"repair[{index}].allowed_paths must be a non-empty array of globs")
    for g in value:
        p = PurePosixPath(g.strip())
        if (
            p.is_absolute()
            or ".." in p.parts
            or g.strip() in (".", "*", "**", "**/*", "./**")
        ):
            die(
                f"repair[{index}].allowed_paths entry {g!r} is not a narrow repo-relative glob"
            )
    return [g.strip() for g in value]


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
            f"no [[repair]] in the default branch's {manifest} declares step={inputs['step']!r} kind={inputs['kind']!r}"
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
        "allowed_paths": allowed_paths_field(repair, index),
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(selected, indent=2) + "\n", encoding="utf-8")
    print(
        f"selected repair[{index}]: kind={selected['kind']} command={selected['command']}"
    )


def git(work: Path, *argv: str) -> bytes:
    proc = subprocess.run(
        ["git", "-C", str(work), *argv], capture_output=True, check=False
    )
    if proc.returncode != 0:
        die(
            f"git {' '.join(argv)} exited {proc.returncode}: {proc.stderr.decode(errors='replace').strip()}"
        )
    return proc.stdout


def z_paths(raw: bytes) -> list[bytes]:
    return [n for n in raw.split(b"\0") if n]


def decode_paths(raw: bytes, what: str) -> list[str]:
    """Decode a -z path listing; a path a newline-separated list cannot carry
    faithfully (non-UTF-8, or containing a newline) fails the job — dropping it
    would understate what the PR or the repair touched."""
    out = []
    for n in z_paths(raw):
        try:
            s = n.decode("utf-8")
        except UnicodeDecodeError:
            die(
                f"{what}: a path is not valid UTF-8 ({n!r}); refusing to carry it in a newline-separated list"
            )
        if "\n" in s or "\r" in s:
            die(
                f"{what}: path {s!r} contains a newline; refusing to carry it in a newline-separated list"
            )
        out.append(s)
    return out


def changed_files(work: Path, head_sha: str, base_ref: str) -> list[str]:
    # coord-repair.yml checks work/ out with fetch-depth: 0, which fetches
    # every branch to refs/remotes/origin/*, so the PR's base is present.
    base = f"refs/remotes/origin/{base_ref}"
    if (
        subprocess.run(
            ["git", "-C", str(work), "rev-parse", "--verify", "--quiet", base],
            capture_output=True,
        ).returncode
        != 0
    ):
        die(
            f"{base} is not in work/ — base_ref {base_ref!r} is not a branch of this repository"
        )
    merge_base = git(work, "merge-base", head_sha, base).decode().strip()
    if not SHA_RE.match(merge_base):
        die(f"merge-base of {head_sha} and {base} is not a commit: {merge_base!r}")
    raw = git(work, "diff", "--name-only", "-z", "--no-renames", merge_base, head_sha)
    return decode_paths(raw, "changed files")


def glob_pathspecs(allowed_paths: list[str]) -> list[str]:
    # `:(glob)` gives `**` its gitignore meaning (any depth), matching how the
    # declaration's globs are read elsewhere; `top` anchors at the repo root.
    return [f":(top,glob){g}" for g in allowed_paths]


def run_argv(
    argv: list[str], work: Path, extra_env: dict, deadline: float, cap: int, label: str
) -> int:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        print(f"{label}: time budget exhausted before it could start", file=sys.stderr)
        return EXIT_TIMEOUT
    timeout = min(remaining, cap)
    print(f"::group::{label} (<= {int(timeout)}s): {argv}", flush=True)
    try:
        # Own process group, so a timeout kills everything the argv spawned
        # (cargo -> rustc, poetry -> python), not just the direct child.
        proc = subprocess.Popen(
            argv, cwd=str(work), env={**os.environ, **extra_env}, start_new_session=True
        )
    except FileNotFoundError:
        print("::endgroup::", flush=True)
        print(f"{label}: {argv[0]!r} not found", file=sys.stderr)
        return EXIT_NOT_FOUND
    except PermissionError:
        print("::endgroup::", flush=True)
        print(f"{label}: {argv[0]!r} is not executable", file=sys.stderr)
        return 126
    try:
        code = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()
        print("::endgroup::", flush=True)
        print(
            f"{label}: timed out after {int(timeout)}s; process group killed",
            file=sys.stderr,
        )
        return EXIT_TIMEOUT
    print("::endgroup::", flush=True)
    if code < 0:  # killed by a signal: report it the way a shell does
        code = 128 + (-code)
    print(f"{label} exit: {code}", flush=True)
    return code


def cmd_run(args: argparse.Namespace) -> None:
    inputs = validated_inputs()
    started_at = float(os.environ.get("COORD_REPAIR_STARTED_AT") or time.time())
    budget = int(os.environ.get("COORD_REPAIR_BUDGET_SECS") or DEFAULT_BUDGET_SECS)
    cap = int(os.environ.get("COORD_REPAIR_ARGV_CAP_SECS") or ARGV_CAP_SECS)
    # Wall clock -> monotonic, once.
    deadline = time.monotonic() + (started_at + budget - time.time())

    selected = json.loads(Path(args.selected).read_text(encoding="utf-8"))
    if selected.get("step") != inputs["step"] or selected.get("kind") != inputs["kind"]:
        die("the selected repair does not match the inputs")
    work = Path(args.work).resolve()
    out_dir = Path(args.out_dir).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    for stale in ("repair.patch", "report.json", "changed-files.txt"):
        (out_dir / stale).unlink(missing_ok=True)

    # Every `../trusted/...` element must resolve to the trusted checkout.
    trusted_root = (work.parent / "trusted").resolve()
    for token in [*selected["command"], *selected["check"]]:
        if token.startswith(TRUSTED_PREFIX):
            target = (work / token).resolve()
            if trusted_root not in target.parents or not target.exists():
                die(
                    f"{token!r} does not resolve to a file in the trusted checkout ({trusted_root})"
                )

    head = git(work, "rev-parse", "HEAD").decode().strip()
    if head != inputs["head_sha"]:
        die(f"work/ is at {head}, not the requested head_sha {inputs['head_sha']}")
    pathspecs = glob_pathspecs(selected["allowed_paths"])
    if git(
        work, "status", "--porcelain", "--untracked-files=all", "--", *pathspecs
    ).strip():
        die(
            "work/ already differs from HEAD inside allowed_paths before the repair ran"
        )
    if (
        subprocess.run(
            ["git", "-C", str(work), "diff", "--quiet", "HEAD"], check=False
        ).returncode
        != 0
    ):
        die(
            "work/ has tracked modifications before the repair ran; the patch would not be the repair's alone"
        )

    changed = changed_files(work, inputs["head_sha"], inputs["base_ref"])
    changed_path = out_dir / "changed-files.txt"
    changed_path.write_text("".join(f"{n}\n" for n in changed), encoding="utf-8")
    print(f"{len(changed)} changed file(s) vs origin/{inputs['base_ref']}")

    extra_env = {
        "COORD_REPAIR_CHANGED_FILES": str(changed_path),
        "COORD_REPAIR_KIND": inputs["kind"],
        "COORD_REPAIR_STEP": inputs["step"],
    }
    command_exit = run_argv(
        selected["command"], work, extra_env, deadline, cap, "command"
    )

    # Stage what the repair changed inside allowed_paths — NEW files included
    # (a plain `git diff` would drop them and report an empty patch) — and
    # nothing outside. What it changed outside is reported, not shipped:
    # coord refuses on it rather than reading a silently narrowed patch.
    # Listed first and added by literal name: `git add` refuses a pathspec
    # that matches nothing, and an allowed glob the repair did not touch is
    # the normal case.
    touched = decode_paths(
        git(
            work,
            "ls-files",
            "-z",
            "--modified",
            "--deleted",
            "--others",
            "--exclude-standard",
            "--",
            *pathspecs,
        ),
        "repair output inside allowed_paths",
    )
    if touched:
        git(
            work,
            "add",
            "-A",
            "--",
            *(f":(top,literal){p}" for p in sorted(set(touched))),
        )
    patch = git(
        work,
        "-c",
        "core.quotePath=false",
        "diff",
        "--cached",
        "--binary",
        "--no-color",
        "--no-ext-diff",
        "HEAD",
    )
    (out_dir / "repair.patch").write_bytes(patch)
    print(f"repair.patch: {len(patch)} bytes")
    modified_outside = decode_paths(
        git(work, "diff", "--name-only", "-z"), "modified outside allowed_paths"
    )
    untracked_outside = decode_paths(
        git(work, "ls-files", "--others", "--exclude-standard", "-z"),
        "untracked outside allowed_paths",
    )
    if modified_outside:
        print(
            f"::warning title=coord-repair::the repair modified {len(modified_outside)} tracked file(s) outside allowed_paths: {modified_outside[:20]}"
        )

    check_exit = run_argv(selected["check"], work, extra_env, deadline, cap, "check")

    report = {
        "schema": SCHEMA,
        "pr": inputs["pr"],
        "head_sha": inputs["head_sha"],
        "base_ref": inputs["base_ref"],
        "step": inputs["step"],
        "kind": inputs["kind"],
        "command_exit": command_exit,
        "check_exit": check_exit,
        "changed_files": changed,
        # Informational. Tracked files the command changed OUTSIDE
        # allowed_paths (not in the patch), and how many untracked files it
        # left outside them (build output and the like; not in the patch).
        "modified_outside_allowed": modified_outside,
        "untracked_ignored": len(untracked_outside),
    }
    (out_dir / "report.json").write_text(
        json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
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
