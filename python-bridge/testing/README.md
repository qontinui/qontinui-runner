# GUI Simulator and Workflow Testing Framework

A comprehensive integration testing framework for automation workflows using historical data simulation.

## Overview

This framework enables testing automation code by simulating GUI responses using historical captures from real automation runs. Instead of mocking external applications, it uses **actual GUI responses** recorded during past executions to create a realistic test environment.

### Key Concept

Traditional testing mocks external dependencies. This framework is different:

- **NOT mocking applications**: We use REAL GUI responses captured from actual automation runs
- **Testing automation code**: We verify the automation logic, state management, and workflow execution
- **Non-deterministic by design**: Random selection of historical matches mimics real automation variability
- **Integration testing**: Tests complete workflows, not isolated units

## Architecture

### Components

1. **HistoricalData** (`historical_data.py`)
   - SQLite database of captured GUI responses
   - Built from video recordings + action execution records
   - Stores state transitions and action outcomes
   - Query interface for finding matching captures

2. **GUISimulator** (`gui_simulator.py`)
   - Simulates GUI responses using historical data
   - Randomly selects from matching captures (non-deterministic)
   - Maintains simulated state throughout test
   - Provides execution logs for debugging

3. **AutomationWorkflowTester** (`workflow_tester.py`)
   - Executes workflows in simulated environment
   - The workflow itself IS the test case
   - Verifies state transitions and execution
   - Immediate feedback (no build steps)

4. **FailureInvestigator** (`failure_investigator.py`)
   - Analyzes test failures
   - Identifies root causes (bugs vs data gaps vs non-determinism)
   - Provides actionable suggestions
   - References video timestamps for review

## Installation

No additional dependencies beyond the main project requirements:

```bash
cd /mnt/c/qontinui/qontinui-runner/python-bridge
pip install -r requirements.txt
```

## Quick Start

### 1. Build Historical Database

Import automation runs to build the historical database:

```python
from testing import HistoricalData

# Create or open database
history = HistoricalData("captures.db")

# Import an automation run
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

# Check coverage
stats = history.get_statistics()
print(f"Total captures: {stats['total_captures']}")
print(f"State coverage: {stats['state_coverage']} unique combinations")

history.close()
```

### 2. Test a Workflow

Run a workflow in the simulated environment:

```python
from testing import GUISimulator, AutomationWorkflowTester

# Setup simulator
history = HistoricalData("captures.db")
simulator = GUISimulator(history)
tester = AutomationWorkflowTester(simulator)

# Test a workflow
result = tester.run_workflow(my_workflow)

if result.passed:
    print(f"✓ {result.workflow_name} passed!")
    print(f"  Steps: {result.passed_steps}/{result.total_steps}")
    print(f"  Duration: {result.duration:.2f}s")
else:
    print(f"✗ {result.workflow_name} failed at step {result.failed_at.step_index}")
    print(f"  Error: {result.failed_at.error_message}")

history.close()
```

### 3. Investigate Failures

Analyze why a test failed:

```python
from testing import FailureInvestigator

investigator = FailureInvestigator(history)
detail = investigator.investigate(result)

if detail:
    print(f"\nFailure Analysis:")
    print(f"  Failed action: Step {detail.failed_step}")

    print(f"\n  Issues:")
    for issue in detail.possible_issues:
        print(f"    - {issue}")

    print(f"\n  Suggestions:")
    for suggestion in detail.suggestions:
        print(f"    • {suggestion}")

    # Review video captures
    for video_path, timestamp in detail.related_video_timestamps[:3]:
        print(f"    - {video_path} at {timestamp:.1f}s")
```

## Usage Examples

See `example_usage.py` for complete working examples:

```bash
cd /mnt/c/qontinui/qontinui-runner/python-bridge/testing
python3 example_usage.py
```

Examples include:
1. Setting up historical database
2. Testing simple workflows
3. Investigating failures
4. Batch testing multiple workflows
5. Analyzing simulator statistics

## Workflow Structure

Workflows must provide:

```python
class Workflow:
    id: str                     # Unique identifier
    name: str                   # Human-readable name
    actions: List[Action]       # List of actions to execute
    initial_states: Set[str]    # States active at start (optional)
```

Actions must provide:

```python
class Action:
    type: str                   # Action type (CLICK, TYPE, etc.)
    target: str                 # Target identifier
    config: Dict[str, Any]      # Action configuration
    expected_states: Set[str]   # Expected states after (optional)
```

Or as dictionaries:

```python
workflow = {
    'id': 'login_001',
    'name': 'Login Flow',
    'initial_states': {'LoginScreen', 'LoggedOut'},
    'actions': [
        {
            'type': 'CLICK',
            'target': 'LoginButton',
            'config': {'target': 'LoginButton'},
            'expected_states': {'LoginScreen', 'FormVisible'}
        },
        # ... more actions
    ]
}
```

## Database Schema

### captures table

Stores historical GUI responses:

```sql
CREATE TABLE captures (
    session_id TEXT NOT NULL,           -- Automation run ID
    timestamp REAL NOT NULL,            -- When action occurred
    frame_before INTEGER NOT NULL,      -- Frame number before
    frame_after INTEGER NOT NULL,       -- Frame number after
    states_before_json TEXT NOT NULL,   -- Active states before
    action_type TEXT NOT NULL,          -- Type of action
    action_target TEXT NOT NULL,        -- Action target
    action_params_json TEXT,            -- Action parameters
    states_after_json TEXT NOT NULL,    -- Active states after
    states_appeared_json TEXT,          -- States that appeared
    states_disappeared_json TEXT,       -- States that disappeared
    metadata_json TEXT,                 -- Additional data
    PRIMARY KEY (session_id, timestamp)
);
```

