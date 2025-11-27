"""Integration testing framework for automation workflows.

This package provides a complete testing framework for automation workflows.
Unlike traditional testing where you mock external dependencies, this framework
tests the automation code itself by simulating GUI responses using historical
data from real automation runs.

Key Concepts:

1. Historical Data (historical_data.py):
   - Database of captured GUI responses from real automation runs
   - Built from video recordings + action execution records
   - Stores what states resulted from each action in various contexts

2. GUI Simulator (gui_simulator.py):
   - Simulates GUI responses using historical captures
   - RANDOMLY selects from matching captures (non-deterministic, like real automation)
   - Provides the "environment" for testing workflows
   - This is NOT mocking external apps - it's replaying real responses

3. Workflow Tester (workflow_tester.py):
   - Executes workflows in the simulated environment
   - The workflow itself IS the test case
   - Verifies state transitions and action execution
   - Provides immediate feedback (no separate build/run steps)

4. Failure Investigator (failure_investigator.py):
   - Analyzes test failures to identify root causes
   - Distinguishes between: workflow bugs, missing data, inconsistent behavior
   - Provides actionable suggestions for fixes
   - References video timestamps for manual review

Usage Example:

    # 1. Build historical database from automation runs
    from testing import HistoricalData

    history = HistoricalData("captures.db")
    history.import_automation_run(
        run_id="run001",
        video_path="/recordings/run001.mp4",
        events_path="/recordings/run001_events.json",
        processed_states=state_data
    )

    # 2. Create simulator and tester
    from testing import GUISimulator, AutomationWorkflowTester

    simulator = GUISimulator(history)
    tester = AutomationWorkflowTester(simulator)

    # 3. Test workflows
    result = tester.run_workflow(my_workflow)

    if result.passed:
        print(f"SUCCESS: {result.workflow_name}")
    else:
        # 4. Investigate failures
        from testing import FailureInvestigator

        investigator = FailureInvestigator(history)
        detail = investigator.investigate(result)

        print(f"FAILED at step {detail.failed_step}")
        for suggestion in detail.suggestions:
            print(f"  - {suggestion}")

What This Framework Tests:

✓ Automation workflow logic and state management
✓ Action sequencing and dependencies
✓ State transition handling
✓ Error handling and recovery
✓ Workflow robustness against varying GUI responses

What This Framework Does NOT Test:

✗ External application functionality (not our concern)
✗ GUI rendering or visual appearance (not relevant)
✗ Network connectivity (can be added to captures if needed)
✗ Performance/timing (simulator has no delays)

Design Philosophy:

This is INTEGRATION TESTING of automation code, not unit testing. We test
complete workflows in a realistic (but simulated) environment. The historical
database provides a "replay buffer" of real GUI behavior, allowing tests to
run quickly and deterministically while still exercising the automation code
against real-world scenarios.

The random selection of historical matches introduces non-determinism similar
to real automation, helping identify race conditions and timing issues that
might not appear in perfectly deterministic tests.
"""

from .historical_data import HistoricalData
from .gui_simulator import GUISimulator, SimulatedResult
from .workflow_tester import (
    AutomationWorkflowTester,
    WorkflowTestResult,
    StepResult
)
from .failure_investigator import (
    FailureInvestigator,
    FailureDetail
)

__all__ = [
    # Historical data management
    "HistoricalData",

    # Simulation
    "GUISimulator",
    "SimulatedResult",

    # Testing
    "AutomationWorkflowTester",
    "WorkflowTestResult",
    "StepResult",

    # Failure analysis
    "FailureInvestigator",
    "FailureDetail",
]
