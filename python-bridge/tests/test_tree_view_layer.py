"""Comprehensive unit tests for TreeViewLayer.

This module tests the TreeViewLayer service:
- execution_tree_view() builds correct hierarchy
- action_only_tree_view() flattens structure
- state_transition_tree_view() groups by state changes
- timeline_tree_view() adds relative timestamps
- state_grouped_tree_view() groups by state sets
- _format_action_label() for different action types
- TreeNode.to_dict() serialization
"""

import time

import pytest

from models.action_execution_record import ActionExecutionRecord
from services.tree_view_layer import TreeNode, TreeViewLayer


class TestTreeNodeSerialization:
    """Test TreeNode.to_dict() serialization."""

    def test_to_dict_basic(self):
        """Test basic TreeNode serialization."""
        node = TreeNode(
            id="node-001",
            type="action",
            label="Click Button",
            data={"status": "success"},
            metadata={"workflow_id": "workflow-001"},
        )

        node_dict = node.to_dict()

        assert node_dict["id"] == "node-001"
        assert node_dict["type"] == "action"
        assert node_dict["label"] == "Click Button"
        assert node_dict["data"]["status"] == "success"
        assert node_dict["metadata"]["workflow_id"] == "workflow-001"
        assert node_dict["children"] == []

    def test_to_dict_with_children(self):
        """Test TreeNode serialization with nested children."""
        child = TreeNode(id="child-001", type="action", label="Child Action")

        parent = TreeNode(
            id="parent-001", type="workflow", label="Parent Workflow", children=[child]
        )

        parent_dict = parent.to_dict()

        assert len(parent_dict["children"]) == 1
        assert parent_dict["children"][0]["id"] == "child-001"
        assert parent_dict["children"][0]["label"] == "Child Action"

    def test_to_dict_deeply_nested(self):
        """Test TreeNode serialization with deep nesting."""
        grandchild = TreeNode(id="gc-001", type="action", label="Grandchild")
        child = TreeNode(id="c-001", type="action", label="Child", children=[grandchild])
        parent = TreeNode(id="p-001", type="workflow", label="Parent", children=[child])

        parent_dict = parent.to_dict()

        assert parent_dict["children"][0]["children"][0]["id"] == "gc-001"


class TestExecutionTreeView:
    """Test execution_tree_view method."""

    def test_execution_tree_view_empty_list(self):
        """Test execution_tree_view with empty record list."""
        result = TreeViewLayer.execution_tree_view([])

        assert result == []

    def test_execution_tree_view_single_action(self, action_record_builder):
        """Test execution_tree_view with single action."""
        record = action_record_builder(
            action_id="action-001", action_type="CLICK", workflow_id="workflow-001"
        )

        result = TreeViewLayer.execution_tree_view([record])

        assert len(result) == 1
        workflow_node = result[0]
        assert workflow_node.type == "workflow"
        assert workflow_node.id == "workflow-001"
        assert len(workflow_node.children) == 1
        assert workflow_node.children[0].id == "action-001"

    def test_execution_tree_view_parent_child_relationship(self, action_record_builder):
        """Test execution_tree_view builds parent-child hierarchy."""
        parent = action_record_builder(
            action_id="parent-001", action_type="COMPLEX", workflow_id="workflow-001"
        )

        child = action_record_builder(
            action_id="child-001",
            action_type="CLICK",
            parent_action_id="parent-001",
            workflow_id="workflow-001",
            nesting_level=1,
        )

        result = TreeViewLayer.execution_tree_view([parent, child])

        workflow_node = result[0]
        parent_node = workflow_node.children[0]
        assert parent_node.id == "parent-001"
        assert len(parent_node.children) == 1
        assert parent_node.children[0].id == "child-001"

    def test_execution_tree_view_multiple_workflows(self, action_record_builder):
        """Test execution_tree_view with multiple workflows."""
        action1 = action_record_builder(action_id="action-001", workflow_id="workflow-A")

        action2 = action_record_builder(action_id="action-002", workflow_id="workflow-B")

        result = TreeViewLayer.execution_tree_view([action1, action2])

        assert len(result) == 2
        workflow_ids = {node.id for node in result}
        assert "workflow-A" in workflow_ids
        assert "workflow-B" in workflow_ids

    def test_execution_tree_view_includes_metadata(self, action_record_builder):
        """Test that execution_tree_view includes action metadata."""
        record = action_record_builder(
            action_id="action-001",
            action_type="CLICK",
            workflow_id="workflow-001",
            nesting_level=2,
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Dashboard"]),
        )

        result = TreeViewLayer.execution_tree_view([record])

        action_node = result[0].children[0]
        assert action_node.metadata["workflow_id"] == "workflow-001"
        assert action_node.metadata["depth"] == 2
        assert action_node.metadata["has_state_change"] is True


