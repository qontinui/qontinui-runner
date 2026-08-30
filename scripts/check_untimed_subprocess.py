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
closure of the dependencies of `src-tauri/Cargo.toml`, keeping only those that
resolve inside this repository. Each contributes `<crate>/src/**/*.rs`.

BOTH dependency spellings are followed:

  * `foo = { path = "…" }`        — relative to the declaring manifest;
  * `foo = { workspace = true }`  — resolved through the workspace root's
    `[workspace.dependencies]`, where the `path` actually lives (relative to the
    WORKSPACE ROOT). Following only the first meant that converting a path dep
    to the standard workspace-inheritance idiom — a routine, obviously-safe
    refactor — silently removed that crate from the scan and moved it into
    `--list-roots`' "NOT SCANNED" list, which was then an affirmatively false
    statement about a crate linked into the runner.

…and all three dependency TABLES (`dependencies`, `dev-dependencies`,
`build-dependencies`, plus their `[target.'cfg(..)'.…]` forms). That is
deliberately an OVER-approximation of "linked into the runner binary" — a
dev-dependency is linked into the test binary and a build-dependency into
`build.rs` — because over-approximating is the fail-CLOSED direction: scanning a
crate that is not in the shipping binary costs a few baseline entries, whereas
missing one reopens the defect class. (The `build.rs` FILE is still out of scope,
but that is about where the file sits — outside `<crate>/src` — not about which
table named the crate.)

A MANIFEST THAT CANNOT BE PARSED IS FATAL. It used to be swallowed into "this
crate has no path dependencies", so one malformed line appended to
`src-tauri/Cargo.toml` shrank the scan from five crate trees to `src-tauri/src`
alone and the checker still printed `OK` and exited 0. A discovery failure is
UNKNOWN, never empty. For the same reason there is no longer a regex fallback
for interpreters without `tomllib`: it could not see `workspace = true` or
nested target tables, so on an old interpreter it silently under-scanned. The
checker requires Python 3.11+ and says so instead of guessing.

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
`std|tokio::process::{Command, Child}`, this file's `use std::process;` /
`use std::process as p;` MODULE imports (so `process::Command::new(..)`
resolves), and a crate-wide map of TOP-LEVEL helper functions whose declared
return type is a `std::process::Command`. That helper map is consulted only when
the receiver is an actual CALL, never for a bare identifier that merely shares a
name with a helper (`let build = …; build.output()`), and this file's own
helpers win over the crate-wide map.

The helper map is consulted at THREE positions, not one:

  * the chained spelling                `git_cmd(p).output()`
  * a `let` BINDING of a helper's result `let mut c = pm_command("cargo");
                                          c.arg("-V"); c.output();`
  * the chain's LAST LINK               `self.build_command().output()`

Only the first was covered originally. The second is the one that matters most:
it is the only way to write the call when you need to add arguments afterwards,
six in-tree call sites already have that shape, and it made a `Command` built by
a helper completely invisible to the gate. When two top-level helpers in different files
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
    `use std::process::Command as C;`,
    `use std::process::{Command as C, Stdio};` and `use std::process;`, plus
    `type Cmd = …;` aliases of any of those. Zero files in this tree use the
    nested-group spelling.
  * A receiver whose kind is only knowable from a struct FIELD's declared type
    (`self.cmd.output()`). The chain's last METHOD call is resolved now
    (`self.build_command().output()`); a bare field access is not.
Each of those resolves UNKNOWN and is therefore NOT flagged — the same
fail-quiet posture as every other unresolvable receiver.

Two further limits, which are about what the gate says rather than what it
finds:

  * A `#[cfg]`-GATED KILL still suppresses the `wait()` that follows it. The
    real in-tree shape is a `#[cfg(not(windows))] { child.kill(); }` block
    immediately above `child.wait()`, and every such arm in this tree has a
    sibling arm that also kills. But `#[cfg]` is not a proof: on a target where
    the cfg is false, no kill precedes that wait. A RUNTIME conditional
    (`if cond { child.kill(); }`) does NOT suppress — that hole is closed.
  * A wait whose PROGRAM the checker cannot name records `?`, and a swap
    between two `?` waits inside one baselined function is not detected. `?`
    is what a `Command` handed in as a function PARAMETER produces, since the
    caller chose the program. One of the 54 baselined waits is `?` today; every
    other one carries a real program name, a `dyn:<identifier>`, or a
    `fn:<helper>`.

Deliberately conservative: unresolvable receivers are NOT flagged. A gate that
cries wolf gets disabled, and that reopens the class this exists to close.

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
  * A `Child::wait()` / `wait_with_output()` on a receiver that an UNCONDITIONAL
    `.kill()` on the SAME receiver precedes, in the same block, within 3
    statements — reaping a child you just killed is bounded by construction.
    All three conditions are required: `a.kill(); … b.wait()` is still flagged;
    so is a `kill()` separated from the `wait()` by a block that OPENS after it;
    and so, now, is a kill that only runs on some paths —

        if cond { let _ = child.kill(); }
        let _ = child.wait();

    which is spawn -> MAYBE-kill -> unbounded wait, i.e. exactly the periodic
    hang shape, and which used to read as "reaping the child you just killed"
    because the span test rejected a `{` but tolerated a `}`. Every block the
    span CLOSES is now classified: `if` / `else` / `while` / `for` / `loop` /
    match arm / closure body suppresses nothing; a bare, `unsafe` or
    `#[cfg(..)]`-attributed block still does.

NOTHING ELSE IS SKIPPED, and there is deliberately no mechanism for it.
`src-tauri/src/process_helpers.rs` used to be exempt wholesale because it
IMPLEMENTS the bounded primitives and therefore necessarily contains raw
`.output()` / `Child::wait()` calls. But that made the one file whose name reads
as "sanctioned" a blind spot: adding

    pub fn output_now(mut cmd: Command) -> io::Result<Output> { cmd.output() }

beside the three real wrappers passed the gate, and so did every periodic caller
routed through it. The file is scanned like any other now; the wrappers' own raw
waits are baselined like any other exemption, so a fourth "wrapper" appearing
there is a brand-new baseline key with an empty reason -> red.

THE BASELINE
------------
The 2026-08-30 sweep fixed the periodic/hot-path sites. The residue is real and
mostly legitimate: one-shot CLI paths, user-triggered actions, startup and
shutdown work. Rather than pretend it does not exist, every surviving site is
enumerated in `scripts/untimed-subprocess-baseline.json`, keyed by
`<path>::<qualified fn>` with a REQUIRED written reason.

The function is QUALIFIED by everything that encloses it — `mod`, `impl`,
`trait`, and any outer `fn` — so `impl A { fn run }` keys as `…rs::A::run` and
`impl B { fn run }` as `…rs::B::run`. GENERIC ARGUMENTS are part of the label,
so `impl Bar<u8>` and `impl Bar<u16>` are distinct too (truncating at the first
`<` reinstated the same-name hazard for exactly that pair). A trait impl keys as
`<Type as Trait>`. Without qualification, removing a wait from `B::run` while
adding one to `A::run` left the count constant and the gate green. The key
survives edits above and below the site, which a line number does not.

