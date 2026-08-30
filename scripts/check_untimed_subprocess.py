#!/usr/bin/env python3
"""
Fail the build on a NEW untimed synchronous subprocess call site.

WHAT THIS GATES
---------------
A `std::process::Command` that is *waited on* with no time bound:

    let out = no_window("git").args(["status"]).output();   // <-- unbounded
    let st  = cmd.status();                                 // <-- unbounded
    let out = child.wait_with_output();                     // <-- unbounded
    let _   = child.wait();                                 // <-- unbounded

`std::process::Command::output()` / `status()` and `Child::wait()` /
`wait_with_output()` have NO timeout. A child that never exits parks the
calling thread FOREVER. When that thread came from tokio's blocking pool it is
never returned to the pool, and tokio's default `max_blocking_threads` is 512.
On 2026-08-30 eight independent *periodic* callers each leaked a thread per
tick until the pool was exhausted; `spawn_blocking` then stopped scheduling
anything, which starved the PG pool, which disabled `zombie_sweep`, which made
the spiral self-reinforcing until `/livez` went dark.

This is the FOURTH independently-discovered instance of that defect shape
across two incidents four days apart. Code review demonstrably did not catch
it three times, so it gets a machine check.

THE SANCTIONED BOUNDED WRAPPERS (all in `src-tauri/src/process_helpers.rs`)
--------------------------------------------------------------------------
    run_with_timeout(cmd: Command, timeout: Duration) -> io::Result<TimedOutput>
    output_with_timeout(cmd: Command, timeout: Duration) -> io::Result<Output>
    run_probe(cmd: Command, timeout: Duration, label: &str) -> ProbeOutcome

A site routed through any of these never calls `.output()` / `.status()` at
all — it hands the *built* command to the wrapper — so "is it routed?" needs no
heuristic: the presence of the blocking wait IS the violation.

WHY `.spawn()` IS NOT IN THE PATTERN
------------------------------------
`.spawn()` returns immediately; it parks nothing. Gating it would fire on
every deliberately-detached long-lived child (the python sidecar, ffmpeg,
rathole, claude CLI sessions, terminal spawns, managed services) and on every
hand-rolled bounded `spawn` + `try_wait` poll loop — a large, legitimate,
mostly-indistinguishable population. The defect is the unbounded WAIT, so the
gate follows the wait: `.output()`, `.status()`, `Child::wait()`,
`Child::wait_with_output()`. That also catches the `spawn`-then-`wait` shape a
bare-`spawn` rule would have had to let through anyway. `try_wait()` is
non-blocking and is deliberately absent from the list.

HOW IT TELLS `std::process` FROM `tokio::process`
-------------------------------------------------
A bare regex on `.output()` cannot: 53 files use both, and `reqwest`'s
`Response::status()` alone would produce hundreds of false hits. So the checker
resolves the RECEIVER of each blocking wait back to its origin:

  * `process_helpers::no_window(..)` / `cmd_no_window()` / `std::process::Command::new(..)`
    (and a bare `Command::new(..)` in a file that imports `std::process::Command`)
        -> SYNC, gate applies
  * `tokio_no_window(..)` / `tokio_cmd_no_window()` / `tokio::process::Command::new(..)`
    (and a bare `Command::new(..)` in a file importing `tokio::process::Command`)
        -> ASYNC, ignored: an awaited async wait occupies no blocking-pool thread
  * anything the checker cannot resolve to a SYNC origin -> ignored

Resolution is local: `let` bindings and typed `fn` parameters inside the
enclosing function, plus a crate-wide map of helper functions whose declared
return type is a `std::process::Command`. `.await` after a wait is treated as
proof of the async arm regardless of what resolution said.

Deliberately conservative: unresolvable receivers are NOT flagged. A gate that
cries wolf gets disabled, and that reopens the class this exists to close.

WHAT IS SKIPPED WHOLESALE
-------------------------
  * `#[cfg(test)]` items and `#[test]` functions — a fixture is not a periodic path.
  * `src-tauri/src/process_helpers.rs` itself — it *implements* the bounded
    primitive, so it necessarily contains the raw calls.
  * `Child::wait()` immediately preceded by a `.kill()` in the same function —
    reaping a child you just killed is bounded by construction.

THE BASELINE
------------
The 2026-08-30 sweep fixed the periodic/hot-path sites. The residue is real and
mostly legitimate: one-shot CLI paths, user-triggered actions, startup and
shutdown work. Rather than pretend it does not exist, every surviving site is
enumerated in `scripts/untimed-subprocess-baseline.json`, keyed by
`<path>::<function>` with a REQUIRED written reason. That key survives edits
above and below the site, which a line number does not.

The baseline is a RATCHET:
  * a site in a function with no baseline entry            -> FAIL
  * more sites in a function than its entry records        -> FAIL
  * fewer sites than recorded                              -> FAIL, "tighten it"
  * an entry with an empty `reason`                        -> FAIL
so it can only ever shrink, and `--update-baseline` cannot smuggle an exemption
in: a regenerated entry arrives with an empty reason and is rejected until a
human writes one.

USAGE
-----
    python3 scripts/check_untimed_subprocess.py              # check (exit 1 on a finding)
    python3 scripts/check_untimed_subprocess.py --list       # print every SYNC wait found
    python3 scripts/check_untimed_subprocess.py --update-baseline
    python3 scripts/check_untimed_subprocess.py --root <dir> # check a different tree

Pure stdlib, no third-party imports: it runs identically on the Windows dev box
and on the ubuntu CI runner.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

#: Scanned tree, relative to the repo root.
SOURCE_DIR = Path("src-tauri") / "src"

#: Baseline location, relative to the repo root.
BASELINE_PATH = Path("scripts") / "untimed-subprocess-baseline.json"

#: Files exempt from scanning entirely, relative to the repo root (POSIX form).
SKIPPED_FILES = {
    # Implements the bounded primitive; the raw calls here ARE the fix.
    "src-tauri/src/process_helpers.rs",
}

#: Blocking waits with no time bound. `try_wait()` is absent on purpose — it
#: does not block. `.spawn()` is absent on purpose — see the module docstring.
BLOCKING_WAITS = ("output", "status", "wait", "wait_with_output")

#: Method names that only make sense on a `Command`/`Child`, used to keep a
#: receiver chain alive while walking back to its root.
_CHAIN_METHODS = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

#: Constructors that yield a synchronous `std::process::Command`.
SYNC_ORIGINS = ("no_window", "cmd_no_window")

#: Constructors that yield an asynchronous `tokio::process::Command`.
ASYNC_ORIGINS = ("tokio_no_window", "tokio_cmd_no_window")

#: The bounded wrappers, named in the failure message.
WRAPPERS = (
    "run_with_timeout(cmd, timeout)   -> io::Result<TimedOutput>",
    "output_with_timeout(cmd, timeout) -> io::Result<Output>   (drop-in for .output())",
    "run_probe(cmd, timeout, label)    -> ProbeOutcome         (shell out / read stdout / degrade)",
)


# ---------------------------------------------------------------------------
# Lexical pre-pass: blank out comments and string literals
# ---------------------------------------------------------------------------


_NOISE_START = re.compile(r"""//|/\*|b?r\#*"|"|'(?:\\.|[^\\'])'""")


