#!/usr/bin/env python3
"""Print the executor's command table as sorted ``name -> method (params|noargs)`` lines.

Reads ``qontinui_executor.py`` with ``ast`` only, so it needs neither the ``qontinui``
library nor any GPU dependency, and never imports the executor.

It understands both shapes the dispatcher has had:

- **The old if/elif chain** in ``QontinuiExecutor.handle_command``. A branch whose body is
  a single ``return self._handle_X(...)`` maps to ``_handle_X``, tagged ``params`` when the
  call passes an argument and ``noargs`` when it does not. Any other (inline) branch maps
  to ``_cmd_<name> (params)``, the core method that branch body became.
- **The command table.** ``COMMANDS`` dicts on ``QontinuiExecutor`` and on every class in
  the sibling ``executor_commands/`` package, with each method's arity read from its
  ``def`` (``params`` when it takes an argument besides ``self``).

Usage::

    python scripts/snapshot_executor_commands.py                       # current source
    python scripts/snapshot_executor_commands.py path/to/qontinui_executor.py
    git show HEAD~1:python-bridge/qontinui_executor.py | \\
        python scripts/snapshot_executor_commands.py -                 # an old revision
    python scripts/snapshot_executor_commands.py --output tests/executor-commands.snapshot.txt

Reading stdin (``-``) parses only that one file, so it suits the old single-file chain;
a table-form revision whose mixins live in ``executor_commands/`` needs a checkout.
"""

from __future__ import annotations

import argparse
import ast
import sys
from pathlib import Path

BRIDGE_DIR = Path(__file__).resolve().parent.parent
DEFAULT_SOURCE = BRIDGE_DIR / "qontinui_executor.py"
EXECUTOR_CLASS = "QontinuiExecutor"
DISPATCH_METHOD = "handle_command"
DISPATCH_VAR = "cmd_type"


class SnapshotError(RuntimeError):
    """The source does not have a shape this script can read."""


def _find_class(tree: ast.Module, name: str) -> ast.ClassDef:
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == name:
            return node
    raise SnapshotError(f"class {name} not found")


def _methods(cls: ast.ClassDef) -> dict[str, ast.FunctionDef]:
    return {n.name: n for n in cls.body if isinstance(n, ast.FunctionDef)}


def _takes_params(func: ast.FunctionDef) -> bool:
    """True when the method accepts a positional argument besides ``self``."""
    positional = [*func.args.posonlyargs, *func.args.args]
    return len(positional) > 1 or func.args.vararg is not None


def _dispatch_name(test: ast.expr) -> str | None:
    """``cmd_type == "x"`` -> ``"x"``; anything else -> None."""
    if (
        isinstance(test, ast.Compare)
        and isinstance(test.left, ast.Name)
        and test.left.id == DISPATCH_VAR
        and len(test.ops) == 1
        and isinstance(test.ops[0], ast.Eq)
        and len(test.comparators) == 1
        and isinstance(test.comparators[0], ast.Constant)
        and isinstance(test.comparators[0].value, str)
    ):
        return test.comparators[0].value
    return None


def _delegate(body: list[ast.stmt]) -> tuple[str, bool] | None:
    """A body that is exactly ``return self._handle_X(...)`` -> (method, passes_args)."""
    if len(body) != 1 or not isinstance(body[0], ast.Return):
        return None
    call = body[0].value
    if (
        isinstance(call, ast.Call)
        and isinstance(call.func, ast.Attribute)
        and isinstance(call.func.value, ast.Name)
        and call.func.value.id == "self"
        and call.func.attr.startswith("_handle_")
    ):
        return call.func.attr, bool(call.args or call.keywords)
    return None


def table_from_if_chain(dispatch: ast.FunctionDef) -> dict[str, tuple[str, bool]] | None:
    """Read the old chain. Returns None when ``handle_command`` holds no chain."""
    chain: ast.If | None = None
    for stmt in dispatch.body:
        if isinstance(stmt, ast.If) and _dispatch_name(stmt.test) is not None:
            chain = stmt
            break
    if chain is None:
        return None

    table: dict[str, tuple[str, bool]] = {}
    node: ast.stmt | None = chain
    while isinstance(node, ast.If):
        name = _dispatch_name(node.test)
        if name is None:
            raise SnapshotError(f"unreadable branch test at line {node.lineno}")
        if name in table:
            raise SnapshotError(f"duplicate command {name!r} in the if-chain")
        delegate = _delegate(node.body)
        table[name] = delegate if delegate is not None else (f"_cmd_{name}", True)
        node = node.orelse[0] if len(node.orelse) == 1 else None
    return table


