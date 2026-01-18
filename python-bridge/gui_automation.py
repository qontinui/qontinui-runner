"""
GUI Automation Module

Single Responsibility: Handle GUI interaction and action execution
- Execute actions via qontinui ActionExecutor
- Execute workflows and nested workflows
- Track execution hierarchy with ExecutionTree
- Collect execution data via UnifiedDataCollector
- Handle special action types (RUN_WORKFLOW, GO_TO_STATE, FIND, AI_PROMPT, RUN_PROMPT_SEQUENCE)
- Handle accessibility-based targeting via AccessibilityCaptureService
"""

import asyncio
import logging
import time
from collections.abc import Callable
from typing import Any

from ai_prompt_executor import AIPromptExecutor
from gemini_executor import GeminiExecutor
from services.accessibility_capture_service import AccessibilityCaptureService

logger = logging.getLogger(__name__)

try:
    from qontinui import Location, navigation_api, registry
    from qontinui_schemas.config.models import Action

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

        # Initialize AI prompt executors for AI_PROMPT and RUN_PROMPT_SEQUENCE actions
        self.ai_prompt_executor = AIPromptExecutor(emit_log_fn=emit_log_fn)
        self.gemini_executor = GeminiExecutor(emit_log_fn=emit_log_fn)

        # Initialize accessibility capture service for ref-based actions
        self._accessibility_service: AccessibilityCaptureService | None = None

    def _get_accessibility_service(self) -> AccessibilityCaptureService:
        """Get or create the accessibility capture service (lazy initialization)."""
        if self._accessibility_service is None:
            self._accessibility_service = AccessibilityCaptureService()
        return self._accessibility_service

    async def _handle_accessibility_action(
        self, action_type: str, target: dict[str, Any]
    ) -> bool:
        """Handle actions with accessibility targets.

        This method:
        1. Captures accessibility tree if needed
        2. Finds the element by ref or selector
        3. Performs the action (click, type, focus)

        Args:
            action_type: The action type (CLICK, TYPE, FOCUS)
            target: Accessibility target config with ref or selector

        Returns:
            True if action succeeded, False otherwise
        """
        service = self._get_accessibility_service()

        # Extract target config
        ref = target.get("ref")
        role = target.get("role")
        name = target.get("name")
        name_contains = target.get("nameContains")
        is_interactive = target.get("isInteractive")
        capture_first = target.get("captureFirst", True)
        cdp_host = target.get("cdpHost", "localhost")
        cdp_port = target.get("cdpPort", 9222)

        try:
            # Capture tree if needed
            if capture_first or not service.has_snapshot:
                self.emit_log("info", f"Capturing accessibility tree from {cdp_host}:{cdp_port}")
                result = await service.capture_accessibility_tree(
                    cdp_host=cdp_host,
                    cdp_port=cdp_port,
                    interactive_only=True,
                )
                if not result.get("success"):
                    self.emit_log("error", f"Failed to capture accessibility tree: {result.get('error')}")
                    return False

            # If we have a direct ref, use it
            if ref:
                target_ref = ref
            else:
                # Find element by selector
                self.emit_log("info", f"Finding element by selector: role={role}, name={name}")
                find_result = await service.find_elements(
                    role=role,
                    name=name,
                    name_contains=name_contains,
                    is_interactive=is_interactive,
                )
                if not find_result.get("success") or find_result.get("count", 0) == 0:
                    self.emit_log("error", f"No elements found matching selector")
                    return False

                # Use the first match
                elements = find_result.get("elements", [])
                if not elements:
                    return False
                target_ref = elements[0].get("ref")
                self.emit_log("info", f"Found element: {target_ref}")

            # Perform the action
            if action_type == "CLICK":
                result = await service.click_ref(target_ref)
            elif action_type == "TYPE":
                # For TYPE actions, we need the text from the action config
                # This will be passed separately - for now just focus
                result = await service.focus_ref(target_ref)
            elif action_type == "FOCUS":
                result = await service.focus_ref(target_ref)
            else:
                self.emit_log("warning", f"Unknown accessibility action type: {action_type}")
                return False

            success = result.get("success", False)
            if success:
                self.emit_log("info", f"Accessibility action {action_type} on {target_ref} succeeded")
            else:
                self.emit_log("error", f"Accessibility action {action_type} on {target_ref} failed: {result.get('error')}")

            return success

        except Exception as e:
            self.emit_log("error", f"Accessibility action failed: {e}")
            return False

    async def execute_action(self, action_data: dict[str, Any]) -> bool:
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
        action_def = self.get_action_definition(action_type)  # type: ignore[arg-type]

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
                    success = await self.execute_workflow(workflow_id)
                else:
                    self.emit_log("error", "RUN_WORKFLOW action missing workflowId")
                    success = False

            # Handle AI_PROMPT - execute AI prompt via Claude CLI or Gemini
            elif action_type == "AI_PROMPT":
                prompt = config.get("prompt")
                fresh_context = config.get("freshContext", True)
                timeout = config.get("timeout", 600000)
                working_directory = config.get("workingDirectory")
                output_variable = config.get("outputVariable")
                # Provider/model override (if not specified, use Claude CLI as default)
                provider = config.get("provider", "claude_cli")
                model = config.get("model")

                if not prompt:
                    self.emit_log("error", "AI_PROMPT action missing prompt")
                    success = False
                else:
                    # Route to appropriate executor based on provider
                    if provider and provider.startswith("gemini"):
                        # Use Gemini executor
                        gemini_model = model or "gemini-3-flash-preview"
                        self.emit_log(
                            "info", f"Using Gemini provider: {provider}, model: {gemini_model}"
                        )
                        result = self.gemini_executor.execute_prompt(
                            prompt=prompt,
                            provider=provider,
                            model=gemini_model,
                            working_directory=working_directory,
                            timeout=timeout,
                            max_output_tokens=config.get("maxOutputTokens", 8192),
                            temperature=config.get("temperature", 0.7),
                        )
                    else:
                        # Use Claude executor (default)
                        result = self.ai_prompt_executor.execute_prompt(
                            prompt=prompt,
                            fresh_context=fresh_context,
                            working_directory=working_directory,
                            timeout=timeout,
                            output_variable=output_variable,
                        )
                    success = result["success"]

                    # Store output in metadata for tree display
                    if result.get("output"):
                        node.metadata["ai_output"] = result["output"][:500]  # Truncate for display
                    if result.get("error"):
                        error_message = result["error"]

            # Handle RUN_PROMPT_SEQUENCE - execute sequence of AI prompts
            elif action_type == "RUN_PROMPT_SEQUENCE":
                sequence_id = config.get("sequenceId")
                inline_sequence = config.get("inlineSequence")
                parameter_overrides = config.get("parameterOverrides")
                working_directory = config.get("workingDirectory")
                results_directory = config.get("resultsDirectory")

                if not sequence_id and not inline_sequence:
                    self.emit_log(
                        "error", "RUN_PROMPT_SEQUENCE missing sequenceId or inlineSequence"
                    )
                    success = False
                else:
                    # Use inline sequence if provided, otherwise would look up by ID
                    sequence_config = inline_sequence if inline_sequence else {"id": sequence_id}

                    result = self.ai_prompt_executor.execute_prompt_sequence(
                        sequence_config=sequence_config,
                        parameter_overrides=parameter_overrides,
                        working_directory=working_directory,
                        results_directory=results_directory,
                    )
                    success = result["success"]

                    # Store sequence results in metadata
                    node.metadata["sequence_results"] = {
                        "total_steps": len(result.get("steps", [])),
                        "failed_step": result.get("failed_step_id"),
                        "duration": result.get("total_duration_seconds"),
                    }

            # Check for accessibility target before normal action delegation
            elif action_type in ("CLICK", "TYPE", "FOCUS"):
                # Check if action has an accessibility target
                target = config.get("target", {})
                if isinstance(target, dict) and target.get("type") == "accessibility":
                    # Handle via accessibility service
                    self.emit_log("info", f"Handling {action_type} with accessibility target")
                    success = await self._handle_accessibility_action(action_type, target)

                    # For TYPE actions with accessibility targets, also type the text
                    if action_type == "TYPE" and success:
                        text = config.get("text", "")
                        if text:
                            service = self._get_accessibility_service()
                            target_ref = target.get("ref")
                            if target_ref:
                                fill_result = await service.fill_ref(
                                    target_ref, text, clear_first=config.get("clearFirst", False)
                                )
                                success = fill_result.get("success", False)
                else:
                    # Normal action delegation to library
                    success = await self.action_executor.execute_action(action)
            else:
                # Normal action delegation to library
                success = await self.action_executor.execute_action(action)

            # Capture error message for failed GO_TO_STATE actions
            if action_type == "GO_TO_STATE" and not success and not error_message:
                state_names = config.get("stateNames", [])
                state_ids = config.get("stateIds", [])
                target_state = (
                    state_names[0] if state_names else (state_ids[0] if state_ids else "unknown")
                )
                error_message = f"Navigation to state '{target_state}' failed - no path found or transition execution failed"

            # Sync last_find_location from library if available
            if (
                hasattr(self.action_executor, "last_find_location")
                and self.action_executor.last_find_location
            ):
                loc = self.action_executor.last_find_location
                self._last_find_location = Location(loc[0], loc[1])  # type: ignore[assignment]
                self.emit_log(
                    "debug", f"Synced last_find_location from library: {self._last_find_location}"
                )

            # Handle GO_TO_STATE actions - record transition data
            if action_type == "GO_TO_STATE" and self.unified_data_collector:
                # Get target state from stateNames (display names) or stateIds (fallback)
                state_names = config.get("stateNames", [])
                state_ids = config.get("stateIds", [])
                target_state = (
                    state_names[0] if state_names else (state_ids[0] if state_ids else "unknown")
                )
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

    async def execute_workflow(
        self, workflow_id: str, transition_context: dict | None = None
    ) -> bool:
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
                    success = await self._execute_workflow_internal(workflow_id)

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
                return await self._execute_workflow_internal(workflow_id)
        except Exception as e:
            self.emit_log("error", f"Workflow execution failed: {e}")
            return False

    async def _execute_workflow_internal(self, workflow_id: str) -> bool:
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

        # Try local workflows first - by ID or by name
        actual_workflow_id = workflow_id
        if workflow_id in self.workflows:
            workflow_data = self.workflows[workflow_id]
        else:
            # Try to find workflow by name
            workflow_data = None
            for wf_id, wf_data in self.workflows.items():
                if isinstance(wf_data, dict) and wf_data.get("name") == workflow_id:
                    workflow_data = wf_data
                    actual_workflow_id = wf_id
                    self.emit_log("debug", f"Found workflow '{workflow_id}' by name (id: {wf_id})")
                    break

        if workflow_data is not None:
            if isinstance(workflow_data, dict):
                actions = workflow_data.get("actions", [])
                workflow_name = workflow_data.get("name", actual_workflow_id)
            else:
                actions = workflow_data
        # Fallback to registry for inline workflows
        elif QONTINUI_AVAILABLE:
            actions = registry.get_workflow(workflow_id)
            if actions is None:
                self.emit_log("error", f"Workflow {workflow_id} not found")
                return False
            workflow_name = (
                registry.get_workflow_name(workflow_id)  # type: ignore[assignment]
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
        node = self.execution_tree.start_workflow(
            actual_workflow_id, workflow_name, is_inline=is_inline
        )
        self.emit_tree_event("workflow_started", node, None)

        success = True

        try:
            for _, action in enumerate(actions):
                if not self.is_running:
                    break

                # Execute action and track success, but always continue
                action_success = await self.execute_action(action)

                if not action_success:
                    success = False
                    self.emit_log(
                        "debug", "Action failed, continuing workflow (resilient automation)"
                    )

                # Small delay between actions
                time.sleep(0.5)

            # End workflow with success status
            self.execution_tree.end_workflow(actual_workflow_id, success=success)
            self.emit_tree_event("workflow_completed" if success else "workflow_failed", node, None)

        except Exception as e:
            # End workflow with failure status
            self.execution_tree.end_workflow(actual_workflow_id, success=False, error=str(e))
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
            image_name = self.get_image_name(image_id)  # type: ignore[arg-type]
            if image_name:
                config_copy = config.copy() if config_copy is None else config_copy
                config_copy["imageName"] = image_name

        # Check for imageId nested in target object
        target = config.get("target")
        if isinstance(target, dict):
            if "imageId" in target:
                image_id = target.get("imageId")
                image_name = self.get_image_name(image_id)  # type: ignore[arg-type]
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
                    config_copy["imageName"] = ", ".join(image_names)  # type: ignore[arg-type]

        # Handle multiple images at root level
        if "imageIds" in config:
            image_ids = config.get("imageIds", [])
            image_names = [self.get_image_name(img_id) for img_id in image_ids]
            image_names = [name for name in image_names if name]
            if image_names:
                config_copy = config.copy() if config_copy is None else config_copy
                config_copy["imageNames"] = image_names
                config_copy["imageName"] = ", ".join(image_names)  # type: ignore[arg-type]

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