class TestActionOnlyTreeView:
    """Test action_only_tree_view method."""

    def test_action_only_tree_view_empty_list(self):
        """Test action_only_tree_view with empty list."""
        result = TreeViewLayer.action_only_tree_view([])

        assert result == []

    def test_action_only_tree_view_flattens_hierarchy(self, action_record_builder):
        """Test that action_only_tree_view creates flat list."""
        parent = action_record_builder(action_id="parent-001", start_time=100.0)

        child = action_record_builder(
            action_id="child-001", parent_action_id="parent-001", start_time=101.0
        )

        result = TreeViewLayer.action_only_tree_view([parent, child])

        # Should be flat list, no nesting
        assert len(result) == 2
        assert result[0].id == "parent-001"
        assert result[1].id == "child-001"
        assert len(result[0].children) == 0
        assert len(result[1].children) == 0

    def test_action_only_tree_view_chronological_order(self, action_record_builder):
        """Test that actions are sorted chronologically."""
        action3 = action_record_builder(action_id="action-003", start_time=300.0)
        action1 = action_record_builder(action_id="action-001", start_time=100.0)
        action2 = action_record_builder(action_id="action-002", start_time=200.0)

        # Pass in scrambled order
        result = TreeViewLayer.action_only_tree_view([action3, action1, action2])

        # Should be sorted by timestamp
        assert result[0].id == "action-001"
        assert result[1].id == "action-002"
        assert result[2].id == "action-003"

    def test_action_only_tree_view_includes_parent_id(self, action_record_builder):
        """Test that metadata includes parent_action_id."""
        child = action_record_builder(action_id="child-001", parent_action_id="parent-001")

        result = TreeViewLayer.action_only_tree_view([child])

        assert result[0].metadata["parent_action_id"] == "parent-001"


class TestStateTransitionTreeView:
    """Test state_transition_tree_view method."""

    def test_state_transition_tree_view_empty_list(self):
        """Test state_transition_tree_view with empty list."""
        result = TreeViewLayer.state_transition_tree_view([])

        assert result == []

    def test_state_transition_tree_view_groups_entered_states(self, action_record_builder):
        """Test grouping of entered states."""
        record = action_record_builder(
            action_id="action-001",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login", "Dashboard"]),
        )

        result = TreeViewLayer.state_transition_tree_view([record])

        # Should have "States Entered" group
        entered_group = next(node for node in result if node.id == "state_entered")
        assert entered_group.label == "States Entered"

        # Should have Dashboard state node
        dashboard_node = entered_group.children[0]
        assert "Dashboard" in dashboard_node.label
        assert len(dashboard_node.children) == 1
        assert dashboard_node.children[0].id == "action-001"

    def test_state_transition_tree_view_groups_exited_states(self, action_record_builder):
        """Test grouping of exited states."""
        record = action_record_builder(
            action_id="action-001",
            active_states_before=frozenset(["Login", "Form"]),
            active_states_after=frozenset(["Login"]),
        )

        result = TreeViewLayer.state_transition_tree_view([record])

        # Should have "States Exited" group
        exited_group = next(node for node in result if node.id == "state_exited")
        assert exited_group.label == "States Exited"

        # Should have Form state node
        form_node = exited_group.children[0]
        assert "Form" in form_node.label
        assert len(form_node.children) == 1

    def test_state_transition_tree_view_no_change_group(self, action_record_builder):
        """Test grouping of actions with no state change."""
        record = action_record_builder(
            action_id="action-001",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login"]),
        )

        result = TreeViewLayer.state_transition_tree_view([record])

        # Should have "No State Change" group
        no_change_group = next(node for node in result if node.id == "no_state_change")
        assert no_change_group.label == "No State Change"
        assert len(no_change_group.children) == 1
        assert no_change_group.children[0].id == "action-001"

    def test_state_transition_tree_view_multiple_actions_same_state(self, action_record_builder):
        """Test multiple actions entering the same state."""
        record1 = action_record_builder(
            action_id="action-001",
            active_states_before=frozenset(),
            active_states_after=frozenset(["Dashboard"]),
        )

        record2 = action_record_builder(
            action_id="action-002",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login", "Dashboard"]),
        )

        result = TreeViewLayer.state_transition_tree_view([record1, record2])

        entered_group = next(node for node in result if node.id == "state_entered")
        dashboard_node = next(node for node in entered_group.children if "Dashboard" in node.label)

        # Both actions should be under Dashboard state
        assert len(dashboard_node.children) == 2


