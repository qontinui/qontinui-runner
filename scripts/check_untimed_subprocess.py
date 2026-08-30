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

WHAT IS SCANNED
---------------
Every in-repo Rust crate LINKED INTO THE RUNNER BINARY — i.e. every crate that
shares the runner's tokio runtime and therefore its 512-thread blocking pool.
The list is DISCOVERED, never hard-coded: `src-tauri/src` plus the transitive
closure of the `path = "…"` dependencies of `src-tauri/Cargo.toml`, keeping
only those that resolve inside this repository. Each contributes
`<crate>/src/**/*.rs`.

Discovery is the point. Plan `2026-08-21-runner-extract-crates-frontier-first`
is actively carving modules OUT of `src-tauri/src` into sibling crates
(`crates/runner-stats`, `crates/runner-win32` so far). A hard-coded root would
let every future phase silently remove code from coverage — and worse, the
ratchet would then tell the contributor to DELETE the newly-unresolvable
baseline entries, permanently ungating them. An extraction necessarily adds a
path dependency, so the closure picks the new crate up with no edit here.
Today that resolves to `src-tauri/src`, `src-tauri/clorinde/src`,
`crates/spec-check/src`, `crates/runner-stats/src`, `crates/runner-win32/src`.

Two things are deliberately OUT of scope, and `--list-roots` prints both so the
boundary is inspectable rather than assumed:

  * path deps resolving outside the repo (`../../qontinui-schemas/*`) — a
    different repository with its own CI, not even checked out in this repo's
    CI job;
  * in-repo workspace members that are NOT linked into the runner
    (`crates/comprehension`, `crates/qontinui-app-generator`,
    `crates/qontinui-backend-generator`) — they ship as their own processes, so
    a blocking wait there cannot consume a runner blocking-pool thread. If one
    of them ever becomes a runner dependency, the closure starts scanning it
    automatically and its waits must be fixed or baselined at that moment.

Build scripts (`build.rs`) are outside `<crate>/src` and are not scanned: they
run at compile time, in a process with no tokio runtime and therefore no
blocking pool to exhaust.

HOW IT TELLS `std::process` FROM `tokio::process`
-------------------------------------------------
A bare regex on `.output()` cannot: many files use both, and `reqwest`'s
`Response::status()` alone would produce hundreds of false hits. So the checker
resolves the RECEIVER of each blocking wait back to its origin:

  * `process_helpers::no_window(..)` / `cmd_no_window()` / `std::process::Command::new(..)`
    (and a bare `Command::new(..)`, or an ALIASED `C::new(..)` from
    `use std::process::Command as C;`, in a file that imports it from `std`)
        -> SYNC, gate applies
  * `tokio_no_window(..)` / `tokio_cmd_no_window()` / `tokio::process::Command::new(..)`
    (and the same bare/aliased spellings resolving to `tokio::process`)
        -> ASYNC, ignored: an awaited async wait occupies no blocking-pool thread
  * anything the checker cannot resolve to a SYNC origin -> ignored

Resolution is local: `let` bindings and typed `fn` parameters inside the
enclosing function, this file's `use`/`type` aliases for
`std|tokio::process::{Command, Child}`, and a crate-wide map of TOP-LEVEL
helper functions whose declared return type is a `std::process::Command`. That
helper map is consulted only when the receiver root is an actual CALL
(`build(..).output()`), never for a bare identifier that merely shares a name
with a helper (`let build = …; build.output()`), and this file's own helpers
win over the crate-wide map. When two top-level helpers in different files
share a name and disagree, the map resolves SYNC — fail closed, so a
same-named async helper elsewhere cannot un-gate a real site. `.await` after a
wait is treated as proof of the async arm regardless of what resolution said.

A name imported as BOTH a `std` and a `tokio` `Command` in one file resolves to
SYNC (fail closed). Rust forbids the duplicate at one scope (E0252), so the
only ways to reach that state are `#[cfg]`-gated or block-scoped imports —
where the `std` arm really does carry an unbounded wait. Zero files in this
tree do it today.

Deliberately conservative: unresolvable receivers are NOT flagged. A gate that
cries wolf gets disabled, and that reopens the class this exists to close.

WHAT IT DOES NOT SEE — stated so the claim above is not overstated
------------------------------------------------------------------
  * A wait produced by MACRO EXPANSION. `macro_rules!` bodies are scanned as
    plain text, so a wait written literally inside one is seen at its
    definition site, but a wait assembled from macro fragments
    (`$recv.$method()`) is not.
  * `#[path = "…"] mod` includes are resolved by the crate ROOT the file is
    reached from, not by this checker; the included file is still scanned in
    its own right (every `.rs` under a crate's `src/` is scanned regardless of
    whether the module tree reaches it), so its waits are found — but its
    baseline key is its own path, not the including module's.
  * Grouped-path imports of the form `use std::{process::Command, io};`. The
    alias reader handles `use std::process::Command;`,
    `use std::process::Command as C;` and
    `use std::process::{Command as C, Stdio};`, plus `type Cmd = …;` aliases of
    any of those. Zero files in this tree use the nested-group spelling.
  * A receiver whose kind is only knowable from a struct FIELD's declared type
    (`self.cmd.output()`).
Each of those resolves UNKNOWN and is therefore NOT flagged — the same
fail-quiet posture as every other unresolvable receiver.

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

