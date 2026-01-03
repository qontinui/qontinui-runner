"""Execution tree module for tracking workflow/action hierarchy during execution.

This module provides the core data structures for building an execution tree
as workflows and actions are executed, replacing the old stack-based approach.

Following the Single Responsibility Principle, this module:
- Defines the ExecutionNode and ExecutionTree data structures
- Manages tree construction during execution
- Provides hierarchy metadata for event emission
- Does NOT emit events or execute actions
"""

import time
from dataclasses import dataclass, field
from typing import Any, Optional


@dataclass
class ExecutionNode:
    """A node in the execution tree (built during execution).

    Represents a workflow, transition, or action in the execution hierarchy.
    Parent-child relationships are established immediately when nodes are added.

    Attributes:
        id: Unique identifier for this node
        node_type: Type of node ("workflow", "transition", or "action")
        name: Display name (workflow name, transition name, or action type)
        timestamp: Unix timestamp when node was created
        parent: Reference to parent node (None for root)
        children: List of child nodes
        metadata: Additional data (config, action definition, transition type, target states, etc.)
        status: Current execution status
        error: Error message if execution failed
    """

    id: str
    node_type: str  # "workflow" | "transition" | "action"
    name: str
    timestamp: float  # Start time (Unix timestamp)
    end_timestamp: float | None = None  # End time (Unix timestamp)
    parent: Optional["ExecutionNode"] = None
    children: list["ExecutionNode"] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)
    status: str = "pending"  # "pending" | "running" | "success" | "failed"
    error: str | None = None
    _timestamp_synced: bool = False  # Whether timestamp was synced with first child

    @property
    def duration(self) -> float | None:
        """Calculate duration in seconds.

        Returns:
            Duration in seconds if node has completed, None otherwise
        """
        if self.end_timestamp is None:
            return None
        return self.end_timestamp - self.timestamp

    def add_child(self, child: "ExecutionNode") -> "ExecutionNode":
        """Add a child node and set its parent reference.

        When the first child is added to an action node, the parent's timestamp
        is synchronized to match the child's timestamp. This ensures that parent
        actions (like GO_TO_STATE) have timestamps that reflect when their
        children (like Outgoing Transition) actually execute, not when the
        parent was initially created.

        Args:
            child: The child node to add

        Returns:
            The child node (for chaining)
        """
        child.parent = self
        self.children.append(child)

        # Sync parent timestamp with first child for action nodes
        # This ensures GO_TO_STATE has the same timestamp as its transitions
        if not self._timestamp_synced and self.node_type == "action":
            self.timestamp = child.timestamp
            self._timestamp_synced = True

        return child

    def to_dict(self, include_children: bool = True) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization.

        Args:
            include_children: Whether to recursively include child nodes

        Returns:
            Dictionary representation of the node
        """
        result = {
            "id": self.id,
            "node_type": self.node_type,
            "name": self.name,
            "timestamp": self.timestamp,
            "end_timestamp": self.end_timestamp,
            "duration": self.duration,
            "parent_id": self.parent.id if self.parent else None,
            "status": self.status,
            "metadata": self.metadata,
            "error": self.error,
        }
        if include_children:
            result["children"] = [child.to_dict() for child in self.children]
        return result

    def get_depth(self) -> int:
        """Calculate depth from root (root = 0).

        Returns:
            Depth level in the tree
        """
        depth = 0
        node = self
        while node.parent:
            depth += 1
            node = node.parent
        return depth

    def get_path_from_root(self) -> list[dict[str, str]]:
        """Get path from root to this node.

        Returns:
            List of {id, name, type} dictionaries from root to this node
        """
        path: list[dict[str, str]] = []
        node: ExecutionNode | None = self
        while node:
            path.insert(0, {"id": node.id, "name": node.name, "type": node.node_type})
            node = node.parent
        return path


class ExecutionTree:
    """Manages the execution tree during workflow execution.

    Builds the tree structure as workflows and actions are executed,
    maintaining a "current" pointer that moves through the tree.

    The tree is built eagerly - parent nodes always exist before children
    are added, eliminating timing issues.
    """

    def __init__(self):
        """Initialize an empty execution tree."""
        self.root: ExecutionNode | None = None
        self.current: ExecutionNode | None = None
        self.node_map: dict[str, ExecutionNode] = {}

    def start_workflow(
        self,
        workflow_id: str,
        workflow_name: str,
        metadata: dict[str, Any] | None = None,
        is_inline: bool = False,
    ) -> ExecutionNode:
        """Start a new workflow (creates root or child workflow node).

        Args:
            workflow_id: Unique identifier for the workflow
            workflow_name: Display name of the workflow
            metadata: Optional additional data
            is_inline: If True, this is an inline workflow (transition workflow)
                      that shouldn't be displayed as a separate node

        Returns:
            The created workflow node
        """
        # Merge is_inline into metadata for frontend filtering
        merged_metadata = metadata or {}
        merged_metadata["is_inline"] = is_inline

        node = ExecutionNode(
            id=workflow_id,
            node_type="workflow",
            name=workflow_name,
            timestamp=time.time(),
            status="running",
            metadata=merged_metadata,
        )
        self.node_map[workflow_id] = node

        if self.root is None:
            # First workflow - set as root
            self.root = node
            self.current = node
        else:
            # Nested workflow - add as child of current node
            if self.current is not None:
                self.current.add_child(node)
            self.current = node

        return node

    def end_workflow(self, workflow_id: str, success: bool, error: str | None = None) -> None:
        """Mark workflow as complete and move up the tree.

        If any child action/transition failed, the workflow is also marked as failed.

        Args:
            workflow_id: ID of the workflow to complete
            success: Whether the workflow completed successfully
            error: Optional error message if failed
        """
        node = self.node_map.get(workflow_id)
        if node is not None:
            # Check if any child failed - if so, this workflow should also fail
            child_failed = any(child.status == "failed" for child in node.children)
            if child_failed and success:
                success = False
                if not error:
                    failed_child = next((c for c in node.children if c.status == "failed"), None)
                    if failed_child:
                        error = f"Child '{failed_child.name}' failed"

            node.status = "success" if success else "failed"
            node.error = error
            node.end_timestamp = time.time()
            # Move current pointer to parent
            if node.parent is not None:
                self.current = node.parent

    def start_transition(
        self, transition_id: str, transition_name: str, metadata: dict[str, Any] | None = None
    ) -> ExecutionNode:
        """Start a transition under the current workflow.

        Args:
            transition_id: Unique identifier for the transition
            transition_name: Display name of the transition (e.g., "outgoing (processing, inventory)")
            metadata: Optional additional data (transition type, target states, etc.)

        Returns:
            The created transition node
        """
        node = ExecutionNode(
            id=transition_id,
            node_type="transition",
            name=transition_name,
            timestamp=time.time(),
            status="running",
            metadata=metadata or {},
        )
        self.node_map[transition_id] = node

        # Add as child of current node
        if self.current:
            self.current.add_child(node)
        self.current = node

        return node

    def end_transition(self, transition_id: str, success: bool, error: str | None = None) -> None:
        """Mark transition as complete and move up the tree.

        If any child action failed, the transition is also marked as failed.

        Args:
            transition_id: ID of the transition to complete
            success: Whether the transition completed successfully
            error: Optional error message if failed
        """
        node = self.node_map.get(transition_id)
        if node is not None:
            # Check if any child action failed - if so, this transition should also fail
            child_failed = any(child.status == "failed" for child in node.children)
            if child_failed and success:
                success = False
                if not error:
                    failed_child = next((c for c in node.children if c.status == "failed"), None)
                    if failed_child:
                        error = f"Child action '{failed_child.name}' failed"

            node.status = "success" if success else "failed"
            node.error = error
            node.end_timestamp = time.time()
            # Move current pointer to parent
            if node.parent is not None:
                self.current = node.parent

    def start_action(
        self, action_id: str, action_type: str, metadata: dict[str, Any] | None = None
    ) -> ExecutionNode:
        """Start an action under the current workflow/action.

        Args:
            action_id: Unique identifier for the action
            action_type: Type of action (e.g., "CLICK", "GO_TO_STATE")
            metadata: Optional additional data (config, action definition, etc.)

        Returns:
            The created action node
        """
        node = ExecutionNode(
            id=action_id,
            node_type="action",
            name=action_type,
            timestamp=time.time(),
            status="running",
            metadata=metadata or {},
        )
        self.node_map[action_id] = node

        # Add as child of current node
        if self.current:
            self.current.add_child(node)
        self.current = node

        return node

    def end_action(self, action_id: str, success: bool, error: str | None = None) -> None:
        """Mark action as complete and move up the tree.

        If any child action failed, the parent action is also marked as failed,
        regardless of the success parameter. This ensures that composite actions
        (like CLICK with StateImage target that internally does FIND + CLICK)
        properly propagate child failures to the parent.

        Args:
            action_id: ID of the action to complete
            success: Whether the action completed successfully
            error: Optional error message if failed
        """
        node = self.node_map.get(action_id)
        if node is not None:
            # Check if any child action failed - if so, this action should also fail
            # This handles cases like CLICK StateImage where FIND is a child action
            child_failed = any(child.status == "failed" for child in node.children)
            if child_failed and success:
                # Override success - a child failed so this action should fail too
                success = False
                if not error:
                    # Find the first failed child for error context
                    failed_child = next((c for c in node.children if c.status == "failed"), None)
                    if failed_child:
                        error = f"Child action '{failed_child.name}' failed"

            node.status = "success" if success else "failed"
            node.error = error
            node.end_timestamp = time.time()
            # Move current pointer to parent
            if node.parent is not None:
                self.current = node.parent

    def get_current_hierarchy(self) -> dict[str, Any]:
        """Get hierarchy metadata for the current position in the tree.

        Returns:
            Dictionary with parent_id, nesting_level, and workflow_name
        """
        if not self.current:
            return {"parent_id": None, "nesting_level": 0, "workflow_name": None}

        # Calculate nesting level (depth from root)
        nesting_level = self.current.get_depth()

        # Find workflow name (traverse up to find nearest workflow)
        workflow_name = None
        node = self.current
        while node:
            if node.node_type == "workflow":
                workflow_name = node.name
                break
            node = node.parent  # type: ignore[assignment]

        return {
            "parent_id": self.current.id,
            "nesting_level": nesting_level + 1,  # +1 for the new child
            "workflow_name": workflow_name,
        }

    def get_node(self, node_id: str) -> ExecutionNode | None:
        """Get a node by ID.

        Args:
            node_id: The node ID to look up

        Returns:
            The node if found, None otherwise
        """
        return self.node_map.get(node_id)

    def to_dict(self) -> dict[str, Any]:
        """Serialize entire tree to dictionary.

        Returns:
            Dictionary representation of the tree
        """
        return {
            "root": self.root.to_dict() if self.root else None,
            "current_id": self.current.id if self.current else None,
            "node_count": len(self.node_map),
        }