def _commands_dict(cls: ast.ClassDef) -> list[tuple[str, str]] | None:
    """The ``COMMANDS = {...}`` (or annotated) class attribute, as ordered pairs."""
    for stmt in cls.body:
        target: ast.expr | None = None
        value: ast.expr | None = None
        if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1:
            target, value = stmt.targets[0], stmt.value
        elif isinstance(stmt, ast.AnnAssign):
            target, value = stmt.target, stmt.value
        if isinstance(target, ast.Name) and target.id == "COMMANDS":
            if not isinstance(value, ast.Dict):
                raise SnapshotError(f"{cls.name}.COMMANDS is not a dict literal")
            pairs = []
            for key, val in zip(value.keys, value.values, strict=True):
                if not (
                    isinstance(key, ast.Constant)
                    and isinstance(key.value, str)
                    and isinstance(val, ast.Constant)
                    and isinstance(val.value, str)
                ):
                    raise SnapshotError(f"{cls.name}.COMMANDS has a non-string entry")
                pairs.append((key.value, val.value))
            return pairs
    return None


def command_pairs(source: str, package_dir: Path | None) -> list[tuple[str, str, str]]:
    """Every ``(class, command, method)`` declared in the table form, in source order.

    Duplicates are kept, so a caller can detect them.
    """
    tree = ast.parse(source)
    classes = [_find_class(tree, EXECUTOR_CLASS)]
    if package_dir is not None and package_dir.is_dir():
        for path in sorted(package_dir.glob("*.py")):
            mod = ast.parse(path.read_text(encoding="utf-8"))
            classes.extend(n for n in mod.body if isinstance(n, ast.ClassDef))
    out = []
    for cls in classes:
        for name, method in _commands_dict(cls) or []:
            out.append((cls.name, name, method))
    return out


def table_from_commands(source: str, package_dir: Path | None) -> dict[str, tuple[str, bool]]:
    """Read the table form: ``COMMANDS`` dicts on the executor and the mixin package."""
    tree = ast.parse(source)
    methods = _methods(_find_class(tree, EXECUTOR_CLASS))
    if package_dir is not None and package_dir.is_dir():
        for path in sorted(package_dir.glob("*.py")):
            mod = ast.parse(path.read_text(encoding="utf-8"))
            for cls in (n for n in mod.body if isinstance(n, ast.ClassDef)):
                methods.update(_methods(cls))

    table: dict[str, tuple[str, bool]] = {}
    for cls_name, name, method in command_pairs(source, package_dir):
        if name in table:
            raise SnapshotError(f"duplicate command {name!r} (second in {cls_name})")
        func = methods.get(method)
        if func is None:
            raise SnapshotError(f"{name!r} -> {method}: no such method")
        table[name] = (method, _takes_params(func))
    return table


def extract_table(source: str, package_dir: Path | None = None) -> dict[str, tuple[str, bool]]:
    """The command table from either dispatcher shape."""
    tree = ast.parse(source)
    dispatch = _methods(_find_class(tree, EXECUTOR_CLASS)).get(DISPATCH_METHOD)
    if dispatch is None:
        raise SnapshotError(f"{EXECUTOR_CLASS}.{DISPATCH_METHOD} not found")
    chain = table_from_if_chain(dispatch)
    if chain is not None:
        return chain
    return table_from_commands(source, package_dir)


def render(table: dict[str, tuple[str, bool]]) -> str:
    lines = [
        f"{name} -> {method} ({'params' if takes else 'noargs'})"
        for name, (method, takes) in sorted(table.items())
    ]
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "source",
        nargs="?",
        default=str(DEFAULT_SOURCE),
        help="qontinui_executor.py to read, or '-' for stdin (default: the checked-in one)",
    )
    parser.add_argument("--output", help="write here instead of stdout")
    args = parser.parse_args(argv)

    if args.source == "-":
        source = sys.stdin.buffer.read().decode("utf-8")
        package_dir = None
    else:
        path = Path(args.source)
        source = path.read_text(encoding="utf-8")
        package_dir = path.resolve().parent / "executor_commands"

    text = render(extract_table(source, package_dir))
    if args.output:
        Path(args.output).write_text(text, encoding="utf-8", newline="\n")
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
