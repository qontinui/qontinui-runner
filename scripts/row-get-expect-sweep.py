#!/usr/bin/env python3
"""Attribute every remaining `tokio_postgres::Row::get` site with a fn-level `#[expect]`.

Plan `2026-09-03-coord-row-get-panic-class-closed-by-lint-and-supervisor`,
Phase 3 step 2. Source: dossier:row-get-panic-kills-spawned-loop.

Provenance: adapted from qontinui-coord's `scripts/row-get-expect-sweep.py`
(the same plan's Phase 1). The coord copy hard-codes coord's CI invocation
(`cargo clippy --workspace --tests --message-format=json` from the workspace
root); this copy takes the cargo command, its cwd, and an optional cross
`--target` on the command line, because the runner's required clippy checks
are `cd src-tauri && cargo clippy` on ubuntu AND a separate `Clippy (windows)`
job — `#[cfg(windows)]` fns are only reported by the windows leg, so the sweep
must run once per leg (`--target x86_64-pc-windows-msvc` for the second; the
check needs no linker). Runner builds go through `cargo-guard.sh`, hence
`--cargo`.

The repo-root `clippy.toml` disallows `tokio_postgres::Row::get` and
`src-tauri/Cargo.toml` raises `clippy::disallowed_methods` to `deny`. Every site
that existed when the gate landed is grandfathered with ONE attribute directly
above its ENCLOSING FUNCTION:

    #[expect(clippy::disallowed_methods, reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop")]

written in the four-line shape `rustfmt` produces for it (the runner's required
`cargo fmt -- --check` step reformats a 131-column attribute):

    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]

Both spellings are recognised when reading (idempotency, stale-attribute
removal, the count); only the four-line one is ever written.

`expect`, never `allow` (a migrated fn's stale attribute is itself a deny error
via `unfulfilled_lint_expectations`), fn granularity, never file- or
crate-level. `src-tauri/src/row_get_ratchet.rs` pins the attribute count as a
ceiling that only falls.

What this script does, per pass:

  1. Runs `<cargo> clippy [--target T] --message-format=json` from `--cargo-cwd`
     (default `<repo-root>/src-tauri`, the CI cwd).
  2. Collects every `clippy::disallowed_methods` diagnostic's primary span
     (`file_name`, `line_start`), de-duplicated (modules shared by the lib and
     the bin are compiled twice, so one site can be reported twice).
  3. For each site finds the nearest PRECEDING fn-signature line in the same
     file whose indentation is <= the site's, and inserts the attribute above it,
     matching that line's indentation. Idempotent: a fn already carrying the
     attribute directly above is skipped -- and, since the site was STILL
     reported, that attribute is not covering it (a nested `fn` item sat between
     the site and its real enclosing fn), so the search continues upward to the
     next fn line at a STRICTLY smaller indentation. No other reformatting, and
     never a repo-wide `cargo fmt`.
  4. Repeats until clippy exits 0 with zero `disallowed_methods` and zero
     `unfulfilled_lint_expectations` diagnostics.

Unattributable sites TERMINATE the loop, they do not spin it: every pass asserts
the diagnostic count strictly fell; on a no-progress pass the residual
`file:line` list is printed and the script exits 2. Those sites (a `Row::get`
inside a module-level `static`/`const`/`Lazy` initializer or a `macro_rules!`
body) are handled by hand -- an `#[expect]` on the enclosing non-fn item, or a
`try_get` migration -- before the next run.

Re-run this after a rebase that lands a new site (a peer's PR is red until it
does). Exit codes: 0 clean, 1 clippy failed for some other reason (compile
error, an unfulfilled expectation -- the rendered diagnostics are printed),
2 no progress / residual sites.

Usage:
    python3 scripts/row-get-expect-sweep.py [--repo-root DIR] [--cargo-cwd DIR]
        [--cargo "bash /path/cargo-guard.sh"] [--target x86_64-pc-windows-msvc]
        [--clippy-arg ARG ...] [--target-dir DIR] [--max-passes N] [--dry-run]
        [--from-json FILE]

`--from-json FILE` feeds the FIRST pass from a saved `--message-format=json`
stream instead of running clippy (a CI job's captured output, or a run you
already paid for); later passes run clippy as usual. That is how a
`Clippy (windows)` residue is completed from the job's own log when the cross
target cannot compile locally.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

LINT = "clippy::disallowed_methods"
UNFULFILLED = "unfulfilled_lint_expectations"
REASON = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
# The single-line spelling (what the plan text shows; what a hand edit may type).
ATTRIBUTE = f'#[expect(clippy::disallowed_methods, reason = "{REASON}")]'
# The rustfmt spelling — the one this script writes. Inner lines are indented
# one level (4 spaces) deeper than the `#[expect(` / `)]` lines.
ATTRIBUTE_BLOCK = ("#[expect(", "clippy::disallowed_methods,", f'reason = "{REASON}"', ")]")
# Whitespace-free canonical form, equal for both spellings.
ATTRIBUTE_COMPACT = re.sub(r"\s+", "", ATTRIBUTE)
# The plan's fn-signature shape, widened only by `extern "…"` / `default` (both
# legal item qualifiers). `fn(` (a fn-pointer type) deliberately does not match.
FN_LINE = re.compile(
    r'^\s*(pub(\([^)]*\))?\s+)?(default\s+)?(const\s+)?(async\s+)?(unsafe\s+)?'
    r'(extern\s+"[^"]*"\s+)?fn\s+\w+'
)
# The directories the ratchet test walks (relative to the repo root); the final
# attribute count is taken over the same set so the number printed here IS the
# baseline. `src-tauri/clorinde` is generated ("Do not modify") and is not
# under `src-tauri/src`, but the walk excludes any `clorinde` component anyway.
RATCHET_DIRS = ("src-tauri/src",)
EXCLUDED_DIR_NAMES = ("clorinde",)


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" \t"))


def clippy_command(cargo: str, target: str | None, extra: list[str]) -> list[str]:
    cmd = shlex.split(cargo) + ["clippy"]
    if target:
        cmd += ["--target", target]
    cmd += extra
    cmd.append("--message-format=json")
    return cmd


def run_clippy(cmd: list[str], cwd: Path, target_dir: str | None) -> tuple[int, list[dict]]:
    env = dict(os.environ)
    if target_dir:
        env["CARGO_TARGET_DIR"] = target_dir
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        errors="replace",
    )
    records: list[dict] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if rec.get("reason") == "compiler-message" and rec.get("message"):
            records.append(rec["message"])
    if proc.returncode != 0:
        tail = proc.stderr.strip().splitlines()[-40:]
        if tail:
            print("--- cargo stderr (tail) ---")
            print("\n".join(tail))
            print("--- end cargo stderr ---")
    return proc.returncode, records


def load_records(path: Path) -> list[dict]:
    """Parse a saved `--message-format=json` stream (one JSON object per line)."""
    records: list[dict] = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if rec.get("reason") == "compiler-message" and rec.get("message"):
            records.append(rec["message"])
    return records


def code_of(msg: dict) -> str | None:
    code = msg.get("code")
    return code.get("code") if isinstance(code, dict) else None


def primary_site(msg: dict) -> tuple[str, int] | None:
    for span in msg.get("spans") or []:
        if span.get("is_primary"):
            # A site expanded from a macro carries the call site in the
            # primary span already; `<…>` pseudo-files cannot be edited.
            name = span.get("file_name") or ""
            if name.startswith("<"):
                return None
            return name, int(span.get("line_start") or 0)
    return None


def collect(records: list[dict]) -> tuple[set[tuple[str, int]], list[dict], list[dict]]:
    sites: set[tuple[str, int]] = set()
    unfulfilled: list[dict] = []
    other_errors: list[dict] = []
    for msg in records:
        code = code_of(msg)
        if code == LINT:
            site = primary_site(msg)
            if site:
                sites.add(site)
        elif code == UNFULFILLED:
            unfulfilled.append(msg)
        elif msg.get("level") == "error" and not (msg.get("message") or "").startswith(
            "aborting due to"
        ):
            other_errors.append(msg)
    return sites, unfulfilled, other_errors


def resolve_source(file_name: str, roots: list[Path]) -> Path | None:
    """rustc reports `file_name` relative to the directory cargo ran rustc in.

    Cargo runs rustc from the WORKSPACE root even when invoked from
    `src-tauri`, so the path is normally `src-tauri/src/…`; try the repo root
    first, then the cargo cwd, then the name as given (absolute paths).
    """
    candidate = Path(file_name)
    if candidate.is_absolute():
        return candidate.resolve() if candidate.is_file() else None
    for root in roots:
        path = (root / file_name).resolve()
        if path.is_file():
            return path
    return None


def attribute_block_at(lines: list[str], end: int) -> int | None:
    """Start index of this script's attribute whose LAST line is `lines[end]`, else None.

    Recognises the single-line spelling (start == end) and the four-line
    rustfmt spelling, compared whitespace-free.
    """
    if end < 0 or end >= len(lines):
        return None
    if lines[end].strip() == ATTRIBUTE:
        return end
    n = len(ATTRIBUTE_BLOCK)
    start = end - n + 1
    if start < 0:
        return None
    compact = re.sub(r"\s+", "", "".join(lines[start : end + 1]))
    if compact == ATTRIBUTE_COMPACT and lines[start].strip() == ATTRIBUTE_BLOCK[0]:
        return start
    return None


def attribute_block_from(lines: list[str], start: int) -> int | None:
    """End index of this script's attribute whose FIRST line is `lines[start]`, else None."""
    if start < 0 or start >= len(lines):
        return None
    if lines[start].strip() == ATTRIBUTE:
        return start
    end = start + len(ATTRIBUTE_BLOCK) - 1
    if end < len(lines) and attribute_block_at(lines, end) == start:
        return end
    return None


