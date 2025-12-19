"""HistoricalCapture data model for video capture architecture.

This module defines the HistoricalCapture model, which represents a single
recorded interaction from a capture session. It captures the complete context
of a user action and its effect on the application state.

HistoricalCaptures form a chronological record of all interactions during
a capture session, enabling replay, analysis, and training data generation.
"""

from dataclasses import dataclass, field
from typing import Any


@dataclass
class HistoricalCapture:
    """Represents a single recorded interaction from a capture session.

    A HistoricalCapture records the complete context around a user action,
    including:
    - What states were active before and after the action
    - What action was performed (type, target, parameters)
    - Which states appeared or disappeared as a result
    - Frame and timing information

    This model is primarily used for:
    - Creating a chronological log of capture sessions
    - Generating training data for ML models
    - Replaying captured sessions
    - Analyzing user interaction patterns

    Attributes:
        session_id: Unique identifier for the capture session
        timestamp: Unix timestamp when the action occurred
        frame_before: Frame number immediately before the action
        frame_after: Frame number immediately after the action
        active_states_before: Set of state IDs that were active before the action
        action_type: Type of action performed ("click", "key_press", "drag", etc.)
        action_target: Target of the action (StateImage ID, coordinates, etc.)
        action_params: Dictionary of action-specific parameters
        active_states_after: Set of state IDs that were active after the action
        states_appeared: Set of state IDs that appeared during this interaction
        states_disappeared: Set of state IDs that disappeared during this interaction
        metadata: Additional arbitrary data for extensibility
    """

    session_id: str
    timestamp: float
    frame_before: int
    frame_after: int
    active_states_before: set[str]
    action_type: str
    action_target: str
    action_params: dict[str, Any]
    active_states_after: set[str]
    states_appeared: set[str]
    states_disappeared: set[str]
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def is_state_change(self) -> bool:
        """Check if this action resulted in a state change.

        Returns:
            True if any states appeared or disappeared, False otherwise
        """
        return len(self.states_appeared) > 0 or len(self.states_disappeared) > 0

    @property
    def num_states_before(self) -> int:
        """Get the number of states active before the action.

        Returns:
            Count of active states before
        """
        return len(self.active_states_before)

    @property
    def num_states_after(self) -> int:
        """Get the number of states active after the action.

        Returns:
            Count of active states after
        """
        return len(self.active_states_after)

    @property
    def frame_count(self) -> int:
        """Calculate the number of frames between before and after.

        Returns:
            Number of frames (after - before)
        """
        return self.frame_after - self.frame_before

    @property
    def state_change_magnitude(self) -> int:
        """Calculate the magnitude of state change.

        Returns:
            Total number of states that changed (appeared + disappeared)
        """
        return len(self.states_appeared) + len(self.states_disappeared)

    def has_action_param(self, param_name: str) -> bool:
        """Check if a specific action parameter exists.

        Args:
            param_name: Name of the parameter to check

        Returns:
            True if parameter exists in action_params
        """
        return param_name in self.action_params

    def get_action_param(self, param_name: str, default: Any = None) -> Any:
        """Get an action parameter value with optional default.

        Args:
            param_name: Name of the parameter to retrieve
            default: Default value if parameter doesn't exist

        Returns:
            Parameter value or default
        """
        return self.action_params.get(param_name, default)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization.

        Returns:
            Dictionary containing all serializable fields
        """
        return {
            "session_id": self.session_id,
            "timestamp": self.timestamp,
            "frame_before": self.frame_before,
            "frame_after": self.frame_after,
            "active_states_before": sorted(self.active_states_before),
            "action_type": self.action_type,
            "action_target": self.action_target,
            "action_params": self.action_params,
            "active_states_after": sorted(self.active_states_after),
            "states_appeared": sorted(self.states_appeared),
            "states_disappeared": sorted(self.states_disappeared),
            "is_state_change": self.is_state_change,
            "num_states_before": self.num_states_before,
            "num_states_after": self.num_states_after,
            "frame_count": self.frame_count,
            "state_change_magnitude": self.state_change_magnitude,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "HistoricalCapture":
        """Create HistoricalCapture from dictionary.

        Args:
            data: Dictionary containing capture data

        Returns:
            New HistoricalCapture instance
        """
        return cls(
            session_id=data["session_id"],
            timestamp=data["timestamp"],
            frame_before=data["frame_before"],
            frame_after=data["frame_after"],
            active_states_before=set(data["active_states_before"]),
            action_type=data["action_type"],
            action_target=data["action_target"],
            action_params=data["action_params"],
            active_states_after=set(data["active_states_after"]),
            states_appeared=set(data["states_appeared"]),
            states_disappeared=set(data["states_disappeared"]),
            metadata=data.get("metadata", {}),
        )

    def __repr__(self) -> str:
        """String representation for debugging."""
        return (
            f"HistoricalCapture(session={self.session_id}, "
            f"action={self.action_type}, "
            f"target={self.action_target}, "
            f"frames=[{self.frame_before}, {self.frame_after}], "
            f"state_change={self.is_state_change})"
        )
