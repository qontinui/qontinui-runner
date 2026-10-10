"""Guards for the executor's command table (``QontinuiExecutor.COMMANDS``).

The executor and its command mixins (``executor_commands/``) are read as source with
``ast`` (and Rust with a regex); nothing imports ``qontinui_executor`` or a mixin, so
these run without the ``qontinui`` library or any GPU dependency installed. Only the
dependency-free ``executor_commands/_table.py`` is loaded, on its own, from its file.

What the Rust-sender guard (``test_every_rust_sent_command_is_handled``) cannot see, by
construction:

- ``src-tauri/src/mcp/gui_execution.rs`` (~:860) forwards a caller-chosen ``cmd_type``
  taken from an HTTP request body; there is no literal to check.
- ``src-tauri/src/mcp/testing.rs`` (~:399) sends a variable whose values
  (``testing_mock_click`` / ``testing_mock_type`` / ``testing_mock_screenshot``) are tuple
  literals in a ``match``, not call arguments. All three are in the table already.

Any other sender that passes a non-literal command name is likewise out of reach.
It does see ``send_command_async(`` calls, and the hand-built
``ExecutorCommand { command: "...".to_string() }`` literals under ``src-tauri/src/executor/``.

**No CI job runs this file yet** (no workflow runs ``python-bridge/tests``); run it
locally with ``cd python-bridge && python -m pytest tests/test_executor_command_table.py``
(``tests/conftest.py`` imports ``models``, so pytest must run from ``python-bridge/``).
"""

from __future__ import annotations

import ast
import importlib.util
import re
from pathlib import Path

import pytest

BRIDGE_DIR = Path(__file__).resolve().parent.parent
EXECUTOR = BRIDGE_DIR / "qontinui_executor.py"
EXTRACTION_EXECUTOR = BRIDGE_DIR / "extraction_executor.py"
SNAPSHOT = Path(__file__).resolve().parent / "executor-commands.snapshot.txt"
METHODS_SNAPSHOT = Path(__file__).resolve().parent / "executor-methods.snapshot.txt"
PACKAGE_DIR = BRIDGE_DIR / "executor_commands"
RUST_SRC = BRIDGE_DIR.parent / "src-tauri" / "src"

# Command names the Rust side sends that no Python dispatcher handles; each would return
# ``Unknown command``. Empty since plan
# 2026-10-06-runner-python-bridge-guards-run-nowhere-and-seven-rust-commands-have-no-handler
# deleted the last seven senders. ``test_every_rust_sent_command_is_handled`` fails on any
# new unhandled sender; add an entry here only for a known gap, with a pointer to the plan
# that will delete or route it.
KNOWN_UNHANDLED: dict[str, str] = {}

# Newline-tolerant: ``\s*`` spans the line break when the literal sits on the next line.
_SEND_RE = re.compile(r'send_command(?:_and_wait|_async)?\(\s*"([^"]+)"')
# The bridge also builds ``ExecutorCommand { command: "...".to_string(), .. }`` by hand.
_STRUCT_RE = re.compile(r'\bcommand:\s*"([^"]+)"\.to_string\(\)')
RUST_EXECUTOR_DIR = RUST_SRC / "executor"