def enclosing_fn_line(lines: list[str], site_line: int) -> int | None:
    """0-based index of the fn-signature line the attribute goes above, or None.

    Walks upward from the site. A candidate is a fn line at indentation <= the
    site's. If that candidate already carries the attribute (the site was still
    reported, so it is not the real enclosing item), the walk continues, now
    requiring a STRICTLY smaller indentation than that candidate.
    """
    if site_line < 1 or site_line > len(lines):
        return None
    max_indent = indent_of(lines[site_line - 1])
    i = site_line - 2
    while i >= 0:
        line = lines[i]
        if FN_LINE.match(line) and indent_of(line) <= max_indent:
            if attribute_block_at(lines, i - 1) is not None:
                max_indent = indent_of(line) - 1
                if max_indent < 0:
                    return None
            else:
                return i
        i -= 1
    return None


def plan_insertions(
    roots: list[Path], sites: set[tuple[str, int]]
) -> tuple[dict[Path, set[int]], list[tuple[str, int]]]:
    insertions: dict[Path, set[int]] = defaultdict(set)
    residual: list[tuple[str, int]] = []
    cache: dict[Path, list[str]] = {}
    for file_name, line_no in sorted(sites):
        path = resolve_source(file_name, roots)
        if path is None:
            residual.append((file_name, line_no))
            continue
        if path not in cache:
            cache[path] = path.read_text(encoding="utf-8").splitlines(keepends=True)
        idx = enclosing_fn_line(cache[path], line_no)
        if idx is None:
            residual.append((file_name, line_no))
        else:
            insertions[path].add(idx)
    return insertions, residual