WHAT IS SKIPPED WHOLESALE
-------------------------
  * `#[cfg(test)]` items and `#[test]` functions — a fixture is not a periodic path.
  * `src-tauri/src/process_helpers.rs` itself — it *implements* the bounded
    primitive, so it necessarily contains the raw calls.
  * A `Child::wait()` / `wait_with_output()` on a receiver that a `.kill()` on
    the SAME receiver precedes, in the same block, within 3 statements —
    reaping a child you just killed is bounded by construction. Same receiver
    and adjacency are both required: `a.kill(); … b.wait()` is still flagged,
    and so is a `kill()` that is separated from the `wait()` by a block
    boundary.

THE BASELINE
------------
The 2026-08-30 sweep fixed the periodic/hot-path sites. The residue is real and
mostly legitimate: one-shot CLI paths, user-triggered actions, startup and
shutdown work. Rather than pretend it does not exist, every surviving site is
enumerated in `scripts/untimed-subprocess-baseline.json`, keyed by
`<path>::<qualified fn>` with a REQUIRED written reason.

The function is QUALIFIED by everything that encloses it — `mod`, `impl`,
`trait`, and any outer `fn` — so `impl A { fn run }` keys as `…rs::A::run` and
`impl B { fn run }` as `…rs::B::run`. A trait impl keys as `<Type as Trait>`.
Without that, removing a wait from `B::run` while adding one to `A::run` left
the count constant and the gate green. The key survives edits above and below
the site, which a line number does not.

The baseline is a RATCHET:
  * a site in a function with no baseline entry            -> FAIL
  * more sites in a function than its entry records        -> FAIL
  * fewer sites than recorded                              -> FAIL, "tighten it"
  * an entry with an empty `reason`                        -> FAIL
  * an entry whose `reason_covers_sites` != `sites`        -> FAIL
  * two entries with the same `site`                       -> FAIL
  * a baseline that is not `"format": 2`                   -> FAIL, with a
                                                              migration message
so it can only ever shrink.

WHAT `--update-baseline` CAN AND CANNOT SMUGGLE IN
--------------------------------------------------
Every entry records `reason_covers_sites`: the count the written reason was
authored against. The checker requires it to equal `sites`, and
`--update-baseline` CLEARS the reason whenever a site's count INCREASES. So:

  * a brand-new `path::fn` key            -> empty reason -> rejected
  * a NEW wait inside an already-baselined function
                                          -> reason cleared -> rejected
  * hand-raising `sites` without touching the reason
                                          -> reason_covers_sites mismatch -> rejected
  * a count DECREASE                      -> reason kept, `reason_covers_sites`
                                             lowered. A strict improvement does
                                             not need fresh prose.

What it does NOT detect: a human who hand-edits BOTH `sites` and
`reason_covers_sites` and leaves stale prose behind. That is a false statement
standing in a reviewable diff, not a silent bypass — no static check can tell
apposite prose from inapposite prose. The property the gate actually enforces
is "no count may rise without a human editing the reason field in the same
diff", not "the reason is true".

USAGE
-----
    python3 scripts/check_untimed_subprocess.py              # check (exit 1 on a finding)
    python3 scripts/check_untimed_subprocess.py --list       # print every SYNC wait found
    python3 scripts/check_untimed_subprocess.py --list-roots # print the discovered scan scope
    python3 scripts/check_untimed_subprocess.py --update-baseline
    python3 scripts/check_untimed_subprocess.py --root <dir> # check a different tree

Pure stdlib, no third-party imports: it runs identically on the Windows dev box
and on the ubuntu CI runner. Manifests are read with `tomllib` (stdlib since
3.11) and fall back to a narrow regex reader on older interpreters.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

try:  # stdlib since 3.11; the fallback keeps 3.8-3.10 dev boxes working.
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - interpreter-version dependent
    tomllib = None  # type: ignore[assignment]

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

#: The crate whose Cargo.toml roots path-dependency discovery, relative to the
#: repo root. This is the runner binary crate.
BINARY_CRATE_DIR = Path("src-tauri")

#: The workspace manifest, relative to the repo root.
WORKSPACE_MANIFEST = Path("Cargo.toml")

#: Baseline location, relative to the repo root.
BASELINE_PATH = Path("scripts") / "untimed-subprocess-baseline.json"

#: Baseline schema version. Bumped when the `site` key format or the required
#: entry fields change; an older file fails the gate CLOSED with a migration
#: message rather than silently matching nothing.
BASELINE_FORMAT = 2

#: Files exempt from scanning entirely, relative to the repo root (POSIX form).
SKIPPED_FILES = {
    # Implements the bounded primitive; the raw calls here ARE the fix.
    "src-tauri/src/process_helpers.rs",
}

#: Blocking waits with no time bound. `try_wait()` is absent on purpose — it
#: does not block. `.spawn()` is absent on purpose — see the module docstring.
BLOCKING_WAITS = ("output", "status", "wait", "wait_with_output")

#: Constructors that yield a synchronous `std::process::Command`.
SYNC_ORIGINS = ("no_window", "cmd_no_window")

#: Constructors that yield an asynchronous `tokio::process::Command`.
ASYNC_ORIGINS = ("tokio_no_window", "tokio_cmd_no_window")

#: How many statements a `.kill()` may precede a `wait()` by and still count as
#: "reaping the child you just killed".
KILL_ADJACENCY_STATEMENTS = 3

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


# ---------------------------------------------------------------------------
# Item qualification: mod / impl / trait / enclosing fn
# ---------------------------------------------------------------------------