class TestTimelineTreeView:
    """Test timeline_tree_view method."""

    def test_timeline_tree_view_empty_list(self):
        """Test timeline_tree_view with empty list."""
        result = TreeViewLayer.timeline_tree_view([])

        assert result == []

    def test_timeline_tree_view_adds_relative_timestamps(self, action_record_builder):
        """Test that timeline view adds relative timestamps."""
        base_time = time.time()

        record1 = action_record_builder(action_id="action-001", start_time=base_time)

        record2 = action_record_builder(action_id="action-002", start_time=base_time + 1.5)

        result = TreeViewLayer.timeline_tree_view([record1, record2])

        # First action should have relative_time = 0
        assert result[0].data["relative_time"] == 0.0

        # Second action should have relative_time = 1.5
        assert result[1].data["relative_time"] == pytest.approx(1.5, rel=1e-6)

    def test_timeline_tree_view_formats_labels_with_timestamps(self, action_record_builder):
        """Test that labels include timestamp prefix."""
        base_time = time.time()

        record = action_record_builder(
            action_id="action-001", action_type="CLICK", start_time=base_time
        )

        result = TreeViewLayer.timeline_tree_view([record])

        # Label should start with [0.000s]
        assert result[0].label.startswith("[0.000s]")

    def test_timeline_tree_view_maintains_hierarchy(self, action_record_builder):
        """Test that timeline view maintains parent-child relationships."""
        base_time = time.time()

        parent = action_record_builder(action_id="parent-001", start_time=base_time)

        child = action_record_builder(
            action_id="child-001", parent_action_id="parent-001", start_time=base_time + 0.5
        )

        result = TreeViewLayer.timeline_tree_view([parent, child])

        # Parent should have child
        assert len(result) == 1
        assert result[0].id == "parent-001"
        assert len(result[0].children) == 1
        assert result[0].children[0].id == "child-001"


class TestStateGroupedTreeView:
    """Test state_grouped_tree_view method."""

    def test_state_grouped_tree_view_empty_list(self):
        """Test state_grouped_tree_view with empty list."""
        result = TreeViewLayer.state_grouped_tree_view([])

        assert result == []

    def test_state_grouped_tree_view_groups_by_active_states(self, action_record_builder):
        """Test grouping by active state sets."""
        record1 = action_record_builder(
            action_id="action-001", active_states_after=frozenset(["Login", "MainMenu"])
        )

        record2 = action_record_builder(
            action_id="action-002", active_states_after=frozenset(["Login", "MainMenu"])
        )

        record3 = action_record_builder(
            action_id="action-003", active_states_after=frozenset(["Dashboard"])
        )

        result = TreeViewLayer.state_grouped_tree_view([record1, record2, record3])

        # Should have 2 state groups
        assert len(result) == 2

        # First group should have 2 actions
        assert len(result[0].children) == 2

        # Second group should have 1 action
        assert len(result[1].children) == 1

    def test_state_grouped_tree_view_empty_state_set(self, action_record_builder):
        """Test handling of empty active state set."""
        record = action_record_builder(action_id="action-001", active_states_after=frozenset())

        result = TreeViewLayer.state_grouped_tree_view([record])

        # Should have label indicating no active states
        assert "no active states" in result[0].label.lower()

    def test_state_grouped_tree_view_sorts_by_first_action_time(self, action_record_builder):
        """Test that state groups are sorted by first action timestamp."""
        # Group A: starts at 200
        action_a = action_record_builder(
            action_id="action-a", start_time=200.0, active_states_after=frozenset(["StateA"])
        )

        # Group B: starts at 100
        action_b = action_record_builder(
            action_id="action-b", start_time=100.0, active_states_after=frozenset(["StateB"])
        )

        result = TreeViewLayer.state_grouped_tree_view([action_a, action_b])

        # Group with earlier timestamp should come first
        assert result[0].children[0].id == "action-b"
        assert result[1].children[0].id == "action-a"

    def test_state_grouped_tree_view_includes_state_changes(self, action_record_builder):
        """Test that action nodes include state_changes data."""
        record = action_record_builder(
            action_id="action-001",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login", "Dashboard"]),
        )

        result = TreeViewLayer.state_grouped_tree_view([record])

        action_node = result[0].children[0]
        state_changes = action_node.data["state_changes"]

        assert state_changes["Dashboard"] == "entered"