def apply_insertions(insertions: dict[Path, set[int]], dry_run: bool) -> int:
    inserted = 0
    for path, idxs in insertions.items():
        lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
        for idx in sorted(idxs, reverse=True):
            fn_line = lines[idx]
            pad = fn_line[: indent_of(fn_line)]
            newline = "\r\n" if fn_line.endswith("\r\n") else "\n"
            block = [
                f"{pad}{ATTRIBUTE_BLOCK[0]}{newline}",
                f"{pad}    {ATTRIBUTE_BLOCK[1]}{newline}",
                f"{pad}    {ATTRIBUTE_BLOCK[2]}{newline}",
                f"{pad}{ATTRIBUTE_BLOCK[3]}{newline}",
            ]
            lines[idx:idx] = block
            inserted += 1
        if not dry_run:
            path.write_text("".join(lines), encoding="utf-8")
    return inserted


def remove_stale_attributes(
    roots: list[Path], unfulfilled: list[dict], dry_run: bool
) -> tuple[int, list[dict]]:
    """Delete the attribute lines that `unfulfilled_lint_expectations` points at.

    Two ways an expectation goes stale: the fn under it migrated to `try_get`
    (the peer-rebase case the reason string warns about), or a pass placed the
    attribute on a nested fn that sat between a site and its real enclosing fn
    (the escalation in `enclosing_fn_line` then attributes the outer fn on the
    next pass, leaving the inner one unfulfilled). Both are fixed by removing
    the line. A diagnostic whose primary span is NOT exactly this script's
    attribute line is left alone and returned for the caller to print.

    NOTE for the two-leg runner sweep: an attribute placed for a
    `#[cfg(windows)]` fn is reported unfulfilled by the LINUX leg only if the
    fn itself is compiled there — a `cfg`-gated fn is not compiled at all, so
    its attribute is silent on the other leg. Nothing here removes a
    cross-leg attribute by mistake.
    """
    by_file: dict[Path, set[tuple[int, int]]] = defaultdict(set)
    foreign: list[dict] = []
    for msg in unfulfilled:
        site = primary_site(msg)
        if site is None:
            foreign.append(msg)
            continue
        path = resolve_source(site[0], roots)
        if path is None:
            foreign.append(msg)
            continue
        lines = path.read_text(encoding="utf-8").splitlines()
        start = site[1] - 1
        end = attribute_block_from(lines, start)
        if end is not None:
            by_file[path].add((start, end))
        else:
            foreign.append(msg)
    removed = 0
    for path, spans in by_file.items():
        lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
        for start, end in sorted(spans, reverse=True):
            del lines[start : end + 1]
            removed += 1
        if not dry_run:
            path.write_text("".join(lines), encoding="utf-8")
    return removed, foreign


