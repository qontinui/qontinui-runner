"""Example usage of the GUI simulator and workflow testing framework.

This demonstrates how to:
1. Import historical data from automation runs
2. Create a GUI simulator
3. Test automation workflows
4. Investigate and analyze failures

This is a complete working example showing the full testing workflow.
"""

import logging
from pathlib import Path
from typing import Set

from testing import (
    HistoricalData,
    GUISimulator,
    AutomationWorkflowTester,
    FailureInvestigator
)

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)

logger = logging.getLogger(__name__)


# Example workflow structure (simplified)
class SimpleWorkflow:
    """Example workflow for demonstration."""

    def __init__(self, workflow_id: str, name: str):
        self.id = workflow_id
        self.name = name
        self.actions = []
        self.initial_states: Set[str] = set()

    def add_action(self, action_type: str, target: str, expected_states: Set[str] = None):
        """Add an action to the workflow."""
        action = {
            'type': action_type,
            'target': target,
            'config': {'target': target},
            'expected_states': expected_states or set()
        }
        self.actions.append(action)
        return self


def example_1_setup_historical_database():
    """Example 1: Setting up the historical database from automation runs."""

    print("\n" + "="*80)
    print("EXAMPLE 1: Setting up Historical Database")
    print("="*80 + "\n")

    # Create or open the historical database
    db_path = Path(__file__).parent / "data" / "example_captures.db"
    db_path.parent.mkdir(exist_ok=True)

    history = HistoricalData(str(db_path))

    # In a real scenario, you would import from actual automation runs
    # For this example, we'll show what the import call would look like:

    print("To import an automation run, you would call:")
    print("""
    history.import_automation_run(
        run_id="login_test_001",
        video_path="/recordings/login_test_001.mp4",
        events_path="/recordings/login_test_001_events.json",
        processed_states={
            "action_001": {"LoginScreen", "LoggedOut"},
            "action_002": {"LoginScreen", "FormFilled"},
            "action_003": {"Dashboard", "LoggedIn"},
        }
    )
    """)

    # Get database statistics
    stats = history.get_statistics()
    print(f"\nDatabase Statistics:")
    print(f"  Total runs: {stats.get('total_runs', 0)}")
    print(f"  Total captures: {stats.get('total_captures', 0)}")
    print(f"  State coverage: {stats.get('state_coverage', 0)} unique state combinations")

    history.close()

    print("\n✓ Historical database setup complete")


def example_2_test_simple_workflow():
    """Example 2: Testing a simple workflow with the simulator."""

    print("\n" + "="*80)
    print("EXAMPLE 2: Testing a Simple Workflow")
    print("="*80 + "\n")

    # Setup
    db_path = Path(__file__).parent / "data" / "example_captures.db"
    history = HistoricalData(str(db_path))
    simulator = GUISimulator(history)
    tester = AutomationWorkflowTester(simulator)

    # Create a simple login workflow
    login_workflow = SimpleWorkflow("login_001", "Simple Login")
    login_workflow.initial_states = {"LoginScreen", "LoggedOut"}
    login_workflow.add_action("CLICK", "LoginButton", {"LoginScreen", "FormVisible"})
    login_workflow.add_action("TYPE", "UsernameField", {"LoginScreen", "FormFilled"})
    login_workflow.add_action("CLICK", "SubmitButton", {"Dashboard", "LoggedIn"})

    print(f"Testing workflow: {login_workflow.name}")
    print(f"Initial states: {sorted(login_workflow.initial_states)}")
    print(f"Actions: {len(login_workflow.actions)}")

    # Run the workflow test
    result = tester.run_workflow(login_workflow)

    print(f"\nTest Result:")
    print(f"  Status: {'PASSED' if result.passed else 'FAILED'}")
    print(f"  Steps completed: {result.passed_steps}/{result.total_steps}")
    print(f"  Duration: {result.duration:.2f}s" if result.duration else "  Duration: N/A")

    if result.passed:
        print(f"  Final states: {sorted(result.final_states)}")
    else:
        print(f"  Failed at step: {result.failed_at.step_index}")
        print(f"  Error: {result.failed_at.error_message}")

    history.close()

    print("\n✓ Workflow test complete")


def example_3_investigate_failure():
    """Example 3: Investigating a failed workflow test."""

    print("\n" + "="*80)
    print("EXAMPLE 3: Investigating Test Failures")
    print("="*80 + "\n")

    # Setup
    db_path = Path(__file__).parent / "data" / "example_captures.db"
    history = HistoricalData(str(db_path))
    simulator = GUISimulator(history)
    tester = AutomationWorkflowTester(simulator)

    # Create a workflow that will likely fail (missing data)
    problematic_workflow = SimpleWorkflow("checkout_001", "Checkout Flow")
    problematic_workflow.initial_states = {"Cart", "ItemsInCart"}
    problematic_workflow.add_action("CLICK", "CheckoutButton", {"Checkout", "AddressForm"})
    problematic_workflow.add_action("TYPE", "AddressField", {"Checkout", "AddressFilled"})

    print(f"Testing potentially problematic workflow: {problematic_workflow.name}")

    # Run the test
    result = tester.run_workflow(problematic_workflow)

    if not result.passed:
        print(f"\n⚠ Workflow failed at step {result.failed_at.step_index}")

        # Investigate the failure
        investigator = FailureInvestigator(history)
        detail = investigator.investigate(result)

        if detail:
            print(f"\nFailure Analysis:")
            print(f"  Workflow: {detail.workflow_name}")
            print(f"  Failed action: {detail.failed_step}")

            print(f"\n  Possible Issues:")
            for issue in detail.possible_issues:
                print(f"    - {issue}")

            print(f"\n  Suggestions:")
            for suggestion in detail.suggestions:
                print(f"    • {suggestion}")

            if detail.related_video_timestamps:
                print(f"\n  Video References:")
                for video_path, timestamp in detail.related_video_timestamps[:3]:
                    print(f"    - {Path(video_path).name} at {timestamp:.1f}s")

    history.close()

    print("\n✓ Failure investigation complete")


