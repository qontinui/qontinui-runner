#!/usr/bin/env python3
"""Comprehensive test of the GUI simulator and workflow testing framework.

This script verifies that all components work together correctly.
It creates a minimal in-memory database, populates it with test data,
and runs a complete test workflow.

Run with: python3 test_framework.py
"""

import sys
import logging
from pathlib import Path

# Add parent directory to path for imports
parent_dir = Path(__file__).parent.parent
sys.path.insert(0, str(parent_dir))

# Now we can import from the testing package
from testing.historical_data import HistoricalData
from testing.gui_simulator import GUISimulator
from testing.workflow_tester import AutomationWorkflowTester
from testing.failure_investigator import FailureInvestigator
from models.historical_capture import HistoricalCapture

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(levelname)s: %(message)s'
)

logger = logging.getLogger(__name__)


def create_test_data():
    """Create test historical captures."""
    captures = []

    # Capture 1: Login button click from logged-out state
    captures.append(HistoricalCapture(
        session_id="test_session_001",
        timestamp=1000.0,
        frame_before=0,
        frame_after=1,
        active_states_before={"LoginScreen", "LoggedOut"},
        action_type="CLICK",
        action_target="LoginButton",
        action_params={"target": "LoginButton"},
        active_states_after={"LoginScreen", "FormVisible"},
        states_appeared={"FormVisible"},
        states_disappeared={"LoggedOut"},
        metadata={}
    ))

    # Capture 2: Type username
    captures.append(HistoricalCapture(
        session_id="test_session_001",
        timestamp=1001.0,
        frame_before=1,
        frame_after=2,
        active_states_before={"LoginScreen", "FormVisible"},
        action_type="TYPE",
        action_target="UsernameField",
        action_params={"field": "UsernameField", "text": "testuser"},
        active_states_after={"LoginScreen", "FormFilled"},
        states_appeared={"FormFilled"},
        states_disappeared={"FormVisible"},
        metadata={}
    ))

    # Capture 3: Submit login
    captures.append(HistoricalCapture(
        session_id="test_session_001",
        timestamp=1002.0,
        frame_before=2,
        frame_after=3,
        active_states_before={"LoginScreen", "FormFilled"},
        action_type="CLICK",
        action_target="SubmitButton",
        action_params={"target": "SubmitButton"},
        active_states_after={"Dashboard", "LoggedIn"},
        states_appeared={"Dashboard", "LoggedIn"},
        states_disappeared={"LoginScreen", "FormFilled"},
        metadata={}
    ))

    return captures


def populate_database(history: HistoricalData):
    """Populate database with test captures."""
    captures = create_test_data()

    for capture in captures:
        # Insert directly into database
        history._insert_capture(capture)

    logger.info(f"Populated database with {len(captures)} test captures")


def create_test_workflow():
    """Create a simple test workflow."""
    return {
        'id': 'test_login_workflow',
        'name': 'Test Login Workflow',
        'initial_states': {'LoginScreen', 'LoggedOut'},
        'actions': [
            {
                'type': 'CLICK',
                'target': 'LoginButton',
                'config': {'target': 'LoginButton'},
                'expected_states': {'LoginScreen', 'FormVisible'}
            },
            {
                'type': 'TYPE',
                'target': 'UsernameField',
                'config': {'field': 'UsernameField', 'text': 'testuser'},
                'expected_states': {'LoginScreen', 'FormFilled'}
            },
            {
                'type': 'CLICK',
                'target': 'SubmitButton',
                'config': {'target': 'SubmitButton'},
                'expected_states': {'Dashboard', 'LoggedIn'}
            },
        ]
    }


def test_historical_data():
    """Test 1: Historical data management."""
    print("\n" + "="*60)
    print("TEST 1: Historical Data Management")
    print("="*60)

    try:
        # Create in-memory database
        history = HistoricalData(":memory:")

        # Populate with test data
        populate_database(history)

        # Test querying
        matches = history.find_matching(
            {"LoginScreen", "LoggedOut"},
            "CLICK",
            "LoginButton"
        )

        assert len(matches) == 1, f"Expected 1 match, got {len(matches)}"
        assert matches[0].action_type == "CLICK"
        assert matches[0].action_target == "LoginButton"

        # Test statistics
        stats = history.get_statistics()
        assert stats['total_captures'] == 3, f"Expected 3 captures, got {stats['total_captures']}"

        print("✓ Historical data test passed")
        return history

    except Exception as e:
        print(f"✗ Historical data test failed: {e}")
        raise