def _load_snapshot_script():
    path = BRIDGE_DIR / "scripts" / "snapshot_executor_commands.py"
    spec = importlib.util.spec_from_file_location("snapshot_executor_commands", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


snapshot_script = _load_snapshot_script()


def _load_table_module():
    """``executor_commands/_table.py`` alone: importing the package would run ``_shared``."""
    path = PACKAGE_DIR / "_table.py"
    spec = importlib.util.spec_from_file_location("executor_commands_table", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


table_module = _load_table_module()


def _source() -> str:
    return EXECUTOR.read_text(encoding="utf-8")


def _executor_methods() -> dict[str, ast.FunctionDef]:
    tree = ast.parse(_source())
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "QontinuiExecutor")
    return {n.name: n for n in cls.body if isinstance(n, ast.FunctionDef)}


def _executor_table() -> dict[str, tuple[str, bool]]:
    return snapshot_script.extract_table(_source(), BRIDGE_DIR / "executor_commands")


def rust_sent_commands(text: str, *, struct_literals: bool = False) -> set[str]:
    """Literal first arguments of ``send_command[_and_wait|_async](`` calls in ``text``.

    With ``struct_literals``, also the ``command: "...".to_string()`` fields of hand-built
    ``ExecutorCommand`` values (only meaningful inside ``src-tauri/src/executor/``).
    """
    sent = set(_SEND_RE.findall(text))
    if struct_literals:
        sent |= set(_STRUCT_RE.findall(text))
    return sent


def _all_rust_sent_commands() -> set[str]:
    sent: set[str] = set()
    for path in RUST_SRC.rglob("*.rs"):
        sent |= rust_sent_commands(
            path.read_text(encoding="utf-8", errors="replace"),
            struct_literals=RUST_EXECUTOR_DIR in path.parents,
        )
    return sent


def _extraction_executor_commands() -> set[str]:
    """Names compared against ``cmd_name`` in ``extraction_executor.py``'s dispatch chain."""
    tree = ast.parse(EXTRACTION_EXECUTOR.read_text(encoding="utf-8"))
    names = set()
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Compare)
            and isinstance(node.left, ast.Name)
            and node.left.id == "cmd_name"
            and len(node.ops) == 1
            and isinstance(node.ops[0], ast.Eq)
            and isinstance(node.comparators[0], ast.Constant)
            and isinstance(node.comparators[0].value, str)
        ):
            names.add(node.comparators[0].value)
    return names


# 1 ---------------------------------------------------------------------------


def test_command_table_matches_snapshot():
    """The table equals the snapshot generated from the pre-table if/elif chain."""
    rendered = snapshot_script.render(_executor_table())
    expected = SNAPSHOT.read_text(encoding="utf-8")
    assert rendered == expected, (
        "Executor command table drifted from tests/executor-commands.snapshot.txt. "
        "If the change is intended, regenerate it with "
        "`python scripts/snapshot_executor_commands.py --output tests/executor-commands.snapshot.txt`."
    )


def test_snapshot_has_118_commands():
    assert len(SNAPSHOT.read_text(encoding="utf-8").splitlines()) == 118


# 2 ---------------------------------------------------------------------------


def test_no_duplicate_command_names():
    pairs = snapshot_script.command_pairs(_source(), BRIDGE_DIR / "executor_commands")
    assert pairs, "no COMMANDS table found on QontinuiExecutor"
    names = [name for _cls, name, _method in pairs]
    duplicates = sorted({n for n in names if names.count(n) > 1})
    assert not duplicates, f"command names declared more than once: {duplicates}"


# 3 ---------------------------------------------------------------------------


def test_send_scanner_is_newline_tolerant():
    text = (
        'bridge.send_command("single_line", None);\n'
        "bridge.send_command_and_wait(\n"
        '    "zz_new_cmd",\n'
        "    Some(params),\n"
        "    timeout,\n"
        ");\n"
        "bridge.send_command_and_wait(&cmd_type, params, timeout);\n"
    )
    assert rust_sent_commands(text) == {"single_line", "zz_new_cmd"}


def test_send_scanner_sees_async_sends_and_struct_literals():
    text = (
        'self.send_command_async("zz_async", None);\n'
        "let cmd = ExecutorCommand {\n"
        '    command: "zz_struct".to_string(),\n'
        "    params: None,\n"
        "};\n"
    )
    assert rust_sent_commands(text) == {"zz_async"}
    assert rust_sent_commands(text, struct_literals=True) == {"zz_async", "zz_struct"}


def test_every_rust_sent_command_is_handled():
    sent = _all_rust_sent_commands()
    assert sent, f"no send_command literals found under {RUST_SRC}"
    handled = set(_executor_table()) | _extraction_executor_commands() | set(KNOWN_UNHANDLED)
    unhandled = sorted(sent - handled)
    assert not unhandled, (
        f"Rust sends commands no Python dispatcher handles: {unhandled}. Add them to "
        "QontinuiExecutor.COMMANDS, or (for a known gap) to KNOWN_UNHANDLED with a pointer."
    )


