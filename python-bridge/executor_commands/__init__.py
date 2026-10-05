"""Command table for the long-lived ``qontinui_executor.py`` stdin/stdout dispatcher.

Each class that serves commands declares them in a class attribute::

    COMMANDS: ClassVar[dict[str, str]] = {"models_list": "_handle_models_list", ...}

mapping a command name (the string the Rust side passes to ``send_command``) to the name
of the method that handles it. ``build_command_table`` merges the ``COMMANDS`` of every
mixin in ``COMMAND_MIXINS`` with the executor class's own, and fails at import on a
duplicate name or a method that does not exist, so a typo can never reach run time.

``COMMAND_MIXINS`` is the composition point: per-domain mixin classes are added here and
listed as bases of ``QontinuiExecutor``. Every mixin must be imported statically in this
file, because the PyInstaller build finds modules by import analysis and a string import
would fail only in the frozen executable.

This is unrelated to ``handlers/dispatch.py``, which is the one-shot WS-bridge dispatcher
with a different protocol.
"""

from __future__ import annotations

import inspect
from collections.abc import Callable
from typing import NamedTuple

# Per-domain mixin classes, each carrying its own ``COMMANDS``. Empty until the handlers
# move out of qontinui_executor.py.
COMMAND_MIXINS: tuple[type, ...] = ()


class CommandEntry(NamedTuple):
    """One resolved command: the handler's method name and whether it takes ``params``."""

    method: str
    takes_params: bool


def _takes_params(func: Callable[..., object]) -> bool:
    """True when the (unbound) method accepts a positional argument besides ``self``."""
    parameters = list(inspect.signature(func).parameters.values())[1:]
    return any(
        p.kind
        in (
            inspect.Parameter.POSITIONAL_ONLY,
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.VAR_POSITIONAL,
        )
        for p in parameters
    )


def build_command_table(cls: type) -> dict[str, CommandEntry]:
    """Merge the ``COMMANDS`` of ``COMMAND_MIXINS`` and ``cls`` into one validated table.

    Raises:
        TypeError: a mixin in ``COMMAND_MIXINS`` is not a base of ``cls``.
        ValueError: a command name is declared twice, or names a method ``cls`` lacks.
    """
    table: dict[str, CommandEntry] = {}
    owner: dict[str, str] = {}
    for source in (*COMMAND_MIXINS, cls):
        if source is not cls and source not in cls.__mro__:
            raise TypeError(
                f"{source.__name__} is in COMMAND_MIXINS but not a base of {cls.__name__}"
            )
        # The class's own dict, not getattr: a subclass must not re-read a base's COMMANDS.
        commands: dict[str, str] = vars(source).get("COMMANDS", {})
        for name, method_name in commands.items():
            if name in table:
                raise ValueError(
                    f"command {name!r} is declared by both {owner[name]} and {source.__name__}"
                )
            method = getattr(cls, method_name, None)
            if method is None or not callable(method):
                raise ValueError(
                    f"command {name!r} names {source.__name__}.{method_name}, which does not exist"
                )
            table[name] = CommandEntry(method_name, _takes_params(method))
            owner[name] = source.__name__
    return table


__all__ = ["COMMAND_MIXINS", "CommandEntry", "build_command_table"]
