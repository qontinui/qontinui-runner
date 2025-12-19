"""Tree view layer service for transforming execution records into various tree views.

This module provides pure, stateless functions for transforming flat lists of
ActionExecutionRecord objects into hierarchical tree structures optimized for
different visualization needs.

All methods are static and produce new data structures without side effects,
making them easy to test and reason about.
"""

from dataclasses import dataclass, field
from typing import Any

from models.action_execution_record import ActionExecutionRecord


@dataclass
class TreeNode:
    """A node in a tree view representation.

    This is a generic tree node structure that can represent different
    types of hierarchies depending on which view transformation created it.

    Attributes:
        id: Unique identifier for this node
        type: Type of node (e.g., "workflow", "action", "state_group", "timestamp")
        label: Human-readable label for display
        data: Additional data specific to this node
        children: Child nodes
        metadata: Extra metadata for rendering (icons, colors, etc.)
    """

    id: str
    type: str
    label: str
    data: dict[str, Any] = field(default_factory=dict)
    children: list["TreeNode"] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the node and its children
        """
        return {
            "id": self.id,
            "type": self.type,
            "label": self.label,
            "data": self.data,
            "children": [child.to_dict() for child in self.children],
            "metadata": self.metadata,
        }


class TreeViewLayer:
    """Pure, stateless transformations for building tree views from execution records.

    All methods are static and take a list of ActionExecutionRecord objects
    as input, returning a list of TreeNode objects representing the desired view.

    These transformations are composable and can be chained or combined to
    create custom views.
    """

    @staticmethod
    def execution_tree_view(records: list[ActionExecutionRecord]) -> list[TreeNode]:
        """Build hierarchical tree based on workflow and parent-child relationships.

        This view recreates the execution hierarchy as it happened, with workflows
        as top-level nodes and actions nested according to their parent_action_id.

        Structure:
            Workflow A
            ├── Action 1
            │   ├── Action 1.1
            │   └── Action 1.2
            └── Action 2
                └── Workflow B (nested)
                    └── Action 2.1

        Args:
            records: List of action execution records

        Returns:
            List of root TreeNode objects representing the execution hierarchy
        """
        if not records:
            return []

        # Build lookup maps
        node_map: dict[str, TreeNode] = {}
        workflow_map: dict[str, TreeNode] = {}

        # First pass: Create nodes for all records
        for record in records:
            status = "success" if record.success else "failed" if record.end_time else "running"

            node = TreeNode(
                id=record.action_id,
                type="action",
                label=TreeViewLayer._format_action_label(record),
                data={
                    "action_type": record.action_type,
                    "timestamp": record.start_time,
                    "duration": record.duration_seconds,
                    "status": status,
                    "error": record.error_message,
                },
                metadata={
                    "workflow_id": record.workflow_id,
                    "depth": record.nesting_level,
                    "has_state_change": record.state_changed,
                },
            )
            node_map[record.action_id] = node

            # Track workflow nodes
            if record.workflow_id and record.workflow_id not in workflow_map:
                workflow_node = TreeNode(
                    id=record.workflow_id,
                    type="workflow",
                    label=record.workflow_id,
                    data={"workflow_id": record.workflow_id},
                    metadata={"is_workflow": True},
                )
                workflow_map[record.workflow_id] = workflow_node

        # Second pass: Build hierarchy
        roots: list[TreeNode] = []

        for record in records:
            node = node_map[record.action_id]

            if record.parent_action_id and record.parent_action_id in node_map:
                # Has a parent action - add as child
                parent_node = node_map[record.parent_action_id]
                parent_node.children.append(node)
            elif record.workflow_id and record.workflow_id in workflow_map:
                # No parent action - add to workflow
                workflow_node = workflow_map[record.workflow_id]
                workflow_node.children.append(node)
            else:
                # No parent - add as root
                roots.append(node)

        # Add workflows with children to roots
        for workflow_node in workflow_map.values():
            if workflow_node.children:
                roots.append(workflow_node)

        return roots

    @staticmethod
    def action_only_tree_view(records: list[ActionExecutionRecord]) -> list[TreeNode]:
        """Build flat list of actions without hierarchy, chronologically ordered.

        This view shows all actions in execution order without nesting,
        useful for timeline analysis or simple action lists.

        Structure:
            Action 1
            Action 1.1
            Action 1.2
            Action 2
            Action 2.1

        Args:
            records: List of action execution records

        Returns:
            List of TreeNode objects in chronological order (no hierarchy)
        """
        if not records:
            return []

        # Sort by timestamp
        sorted_records = sorted(records, key=lambda r: r.start_time)

        nodes = []
        for record in sorted_records:
            status = "success" if record.success else "failed" if record.end_time else "running"

            node = TreeNode(
                id=record.action_id,
                type="action",
                label=TreeViewLayer._format_action_label(record),
                data={
                    "action_type": record.action_type,
                    "timestamp": record.start_time,
                    "duration": record.duration_seconds,
                    "status": status,
                    "error": record.error_message,
                },
                metadata={
                    "workflow_id": record.workflow_id,
                    "depth": record.nesting_level,
                    "parent_action_id": record.parent_action_id,
                },
            )
            nodes.append(node)

        return nodes

    @staticmethod
    def state_transition_tree_view(records: list[ActionExecutionRecord]) -> list[TreeNode]:
        """Group actions by state changes they caused.

        This view organizes actions based on state transitions, making it easy
        to see which actions caused which state changes.

        Structure:
            State Changes
            ├── Entered: StateA
            │   ├── Action 1 (GO_TO_STATE)
            │   └── Action 3 (FIND)
            ├── Exited: StateB
            │   └── Action 1 (GO_TO_STATE)
            └── No State Change
                ├── Action 2 (CLICK)
                └── Action 4 (WAIT)

        Args:
            records: List of action execution records

        Returns:
            List of TreeNode objects grouped by state changes
        """
        if not records:
            return []

        # Group by state changes
        entered_states: dict[str, list[ActionExecutionRecord]] = {}
        exited_states: dict[str, list[ActionExecutionRecord]] = {}
        no_change_actions: list[ActionExecutionRecord] = []

        for record in records:
            if record.state_changed:
                for state in record.states_activated:
                    if state not in entered_states:
                        entered_states[state] = []
                    entered_states[state].append(record)

                for state in record.states_deactivated:
                    if state not in exited_states:
                        exited_states[state] = []
                    exited_states[state].append(record)
            else:
                no_change_actions.append(record)

        roots = []

        # Create "Entered" group
        if entered_states:
            entered_node = TreeNode(
                id="state_entered",
                type="state_group",
                label="States Entered",
                metadata={"group_type": "entered"},
            )

            for state_name, state_records in sorted(entered_states.items()):
                state_node = TreeNode(
                    id=f"entered_{state_name}",
                    type="state",
                    label=f"→ {state_name}",
                    data={"state_name": state_name, "change_type": "entered"},
                )

                for record in sorted(state_records, key=lambda r: r.start_time):
                    status = (
                        "success" if record.success else "failed" if record.end_time else "running"
                    )
                    action_node = TreeNode(
                        id=record.action_id,
                        type="action",
                        label=TreeViewLayer._format_action_label(record),
                        data={
                            "action_type": record.action_type,
                            "timestamp": record.start_time,
                            "status": status,
                        },
                    )
                    state_node.children.append(action_node)

                entered_node.children.append(state_node)

            roots.append(entered_node)

        # Create "Exited" group
        if exited_states:
            exited_node = TreeNode(
                id="state_exited",
                type="state_group",
                label="States Exited",
                metadata={"group_type": "exited"},
            )

            for state_name, state_records in sorted(exited_states.items()):
                state_node = TreeNode(
                    id=f"exited_{state_name}",
                    type="state",
                    label=f"← {state_name}",
                    data={"state_name": state_name, "change_type": "exited"},
                )

                for record in sorted(state_records, key=lambda r: r.start_time):
                    status = (
                        "success" if record.success else "failed" if record.end_time else "running"
                    )
                    action_node = TreeNode(
                        id=record.action_id,
                        type="action",
                        label=TreeViewLayer._format_action_label(record),
                        data={
                            "action_type": record.action_type,
                            "timestamp": record.start_time,
                            "status": status,
                        },
                    )
                    state_node.children.append(action_node)

                exited_node.children.append(state_node)

            roots.append(exited_node)

        # Create "No Change" group
        if no_change_actions:
            no_change_node = TreeNode(
                id="no_state_change",
                type="state_group",
                label="No State Change",
                metadata={"group_type": "no_change"},
            )

            for record in sorted(no_change_actions, key=lambda r: r.start_time):
                status = "success" if record.success else "failed" if record.end_time else "running"
                action_node = TreeNode(
                    id=record.action_id,
                    type="action",
                    label=TreeViewLayer._format_action_label(record),
                    data={
                        "action_type": record.action_type,
                        "timestamp": record.start_time,
                        "status": status,
                    },
                )
                no_change_node.children.append(action_node)

            roots.append(no_change_node)

        return roots

    @staticmethod
    def timeline_tree_view(records: list[ActionExecutionRecord]) -> list[TreeNode]:
        """Build chronological view with relative timestamps.

        This view presents actions in time order with formatted timestamps
        showing time elapsed from the start of execution.

        Structure:
            [0.000s] Workflow: Main
            [0.123s] └─ Action: FIND button
            [0.456s]    └─ Action: CLICK button
            [0.789s] └─ Action: WAIT 1000ms

        Args:
            records: List of action execution records

        Returns:
            List of TreeNode objects with timeline formatting
        """
        if not records:
            return []

        # Sort by timestamp
        sorted_records = sorted(records, key=lambda r: r.start_time)

        # Get start time for relative timestamps
        start_time = sorted_records[0].start_time if sorted_records else 0

        # Build hierarchy similar to execution_tree_view but with timeline labels
        node_map: dict[str, TreeNode] = {}
        roots: list[TreeNode] = []

        for record in sorted_records:
            relative_time = record.start_time - start_time
            status = "success" if record.success else "failed" if record.end_time else "running"

            node = TreeNode(
                id=record.action_id,
                type="action",
                label=TreeViewLayer._format_timeline_label(record, relative_time),
                data={
                    "action_type": record.action_type,
                    "timestamp": record.start_time,
                    "relative_time": relative_time,
                    "duration": record.duration_seconds,
                    "status": status,
                },
                metadata={
                    "workflow_id": record.workflow_id,
                    "depth": record.nesting_level,
                },
            )
            node_map[record.action_id] = node

            # Build hierarchy
            if record.parent_action_id and record.parent_action_id in node_map:
                parent_node = node_map[record.parent_action_id]
                parent_node.children.append(node)
            else:
                roots.append(node)

        return roots

    @staticmethod
    def state_grouped_tree_view(records: list[ActionExecutionRecord]) -> list[TreeNode]:
        """Group actions by active state sets.

        This view groups actions by which combination of states were active
        when they executed, useful for understanding state-dependent behavior.

        Structure:
            Active States: {StateA, StateB}
            ├── Action 1
            └── Action 2
            Active States: {StateB, StateC}
            ├── Action 3
            └── Action 4
            Active States: {}
            └── Action 5

        Args:
            records: List of action execution records

        Returns:
            List of TreeNode objects grouped by active state sets
        """
        if not records:
            return []

        # Group by active state sets (active_states_after is the active set)
        state_groups: dict[frozenset, list[ActionExecutionRecord]] = {}

        for record in records:
            state_set = record.active_states_after
            if state_set not in state_groups:
                state_groups[state_set] = []
            state_groups[state_set].append(record)

        roots = []

        # Sort groups by first action timestamp
        sorted_groups = sorted(
            state_groups.items(), key=lambda item: min(r.start_time for r in item[1])
        )

        for state_set, group_records in sorted_groups:
            # Format state set label
            if state_set:
                state_list = sorted(state_set)
                state_label = "{" + ", ".join(state_list) + "}"
            else:
                state_label = "{no active states}"

            group_node = TreeNode(
                id=f"state_group_{hash(state_set)}",
                type="state_group",
                label=f"Active States: {state_label}",
                data={
                    "active_states": list(state_set),
                    "action_count": len(group_records),
                },
                metadata={"is_state_group": True},
            )

            # Add actions to group
            for record in sorted(group_records, key=lambda r: r.start_time):
                status = "success" if record.success else "failed" if record.end_time else "running"

                # Build state_changes dict from properties
                state_changes = {}
                for state in record.states_activated:
                    state_changes[state] = "entered"
                for state in record.states_deactivated:
                    state_changes[state] = "exited"

                action_node = TreeNode(
                    id=record.action_id,
                    type="action",
                    label=TreeViewLayer._format_action_label(record),
                    data={
                        "action_type": record.action_type,
                        "timestamp": record.start_time,
                        "duration": record.duration_seconds,
                        "status": status,
                        "state_changes": state_changes,
                    },
                    metadata={
                        "workflow_id": record.workflow_id,
                        "has_state_change": record.state_changed,
                    },
                )
                group_node.children.append(action_node)

            roots.append(group_node)

        return roots

    @staticmethod
    def _format_action_label(record: ActionExecutionRecord) -> str:
        """Format a human-readable label for an action.

        Args:
            record: The action execution record

        Returns:
            Formatted label string
        """
        # Base label with action type
        label = record.action_type

        # Add status indicator
        if record.success:
            status_icon = "✓"
        elif record.end_time:
            status_icon = "✗"
        else:
            status_icon = "⟳"

        # Add duration if available
        duration_str = ""
        if record.duration_seconds is not None:
            duration_str = f" ({record.duration_seconds * 1000:.0f}ms)"

        # Add state change indicator
        state_change_str = ""
        if record.state_changed:
            changes = []
            if record.states_activated:
                changes.append(f"+{','.join(sorted(record.states_activated))}")
            if record.states_deactivated:
                changes.append(f"-{','.join(sorted(record.states_deactivated))}")
            state_change_str = f" [{' '.join(changes)}]"

        return f"{status_icon} {label}{duration_str}{state_change_str}"

    @staticmethod
    def _format_state_change(record: ActionExecutionRecord) -> str:
        """Format state changes as a human-readable string.

        Args:
            record: The action execution record

        Returns:
            Formatted state change string or empty string if no changes
        """
        if not record.state_changed:
            return ""

        changes = []
        if record.states_activated:
            changes.append("Entered: " + ", ".join(sorted(record.states_activated)))
        if record.states_deactivated:
            changes.append("Exited: " + ", ".join(sorted(record.states_deactivated)))

        return " | ".join(changes)

    @staticmethod
    def _format_timeline_label(record: ActionExecutionRecord, relative_time: float) -> str:
        """Format a timeline label with relative timestamp.

        Args:
            record: The action execution record
            relative_time: Time in seconds relative to start

        Returns:
            Formatted timeline label
        """
        timestamp_str = f"[{relative_time:.3f}s]"
        action_label = TreeViewLayer._format_action_label(record)
        return f"{timestamp_str} {action_label}"