class TestFormatActionLabel:
    """Test _format_action_label method."""

    def test_format_action_label_success(self, action_record_builder):
        """Test label formatting for successful action."""
        record = action_record_builder(action_type="CLICK", success=True, duration_ms=500.0)

        label = TreeViewLayer._format_action_label(record)

        assert "✓" in label
        assert "CLICK" in label
        assert "500ms" in label

    def test_format_action_label_failed(self, action_record_builder):
        """Test label formatting for failed action."""
        record = action_record_builder(action_type="FIND", success=False, end_time=time.time())

        label = TreeViewLayer._format_action_label(record)

        assert "✗" in label
        assert "FIND" in label

    def test_format_action_label_running(self, action_record_builder):
        """Test label formatting for running action."""
        record = action_record_builder(action_type="WAIT", end_time=None)

        label = TreeViewLayer._format_action_label(record)

        assert "⟳" in label
        assert "WAIT" in label

    def test_format_action_label_with_state_changes(self, action_record_builder):
        """Test label formatting includes state changes."""
        record = action_record_builder(
            action_type="GO_TO_STATE",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Dashboard", "Settings"]),
        )

        label = TreeViewLayer._format_action_label(record)

        assert "+" in label  # States activated
        assert "-" in label  # States deactivated
        assert "Dashboard" in label or "Settings" in label
        assert "Login" in label

    def test_format_action_label_no_duration(self, action_record_builder):
        """Test label formatting when duration is not available."""
        record = action_record_builder(action_type="CLICK", duration_ms=None)

        label = TreeViewLayer._format_action_label(record)

        assert "ms" not in label

    def test_format_action_label_no_state_change(self, action_record_builder):
        """Test label formatting when no state change occurred."""
        record = action_record_builder(
            action_type="CLICK",
            active_states_before=frozenset(["Login"]),
            active_states_after=frozenset(["Login"]),
        )

        label = TreeViewLayer._format_action_label(record)

        # Should not have state change indicators
        assert "+" not in label
        assert "-" not in label


class TestFormatTimelineLabel:
    """Test _format_timeline_label method."""

    def test_format_timeline_label_zero_time(self, action_record_builder):
        """Test timeline label formatting at start time."""
        record = action_record_builder(action_type="CLICK")

        label = TreeViewLayer._format_timeline_label(record, 0.0)

        assert label.startswith("[0.000s]")
        assert "CLICK" in label

    def test_format_timeline_label_non_zero_time(self, action_record_builder):
        """Test timeline label formatting with elapsed time."""
        record = action_record_builder(action_type="TYPE")

        label = TreeViewLayer._format_timeline_label(record, 1.234)

        assert label.startswith("[1.234s]")
        assert "TYPE" in label

    def test_format_timeline_label_includes_action_label(self, action_record_builder):
        """Test that timeline label includes full action label."""
        record = action_record_builder(action_type="FIND", success=True, duration_ms=500.0)

        label = TreeViewLayer._format_timeline_label(record, 2.5)

        assert "[2.500s]" in label
        assert "✓" in label
        assert "FIND" in label
        assert "500ms" in label


