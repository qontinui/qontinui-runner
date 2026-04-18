"""Unit tests: WSM success_source stamping in TrajectoryLogger.

These tests verify that the dynamic-capture path correctly:

1. Calls ``self._wsm_client.verify(...)`` when one is configured.
2. Stamps ``GroundingAction.success_source`` from the verdict's
   ``source`` field (``"wsm"`` or ``"pixel_diff"``).
3. Falls through to the pixel-diff heuristic when WSM raises.
4. Allows instantiation with ``wsm_enabled=False`` without touching the
   WSM stack.

We stub the ``qontinui_train.export.grounding_record`` module because it
lives in an optional Poetry group; these are meant to run as fast unit
tests inside the ``python-bridge`` suite without the ML group installed.
"""

from __future__ import annotations

import sys
import time
import types
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import numpy as np

# ---------------------------------------------------------------------------
# Stub qontinui_train.export.grounding_record so trajectory_logger imports.
# The real module lives in the optional ML poetry group.
# ---------------------------------------------------------------------------


def _install_grounding_stub() -> None:
    if "qontinui_train.export.grounding_record" in sys.modules:
        return

    mod = types.ModuleType("qontinui_train.export.grounding_record")

    @dataclass
    class GroundingElement:
        role: str
        text: str | None
        bbox: tuple[int, int, int, int]
        interactable: bool | None = None

    @dataclass
    class GroundingAction:
        type: str
        target_bbox: tuple[int, int, int, int] | None = None
        typed_text: str | None = None
        success: bool | None = None
        success_source: str | None = None

    @dataclass
    class GroundingRecord:
        image_hash: str
        image_path: str
        viewport_width: int
        viewport_height: int
        elements: list = field(default_factory=list)
        action: Any = None
        source: str = "dynamic"
        timestamp: str = ""
        session_id: str | None = None
        phash: str | None = None
        metadata: Any = None

    class GroundingJSONLWriter:
        def __init__(self, output_dir: Path) -> None:
            self.output_dir = Path(output_dir)
            self.written_records: list[GroundingRecord] = []

        def write(self, record: GroundingRecord, image_bytes: bytes | None = None) -> bool:
            self.written_records.append(record)
            return True

    mod.GroundingElement = GroundingElement
    mod.GroundingAction = GroundingAction
    mod.GroundingRecord = GroundingRecord
    mod.GroundingJSONLWriter = GroundingJSONLWriter

    # Ensure parent package entries exist for the submodule lookup chain.
    if "qontinui_train" not in sys.modules:
        sys.modules["qontinui_train"] = types.ModuleType("qontinui_train")
    if "qontinui_train.export" not in sys.modules:
        sys.modules["qontinui_train.export"] = types.ModuleType("qontinui_train.export")
    sys.modules["qontinui_train.export.grounding_record"] = mod


_install_grounding_stub()

# Now we can import TrajectoryLogger.
# And grab the stub types (the logger holds the same refs).
from qontinui_train.export.grounding_record import GroundingAction  # noqa: E402

from services.trajectory_logger import TrajectoryLogger  # noqa: E402

# ---------------------------------------------------------------------------
# Fake WSMClient + verdict doubles
# ---------------------------------------------------------------------------


class _FakeVerdict:
    def __init__(self, success: bool, source: str, confidence: float = 0.9, reason: str = "ok") -> None:
        self.success = success
        self.source = source
        self.confidence = confidence
        self.reason = reason


def _make_record(**overrides: Any):
    """Build a minimal ActionExecutionRecord for the logger."""
    from models.action_execution_record import ActionExecutionRecord

    defaults: dict[str, Any] = {
        "action_id": "act-001",
        "action_type": "CLICK",
        "config": {"x": 100, "y": 200},
        "start_time": time.time(),
        "end_time": time.time() + 0.05,
        "duration_ms": 50.0,
        "active_states_before": frozenset(),
        "active_states_after": frozenset(),
        "success": True,
        "clicked_location": (100, 200),
        "workflow_id": "wf",
        "nesting_level": 0,
        "metadata": {},
    }
    defaults.update(overrides)
    return ActionExecutionRecord(**defaults)


def _make_logger(tmp_path: Path, wsm_client: Any | None, wsm_enabled: bool = True) -> TrajectoryLogger:
    return TrajectoryLogger(
        output_dir=tmp_path,
        max_records_per_session=10,
        wsm_client=wsm_client,
        omniparser_detector=None,
        wsm_enabled=wsm_enabled,
    )


# ---------------------------------------------------------------------------
# Tests — direct _compute_success_label
# ---------------------------------------------------------------------------


def test_wsm_verdict_success_source_is_wsm(tmp_path: Path) -> None:
    """A WSMVerdict with source='wsm' stamps success_source='wsm'."""
    fake_client = MagicMock()
    fake_client.verify = AsyncMock(
        return_value=_FakeVerdict(success=True, source="wsm", confidence=0.9)
    )

    logger = _make_logger(tmp_path, fake_client)
    record = _make_record()
    pre = b"\x89PNG\r\n\x1a\nPRE"
    post = b"\x89PNG\r\n\x1a\nPOST"
    pre_arr = np.zeros((10, 10, 3), dtype=np.uint8)
    post_arr = np.zeros((10, 10, 3), dtype=np.uint8)

    success, source = logger._compute_success_label(
        record, pre, pre_arr, post, post_arr
    )

    assert success is True
    assert source == "wsm"
    fake_client.verify.assert_awaited_once()


