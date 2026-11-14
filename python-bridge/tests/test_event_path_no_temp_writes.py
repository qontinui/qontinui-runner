"""The bridge's event path writes nothing into the OS temp dir.

Plan 2026-11-14-one-xlib-distribution-and-no-tmp-debug-logs-in-the-python-bridge,
Phase 4. ``EventTranslator.on_match_attempted``, ``EventManager.emit_tree_event``
and ``QontinuiExecutor.start_execution`` used to append debug lines under
``tempfile.gettempdir()`` / ``$TEMP`` on every call; ``emit_tree_event`` did so with
no try/except, so a read-only temp dir raised out of the event path.

These tests run in the python-executor-command-table CI job, whose venv holds only
pytest, numpy and Pillow, so they import nothing heavy:

- ``event_manager`` is stdlib-only and is imported as is;
- ``event_translator`` is loaded under a private name with ``services`` and
  ``qontinui`` stubbed in ``sys.modules`` for the duration of the load only
  (``patch.dict`` restores the real table), and ``QONTINUI_AVAILABLE`` forced on;
- ``start_execution`` is compiled on its own from ``qontinui_executor.py``'s source
  and run against stub globals, because importing that module writes the READY
  line to stdout and pulls in the whole executor.
"""

from __future__ import annotations

import ast
import datetime
import importlib.util
import os
import sys
import tempfile
import time
from pathlib import Path
from types import ModuleType, SimpleNamespace
from unittest.mock import Mock, patch

import pytest

BRIDGE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BRIDGE))

from event_manager import EventManager  # noqa: E402  (stdlib-only module)


@pytest.fixture
def isolated_temp_dir(tmp_path, monkeypatch):
    """Point every temp-dir source at an empty directory.

    Covers ``tempfile.gettempdir()`` (via ``tempfile.tempdir``) and the
    ``TEMP``/``TMP``/``TMPDIR`` environment names the deleted writes read.
    """
    temp = tmp_path / "os-temp"
    temp.mkdir()
    monkeypatch.setattr(tempfile, "tempdir", str(temp))
    for name in ("TEMP", "TMP", "TMPDIR"):
        monkeypatch.setenv(name, str(temp))
    return temp


def _load_event_translator() -> ModuleType:
    services = ModuleType("services")
    services.__path__ = []  # a package, so the submodule stub resolves under it
    collector = ModuleType("services.unified_data_collector")
    collector.UnifiedDataCollector = object  # type: ignore[attr-defined]
    stubs = {
        "services": services,
        "services.unified_data_collector": collector,
        # None in sys.modules makes `import qontinui...` raise ImportError, which
        # event_translator's guarded import turns into QONTINUI_AVAILABLE = False.
        "qontinui": None,
        "qontinui.reporting": None,
    }
    with patch.dict(sys.modules, stubs):
        spec = importlib.util.spec_from_file_location(
            "_event_translator_light", BRIDGE / "event_translator.py"
        )
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    module.QONTINUI_AVAILABLE = True  # type: ignore[attr-defined]
    return module


def _load_start_execution(extra_globals: dict) -> object:
    """``QontinuiExecutor.start_execution`` compiled alone against ``extra_globals``."""
    tree = ast.parse((BRIDGE / "qontinui_executor.py").read_text(encoding="utf-8"))
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "QontinuiExecutor")
    func = next(
        n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name == "start_execution"
    )
    module = ast.fix_missing_locations(ast.Module(body=[func], type_ignores=[]))
    namespace: dict = {
        # What the base source's deleted debug blocks used, so running the
        # pre-fix function fails on its assertion rather than on a NameError.
        "os": os,
        "tempfile": tempfile,
        "datetime": datetime.datetime,
        "time": time,
        **extra_globals,
    }
    exec(compile(module, "qontinui_executor.py", "exec"), namespace)
    return namespace["start_execution"]


def _match_event(sample: dict) -> SimpleNamespace:
    # on_match_attempted reads only ``event.data``.
    return SimpleNamespace(data=sample)


class _Node:
    """The slice of an execution-tree node that ``emit_tree_event`` reads."""

    id = "n1"
    status = "running"
    node_type = "action"

    def to_dict(self, include_children: bool = False) -> dict:
        return {"id": self.id, "status": self.status}

    def get_depth(self) -> int:
        return 0

    def get_path_from_root(self) -> list[str]:
        return [self.id]


def test_on_match_attempted_writes_nothing_to_temp_dir(isolated_temp_dir):
    module = _load_event_translator()
    emitter = Mock()
    translator = module.EventTranslator(emitter=emitter, collector=None)

    translator.on_match_attempted(
        _match_event(
            {
                "image_id": "button.png",
                "similarity_threshold": 0.8,
                "best_match_confidence": 0.95,
                "best_match_location": {"x": 1, "y": 2},
                "threshold_passed": True,
            }
        )
    )

    emitter.assert_called_once()
    assert emitter.call_args[0][0] == "image_recognition"
    assert list(isolated_temp_dir.iterdir()) == []


def test_emit_tree_event_writes_nothing_to_temp_dir(isolated_temp_dir, capsys):
    EventManager().emit_tree_event("action_started", _Node())

    assert list(isolated_temp_dir.iterdir()) == []
    assert '"event_type": "action_started"' in capsys.readouterr().out


@pytest.mark.skipif(
    sys.platform == "win32" or (hasattr(os, "geteuid") and os.geteuid() == 0),
    reason="directory modes do not make a dir read-only on Windows or for root",
)
def test_emit_tree_event_survives_read_only_temp_dir(isolated_temp_dir, capsys):
    isolated_temp_dir.chmod(0o555)
    try:
        EventManager().emit_tree_event("action_started", _Node())
    finally:
        isolated_temp_dir.chmod(0o755)

    assert '"event_type": "action_started"' in capsys.readouterr().out


def test_start_execution_without_state_executor_warns_and_writes_nothing(isolated_temp_dir):
    settings = SimpleNamespace(monitor=SimpleNamespace(default_screen_index=None))
    start_execution = _load_start_execution(
        {
            "QONTINUI_AVAILABLE": True,
            "get_settings": lambda: settings,
            "threading": SimpleNamespace(Thread=lambda **_: Mock()),
            "EventType": SimpleNamespace(EXECUTION_STARTED="execution_started"),
        }
    )
    executor = SimpleNamespace(
        event_manager=Mock(),
        is_running=False,
        gui_automation=Mock(),
        executor_core=None,
        capture_input_for_validation=False,
        _run_workflow=Mock(),
    )

    assert start_execution(executor, "wf-1", monitor=1) is True

    assert settings.monitor.default_screen_index == 1
    assert (
        "warning",
        "Cannot set monitor - executor_core or state_executor is None",
    ) in [c.args for c in executor.event_manager.emit_log.call_args_list]
    assert list(isolated_temp_dir.iterdir()) == []
