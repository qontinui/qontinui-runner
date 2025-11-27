"""Failure investigation and analysis for workflow tests.

This module provides tools for analyzing failed workflow tests and suggesting
fixes. It examines the historical data, state transitions, and patterns to help
identify why a test failed and what might be done to fix it.

Key Concept:
    When a workflow test fails, it's often because:
    1. The workflow logic has a bug
    2. The historical data doesn't cover this execution path
    3. State transitions aren't what the workflow expected
    4. The action matching logic needs refinement

    This investigator helps distinguish between these cases and provides
    actionable suggestions.
"""

import logging
from dataclasses import dataclass, field
from typing import List, Set, Tuple, Optional, Any, Dict

try:
    from ..models.historical_capture import HistoricalCapture
except ImportError:
    from models.historical_capture import HistoricalCapture

try:
    from .workflow_tester import WorkflowTestResult, StepResult
    from .historical_data import HistoricalData
except ImportError:
    from testing.workflow_tester import WorkflowTestResult, StepResult
    from testing.historical_data import HistoricalData

logger = logging.getLogger(__name__)


@dataclass
class FailureDetail:
    """Detailed analysis of a workflow test failure.

    Attributes:
        workflow_id: ID of the workflow that failed
        workflow_name: Name of the workflow
        failed_step: Index of the step that failed
        action_attempted: The action that was being executed
        expected_states: States that were expected
        actual_states: States that actually occurred
        historical_matches: All matching historical captures (for analysis)
        consistency_score: 0.0-1.0 score of how consistent the historical data is
        possible_issues: List of potential problems identified
        related_video_timestamps: Video references for reviewing the captures
        suggestions: List of suggested fixes
    """
    workflow_id: str
    workflow_name: str
    failed_step: int
    action_attempted: Any
    expected_states: Set[str] = field(default_factory=set)
    actual_states: Set[str] = field(default_factory=set)
    historical_matches: List[HistoricalCapture] = field(default_factory=list)
    consistency_score: float = 0.0
    possible_issues: List[str] = field(default_factory=list)
    related_video_timestamps: List[Tuple[str, float]] = field(default_factory=list)
    suggestions: List[str] = field(default_factory=list)

    @property
    def has_data_gap(self) -> bool:
        """Check if failure was due to missing historical data."""
        return len(self.historical_matches) == 0

    @property
    def has_state_mismatch(self) -> bool:
        """Check if failure was due to state mismatch."""
        return (
            self.expected_states and
            self.actual_states and
            self.expected_states != self.actual_states
        )

    @property
    def is_inconsistent_data(self) -> bool:
        """Check if historical data shows inconsistent behavior."""
        return self.consistency_score < 0.7 and len(self.historical_matches) > 1