def _type_label(expr: str) -> str:
    """Reduce a type expression to a single identifier usable in a key."""
    s = expr.strip()
    s = re.sub(r"\bdyn\b|\bmut\b|\bimpl\b", " ", s)
    s = s.split("<", 1)[0]
    s = s.replace("&", " ").replace("'", " ").strip()
    s = s.split("::")[-1]
    s = re.sub(r"[^A-Za-z0-9_]", "", s)
    return s or "impl"


def _strip_leading_generics(s: str) -> str:
    s = s.lstrip()
    if not s.startswith("<"):
        return s
    depth = 0
    for i, c in enumerate(s):
        if c == "<":
            depth += 1
        elif c == ">":
            depth -= 1
            if depth == 0:
                return s[i + 1 :]
    return s


def _split_impl_for(head: str) -> tuple[str, str] | None:
    """Split `Trait for Type` at depth 0; None when the impl is inherent."""
    depth = 0
    for m in re.finditer(r"[<>()\[\]]|\bfor\b", head):
        tok = m.group(0)
        if tok in "<([":
            depth += 1
        elif tok in ">)]":
            depth -= 1
        elif tok == "for" and depth == 0:
            return head[: m.start()], head[m.end() :]
    return None


def _impl_is_item(src: str, idx: int) -> bool:
    """True when the `impl` at ``idx`` starts an item, not `-> impl Trait`."""
    i = idx - 1
    while True:
        while i >= 0 and src[i].isspace():
            i -= 1
        if i < 0:
            return True
        if src[i] in ";}{]":
            return True
        # `unsafe impl` / `default impl` are still items.
        j = i
        while j >= 0 and (src[j].isalnum() or src[j] == "_"):
            j -= 1
        word = src[j + 1 : i + 1]
        if word in ("unsafe", "default"):
            i = j
            continue
        return False


@dataclass(frozen=True)
class Container:
    start: int  # offset of the body's `{`
    end: int  # offset just past the body's `}`
    label: str


