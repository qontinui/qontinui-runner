"""Guard: no python-bridge source writes a debug log into the OS temp dir or ~.

Plan 2026-11-14-one-xlib-distribution-and-no-tmp-debug-logs-in-the-python-bridge,
Phase 4. The bridge used to append event payloads to files such as
``$TMP/qontinui_event_emission.log`` and ``~/.qontinui/ai-shell-debug.log``:
unrotated, read by nothing, world-visible on a shared Linux ``/tmp``, and (where
unguarded) able to raise out of the event path on a read-only temp dir. The two
sanctioned routes are ``EventManager.emit_log`` and a module
``logging.getLogger(__name__)`` logger.

This is a source scan (``ast``, stdlib only), so it runs in the command-table CI
job without the qontinui library. It flags, in every non-test ``*.py`` under
python-bridge:

(a) a write (``open``/``io.open``/``codecs.open`` or ``<path>.open`` in a
    w/a/x/+ mode, or ``<path>.write_text``/``write_bytes``) whose path
    expression references ``gettempdir()``, the ``TEMP``/``TMP``/``TMPDIR``
    environment names, a ``"/tmp"`` literal or ``expanduser("~...")``.
    A bare name in the path is resolved through its assignments in the
    enclosing function, every enclosing function (closures) and the module,
    because nearly every historical site reached ``open()`` through a variable
    such as ``debug_log_path``;
(b) an assignment whose value calls one of those sources and also contains a
    ``.log`` string literal (a log path parked in a variable or constant).

Legitimate temp-dir uses are allow-listed by ``file:function`` below.
"""

from __future__ import annotations

import ast
from collections import Counter
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

BRIDGE_ROOT = Path(__file__).resolve().parent.parent

# Directories that are not bridge source.
_SKIP_DIRS = {"tests", ".venv", "venv", "build", "dist", "__pycache__", ".pytest_cache"}

TEMP_ENV_NAMES = {"TEMP", "TMP", "TMPDIR"}
_OPEN_FUNCS = {"open"}  # builtin / io.open / codecs.open: path is args[0]
_PATH_WRITERS = {"write_text", "write_bytes"}

# file:function -> why the write is not a debug log. Every entry must name a
# function that exists (checked below), so the list cannot silently rot.
ALLOWLIST: dict[str, str] = {
    # Windows Claude CLI path: the prompt is handed to PowerShell via a temp file.
    "services/claude_cli_runner.py:_run_native_windows_stdin": "prompt temp file",
    # Temp dir used as the CLI's cwd so it picks up no CLAUDE.md (no write).
    "services/ai_builder_generator.py:generate_via_claude_cli": "temp dir as CLI cwd",
    "services/ai_shell_command_generator.py:generate_command_via_claude_cli": (
        "temp dir as CLI cwd"
    ),
}


@dataclass(frozen=True)
class Offender:
    path: str
    line: int
    function: str
    arm: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line} in {self.function} [{self.arm}]"


def iter_source_files(root: Path) -> Iterator[Path]:
    for path in sorted(root.rglob("*.py")):
        rel = path.relative_to(root)
        if any(part in _SKIP_DIRS for part in rel.parts[:-1]):
            continue
        if rel.name.startswith("test_") or rel.name == "conftest.py":
            continue
        yield path


def _call_name(func: ast.expr) -> str | None:
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def _is_direct_source(node: ast.AST) -> bool:
    """True if ``node`` itself is one of the temp/home path sources."""
    if isinstance(node, ast.Call):
        name = _call_name(node.func)
        if name == "gettempdir":
            return True
        if (
            name == "expanduser"
            and node.args
            and isinstance(node.args[0], ast.Constant)
            and isinstance(node.args[0].value, str)
            and node.args[0].value.startswith("~")
        ):
            return True
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        value = node.value
        return value in TEMP_ENV_NAMES or value == "/tmp" or value.startswith("/tmp/")
    return False


def _contains_source(expr: ast.AST) -> bool:
    return any(_is_direct_source(n) for n in ast.walk(expr))


def _contains_log_literal(expr: ast.AST) -> bool:
    return any(
        isinstance(n, ast.Constant) and isinstance(n.value, str) and ".log" in n.value
        for n in ast.walk(expr)
    )


Scope = dict[str, list[ast.expr]]