def test_known_unhandled_are_still_sent_and_still_unhandled():
    sent = _all_rust_sent_commands()
    stale = sorted(set(KNOWN_UNHANDLED) - sent)
    assert not stale, f"KNOWN_UNHANDLED names Rust no longer sends; remove them: {stale}"
    now_handled = sorted(
        set(KNOWN_UNHANDLED) & (set(_executor_table()) | _extraction_executor_commands())
    )
    assert not now_handled, f"KNOWN_UNHANDLED names are now handled; remove them: {now_handled}"


def test_extraction_executor_command_set_is_parsed():
    assert {"export_training_data", "list_extractions"} <= _extraction_executor_commands()


# 4 ---------------------------------------------------------------------------

_UNKNOWN_RETURN = ast.dump(
    ast.parse('{"success": False, "error": f"Unknown command: {cmd_type}"}', mode="eval").body
)


def test_unknown_command_returns_the_old_dict():
    handle = _executor_methods()["handle_command"]
    returns = [
        ast.dump(n.value)
        for n in ast.walk(handle)
        if isinstance(n, ast.Return) and n.value is not None
    ]
    assert _UNKNOWN_RETURN in returns


# 5 ---------------------------------------------------------------------------


def _is_pong_print(node: ast.AST) -> bool:
    """``print(json.dumps(<x>), flush=True)``."""
    return (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "print"
        and len(node.args) == 1
        and isinstance(node.args[0], ast.Call)
        and ast.unparse(node.args[0].func) == "json.dumps"
        and any(
            k.arg == "flush" and isinstance(k.value, ast.Constant) and k.value.value is True
            for k in node.keywords
        )
    )


def test_ping_prints_pong_to_stdout_before_returning():
    ping = _executor_methods()["_cmd_ping"]
    body = [
        s for s in ping.body if not (isinstance(s, ast.Expr) and isinstance(s.value, ast.Constant))
    ]
    # pong_message = {"type": "pong", "timestamp": time.time()}
    assign = body[0]
    assert isinstance(assign, ast.Assign) and isinstance(assign.value, ast.Dict)
    keys = [k.value for k in assign.value.keys if isinstance(k, ast.Constant)]
    assert keys == ["type", "timestamp"]
    assert ast.literal_eval(assign.value.values[0]) == "pong"
    # print(json.dumps(pong_message), flush=True)
    assert isinstance(body[1], ast.Expr) and _is_pong_print(body[1].value)
    assert ast.unparse(body[1].value.args[0].args[0]) == ast.unparse(assign.targets[0])
    # return {"success": True}
    assert isinstance(body[2], ast.Return)
    assert ast.literal_eval(body[2].value) == {"success": True}


def test_log_guard_still_skips_ping_and_status():
    handle = _executor_methods()["handle_command"]
    guards = [
        n.test
        for n in ast.walk(handle)
        if isinstance(n, ast.If)
        and isinstance(n.test, ast.Compare)
        and isinstance(n.test.left, ast.Name)
        and n.test.left.id == "cmd_type"
        and isinstance(n.test.ops[0], ast.NotIn)
    ]
    assert len(guards) == 1
    assert ast.literal_eval(guards[0].comparators[0]) == ("ping", "status")


# acceptance ------------------------------------------------------------------


HANDLE_COMMAND_MAX_LINES = 30


def test_handle_command_stays_short():
    handle = _executor_methods()["handle_command"]
    assert handle.end_lineno is not None
    length = handle.end_lineno - handle.lineno + 1
    assert length <= HANDLE_COMMAND_MAX_LINES, (
        f"handle_command is {length} lines; dispatch belongs in COMMANDS"
    )


# build_command_table (runtime; ``_table.py`` is dependency-free) -------------


def test_build_command_table_resolves_arity():
    class Core:
        COMMANDS = {"a": "_with_params", "b": "_no_args"}

        def _with_params(self, params):
            return params

        def _no_args(self):
            return {}

    table = table_module.build_command_table(Core, ())
    assert table == {
        "a": table_module.CommandEntry("_with_params", True),
        "b": table_module.CommandEntry("_no_args", False),
    }