Each entry records WHAT it covers, not how many: `waits` is one normalized
PROGRAM token per untimed wait in that function, and `reason_covers_waits` is
the list the prose was authored against.

  * a string-literal program -> its lowercased basename without `.exe`, so
    `"C:/Windows/System32/taskkill.exe"` and `"taskkill"` are one token;
  * a computed program       -> `dyn:<last identifier>` (`dyn:python_path`);
  * a Command built by a helper -> `fn:<helper>`; handed in by a caller -> `?`.

WHY A PROGRAM AND NOT A COUNT. A count cannot see a count-PRESERVING SWAP.
Replacing the baselined `osascript` one-shot in `window_manager::list_windows_macos`
with `Command::new("aws").args(["s3","ls",…]).output()` keeps `sites` at 1, so
the ratchet stayed green while an unbounded NETWORK call shipped under prose
that read "User-triggered window enumeration via `osascript`, one pass per
click".

WHY A PROGRAM AND NOT A HASH OF THE CALL TEXT. Churn. A program token is
invariant under reformatting, argument edits, chain reordering and renames
elsewhere in the function — the overwhelming majority of legitimate edits — and
moves exactly when the thing being waited on changes. A text hash would go red
on `git status` -> `git status --porcelain`, an edit that cannot invalidate any
reason, and a gate that cries wolf gets turned off. The one churn cost accepted
is that renaming the variable behind a `dyn:` token re-clears that entry's
reason; that is rare, visible in the same diff, and re-reading the exemption
then is arguably correct.

The baseline is a RATCHET:
  * a site in a function with no baseline entry            -> FAIL
  * a wait ADDED to a function's list                      -> FAIL
  * a wait REMOVED from it                                 -> FAIL, "tighten it"
  * a wait REPLACED by a different program                 -> FAIL, "swap"
  * an entry with an empty `reason`                        -> FAIL
  * an entry whose `reason_covers_waits` != `waits`        -> FAIL
  * two entries with the same `site`                       -> FAIL
  * a `waits` field that is not a list of strings          -> FAIL, with a
                                                              diagnostic
  * a baseline that is not `"format": 3`                   -> FAIL, with a
                                                              migration message
so it can only ever shrink.

WHAT `--update-baseline` CAN AND CANNOT SMUGGLE IN
--------------------------------------------------
`--update-baseline` carries a reason forward ONLY when the new wait list is a
sub-multiset of the one that reason was authored against — i.e. waits were
removed and nothing was added or swapped. So:

  * a brand-new `path::fn` key            -> empty reason -> rejected
  * a NEW wait inside an already-baselined function
                                          -> reason cleared -> rejected
  * a wait SWAPPED for a different program
                                          -> reason cleared -> rejected
  * hand-editing `waits` without touching the reason
                                          -> `reason_covers_waits` mismatch -> rejected
  * a pure REMOVAL                        -> reason kept, `reason_covers_waits`
                                             shortened. A strict improvement
                                             does not need fresh prose.

It also refuses to read a baseline whose `format` is NEWER than the one it
writes. Accepting any format whatever meant a future format-4 file — written by
a checker enforcing rules this one does not implement — was silently rewritten
back down, dropping every field format 4 added.

What it does NOT detect: a human who hand-edits BOTH `waits` and
`reason_covers_waits` and leaves stale prose behind, or who swaps one `?` wait
for another `?` wait. The first is a false statement standing in a reviewable
diff, not a silent bypass — no static check can tell apposite prose from
inapposite prose. The second is named in "WHAT IT DOES NOT SEE" above. The
property the gate actually enforces is "no wait may be added, or replaced by a
different program, without a human editing the reason field in the same diff",
not "the reason is true".

USAGE
-----
    python3 scripts/check_untimed_subprocess.py              # check (exit 1 on a finding)
    python3 scripts/check_untimed_subprocess.py --list       # print every SYNC wait found
    python3 scripts/check_untimed_subprocess.py --list-roots # print the discovered scan scope
    python3 scripts/check_untimed_subprocess.py --update-baseline
    python3 scripts/check_untimed_subprocess.py --root <dir> # check a different tree

Pure stdlib, no third-party imports: it runs identically on the Windows dev box
and on the ubuntu CI runner. Manifests are read with `tomllib` (stdlib since
3.11), which makes 3.11 the MINIMUM — the narrow regex reader that used to cover
3.8-3.10 could not see `workspace = true` inheritance or nested target tables,
so it silently under-scanned, and an under-scan here prints OK. Refusing to run
beats reporting a scope the checker cannot compute.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

try:  # stdlib since 3.11. There is deliberately no fallback — see _read_manifest.
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
BASELINE_FORMAT = 3

#: NOTHING is exempt from scanning, and there is deliberately no mechanism for
#: it. `src-tauri/src/process_helpers.rs` used to be skipped wholesale — it
#: *implements* the bounded primitives, so it necessarily contains raw
#: `.output()` / `Child::wait()` calls. But a whole-file skip turned the one
#: file whose NAME reads as "sanctioned" into a blind spot: dropping
#: `pub fn output_now(mut cmd: Command) -> io::Result<Output> { cmd.output() }`
#: in beside the three real wrappers passed the gate, and so did every periodic
#: caller routed through it. The wrappers' own raw waits are BASELINED like any
#: other exemption instead, so a fourth "wrapper" appearing in that file is a
#: brand-new baseline key with an empty reason -> red.

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


def _split_top_level_commas(s: str) -> list[str]:
    """Split on `,` at bracket depth 0."""
    out: list[str] = []
    depth = 0
    cur: list[str] = []
    for c in s:
        if c in "<([":
            depth += 1
        elif c in ">)]":
            depth -= 1
        elif c == "," and depth == 0:
            out.append("".join(cur))
            cur = []
            continue
        cur.append(c)
    out.append("".join(cur))
    return [p for p in (x.strip() for x in out) if p]


def _base_label(expr: str) -> str:
    s = expr.replace("&", " ").replace("'", " ").strip()
    s = s.split("::")[-1]
    return re.sub(r"[^A-Za-z0-9_]", "", s)


def _type_label(expr: str) -> str:
    """Reduce a type expression to an identifier usable in a key.

    GENERIC ARGUMENTS ARE KEPT (lifetimes are not). They used to be truncated
    at the first `<`, which collapsed `impl Bar<u8> { fn run }` and
    `impl Bar<u16> { fn run }` onto ONE key — reinstating, for that pair,
    exactly the same-name hazard the qualified key exists to close. `Bar<u8>`
    and `Bar<u16>` are different impls and get different keys.
    """
    s = re.sub(r"\bdyn\b|\bmut\b|\bimpl\b", " ", expr.strip())
    head, sep, rest = s.partition("<")
    label = _base_label(head)
    if sep:
        inner = rest.rsplit(">", 1)[0]
        args = [
            _type_label(a)
            for a in _split_top_level_commas(inner)
            if not a.lstrip().startswith("'")
        ]
        args = [a for a in args if a]
        if args:
            label = f"{label or 'impl'}<{','.join(args)}>"
    return label or "impl"


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