def test_wsm_verdict_source_pixel_diff_is_propagated(tmp_path: Path) -> None:
    """When WSMVerdict.source='pixel_diff' (low-confidence fallback inside
    the client), TrajectoryLogger stamps 'pixel_diff' rather than 'wsm'."""
    fake_client = MagicMock()
    fake_client.verify = AsyncMock(
        return_value=_FakeVerdict(success=False, source="pixel_diff", confidence=0.2)
    )

    logger = _make_logger(tmp_path, fake_client)
    record = _make_record()
    pre = b"PRE"
    post = b"POST"
    pre_arr = np.zeros((10, 10, 3), dtype=np.uint8)
    post_arr = np.zeros((10, 10, 3), dtype=np.uint8)

    success, source = logger._compute_success_label(
        record, pre, pre_arr, post, post_arr
    )

    assert success is False
    assert source == "pixel_diff"


def test_wsm_exception_falls_through_to_pixel_diff(tmp_path: Path) -> None:
    """If WSM raises, fall through to the in-logger pixel-diff heuristic."""
    fake_client = MagicMock()
    fake_client.verify = AsyncMock(side_effect=RuntimeError("wsm unreachable"))

    logger = _make_logger(tmp_path, fake_client)
    record = _make_record(clicked_location=(5, 5))

    pre = b"PRE"
    post = b"POST"
    # Two visibly different arrays so pixel diff registers a change.
    pre_arr = np.zeros((50, 50, 3), dtype=np.uint8)
    post_arr = np.full((50, 50, 3), 255, dtype=np.uint8)

    success, source = logger._compute_success_label(
        record, pre, pre_arr, post, post_arr
    )

    assert source == "pixel_diff"
    # Arrays differ by 255 per channel ⇒ mean diff > threshold ⇒ success True
    assert success is True


def test_legacy_client_with_status_field_still_works(tmp_path: Path) -> None:
    """Legacy clients return a sync verdict whose ``.status`` field is the
    label carrier (no ``.source``). The logger must stamp 'wsm' and derive
    success from ``status in {"pass","partial"}``."""

    class LegacyVerdict:
        def __init__(self, status: str) -> None:
            self.status = status

    class LegacyClient:
        def verify(self, pre: bytes, post: bytes, intent: str) -> LegacyVerdict:
            return LegacyVerdict(status="pass")

    logger = _make_logger(tmp_path, LegacyClient())
    record = _make_record()
    pre = b"PRE"
    post = b"POST"
    pre_arr = np.zeros((10, 10, 3), dtype=np.uint8)
    post_arr = np.zeros((10, 10, 3), dtype=np.uint8)

    success, source = logger._compute_success_label(
        record, pre, pre_arr, post, post_arr
    )

    assert success is True
    assert source == "wsm"


# ---------------------------------------------------------------------------
# Tests — construction + env gating
# ---------------------------------------------------------------------------


def test_disabled_wsm_does_not_instantiate_client(tmp_path: Path) -> None:
    """wsm_enabled=False → no client, even if the canonical one is available."""
    logger = TrajectoryLogger(
        output_dir=tmp_path,
        max_records_per_session=5,
        wsm_client=None,
        wsm_enabled=False,
    )
    assert logger._wsm_client is None


def test_disabled_wsm_ignores_explicit_client(tmp_path: Path) -> None:
    """wsm_enabled=False wins even if a client is passed in explicitly."""
    fake_client = MagicMock()
    logger = TrajectoryLogger(
        output_dir=tmp_path,
        max_records_per_session=5,
        wsm_client=fake_client,
        wsm_enabled=False,
    )
    assert logger._wsm_client is None


def test_enabled_wsm_lazy_constructs_default_client(tmp_path: Path) -> None:
    """wsm_enabled=True with no client → construct the canonical WSMClient."""
    sentinel = object()
    with patch(
        "services.trajectory_logger._CanonicalWSMClient",
        return_value=sentinel,
    ) as mock_ctor, patch(
        "services.trajectory_logger._WSM_AVAILABLE", True
    ):
        logger = TrajectoryLogger(
            output_dir=tmp_path,
            max_records_per_session=5,
            wsm_client=None,
            wsm_enabled=True,
        )
        mock_ctor.assert_called_once_with()
        assert logger._wsm_client is sentinel


def test_lazy_construction_failure_is_non_fatal(tmp_path: Path) -> None:
    """If lazy WSMClient construction raises, the logger still initialises."""
    with patch(
        "services.trajectory_logger._CanonicalWSMClient",
        side_effect=RuntimeError("bad endpoint"),
    ), patch("services.trajectory_logger._WSM_AVAILABLE", True):
        logger = TrajectoryLogger(
            output_dir=tmp_path,
            max_records_per_session=5,
            wsm_client=None,
            wsm_enabled=True,
        )
        assert logger._wsm_client is None


# ---------------------------------------------------------------------------
# Integration-lite: GroundingAction.success_source ends up stamped correctly.
# ---------------------------------------------------------------------------


def test_grounding_action_success_source_is_stamped(tmp_path: Path) -> None:
    """Drive the full _write_record path and assert the GroundingAction
    written to the JSONL writer carries success_source='wsm'."""
    fake_client = MagicMock()
    fake_client.verify = AsyncMock(
        return_value=_FakeVerdict(success=True, source="wsm", confidence=0.95)
    )

    logger = _make_logger(tmp_path, fake_client)

    # Seed pre-state as if on_action_start() ran.
    logger._pre_png_bytes = b"PRE"
    logger._pre_array = np.zeros((100, 100, 3), dtype=np.uint8)

    record = _make_record()
    post_bytes = b"POST"
    post_array = np.zeros((100, 100, 3), dtype=np.uint8)

    logger._write_record(record, post_bytes, post_array)

    written = logger._writer.written_records  # type: ignore[attr-defined]
    assert len(written) == 1
    action: GroundingAction = written[0].action
    assert action.success_source == "wsm"
    assert action.success is True
