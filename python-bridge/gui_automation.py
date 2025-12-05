"""
GUI Automation Module

Single Responsibility: Handle GUI interaction and action execution
- Execute actions via qontinui ActionExecutor
- Execute workflows and nested workflows
- Track execution hierarchy with ExecutionTree
- Collect execution data via UnifiedDataCollector
- Handle special action types (RUN_WORKFLOW, GO_TO_STATE, FIND)
"""

import logging
import time
from collections.abc import Callable
from typing import Any

logger = logging.getLogger(__name__)

try:
    from qontinui import Location, navigation_api, registry
    from qontinui.config.schema import Action

    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    logger.debug("Qontinui library not available")


class GUIAutomation:
    """
    Manages GUI interaction and action execution.

    Responsibilities:
    - Execute individual actions via ActionExecutor
    - Execute workflows sequentially
    - Track execution hierarchy
    - Collect execution data
    - Handle special action types
    """

    def __init__(
        self,
        emit_log_fn: Callable[[str, str], None],
        emit_tree_event_fn: Callable[[str, Any, dict | None], None],
        execution_tree: Any,
        unified_data_collector: Any,
        action_executor: Any,
        state_executor: Any,
        workflows: dict,
        images: dict,
        get_image_name_fn: Callable[[str], str | None],
        get_action_definition_fn: Callable[[str], Any],
    ):
        """
        Initialize GUI automation module.

        Args:
            emit_log_fn: Function to emit log messages
            emit_tree_event_fn: Function to emit tree events
            execution_tree: ExecutionTree instance for tracking hierarchy
            unified_data_collector: UnifiedDataCollector for data collection
            action_executor: ActionExecutor from qontinui library
            state_executor: StateExecutor from qontinui library
            workflows: Dictionary of workflow definitions
            images: Dictionary of loaded images
            get_image_name_fn: Function to get image name by ID
            get_action_definition_fn: Function to get action definition
        """
        self.emit_log = emit_log_fn
        self.emit_tree_event = emit_tree_event_fn
        self.execution_tree = execution_tree
        self.unified_data_collector = unified_data_collector
        self.action_executor = action_executor
        self.state_executor = state_executor
        self.workflows = workflows
        self.images = images
        self.get_image_name = get_image_name_fn
        self.get_action_definition = get_action_definition_fn
        self.is_running = False
        self._last_find_location = None

    def execute_action(self, action_data: dict[str, Any]) -> bool:
        """
        Execute a single action using ExecutionTree and UnifiedDataCollector.

        Args:
            action_data: Dictionary containing action type, id, config, etc.

        Returns:
            True if action succeeded, False otherwise
        """
        action_type = action_data.get("type")
        action_id = action_data.get("id", f"action-{action_type}-{id(action_data)}")
        config = action_data.get("config", {})

        # Get action definition from registry
        action_def = self.get_action_definition(action_type)

        # Extract hierarchy context from execution tree for data collection
        hierarchy_context = self._get_hierarchy_context()

        # Start data collection in UnifiedDataCollector
        if self.unified_data_collector:
            self.unified_data_collector.start_action(
                action_id=action_id,
                parent_action_id=hierarchy_context["parent_action_id"],
                workflow_id=hierarchy_context["workflow_id"],
                nesting_level=hierarchy_context["nesting_level"],
            )

        # Start action in execution tree with metadata
        node = self.execution_tree.start_action(
            action_id,
            action_type,
            metadata={
                "config": config,
                "is_expandable": action_def.is_expandable,
                "action_definition": action_def.to_dict(),
            },
        )
        self.emit_tree_event("action_started", node, None)

        # Capture start time for record creation
        start_time = time.time()
        success = False
        error_message = None

        try:
            self.emit_log("info", f"Executing action: {action_type}")

            # Handle missing action executor
            if not QONTINUI_AVAILABLE or not self.action_executor:
                self.emit_log(
                    "warning", f"Simulating action: {action_type} (ActionExecutor not available)"
                )
                time.sleep(0.5)  # Simulate action delay
                success = True
                self.execution_tree.end_action(action_id, success=True)
                self.emit_tree_event("action_completed", node, None)
                return True

            # Convert action_data to Action object for library
            action = Action(
                id=action_id, type=action_type, config=config, base=action_data.get("base")
            )

            # Handle RUN_WORKFLOW specially - execute workflow via runner's execute_workflow
            if action_type == "RUN_WORKFLOW":
                workflow_id = config.get("workflowId")
                if workflow_id:
                    success = self.execute_workflow(workflow_id)
                else:
                    self.emit_log("error", "RUN_WORKFLOW action missing workflowId")
                    success = False
            else:
                # Normal action delegation to library
                success = self.action_executor.execute_action(action)

            # Sync last_find_location from library if available
            if (
                hasattr(self.action_executor, "last_find_location")
                and self.action_executor.last_find_location
            ):
                loc = self.action_executor.last_find_location
                self._last_find_location = Location(loc[0], loc[1])
                self.emit_log(
                    "debug", f"Synced last_find_location from library: {self._last_find_location}"
                )

            # Handle GO_TO_STATE actions - record transition data
            if action_type == "GO_TO_STATE" and self.unified_data_collector:
                target_state = config.get("targetState", "unknown")
                from_state = self.state_executor.current_state if self.state_executor else None

                transition_data = {
                    "from_state": from_state,
                    "to_state": target_state,
                    "transition_type": "navigation",
                    "success": success,
                }
                self.unified_data_collector.record_transition(transition_data)

            # Handle RUN_WORKFLOW actions - add workflow name to metadata
            if action_type == "RUN_WORKFLOW" and self.unified_data_collector:
                workflow_id = config.get("workflowId")
                if workflow_id:
                    workflow_name = workflow_id
                    if workflow_id in self.workflows:
                        workflow_data = self.workflows[workflow_id]
                        if isinstance(workflow_data, dict):
                            workflow_name = workflow_data.get("name", workflow_id)

                    self.unified_data_collector.record_workflow_execution(
                        workflow_name=workflow_name,
                        workflow_status="success" if success else "failed",
                    )

        except Exception as e:
            success = False
            error_message = str(e)
            self.emit_log("error", f"Action failed with exception: {e}")

        finally:
            # Capture end time
            end_time = time.time()

            # Enrich FIND action config with human-readable image names
            if action_type == "FIND":
                config = self._enrich_find_action_config(config)
                node.metadata["config"] = config

            # Create ActionExecutionRecord with UnifiedDataCollector
            if self.unified_data_collector:
                record = self.unified_data_collector.create_record(
                    action_type=action_type,
                    config=config,
                    start_time=start_time,
                    end_time=end_time,
                    success=success,
                    error_message=error_message,
                    retry_count=0,
                    metadata={"action_definition": action_def.to_dict()},
                )

                # Enrich ExecutionNode metadata with record data
                node.metadata.update({"execution_record": record.to_tree_event_data()})

            # End action in execution tree
            self.execution_tree.end_action(action_id, success=success, error=error_message)

            # Emit tree event with enriched metadata
            self.emit_tree_event("action_completed" if success else "action_failed", node, None)

            return success

    def execute_workflow(self, workflow_id: str, transition_context: dict | None = None) -> bool:
        """
        Execute a workflow and return success status.

        Args:
            workflow_id: ID of workflow to execute
            transition_context: Optional transition metadata

        Returns:
            True if workflow succeeded, False otherwise
        """
        try:
            # If transition context provided, create a transition node
            if transition_context:
                transition_id = transition_context.get("transition_id", f"transition-{time.time()}")
                transition_name = transition_context.get("transition_name", "Transition")

                # Start transition node
                trans_node = self.execution_tree.start_transition(
                    transition_id,
                    transition_name,
                    metadata={
                        "transition_type": transition_context.get("transition_type"),
                        "target_states": transition_context.get("target_states", []),
                        "source_state": transition_context.get("source_state"),
                        "is_expandable": True,
                    },
                )
                self.emit_tree_event("transition_started", trans_node, None)

                try:
                    # Execute workflow under this transition
                    success = self._execute_workflow_internal(workflow_id)

                    # End transition node
                    self.execution_tree.end_transition(transition_id, success=success)
                    self.emit_tree_event(
                        "transition_completed" if success else "transition_failed", trans_node, None
                    )

                    return success
                except Exception as e:
                    # End transition with failure
                    self.execution_tree.end_transition(transition_id, success=False, error=str(e))
                    self.emit_tree_event("transition_failed", trans_node, {"error": str(e)})
                    raise
            else:
                # No transition context - execute workflow normally
                return self._execute_workflow_internal(workflow_id)
        except Exception as e:
            self.emit_log("error", f"Workflow execution failed: {e}")
            return False

    def _execute_workflow_internal(self, workflow_id: str) -> bool:
        """
        Internal workflow execution using ExecutionTree for tracking.

        Args:
            workflow_id: ID of workflow to execute

        Returns:
            True if workflow succeeded, False otherwise
        """
        # Get workflow data
        workflow_data = None
        workflow_name = workflow_id
        is_inline = False

        # Check if called from within an action (inline workflow)
        current_node = self.execution_tree.current
        if current_node and current_node.node_type == "action":
            is_inline = True
            self.emit_log("debug", f"Workflow {workflow_id} called from action, marking as inline")

        # Try local workflows first
        if workflow_id in self.workflows:
            workflow_data = self.workflows[workflow_id]
            if isinstance(workflow_data, dict):
                actions = workflow_data.get("actions", [])
                workflow_name = workflow_data.get("name", workflow_id)
            else:
                actions = workflow_data
        # Fallback to registry for inline workflows
        elif QONTINUI_AVAILABLE:
            actions = registry.get_workflow(workflow_id)
            if actions is None:
                self.emit_log("error", f"Workflow {workflow_id} not found")
                return False
            workflow_name = (
                registry.get_workflow_name(workflow_id)
                if hasattr(registry, "get_workflow_name")
                else workflow_id
            )
            is_inline = True
            # Cache it locally
            self.workflows[workflow_id] = {"actions": actions, "name": workflow_name}
            self.emit_log("debug", f"Loaded inline workflow {workflow_id} from registry")
        else:
            self.emit_log("error", f"Workflow {workflow_id} not found")
            return False

        # Start workflow in execution tree
        node = self.execution_tree.start_workflow(workflow_id, workflow_name, is_inline=is_inline)
        self.emit_tree_event("workflow_started", node, None)

        success = True

        try:
            for idx, action in enumerate(actions):
                if not self.is_running:
                    break

                # Execute action and track success, but always continue
                action_success = self.execute_action(action)

                if not action_success:
                    success = False
                    self.emit_log(
                        "debug", "Action failed, continuing workflow (resilient automation)"
                    )

                # Small delay between actions
                time.sleep(0.5)

            # End workflow with success status
            self.execution_tree.end_workflow(workflow_id, success=success)
            self.emit_tree_event("workflow_completed" if success else "workflow_failed", node, None)

        except Exception as e:
            # End workflow with failure status
            self.execution_tree.end_workflow(workflow_id, success=False, error=str(e))
            self.emit_tree_event("workflow_failed", node, {"error": str(e)})
            success = False

        return success

    def _get_hierarchy_context(self) -> dict[str, Any]:
        """
        Extract hierarchy context for data collection.

        Returns:
            Dictionary with parent_action_id, workflow_id, nesting_level
        """
        if not self.execution_tree or not self.execution_tree.current:
            return {"parent_action_id": None, "workflow_id": None, "nesting_level": 0}

        current = self.execution_tree.current
        parent_action_id = current.id

        # Find workflow by traversing up the tree
        workflow_id = None
        node = current
        while node:
            if node.node_type == "workflow":
                workflow_id = node.id
                break
            node = node.parent

        nesting_level = current.get_depth() + 1

        return {
            "parent_action_id": parent_action_id,
            "workflow_id": workflow_id,
            "nesting_level": nesting_level,
        }

    def _enrich_find_action_config(self, config: dict[str, Any]) -> dict[str, Any]:
        """
        Enrich FIND action config with human-readable image names.

        Args:
            config: Original action config

        Returns:
            Enriched config with image names
        """
        config_copy = None

        # Check for imageId at root level
        if "imageId" in config:
            image_id = config.get("imageId")
            image_name = self.get_image_name(image_id)
            if image_name:
                config_copy = config.copy() if config_copy is None else config_copy
                config_copy["imageName"] = image_name

        # Check for imageId nested in target object
        target = config.get("target")
        if isinstance(target, dict):
            if "imageId" in target:
                image_id = target.get("imageId")
                image_name = self.get_image_name(image_id)
                if image_name:
                    config_copy = config.copy() if config_copy is None else config_copy
                    target_copy = (
                        config_copy.get("target", {}).copy()
                        if isinstance(config_copy.get("target"), dict)
                        else {}
                    )
                    target_copy["imageName"] = image_name
                    config_copy["target"] = target_copy
                    config_copy["imageName"] = image_name

            # Handle multiple images in target
            if "imageIds" in target:
                image_ids = target.get("imageIds", [])
                image_names = [self.get_image_name(img_id) for img_id in image_ids]
                image_names = [name for name in image_names if name]
                if image_names:
                    config_copy = config.copy() if config_copy is None else config_copy
                    target_copy = (
                        config_copy.get("target", {}).copy()
                        if isinstance(config_copy.get("target"), dict)
                        else {}
                    )
                    target_copy["imageNames"] = image_names
                    config_copy["target"] = target_copy
                    config_copy["imageNames"] = image_names
                    config_copy["imageName"] = ", ".join(image_names)

        # Handle multiple images at root level
        if "imageIds" in config:
            image_ids = config.get("imageIds", [])
            image_names = [self.get_image_name(img_id) for img_id in image_ids]
            image_names = [name for name in image_names if name]
            if image_names:
                config_copy = config.copy() if config_copy is None else config_copy
                config_copy["imageNames"] = image_names
                config_copy["imageName"] = ", ".join(image_names)

        # Use enriched config if we made changes
        if config_copy:
            return config_copy
        return config

    def reset_navigation_state(self) -> bool:
        """
        Reset navigation state to initial conditions.

        Returns:
            True if reset successful, False otherwise
        """
        if not QONTINUI_AVAILABLE:
            return False

        try:
            reset_success = navigation_api.reset_to_initial_state()
            if reset_success:
                self.emit_log("info", "Navigation state reset to initial conditions")
            else:
                self.emit_log("warning", "Failed to reset navigation state")
            return reset_success
        except Exception as e:
            self.emit_log("warning", f"Error resetting navigation state: {e}")
            return False

    def get_last_find_location(self) -> Any | None:
        """
        Get location of most recent FIND result.

        Returns:
            Location object or None
        """
        return self._last_find_location

    def set_running(self, is_running: bool):
        """
        Set running state.

        Args:
            is_running: True to set running, False to stop
        """
        self.is_running = is_running
