"""Guard: no python-bridge source writes a debug log into the OS temp dir or ~.

Plan 2026-11-14-one-xlib-distribution-and-no-tmp-debug-logs-in-the-python-bridge,
Phase 4. The bridge used to append event payloads to files such as
``$TMP/qontinui_event_emission.log`` and ``~/.qontinui/ai-shell-debug.log``:
unrotated, read by nothing, world-visible on a shared Linux ``/tmp``, and (where
unguarded) able to raise out of the event path on a read-only temp dir. The two
sanctioned routes are ``EventManager.emit_log`` and a module
``logging.getLogger(__name__)`` logger.

This is a source scan (``ast``, stdlib only), so it runs in the command-table CI
job without the qontinui library. It scans every ``*.py`` under python-bridge
except ``tests/``, ``conftest.py`` and script-style test modules (a ``test_*.py``
that defines a ``test_*`` function); a production module whose name merely starts
with ``test_`` (``services/test_analysis_service.py``) IS scanned.

**Sources** (a temp or home path):

- ``gettempdir()``;
- the ``TEMP`` / ``TMP`` / ``TMPDIR`` and ``HOME`` / ``USERPROFILE`` environment
  names (any string literal equal to one), and a ``"/tmp"`` literal;
- ``expanduser("~...")``, ``Path.home()`` (also ``pathlib.Path.home()``), and
  ``<x>.expanduser()`` whose receiver contains a ``"~"`` literal
  (``Path("~/x").expanduser()``).

Imports are resolved per file, dotted receivers included, so
``from tempfile import gettempdir as g`` makes ``g()`` a source and
``from pathlib import Path as P`` / ``import pathlib as pl`` make ``P.home()`` /
``pl.Path.home()`` one.

**Sinks** (a write):

- ``open`` / ``io.open`` / ``codecs.open`` / ``gzip.open`` / ``bz2.open`` /
  ``lzma.open`` (path as ``args[0]`` or ``file=``/``filename=``) in a w/a/x/+ mode;
- ``<path>.open(mode)`` in such a mode, and ``<path>.write_text`` /
  ``<path>.write_bytes``;
- ``logging.FileHandler`` and its rotating/watched subclasses (append by default).

**Arms.** (a) a sink whose path expression reaches a source. A bare name in the
path is resolved through its assignments in the enclosing function, every
enclosing function (closures) and the module, because nearly every historical
site reached ``open()`` through a variable such as ``debug_log_path``.
(b) an assignment whose value contains a source and a ``.log`` string literal
(a log path parked in a variable or constant).

**Known limits** (not followed, so not caught). Each has a ``*_not_followed`` shape
below expecting no (or only the stated) offenders, so widening the scanner shows up
as a deliberate test change:

- a path stored on an attribute (``self.log_path = ...`` then
  ``open(self.log_path, "a")``); arm (b) still flags the assignment when it builds
  a ``.log`` path from a source;
- a path returned by a helper function;
- a path passed in as a parameter, including a parameter DEFAULT built from a
  source (defaults are not assignments);
- ``for`` and ``with ... as`` targets (only assignments are resolved);
- ``tempfile.mkdtemp`` / ``mkstemp`` / ``NamedTemporaryFile`` / ``TemporaryFile``
  outputs. Deliberately not sources: the bridge uses ``mkdtemp`` legitimately
  (``executor_core.py`` ``self.temp_dir``, ``qontinui_executor.py`` ``run_dir``)
  and ``NamedTemporaryFile`` for real temp files;
- ``os.open`` (the low-level fd API) is not a sink;
- ``shutil.copy*`` / ``shutil.move`` into a temp or home dir are not sinks.

Legitimate temp-dir writes are allow-listed by (file, function, arm, path
expression), so a NEW write inside an allow-listed function is still flagged.
A temp dir used only as a subprocess cwd is not a write and needs no entry.
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
HOME_ENV_NAMES = {"HOME", "USERPROFILE"}
_ENV_SOURCE_NAMES = TEMP_ENV_NAMES | HOME_ENV_NAMES
# A receiver of .home() that is pathlib's Path (bare when its import is not seen).
_PATH_CLASS_NAMES = {"Path", "pathlib.Path", "pathlib.PosixPath", "pathlib.WindowsPath"}
_OPEN_MODULES = {"io", "codecs", "builtins", "gzip", "bz2", "lzma"}
_PATH_WRITERS = {"write_text", "write_bytes"}
_FILE_HANDLERS = {
    "FileHandler",
    "RotatingFileHandler",
    "TimedRotatingFileHandler",
    "WatchedFileHandler",
}


@dataclass(frozen=True)
class AllowEntry:
    path: str
    function: str  # qualname as the scanner reports it
    arm: str
    expr: str  # ast.unparse of the flagged path expression / assignment target
    why: str


# Every entry must still match a live raw offender (checked below), so the list
# cannot silently rot, and it is narrow enough that a second write in the same
# function is still reported.
ALLOWLIST: tuple[AllowEntry, ...] = (
    AllowEntry(
        "services/claude_cli_runner.py",
        "_run_native_windows_stdin",
        "a:write",
        "prompt_file",
        "Windows Claude CLI path: the prompt is handed to PowerShell via a temp file",
    ),
    # The QA-results store under ~/.automation-results (AUTOMATION_RESULTS_DIR =
    # Path.home() / ...): a bounded, read-back product artifact (the same module's
    # readers load it), not a debug side log.
    AllowEntry(
        "automation_results.py",
        "save_automation_results",
        "a:write",
        "execution_file",
        "QA-results store: latest execution.json",
    ),
    AllowEntry(
        "automation_results.py",
        "save_automation_results",
        "a:write",
        "dst_log",
        "QA-results store: copies of the run's dev logs beside execution.json",
    ),
)


@dataclass(frozen=True)
class Offender:
    path: str
    line: int
    function: str
    arm: str
    expr: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line} in {self.function} [{self.arm}] {self.expr}"


def _is_test_module(path: Path) -> bool:
    """A ``test_*.py`` that defines at least one ``test_*`` function or method.

    The name alone is not enough: ``services/test_analysis_service.py`` is a
    production module (the Test Builder's analysis service) and must be scanned.
    """
    if not path.name.startswith("test_"):
        return False
    tree = ast.parse(path.read_text(encoding="utf-8"))
    return any(
        isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and n.name.startswith("test_")
        for n in ast.walk(tree)
    )


def iter_source_files(root: Path) -> Iterator[Path]:
    for path in sorted(root.rglob("*.py")):
        rel = path.relative_to(root)
        if any(part in _SKIP_DIRS for part in rel.parts[:-1]):
            continue
        if rel.name == "conftest.py" or _is_test_module(path):
            continue
        yield path


def _str_const(node: ast.AST) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    return None


def _kwarg(call: ast.Call, *names: str) -> ast.expr | None:
    for kw in call.keywords:
        if kw.arg in names:
            return kw.value
    return None


def _mode_is_write(mode: ast.expr | None, default: str) -> bool:
    if mode is None:
        return any(c in default for c in "wax+")
    value = _str_const(mode)
    if value is not None:
        return any(c in value for c in "wax+")
    return True  # a computed mode may write; the path test still has to match


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


def _import_aliases(tree: ast.AST) -> dict[str, tuple[str, str]]:
    """Local name -> (module, imported name), for every ``from m import n [as a]``.

    ``import m as a`` maps ``a`` to ``(m, "")``. Collected file-wide: an import in a
    function body is treated as visible everywhere, which only over-reports.
    """
    aliases: dict[str, tuple[str, str]] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module:
            for alias in node.names:
                aliases[alias.asname or alias.name] = (node.module, alias.name)
        elif isinstance(node, ast.Import):
            for alias in node.names:
                if alias.asname:
                    aliases[alias.asname] = (alias.name, "")
    return aliases


class _FileScanner:
    def __init__(self, tree: ast.Module, rel_path: str) -> None:
        self.tree = tree
        self.rel_path = rel_path
        self.aliases = _import_aliases(tree)
        self.offenders: list[Offender] = []

    # -- names -------------------------------------------------------------
    def _canonical(self, func: ast.expr) -> tuple[str | None, str | None]:
        """(module-ish qualifier, final name) of a call target, resolving imports."""
        if isinstance(func, ast.Name):
            module, name = self.aliases.get(func.id, (None, func.id))
            return module, name or func.id
        if isinstance(func, ast.Attribute):
            return self._dotted(func.value), func.attr
        return None, None

    def _dotted(self, expr: ast.expr) -> str | None:
        """Dotted name of a receiver with its root import resolved, else None.

        ``P`` (``from pathlib import Path as P``) -> ``pathlib.Path``;
        ``pl.Path`` (``import pathlib as pl``) -> ``pathlib.Path``;
        ``gz`` (``import gzip as gz``) -> ``gzip``; an unimported name -> itself.
        """
        if isinstance(expr, ast.Name):
            module, name = self.aliases.get(expr.id, (None, expr.id))
            if module is None:
                return name
            return f"{module}.{name}" if name else module
        if isinstance(expr, ast.Attribute):
            root = self._dotted(expr.value)
            return f"{root}.{expr.attr}" if root else None
        return None

    # -- sources -----------------------------------------------------------
    def _is_direct_source(self, node: ast.AST) -> bool:
        if isinstance(node, ast.Call):
            qualifier, name = self._canonical(node.func)
            if name == "gettempdir":
                return True
            if name == "home" and qualifier in _PATH_CLASS_NAMES:
                return True
            if name == "expanduser":
                if node.args and (_str_const(node.args[0]) or "").startswith("~"):
                    return True
                if (
                    not node.args
                    and isinstance(node.func, ast.Attribute)
                    and any(
                        (_str_const(n) or "").startswith("~") for n in ast.walk(node.func.value)
                    )
                ):
                    return True
        value = _str_const(node)
        if value is not None:
            return value in _ENV_SOURCE_NAMES or value == "/tmp" or value.startswith("/tmp/")
        return False

    def _contains_source(self, expr: ast.AST) -> bool:
        return any(self._is_direct_source(n) for n in ast.walk(expr))

    def _tainted(self, expr: ast.AST, scopes: list[Scope], seen: frozenset[str]) -> bool:
        if self._contains_source(expr):
            return True
        for node in ast.walk(expr):
            if isinstance(node, ast.Name) and node.id not in seen:
                for scope in scopes:
                    for value in scope.get(node.id, ()):
                        if self._tainted(value, scopes, seen | {node.id}):
                            return True
        return False

    # -- sinks -------------------------------------------------------------
    def _write_target(self, call: ast.Call) -> ast.expr | None:
        """The path expression of a write call, or None if ``call`` is not a write."""
        func = call.func
        qualifier, name = self._canonical(func)
        if name in _PATH_WRITERS and isinstance(func, ast.Attribute):
            return func.value
        if name in _FILE_HANDLERS:
            path = call.args[0] if call.args else _kwarg(call, "filename")
            mode = call.args[1] if len(call.args) > 1 else _kwarg(call, "mode")
            return path if path is not None and _mode_is_write(mode, "a") else None
        if name != "open":
            return None
        is_module_open = (isinstance(func, ast.Name) and qualifier in (None, *_OPEN_MODULES)) or (
            isinstance(func, ast.Attribute) and qualifier in _OPEN_MODULES
        )
        if is_module_open:
            if call.args:
                path = call.args[0]
                mode = call.args[1] if len(call.args) > 1 else _kwarg(call, "mode")
            else:
                path = _kwarg(call, "file", "filename")
                mode = _kwarg(call, "mode")
            return path if path is not None and _mode_is_write(mode, "r") else None
        if isinstance(func, ast.Attribute) and qualifier != "os":
            # <path>.open(mode)
            mode = call.args[0] if call.args else _kwarg(call, "mode")
            return func.value if _mode_is_write(mode, "r") else None
        return None

    # -- walk --------------------------------------------------------------
    def _flag(self, node: ast.AST, qualname: str, arm: str, expr: ast.AST) -> None:
        self.offenders.append(
            Offender(self.rel_path, node.lineno, qualname, arm, ast.unparse(expr))
        )

    def _visit(self, owner: ast.AST, qualname: str, scopes: list[Scope]) -> None:
        stack = list(ast.iter_child_nodes(owner))
        while stack:
            node = stack.pop()
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                inner = f"{qualname}.{node.name}" if qualname != "<module>" else node.name
                self._visit(node, inner, [_collect_assignments(node), *scopes])
                continue
            if isinstance(node, ast.ClassDef):
                inner = f"{qualname}.{node.name}" if qualname != "<module>" else node.name
                # Class bodies are not a closure scope for their methods.
                self._visit(node, inner, scopes)
                continue
            if isinstance(node, ast.Call):
                target = self._write_target(node)
                if target is not None and self._tainted(target, scopes, frozenset()):
                    self._flag(node, qualname, "a:write", target)
            if isinstance(node, (ast.Assign, ast.AnnAssign)) and node.value is not None:
                if self._contains_source(node.value) and any(
                    ".log" in (_str_const(n) or "") for n in ast.walk(node.value)
                ):
                    targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                    self._flag(node, qualname, "b:log-path", targets[0])
            stack.extend(ast.iter_child_nodes(node))

    def scan(self) -> list[Offender]:
        self._visit(self.tree, "<module>", [_collect_assignments(self.tree)])
        return self.offenders


def scan_source(source: str, rel_path: str) -> list[Offender]:
    """Every raw offender in ``source``, before the allowlist is applied."""
    return _FileScanner(ast.parse(source, filename=rel_path), rel_path).scan()


def _allow_entry(offender: Offender) -> AllowEntry | None:
    for entry in ALLOWLIST:
        if (entry.path, entry.function, entry.arm, entry.expr) == (
            offender.path,
            offender.function,
            offender.arm,
            offender.expr,
        ):
            return entry
    return None


def scan_bridge_raw(root: Path = BRIDGE_ROOT) -> list[Offender]:
    found: list[Offender] = []
    for path in iter_source_files(root):
        rel = path.relative_to(root).as_posix()
        found.extend(scan_source(path.read_text(encoding="utf-8"), rel))
    return sorted(found, key=lambda o: (o.path, o.line))


def scan_bridge(root: Path = BRIDGE_ROOT) -> list[Offender]:
    return [o for o in scan_bridge_raw(root) if _allow_entry(o) is None]


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
        f"a genuine non-log write by (file, function, arm, expr).\nper file:\n{summary}\n{report}"
    )


def test_scan_is_not_vacuous():
    files = list(iter_source_files(BRIDGE_ROOT))
    assert len(files) > 50, f"scanned only {len(files)} files under {BRIDGE_ROOT}"
    rels = {p.relative_to(BRIDGE_ROOT).as_posix() for p in files}
    assert {"qontinui_executor.py", "event_manager.py", "event_translator.py"} <= rels


def test_every_allowlist_entry_matches_exactly_one_live_offender():
    raw = scan_bridge_raw()
    for entry in ALLOWLIST:
        hits = [o for o in raw if _allow_entry(o) is entry]
        assert len(hits) == 1, f"allowlist entry {entry} matches {len(hits)} offenders: {hits}"


# Each shape the guard must keep catching, with its expected arms.
_SHAPES: dict[str, tuple[str, list[str]]] = {
    "closure_through_variable": (
        """
import os
def handler():
    debug_log = os.path.join(os.path.expanduser("~"), ".qontinui", "ai-shell-debug.log")
    def debug(msg):
        with open(debug_log, "a", encoding="utf-8") as f:
            f.write(msg)
    debug("x")
""",
        ["a:write", "b:log-path"],
    ),
    "two_level_closure": (
        """
import tempfile
def outer():
    p = tempfile.gettempdir() + "/x.txt"
    def middle():
        def inner():
            open(p, "w").write("x")
        inner()
    middle()
""",
        ["a:write"],
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
    "inline_gettempdir_mode_kw": (
        "import os, tempfile\nopen(os.path.join(tempfile.gettempdir(), 'm'), mode='a')\n",
        ["a:write"],
    ),
    "file_keyword": (
        "import tempfile\nopen(file=tempfile.gettempdir() + '/m', mode='w')\n",
        ["a:write"],
    ),
    "io_open": ("import io, tempfile\nio.open(tempfile.gettempdir() + '/m', 'w')\n", ["a:write"]),
    "codecs_open": (
        "import codecs, tempfile\ncodecs.open(tempfile.gettempdir() + '/m', 'a', 'utf-8')\n",
        ["a:write"],
    ),
    "gzip_open": ("import gzip\ngzip.open('/tmp/m.gz', 'wb')\n", ["a:write"]),
    "gzip_open_default_read": ("import gzip\ngzip.open('/tmp/m.gz')\n", []),
    "path_open": (
        "import tempfile\nfrom pathlib import Path\nPath(tempfile.gettempdir()).open('a')\n",
        ["a:write"],
    ),
    "path_write_text": (
        "import tempfile\nfrom pathlib import Path\n"
        "(Path(tempfile.gettempdir()) / 'dump.json').write_text('x')\n",
        ["a:write"],
    ),
    "path_write_bytes": (
        "from pathlib import Path\n(Path('/tmp') / 'dump.bin').write_bytes(b'x')\n",
        ["a:write"],
    ),
    "path_home": (
        "from pathlib import Path\n(Path.home() / '.qontinui' / 'x').write_text('x')\n",
        ["a:write"],
    ),
    "aliased_path_home": (
        "from pathlib import Path as P\nP.home().joinpath('x').open('w')\n",
        ["a:write"],
    ),
    "tilde_receiver_expanduser": (
        "from pathlib import Path\nPath('~/.qontinui/x').expanduser().write_text('x')\n",
        ["a:write"],
    ),
    "aliased_gettempdir": (
        "from tempfile import gettempdir as g\nopen(g() + '/m', 'a')\n",
        ["a:write"],
    ),
    "aliased_expanduser_log_path": (
        "from os.path import expanduser as eu\nLOG = eu('~') + '/debug.log'\n",
        ["b:log-path"],
    ),
    "file_handler": (
        "import logging, tempfile\nlogging.FileHandler(tempfile.gettempdir() + '/bridge')\n",
        ["a:write"],
    ),
    "dotted_pathlib_path_home": (
        "import pathlib\npathlib.Path.home().joinpath('x').open('a')\n",
        ["a:write"],
    ),
    "aliased_module_path_home": (
        "import pathlib as pl\n(pl.Path.home() / 'x').write_text('x')\n",
        ["a:write"],
    ),
    "home_env": ("import os\nopen(os.environ['HOME'] + '/x', 'a')\n", ["a:write"]),
    "userprofile_env_log_path": (
        "import os\nLOG = os.path.join(os.getenv('USERPROFILE'), 'debug.log')\n",
        ["b:log-path"],
    ),
    "rotating_handler_imported": (
        "from logging.handlers import RotatingFileHandler\nRotatingFileHandler('/tmp/x')\n",
        ["a:write"],
    ),
    "read_is_fine": (
        "import os, tempfile\nopen(os.path.join(tempfile.gettempdir(), 'm.txt')).read()\n",
        [],
    ),
    "non_temp_write_is_fine": ("def f(d):\n    open(d + '/x.log', 'w')\n", []),
    "home_without_write_is_fine": (
        "from pathlib import Path\nCACHE = Path.home() / '.cache'\n",
        [],
    ),
    # ---- documented limits (see "Known limits"): pinned, not endorsed ----
    # A path parked on an attribute is not followed by arm (a). Arm (b) still
    # flags the assignment because it builds a .log path.
    "self_attribute_not_followed": (
        """
import tempfile
class C:
    def __init__(self):
        self.p = tempfile.gettempdir() + "/x.txt"
        self.q = tempfile.gettempdir() + "/x.log"
    def w(self):
        open(self.p, "a")
        open(self.q, "a")
""",
        ["b:log-path"],
    ),
    "helper_return_not_followed": (
        "import tempfile\n"
        "def where():\n    return tempfile.gettempdir() + '/x'\n"
        "def w():\n    open(where(), 'a')\n",
        [],
    ),
    "parameter_not_followed": ("def w(p):\n    open(p, 'a')\n", []),
    "parameter_default_not_followed": (
        "import tempfile\ndef w(p=tempfile.gettempdir() + '/x'):\n    open(p, 'a')\n",
        [],
    ),
    "for_target_not_followed": (
        "import tempfile\nfor d in [tempfile.gettempdir()]:\n    open(d + '/x', 'a')\n",
        [],
    ),
    "with_target_not_followed": (
        "import tempfile\nwith tempfile.TemporaryDirectory() as d:\n    open(d + '/x', 'a')\n",
        [],
    ),
    "mkdtemp_not_followed": (
        "import tempfile\nd = tempfile.mkdtemp()\nopen(d + '/x.log', 'a')\n",
        [],
    ),
    "mkstemp_not_followed": (
        "import tempfile\nfd, p = tempfile.mkstemp(suffix='.log')\nopen(p, 'a')\n",
        [],
    ),
    "named_temporary_file_not_followed": (
        "import tempfile\n"
        "with tempfile.NamedTemporaryFile('w', suffix='.log', delete=False) as f:\n"
        "    f.write('x')\n",
        [],
    ),
    "os_open_not_followed": (
        "import os, tempfile\nos.open(tempfile.gettempdir() + '/x', os.O_WRONLY | os.O_CREAT)\n",
        [],
    ),
    "shutil_copy_not_followed": (
        "import shutil, tempfile\nshutil.copy('a.txt', tempfile.gettempdir())\n",
        [],
    ),
}


def test_scanner_catches_known_shapes():
    failures = []
    for name, (source, expected) in _SHAPES.items():
        arms = sorted(o.arm for o in scan_source(source, f"{name}.py"))
        if arms != sorted(expected):
            failures.append(f"{name}: got {arms}, want {sorted(expected)}")
    assert not failures, "\n".join(failures)


def test_allowlist_is_narrow():
    """A second temp write in an allow-listed function is still reported."""
    source = """
import os, tempfile
def _run_native_windows_stdin():
    prompt_file = os.path.join(tempfile.gettempdir(), "p.txt")
    open(prompt_file, "w")
    open(os.path.join(tempfile.gettempdir(), "debug.txt"), "a")
"""
    raw = scan_source(source, "services/claude_cli_runner.py")
    kept = [o for o in raw if _allow_entry(o) is None]
    assert len(raw) == 2
    assert [o.expr for o in kept] == ["os.path.join(tempfile.gettempdir(), 'debug.txt')"]
