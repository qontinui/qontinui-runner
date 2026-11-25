"""An integration test run is judged by the results it recorded.

``IntegrationTestingService.end_test_run`` folded ``all()`` over the run's recorded
test results, which is True over an empty list, so a run that recorded nothing ended
``passed``. Nothing records a result any more (``testing_run_test_case`` was deleted
as a dormant twin that passed a test case with no assertion), so every run reached
that branch: ``POST /testing/end/{id}`` answered ``passed`` for any run at all.

The service is loaded from its file alone: importing the ``services`` package would
run its eager imports. Locally: ``cd python-bridge && python -m pytest
tests/test_integration_testing_run_status.py`` (``tests/conftest.py`` imports
``models``, so pytest must run from ``python-bridge/``).
"""

from __future__ import annotations

import importlib.util
from datetime import datetime
from pathlib import Path

SERVICE = Path(__file__).resolve().parent.parent / "services" / "integration_testing_service.py"


def _load_service_module():
    spec = importlib.util.spec_from_file_location("integration_testing_service_under_test", SERVICE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


service_module = _load_service_module()


def _ended_run_status(result_statuses: list[str]) -> str:
    """Start a run, record one result per status, end it; the run's final status."""
    service = service_module.IntegrationTestingService(emit_log_fn=lambda level, msg: None)
    run_id = service.start_test_run("run")["run_id"]
    for index, status in enumerate(result_statuses):
        service._test_runs[run_id].test_results.append(
            service_module.TestResult(
                test_id=f"t{index}",
                test_name=f"test {index}",
                status=service_module.TestStatus(status),
                assertions=[],
                start_time=datetime(2026, 11, 7),
            )
        )
    ended = service.end_test_run()
    assert ended["success"] is True
    return ended["run"]["status"]


def test_a_run_that_recorded_nothing_is_unknown_not_passed():
    assert _ended_run_status([]) == "unknown"


def test_one_passed_result_passes():
    assert _ended_run_status(["passed"]) == "passed"


def test_many_results_fold_error_over_failed_over_passed():
    assert _ended_run_status(["passed", "passed", "passed"]) == "passed"
    assert _ended_run_status(["passed", "failed", "passed"]) == "failed"
    assert _ended_run_status(["passed", "failed", "error"]) == "error"
