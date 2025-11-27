"""GUI simulator for testing automation workflows.

This module provides the simulation environment for testing automation code.
It simulates how a GUI would respond to actions, using historical capture data
from real automation runs.

Key Concept:
    This does NOT mock external applications. Instead, it uses REAL GUI responses
    captured from past automation runs to create a realistic test environment.
    This lets us test the automation code's logic, state management, and workflow
    execution without needing a live GUI.

    The random selection of matching captures makes tests non-deterministic (like
    real automation), helping identify race conditions and timing issues.
"""

import logging
import random
from dataclasses import dataclass, field
from typing import Set, Optional, Any

try:
    from ..models.historical_capture import HistoricalCapture
except ImportError:
    from models.historical_capture import HistoricalCapture

try:
    from .historical_data import HistoricalData
except ImportError:
    from testing.historical_data import HistoricalData

logger = logging.getLogger(__name__)


@dataclass
class SimulatedResult:
    """Result of a simulated action execution.

    This represents what happened when we simulated an action in the test
    environment. It includes both the outcome (success/failure) and details
    about how the simulation worked.

    Attributes:
        success: Whether the action simulation succeeded
        previous_states: States that were active before the action
        resulting_states: States that became active after the action
        selected_match: The historical capture that was randomly selected
        total_matches: Total number of matching captures available
        error: Error message if simulation failed
    """
    success: bool
    previous_states: Set[str] = field(default_factory=set)
    resulting_states: Set[str] = field(default_factory=set)
    selected_match: Optional[HistoricalCapture] = None
    total_matches: int = 0
    error: Optional[str] = None

    @property
    def state_changed(self) -> bool:
        """Check if states changed during simulation."""
        return self.previous_states != self.resulting_states

    @property
    def states_activated(self) -> Set[str]:
        """Get states that were activated."""
        return self.resulting_states - self.previous_states

    @property
    def states_deactivated(self) -> Set[str]:
        """Get states that were deactivated."""
        return self.previous_states - self.resulting_states