class TestFormatStateChange:
    """Test _format_state_change method."""

    def test_format_state_change_with_changes(self, action_record_builder):
        """Test state change formatting with actual changes."""
        record = action_record_builder(
            active_states_before=frozenset(["Login", "Form"]),
            active_states_after=frozenset(["Dashboard", "Settings"]),
        )

        state_change_str = TreeViewLayer._format_state_change(record)

        assert "Entered:" in state_change_str
        assert "Dashboard" in state_change_str
        assert "Settings" in state_change_str
        assert "Exited:" in state_change_str
        assert "Login" in state_change_str or "Form" in state_change_str

    def test_format_state_change_no_changes(self, action_record_builder):
        """Test state change formatting with no changes."""
        record = action_record_builder(
            active_states_before=frozenset(["Login"]), active_states_after=frozenset(["Login"])
        )

        state_change_str = TreeViewLayer._format_state_change(record)

        assert state_change_str == ""

    def test_format_state_change_only_entered(self, action_record_builder):
        """Test state change formatting with only entered states."""
        record = action_record_builder(
            active_states_before=frozenset(), active_states_after=frozenset(["Login"])
        )

        state_change_str = TreeViewLayer._format_state_change(record)

        assert "Entered:" in state_change_str
        assert "Login" in state_change_str
        assert "Exited:" not in state_change_str

    def test_format_state_change_only_exited(self, action_record_builder):
        """Test state change formatting with only exited states."""
        record = action_record_builder(
            active_states_before=frozenset(["Login"]), active_states_after=frozenset()
        )

        state_change_str = TreeViewLayer._format_state_change(record)

        assert "Exited:" in state_change_str
        assert "Login" in state_change_str
        assert "Entered:" not in state_change_str


class TestComplexScenarios:
    """Test complex scenarios with multiple records and transformations."""

    def test_complex_workflow_hierarchy(self, action_record_builder):
        """Test complex workflow with multiple levels of nesting."""
        workflow_action = action_record_builder(
            action_id="workflow-001", action_type="WORKFLOW", workflow_id="main-workflow"
        )

        parent_action = action_record_builder(
            action_id="parent-001",
            action_type="COMPLEX",
            parent_action_id="workflow-001",
            workflow_id="main-workflow",
            nesting_level=1,
        )

        child1 = action_record_builder(
            action_id="child-001",
            action_type="CLICK",
            parent_action_id="parent-001",
            workflow_id="main-workflow",
            nesting_level=2,
        )

        child2 = action_record_builder(
            action_id="child-002",
            action_type="TYPE",
            parent_action_id="parent-001",
            workflow_id="main-workflow",
            nesting_level=2,
        )

        records = [workflow_action, parent_action, child1, child2]
        result = TreeViewLayer.execution_tree_view(records)

        # Verify structure
        workflow_node = result[0]
        assert workflow_node.id == "main-workflow"

        workflow_action_node = workflow_node.children[0]
        assert workflow_action_node.id == "workflow-001"

        parent_node = workflow_action_node.children[0]
        assert parent_node.id == "parent-001"
        assert len(parent_node.children) == 2

    def test_multiple_view_transformations_same_data(self, action_record_builder):
        """Test that different views can process the same data."""
        records = [
            action_record_builder(
                action_id="action-001", start_time=100.0, active_states_after=frozenset(["StateA"])
            ),
            action_record_builder(
                action_id="action-002", start_time=200.0, active_states_after=frozenset(["StateB"])
            ),
        ]

        # All views should handle the same data
        exec_view = TreeViewLayer.execution_tree_view(records)
        action_view = TreeViewLayer.action_only_tree_view(records)
        state_view = TreeViewLayer.state_transition_tree_view(records)
        timeline_view = TreeViewLayer.timeline_tree_view(records)
        grouped_view = TreeViewLayer.state_grouped_tree_view(records)

        # All should produce valid results
        assert len(exec_view) > 0
        assert len(action_view) == 2
        assert len(state_view) > 0
        assert len(timeline_view) == 2
        assert len(grouped_view) == 2
