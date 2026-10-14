"""``build_command_table``: merge and validate the executor's ``COMMANDS`` declarations.

Dependency-free (standard library only), so the tests can load this file on its own
without importing the mixins and the services they pull in.
"""

from __future__ import annotations

import inspect
from collections.abc import Callable, Iterable
from typing import NamedTuple


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


def build_command_table(cls: type, mixins: Iterable[type]) -> dict[str, CommandEntry]:
    """Merge the ``COMMANDS`` of ``mixins`` and ``cls`` into one validated table.

    Raises:
        TypeError: a mixin is not a base of ``cls``.
        ValueError: a command name is declared twice, or names a method ``cls`` lacks.
    """
    table: dict[str, CommandEntry] = {}
    owner: dict[str, str] = {}
    for source in (*mixins, cls):
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
            # ``_takes_params`` drops the first parameter as ``self``, which is only right for
            # a plain function. A staticmethod (no ``self``) or classmethod (``cls`` already
            # bound) would be misread as taking no params and then called with none.
            raw = inspect.getattr_static(cls, method_name)
            if not inspect.isfunction(raw):
                raise ValueError(
                    f"command {name!r} names {source.__name__}.{method_name}, which is a "
                    f"{type(raw).__name__}, not a plain method"
                )
            table[name] = CommandEntry(method_name, _takes_params(method))
            owner[name] = source.__name__
    return table