def strip_noise(src: str) -> str:
    """Replace comments and string/char literals with spaces, preserving offsets.

    Byte offsets and line numbers are unchanged, so every index computed on the
    result addresses the same place in the original file. This is what stops
    prose in a doc comment, or a shell command inside a string literal, from
    ever reaching the matcher.

    Driven by a scanner regex that jumps straight to the next interesting
    position rather than stepping character by character — the tree is ~37 MB
    of Rust and a per-character Python loop over it is the whole runtime.
    """
    out = list(src)
    n = len(src)

    def blank(start: int, end: int) -> None:
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    i = 0
    while True:
        m = _NOISE_START.search(src, i)
        if m is None:
            break
        start, tok = m.start(), m.group(0)
        if tok == "//":
            j = src.find("\n", start)
            j = n if j == -1 else j
        elif tok == "/*":
            # Rust block comments nest.
            depth, j = 1, start + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1
                    j += 2
                elif src.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
        elif tok.endswith('"') and len(tok) > 1:
            # raw string:  r"…"  r#"…"#  br##"…"##
            if start > 0 and (src[start - 1].isalnum() or src[start - 1] == "_"):
                i = start + 1  # an identifier that merely ends in `r`/`b`
                continue
            close = '"' + tok[tok.index("r") + 1 : -1]
            j = src.find(close, m.end())
            j = n if j == -1 else j + len(close)
        elif tok == '"':
            j = start + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    break
                j += 1
        else:
            j = m.end()  # char literal
        blank(start, j)
        i = j
    return "".join(out)


