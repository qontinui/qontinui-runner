"""Action definitions registry for Qontinui Runner.

This module provides the single source of truth for action type metadata,
including expandability, library requirements, and child emission behavior.

Following the Single Responsibility Principle, this module:
- Defines action properties declaratively
- Provides lookup functions for action metadata
- Does NOT execute actions or manage execution state
"""

from dataclasses import dataclass


@dataclass
class ActionDefinition:
    """Defines the properties and behavior of an action type.

    Attributes:
        type: The action type identifier (e.g., "CLICK", "GO_TO_STATE")
        is_expandable: Whether this action can contain child nodes in the execution tree
        requires_library: Whether this action requires the qontinui library to execute
        emits_children: Whether this action launches child workflows/actions
        icon: Icon identifier for frontend display
        description: Human-readable description of what this action does
    """

    type: str
    is_expandable: bool
    requires_library: bool = True
    emits_children: bool = False
    icon: str = "activity"
    description: str = ""

    def to_dict(self) -> dict:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the action definition
        """
        return {
            "type": self.type,
            "is_expandable": self.is_expandable,
            "requires_library": self.requires_library,
            "emits_children": self.emits_children,
            "icon": self.icon,
            "description": self.description,
        }


# Single source of truth for action metadata
ACTION_DEFINITIONS: dict[str, ActionDefinition] = {
    "GO_TO_STATE": ActionDefinition(
        type="GO_TO_STATE",
        is_expandable=True,
        requires_library=False,
        emits_children=True,
        icon="target",
        description="Navigate to a specific application state by executing its entry workflow",
    ),
    "RUN_WORKFLOW": ActionDefinition(
        type="RUN_WORKFLOW",
        is_expandable=True,
        requires_library=False,
        emits_children=True,
        icon="workflow",
        description="Execute a nested workflow",
    ),
    "RUN_PROCESS": ActionDefinition(
        type="RUN_PROCESS",
        is_expandable=True,
        requires_library=False,
        emits_children=True,
        icon="layers",
        description="Execute a business process consisting of multiple workflows",
    ),
    "IF": ActionDefinition(
        type="IF",
        is_expandable=True,
        requires_library=True,
        emits_children=False,
        icon="git-branch",
        description="Conditional branching based on evaluation result",
    ),
    "CLICK": ActionDefinition(
        type="CLICK",
        is_expandable=False,
        requires_library=True,
        icon="pointer",
        description="Click at coordinates or image target",
    ),
    "TYPE": ActionDefinition(
        type="TYPE",
        is_expandable=False,
        requires_library=True,
        icon="keyboard",
        description="Type text input",
    ),
    "FIND": ActionDefinition(
        type="FIND",
        is_expandable=False,
        requires_library=True,
        icon="search",
        description="Find an element on screen",
    ),
    "WAIT": ActionDefinition(
        type="WAIT",
        is_expandable=False,
        requires_library=True,
        icon="clock",
        description="Wait for a specified duration or condition",
    ),
    "MOUSE_MOVE": ActionDefinition(
        type="MOUSE_MOVE",
        is_expandable=False,
        requires_library=True,
        icon="move",
        description="Move mouse to coordinates or image target",
    ),
    "SHELL": ActionDefinition(
        type="SHELL",
        is_expandable=False,
        requires_library=True,
        icon="terminal",
        description="Execute a shell command",
    ),
    "SHELL_SCRIPT": ActionDefinition(
        type="SHELL_SCRIPT",
        is_expandable=False,
        requires_library=True,
        icon="file-code",
        description="Execute a multi-line shell script",
    ),
    "AI_PROMPT": ActionDefinition(
        type="AI_PROMPT",
        is_expandable=False,
        requires_library=False,
        icon="sparkles",
        description="Execute a single AI prompt with optional fresh context",
    ),
    "RUN_PROMPT_SEQUENCE": ActionDefinition(
        type="RUN_PROMPT_SEQUENCE",
        is_expandable=True,
        requires_library=False,
        emits_children=True,
        icon="list-tree",
        description="Execute a sequence of AI prompts with context isolation",
    ),
}


def get_action_definition(action_type: str) -> ActionDefinition:
    """Get action definition for a given action type.

    Args:
        action_type: The action type identifier (e.g., "CLICK", "GO_TO_STATE")

    Returns:
        ActionDefinition for the type, or a default non-expandable definition
        for unknown types

    Example:
        >>> definition = get_action_definition("CLICK")
        >>> assert definition.is_expandable == False
        >>> assert definition.requires_library == True

        >>> unknown = get_action_definition("UNKNOWN_TYPE")
        >>> assert unknown.is_expandable == False
        >>> assert unknown.type == "UNKNOWN_TYPE"
    """
    return ACTION_DEFINITIONS.get(
        action_type,
        ActionDefinition(
            type=action_type,
            is_expandable=False,
            requires_library=True,
            icon="activity",
            description=f"Unknown action type: {action_type}",
        ),
    )


def is_expandable_action(action_type: str) -> bool:
    """Check if an action type is expandable.

    Convenience function for common expandability checks.

    Args:
        action_type: The action type identifier

    Returns:
        True if the action can contain child nodes

    Example:
        >>> assert is_expandable_action("GO_TO_STATE") == True
        >>> assert is_expandable_action("CLICK") == False
    """
    return get_action_definition(action_type).is_expandable


def emits_child_workflows(action_type: str) -> bool:
    """Check if an action type emits child workflows/actions.

    Args:
        action_type: The action type identifier

    Returns:
        True if the action launches child workflows or actions

    Example:
        >>> assert emits_child_workflows("RUN_WORKFLOW") == True
        >>> assert emits_child_workflows("IF") == False
    """
    return get_action_definition(action_type).emits_children