def attribute_starts(text: str):
    """Yield `(line_no, compact)` for every attribute in `text`, both spellings.

    An attribute starts at a line whose stripped form begins with `#[` or
    `#![`; a multi-line one (stripped line ending in `(`) is joined with the
    following lines up to the one that is exactly `)]`. `compact` is the joined
    text with all whitespace removed, so `#[expect(clippy::disallowed_methods`
    matches either spelling and prose quoting the attribute inside a comment
    does not. Mirrors `attribute_starts` in `row_get_ratchet.rs`.
    """
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        stripped = lines[i].strip()
        if stripped.startswith("#[") or stripped.startswith("#!["):
            j = i
            if stripped.endswith("("):
                while j + 1 < len(lines) and lines[j].strip() != ")]":
                    j += 1
            compact = re.sub(r"\s+", "", "".join(lines[i : j + 1]))
            yield i + 1, compact
            i = j + 1
        else:
            i += 1


def count_attributes(root: Path) -> tuple[int, int]:
    """Count ATTRIBUTES the way `row_get_ratchet.rs` does.

    An attribute counts when its whitespace-free form STARTS with the needle
    (either spelling) -- prose in a doc comment that quotes the attribute does
    not. The ratchet file itself is skipped there too (it spells the needle as
    a literal).
    """
    needle = "#[expect(clippy::disallowed_methods"
    total = 0
    files = 0
    for rel in RATCHET_DIRS:
        base = root / rel
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            if path.name == "row_get_ratchet.rs":
                continue
            if any(part in EXCLUDED_DIR_NAMES for part in path.relative_to(base).parts):
                continue
            n = sum(
                1
                for _, compact in attribute_starts(path.read_text(encoding="utf-8"))
                if compact.startswith(needle)
            )
            if n:
                total += n
                files += 1
    return total, files


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument(
        "--repo-root",
        default=str(Path(__file__).resolve().parent.parent),
        help="the directory holding clippy.toml and the workspace Cargo.toml (default: this script's parent's parent)",
    )
    ap.add_argument(
        "--cargo-cwd",
        default=None,
        help="directory to run clippy from (default: <repo-root>/src-tauri — the CI cwd)",
    )
    ap.add_argument(
        "--cargo",
        default="cargo",
        help='the cargo front-end, shell-split (default "cargo"; runner builds: "bash <path>/cargo-guard.sh")',
    )
    ap.add_argument(
        "--target",
        default=None,
        help="cross target triple, e.g. x86_64-pc-windows-msvc for the `Clippy (windows)` leg",
    )
    ap.add_argument(
        "--clippy-arg",
        action="append",
        default=[],
        help="extra argument passed to `clippy` (repeatable), e.g. --clippy-arg=--tests",
    )
    ap.add_argument(
        "--target-dir",
        default=os.environ.get("CARGO_TARGET_DIR"),
        help="CARGO_TARGET_DIR for the clippy runs (default: inherit; cargo-guard picks its own when unset)",
    )
    ap.add_argument(
        "--from-json",
        default=None,
        help="feed pass 1 from this saved --message-format=json stream instead of running clippy",
    )
    ap.add_argument("--max-passes", type=int, default=12)
    ap.add_argument("--dry-run", action="store_true", help="plan insertions for one pass, write nothing")
    args = ap.parse_args()
    root = Path(args.repo_root).resolve()
    if not (root / "Cargo.toml").is_file():
        print(f"no Cargo.toml under {root}", file=sys.stderr)
        return 1
    cwd = Path(args.cargo_cwd).resolve() if args.cargo_cwd else root / "src-tauri"
    if not (cwd / "Cargo.toml").is_file():
        print(f"no Cargo.toml under cargo cwd {cwd}", file=sys.stderr)
        return 1
    roots = [root, cwd]
    cmd = clippy_command(args.cargo, args.target, args.clippy_arg)

    previous: int | None = None
    for pass_no in range(1, args.max_passes + 1):
        if pass_no == 1 and args.from_json:
            print(f"[pass {pass_no}] reading saved diagnostics from {args.from_json}", flush=True)
            records = load_records(Path(args.from_json))
            # A saved stream carries no exit code; any diagnostic at all means
            # the run was not clean, and an empty one is verified by pass 2.
            status = 1 if records else 0
        else:
            print(f"[pass {pass_no}] {shlex.join(cmd)} (cwd={cwd})", flush=True)
            status, records = run_clippy(cmd, cwd, args.target_dir)
        sites, unfulfilled, other_errors = collect(records)
        print(
            f"[pass {pass_no}] clippy exit={status} {LINT} sites={len(sites)} "
            f"{UNFULFILLED}={len(unfulfilled)} other_errors={len(other_errors)}",
            flush=True,
        )
        removed = 0
        if unfulfilled:
            removed, foreign = remove_stale_attributes(roots, unfulfilled, args.dry_run)
            print(
                f"[pass {pass_no}] {'would remove' if args.dry_run else 'removed'} {removed} stale "
                f"attribute(s) pointed at by {UNFULFILLED}; not this script's line: {len(foreign)}",
                flush=True,
            )
            for msg in foreign:
                sys.stdout.write(msg.get("rendered") or (msg.get("message", "") + "\n"))
        if not sites:
            if status == 0 and not unfulfilled:
                total, files = count_attributes(root)
                print(f"[done] clean. attribute count (baseline) = {total} across {files} files")
                return 0
            if removed and not args.dry_run and removed == len(unfulfilled) and not other_errors:
                # Only stale attributes stood between this pass and clean; the
                # next pass verifies the removal.
                previous = None
                continue
            for msg in other_errors:
                sys.stdout.write(msg.get("rendered") or (msg.get("message", "") + "\n"))
            print(f"[fail] clippy exit={status} with no {LINT} sites left to attribute — see above")
            return 1
        if previous is not None and len(sites) >= previous and not removed:
            print(f"[fail] no progress: pass {pass_no} reported {len(sites)} sites, previous pass {previous}")
            _, residual = plan_insertions(roots, sites)
            print("residual (handle by hand — #[expect] on the enclosing non-fn item, or migrate to try_get):")
            for file_name, line_no in sorted(set(residual) | sites):
                print(f"  {file_name}:{line_no}")
            return 2
        previous = len(sites)
        insertions, residual = plan_insertions(roots, sites)
        inserted = apply_insertions(insertions, args.dry_run)
        print(
            f"[pass {pass_no}] {'would insert' if args.dry_run else 'inserted'} {inserted} attribute(s) "
            f"across {len(insertions)} file(s); unattributable now: {len(residual)}",
            flush=True,
        )
        for file_name, line_no in residual:
            print(f"  unattributable: {file_name}:{line_no}")
        if args.dry_run:
            return 0
        if inserted == 0:
            print("[fail] nothing could be attributed this pass; residual sites above need a hand fix")
            return 2
    print(f"[fail] {args.max_passes} passes exhausted without reaching clean")
    return 2


if __name__ == "__main__":
    sys.exit(main())