# ---------------------------------------------------------------------------
# Structure: strip test items, then carve function bodies
# ---------------------------------------------------------------------------


def _match_brace(src: str, open_idx: int) -> int:
    """Index just past the `}` closing the `{` at ``open_idx`` (or len(src))."""
    depth = 0
    i = open_idx
    n = len(src)
    while i < n:
        if src[i] == "{":
            depth += 1
        elif src[i] == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def strip_test_items(src: str) -> str:
    """Blank out every `#[cfg(test)]` / `#[test]` item, preserving offsets."""
    out = list(src)

    def blank(start: int, end: int) -> None:
        for k in range(start, min(end, len(out))):
            if out[k] != "\n":
                out[k] = " "

    for m in re.finditer(r"#\[\s*(?:cfg\s*\(\s*test\s*\)|test|tokio::test)\s*\]", src):
        # Walk forward past any further attributes to the item's own `{`.
        j = m.end()
        brace = src.find("{", j)
        semi = src.find(";", j)
        if brace == -1 or (semi != -1 and semi < brace):
            # An attribute on a `use`/`const`/decl item — nothing to carve.
            continue
        blank(m.start(), _match_brace(src, brace))
    return "".join(out)


@dataclass(frozen=True)
class Function:
    """One function body, with the offsets needed to map hits back to lines."""

    name: str
    start: int  # offset of the body's `{`
    end: int  # offset just past the body's `}`
    body: str


_FN_RE = re.compile(
    r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^{;]*?>)?\s*\(",
)


def find_functions(src: str) -> list[Function]:
    """Every `fn` in ``src``, outermost-first, with nested `fn`s also listed.

    A closure lives inside its enclosing `fn` and is intentionally NOT split
    out: "the same function body" is the unit the baseline is keyed on.
    """
    fns: list[Function] = []
    for m in _FN_RE.finditer(src):
        # Find the body brace: skip the parameter list and the return type.
        i = m.end() - 1  # at '('
        depth = 0
        n = len(src)
        while i < n:
            if src[i] == "(":
                depth += 1
            elif src[i] == ")":
                depth -= 1
                if depth == 0:
                    i += 1
                    break
            i += 1
        # Now scan to the first `{` or `;` (a trait/extern decl has no body).
        j = i
        while j < n and src[j] not in "{;":
            j += 1
        if j >= n or src[j] == ";":
            continue
        end = _match_brace(src, j)
        fns.append(Function(m.group(1), j, end, src[j:end]))
    return fns


def innermost_function(fns: list[Function], offset: int) -> Function | None:
    """The smallest function body containing ``offset``."""
    best: Function | None = None
    for f in fns:
        if f.start <= offset < f.end:
            if best is None or (f.end - f.start) < (best.end - best.start):
                best = f
    return best


# ---------------------------------------------------------------------------
# Receiver resolution
# ---------------------------------------------------------------------------

SYNC = "sync"
ASYNC = "async"
UNKNOWN = "unknown"


def _skip_ws_back(src: str, i: int) -> int:
    while i >= 0 and src[i].isspace():
        i -= 1
    return i


def _match_close_back(src: str, i: int) -> int:
    """Given ``src[i]`` is a closer, return the index of its opener minus one."""
    pairs = {")": "(", "]": "[", "}": "{"}
    closer = src[i]
    opener = pairs[closer]
    depth = 0
    while i >= 0:
        if src[i] == closer:
            depth += 1
        elif src[i] == opener:
            depth -= 1
            if depth == 0:
                return i - 1
        i -= 1
    return -1