def test_gui_simulator(history: HistoricalData):
    """Test 2: GUI simulator."""
    print("\n" + "="*60)
    print("TEST 2: GUI Simulator")
    print("="*60)

    try:
        simulator = GUISimulator(history)

        # Set initial state
        simulator.set_initial_states({"LoginScreen", "LoggedOut"})
        assert simulator.get_current_states() == {"LoginScreen", "LoggedOut"}

        # Simulate action
        action = {
            'type': 'CLICK',
            'target': 'LoginButton',
            'config': {'target': 'LoginButton'}
        }

        result = simulator.execute_action(action)

        assert result.success, f"Simulation failed: {result.error}"
        assert result.resulting_states == {"LoginScreen", "FormVisible"}
        assert simulator.get_current_states() == {"LoginScreen", "FormVisible"}

        print("✓ GUI simulator test passed")
        return simulator

    except Exception as e:
        print(f"✗ GUI simulator test failed: {e}")
        raise


def test_workflow_execution(history: HistoricalData):
    """Test 3: Workflow execution."""
    print("\n" + "="*60)
    print("TEST 3: Workflow Execution")
    print("="*60)

    try:
        simulator = GUISimulator(history)
        tester = AutomationWorkflowTester(simulator)

        workflow = create_test_workflow()

        result = tester.run_workflow(workflow)

        assert result.completed, "Workflow did not complete"
        assert result.passed, f"Workflow failed: {result.failed_at}"
        assert result.passed_steps == 3, f"Expected 3 passed steps, got {result.passed_steps}"
        assert result.final_states == {"Dashboard", "LoggedIn"}

        print(f"✓ Workflow execution test passed")
        print(f"  - Completed {result.passed_steps}/{result.total_steps} steps")
        print(f"  - Final states: {sorted(result.final_states)}")

        return result

    except Exception as e:
        print(f"✗ Workflow execution test failed: {e}")
        raise


def test_failure_investigation(history: HistoricalData):
    """Test 4: Failure investigation."""
    print("\n" + "="*60)
    print("TEST 4: Failure Investigation")
    print("="*60)

    try:
        simulator = GUISimulator(history)
        tester = AutomationWorkflowTester(simulator)

        # Create a workflow that will fail (missing data)
        failing_workflow = {
            'id': 'failing_workflow',
            'name': 'Failing Workflow',
            'initial_states': {'UnknownState'},  # State with no data
            'actions': [
                {
                    'type': 'CLICK',
                    'target': 'NonexistentButton',
                    'config': {'target': 'NonexistentButton'}
                }
            ]
        }

        result = tester.run_workflow(failing_workflow)

        assert not result.passed, "Workflow should have failed"
        assert result.failed_at is not None, "Should have failure information"

        # Investigate the failure
        investigator = FailureInvestigator(history)
        detail = investigator.investigate(result)

        assert detail is not None, "Investigation should return details"
        assert detail.has_data_gap, "Should identify as data gap"
        assert len(detail.suggestions) > 0, "Should provide suggestions"

        print("✓ Failure investigation test passed")
        print(f"  - Correctly identified data gap")
        print(f"  - Provided {len(detail.suggestions)} suggestions")

    except Exception as e:
        print(f"✗ Failure investigation test failed: {e}")
        raise


def test_batch_execution(history: HistoricalData):
    """Test 5: Batch workflow execution."""
    print("\n" + "="*60)
    print("TEST 5: Batch Workflow Execution")
    print("="*60)

    try:
        simulator = GUISimulator(history)
        tester = AutomationWorkflowTester(simulator)

        # Create multiple workflows
        workflows = [create_test_workflow() for _ in range(3)]
        for i, wf in enumerate(workflows):
            wf['id'] = f'workflow_{i}'
            wf['name'] = f'Workflow {i}'

        results = tester.run_all_workflows(workflows)

        assert len(results) == 3, f"Expected 3 results, got {len(results)}"
        assert all(r.passed for r in results), "All workflows should pass"

        print("✓ Batch execution test passed")
        print(f"  - Executed {len(results)} workflows")
        print(f"  - All passed successfully")

    except Exception as e:
        print(f"✗ Batch execution test failed: {e}")
        raise


def main():
    """Run all tests."""
    print("\n" + "="*60)
    print("GUI SIMULATOR AND WORKFLOW TESTER - FRAMEWORK TEST")
    print("="*60)

    try:
        # Run all tests
        history = test_historical_data()
        test_gui_simulator(history)
        test_workflow_execution(history)
        test_failure_investigation(history)
        test_batch_execution(history)

        # Cleanup
        history.close()

        print("\n" + "="*60)
        print("ALL TESTS PASSED ✓")
        print("="*60 + "\n")

        print("The framework is working correctly!")
        print("\nNext steps:")
        print("1. Import real automation runs into the database")
        print("2. Run your automation workflows through the tester")
        print("3. Use the failure investigator for debugging")
        print("\nSee example_usage.py for more detailed examples.")

        return 0

    except Exception as e:
        print("\n" + "="*60)
        print("TESTS FAILED ✗")
        print("="*60 + "\n")
        logger.error(f"Test suite failed: {e}", exc_info=True)
        return 1


if __name__ == "__main__":
    sys.exit(main())