def test_build_command_table_rejects_missing_method():
    class Core:
        COMMANDS = {"a": "_nope"}

    with pytest.raises(ValueError, match="does not exist"):
        table_module.build_command_table(Core, ())


@pytest.mark.parametrize("decorator", [staticmethod, classmethod])
def test_build_command_table_rejects_a_decorated_handler(decorator):
    # Arity is read by dropping the first parameter as ``self``; a staticmethod or
    # classmethod handler would be misclassified as no-args and then called with none.
    class Core:
        COMMANDS = {"a": "_h"}
        _h = decorator(lambda *args: {})

    with pytest.raises(ValueError, match="not a plain method"):
        table_module.build_command_table(Core, ())


def test_build_command_table_rejects_duplicate_across_mixins():
    class Mixin:
        COMMANDS = {"a": "_m"}

        def _m(self):
            return {}

    class Core(Mixin):
        COMMANDS = {"a": "_m"}

    with pytest.raises(ValueError, match="declared by both Mixin and Core"):
        table_module.build_command_table(Core, (Mixin,))


def test_build_command_table_rejects_a_mixin_that_is_not_a_base():
    class Mixin:
        COMMANDS = {"m": "_m"}

        def _m(self):
            return {}

    class Core:
        COMMANDS: dict[str, str] = {}

    with pytest.raises(TypeError, match="not a base of Core"):
        table_module.build_command_table(Core, (Mixin,))


# the move into per-domain mixins (AST only; no mixin is imported) --------------

# ``_host.py`` declares ``ExecutorHost`` under ``if TYPE_CHECKING`` only (``object`` at
# run time), so its method stubs are not runtime definitions and are not counted.
PRE_MOVE_METHOD_COUNT = 165
QONTINUI_EXECUTOR_MAX_LINES = 1700
MIXIN_MAX_LINES = 1000


def _mixin_modules() -> list[Path]:
    """The per-domain mixin files: every package module whose name has no leading ``_``."""
    return [p for p in sorted(PACKAGE_DIR.glob("*.py")) if not p.name.startswith("_")]


def _init_tree() -> ast.Module:
    return ast.parse((PACKAGE_DIR / "__init__.py").read_text(encoding="utf-8"))


def _command_mixin_names() -> list[str]:
    """The names in ``COMMAND_MIXINS = (...)``, in order."""
    for node in _init_tree().body:
        target: ast.expr | None = None
        value: ast.expr | None = None
        if isinstance(node, ast.AnnAssign):
            target, value = node.target, node.value
        elif isinstance(node, ast.Assign) and len(node.targets) == 1:
            target, value = node.targets[0], node.value
        if isinstance(target, ast.Name) and target.id == "COMMAND_MIXINS":
            assert isinstance(value, ast.Tuple), "COMMAND_MIXINS is not a tuple literal"
            assert all(isinstance(e, ast.Name) for e in value.elts)
            return [e.id for e in value.elts if isinstance(e, ast.Name)]
    raise AssertionError("COMMAND_MIXINS not found in executor_commands/__init__.py")


def _runtime_classes() -> dict[str, tuple[str, ast.ClassDef]]:
    """``QontinuiExecutor`` plus every top-level class of a mixin module, by name."""
    executor = next(
        n
        for n in ast.parse(_source()).body
        if isinstance(n, ast.ClassDef) and n.name == "QontinuiExecutor"
    )
    classes = {"QontinuiExecutor": (EXECUTOR.name, executor)}
    for path in _mixin_modules():
        for node in ast.parse(path.read_text(encoding="utf-8")).body:
            if isinstance(node, ast.ClassDef):
                assert node.name not in classes, f"class {node.name} defined twice"
                classes[node.name] = (path.name, node)
    return classes


def _class_methods(cls: ast.ClassDef) -> list[str]:
    return [n.name for n in cls.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]


def _executor_and_mixins() -> list[tuple[str, str, ast.ClassDef]]:
    """(class, file, node) for QontinuiExecutor and each COMMAND_MIXINS entry."""
    classes = _runtime_classes()
    names = ["QontinuiExecutor", *_command_mixin_names()]
    missing = [n for n in names if n not in classes]
    assert not missing, f"COMMAND_MIXINS names classes no mixin module defines: {missing}"
    return [(name, *classes[name]) for name in names]