def example_4_batch_testing():
    """Example 4: Running multiple workflows as a test suite."""

    print("\n" + "="*80)
    print("EXAMPLE 4: Batch Testing Multiple Workflows")
    print("="*80 + "\n")

    # Setup
    db_path = Path(__file__).parent / "data" / "example_captures.db"
    history = HistoricalData(str(db_path))
    simulator = GUISimulator(history)
    tester = AutomationWorkflowTester(simulator)

    # Create multiple workflows
    workflows = []

    # Workflow 1: Login
    login = SimpleWorkflow("login_001", "Login")
    login.initial_states = {"LoginScreen", "LoggedOut"}
    login.add_action("CLICK", "LoginButton")
    workflows.append(login)

    # Workflow 2: Search
    search = SimpleWorkflow("search_001", "Search Products")
    search.initial_states = {"Dashboard", "LoggedIn"}
    search.add_action("CLICK", "SearchBox")
    search.add_action("TYPE", "SearchField")
    workflows.append(search)

    # Workflow 3: Logout
    logout = SimpleWorkflow("logout_001", "Logout")
    logout.initial_states = {"Dashboard", "LoggedIn"}
    logout.add_action("CLICK", "UserMenu")
    logout.add_action("CLICK", "LogoutButton")
    workflows.append(logout)

    print(f"Running test suite with {len(workflows)} workflows...")

    # Run all workflows
    results = tester.run_all_workflows(workflows)

    # Summary
    passed = sum(1 for r in results if r.passed)
    failed = len(results) - passed

    print(f"\nTest Suite Results:")
    print(f"  Total: {len(results)}")
    print(f"  Passed: {passed} ({passed/len(results)*100:.1f}%)")
    print(f"  Failed: {failed}")

    print(f"\nDetailed Results:")
    for i, result in enumerate(results, 1):
        status = "✓ PASS" if result.passed else "✗ FAIL"
        print(f"  {i}. {status} - {result.workflow_name} ({result.passed_steps}/{result.total_steps} steps)")

    history.close()

    print("\n✓ Batch testing complete")


def example_5_simulator_statistics():
    """Example 5: Using simulator statistics for insights."""

    print("\n" + "="*80)
    print("EXAMPLE 5: Simulator Statistics and Insights")
    print("="*80 + "\n")

    # Setup
    db_path = Path(__file__).parent / "data" / "example_captures.db"
    history = HistoricalData(str(db_path))
    simulator = GUISimulator(history)

    # Simulate some actions manually
    simulator.set_initial_states({"LoginScreen", "LoggedOut"})

    # Simulate a few actions to generate statistics
    print("Simulating actions...")

    # Get simulator statistics
    stats = simulator.get_statistics()

    print(f"\nSimulator Statistics:")
    print(f"  Total actions simulated: {stats['total_actions']}")
    print(f"  Successful: {stats['successful_actions']}")
    print(f"  Failed: {stats['failed_actions']}")
    print(f"  State changes: {stats['state_changes']}")
    print(f"  Unique states visited: {stats['unique_states_visited']}")
    print(f"  Current states: {stats['current_states']}")

    # Get historical database statistics
    db_stats = history.get_statistics()

    print(f"\nHistorical Database Statistics:")
    print(f"  Total runs imported: {db_stats.get('total_runs', 0)}")
    print(f"  Total captures: {db_stats.get('total_captures', 0)}")
    print(f"  Action type distribution:")
    for action_type, count in db_stats.get('action_type_counts', {}).items():
        print(f"    - {action_type}: {count}")

    history.close()

    print("\n✓ Statistics collection complete")


def main():
    """Run all examples."""

    print("\n" + "="*80)
    print("GUI SIMULATOR AND WORKFLOW TESTER - EXAMPLES")
    print("="*80)

    try:
        example_1_setup_historical_database()
        example_2_test_simple_workflow()
        example_3_investigate_failure()
        example_4_batch_testing()
        example_5_simulator_statistics()

        print("\n" + "="*80)
        print("ALL EXAMPLES COMPLETED SUCCESSFULLY")
        print("="*80 + "\n")

    except Exception as e:
        logger.error(f"Example failed: {e}", exc_info=True)
        print(f"\n✗ Examples failed: {e}")


if __name__ == "__main__":
    main()