def _collect_assignments(body_owner: ast.AST) -> Scope:
    """Name -> assigned values, for one scope (not descending into nested defs)."""
    scope: Scope = {}

    def add(target: ast.expr, value: ast.expr) -> None:
        if isinstance(target, ast.Name):
            scope.setdefault(target.id, []).append(value)
        elif isinstance(target, (ast.Tuple, ast.List)):
            for elt in target.elts:  # conservative: each element may carry the value
                add(elt, value)

    stack = list(ast.iter_child_nodes(body_owner))
    while stack:
        node = stack.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef)):
            continue
        if isinstance(node, ast.Assign):
            for target in node.targets:
                add(target, node.value)
        elif isinstance(node, (ast.AnnAssign, ast.AugAssign, ast.NamedExpr)) and (
            node.value is not None
        ):
            add(node.target, node.value)
        stack.extend(ast.iter_child_nodes(node))
    return scope


def _tainted(expr: ast.AST, scopes: list[Scope], seen: frozenset[str] = frozenset()) -> bool:
    """True if ``expr`` reaches a temp/home source, resolving names through ``scopes``."""
    if _contains_source(expr):
        return True
    for node in ast.walk(expr):
        if isinstance(node, ast.Name) and node.id not in seen:
            for scope in scopes:
                for value in scope.get(node.id, ()):
                    if _tainted(value, scopes, seen | {node.id}):
                        return True
    return False


def _mode_is_write(mode: ast.expr | None, default: str) -> bool:
    if mode is None:
        return any(c in default for c in "wax+")
    if isinstance(mode, ast.Constant) and isinstance(mode.value, str):
        return any(c in mode.value for c in "wax+")
    return True  # a computed mode may write; the path test still has to match


def _kwarg(call: ast.Call, name: str) -> ast.expr | None:
    for kw in call.keywords:
        if kw.arg == name:
            return kw.value
    return None


def _write_target(call: ast.Call) -> ast.expr | None:
    """The path expression of a write call, or None if ``call`` is not a write."""
    func = call.func
    name = _call_name(func)
    if name in _PATH_WRITERS and isinstance(func, ast.Attribute):
        return func.value
    if name not in _OPEN_FUNCS:
        return None
    is_module_open = isinstance(func, ast.Name) or (
        isinstance(func, ast.Attribute)
        and isinstance(func.value, ast.Name)
        and func.value.id in {"io", "codecs", "builtins"}
    )
    if is_module_open:
        if not call.args:
            path = _kwarg(call, "file")
            mode = _kwarg(call, "mode")
        else:
            path = call.args[0]
            mode = call.args[1] if len(call.args) > 1 else _kwarg(call, "mode")
        return path if path is not None and _mode_is_write(mode, "r") else None
    if isinstance(func, ast.Attribute) and not (
        isinstance(func.value, ast.Name) and func.value.id == "os"
    ):
        # Path(...).open(mode)
        mode = call.args[0] if call.args else _kwarg(call, "mode")
        return func.value if _mode_is_write(mode, "r") else None
    return None


def scan_source(source: str, rel_path: str) -> list[Offender]:
    tree = ast.parse(source, filename=rel_path)
    offenders: list[Offender] = []
    module_scope = _collect_assignments(tree)

    def visit(owner: ast.AST, qualname: str, scopes: list[Scope]) -> None:
        stack = list(ast.iter_child_nodes(owner))
        while stack:
            node = stack.pop()
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                inner = f"{qualname}.{node.name}" if qualname != "<module>" else node.name
                visit(node, inner, [_collect_assignments(node), *scopes])
                continue
            if isinstance(node, ast.ClassDef):
                inner = f"{qualname}.{node.name}" if qualname != "<module>" else node.name
                # Class bodies are not a closure scope for their methods.
                visit(node, inner, scopes)
                continue
            if isinstance(node, ast.Call):
                target = _write_target(node)
                if target is not None and _tainted(target, scopes):
                    offenders.append(Offender(rel_path, node.lineno, qualname, "a:write"))
            if isinstance(node, (ast.Assign, ast.AnnAssign)) and node.value is not None:
                if _contains_source(node.value) and _contains_log_literal(node.value):
                    offenders.append(Offender(rel_path, node.lineno, qualname, "b:log-path"))
            stack.extend(ast.iter_child_nodes(node))

    visit(tree, "<module>", [module_scope])
    return offenders