def receiver_root(src: str, dot_idx: int) -> tuple[str, int, int]:
    """Walk back from the `.` of a method call to the root of its chain.

    Returns ``(root, start_offset, end_offset)`` where ``root`` is a (possibly
    `::`-qualified) path string — "" when the chain does not root in something
    nameable — ``start_offset`` is the index of the root's FIRST character (so
    the caller can slice the whole receiver expression, which is where the
    program name lives), and ``end_offset`` is the index just past the root's
    last identifier character, so the caller can tell a CALL (`build(..)`) from
    a bare binding (`build`).
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
                first = j + 1
                while k >= 1 and src[k] == ":" and src[k - 1] == ":":
                    p = _skip_ws_back(src, k - 2)
                    q = p
                    while q >= 0 and (src[q].isalnum() or src[q] == "_"):
                        q -= 1
                    if q == p:
                        break
                    path.insert(0, src[q + 1 : p + 1])
                    first = q + 1
                    k = _skip_ws_back(src, q)
                return "::".join(path), first, root_end
            return ident, j + 1, root_end
        return "", -1, -1
    return "", -1, -1


def receiver_link(src: str, dot_idx: int) -> tuple[str, bool]:
    """The identifier of the link IMMEDIATELY before the `.` at ``dot_idx``.

    `self.build_command().output()` roots in `self`, which the checker cannot
    type — but `build_command` is a `-> std::process::Command` method, and the
    crate-wide helper map already knows that. This gives the scanner the last
    link so it can consult that map when the ROOT resolves UNKNOWN.

    Returns ``(name, is_call)``; ``is_call`` distinguishes `x.build()` from the
    field access `x.build`.
    """
    i = _skip_ws_back(src, dot_idx - 1)
    if i < 0:
        return "", False
    if src[i] == "?":
        i = _skip_ws_back(src, i - 1)
    is_call = False
    if i >= 0 and src[i] == ")":
        is_call = True
        i = _skip_ws_back(src, _match_close_back(src, i))
    if i < 0 or not (src[i].isalnum() or src[i] == "_"):
        return "", False
    j = i
    while j >= 0 and (src[j].isalnum() or src[j] == "_"):
        j -= 1
    return src[j + 1 : i + 1], is_call


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


_USE_MODULE_RE = re.compile(
    r"\buse\s+(std|tokio)\s*::\s*process\s*"
    r"(?:as\s+([A-Za-z_][A-Za-z0-9_]*)\s*)?;"
)


def process_module_aliases(src: str) -> dict[str, str]:
    """Local names for the `std::process` / `tokio::process` MODULE itself.

    `use std::process;` followed by `process::Command::new("git").output()`
    resolved UNKNOWN before: the receiver root is `process::Command::new`,
    whose penultimate segment is `Command`, and `Command` is not in the TYPE
    alias map because the file never imported the type. The module import is
    the missing half. `use std::process as p;` is covered too.

    Same fail-closed rule as the type map: a name bound to both kinds is SYNC.
    """
    out: dict[str, str] = {}
    for m in _USE_MODULE_RE.finditer(src):
        kind = SYNC if m.group(1) == "std" else ASYNC
        name = m.group(2) or "process"
        prev = out.get(name)
        out[name] = SYNC if (prev is not None and prev != kind) else kind
    return out


# --- WHICH program each wait is on -----------------------------------------
#
# The count ratchet ("no function may gain a wait") cannot see a count-PRESERVING
# SWAP: replace a baselined `osascript` one-shot with
# `Command::new("aws").args(["s3","ls",…]).output()` and the count is still 1, so
# the gate stayed green while an unbounded NETWORK call shipped under prose
# written for a local one-shot. Each exemption therefore records WHAT it covers,
# not just how many.
#
# The token is the PROGRAM, not a hash of the call text. That choice is about
# churn: a program token is invariant under reformatting, argument edits, chain
# reordering and renames elsewhere in the function — the overwhelming majority of
# legitimate edits — and moves exactly when the thing being waited on changes,
# which is exactly when "why this wait is bounded in practice" stops being true.
# A text hash would go red on `git status` -> `git status --porcelain`, an edit
# that cannot invalidate any reason, and a gate that cries wolf gets disabled.

#: The token for a wait whose program the checker could not resolve at all.
UNKNOWN_PROGRAM = "?"

_STR_LIT_RE = re.compile(r'^b?"((?:\\.|[^"\\])*)"$', re.S)
_RAW_STR_RE = re.compile(r'^b?r(#*)"(.*)"\1$', re.S)


def _first_arg_span(src: str, open_paren: int) -> tuple[int, int] | None:
    """`(start, end)` of the first argument of the call whose `(` is at ``open_paren``."""
    i = open_paren + 1
    n = len(src)
    depth = 0
    start = i
    while i < n:
        c = src[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if c == ")" and depth == 0:
                # The span is returned even when it looks empty here: this
                # operates on the NOISE-STRIPPED copy, where a string literal is
                # a run of spaces. Emptiness is decided by the caller against
                # the raw text.
                return (start, i)
            depth -= 1
        elif c == "," and depth == 0:
            return (start, i)
        i += 1
    return None


def _normalize_program(raw_arg: str, src_arg: str) -> str:
    """One stable token naming WHAT is waited on.

    A string-literal program becomes its lowercased basename with any `.exe`
    stripped, so `"C:/Windows/System32/taskkill.exe"` and `"taskkill"` are the
    same token. A COMPUTED program becomes `dyn:<last identifier>` — stable
    under reformatting and argument edits, and it still moves when the variable
    holding the program is swapped for a different one. Nothing nameable is `?`.

    ``raw_arg`` is the ORIGINAL source text (string literals are blanked in the
    noise-stripped copy, so the literal has to be read from the raw one);
    ``src_arg`` is the noise-stripped text at the same offsets, used for the
    identifier scan so no string CONTENT can leak into a `dyn:` token.
    """
    a = raw_arg.strip()
    lit: str | None = None
    m = _RAW_STR_RE.match(a)
    if m:
        lit = m.group(2)
    else:
        m = _STR_LIT_RE.match(a)
        if m:
            lit = m.group(1)
    if lit is not None:
        base = lit.replace("\\\\", "/").replace("\\", "/").rsplit("/", 1)[-1]
        base = base.strip().strip('"').lower()
        if base.endswith(".exe"):
            base = base[:-4]
        return base or UNKNOWN_PROGRAM
    idents = [
        i
        for i in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", src_arg)
        if i not in ("mut", "ref", "as", "String", "OsStr", "OsString", "Path", "PathBuf")
    ]
    return f"dyn:{idents[-1]}" if idents else UNKNOWN_PROGRAM


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


def classify_origin(
    expr: str,
    aliases: dict[str, str],
    default: str,
    mods: dict[str, str] | None = None,
) -> str:
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
    for name, kind in (mods or {}).items():
        if re.search(
            r"(?<![:\w])" + re.escape(name) + r"\s*::\s*Command\s*::\s*new\b", expr
        ):
            return kind
    if re.search(r"(?<![:\w])Command\s*::\s*new\b", expr):
        return default
    return UNKNOWN


def program_of_expr(
    src_expr: str,
    raw_expr: str,
    aliases: dict[str, str],
    default: str,
    mods: dict[str, str],
) -> str:
    """The program token for the FIRST Command constructor inside an expression.

    `src_expr` and `raw_expr` must be the SAME span of the same file — the
    noise-stripped copy and the original — because a string-literal program is
    only readable in the original while the identifier scan must only ever see
    the stripped one.

    Returns "" when the expression builds no recognisable Command, which lets
    the caller fall back to inheriting from a binding.

    This deliberately does NOT re-decide sync-vs-async — the caller has already
    established that — so it accepts any `::`-qualified path in front of a known
    constructor rather than insisting on the exact `std::process::` spelling.
    """
    del default, mods  # kind is the caller's decision; this only names the program
    # Endings, longest first. A `::`-qualified PREFIX of any length is allowed in
    # front of every one of them: the in-tree spelling is
    # `crate::process_helpers::no_window("node")`, and requiring a bare name made
    # the token `?` for most of the tree.
    endings = [
        "tokio_cmd_no_window",
        "tokio_no_window",
        "cmd_no_window",
        "no_window",
    ]
    endings += [re.escape(n) + r"\s*::\s*new" for n in sorted(aliases, key=len, reverse=True)]
    endings.append(r"Command\s*::\s*new")
    pat = (
        r"(?<![:\w])(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*(?:"
        + "|".join(f"(?:{e})" for e in endings)
        + r")\s*\("
    )
    m = re.search(pat, src_expr)
    if m is None:
        return ""
    if re.search(r"(?:tokio_)?cmd_no_window\s*\($", m.group(0)):
        return "cmd"  # `cmd_no_window()` IS `no_window("cmd.exe")`
    span = _first_arg_span(src_expr, m.end() - 1)
    if span is None:
        return UNKNOWN_PROGRAM
    a, b = span
    if not raw_expr[a:b].strip():
        return UNKNOWN_PROGRAM  # a constructor called with no arguments
    return _normalize_program(raw_expr[a:b], src_expr[a:b])


def file_command_default(aliases: dict[str, str]) -> str:
    """What a bare `Command::new(..)` means in this file, from its imports."""
    return aliases.get("Command", UNKNOWN)


_LET_RE = re.compile(
    r"\blet\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*([^=;]+?))?\s*=\s*",
)


@dataclass(frozen=True)
class Bound:
    """What a local name holds: which runtime, and which program."""

    kind: str
    program: str  # "" = undetermined; resolved to `?` at the point of use


def local_bindings(
    body: str,
    raw_body: str,
    aliases: dict[str, str],
    default: str,
    helpers: dict[str, str],
    helper_kinds: dict[str, str],
    mods: dict[str, str],
) -> dict[str, Bound]:
    """Map local variable name -> `Bound` for `Command` and `Child` values.

    A `Child` inherits its parent `Command`'s kind AND program, so
    `let mut c = no_window("x").spawn()?;` makes `c.wait()` a SYNC blocking
    wait on `x`.

    CRITICAL: the initialiser is resolved against the HELPER MAPS too. Without
    that, `let mut cmd = pm_command("cargo"); cmd.arg("-V"); cmd.output();` was
    invisible — the `pm_command(..).output()` chained spelling resolved, but the
    bound spelling did not, and the bound spelling is the only way to write it
    when you need to add arguments afterwards. Six in-tree call sites already
    have that shape, so it is the shape the next regression takes.
    """
    kinds: dict[str, Bound] = {}
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
        raw_rhs = raw_body[m.end() : i]
        program = ""
        kind = classify_type(ann, aliases) if ann else UNKNOWN
        if kind == UNKNOWN:
            kind = classify_origin(ann, aliases, default, mods)
        if kind == UNKNOWN:
            kind = classify_origin(rhs, aliases, default, mods)
        if kind == UNKNOWN:
            # A crate helper that RETURNS a Command: `let mut c = git_cmd(p);`.
            # Same discipline as the chained spelling — only an actual CALL
            # counts, never a bare identifier that shares a helper's name.
            call = re.match(r"\s*([A-Za-z_][A-Za-z0-9_:]*)\s*(?:::\s*<[^;{}]*?>\s*)?\(", rhs)
            if call:
                fname = call.group(1).rsplit("::", 1)[-1]
                if fname in helpers:
                    kind, program = helpers[fname], f"fn:{fname}"
                elif fname in helper_kinds:
                    kind, program = helper_kinds[fname], f"fn:{fname}"
        if kind == UNKNOWN:
            # `let mut c = cmd;` / `let c = cmd.spawn()?;` — inherit.
            root = re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*)\b", rhs)
            if root and root.group(1) in kinds:
                kind = kinds[root.group(1)].kind
                program = kinds[root.group(1)].program
        if kind != UNKNOWN:
            if not program:
                program = program_of_expr(rhs, raw_rhs, aliases, default, mods)
            if not program:
                root = re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*)\b", rhs)
                if root and root.group(1) in kinds:
                    program = kinds[root.group(1)].program
            kinds[name] = Bound(kind, program)
    return kinds


_PARAM_RE = re.compile(
    r"\b(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(&?\s*(?:mut\s+)?[A-Za-z_][A-Za-z0-9_:]*)"
)


def param_bindings(
    src: str, fn: Function, aliases: dict[str, str], default: str
) -> dict[str, Bound]:
    """Map parameter name -> `Bound` from the signature's declared types.

    A parameter carries no program: the caller chose it. Such a site records
    `?`, and a swap inside one is therefore NOT caught — stated in the
    docstring's "WHAT IT DOES NOT SEE" list rather than papered over.
    """
    # The signature is the text between the preceding `fn` and the body brace.
    head_start = src.rfind("fn ", 0, fn.start)
    if head_start == -1:
        return {}
    head = src[head_start : fn.start]
    kinds: dict[str, Bound] = {}
    for m in _PARAM_RE.finditer(head):
        ty = m.group(2)
        kind = classify_type(ty, aliases)
        if kind == UNKNOWN and re.search(r"(?<![:\w])(?:Command|Child)\b", ty):
            kind = default
        if kind != UNKNOWN:
            kinds[m.group(1)] = Bound(kind, UNKNOWN_PROGRAM)
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
    mods: dict[str, str]  # local names for the `std|tokio::process` MODULE
    default: str
    fns: list[Function]
    helpers: dict[str, str]  # THIS file's `fn … -> Command` helpers
    top_helpers: dict[str, str]  # …restricted to non-nested ones


def parse_file(rel: str, raw: str) -> FileInfo:
    src = strip_test_items(strip_noise(raw))
    aliases = command_aliases(src)
    mods = process_module_aliases(src)
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
    return FileInfo(rel, raw, src, aliases, mods, default, fns, helpers, top_helpers)


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
    program: str  # normalized name of WHAT is waited on — see UNKNOWN_PROGRAM
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


_COND_HEAD_RE = re.compile(
    r"(?:^|[\s;(){}\[\],])(?:if|else|while|for|loop|match)\b[^{}]*$"
)


def _block_is_conditional(body: str, open_idx: int) -> bool:
    """True when the block opened at ``open_idx`` runs only on some paths.

    `if` / `else` / `while` / `for` / `loop` / `match`-arm / closure bodies are
    conditional. A BARE block, an `unsafe` block, and a `#[cfg(..)]`-attributed
    block are not — for a given compilation the cfg arm is either wholly present
    or wholly absent, which is a different thing from a runtime branch.
    """
    p = body[max(0, open_idx - 300) : open_idx].rstrip()
    if p.endswith("=>"):  # a `match` arm
        return True
    if p.endswith("|"):  # a closure body
        return True
    return bool(_COND_HEAD_RE.search(p))


def _killed_before(body: str, offset: int, recv: str) -> bool:
    """True when ``recv`` was UNCONDITIONALLY `.kill()`-ed just before the wait.

    Reaping a child you have just killed is bounded by construction — that is
    exactly what `run_with_timeout`'s own expiry path does — so a `wait()` that
    follows a `kill()` is not the defect.

    THREE conditions are required:

      * SAME RECEIVER. `a.kill(); … b.wait()` used to suppress the gate on
        `b.wait()` because *some* `.kill(` appeared earlier in the body.
      * ADJACENCY. At most ``KILL_ADJACENCY_STATEMENTS`` statements may
        separate the two, and the span may not OPEN a block — a `wait()` nested
        inside a block that starts after the `kill()` is on a different path.
      * THE KILL MUST NOT BE CONDITIONAL. The span may CLOSE blocks, because the
        real in-tree shape in `ai_provider/pi_cli.rs` is a `#[cfg]`-gated
        `{ child.kill(); }` immediately above a `child.wait()`. Tolerating a
        closing `}` unconditionally, though, also swallowed

            if cond { let _ = child.kill(); }
            let _ = child.wait();

        which is spawn -> MAYBE-kill -> unbounded wait: precisely the periodic
        hang shape, reading as "reaping the child you just killed". Every block
        the span closes is now classified, and a runtime-conditional one
        (`if` / `else` / `while` / `for` / `loop` / match arm / closure) does not
        suppress anything.

    The residual hole is deliberate and named in the module docstring: a
    `#[cfg]`-gated kill is accepted, so on a target where that cfg is false no
    kill precedes the wait. In tree, every such arm has a sibling arm that kills.
    """
    if not recv:
        return False
    kills = list(_KILL_RE.finditer(body, 0, offset))
    if not kills:
        return False

    # One pass: the stack of still-open `{` at each kill, and at the wait.
    want = sorted({m.start() for m in kills} | {offset})
    stacks: dict[int, tuple[int, ...]] = {}
    st: list[int] = []
    wi = 0
    for i in range(offset + 1):
        while wi < len(want) and want[wi] == i:
            stacks[i] = tuple(st)
            wi += 1
        c = body[i]
        if c == "{":
            st.append(i)
        elif c == "}" and st:
            st.pop()
    wait_stack = stacks[offset]

    for m in kills:
        kill_recv, _, _ = receiver_root(body, m.start())
        if kill_recv != recv:
            continue
        span = body[m.end() : offset]
        if "{" in span:
            continue
        if span.count(";") > KILL_ADJACENCY_STATEMENTS:
            continue
        kill_stack = stacks[m.start()]
        if kill_stack[: len(wait_stack)] != wait_stack:
            continue  # not a plain close of the blocks enclosing the wait
        if any(_block_is_conditional(body, o) for o in kill_stack[len(wait_stack) :]):
            continue
        return True
    return False


def scan_file(fi: FileInfo, helper_kinds: dict[str, str]) -> list[Finding]:
    src = fi.src
    if "output" not in src and "status" not in src and "wait" not in src:
        return []
    default = fi.default
    aliases = fi.aliases
    mods = fi.mods
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

    cache: dict[int, dict[str, Bound]] = {}
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
            binds.update(
                local_bindings(
                    fn.body,
                    raw[fn.start : fn.end],
                    aliases,
                    default,
                    fi.helpers,
                    helper_kinds,
                    mods,
                )
            )
            cache[fn.start] = binds
        binds = cache[fn.start]

        root, root_start, root_end = receiver_root(src, m.start())
        if not root:
            continue
        is_call = _root_is_call(src, root_end)
        parts = root.split("::")
        tail = parts[-1]
        owner = parts[-2] if len(parts) >= 2 else ""
        modseg = parts[-3] if len(parts) >= 3 else ""

        kind = UNKNOWN
        program = ""
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
        elif tail == "new" and owner == "Command" and modseg in mods:
            # `use std::process;` + `process::Command::new("git").output()`.
            kind = mods[modseg]
        elif tail == "new" and owner == "Command":
            kind = default
        elif root in binds:
            kind = binds[root].kind
            program = binds[root].program
        elif is_call and tail in fi.helpers:
            kind = fi.helpers[tail]
            program = f"fn:{tail}"
        elif is_call and tail in helper_kinds:
            kind = helper_kinds[tail]
            program = f"fn:{tail}"

        if kind == UNKNOWN:
            # The LAST LINK of the chain rather than its root:
            # `self.build_command().output()` roots in `self`, which has no
            # local type, but `build_command` is a known `-> Command` helper.
            link, link_is_call = receiver_link(src, m.start())
            if link_is_call and link not in BLOCKING_WAITS:
                if link in fi.helpers:
                    kind, program = fi.helpers[link], f"fn:{link}"
                elif link in helper_kinds:
                    kind, program = helper_kinds[link], f"fn:{link}"

        if kind != SYNC:
            continue
        if method in ("wait", "wait_with_output") and _killed_before(
            fn.body, m.start() - fn.start, root
        ):
            continue

        if not program:
            program = program_of_expr(
                src[root_start : m.start()],
                raw[root_start : m.start()],
                aliases,
                default,
                mods,
            )
        if not program:
            program = UNKNOWN_PROGRAM

        ln = line_of(m.start())
        snippet = raw_lines[ln - 1].strip() if ln - 1 < len(raw_lines) else ""
        findings.append(Finding(fi.rel, ln, fn.name, method, program, snippet[:160]))
    return findings


# ---------------------------------------------------------------------------
# Scan scope discovery
# ---------------------------------------------------------------------------


def _read_manifest(path: Path) -> dict:
    """`{"members", "path_deps", "workspace_deps", "ws_dep_paths"}` from a Cargo.toml.

    EVERY FAILURE HERE IS FATAL. This used to swallow any parse error into
    `data = {}` — i.e. "this crate has no path dependencies" — so ONE malformed
    line appended to `src-tauri/Cargo.toml` shrank the scan from five crate
    trees to `src-tauri/src` alone and the checker still printed `OK` and exited
    0. A discovery failure is UNKNOWN, never empty; that is the same
    silent-empty-is-unknown error this gate exists to prevent, so it now stops
    the run with the manifest path and the parser's own message.

    * ``path_deps``      — `foo = { path = "…" }`, relative to THIS manifest.
    * ``workspace_deps`` — names declared `foo = { workspace = true }`, which
      resolve through the workspace root's `[workspace.dependencies]`.
    * ``ws_dep_paths``   — this manifest's own `[workspace.dependencies]` entries
      that carry a `path`, relative to the WORKSPACE ROOT.
    """
    if tomllib is None:  # pragma: no cover - interpreter-version dependent
        raise SystemExit(
            "error: this checker requires Python 3.11 or newer (stdlib "
            "`tomllib`) to read Cargo manifests.\n"
            "The regex reader it used to fall back to could not see "
            "`workspace = true` dependencies or nested target tables, so on an "
            "older interpreter it silently UNDER-SCANNED — and an under-scan "
            "here prints OK, which reads as 'clean'. Refusing to run rather "
            "than reporting a scope it cannot compute.\n"
            f"CI pins 3.12; this interpreter is {sys.version.split()[0]}."
        )
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        raise SystemExit(f"error: cannot read {path.as_posix()}: {exc}") from exc
    try:
        data = tomllib.loads(text)
    except Exception as exc:
        raise SystemExit(
            f"error: {path.as_posix()} is not parseable TOML: {exc}\n"
            f"The scan scope is DISCOVERED from this manifest, so a manifest the "
            f"checker cannot read means the scanned crate set is UNKNOWN. That is "
            f"a hard failure, not an empty dependency list — treating it as empty "
            f"is how a one-line typo silently ungated four of five crate trees."
        ) from exc

    workspace = data.get("workspace")
    workspace = workspace if isinstance(workspace, dict) else {}
    members = list(workspace.get("members", []) or [])
    path_deps: list[str] = []
    workspace_deps: list[str] = []

    def harvest(table: object) -> None:
        if not isinstance(table, dict):
            return
        for name, value in table.items():
            if not isinstance(value, dict):
                continue
            if isinstance(value.get("path"), str):
                path_deps.append(value["path"])
            elif value.get("workspace") is True:
                workspace_deps.append(name)

    # All THREE dependency kinds, deliberately: this is an over-approximation of
    # "linked into the runner binary" (a dev-dependency is linked into the test
    # binary, a build-dependency into `build.rs`), and over-approximating is the
    # fail-CLOSED direction. Scanning a crate that is not in the shipping binary
    # costs a few baseline entries; missing one reopens the defect class. The
    # `build.rs` FILE is still out of scope — that is about where the file sits
    # (outside `<crate>/src`), not about which dependency table named the crate.
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        harvest(data.get(key))
    for tgt in (data.get("target") or {}).values():
        if isinstance(tgt, dict):
            for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                harvest(tgt.get(key))

    ws_dep_paths: dict[str, str] = {}
    for name, value in (workspace.get("dependencies") or {}).items():
        if isinstance(value, dict) and isinstance(value.get("path"), str):
            ws_dep_paths[name] = value["path"]

    return {
        "members": members,
        "path_deps": path_deps,
        "workspace_deps": workspace_deps,
        "ws_dep_paths": ws_dep_paths,
    }


@dataclass(frozen=True)
class Scope:
    """What the scan covers, and what it deliberately does not."""

    scanned: list[Path]  # `<crate>/src` dirs, absolute
    external: list[str]  # path deps that resolve outside this repo
    #: In-repo workspace members that the dependency closure from
    #: `src-tauri/Cargo.toml` does not REACH. Stated as what was computed, not
    #: as a claim about how they run: "unreached" is a fact about this scan,
    #: whereas the old name `unlinked` asserted they are not in the runner
    #: binary — an assertion that went false the moment a dependency spelling
    #: the closure could not follow appeared.
    unreached: list[str]


def discover_scope(root: Path) -> Scope:
    """Every in-repo crate `src/` dir LINKED INTO THE RUNNER BINARY.

    Discovery, not a hard-coded list: plan
    `2026-08-21-runner-extract-crates-frontier-first` is moving modules out of
    `src-tauri/src` into sibling crates, and a hard-coded root would drop each
    extracted module out of coverage silently. An extraction phase necessarily
    adds a dependency to `src-tauri/Cargo.toml`, so the transitive closure of
    those picks the new crate up with no edit here.

    BOTH dependency spellings are followed. `foo = { path = "…" }` is the local
    one; `foo = { workspace = true }` resolves through the workspace root's
    `[workspace.dependencies]`, where the `path` actually lives (relative to the
    WORKSPACE ROOT, not to the inheriting member). Following only the first used
    to mean that converting a path dep to the standard workspace-inheritance
    idiom — a routine, obviously-safe refactor — silently removed that crate
    from the scan AND moved it into the `--list-roots` "NOT SCANNED … they ship
    as their own processes" list, which was then an affirmatively false
    statement about a crate linked into the runner.
    """
    root = root.resolve()

    ws_manifest = root / WORKSPACE_MANIFEST
    ws_dep_paths: dict[str, str] = {}
    ws_present = ws_manifest.is_file()
    if ws_present:
        ws_dep_paths = _read_manifest(ws_manifest)["ws_dep_paths"]

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
        info = _read_manifest(manifest)

        # (base directory the path is relative to, path)
        deps: list[tuple[Path, str]] = [(resolved, d) for d in info["path_deps"]]
        for name in info["workspace_deps"]:
            if not ws_present:
                raise SystemExit(
                    f"error: {manifest.as_posix()} declares `{name} = {{ workspace = "
                    f"true }}` but {ws_manifest.as_posix()} does not exist, so the "
                    f"dependency cannot be resolved and the scan scope is UNKNOWN."
                )
            # A name absent from `ws_dep_paths` is a REGISTRY dependency
            # inherited from the workspace (no `path`), correctly out of scope.
            if name in ws_dep_paths:
                deps.append((root, ws_dep_paths[name]))

        for base, dep in deps:
            target = (base / dep).resolve()
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

    unreached: list[str] = []
    if ws_present:
        members: list[Path] = []
        for member in _read_manifest(ws_manifest)["members"]:
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
                unreached.append(r.relative_to(root).as_posix())
    return Scope(dirs, sorted(external), sorted(set(unreached)))


def collect_sources(root: Path) -> dict[str, str]:
    sources: dict[str, str] = {}
    for base in discover_scope(root).scanned:
        for p in sorted(base.rglob("*.rs")):
            rel = p.relative_to(root).as_posix()
            if rel in sources:
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


def _as_wait_list(path: Path, site: str, field: str, value: object) -> list[str]:
    """Validate one `waits`-shaped field, with a DIAGNOSTIC rather than a traceback."""
    if not isinstance(value, list) or not all(isinstance(x, str) for x in value):
        raise BaselineError(
            f"{path.as_posix()}: entry {site!r} has a {field!r} of {value!r}, but it "
            f"must be a LIST OF STRINGS — one normalized program name per wait the "
            f"entry covers (e.g. [\"git\", \"git\", \"taskkill\"]). A malformed "
            f"field is rejected rather than coerced: coercing it would decide, "
            f"silently, how many unbounded waits this function is allowed."
        )
    return sorted(value)


def load_baseline(path: Path, *, lenient: bool = False) -> dict[str, dict]:
    """Read the baseline, FAILING CLOSED on any format it does not understand.

    ``lenient`` is used only by `--update-baseline`, which is allowed to read an
    OLDER format in order to migrate the reasons forward. It is NOT allowed to
    read a NEWER one: accepting any `format` whatever meant a future format 4
    file — written by a checker with rules this one does not implement — was
    silently rewritten back down to the weaker schema, dropping every field
    format 4 added. A newer file means the checker is the stale half.
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
        if not isinstance(entry, dict) or not isinstance(entry.get("site"), str):
            raise BaselineError(
                f"{path.as_posix()}: every exemption must be an object with a "
                f"string 'site' key; found {entry!r}."
            )
        site = entry["site"]
        if site in out:
            raise BaselineError(
                f"{path.as_posix()}: duplicate exemption for {site!r}. Two entries "
                f"with the same 'site' silently collapse to one and the earlier "
                f"one's waits and reason are lost — merge them into a single entry."
            )
        out[site] = entry

    if fmt == BASELINE_FORMAT:
        # Validate the shape ONLY for the current format; an older file is read
        # by --update-baseline purely to salvage its prose.
        for site, entry in out.items():
            if not isinstance(entry.get("reason"), str):
                raise BaselineError(
                    f"{path.as_posix()}: entry {site!r} has a non-string 'reason'."
                )
            for field in ("waits", "reason_covers_waits"):
                if field not in entry:
                    raise BaselineError(
                        f"{path.as_posix()}: entry {site!r} is missing {field!r}. "
                        f"Regenerate with --update-baseline."
                    )
                entry[field] = _as_wait_list(path, site, field, entry[field])
        return out

    if lenient:
        if fmt is not None and (isinstance(fmt, bool) or not isinstance(fmt, int)):
            raise BaselineError(
                f"{path.as_posix()} declares a non-integer format {fmt!r}. Refusing "
                f"to migrate a file whose schema version cannot be compared."
            )
        if isinstance(fmt, int) and not isinstance(fmt, bool) and fmt > BASELINE_FORMAT:
            raise BaselineError(
                f"{path.as_posix()} declares format {fmt}, which is NEWER than the "
                f"format {BASELINE_FORMAT} this checker writes. --update-baseline "
                f"would rewrite it DOWN to {BASELINE_FORMAT}, silently dropping "
                f"whatever fields format {fmt} added and re-opening whatever they "
                f"gate. Update the checker instead."
            )
        return out

    raise BaselineError(
        f"{path.as_posix()} declares format {fmt!r}, but this checker requires "
        f"format {BASELINE_FORMAT}.\n"
        f"Format {BASELINE_FORMAT} replaced the two COUNT fields ('sites' and "
        f"'reason_covers_sites') with two WAIT LISTS:\n"
        f"  'waits'               — one normalized program name per untimed wait in "
        f"that function, e.g. [\"osascript\"].\n"
        f"  'reason_covers_waits' — the list the written reason was authored "
        f"against, which must equal 'waits'.\n"
        f"A count alone could not see a count-PRESERVING SWAP: replacing a "
        f"baselined `osascript` one-shot with an `aws s3 ls` call left the count at "
        f"1, so the ratchet stayed green while an unbounded NETWORK call shipped "
        f"under prose written for a local one.\n"
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
            "`impl B { fn run }` are different sites, as are `impl Bar<u8>` and",
            "`impl Bar<u16>`. A trait impl reads as '<Type as Trait>'. The key is",
            "stable across edits above and below the call, which a line number is",
            "not.",
            "",
            "'waits' names WHAT this function currently waits on, one normalized",
            "program per untimed wait: a string-literal program as its lowercased",
            "basename without '.exe', a computed one as 'dyn:<last identifier>', a",
            "Command handed in by a caller or built by a helper as '?' /",
            "'fn:<helper>'. The checker fails if the real list differs in ANY way —",
            "longer, shorter, or the same length with a different program — so the",
            "baseline can only shrink and cannot silently rot.",
            "",
            "A COUNT alone could not see a SWAP. Replacing a baselined `osascript`",
            "one-shot with `Command::new(\"aws\").args([\"s3\",\"ls\",…]).output()`",
            "keeps the count at 1, so a count ratchet stays green while an unbounded",
            "NETWORK call ships under prose written for a local one-shot. The",
            "program token is what makes the swap visible; it is deliberately NOT a",
            "hash of the call text, which would go red on `git status` ->",
            "`git status --porcelain` — an edit that cannot invalidate any reason.",
            "",
            "'reason' is REQUIRED and must say why an unbounded wait is acceptable",
            "HERE — i.e. why this code is not on a periodic or hot path.",
            "'reason_covers_waits' is the list that reason was written against and",
            "MUST equal 'waits'. --update-baseline clears the reason whenever a",
            "wait is ADDED or SWAPPED (never for a pure removal), so neither a new",
            "wait nor a different program can inherit prose written for another",
            "call; and editing 'waits' by hand without touching the reason fails on",
            "the mismatch.",
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
    reason was dropped, and the baseline keys that now match nothing.

    A reason survives ONLY when the new wait list is a sub-multiset of the one
    the reason was authored against: i.e. waits were removed and nothing was
    added or swapped. A strict improvement needs no fresh prose; anything else
    does.
    """
    # Old-format keys are ambiguous exactly when two new keys share one.
    legacy_owner: dict[str, list[str]] = {}
    for site in grouped:
        legacy_owner.setdefault(_legacy_key(site), []).append(site)

    entries: list[dict] = []
    cleared: list[str] = []
    consumed: set[str] = set()
    for site in sorted(grouped):
        waits = sorted(f.program for f in grouped[site])
        prev = existing.get(site)
        if prev is None:
            legacy = _legacy_key(site)
            if len(legacy_owner.get(legacy, [])) == 1:
                prev = existing.get(legacy)
        if prev is not None:
            consumed.add(prev["site"])
        reason = str((prev or {}).get("reason", "") or "")

        covered: list[str]
        if prev is None:
            covered = []
        elif "reason_covers_waits" in prev or "waits" in prev:
            raw_cov = prev.get("reason_covers_waits", prev.get("waits", []))
            covered = sorted(x for x in raw_cov if isinstance(x, str))
        else:
            # FORMAT 1/2 MIGRATION. Those schemas recorded a COUNT only, so the
            # strongest honest statement about their prose is "it was written
            # for N waits in this function". Carry it forward at exactly that
            # strength: if the count still matches, adopt the observed programs
            # as what it covers; if it does not, the count ratchet would have
            # rejected it anyway, so clear.
            try:
                n = int(prev.get("reason_covers_sites", prev.get("sites", 0)) or 0)
            except (TypeError, ValueError):
                n = -1
            covered = list(waits) if len(waits) == n else []

        if reason.strip() and (Counter(waits) - Counter(covered)):
            reason = ""
            cleared.append(site)
        entries.append(
            {
                "site": site,
                "waits": waits,
                # Always equal to `waits` in a freshly written baseline: either
                # the reason survived (nothing was added or swapped) or it was
                # blanked and has to be re-authored against this list anyway.
                "reason_covers_waits": list(waits),
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
        f"{err}  {{ \"site\": \"<path>::<qualified fn>\",",
        f"{err}    \"waits\": [\"<program>\", …], \"reason_covers_waits\": [\"<program>\", …],",
        f"{err}    \"reason\": \"<why it is not periodic>\" }}",
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
        print("from the transitive deps of src-tauri/Cargo.toml. BOTH spellings are")
        print("followed: `path = \"…\"`, and `workspace = true` resolved through the")
        print("workspace root's [workspace.dependencies]. A manifest that will not")
        print("parse is fatal — an unreadable scope is UNKNOWN, never empty:")
        for d in scope.scanned:
            print(f"  {d.relative_to(root).as_posix()}")
        print("\nNOT SCANNED — path deps outside this repo (separate repo, own CI,")
        print("not even checked out in this repo's CI job):")
        for e in scope.external or ["(none)"]:
            print(f"  {e}")
        print("\nNOT SCANNED — in-repo workspace members that the dependency closure")
        print("from src-tauri/Cargo.toml does not REACH. That is a statement about this")
        print("scan, not about how they run: today they ship as their own processes, so")
        print("a blocking wait there cannot consume a runner blocking-pool thread — but")
        print("verify that before trusting it, because the closure following a")
        print("dependency spelling incorrectly would land a linked crate in this list.")
        print("Both `path = \"…\"` and `workspace = true` inheritance are followed.")
        for e in scope.unreached or ["(none)"]:
            print(f"  {e}")
        return 0

    def fail_closed(exc: BaselineError) -> int:
        prefix = "::error::" if args.ci else ""
        for line in str(exc).splitlines():
            print(f"{prefix}{line}", file=sys.stderr)
        print(
            f"\n{prefix}FAILED: the baseline could not be read, so NOTHING is "
            f"exempt. This check fails closed on purpose.",
            file=sys.stderr,
        )
        return 1

    findings = run_scan(root)
    grouped = group(findings)

    if args.list:
        for f in findings:
            print(
                f"{f.path}:{f.line}  {f.function}()  .{f.method}()  on {f.program}"
                f"  |  {f.snippet}"
            )
        print(f"\n{len(findings)} synchronous blocking wait(s) in {len(grouped)} function(s)")
        return 0

    if args.update_baseline:
        try:
            existing = load_baseline(baseline_path, lenient=True)
        except BaselineError as exc:
            # A traceback is not a diagnostic. --update-baseline refuses a
            # baseline it cannot safely migrate (a NEWER format, a corrupt file)
            # for the same reason the check refuses one it cannot read.
            return fail_closed(exc)
        entries, cleared, unmatched = rebuild_entries(grouped, existing)
        write_baseline(baseline_path, entries)
        blank = [e["site"] for e in entries if not str(e["reason"]).strip()]
        print(f"wrote {baseline_path} with {len(entries)} exemption(s)")
        if cleared:
            print(
                f"\n{len(cleared)} entr(y|ies) had their reason CLEARED because a "
                f"wait was ADDED or SWAPPED inside an already-exempt function. New "
                f"prose must not be inherited from a different call — nor from a "
                f"different PROGRAM, which is how an unbounded network call would "
                f"otherwise ship under a reason written for a local one-shot:"
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
        return fail_closed(exc)

    problems: list[str] = []

    # 1. Unexempted sites.
    unexempted: list[str] = []
    for site in sorted(grouped):
        if site not in existing:
            for f in grouped[site]:
                unexempted.append(
                    f"{f.path}:{f.line}  in {f.function}()  .{f.method}()  on {f.program!r}"
                )

    # 2. Ratchet violations and unreasoned entries.
    for site, entry in sorted(existing.items()):
        want = list(entry["waits"])
        covers = list(entry["reason_covers_waits"])
        have = sorted(f.program for f in grouped.get(site, []))
        reason = str(entry.get("reason", "") or "")
        if not reason.strip():
            problems.append(
                f"baseline entry {site!r} has an empty 'reason' — every exemption "
                f"must say why an unbounded wait is acceptable there."
            )
        elif covers != want:
            problems.append(
                f"baseline entry {site!r} allows waits on {want} but its reason was "
                f"written for {covers}. Someone edited 'waits' without rewriting the "
                f"reason. Write a reason that covers {want}, then set "
                f"\"reason_covers_waits\": {json.dumps(want)}."
            )
        if not have:
            problems.append(
                f"baseline entry {site!r} matches nothing any more (it recorded "
                f"{want}). The site was fixed, renamed or moved — delete the entry "
                f"from {BASELINE_PATH.as_posix()}."
            )
        elif have != want:
            added = sorted((Counter(have) - Counter(want)).elements())
            removed = sorted((Counter(want) - Counter(have)).elements())
            if added and removed:
                problems.append(
                    f"{site} now waits on {have}, baseline allows {want}. A wait was "
                    f"REPLACED WITH A DIFFERENT ONE (gone: {removed}; new: {added}) — "
                    f"the count did not move, so nothing else here would have caught "
                    f"it, and the written reason describes {removed}, not {added}. "
                    f"That is how an unbounded network call ships under prose written "
                    f"for a local one-shot. Bound the new call through a wrapper, or "
                    f"run --update-baseline (which CLEARS the reason on a swap) and "
                    f"write a fresh reason covering {have}."
                )
            elif added:
                problems.append(
                    f"{site} now waits on {have}, baseline allows {want}. A NEW "
                    f"untimed wait ({added}) was added to an already-exempt function "
                    f"— route it through a bounded wrapper, or run --update-baseline "
                    f"(which CLEARS the reason on an addition) and write a fresh "
                    f"reason covering all of {have}."
                )
            else:
                problems.append(
                    f"{site} now waits on only {have}, baseline records {want} "
                    f"({removed} gone). Tighten it: set \"waits\": {json.dumps(have)} "
                    f"and \"reason_covers_waits\": {json.dumps(have)} in "
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
