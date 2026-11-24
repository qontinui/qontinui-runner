"""Command table and per-domain command mixins for ``qontinui_executor.py``.

Each class that serves commands declares them in a class attribute::

    COMMANDS: ClassVar[dict[str, str]] = {"models_list": "_handle_models_list", ...}

mapping a command name (the string the Rust side passes to ``send_command``) to the name
of the method that handles it. ``build_command_table`` merges the ``COMMANDS`` of every
mixin in ``COMMAND_MIXINS`` with the executor class's own, and fails at import on a
duplicate name or a method that does not exist, so a typo can never reach run time.

``COMMAND_MIXINS`` is the composition point: each per-domain mixin is listed here and as
a base of ``QontinuiExecutor``. Adding a command touches one mixin file. Every mixin must
be imported statically in this file, because the PyInstaller build finds modules by
import analysis and a string import would fail only in the frozen executable.

Importing this package runs ``_shared``, which owns the optional ML-service imports and
the ``qontinui`` availability check.

This is unrelated to ``handlers/dispatch.py``, which is the one-shot WS-bridge dispatcher
with a different protocol.
"""

from __future__ import annotations

from . import _table
from ._table import CommandEntry
from .accessibility import AccessibilityCommands
from .ai_generate import AiGenerateCommands
from .awas import AwasCommands
from .capture import CaptureCommands
from .click_capture import ClickCaptureCommands
from .extraction import ExtractionCommands
from .gui_config import GuiConfigCommands
from .models import ModelCommands
from .recording import RecordingCommands
from .state_machine import StateMachineCommands
from .testing import TestingCommands
from .ui_bridge_discovery import UiBridgeDiscoveryCommands

# Per-domain mixin classes, each carrying its own ``COMMANDS``. ``QontinuiExecutor``
# lists exactly these as its bases, in this order.
COMMAND_MIXINS: tuple[type, ...] = (
    TestingCommands,
    StateMachineCommands,
    ExtractionCommands,
    CaptureCommands,
    UiBridgeDiscoveryCommands,
    AccessibilityCommands,
    AiGenerateCommands,
    ClickCaptureCommands,
    GuiConfigCommands,
    RecordingCommands,
    ModelCommands,
    AwasCommands,
)


def build_command_table(cls: type) -> dict[str, CommandEntry]:
    """Merge the ``COMMANDS`` of ``COMMAND_MIXINS`` and ``cls`` into one validated table.

    Raises:
        TypeError: a mixin in ``COMMAND_MIXINS`` is not a base of ``cls``.
        ValueError: a command name is declared twice, or names a method ``cls`` lacks.
    """
    return _table.build_command_table(cls, COMMAND_MIXINS)


__all__ = [
    "COMMAND_MIXINS",
    "AccessibilityCommands",
    "AiGenerateCommands",
    "AwasCommands",
    "CaptureCommands",
    "ClickCaptureCommands",
    "CommandEntry",
    "ExtractionCommands",
    "GuiConfigCommands",
    "ModelCommands",
    "RecordingCommands",
    "StateMachineCommands",
    "TestingCommands",
    "UiBridgeDiscoveryCommands",
    "build_command_table",
]