def receiver_root(src: str, dot_idx: int) -> str:
    """Walk back from the `.` of a method call to the root of its chain.

    Returns the root as a (possibly `::`-qualified) path string, or "" when the
    chain does not root in something nameable.
    """
    i = _skip_ws_back(src, dot_idx - 1)
    while i >= 0:
        c = src[i]
        if c == "?":
            i = _skip_ws_back(src, i - 1)
            continue
        if c in ")]}":
            i = _skip_ws_back(src, _match_close_back(src, i))
            continue
        if c.isalnum() or c == "_":
            j = i
            while j >= 0 and (src[j].isalnum() or src[j] == "_"):
                j -= 1
            ident = src[j + 1 : i + 1]
            k = _skip_ws_back(src, j)
            if k >= 0 and src[k] == ".":
                # a further link in the chain — keep walking
                i = _skip_ws_back(src, k - 1)
                continue
            if k >= 1 and src[k] == ":" and src[k - 1] == ":":
                # a path segment — absorb the qualifier and keep going
                path = [ident]
                while k >= 1 and src[k] == ":" and src[k - 1] == ":":
                    p = _skip_ws_back(src, k - 2)
                    q = p
                    while q >= 0 and (src[q].isalnum() or src[q] == "_"):
                        q -= 1
                    if q == p:
                        break
                    path.insert(0, src[q + 1 : p + 1])
                    k = _skip_ws_back(src, q)
                return "::".join(path)
            return ident
        return ""
    return ""


def classify_origin(expr: str, file_default: str) -> str:
    """Classify a constructor-ish expression as SYNC / ASYNC / UNKNOWN."""
    if re.search(r"\b(?:tokio_no_window|tokio_cmd_no_window)\b", expr):
        return ASYNC
    if re.search(r"\btokio::process::Command\s*::\s*new\b", expr):
        return ASYNC
    if re.search(r"\b(?:no_window|cmd_no_window)\b", expr):
        return SYNC
    if re.search(r"\bstd::process::Command\s*::\s*new\b", expr):
        return SYNC
    if re.search(r"\bCommand\s*::\s*new\b", expr):
        return file_default
    return UNKNOWN


def file_command_default(src: str) -> str:
    """What a bare `Command::new(..)` means in this file, from its imports."""
    has_std = re.search(r"use\s+std::process::(?:\{[^}]*\bCommand\b|Command\b)", src)
    has_tokio = re.search(r"use\s+tokio::process::(?:\{[^}]*\bCommand\b|Command\b)", src)
    if has_std and not has_tokio:
        return SYNC
    if has_tokio and not has_std:
        return ASYNC
    # Both (or neither) imported: unresolvable by import alone. Conservative.
    return UNKNOWN


_LET_RE = re.compile(
    r"\blet\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*([^=;]+?))?\s*=\s*",
)


def local_bindings(body: str, file_default: str) -> dict[str, str]:
    """Map local variable name -> SYNC / ASYNC for `Command` and `Child` values.

    A `Child` inherits its parent `Command`'s kind, so `let mut c =
    no_window("x").spawn()?;` makes `c.wait()` a SYNC blocking wait.
    """
    kinds: dict[str, str] = {}
    for m in _LET_RE.finditer(body):
        name = m.group(1)
        ann = m.group(2) or ""
        # The initialiser runs to the statement's `;` at depth 0.
        i = m.end()
        depth = 0
        while i < len(body):
            ch = body[i]
            if ch in "([{":
                depth += 1
            elif ch in ")]}":
                if depth == 0:
                    break
                depth -= 1
            elif ch == ";" and depth == 0:
                break
            i += 1
        rhs = body[m.end() : i]
        kind = classify_origin(ann, file_default)
        if kind == UNKNOWN:
            kind = classify_origin(rhs, file_default)
        if kind == UNKNOWN:
            # `let mut c = cmd;` / `let c = cmd.spawn()?;` — inherit.
            root = re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*)\b", rhs)
            if root and root.group(1) in kinds:
                kind = kinds[root.group(1)]
        if kind != UNKNOWN:
            kinds[name] = kind
    return kinds


_PARAM_RE = re.compile(
    r"\b(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(&?\s*(?:mut\s+)?[A-Za-z_][A-Za-z0-9_:]*)"
)


