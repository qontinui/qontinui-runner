"""Guards for the executor's command table (``QontinuiExecutor.COMMANDS``).

The executor is read as source with ``ast`` (and Rust with a regex); nothing imports
``qontinui_executor``, so these run without the ``qontinui`` library or any GPU
dependency installed. Only the dependency-free ``executor_commands`` package is imported.

What the Rust-sender guard (``test_every_rust_sent_command_is_handled``) cannot see, by
construction:

- ``src-tauri/src/mcp/gui_execution.rs`` (~:860) forwards a caller-chosen ``cmd_type``
  taken from an HTTP request body; there is no literal to check.
- ``src-tauri/src/mcp/testing.rs`` (~:399) sends a variable whose values
  (``testing_mock_click`` / ``testing_mock_type`` / ``testing_mock_screenshot``) are tuple
  literals in a ``match``, not call arguments. All three are in the table already.

Any other sender that passes a non-literal command name is likewise out of reach.
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
RUST_SRC = BRIDGE_DIR.parent / "src-tauri" / "src"

_FOLLOW_UP = (
    "follow-up: delete or route "
    "(plan 2026-10-04-runner-python-executor-routes-118-commands-through-one-if-chain)"
)

# Command names the Rust side sends that no Python dispatcher handles; each returns
# ``Unknown command`` today. Remove an entry when its sender is deleted or routed.
KNOWN_UNHANDLED: dict[str, str] = {
    # src-tauri/src/commands/state_machine.rs
    "execute_transition": _FOLLOW_UP,
    "navigate_to_multiple_states": _FOLLOW_UP,
    "get_active_states": _FOLLOW_UP,
    "get_available_transitions": _FOLLOW_UP,
    # src-tauri/src/mcp/ai_generation.rs
    "generate_macro_with_ai": _FOLLOW_UP,
    "generate_prompt_snippet_with_ai": _FOLLOW_UP,
    "suggest_check_groups_with_ai": _FOLLOW_UP,
}

# Newline-tolerant: ``\s*`` spans the line break when the literal sits on the next line.
_SEND_RE = re.compile(r'send_command(?:_and_wait)?\(\s*"([^"]+)"')


def _load_snapshot_script():
    path = BRIDGE_DIR / "scripts" / "snapshot_executor_commands.py"
    spec = importlib.util.spec_from_file_location("snapshot_executor_commands", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


snapshot_script = _load_snapshot_script()


def _source() -> str:
    return EXECUTOR.read_text(encoding="utf-8")


def _executor_methods() -> dict[str, ast.FunctionDef]:
    tree = ast.parse(_source())
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "QontinuiExecutor")
    return {n.name: n for n in cls.body if isinstance(n, ast.FunctionDef)}


def _executor_table() -> dict[str, tuple[str, bool]]:
    return snapshot_script.extract_table(_source(), BRIDGE_DIR / "executor_commands")


def rust_sent_commands(text: str) -> set[str]:
    """Literal first arguments of ``send_command[_and_wait](`` calls in ``text``."""
    return set(_SEND_RE.findall(text))


def _all_rust_sent_commands() -> set[str]:
    sent: set[str] = set()
    for path in RUST_SRC.rglob("*.rs"):
        sent |= rust_sent_commands(path.read_text(encoding="utf-8", errors="replace"))
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


# build_command_table (runtime; executor_commands is dependency-free) ----------


def test_build_command_table_resolves_arity():
    import executor_commands

    class Core:
        COMMANDS = {"a": "_with_params", "b": "_no_args"}

        def _with_params(self, params):
            return params

        def _no_args(self):
            return {}

    table = executor_commands.build_command_table(Core)
    assert table == {
        "a": executor_commands.CommandEntry("_with_params", True),
        "b": executor_commands.CommandEntry("_no_args", False),
    }


def test_build_command_table_rejects_missing_method():
    import executor_commands

    class Core:
        COMMANDS = {"a": "_nope"}

    with pytest.raises(ValueError, match="does not exist"):
        executor_commands.build_command_table(Core)


def test_build_command_table_rejects_duplicate_across_mixins(monkeypatch):
    import executor_commands

    class Mixin:
        COMMANDS = {"a": "_m"}

        def _m(self):
            return {}

    class Core(Mixin):
        COMMANDS = {"a": "_m"}

    monkeypatch.setattr(executor_commands, "COMMAND_MIXINS", (Mixin,))
    with pytest.raises(ValueError, match="declared by both Mixin and Core"):
        executor_commands.build_command_table(Core)
