"""Test navigation API loading and event tree structure."""

import json


def test_load_bdo_config():
    """Test that BDO config loads correctly."""
    with open("../bdo_config (42).json", "r") as f:
        config = json.load(f)

    print("\n=== BDO Config Structure ===")
    print(f"Workflows: {len(config.get('workflows', {}))}")
    print(f"Images: {len(config.get('images', {}))}")
    print(f"Initial state: {config.get('initial_state')}")

    # Print first workflow structure
    workflows = config.get("workflows", {})
    if workflows:
        first_workflow_id = list(workflows.keys())[0]
        first_workflow = workflows[first_workflow_id]
        print(f"\nFirst workflow: {first_workflow_id}")
        print(f"  Name: {first_workflow.get('name')}")
        print(f"  Actions: {len(first_workflow.get('actions', []))}")
        if first_workflow.get('actions'):
            print(f"  First action: {first_workflow['actions'][0].get('action_type')}")


def test_event_tree_structure():
    """Test that event tree structure works correctly with workflow events."""
    # Simulate the event structure we expect from the backend
    events = [
        {
            "id": "grind-corn",  # workflow_id used as event id
            "timestamp": 1000,
            "workflow_id": "grind-corn",
            "workflow_name": "grind corn",
            "is_workflow": True,
            "hierarchy": {
                "parent_id": None,  # Top-level workflow has no parent
                "nesting_level": 1,
                "workflow_name": None,
                "is_expandable": True
            }
        },
        {
            "id": "action-1",  # action_id used as event id
            "timestamp": 1100,
            "action_id": "action-1",
            "action_type": "CLICK",
            "success": True,
            "attempts": 1,
            "config": {},
            "typed_text": None,
            "error": None,
            "reason": None,
            "is_workflow": False,  # Explicitly mark as non-workflow
            "hierarchy": {
                "parent_id": "grind-corn",  # References workflow_id
                "nesting_level": 2,
                "workflow_name": "grind corn",
                "is_expandable": False
            }
        }
    ]

    print("\n=== Event Tree Structure Test ===")
    print(f"Total events: {len(events)}")

    # Group by parent_id to simulate buildEventTree logic
    by_parent = {}
    for event in events:
        parent_id = event["hierarchy"]["parent_id"]
        if parent_id not in by_parent:
            by_parent[parent_id] = []
        by_parent[parent_id].append(event)

    print(f"\nGrouped by parent_id:")
    for parent_id, children in by_parent.items():
        print(f"  {parent_id}: {len(children)} events")
        for child in children:
            child_id = child.get("workflow_id") or child.get("action_id")
            is_workflow = child.get("is_workflow", False)
            print(f"    - {child_id} (is_workflow={is_workflow}, nesting_level={child['hierarchy']['nesting_level']})")

    # Check root nodes
    root_events = by_parent.get(None, [])
    print(f"\nRoot events: {len(root_events)}")

    if root_events:
        root = root_events[0]
        root_id = root.get("workflow_id") or root.get("action_id")
        print(f"  Root ID: {root_id}")
        print(f"  Root nesting_level: {root['hierarchy']['nesting_level']}")
        print(f"  Root is_workflow: {root.get('is_workflow', False)}")

        # Check children of root
        children = by_parent.get(root_id, [])
        print(f"  Children: {len(children)}")
        for child in children:
            child_id = child.get("workflow_id") or child.get("action_id")
            is_workflow = child.get("is_workflow", False)
            print(f"    - {child_id} (is_workflow={is_workflow}, type={child.get('action_type', 'workflow')})")

    # Test the filtering logic
    print("\n=== Testing Filtering Logic ===")
    if len(root_events) == 1:
        root = root_events[0]
        root_id = root.get("workflow_id") or root.get("action_id")
        children = by_parent.get(root_id, [])

        should_skip = (
            len(root_events) == 1 and
            root.get("hierarchy", {}).get("nesting_level") == 1 and
            root.get("is_workflow") == True and
            len(children) > 0
        )
        print(f"Should skip top-level workflow: {should_skip}")
        print(f"  Conditions:")
        print(f"    - Only one root: {len(root_events) == 1}")
        print(f"    - Nesting level 1: {root.get('hierarchy', {}).get('nesting_level') == 1}")
        print(f"    - Is workflow: {root.get('is_workflow') == True}")
        print(f"    - Has children: {len(children) > 0}")

        if should_skip:
            print(f"  -> Would display {len(children)} children directly")
        else:
            print(f"  -> Would display full tree with {len(root_events)} root nodes")


def test_empty_workflow():
    """Test workflow with no children - should display the workflow itself."""
    # Simulate a workflow that has just started with no actions yet
    events = [
        {
            "id": "grind-corn",
            "timestamp": 1000,
            "workflow_id": "grind-corn",
            "workflow_name": "grind corn",
            "is_workflow": True,
            "hierarchy": {
                "parent_id": None,
                "nesting_level": 1,
                "workflow_name": None,
                "is_expandable": True
            }
        }
    ]

    print("\n=== Empty Workflow Test ===")
    print(f"Total events: {len(events)}")

    # Group by parent_id
    by_parent = {}
    for event in events:
        parent_id = event["hierarchy"]["parent_id"]
        if parent_id not in by_parent:
            by_parent[parent_id] = []
        by_parent[parent_id].append(event)

    root_events = by_parent.get(None, [])
    print(f"Root events: {len(root_events)}")

    if len(root_events) == 1:
        root = root_events[0]
        root_id = root.get("workflow_id") or root.get("action_id")
        children = by_parent.get(root_id, [])

        should_skip = (
            len(root_events) == 1 and
            root.get("hierarchy", {}).get("nesting_level") == 1 and
            root.get("is_workflow") == True and
            len(children) > 0  # This is the key check
        )

        print(f"\nShould skip top-level workflow: {should_skip}")
        print(f"  Conditions:")
        print(f"    - Only one root: {len(root_events) == 1}")
        print(f"    - Nesting level 1: {root.get('hierarchy', {}).get('nesting_level') == 1}")
        print(f"    - Is workflow: {root.get('is_workflow') == True}")
        print(f"    - Has children: {len(children) > 0} (children count: {len(children)})")

        if should_skip:
            print(f"  -> Would display {len(children)} children directly")
        else:
            print(f"  -> Would display full tree with workflow node")
            print(f"     (This is correct - workflow has no children yet)")


if __name__ == "__main__":
    test_load_bdo_config()
    test_event_tree_structure()
    test_empty_workflow()