def param_bindings(src: str, fn: Function, file_default: str) -> dict[str, str]:
    """Map parameter name -> SYNC / ASYNC from the signature's declared types."""
    # The signature is the text between the preceding `fn` and the body brace.
    head_start = src.rfind("fn ", 0, fn.start)
    if head_start == -1:
        return {}
    head = src[head_start : fn.start]
    kinds: dict[str, str] = {}
    for m in _PARAM_RE.finditer(head):
        ty = m.group(2)
        if re.search(r"\btokio::process::(?:Command|Child)\b", ty):
            kinds[m.group(1)] = ASYNC
        elif re.search(r"\bstd::process::(?:Command|Child)\b", ty):
            kinds[m.group(1)] = SYNC
        elif re.search(r"\b(?:Command|Child)\b", ty):
            kinds[m.group(1)] = file_default
    return kinds


_FN_RET_RE = re.compile(
    r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^{;]*?>)?\s*\([^;{]*?\)\s*->\s*([^{;]+)",
    re.S,
)


def helper_return_kinds(sources: dict[str, str]) -> dict[str, str]:
    """Crate-wide map: helper fn name -> SYNC / ASYNC, from its return type.

    Covers the common `fn git_cmd(repo: &Path) -> std::process::Command` shape,
    so `git_cmd(p).output()` resolves even though the constructor is elsewhere.
    """
    kinds: dict[str, str] = {}
    for src in sources.values():
        default = file_command_default(src)
        for m in _FN_RET_RE.finditer(src):
            ret = m.group(2)
            if re.search(r"\btokio::process::Command\b", ret):
                kinds[m.group(1)] = ASYNC
            elif re.search(r"\bstd::process::Command\b", ret):
                kinds[m.group(1)] = SYNC
            elif re.search(r"(?<![:\w])Command\b", ret) and default != UNKNOWN:
                kinds[m.group(1)] = default
    return kinds


# ---------------------------------------------------------------------------
# The scan
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Finding:
    path: str  # repo-relative, POSIX
    line: int
    function: str
    method: str
    snippet: str

    @property
    def key(self) -> str:
        return f"{self.path}::{self.function}"


_WAIT_RE = re.compile(
    r"\.\s*(" + "|".join(sorted(BLOCKING_WAITS, key=len, reverse=True)) + r")\s*\(\s*\)"
)


def _followed_by_await(src: str, end: int) -> bool:
    """True when the call at ``end`` is immediately `.await`-ed (async arm)."""
    m = re.match(r"\s*\??\s*\.\s*await\b", src[end : end + 32])
    return bool(m)


def _kill_before(body: str, offset: int) -> bool:
    """True when a `.kill()` appears earlier in the same body.

    Reaping a child you have just killed is bounded by construction — that is
    exactly what `run_with_timeout`'s own expiry path does — so a `wait()` that
    follows a `kill()` is not the defect.
    """
    return bool(re.search(r"\.\s*kill\s*\(", body[:offset]))


def scan_file(rel_path: str, raw: str, helper_kinds: dict[str, str]) -> list[Finding]:
    src = strip_test_items(strip_noise(raw))
    if "output" not in src and "status" not in src and "wait" not in src:
        return []
    default = file_command_default(src)
    fns = find_functions(src)
    line_starts = [0] + [i + 1 for i, c in enumerate(raw) if c == "\n"]

    def line_of(off: int) -> int:
        lo, hi = 0, len(line_starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if line_starts[mid] <= off:
                lo = mid
            else:
                hi = mid - 1
        return lo + 1

    cache: dict[int, dict[str, str]] = {}
    raw_lines = raw.splitlines()
    findings: list[Finding] = []

    for m in _WAIT_RE.finditer(src):
        method = m.group(1)
        if _followed_by_await(src, m.end()):
            continue
        fn = innermost_function(fns, m.start())
        if fn is None:
            continue
        if fn.start not in cache:
            binds = param_bindings(src, fn, default)
            binds.update(local_bindings(fn.body, default))
            cache[fn.start] = binds
        binds = cache[fn.start]

        root = receiver_root(src, m.start())
        if not root:
            continue
        tail = root.rsplit("::", 1)[-1]

        kind = UNKNOWN
        if tail in SYNC_ORIGINS:
            kind = SYNC
        elif tail in ASYNC_ORIGINS:
            kind = ASYNC
        elif root.endswith("std::process::Command::new") or (
            root.endswith("Command::new") and "tokio" not in root
        ):
            kind = SYNC if not root.startswith("tokio") else ASYNC
            if root == "Command::new":
                kind = default
        elif root.endswith("tokio::process::Command::new"):
            kind = ASYNC
        elif root in binds:
            kind = binds[root]
        elif tail in helper_kinds:
            kind = helper_kinds[tail]

        if kind != SYNC:
            continue
        if method in ("wait", "wait_with_output") and _kill_before(
            fn.body, m.start() - fn.start
        ):
            continue

        ln = line_of(m.start())
        snippet = raw_lines[ln - 1].strip() if ln - 1 < len(raw_lines) else ""
        findings.append(Finding(rel_path, ln, fn.name, method, snippet[:160]))
    return findings


def collect_sources(root: Path) -> dict[str, str]:
    sources: dict[str, str] = {}
    base = root / SOURCE_DIR
    if not base.is_dir():
        raise SystemExit(f"error: no source tree at {base}")
    for p in sorted(base.rglob("*.rs")):
        rel = p.relative_to(root).as_posix()
        if rel in SKIPPED_FILES:
            continue
        sources[rel] = p.read_text(encoding="utf-8", errors="replace")
    return sources


def run_scan(root: Path) -> list[Finding]:
    sources = collect_sources(root)
    helper_kinds = helper_return_kinds(sources)
    findings: list[Finding] = []
    for rel, raw in sources.items():
        findings.extend(scan_file(rel, raw, helper_kinds))
    findings.sort(key=lambda f: (f.path, f.line))
    return findings


# ---------------------------------------------------------------------------
# Baseline
# ---------------------------------------------------------------------------


def load_baseline(path: Path) -> dict[str, dict]:
    if not path.is_file():
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    return {e["site"]: e for e in data.get("exemptions", [])}


def write_baseline(path: Path, entries: list[dict]) -> None:
    doc = {
        "_comment": [
            "Sites where a synchronous std::process::Command is waited on with no",
            "time bound, and that are ACCEPTED. Enforced by",
            "scripts/check_untimed_subprocess.py and",
            ".github/workflows/forbid-untimed-subprocess.yml.",
            "",
            "'site' is '<repo-relative path>::<enclosing fn name>' — stable across",
            "edits above and below the call, which a line number is not.",
            "'sites' is how many blocking waits that function currently contains;",
            "the checker fails if the real number differs in EITHER direction, so",
            "the baseline can only shrink and cannot silently rot.",
            "'reason' is REQUIRED and must say why an unbounded wait is acceptable",
            "HERE — i.e. why this code is not on a periodic or hot path. An empty",
            "reason fails the gate, so --update-baseline cannot smuggle in an",
            "exemption.",
            "",
            "Do NOT add an entry to silence a periodic caller. Route it through",
            "run_probe / output_with_timeout / run_with_timeout instead.",
        ],
        "exemptions": entries,
    }
    path.write_text(
        json.dumps(doc, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )


def group(findings: Iterable[Finding]) -> dict[str, list[Finding]]:
    out: dict[str, list[Finding]] = {}
    for f in findings:
        out.setdefault(f.key, []).append(f)
    return out


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------


def teach(site_lines: list[str], ci: bool) -> str:
    err = "::error::" if ci else ""
    body = [
        f"{err}Untimed synchronous subprocess call site(s):",
    ]
    body += [f"{err}  {s}" for s in site_lines]
    body += [
        f"{err}",
        f"{err}WHY THIS IS A DEFECT: std::process::Command::output()/status() and",
        f"{err}Child::wait()/wait_with_output() have no timeout. A child that hangs",
        f"{err}parks the calling thread forever; when that thread came from tokio's",
        f"{err}blocking pool it is never returned. tokio's default cap is 512 threads,",
        f"{err}and on 2026-08-30 eight periodic callers exhausted it — which starved",
        f"{err}the PG pool, disabled zombie_sweep, and took /livez dark.",
        f"{err}",
        f"{err}FIX IT by routing the built command through a bounded wrapper from",
        f"{err}src-tauri/src/process_helpers.rs:",
    ]
    body += [f"{err}  {w}" for w in WRAPPERS]
    body += [
        f"{err}",
        f"{err}e.g.  let out = no_window(\"git\").args([\"status\"]).output();",
        f"{err}  ->  let mut c = no_window(\"git\"); c.args([\"status\"]);",
        f"{err}      match run_probe(c, Duration::from_secs(20), \"mod: git status\") {{ .. }}",
        f"{err}",
        f"{err}IF THIS SITE IS GENUINELY EXEMPT (a one-shot CLI path, a user-triggered",
        f"{err}action, startup or shutdown work — NOT anything on a timer or a hot",
        f"{err}path), add it to scripts/untimed-subprocess-baseline.json:",
        f"{err}  {{ \"site\": \"<path>::<fn>\", \"sites\": <n>, \"reason\": \"<why it is not periodic>\" }}",
        f"{err}`python3 scripts/check_untimed_subprocess.py --update-baseline` writes the",
        f"{err}entry with an EMPTY reason; the gate stays red until you write one.",
    ]
    return "\n".join(body)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repo root to scan (default: the repo this script lives in)",
    )
    ap.add_argument("--list", action="store_true", help="print every SYNC wait found")
    ap.add_argument(
        "--update-baseline",
        action="store_true",
        help="rewrite the baseline from the current tree (new entries get an EMPTY reason)",
    )
    ap.add_argument("--ci", action="store_true", help="emit GitHub ::error:: annotations")
    args = ap.parse_args()

    root: Path = args.root.resolve()
    baseline_path = root / BASELINE_PATH
    findings = run_scan(root)
    grouped = group(findings)

    if args.list:
        for f in findings:
            print(f"{f.path}:{f.line}  {f.function}()  .{f.method}()  |  {f.snippet}")
        print(f"\n{len(findings)} synchronous blocking wait(s) in {len(grouped)} function(s)")
        return 0

    existing = load_baseline(baseline_path)

    if args.update_baseline:
        entries = []
        for site in sorted(grouped):
            prev = existing.get(site, {})
            entries.append(
                {
                    "site": site,
                    "sites": len(grouped[site]),
                    "reason": prev.get("reason", ""),
                }
            )
        write_baseline(baseline_path, entries)
        blank = [e["site"] for e in entries if not e["reason"].strip()]
        print(f"wrote {baseline_path} with {len(entries)} exemption(s)")
        if blank:
            print(f"{len(blank)} entr(y|ies) need a written reason:")
            for s in blank:
                print(f"  {s}")
            return 1
        return 0

    problems: list[str] = []

    # 1. Unexempted sites.
    unexempted: list[str] = []
    for site in sorted(grouped):
        if site not in existing:
            for f in grouped[site]:
                unexempted.append(f"{f.path}:{f.line}  in {f.function}()  .{f.method}()")

    # 2. Ratchet violations and unreasoned entries.
    for site, entry in sorted(existing.items()):
        want = int(entry.get("sites", 0))
        have = len(grouped.get(site, []))
        if not str(entry.get("reason", "")).strip():
            problems.append(
                f"baseline entry {site!r} has an empty 'reason' — every exemption "
                f"must say why an unbounded wait is acceptable there."
            )
        if have == 0:
            problems.append(
                f"baseline entry {site!r} matches nothing any more (it recorded "
                f"{want}). The site was fixed, renamed or moved — delete the entry "
                f"from {BASELINE_PATH.as_posix()}."
            )
        elif have > want:
            problems.append(
                f"{site} now has {have} untimed wait(s), baseline allows {want}. "
                f"A NEW untimed call was added to an already-exempt function — route "
                f"it through a bounded wrapper, or raise the count with a reason."
            )
        elif have < want:
            problems.append(
                f"{site} now has only {have} untimed wait(s), baseline records "
                f"{want}. Tighten it: set \"sites\": {have} in "
                f"{BASELINE_PATH.as_posix()} (the baseline is a ratchet)."
            )

    ok = not unexempted and not problems
    if ok:
        print(
            f"OK: every synchronous subprocess wait in {SOURCE_DIR.as_posix()} is either "
            f"bounded or baselined ({len(findings)} baselined wait(s) across "
            f"{len(grouped)} function(s))."
        )
        return 0

    if unexempted:
        print(teach(unexempted, args.ci), file=sys.stderr)
    for p in problems:
        print(f"{'::error::' if args.ci else ''}{p}", file=sys.stderr)
    print(
        f"\n{'::error::' if args.ci else ''}FAILED: "
        f"{len(unexempted)} unexempted site(s), {len(problems)} baseline problem(s).",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