class FailureInvestigator:
    """Analyzes failed workflow tests and suggests fixes.

    This investigator examines test failures to determine their root cause
    and provide actionable suggestions for fixing them.

    Usage:
        investigator = FailureInvestigator(historical_data)
        detail = investigator.investigate(failed_result)

        if detail.has_data_gap:
            print("Need more training data!")
        elif detail.has_state_mismatch:
            print(f"Workflow expects wrong states: {detail.suggestions}")

    Attributes:
        history: HistoricalData database for looking up captures
    """

    def __init__(self, history: HistoricalData):
        """Initialize failure investigator.

        Args:
            history: HistoricalData database for analysis
        """
        self.history = history
        logger.info("FailureInvestigator initialized")

    def investigate(self, result: WorkflowTestResult) -> Optional[FailureDetail]:
        """Analyze a failed workflow test result.

        Args:
            result: WorkflowTestResult from a failed test

        Returns:
            FailureDetail with analysis, or None if workflow passed
        """
        if result.passed:
            logger.info(f"Workflow {result.workflow_name} passed - no investigation needed")
            return None

        if not result.failed_at:
            logger.warning("Workflow failed but no failure step recorded")
            return None

        try:
            logger.info(
                f"Investigating failure in {result.workflow_name} "
                f"at step {result.failed_at.step_index}"
            )

            detail = self._analyze_failure(result, result.failed_at)
            detail.suggestions = self.suggest_fixes(detail)

            logger.info(
                f"Investigation complete: {len(detail.possible_issues)} issues identified, "
                f"{len(detail.suggestions)} suggestions"
            )

            return detail

        except Exception as e:
            logger.error(f"Error investigating failure: {e}", exc_info=True)
            return None

    def suggest_fixes(self, detail: FailureDetail) -> List[str]:
        """Suggest possible fixes based on failure analysis.

        Args:
            detail: FailureDetail from investigation

        Returns:
            List of suggested fixes (human-readable)
        """
        suggestions = []

        try:
            # Data gap - need more training data
            if detail.has_data_gap:
                suggestions.append(
                    f"NO HISTORICAL DATA: Need to capture automation runs that include "
                    f"{self._format_action(detail.action_attempted)} "
                    f"with active states {sorted(detail.expected_states or set())}"
                )
                suggestions.append(
                    "Run the automation manually with these states active and import "
                    "the recording into the historical database"
                )

            # State mismatch - workflow expects wrong states
            elif detail.has_state_mismatch:
                suggestions.append(
                    f"STATE MISMATCH: Workflow expects {sorted(detail.expected_states)} "
                    f"but historical data shows {sorted(detail.actual_states)}"
                )

                if detail.consistency_score > 0.9:
                    suggestions.append(
                        f"Historical data is consistent ({detail.consistency_score:.1%}). "
                        "Likely the workflow has incorrect expectations - update the workflow."
                    )
                else:
                    suggestions.append(
                        f"Historical data is inconsistent ({detail.consistency_score:.1%}). "
                        "The action sometimes produces different states. Consider:"
                    )
                    suggestions.append("  - Adding conditional logic to handle both cases")
                    suggestions.append("  - Adding retry/wait logic for state stabilization")
                    suggestions.append("  - Reviewing video captures to understand why behavior varies")

            # Inconsistent data - non-deterministic behavior
            elif detail.is_inconsistent_data:
                suggestions.append(
                    f"INCONSISTENT BEHAVIOR: Action produces different states "
                    f"in {len(detail.historical_matches)} historical captures "
                    f"(consistency: {detail.consistency_score:.1%})"
                )
                suggestions.append("Review the video timestamps to see why behavior varies:")
                for video_path, timestamp in detail.related_video_timestamps[:3]:
                    suggestions.append(f"  - {video_path} at {timestamp:.1f}s")

                suggestions.append("Consider adding state verification and retry logic")

            # Simulation error
            elif detail.actual_states == detail.expected_states:
                # Might be an internal error
                suggestions.append(
                    "SIMULATION ERROR: Check the test framework logs for details"
                )

            # Add general debugging suggestions
            if detail.related_video_timestamps:
                suggestions.append(
                    f"Review {len(detail.related_video_timestamps)} video captures "
                    "to see actual GUI behavior"
                )

        except Exception as e:
            logger.error(f"Error generating suggestions: {e}", exc_info=True)
            suggestions.append(f"Error generating suggestions: {str(e)}")

        return suggestions

    def _analyze_failure(self,
                        result: WorkflowTestResult,
                        failed_step: StepResult) -> FailureDetail:
        """Perform detailed analysis of a failure.

        Args:
            result: Complete workflow test result
            failed_step: The step that failed

        Returns:
            FailureDetail with analysis
        """
        detail = FailureDetail(
            workflow_id=result.workflow_id,
            workflow_name=result.workflow_name,
            failed_step=failed_step.step_index,
            action_attempted=failed_step.action,
            expected_states=failed_step.expected_states,
            actual_states=failed_step.actual_states
        )

        # Get action details
        action_type = self._get_action_type(failed_step.action)
        action_target = self._get_action_target(failed_step.action)

        # Determine what states were active when failure occurred
        # (from previous step's result, or workflow initial states)
        active_states = self._get_states_before_step(result, failed_step.step_index)

        # Look up all historical matches for this context
        detail.historical_matches = self.history.find_matching(
            active_states,
            action_type,
            action_target
        )

        # Calculate consistency score
        detail.consistency_score = self._calculate_consistency(detail.historical_matches)

        # Identify possible issues
        detail.possible_issues = self._identify_issues(detail, active_states)

        # Extract video references
        detail.related_video_timestamps = self._extract_video_references(
            detail.historical_matches
        )

        return detail

    def _calculate_consistency(self, matches: List[HistoricalCapture]) -> float:
        """Calculate how consistent the historical data is.

        If all matches produce the same resulting states, consistency is 1.0.
        If they produce different states, consistency is lower.

        Args:
            matches: List of historical captures to analyze

        Returns:
            Consistency score from 0.0 (completely inconsistent) to 1.0 (perfectly consistent)
        """
        if len(matches) == 0:
            return 0.0

        if len(matches) == 1:
            return 1.0

        # Count how often each result state set appears
        state_counts: Dict[frozenset, int] = {}
        for match in matches:
            state_counts[match.active_states_after] = state_counts.get(
                match.active_states_after, 0
            ) + 1

        # Most common result
        max_count = max(state_counts.values())

        # Consistency is the proportion that match the most common result
        consistency = max_count / len(matches)

        return consistency

    def _identify_issues(self,
                        detail: FailureDetail,
                        active_states: Set[str]) -> List[str]:
        """Identify specific issues based on the failure.

        Args:
            detail: Current failure detail (being built)
            active_states: States that were active when failure occurred

        Returns:
            List of issue descriptions
        """
        issues = []

        # Check for data gap
        if len(detail.historical_matches) == 0:
            issues.append(
                f"No historical data for {self._format_action(detail.action_attempted)} "
                f"with active states {sorted(active_states)}"
            )
            issues.append("This execution path has never been captured")

        # Check for state mismatch
        elif detail.has_state_mismatch:
            issues.append(
                f"Expected states {sorted(detail.expected_states)} "
                f"but got {sorted(detail.actual_states)}"
            )

            # Check if expected states appear in ANY match
            expected_in_matches = any(
                m.active_states_after == detail.expected_states
                for m in detail.historical_matches
            )

            if expected_in_matches:
                issues.append(
                    "Expected states DO appear in historical data, but rarely. "
                    "This suggests non-deterministic behavior."
                )
            else:
                issues.append(
                    "Expected states NEVER appear in historical data. "
                    "Workflow expectations may be incorrect."
                )

        # Check for inconsistency
        if detail.is_inconsistent_data:
            unique_results = len(set(
                m.active_states_after for m in detail.historical_matches
            ))
            issues.append(
                f"Action produces {unique_results} different state outcomes "
                f"in {len(detail.historical_matches)} captures "
                f"(consistency: {detail.consistency_score:.1%})"
            )

        return issues

    def _extract_video_references(self,
                                  matches: List[HistoricalCapture]) -> List[Tuple[str, float]]:
        """Extract video references from historical matches.

        Args:
            matches: List of historical captures

        Returns:
            List of (video_path, timestamp) tuples
        """
        references = []

        for match in matches:
            if match.video_path and match.video_timestamp is not None:
                references.append((match.video_path, match.video_timestamp))

        return references

    def _get_states_before_step(self,
                                result: WorkflowTestResult,
                                step_index: int) -> Set[str]:
        """Get states that were active before the given step.

        Args:
            result: Workflow test result
            step_index: Index of the step

        Returns:
            Set of active state names
        """
        if step_index == 0:
            return result.initial_states

        # Get from previous step's result
        if step_index > 0 and len(result.step_results) > step_index - 1:
            prev_step = result.step_results[step_index - 1]
            return prev_step.actual_states

        return set()

    def _get_action_type(self, action: Any) -> str:
        """Extract action type from action object."""
        if isinstance(action, dict):
            return action.get('type', action.get('action_type', 'UNKNOWN'))
        return getattr(action, 'type', getattr(action, 'action_type', 'UNKNOWN'))

    def _get_action_target(self, action: Any) -> str:
        """Extract action target from action object."""
        if isinstance(action, dict):
            config = action.get('config', {})
            action_type = self._get_action_type(action)

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

        if hasattr(action, 'target'):
            return str(action.target)
        if hasattr(action, 'config'):
            config = action.config
            if isinstance(config, dict):
                return config.get('imageName', config.get('text', 'unknown'))

        return 'unknown'

    def _format_action(self, action: Any) -> str:
        """Format action for display in messages.

        Args:
            action: Action object

        Returns:
            Human-readable action description
        """
        action_type = self._get_action_type(action)
        action_target = self._get_action_target(action)
        return f"{action_type}({action_target})"