class GUISimulator:
    """Simulates GUI responses using historical capture data.

    This provides the 'environment' for testing automation workflows. Instead
    of running actions against a real GUI, we simulate what the GUI would do
    based on historical captures from real automation runs.

    The key insight: Each time we run a test, we RANDOMLY select from matching
    historical captures. This makes tests non-deterministic, just like real
    automation where timing, system state, and other factors create variability.

    Usage:
        # Create simulator with historical data
        history = HistoricalData("path/to/database.db")
        simulator = GUISimulator(history)

        # Set initial state
        simulator.set_initial_states({"LoginScreen", "LoggedOut"})

        # Simulate an action
        action = ClickAction(target="LoginButton")
        result = simulator.execute_action(action)

        if result.success:
            print(f"Now in states: {result.resulting_states}")
        else:
            print(f"Simulation failed: {result.error}")

    Attributes:
        history: HistoricalData database with captured GUI responses
        current_states: Set of currently active state names
        execution_log: List of all simulated results (for debugging)
    """

    def __init__(self, historical_data: HistoricalData):
        """Initialize GUI simulator.

        Args:
            historical_data: Database of historical captures to use for simulation
        """
        self.history = historical_data
        self.current_states: Set[str] = set()
        self.execution_log: list[SimulatedResult] = []

        logger.info("GUISimulator initialized")

    def set_initial_states(self, states: Set[str]):
        """Set the initial GUI states for simulation.

        This establishes the starting context for the test. Usually these would
        be the states that are active when the automation workflow begins.

        Args:
            states: Set of state names to make active
        """
        self.current_states = set(states)  # Make a copy
        self.execution_log.clear()

        logger.info(f"Initial states set: {sorted(states)}")

    def execute_action(self, action: Any) -> SimulatedResult:
        """Simulate what would happen if this action were executed.

        This is the core simulation method. It:
        1. Looks up historical captures matching the current context
        2. RANDOMLY selects one (making each test run different)
        3. Updates the simulated state based on the selected capture
        4. Returns the result

        The random selection is intentional - it mimics real automation where
        the same action in the same state can have subtle variations (timing,
        rendering, system load, etc.).

        Args:
            action: Action object with type and target attributes

        Returns:
            SimulatedResult with outcome and state changes

        Note:
            If no historical data exists for this action+state combination,
            the simulation fails. This indicates a gap in test data coverage.
        """
        try:
            # Extract action details
            action_type = getattr(action, 'type', getattr(action, 'action_type', 'UNKNOWN'))
            action_target = self._extract_target(action)

            logger.debug(
                f"Simulating {action_type} on {action_target} "
                f"with states {sorted(self.current_states)}"
            )

            # Find all matching historical captures
            matching = self.history.find_matching(
                self.current_states,
                action_type,
                action_target
            )

            if not matching:
                error_msg = (
                    f"No historical data for {action_type} on {action_target} "
                    f"with active states {sorted(self.current_states)}"
                )
                logger.warning(error_msg)

                result = SimulatedResult(
                    success=False,
                    previous_states=self.current_states.copy(),
                    resulting_states=self.current_states.copy(),
                    error=error_msg
                )
                self.execution_log.append(result)
                return result

            # RANDOM selection - makes each test run different!
            # This simulates the non-determinism of real automation
            selected = random.choice(matching)

            logger.info(
                f"Selected 1 of {len(matching)} matches for {action_type} "
                f"(capture {selected.capture_id})"
            )

            # Update simulated state based on selected capture
            previous_states = self.current_states.copy()
            self.current_states = set(selected.active_states_after)

            result = SimulatedResult(
                success=True,
                previous_states=previous_states,
                resulting_states=self.current_states.copy(),
                selected_match=selected,
                total_matches=len(matching)
            )

            self.execution_log.append(result)
            return result

        except Exception as e:
            logger.error(f"Error during action simulation: {e}", exc_info=True)

            result = SimulatedResult(
                success=False,
                previous_states=self.current_states.copy(),
                resulting_states=self.current_states.copy(),
                error=f"Simulation error: {str(e)}"
            )
            self.execution_log.append(result)
            return result

    def get_current_states(self) -> Set[str]:
        """Get the current simulated GUI states.

        Returns:
            Set of currently active state names (copy, not reference)
        """
        return self.current_states.copy()

    def get_execution_log(self) -> list[SimulatedResult]:
        """Get the log of all simulated executions.

        Useful for debugging and analyzing test runs.

        Returns:
            List of all SimulatedResults from this session
        """
        return self.execution_log.copy()

    def reset(self):
        """Reset the simulator to empty state.

        Clears current states and execution log.
        """
        self.current_states.clear()
        self.execution_log.clear()
        logger.info("Simulator reset")

    def _extract_target(self, action: Any) -> str:
        """Extract action target identifier from action object.

        This tries to get a simplified target identifier that can be matched
        against historical captures. It handles various action object formats.

        Args:
            action: Action object (can be dict, object with attributes, etc.)

        Returns:
            Target identifier string
        """
        # Try different ways to get the target
        if isinstance(action, dict):
            config = action.get('config', {})
            action_type = action.get('type', action.get('action_type', ''))

            # Extract based on action type
            if action_type == 'CLICK':
                return config.get('imageName', config.get('text', 'unknown'))
            elif action_type == 'TYPE':
                return config.get('field', 'unknown')
            elif action_type == 'GO_TO_STATE':
                return config.get('target_state', 'unknown')
            elif action_type == 'RUN_WORKFLOW':
                return config.get('workflow_name', 'unknown')
            else:
                return config.get('target', 'unknown')

        # Try object attributes
        if hasattr(action, 'target'):
            return str(action.target)
        if hasattr(action, 'config'):
            config = action.config
            if isinstance(config, dict):
                return config.get('imageName', config.get('text', 'unknown'))

        # Fallback
        return 'unknown'

    def get_statistics(self) -> dict:
        """Get statistics about the simulation session.

        Returns:
            Dictionary with:
                - total_actions: Number of actions simulated
                - successful_actions: Number that succeeded
                - failed_actions: Number that failed
                - current_states: Currently active states
                - state_changes: Number of times states changed
                - unique_states_visited: Number of unique state combinations
        """
        successful = sum(1 for r in self.execution_log if r.success)
        failed = sum(1 for r in self.execution_log if not r.success)
        state_changes = sum(1 for r in self.execution_log if r.state_changed)

        # Track unique state combinations
        unique_states = set()
        for result in self.execution_log:
            unique_states.add(frozenset(result.previous_states))
            unique_states.add(frozenset(result.resulting_states))

        return {
            "total_actions": len(self.execution_log),
            "successful_actions": successful,
            "failed_actions": failed,
            "current_states": sorted(self.current_states),
            "state_changes": state_changes,
            "unique_states_visited": len(unique_states)
        }