def find_containers(src: str) -> list[Container]:
    """`mod` / `impl` / `trait` blocks, with the label each contributes to a key."""
    out: list[Container] = []
    n = len(src)

    for m in re.finditer(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{", src):
        brace = m.end() - 1
        out.append(Container(brace, _match_brace(src, brace), m.group(1)))

    for m in re.finditer(r"\btrait\s+([A-Za-z_][A-Za-z0-9_]*)", src):
        j = m.end()
        while j < n and src[j] not in "{;":
            j += 1
        if j >= n or src[j] == ";":
            continue
        out.append(Container(j, _match_brace(src, j), m.group(1)))

    for m in re.finditer(r"\bimpl\b", src):
        if not _impl_is_item(src, m.start()):
            continue
        j = m.end()
        depth = 0
        while j < n:
            c = src[j]
            if c == "<":
                depth += 1
            elif c == ">":
                depth -= 1
            elif c == "{" and depth <= 0:
                break
            elif c == ";" and depth <= 0:
                break
            j += 1
        if j >= n or src[j] == ";":
            continue
        head = _strip_leading_generics(src[m.end() : j])
        w = re.search(r"\bwhere\b", head)
        if w:
            head = head[: w.start()]
        split = _split_impl_for(head)
        if split is None:
            label = _type_label(head)
        else:
            label = f"<{_type_label(split[1])} as {_type_label(split[0])}>"
        out.append(Container(j, _match_brace(src, j), label))

    return out


@dataclass(frozen=True)
class Function:
    """One function body, with the offsets needed to map hits back to lines."""

    name: str  # qualified: `mod::Impl::fn`, `outer::inner`, …
    simple: str  # the bare `fn` identifier
    start: int  # offset of the body's `{`
    end: int  # offset just past the body's `}`
    body: str
    nested: bool  # True when another `fn` body encloses this one


_FN_RE = re.compile(
    r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^{;]*?>)?\s*\(",
)


def find_functions(src: str) -> list[Function]:
    """Every `fn` in ``src``, with nested `fn`s also listed, name QUALIFIED.

    A closure lives inside its enclosing `fn` and is intentionally NOT split
    out: "the same function body" is the unit the baseline is keyed on. An
    inner `fn`, by contrast, IS its own body and gets its own key — qualified
    by the outer one, because inner helper names (`give_up`, `helper`) collide
    constantly.
    """
    raw: list[tuple[str, int, int]] = []  # (simple name, body start, body end)
    n = len(src)
    for m in _FN_RE.finditer(src):
        # Find the body brace: skip the parameter list and the return type.
        i = m.end() - 1  # at '('
        depth = 0
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
        raw.append((m.group(1), j, _match_brace(src, j)))

    containers = find_containers(src)
    fns: list[Function] = []
    for name, start, end in raw:
        quals: list[tuple[int, str]] = [
            (c.start, c.label) for c in containers if c.start < start < c.end
        ]
        nested = False
        for oname, ostart, oend in raw:
            if ostart < start < oend:
                quals.append((ostart, oname))
                nested = True
        quals.sort()
        qualified = "::".join([q[1] for q in quals] + [name])
        fns.append(Function(qualified, name, start, end, src[start:end], nested))
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


def receiver_root(src: str, dot_idx: int) -> tuple[str, int]:
    """Walk back from the `.` of a method call to the root of its chain.

    Returns ``(root, end_offset)`` where ``root`` is a (possibly
    `::`-qualified) path string — "" when the chain does not root in something
    nameable — and ``end_offset`` is the index just past the root's last
    identifier character, so the caller can tell a CALL (`build(..)`) from a
    bare binding (`build`).
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
            root_end = i + 1
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
                return "::".join(path), root_end
            return ident, root_end
        return "", -1
    return "", -1


_CALL_AFTER_ROOT = re.compile(r"\s*(?:::\s*<[^;{}]*?>\s*)?\(")


def _root_is_call(src: str, root_end: int) -> bool:
    """True when the resolved root is immediately invoked: `helper(..)`."""
    if root_end < 0:
        return False
    return bool(_CALL_AFTER_ROOT.match(src, root_end))


# --- `use` / `type` aliases for std|tokio process::{Command, Child} ---------

_USE_PROCESS_RE = re.compile(
    r"\buse\s+(std|tokio)\s*::\s*process\s*::\s*"
    r"(\{[^}]*\}|[A-Za-z_][A-Za-z0-9_]*(?:\s+as\s+[A-Za-z_][A-Za-z0-9_]*)?)\s*;"
)
_USE_ITEM_RE = re.compile(
    r"^(Command|Child)\b(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?$"
)
_TYPE_ALIAS_RE = re.compile(r"\btype\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^;]+);")


def command_aliases(src: str) -> dict[str, str]:
    """Every local name in this file that denotes a `Command`/`Child` type.

    Maps the name -> SYNC / ASYNC. Covers the plain import, the `as` rename,
    the braced group, and `type` aliases of any of those. When one name is
    bound to BOTH kinds (only reachable via `#[cfg]`-gated or block-scoped
    imports — a duplicate at one scope is E0252) it resolves SYNC: fail closed.
    """
    out: dict[str, str] = {}

    def put(name: str, kind: str) -> None:
        prev = out.get(name)
        out[name] = SYNC if (prev is not None and prev != kind) else kind

    for m in _USE_PROCESS_RE.finditer(src):
        kind = SYNC if m.group(1) == "std" else ASYNC
        spec = m.group(2).strip()
        items = spec[1:-1].split(",") if spec.startswith("{") else [spec]
        for item in items:
            mm = _USE_ITEM_RE.match(item.strip())
            if mm:
                put(mm.group(2) or mm.group(1), kind)

    for m in _TYPE_ALIAS_RE.finditer(src):
        kind = classify_type(m.group(2), out)
        if kind != UNKNOWN:
            put(m.group(1), kind)
    return out


def classify_type(ty: str, aliases: dict[str, str]) -> str:
    """SYNC / ASYNC / UNKNOWN for a type expression."""
    if re.search(r"\btokio::process::(?:Command|Child)\b", ty):
        return ASYNC
    if re.search(r"\bstd::process::(?:Command|Child)\b", ty):
        return SYNC
    for name, kind in aliases.items():
        if re.search(r"(?<![:\w])" + re.escape(name) + r"\b", ty):
            return kind
    return UNKNOWN


def classify_origin(expr: str, aliases: dict[str, str], default: str) -> str:
    """Classify a constructor-ish expression as SYNC / ASYNC / UNKNOWN."""
    if re.search(r"\b(?:tokio_no_window|tokio_cmd_no_window)\b", expr):
        return ASYNC
    if re.search(r"\btokio::process::Command\s*::\s*new\b", expr):
        return ASYNC
    if re.search(r"\b(?:no_window|cmd_no_window)\b", expr):
        return SYNC
    if re.search(r"\bstd::process::Command\s*::\s*new\b", expr):
        return SYNC
    for name, kind in aliases.items():
        if re.search(r"(?<![:\w])" + re.escape(name) + r"\s*::\s*new\b", expr):
            return kind
    if re.search(r"(?<![:\w])Command\s*::\s*new\b", expr):
        return default
    return UNKNOWN


def file_command_default(aliases: dict[str, str]) -> str:
    """What a bare `Command::new(..)` means in this file, from its imports."""
    return aliases.get("Command", UNKNOWN)


_LET_RE = re.compile(
    r"\blet\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*([^=;]+?))?\s*=\s*",
)


def local_bindings(body: str, aliases: dict[str, str], default: str) -> dict[str, str]:
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
        kind = classify_type(ann, aliases) if ann else UNKNOWN
        if kind == UNKNOWN:
            kind = classify_origin(ann, aliases, default)
        if kind == UNKNOWN:
            kind = classify_origin(rhs, aliases, default)
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


def param_bindings(
    src: str, fn: Function, aliases: dict[str, str], default: str
) -> dict[str, str]:
    """Map parameter name -> SYNC / ASYNC from the signature's declared types."""
    # The signature is the text between the preceding `fn` and the body brace.
    head_start = src.rfind("fn ", 0, fn.start)
    if head_start == -1:
        return {}
    head = src[head_start : fn.start]
    kinds: dict[str, str] = {}
    for m in _PARAM_RE.finditer(head):
        ty = m.group(2)
        kind = classify_type(ty, aliases)
        if kind == UNKNOWN and re.search(r"(?<![:\w])(?:Command|Child)\b", ty):
            kind = default
        if kind != UNKNOWN:
            kinds[m.group(1)] = kind
    return kinds


_FN_RET_RE = re.compile(
    r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^{;]*?>)?\s*\([^;{]*?\)\s*->\s*([^{;]+)",
    re.S,
)


# ---------------------------------------------------------------------------
# Per-file parse (done once, shared by the helper map and the scan)
# ---------------------------------------------------------------------------


@dataclass
class FileInfo:
    rel: str
    raw: str
    src: str  # noise- and test-stripped, offsets preserved
    aliases: dict[str, str]
    default: str
    fns: list[Function]
    helpers: dict[str, str]  # THIS file's `fn … -> Command` helpers
    top_helpers: dict[str, str]  # …restricted to non-nested ones


def parse_file(rel: str, raw: str) -> FileInfo:
    src = strip_test_items(strip_noise(raw))
    aliases = command_aliases(src)
    default = file_command_default(aliases)
    fns = find_functions(src)
    nested_starts = {f.start for f in fns if f.nested}

    helpers: dict[str, str] = {}
    top_helpers: dict[str, str] = {}
    for m in _FN_RET_RE.finditer(src):
        ret = m.group(2)
        kind = classify_type(ret, aliases)
        if kind == UNKNOWN and re.search(r"(?<![:\w])Command\b", ret) and default != UNKNOWN:
            kind = default
        if kind == UNKNOWN:
            continue
        helpers[m.group(1)] = kind
        # `-> Command` ends at the body brace; find it to test nesting.
        brace = src.find("{", m.end() - len(ret))
        if brace != -1 and brace not in nested_starts:
            top_helpers[m.group(1)] = kind
    return FileInfo(rel, raw, src, aliases, default, fns, helpers, top_helpers)


def helper_return_kinds(files: Iterable[FileInfo]) -> dict[str, str]:
    """Crate-wide map: TOP-LEVEL helper fn name -> SYNC / ASYNC, by return type.

    Covers the common `fn git_cmd(repo: &Path) -> std::process::Command` shape,
    so `git_cmd(p).output()` resolves even though the constructor is elsewhere.

    Built from noise- and test-stripped sources, so neither a doc-comment
    example nor a `#[cfg(test)]` fixture can enter it, and only from functions
    that are not nested inside another function, so a file-local helper does not
    become a crate-wide fact. When two files disagree about a name the map
    resolves SYNC — fail closed, and independent of filesystem walk order.
    """
    kinds: dict[str, str] = {}
    for fi in files:
        for name, kind in fi.top_helpers.items():
            prev = kinds.get(name)
            kinds[name] = SYNC if (prev is not None and prev != kind) else kind
    return kinds


# ---------------------------------------------------------------------------
# The scan
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Finding:
    path: str  # repo-relative, POSIX
    line: int
    function: str  # QUALIFIED name
    method: str
    snippet: str

    @property
    def key(self) -> str:
        return f"{self.path}::{self.function}"


_WAIT_RE = re.compile(
    r"\.\s*(" + "|".join(sorted(BLOCKING_WAITS, key=len, reverse=True)) + r")\s*\(\s*\)"
)

_KILL_RE = re.compile(r"\.\s*kill\s*\(")


def _followed_by_await(src: str, end: int) -> bool:
    """True when the call at ``end`` is immediately `.await`-ed (async arm)."""
    m = re.match(r"\s*\??\s*\.\s*await\b", src[end : end + 32])
    return bool(m)


def _killed_before(body: str, offset: int, recv: str) -> bool:
    """True when ``recv`` was `.kill()`-ed just before the wait at ``offset``.

    Reaping a child you have just killed is bounded by construction — that is
    exactly what `run_with_timeout`'s own expiry path does — so a `wait()` that
    follows a `kill()` is not the defect.

    BOTH conditions are required, and neither was checked before:

      * SAME RECEIVER. `a.kill(); … b.wait()` used to suppress the gate on
        `b.wait()` because *some* `.kill(` appeared earlier in the body.
      * ADJACENCY. At most ``KILL_ADJACENCY_STATEMENTS`` statements may
        separate the two, and the span may CLOSE blocks (the `#[cfg]`-gated
        `{ child.kill(); }` arm immediately above a `child.wait()` is the real
        shape in `ai_provider/pi_cli.rs`) but may not OPEN one — a `wait()`
        nested inside a block that starts after the `kill()` is on a different
        path and is not covered by it.
    """
    if not recv:
        return False
    for m in _KILL_RE.finditer(body, 0, offset):
        kill_recv, _ = receiver_root(body, m.start())
        if kill_recv != recv:
            continue
        span = body[m.end() : offset]
        if "{" in span:
            continue
        if span.count(";") > KILL_ADJACENCY_STATEMENTS:
            continue
        return True
    return False


def scan_file(fi: FileInfo, helper_kinds: dict[str, str]) -> list[Finding]:
    src = fi.src
    if "output" not in src and "status" not in src and "wait" not in src:
        return []
    default = fi.default
    aliases = fi.aliases
    fns = fi.fns
    raw = fi.raw
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
            binds = param_bindings(src, fn, aliases, default)
            binds.update(local_bindings(fn.body, aliases, default))
            cache[fn.start] = binds
        binds = cache[fn.start]

        root, root_end = receiver_root(src, m.start())
        if not root:
            continue
        is_call = _root_is_call(src, root_end)
        tail = root.rsplit("::", 1)[-1]
        owner = root.rsplit("::", 2)[-2] if root.count("::") >= 1 else ""

        kind = UNKNOWN
        if is_call and tail in SYNC_ORIGINS:
            kind = SYNC
        elif is_call and tail in ASYNC_ORIGINS:
            kind = ASYNC
        elif tail == "new" and root.endswith("tokio::process::Command::new"):
            kind = ASYNC
        elif tail == "new" and root.endswith("std::process::Command::new"):
            kind = SYNC
        elif tail == "new" and owner in aliases:
            kind = aliases[owner]
        elif tail == "new" and owner == "Command":
            kind = default
        elif root in binds:
            kind = binds[root]
        elif is_call and tail in fi.helpers:
            kind = fi.helpers[tail]
        elif is_call and tail in helper_kinds:
            kind = helper_kinds[tail]

        if kind != SYNC:
            continue
        if method in ("wait", "wait_with_output") and _killed_before(
            fn.body, m.start() - fn.start, root
        ):
            continue

        ln = line_of(m.start())
        snippet = raw_lines[ln - 1].strip() if ln - 1 < len(raw_lines) else ""
        findings.append(Finding(fi.rel, ln, fn.name, method, snippet[:160]))
    return findings


# ---------------------------------------------------------------------------
# Scan scope discovery
# ---------------------------------------------------------------------------


def _read_manifest(path: Path) -> dict:
    """`{"members": [...], "path_deps": [...]}` from a Cargo.toml."""
    text = path.read_text(encoding="utf-8", errors="replace")
    members: list[str] = []
    path_deps: list[str] = []
    if tomllib is not None:
        try:
            data = tomllib.loads(text)
        except Exception:
            data = {}
        members = list(data.get("workspace", {}).get("members", []) or [])

        def harvest(table: object) -> None:
            if not isinstance(table, dict):
                return
            for value in table.values():
                if isinstance(value, dict) and isinstance(value.get("path"), str):
                    path_deps.append(value["path"])

        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            harvest(data.get(key))
        for tgt in (data.get("target") or {}).values():
            if isinstance(tgt, dict):
                for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                    harvest(tgt.get(key))
    else:  # pragma: no cover - only on interpreters older than 3.11
        block = re.search(r"^\s*members\s*=\s*\[(.*?)\]", text, re.S | re.M)
        if block:
            members = re.findall(r'"([^"]+)"', block.group(1))
        path_deps = re.findall(r'^\s*[^#\n]*\bpath\s*=\s*"([^"]+)"', text, re.M)
    return {"members": members, "path_deps": path_deps}


@dataclass(frozen=True)
class Scope:
    """What the scan covers, and what it deliberately does not."""

    scanned: list[Path]  # `<crate>/src` dirs, absolute
    external: list[str]  # path deps that resolve outside this repo
    unlinked: list[str]  # in-repo workspace members not linked into the runner


def discover_scope(root: Path) -> Scope:
    """Every in-repo crate `src/` dir LINKED INTO THE RUNNER BINARY.

    Discovery, not a hard-coded list: plan
    `2026-08-21-runner-extract-crates-frontier-first` is moving modules out of
    `src-tauri/src` into sibling crates, and a hard-coded root would drop each
    extracted module out of coverage silently. An extraction phase necessarily
    adds a `path = "…"` dependency to `src-tauri/Cargo.toml`, so the transitive
    closure of those picks the new crate up with no edit here.
    """
    root = root.resolve()
    seen: set[Path] = set()
    external: set[str] = set()
    queue: list[Path] = [root / BINARY_CRATE_DIR]

    while queue:
        crate = queue.pop()
        try:
            resolved = crate.resolve()
        except OSError:
            continue
        if resolved in seen:
            continue
        manifest = resolved / "Cargo.toml"
        if not manifest.is_file():
            continue
        seen.add(resolved)
        for dep in _read_manifest(manifest)["path_deps"]:
            target = (resolved / dep).resolve()
            if root == target or root in target.parents:
                queue.append(target)
            else:
                try:
                    external.add(target.relative_to(root.parent).as_posix())
                except ValueError:
                    external.add(target.as_posix())

    dirs = sorted(d / "src" for d in seen if (d / "src").is_dir())
    if not dirs:
        raise SystemExit(
            f"error: discovered no Rust crate source tree under {root}. Expected at "
            f"least {(root / BINARY_CRATE_DIR / 'src').as_posix()}."
        )

    unlinked: list[str] = []
    ws = root / WORKSPACE_MANIFEST
    if ws.is_file():
        members: list[Path] = []
        for member in _read_manifest(ws)["members"]:
            if any(ch in member for ch in "*?["):
                members.extend(sorted(p for p in root.glob(member) if p.is_dir()))
            else:
                members.append(root / member)
        for m in members:
            try:
                r = m.resolve()
            except OSError:
                continue
            if r not in seen and (r / "src").is_dir():
                unlinked.append(r.relative_to(root).as_posix())
    return Scope(dirs, sorted(external), sorted(set(unlinked)))


def collect_sources(root: Path) -> dict[str, str]:
    sources: dict[str, str] = {}
    for base in discover_scope(root).scanned:
        for p in sorted(base.rglob("*.rs")):
            rel = p.relative_to(root).as_posix()
            if rel in SKIPPED_FILES or rel in sources:
                continue
            sources[rel] = p.read_text(encoding="utf-8", errors="replace")
    return sources


def run_scan(root: Path) -> list[Finding]:
    files = [parse_file(rel, raw) for rel, raw in collect_sources(root).items()]
    helper_kinds = helper_return_kinds(files)
    findings: list[Finding] = []
    for fi in files:
        findings.extend(scan_file(fi, helper_kinds))
    findings.sort(key=lambda f: (f.path, f.line))
    return findings


# ---------------------------------------------------------------------------
# Baseline
# ---------------------------------------------------------------------------


class BaselineError(Exception):
    """The baseline file cannot be trusted — fail CLOSED, never pass."""


_MIGRATION_HINT = (
    "Regenerate it with:\n"
    "    python3 scripts/check_untimed_subprocess.py --update-baseline\n"
    "which reads the old format, carries every reason forward to the new key "
    "where the mapping is unambiguous, and reports the ones you must re-write "
    "by hand."
)


def load_baseline(path: Path, *, lenient: bool = False) -> dict[str, dict]:
    """Read the baseline, FAILING CLOSED on any format it does not understand.

    ``lenient`` is used only by `--update-baseline`, which is allowed to read an
    older format in order to migrate the reasons forward.
    """
    if not path.is_file():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise BaselineError(f"{path.as_posix()} is not valid JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise BaselineError(f"{path.as_posix()} must be a JSON object.")

    fmt = data.get("format")
    entries = data.get("exemptions", [])
    if not isinstance(entries, list):
        raise BaselineError(f"{path.as_posix()}: 'exemptions' must be a list.")

    out: dict[str, dict] = {}
    for entry in entries:
        if not isinstance(entry, dict) or "site" not in entry:
            raise BaselineError(
                f"{path.as_posix()}: every exemption must be an object with a "
                f"'site' key; found {entry!r}."
            )
        site = entry["site"]
        if site in out:
            raise BaselineError(
                f"{path.as_posix()}: duplicate exemption for {site!r}. Two entries "
                f"with the same 'site' silently collapse to one and the earlier "
                f"one's count and reason are lost — merge them into a single entry."
            )
        out[site] = entry

    if fmt == BASELINE_FORMAT:
        return out
    if lenient:
        return out
    raise BaselineError(
        f"{path.as_posix()} declares format {fmt!r}, but this checker requires "
        f"format {BASELINE_FORMAT}.\n"
        f"Format {BASELINE_FORMAT} changed two things:\n"
        f"  1. 'site' now qualifies the function by its enclosing impl / trait / "
        f"mod / outer fn, so `impl A {{ fn run }}` keys as '<path>::A::run' "
        f"instead of colliding with `impl B {{ fn run }}` under '<path>::run'.\n"
        f"  2. every entry carries 'reason_covers_sites' — the count the written "
        f"reason was authored against — which must equal 'sites'.\n"
        f"An old-format file is REJECTED rather than silently matching nothing "
        f"(which would read as 'the site was fixed' and ungate every entry).\n"
        f"{_MIGRATION_HINT}"
    )


def write_baseline(path: Path, entries: list[dict]) -> None:
    doc = {
        "_comment": [
            "Sites where a synchronous std::process::Command is waited on with no",
            "time bound, and that are ACCEPTED. Enforced by",
            "scripts/check_untimed_subprocess.py and",
            ".github/workflows/forbid-untimed-subprocess.yml.",
            "",
            "'format' is the schema version. The checker REJECTS any other value",
            "rather than matching nothing against it, because 'matches nothing'",
            "reads as 'the site was fixed' and would ungate every entry at once.",
            "",
            "'site' is '<repo-relative path>::<qualified fn>' — qualified by every",
            "enclosing impl / trait / mod / outer fn, so `impl A { fn run }` and",
            "`impl B { fn run }` are different sites. A trait impl reads as",
            "'<Type as Trait>'. The key is stable across edits above and below the",
            "call, which a line number is not.",
            "'sites' is how many blocking waits that function currently contains;",
            "the checker fails if the real number differs in EITHER direction, so",
            "the baseline can only shrink and cannot silently rot.",
            "'reason' is REQUIRED and must say why an unbounded wait is acceptable",
            "HERE — i.e. why this code is not on a periodic or hot path.",
            "'reason_covers_sites' is the count that reason was written against and",
            "MUST equal 'sites'. --update-baseline clears the reason whenever a",
            "count rises, so a new wait added inside an already-exempt function",
            "cannot inherit the prose written for a different call; and raising",
            "'sites' by hand without touching the reason fails on the mismatch.",
            "",
            "Do NOT add an entry to silence a periodic caller. Route it through",
            "run_probe / output_with_timeout / run_with_timeout instead.",
        ],
        "format": BASELINE_FORMAT,
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


def _legacy_key(site: str) -> str:
    """The format-1 spelling of ``site``: path plus the BARE function name."""
    path, _, qualified = site.partition("::")
    return f"{path}::{qualified.rsplit('::', 1)[-1]}"


def rebuild_entries(
    grouped: dict[str, list[Finding]], existing: dict[str, dict]
) -> tuple[list[dict], list[str], list[str]]:
    """Regenerate the exemption list, carrying reasons forward HONESTLY.

    Returns ``(entries, cleared, unmatched)`` — the entries, the sites whose
    reason was dropped because their count rose (or because an old-format key
    was ambiguous), and the baseline keys that now match nothing.
    """
    # Old-format keys are ambiguous exactly when two new keys share one.
    legacy_owner: dict[str, list[str]] = {}
    for site in grouped:
        legacy_owner.setdefault(_legacy_key(site), []).append(site)

    entries: list[dict] = []
    cleared: list[str] = []
    consumed: set[str] = set()
    for site in sorted(grouped):
        count = len(grouped[site])
        prev = existing.get(site)
        if prev is None:
            legacy = _legacy_key(site)
            if len(legacy_owner.get(legacy, [])) == 1:
                prev = existing.get(legacy)
        if prev is not None:
            consumed.add(prev["site"])
        reason = str((prev or {}).get("reason", "") or "")
        covered = (prev or {}).get("reason_covers_sites")
        if covered is None:
            # format 1 had no such field; the reason was written for its count.
            covered = (prev or {}).get("sites", 0)
        try:
            covered = int(covered)
        except (TypeError, ValueError):
            covered = 0
        if reason.strip() and count > covered:
            reason = ""
            cleared.append(site)
        entries.append(
            {
                "site": site,
                "sites": count,
                # Always equal to `sites` in a freshly written baseline: either
                # the reason survived (its count did not rise) or it was blanked
                # and has to be re-authored against this count anyway.
                "reason_covers_sites": count,
                "reason": reason,
            }
        )
    unmatched = sorted(set(existing) - consumed)
    return entries, cleared, unmatched


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
        f"{err}  {{ \"site\": \"<path>::<qualified fn>\", \"sites\": <n>,",
        f"{err}    \"reason_covers_sites\": <n>, \"reason\": \"<why it is not periodic>\" }}",
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
        "--list-roots",
        action="store_true",
        help="print the discovered scan scope and exit",
    )
    ap.add_argument(
        "--update-baseline",
        action="store_true",
        help="rewrite the baseline from the current tree (new entries, and any "
        "whose count ROSE, get an EMPTY reason)",
    )
    ap.add_argument("--ci", action="store_true", help="emit GitHub ::error:: annotations")
    args = ap.parse_args()

    root: Path = args.root.resolve()
    baseline_path = root / BASELINE_PATH

    if args.list_roots:
        scope = discover_scope(root)
        print("SCANNED — in-repo crates linked into the runner binary, discovered")
        print("from the transitive `path = ` deps of src-tauri/Cargo.toml:")
        for d in scope.scanned:
            print(f"  {d.relative_to(root).as_posix()}")
        print("\nNOT SCANNED — path deps outside this repo (separate repo, own CI,")
        print("not even checked out in this repo's CI job):")
        for e in scope.external or ["(none)"]:
            print(f"  {e}")
        print("\nNOT SCANNED — in-repo workspace members that are NOT linked into the")
        print("runner binary, so their blocking waits cannot touch the runner's tokio")
        print("blocking pool. They ship as their own processes:")
        for e in scope.unlinked or ["(none)"]:
            print(f"  {e}")
        return 0

    findings = run_scan(root)
    grouped = group(findings)

    if args.list:
        for f in findings:
            print(f"{f.path}:{f.line}  {f.function}()  .{f.method}()  |  {f.snippet}")
        print(f"\n{len(findings)} synchronous blocking wait(s) in {len(grouped)} function(s)")
        return 0

    if args.update_baseline:
        existing = load_baseline(baseline_path, lenient=True)
        entries, cleared, unmatched = rebuild_entries(grouped, existing)
        write_baseline(baseline_path, entries)
        blank = [e["site"] for e in entries if not str(e["reason"]).strip()]
        print(f"wrote {baseline_path} with {len(entries)} exemption(s)")
        if cleared:
            print(
                f"\n{len(cleared)} entr(y|ies) had their reason CLEARED because the "
                f"site count ROSE — a new untimed wait was added inside an "
                f"already-exempt function and must not inherit prose written for a "
                f"different call:"
            )
            for s in cleared:
                print(f"  {s}")
        if unmatched:
            print(
                f"\n{len(unmatched)} previous entr(y|ies) matched nothing and were "
                f"dropped (fixed, renamed, or a key-format change this tool could "
                f"not map unambiguously). Re-check each:"
            )
            for s in unmatched:
                print(f"  {s}")
        if blank:
            print(f"\n{len(blank)} entr(y|ies) need a written reason:")
            for s in blank:
                print(f"  {s}")
            return 1
        return 0

    try:
        existing = load_baseline(baseline_path)
    except BaselineError as exc:
        prefix = "::error::" if args.ci else ""
        for line in str(exc).splitlines():
            print(f"{prefix}{line}", file=sys.stderr)
        print(
            f"\n{prefix}FAILED: the baseline could not be read, so NOTHING is "
            f"exempt. This check fails closed on purpose.",
            file=sys.stderr,
        )
        return 1

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
        reason = str(entry.get("reason", "") or "")
        if not reason.strip():
            problems.append(
                f"baseline entry {site!r} has an empty 'reason' — every exemption "
                f"must say why an unbounded wait is acceptable there."
            )
        elif "reason_covers_sites" not in entry:
            problems.append(
                f"baseline entry {site!r} is missing 'reason_covers_sites'. Set it "
                f"to the number of waits the written reason actually accounts for."
            )
        elif int(entry["reason_covers_sites"]) != want:
            problems.append(
                f"baseline entry {site!r} allows {want} wait(s) but its reason was "
                f"written for {int(entry['reason_covers_sites'])}. Someone raised "
                f"'sites' without rewriting the reason. Write a reason that covers "
                f"all {want} waits, then set \"reason_covers_sites\": {want}."
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
                f"it through a bounded wrapper, or run --update-baseline (which "
                f"CLEARS the reason on a raised count) and write a fresh reason "
                f"covering all {have} waits."
            )
        elif have < want:
            problems.append(
                f"{site} now has only {have} untimed wait(s), baseline records "
                f"{want}. Tighten it: set \"sites\": {have} and "
                f"\"reason_covers_sites\": {have} in "
                f"{BASELINE_PATH.as_posix()} (the baseline is a ratchet)."
            )

    ok = not unexempted and not problems
    if ok:
        roots = ", ".join(
            d.relative_to(root).as_posix() for d in discover_scope(root).scanned
        )
        print(
            f"OK: every synchronous subprocess wait is either bounded or baselined "
            f"({len(findings)} baselined wait(s) across {len(grouped)} function(s)).\n"
            f"scanned: {roots}"
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