def _allowed(offender: Offender) -> bool:
    function = offender.function.rsplit(".", 1)[-1]
    return f"{offender.path}:{function}" in ALLOWLIST or (
        f"{offender.path}:{offender.function}" in ALLOWLIST
    )


def scan_bridge(root: Path = BRIDGE_ROOT) -> list[Offender]:
    found: list[Offender] = []
    for path in iter_source_files(root):
        rel = path.relative_to(root).as_posix()
        found.extend(scan_source(path.read_text(encoding="utf-8"), rel))
    return sorted((o for o in found if not _allowed(o)), key=lambda o: (o.path, o.line))


# --------------------------------------------------------------------------- tests


def test_no_temp_or_home_debug_logs_in_bridge_source():
    offenders = scan_bridge()
    per_file = Counter(o.path for o in offenders)
    report = "\n".join(str(o) for o in offenders)
    summary = "\n".join(f"{path}: {count}" for path, count in sorted(per_file.items()))
    print(f"per-file offender counts:\n{summary or '(none)'}\noffenders:\n{report or '(none)'}")
    assert dict(per_file) == {}, (
        "python-bridge writes a debug log under the OS temp dir or ~ -- route it to "
        "EventManager.emit_log or logging.getLogger(__name__) instead, or allow-list "
        f"a genuine non-log use by file:function.\nper file:\n{summary}\n{report}"
    )


def test_scan_is_not_vacuous():
    files = list(iter_source_files(BRIDGE_ROOT))
    assert len(files) > 50, f"scanned only {len(files)} files under {BRIDGE_ROOT}"
    rels = {p.relative_to(BRIDGE_ROOT).as_posix() for p in files}
    assert {"qontinui_executor.py", "event_manager.py", "event_translator.py"} <= rels


def test_allowlist_entries_name_existing_functions():
    for entry in ALLOWLIST:
        rel, function = entry.split(":", 1)
        tree = ast.parse((BRIDGE_ROOT / rel).read_text(encoding="utf-8"))
        names = {
            n.name for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
        }
        assert function in names, f"stale allowlist entry {entry}"


# Each historical shape the guard must keep catching, with its expected arms.
_SHAPES = {
    "closure_through_variable": (
        """
import os, datetime
def handler():
    debug_log = os.path.join(os.path.expanduser("~"), ".qontinui", "ai-shell-debug.log")
    def debug(msg):
        with open(debug_log, "a", encoding="utf-8") as f:
            f.write(msg)
    debug("x")
""",
        ["a:write", "b:log-path"],
    ),
    "env_temp_no_try": (
        """
import os
class M:
    def emit(self):
        p = os.path.join(os.environ.get("TEMP", "/tmp"), "qontinui_tree_event_debug.log")
        with open(p, "a") as f:
            f.write("x")
""",
        ["a:write", "b:log-path"],
    ),
    "module_level_startup": (
        """
import os, tempfile
debug_log_path = os.path.join(tempfile.gettempdir(), "x_startup.log")
with open(debug_log_path, "a", encoding="utf-8") as f:
    f.write("started")
""",
        ["a:write", "b:log-path"],
    ),
    "inline_gettempdir": (
        """
import os, tempfile
def f():
    with open(os.path.join(tempfile.gettempdir(), "m.log"), mode="a") as fh:
        fh.write("x")
""",
        ["a:write"],
    ),
    "path_write_text": (
        """
import tempfile
from pathlib import Path
def f():
    (Path(tempfile.gettempdir()) / "dump.json").write_text("x")
""",
        ["a:write"],
    ),
    "read_is_fine": (
        """
import os, tempfile
def f():
    with open(os.path.join(tempfile.gettempdir(), "m.txt")) as fh:
        return fh.read()
""",
        [],
    ),
    "non_temp_write_is_fine": (
        """
def f(out_dir):
    with open(out_dir + "/x.log", "w") as fh:
        fh.write("x")
""",
        [],
    ),
}


def test_scanner_catches_known_shapes():
    for name, (source, expected) in _SHAPES.items():
        arms = sorted(o.arm for o in scan_source(source, f"{name}.py"))
        assert arms == sorted(expected), f"{name}: got {arms}, want {sorted(expected)}"