@pytest.mark.parametrize(
    "path",
    [EXECUTOR, *sorted(PACKAGE_DIR.glob("*.py"))],
    ids=lambda p: p.name,
)
def test_every_file_parses(path):
    ast.parse(path.read_text(encoding="utf-8"), filename=str(path))


def test_method_set_equals_pre_move_set():
    """The executor's methods, across it and its mixins, equal the pre-move class's."""
    expected = METHODS_SNAPSHOT.read_text(encoding="utf-8").splitlines()
    assert len(expected) == PRE_MOVE_METHOD_COUNT
    actual = sorted(m for _name, _file, cls in _executor_and_mixins() for m in _class_methods(cls))
    assert actual == expected, (
        f"missing: {sorted(set(expected) - set(actual))}; "
        f"added: {sorted(set(actual) - set(expected))}. "
        "Update tests/executor-methods.snapshot.txt only for an intended change."
    )


def test_no_method_is_defined_in_two_classes():
    """An MRO clash would silently pick one definition, so each name has one owner."""
    owners: dict[str, list[str]] = {}
    for name, file, cls in _executor_and_mixins():
        for method in _class_methods(cls):
            owners.setdefault(method, []).append(f"{file}:{name}")
    clashes = {m: where for m, where in owners.items() if len(where) > 1}
    assert not clashes, f"methods defined in more than one class: {clashes}"


def test_every_mixin_module_is_registered():
    """Each mixin module defines one class, and it is in COMMAND_MIXINS."""
    registered = _command_mixin_names()
    assert len(registered) == len(set(registered)), "COMMAND_MIXINS lists a mixin twice"
    for path in _mixin_modules():
        tree = ast.parse(path.read_text(encoding="utf-8"))
        classes = [n.name for n in tree.body if isinstance(n, ast.ClassDef)]
        assert len(classes) == 1, f"{path.name} should define exactly one mixin: {classes}"
        assert classes[0] in registered, f"{path.name}:{classes[0]} is not in COMMAND_MIXINS"
    assert len(registered) == len(_mixin_modules())


def test_every_mixin_is_imported_statically():
    """``from .<module> import <Mixin>`` for every COMMAND_MIXINS entry (PyInstaller, D3)."""
    imported: dict[str, str] = {}
    for node in _init_tree().body:
        if isinstance(node, ast.ImportFrom) and node.level == 1 and node.module:
            for alias in node.names:
                imported[alias.asname or alias.name] = node.module
    modules = {p.stem for p in _mixin_modules()}
    for name in _command_mixin_names():
        assert name in imported, f"{name} is not statically imported in __init__.py"
        assert imported[name] in modules, f"{name} is imported from .{imported[name]}"
    assert {imported[n] for n in _command_mixin_names()} == modules


def test_executor_bases_are_the_command_mixins_in_order():
    executor = _runtime_classes()["QontinuiExecutor"][1]
    assert [ast.unparse(b) for b in executor.bases] == _command_mixin_names()


def test_host_stub_is_object_at_run_time():
    """``ExecutorHost`` is a class only under TYPE_CHECKING, so it adds nothing to the MRO."""
    tree = ast.parse((PACKAGE_DIR / "_host.py").read_text(encoding="utf-8"))
    assert not [n for n in tree.body if isinstance(n, ast.ClassDef)]
    guard = next(n for n in tree.body if isinstance(n, ast.If))
    assert ast.unparse(guard.test) == "TYPE_CHECKING"
    assert [ast.unparse(s) for s in guard.orelse] == ["ExecutorHost = object"]


def test_executor_file_stays_under_budget():
    lines = len(_source().splitlines())
    assert lines <= QONTINUI_EXECUTOR_MAX_LINES, f"qontinui_executor.py is {lines} lines"


@pytest.mark.parametrize("path", _mixin_modules(), ids=lambda p: p.name)
def test_mixin_file_stays_under_budget(path):
    lines = len(path.read_text(encoding="utf-8").splitlines())
    assert lines <= MIXIN_MAX_LINES, f"{path.name} is {lines} lines"