### runs table

Tracks imported automation runs:

```sql
CREATE TABLE runs (
    run_id TEXT PRIMARY KEY,            -- Unique run ID
    video_path TEXT,                    -- Path to video
    events_path TEXT,                   -- Path to events file
    import_timestamp REAL NOT NULL,     -- When imported
    total_actions INTEGER NOT NULL,     -- Action count
    metadata_json TEXT                  -- Additional data
);
```

## Testing Philosophy

### What We Test

✓ **Automation workflow logic**: Does the workflow sequence make sense?
✓ **State management**: Are states tracked and transitioned correctly?
✓ **Action execution**: Do actions execute in the right order?
✓ **Error handling**: Does the workflow handle failures gracefully?
✓ **Robustness**: Does it work with varying GUI responses?

### What We Don't Test

✗ **External application functionality**: Not our concern
✗ **GUI rendering**: Visual appearance doesn't matter
✗ **Network connectivity**: Can be added to captures if needed
✗ **Performance**: Simulator has no delays

### Why Random Selection?

The simulator **randomly** selects from matching historical captures. This is intentional:

- **Mimics real automation**: Same action can have subtle variations
- **Finds race conditions**: Non-determinism reveals timing issues
- **Tests robustness**: Workflow must handle different outcomes
- **Prevents overfitting**: Can't rely on specific sequence

## Failure Types

The investigator identifies three main failure types:

### 1. Data Gap (No Historical Data)

```
✗ NO HISTORICAL DATA for CLICK(LoginButton) with states {LoginScreen}
```

**Cause**: This execution path has never been captured
**Fix**: Run automation with these states active and import the recording

### 2. State Mismatch (Wrong Expectations)

```
✗ STATE MISMATCH: Expected {Dashboard, LoggedIn} but got {Error, LoginFailed}
```

**Cause**: Workflow expects wrong states, or data is inconsistent
**Fix**: Update workflow expectations or add error handling

### 3. Inconsistent Behavior (Non-Deterministic)

```
✗ INCONSISTENT BEHAVIOR: Action produces 3 different outcomes (67% consistency)
```

**Cause**: Action sometimes produces different states
**Fix**: Add conditional logic, retries, or state verification

## Best Practices

### Building Historical Database

1. **Capture diverse scenarios**: Include success and failure paths
2. **Cover state combinations**: Test different starting states
3. **Include edge cases**: Capture unusual but possible conditions
4. **Regular updates**: Add new captures as workflows evolve
5. **Version control**: Track which app versions captures are from

### Writing Testable Workflows

1. **Clear state expectations**: Document expected states after actions
2. **Error handling**: Include recovery logic for failures
3. **Idempotent actions**: Make actions safe to retry
4. **State verification**: Check states before critical actions
5. **Graceful degradation**: Handle missing or unexpected states

### Debugging Failed Tests

1. **Check execution log**: Review simulator.get_execution_log()
2. **Verify initial states**: Ensure correct starting conditions
3. **Review video captures**: Watch actual GUI behavior
4. **Check consistency scores**: Low scores indicate variability
5. **Analyze state transitions**: Understand why states changed

## API Reference

### HistoricalData

```python
class HistoricalData:
    def __init__(self, db_path: Optional[str] = None)
    def find_matching(self, active_states: Set[str], action_type: str,
                     action_target: str) -> List[HistoricalCapture]
    def get_match_count(self, active_states: Set[str], action_type: str,
                       action_target: str) -> int
    def import_automation_run(self, run_id: str, video_path: str,
                             events_path: str, processed_states: dict) -> int
    def get_statistics(self) -> dict
    def close(self)
```

### GUISimulator

```python
class GUISimulator:
    def __init__(self, historical_data: HistoricalData)
    def set_initial_states(self, states: Set[str])
    def execute_action(self, action: Any) -> SimulatedResult
    def get_current_states(self) -> Set[str]
    def get_execution_log(self) -> List[SimulatedResult]
    def get_statistics(self) -> dict
    def reset(self)
```

### AutomationWorkflowTester

```python
class AutomationWorkflowTester:
    def __init__(self, simulator: GUISimulator, strict_mode: bool = False)
    def run_workflow(self, workflow: Any,
                    initial_states: Optional[Set[str]] = None) -> WorkflowTestResult
    def run_all_workflows(self, workflows: List[Any],
                         continue_on_failure: bool = True) -> List[WorkflowTestResult]
```

### FailureInvestigator

```python
class FailureInvestigator:
    def __init__(self, history: HistoricalData)
    def investigate(self, result: WorkflowTestResult) -> Optional[FailureDetail]
    def suggest_fixes(self, detail: FailureDetail) -> List[str]
```

## Troubleshooting

### "No historical data" errors

- Run automation manually with required states
- Import the recording: `history.import_automation_run(...)`
- Verify states are correctly labeled

### Tests pass locally but fail in CI

- Check database is included in CI environment
- Verify same Python/dependency versions
- Random selection may expose timing issues

### Inconsistent test results

- This is expected! Random selection mimics real automation
- Review consistency scores in failure analysis
- Add retry/verification logic to workflows

### Performance issues

- Database grows large with many captures
- Add indexes for common queries
- Archive old/unused captures
- Use `:memory:` database for unit tests

## Contributing

When adding features to the testing framework:

1. Maintain the core philosophy (test automation code, not apps)
2. Keep random selection for realistic testing
3. Add comprehensive docstrings
4. Update examples and README
5. Test with both simple and complex workflows

## License

Same as the main qontinui-runner project.

## Support

For issues and questions:
- Check `example_usage.py` for working examples
- Review failure investigator suggestions
- Consult main project documentation
- Open an issue with test reproduction steps
